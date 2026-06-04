//! Procedural macros for CIBOS applications.
//!
//! [`macro@main`] turns an `async fn main` into a real entry point. On the host
//! development transport it boots an in-process host kernel ([`AppHost`]), runs
//! the application body on the initial lane — so the ambient execution context
//! (system and lane) is established for the documented `Timer`, `Lane`, and
//! `container` APIs — and drives the runtime until the application is idle.
//!
//! [`AppHost`]: ../cibos_sdk/struct.AppHost.html

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Error, Expr, ItemFn, Pat, Token};

/// Application entry-point attribute.
///
/// Apply it to an argument-less `async fn`:
///
/// ```ignore
/// #[cibos::main]
/// async fn main() {
///     // application body — runs on the initial lane, with the ambient
///     // execution context installed, so `Timer`, `Lane`, and `container`
///     // calls resolve without threading a system handle through.
/// }
/// ```
///
/// The body must evaluate to `()`. It expands to a synchronous `fn main` that
/// constructs the host kernel, spawns the body as the initial lane, and runs it
/// to completion.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);

    if func.sig.asyncness.is_none() {
        return Error::new_spanned(func.sig.fn_token, "#[cibos::main] requires an `async fn`")
            .to_compile_error()
            .into();
    }
    if !func.sig.inputs.is_empty() {
        return Error::new_spanned(
            func.sig.inputs,
            "#[cibos::main] entry point takes no arguments",
        )
        .to_compile_error()
        .into();
    }

    let attrs = &func.attrs;
    let body = &func.block;

    let expanded = quote! {
        #(#attrs)*
        fn main() {
            let mut __cibos_host = ::cibos_sdk::AppHost::new(
                2usize,
                [0u8; 32],
                ::cibos_sdk::CibosProfile::Balanced,
                64usize,
                ::cibos_sdk::ResourceLimits::default_application(),
            );
            __cibos_host.system().spawn_with_lane(
                ::cibos_sdk::WeightClass::User,
                move |_lane| async move #body,
            );
            __cibos_host.run();
        }
    };

    expanded.into()
}

/// One arm of [`select`]: `pattern = future => body`.
struct SelectArm {
    pat: Pat,
    fut: Expr,
    body: Expr,
}

/// The parsed body of a [`select`] invocation: one or more comma-separated arms.
struct SelectInput {
    arms: Vec<SelectArm>,
}

impl Parse for SelectInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut arms = Vec::new();
        while !input.is_empty() {
            let pat = input.call(Pat::parse_single)?;
            input.parse::<Token![=]>()?;
            let fut: Expr = input.parse()?;
            input.parse::<Token![=>]>()?;
            let body: Expr = input.parse()?;
            arms.push(SelectArm { pat, fut, body });
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else {
                break;
            }
        }
        if arms.is_empty() {
            return Err(input.error("select! requires at least one `pattern = future => body` arm"));
        }
        Ok(SelectInput { arms })
    }
}

/// Wait on several futures at once, completing as soon as the first one is
/// ready and running that arm's body with the future's output bound to its
/// pattern.
///
/// ```ignore
/// let winner = select! {
///     msg = channel.receive() => handle(msg),
///     _ = Timer::sleep(timeout) => on_timeout(),
/// };
/// ```
///
/// Arms are polled in written order on each wakeup, so an earlier arm that is
/// ready takes precedence over a later one. Patterns must be irrefutable
/// bindings (the output is bound with `let`).
#[proc_macro]
pub fn select(input: TokenStream) -> TokenStream {
    let SelectInput { arms } = parse_macro_input!(input as SelectInput);

    let pats = arms.iter().map(|a| &a.pat);
    let futs = arms.iter().map(|a| &a.fut);
    let bodies = arms.iter().map(|a| &a.body);
    let bindings: Vec<_> = (0..arms.len())
        .map(|i| format_ident!("__cibos_select_fut_{}", i))
        .collect();

    let expanded = quote! {
        {
            #( let mut #bindings = ::core::pin::pin!(#futs); )*
            ::core::future::poll_fn(move |__cibos_select_cx| {
                #(
                    if let ::core::task::Poll::Ready(__cibos_select_val) =
                        ::core::future::Future::poll(#bindings.as_mut(), __cibos_select_cx)
                    {
                        let #pats = __cibos_select_val;
                        return ::core::task::Poll::Ready(#bodies);
                    }
                )*
                ::core::task::Poll::Pending
            })
            .await
        }
    };

    expanded.into()
}

# Security notes: image signing and verification

CIBOS images carry a per-component SHA-256 and an optional detached signature
over the whole signed region. Two firmware profiles consume them:

* **Lightweight** (default) — verifies component hashes only (physical-trust
  handoff). A corrupt or truncated image is caught; an attacker who can replace
  the whole image is not.
* **Standard** — additionally verifies a SPHINCS+ signature over the image
  before handoff, so only images signed by the trusted root key are booted.

## What works today

The host signing pipeline is complete and exercised end to end via `mkimage`:

```sh
# one-time: generate the trusted root keypair
mkimage keygen keys/trusted_root.pub keys/dev_signing.key

# sign a kernel image (flatten the ELF first with llvm-objcopy)
mkimage sign x86_64 0x1000000 0x1000000 kernel.bin keys/dev_signing.key cibos-signed.cimg

# verify against the public key (Standard policy)
mkimage verify cibos-signed.cimg x86_64 keys/trusted_root.pub
```

Verified behavior (host, SPHINCS+ via `shared`):

* A correctly signed image **verifies** (`signature checked (SPHINCS+)`).
* Flipping any byte of the body → **rejected** at the hash check
  (`component N failed hash verification`).
* Verifying against the wrong public key → **rejected** at the signature check
  (`signature verification failed`).

The signatures are SPHINCS+ (`sphincssha2128fsimple`): 32-byte public key,
64-byte secret key, 17088-byte signature. `shared`'s `verify_image` performs the
same check the firmware would, so producer (`mkimage`) and verifier share one
implementation.

`keys/*.key` are secret keys — never commit them. Regenerate with `keygen`.

## Known limitation: SPHINCS+ does not yet run *in the firmware*

The Standard-profile signature check is fully functional in host tooling and
host tests (`cargo test -p cibios --features test-crypto`), but it cannot yet be
compiled into the **bare-metal** firmware image. The reason is a dependency
constraint, not a design one:

* The PQC backend (`pqcrypto-sphincsplus`) pulls `pqcrypto-internals`, which
  uses `libc` types (`c_int`, `size_t`) and assumes a hosted C environment.
* On a bare target (`*-unknown-none`) there is no `libc`, so the crate fails to
  compile — independent of the `getrandom` backend, which *can* be satisfied:
  supplying a custom `getrandom` backend (entropy-seeded) gets past the
  `getrandom` error, after which `pqcrypto-internals` is the remaining blocker.

### Path to in-firmware verification

A `no_std`, `libc`-free SPHINCS+ **verifier** (verification only — no keygen or
signing needed in firmware) would close this. Options, roughly in order of
effort: a pure-Rust SPHINCS+ verify implementation; or a vendored, freestanding
build of the reference C verifier with the `libc` shim replaced. Until then, the
default firmware is Lightweight, and Standard-profile verification lives in the
build/release tooling that produces and checks images before deployment.

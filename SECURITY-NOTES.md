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

## In-firmware verification — RESOLVED (portable no_std verifier)

The Standard-profile signature check now runs **in the bare-metal firmware**. A
pure-Rust, `no_std`, verification-only SPHINCS+ implementation
(`shared/src/crypto/backends/sphincs_portable.rs`, feature
`pqc-sphincs-portable`) was ported faithfully from the PQClean `clean` reference
for `sphincs-sha2-128f-simple`. It depends only on `sha2` (already a `no_std`
dependency), so it compiles on `*-unknown-none` where the libc-bound
`pqcrypto-sphincsplus` could not.

* **Byte-compatible** with the host signer: a cross-implementation test signs
  with `pqcrypto` and verifies with the portable code (`cargo test -p shared
  --features "std,pqc-sphincs,pqc-sphincs-portable"`), and the firmware's own
  `standard_verifies_real_sphincs_signature` test does the same end to end.
* **`handoff-cryptographic`** now pulls `shared/pqc-sphincs-portable` (not
  `pqc-sphincs`), so Standard firmware links bare on x86_64/aarch64/riscv64.
* The firmware embeds the trusted root public key at build time
  (`include_bytes!("../keys/trusted_root.pub")`) and verifies the CIBOS image
  against it before handoff.
* **Runtime-verified in QEMU:** the `balanced` and `maximum-isolation` signed
  images boot with `firmware profile: Standard` and `image verified (signature
  checked)`, reaching `CIBOS kernel: boot complete`. A tampered signed image is
  **rejected** at boot (`image verification failed`), so the security property
  holds on emulated hardware.

The host signer (`pqcrypto`, `pqc-sphincs`) remains the build-time tooling that
produces signatures; only verification needed to move into the firmware, and it
has. `mkimage sign` now also accepts a profile-stamp argument so signed images
carry the correct operational profile.

## Signature-algorithm selection (quantum vs classical) — current status

The image header carries a `signature_algorithm` field
(`shared::SignatureAlgorithm`: `Ed25519` = classical, `SphincsPlus` and `MlDsa`
= post-quantum). As of this change, **firmware verification dispatches on that
field** via the single shared entry point `shared::crypto::signature::verify_with`,
rather than assuming one scheme:

* The firmware reads the algorithm from the image header and calls the matching
  compiled-in verifier.
* `verify_with`'s SPHINCS+ arm prefers the std backend when present and falls
  back to the no_std **portable** verifier on bare targets, so the dispatcher
  links in firmware as well as host tooling.
* **Fail-closed guarantee (tested):** if the selected algorithm has no verifier
  compiled into the running firmware — an unknown discriminant, or e.g. an image
  stamped `Ed25519` (no backend) or `MlDsa` (its `pqcrypto-mldsa` verifier is
  libc-bound and does not link bare) — verification REJECTS the image rather than
  booting it unverified. Covered by `unavailable_algorithm_fails_closed`.

Honest scope of "selection" today:
* **SPHINCS+** (post-quantum, hash-based): the complete, bare-verifiable root of
  trust — sign (host tooling) and verify (bare firmware) both work. This is the
  production default.
* **ML-DSA** (post-quantum, lattice): verifier present but libc-bound, so it does
  NOT yet link in bare firmware; a no_std/portable ML-DSA verifier (mirroring the
  SPHINCS+ portable port) is the prerequisite before ML-DSA-signed images can be
  booted on hardware. Until then the firmware correctly fails closed on ML-DSA.
* **Ed25519** (classical, NON-quantum-resistant): enum + dispatcher arm only; no
  backend is compiled. It exists so a deployment that explicitly wants a fast
  classical option can add a `classical-crypto` backend — but it is intentionally
  unavailable by default (a boot root of trust should be post-quantum), and the
  firmware fails closed on it today.

`mkimage sign` produces SPHINCS+ signatures (the only scheme that verifies bare).
Adding ML-DSA *signing* there is deferred until the portable ML-DSA verifier
exists, so the tool never emits an image that bare firmware cannot verify.

## (Historical) Known limitation: SPHINCS+ did not run in the firmware

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

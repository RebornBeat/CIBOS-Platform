//! # Pure-Rust SPHINCS+ verifier (`sphincs-sha2-128f-simple`)
//!
//! A `no_std`, allocation-free, **verification-only** SPHINCS+ implementation
//! for the `sphincssha2128fsimple` parameter set — the system's root-of-trust
//! signature scheme for boot/firmware image verification.
//!
//! ## Why this exists
//!
//! The other SPHINCS+ backend (`sphincs.rs`) wraps `pqcrypto-sphincsplus`, which
//! pulls `pqcrypto-internals` and assumes a hosted C environment (`libc`). That
//! cannot compile into the bare-metal firmware (`*-unknown-none`). This module
//! is a faithful port of the PQClean `clean` C reference for the same parameter
//! set, depending only on `sha2` (already a `no_std` dependency), so the
//! **Standard firmware profile can verify image signatures on bare metal**.
//!
//! ## Compatibility
//!
//! Byte-for-byte compatible with `pqcrypto-sphincsplus`'s
//! `sphincssha2128fsimple`: a signature produced by the host tooling (`mkimage`,
//! which uses the pqcrypto signer) verifies here, and vice versa. This is
//! checked by a cross-implementation test against the pqcrypto backend (host,
//! `std` + `pqc-sphincs`), and by a known-answer round trip.
//!
//! ## Scope
//!
//! Verification only. No key generation, no signing — those are build-time
//! tooling operations and stay in the `std`/pqcrypto path. Mirroring PQClean's
//! `crypto_sign_verify`, this derives the FORS public key from the signature,
//! climbs the hypertree, and compares the recovered root to the public key.
//!
//! Compiled when the `pqc-sphincs-portable` feature is enabled.

use crate::crypto::signature::{SignatureAlgorithm, SignatureVerifier};
use crate::types::error::CryptoError;

use sha2::{Digest, Sha256};

// ---- Parameters for sphincs-sha2-128f-simple (PQClean params.h) ------------

/// Hash output length in bytes (`SPX_N`).
const N: usize = 16;
/// Hypertree total height (`SPX_FULL_HEIGHT`).
const FULL_HEIGHT: usize = 66;
/// Number of subtree layers (`SPX_D`).
const D: usize = 22;
/// Per-subtree height (`SPX_TREE_HEIGHT = FULL_HEIGHT / D`).
const TREE_HEIGHT: usize = FULL_HEIGHT / D; // 3
/// FORS tree height (`SPX_FORS_HEIGHT`).
const FORS_HEIGHT: usize = 6;
/// Number of FORS trees (`SPX_FORS_TREES`).
const FORS_TREES: usize = 33;
/// Winternitz parameter (`SPX_WOTS_W`).
const WOTS_W: usize = 16;
/// log2(W) (`SPX_WOTS_LOGW`).
const WOTS_LOGW: usize = 4;
/// First WOTS chain count (`SPX_WOTS_LEN1 = 8*N/LOGW`).
const WOTS_LEN1: usize = 8 * N / WOTS_LOGW; // 32
/// Checksum WOTS chain count (`SPX_WOTS_LEN2`, precomputed in params.h).
const WOTS_LEN2: usize = 3;
/// Total WOTS chains (`SPX_WOTS_LEN`).
const WOTS_LEN: usize = WOTS_LEN1 + WOTS_LEN2; // 35
/// WOTS signature/pk byte length (`SPX_WOTS_BYTES`).
const WOTS_BYTES: usize = WOTS_LEN * N; // 560

/// FORS message bits / bytes (`SPX_FORS_MSG_BYTES`).
const FORS_MSG_BYTES: usize = (FORS_HEIGHT * FORS_TREES).div_ceil(8); // 25
/// FORS signature byte length (`SPX_FORS_BYTES`).
const FORS_BYTES: usize = (FORS_HEIGHT + 1) * FORS_TREES * N; // 3696

/// Public key length (`SPX_PK_BYTES = 2*N`).
pub const PUBLIC_KEY_LEN: usize = 2 * N; // 32
/// Full signature length (`SPX_BYTES`).
pub const SIGNATURE_LEN: usize =
    N + FORS_BYTES + (D * WOTS_BYTES) + (FULL_HEIGHT * N); // 17088

// ADRS layout offsets for the SHA2 instantiation (sha2_offsets.h).
const ADDR_BYTES: usize = 32;
const OFFSET_LAYER: usize = 0;
const OFFSET_TREE: usize = 1;
const OFFSET_TYPE: usize = 9;
const OFFSET_KP_ADDR1: usize = 13;
const OFFSET_CHAIN_ADDR: usize = 17;
const OFFSET_HASH_ADDR: usize = 21;
const OFFSET_TREE_HGT: usize = 17;
const OFFSET_TREE_INDEX: usize = 18;

// ADRS type constants (address.h).
const ADDR_TYPE_WOTS: u8 = 0;
const ADDR_TYPE_WOTSPK: u8 = 1;
const ADDR_TYPE_HASHTREE: u8 = 2;
const ADDR_TYPE_FORSTREE: u8 = 3;
const ADDR_TYPE_FORSPK: u8 = 4;

/// For the SHA-256 instantiation, only the first 22 bytes of the 32-byte ADRS
/// are fed into the tweakable hash (`SPX_SHA256_ADDR_BYTES`).
const SHA256_ADDR_BYTES: usize = 22;

// ---- ADRS (hash address) ---------------------------------------------------

/// A SPHINCS+ hash address. Byte-addressed exactly as the C reference treats
/// `uint32_t addr[8]` via `unsigned char *`.
#[derive(Clone, Copy)]
struct Addr {
    bytes: [u8; ADDR_BYTES],
}

impl Addr {
    fn new() -> Self {
        Self {
            bytes: [0u8; ADDR_BYTES],
        }
    }

    fn set_layer(&mut self, layer: u32) {
        self.bytes[OFFSET_LAYER] = layer as u8;
    }

    fn set_tree(&mut self, tree: u64) {
        self.bytes[OFFSET_TREE..OFFSET_TREE + 8].copy_from_slice(&tree.to_be_bytes());
    }

    fn set_type(&mut self, t: u8) {
        self.bytes[OFFSET_TYPE] = t;
    }

    /// `copy_subtree_addr`: copy the layer + tree fields (bytes `0..TREE+8`).
    fn copy_subtree_from(&mut self, src: &Addr) {
        self.bytes[..OFFSET_TREE + 8].copy_from_slice(&src.bytes[..OFFSET_TREE + 8]);
    }

    /// `copy_keypair_addr`: copy layer + tree, plus the low keypair byte.
    fn copy_keypair_from(&mut self, src: &Addr) {
        self.bytes[..OFFSET_TREE + 8].copy_from_slice(&src.bytes[..OFFSET_TREE + 8]);
        self.bytes[OFFSET_KP_ADDR1] = src.bytes[OFFSET_KP_ADDR1];
    }

    fn set_keypair(&mut self, keypair: u32) {
        self.bytes[OFFSET_KP_ADDR1] = keypair as u8;
    }

    fn set_chain(&mut self, chain: u32) {
        self.bytes[OFFSET_CHAIN_ADDR] = chain as u8;
    }

    fn set_hash(&mut self, hash: u32) {
        self.bytes[OFFSET_HASH_ADDR] = hash as u8;
    }

    fn set_tree_height(&mut self, h: u32) {
        self.bytes[OFFSET_TREE_HGT] = h as u8;
    }

    fn set_tree_index(&mut self, idx: u32) {
        self.bytes[OFFSET_TREE_INDEX..OFFSET_TREE_INDEX + 4].copy_from_slice(&idx.to_be_bytes());
    }
}

// ---- Tweakable hash (simple SHA-256 instantiation) -------------------------

/// The "simple" tweakable hash: `T_l(in) = Trunc_N( SHA-256( pub_seed_block ||
/// ADRS[0..22] || in ) )`, where `pub_seed_block` is `pub_seed` padded with
/// zeros to one 64-byte SHA-256 block. PQClean precomputes the midstate over
/// that block; feeding the same bytes to a fresh hasher yields identical output.
///
/// `out` receives `N` bytes. `inputs` are concatenated `N`-byte blocks.
fn thash(out: &mut [u8], pub_seed_block: &[u8; 64], addr: &Addr, inputs: &[u8]) {
    let mut h = Sha256::new();
    h.update(pub_seed_block);
    h.update(&addr.bytes[..SHA256_ADDR_BYTES]);
    h.update(inputs);
    let digest = h.finalize();
    out[..N].copy_from_slice(&digest[..N]);
}

/// Build the 64-byte block holding `pub_seed` followed by zero padding.
fn seed_block(pub_seed: &[u8]) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..N].copy_from_slice(&pub_seed[..N]);
    b
}

// ---- MGF1-SHA256 and the message hash --------------------------------------

/// MGF1 based on SHA-256 (hash_sha2.c `mgf1_256`). Fills `out` with
/// `SHA256(in || counter_be32)` blocks.
fn mgf1_256(out: &mut [u8], input: &[u8]) {
    let mut counter: u32 = 0;
    let mut produced = 0usize;
    while produced < out.len() {
        let mut h = Sha256::new();
        h.update(input);
        h.update(counter.to_be_bytes());
        let block = h.finalize();
        let take = core::cmp::min(32, out.len() - produced);
        out[produced..produced + take].copy_from_slice(&block[..take]);
        produced += take;
        counter += 1;
    }
}

/// Big-endian bytes → u64 (`bytes_to_ull`).
fn bytes_to_u64(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for &byte in b {
        v = (v << 8) | u64::from(byte);
    }
    v
}

// Message-hash sub-field sizes (hash_message in hash_sha2.c).
const TREE_BITS: usize = TREE_HEIGHT * (D - 1); // 63
const TREE_BYTES: usize = TREE_BITS.div_ceil(8); // 8
const LEAF_BITS: usize = TREE_HEIGHT; // 3
const LEAF_BYTES: usize = LEAF_BITS.div_ceil(8); // 1
const DGST_BYTES: usize = FORS_MSG_BYTES + TREE_BYTES + LEAF_BYTES; // 34

/// `hash_message`: derive the FORS message digest, the hypertree index `tree`,
/// and the leaf index from `R || PK || M`. Mirrors hash_sha2.c exactly:
/// `seed = R || PK.seed || SHA-256(R || PK || M)`, then
/// `buf = MGF1-SHA256(seed, DGST_BYTES)`.
fn hash_message(
    digest: &mut [u8; FORS_MSG_BYTES],
    r: &[u8],
    pk: &[u8],
    message: &[u8],
) -> (u64, u32) {
    // seed has layout: [ R(N) | PK.seed(N) | SHA256(R||PK||M)(32) ]
    let mut seed = [0u8; 2 * N + 32];

    let mut h = Sha256::new();
    h.update(&r[..N]);
    h.update(&pk[..PUBLIC_KEY_LEN]);
    h.update(message);
    let inner = h.finalize();

    seed[..N].copy_from_slice(&r[..N]);
    seed[N..2 * N].copy_from_slice(&pk[..N]); // PK.seed is the first N bytes of pk
    seed[2 * N..].copy_from_slice(&inner);

    let mut buf = [0u8; DGST_BYTES];
    mgf1_256(&mut buf, &seed);

    digest.copy_from_slice(&buf[..FORS_MSG_BYTES]);

    let mut tree = bytes_to_u64(&buf[FORS_MSG_BYTES..FORS_MSG_BYTES + TREE_BYTES]);
    // Mask to TREE_BITS low bits.
    tree &= u64::MAX >> (64 - TREE_BITS);

    let leaf_off = FORS_MSG_BYTES + TREE_BYTES;
    let mut leaf_idx = bytes_to_u64(&buf[leaf_off..leaf_off + LEAF_BYTES]) as u32;
    leaf_idx &= u32::MAX >> (32 - LEAF_BITS);

    (tree, leaf_idx)
}

// ---- WOTS+ public-key-from-signature ---------------------------------------

/// `base_w`: interpret `input` as `out_len` base-w (4-bit) digits.
fn base_w(output: &mut [u32], out_len: usize, input: &[u8]) {
    let mut input_idx = 0usize;
    let mut total: u8 = 0;
    let mut bits = 0i32;
    for o in output.iter_mut().take(out_len) {
        if bits == 0 {
            total = input[input_idx];
            input_idx += 1;
            bits += 8;
        }
        bits -= WOTS_LOGW as i32;
        *o = u32::from((total >> bits) & ((WOTS_W as u8) - 1));
    }
}

/// `chain_lengths`: base-w of the message plus the WOTS checksum digits.
fn chain_lengths(lengths: &mut [u32; WOTS_LEN], msg: &[u8]) {
    base_w(&mut lengths[..WOTS_LEN1], WOTS_LEN1, msg);

    // wots_checksum.
    let mut csum: u32 = 0;
    for &l in lengths.iter().take(WOTS_LEN1) {
        csum += (WOTS_W as u32) - 1 - l;
    }
    // csum << ((8 - ((LEN2*LOGW) % 8)) % 8)
    let shift = (8 - ((WOTS_LEN2 * WOTS_LOGW) % 8)) % 8;
    csum <<= shift as u32;
    let csum_bytes_len = (WOTS_LEN2 * WOTS_LOGW).div_ceil(8); // 2
    let mut csum_bytes = [0u8; (WOTS_LEN2 * WOTS_LOGW).div_ceil(8)];
    // ull_to_bytes (big-endian) of csum into csum_bytes_len bytes.
    for i in 0..csum_bytes_len {
        csum_bytes[csum_bytes_len - 1 - i] = ((csum >> (8 * i)) & 0xff) as u8;
    }
    base_w(&mut lengths[WOTS_LEN1..], WOTS_LEN2, &csum_bytes);
}

/// `gen_chain`: apply the WOTS hash chain from position `start` for `steps`.
fn gen_chain(
    out: &mut [u8],
    input: &[u8],
    start: u32,
    steps: u32,
    pub_seed_block: &[u8; 64],
    addr: &mut Addr,
) {
    out[..N].copy_from_slice(&input[..N]);
    let mut i = start;
    while i < start + steps && (i as usize) < WOTS_W {
        addr.set_hash(i);
        let mut tmp = [0u8; N];
        thash(&mut tmp, pub_seed_block, addr, &out[..N]);
        out[..N].copy_from_slice(&tmp);
        i += 1;
    }
}

/// `wots_pk_from_sig`: recover the WOTS public key (WOTS_LEN * N bytes) from a
/// WOTS signature and the N-byte message.
fn wots_pk_from_sig(
    pk: &mut [u8; WOTS_BYTES],
    sig: &[u8],
    msg: &[u8],
    pub_seed_block: &[u8; 64],
    addr: &mut Addr,
) {
    let mut lengths = [0u32; WOTS_LEN];
    chain_lengths(&mut lengths, msg);

    for i in 0..WOTS_LEN {
        addr.set_chain(i as u32);
        let start = lengths[i];
        let steps = (WOTS_W as u32) - 1 - lengths[i];
        let mut chain_out = [0u8; N];
        gen_chain(
            &mut chain_out,
            &sig[i * N..i * N + N],
            start,
            steps,
            pub_seed_block,
            addr,
        );
        pk[i * N..i * N + N].copy_from_slice(&chain_out);
    }
}

// ---- Merkle root from leaf + authentication path ---------------------------

/// `compute_root`: given a leaf and its auth path, compute the subtree root.
/// `addr` must be complete except for tree height/index, which this sets.
//
// Eight parameters mirror the PQClean `compute_root` signature exactly; bundling
// them into a struct would obscure the one-to-one correspondence with the
// reference and make divergence harder to audit.
#[allow(clippy::too_many_arguments)]
fn compute_root(
    root: &mut [u8; N],
    leaf: &[u8; N],
    mut leaf_idx: u32,
    mut idx_offset: u32,
    auth_path: &[u8],
    tree_height: usize,
    pub_seed_block: &[u8; 64],
    addr: &mut Addr,
) {
    let mut buffer = [0u8; 2 * N];
    let mut ap = 0usize; // offset into auth_path

    // First level: place leaf and first auth node according to parity.
    if leaf_idx & 1 != 0 {
        buffer[N..].copy_from_slice(leaf);
        buffer[..N].copy_from_slice(&auth_path[..N]);
    } else {
        buffer[..N].copy_from_slice(leaf);
        buffer[N..].copy_from_slice(&auth_path[..N]);
    }
    ap += N;

    for i in 0..tree_height.saturating_sub(1) {
        leaf_idx >>= 1;
        idx_offset >>= 1;
        addr.set_tree_height((i + 1) as u32);
        addr.set_tree_index(leaf_idx + idx_offset);

        if leaf_idx & 1 != 0 {
            let mut tmp = [0u8; N];
            thash(&mut tmp, pub_seed_block, addr, &buffer);
            buffer[N..].copy_from_slice(&tmp);
            buffer[..N].copy_from_slice(&auth_path[ap..ap + N]);
        } else {
            let mut tmp = [0u8; N];
            thash(&mut tmp, pub_seed_block, addr, &buffer);
            buffer[..N].copy_from_slice(&tmp);
            buffer[N..].copy_from_slice(&auth_path[ap..ap + N]);
        }
        ap += N;
    }

    // Last level: no auth node copied.
    leaf_idx >>= 1;
    idx_offset >>= 1;
    addr.set_tree_height(tree_height as u32);
    addr.set_tree_index(leaf_idx + idx_offset);
    thash(root, pub_seed_block, addr, &buffer);
}

// ---- FORS public-key-from-signature ----------------------------------------

/// `message_to_indices`: derive the `FORS_TREES` leaf indices from the digest.
fn message_to_indices(indices: &mut [u32; FORS_TREES], m: &[u8]) {
    let mut offset = 0usize;
    for idx in indices.iter_mut() {
        *idx = 0;
        for j in 0..FORS_HEIGHT {
            let bit = (m[offset >> 3] >> (offset & 0x7)) & 0x1;
            *idx ^= u32::from(bit) << j;
            offset += 1;
        }
    }
}

/// `fors_pk_from_sig`: recover the FORS public key from the FORS signature and
/// the message digest, under the given (keypair-bearing) FORS address.
fn fors_pk_from_sig(
    pk: &mut [u8; N],
    sig: &[u8],
    m: &[u8],
    pub_seed_block: &[u8; 64],
    fors_addr: &Addr,
) {
    let mut indices = [0u32; FORS_TREES];
    message_to_indices(&mut indices, m);

    let mut roots = [0u8; FORS_TREES * N];

    let mut fors_tree_addr = Addr::new();
    fors_tree_addr.copy_keypair_from(fors_addr);
    fors_tree_addr.set_type(ADDR_TYPE_FORSTREE);

    let mut fors_pk_addr = Addr::new();
    fors_pk_addr.copy_keypair_from(fors_addr);
    fors_pk_addr.set_type(ADDR_TYPE_FORSPK);

    let mut sig_off = 0usize;
    for (i, &index) in indices.iter().enumerate() {
        let idx_offset = (i as u32) * (1u32 << FORS_HEIGHT);

        // Leaf = thash(sk_part) with tree height 0, index = index+idx_offset.
        fors_tree_addr.set_tree_height(0);
        fors_tree_addr.set_tree_index(index + idx_offset);

        let mut leaf = [0u8; N];
        thash(
            &mut leaf,
            pub_seed_block,
            &fors_tree_addr,
            &sig[sig_off..sig_off + N],
        );
        sig_off += N;

        // Root of this FORS tree from leaf + auth path.
        let mut root = [0u8; N];
        compute_root(
            &mut root,
            &leaf,
            index,
            idx_offset,
            &sig[sig_off..sig_off + FORS_HEIGHT * N],
            FORS_HEIGHT,
            pub_seed_block,
            &mut fors_tree_addr,
        );
        roots[i * N..i * N + N].copy_from_slice(&root);
        sig_off += FORS_HEIGHT * N;
    }

    // FORS pk = thash across all roots.
    thash(pk, pub_seed_block, &fors_pk_addr, &roots);
}

// ---- Top-level verify (crypto_sign_verify) ---------------------------------

/// Verify a detached SPHINCS+ signature. Returns `Ok(())` iff the recovered
/// hypertree root equals the root in the public key.
fn sphincs_verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
    if public_key.len() != PUBLIC_KEY_LEN {
        return Err(CryptoError::InvalidKeyLength {
            expected: PUBLIC_KEY_LEN,
            actual: public_key.len(),
        });
    }
    if signature.len() != SIGNATURE_LEN {
        return Err(CryptoError::InvalidSignatureLength {
            expected: SIGNATURE_LEN,
            actual: signature.len(),
        });
    }

    let pub_seed = &public_key[..N];
    let pub_root = &public_key[N..2 * N];
    let pub_seed_block = seed_block(pub_seed);

    let mut sig_off = 0usize;

    // R is the first N bytes of the signature.
    let r = &signature[..N];

    // Message hash → FORS digest, tree, leaf index.
    let mut mhash = [0u8; FORS_MSG_BYTES];
    let (mut tree, mut idx_leaf) = hash_message(&mut mhash, r, public_key, message);
    sig_off += N;

    // FORS: recover the FORS public key (becomes the first `root`).
    let mut wots_addr = Addr::new();
    wots_addr.set_type(ADDR_TYPE_WOTS);
    wots_addr.set_tree(tree);
    wots_addr.set_keypair(idx_leaf);

    let mut root = [0u8; N];
    fors_pk_from_sig(
        &mut root,
        &signature[sig_off..sig_off + FORS_BYTES],
        &mhash,
        &pub_seed_block,
        &wots_addr,
    );
    sig_off += FORS_BYTES;

    // Hypertree: climb D subtrees, recomputing the root each layer.
    for layer in 0..D {
        let mut tree_addr = Addr::new();
        tree_addr.set_type(ADDR_TYPE_HASHTREE);
        tree_addr.set_layer(layer as u32);
        tree_addr.set_tree(tree);

        let mut w_addr = Addr::new();
        w_addr.set_type(ADDR_TYPE_WOTS);
        w_addr.copy_subtree_from(&tree_addr);
        w_addr.set_keypair(idx_leaf);

        let mut wots_pk_addr = Addr::new();
        wots_pk_addr.set_type(ADDR_TYPE_WOTSPK);
        wots_pk_addr.copy_keypair_from(&w_addr);

        // WOTS public key from the WOTS signature over the current root.
        let mut wots_pk = [0u8; WOTS_BYTES];
        wots_pk_from_sig(
            &mut wots_pk,
            &signature[sig_off..sig_off + WOTS_BYTES],
            &root,
            &pub_seed_block,
            &mut w_addr,
        );
        sig_off += WOTS_BYTES;

        // Leaf = thash over the whole WOTS public key.
        let mut leaf = [0u8; N];
        thash(&mut leaf, &pub_seed_block, &wots_pk_addr, &wots_pk);

        // Subtree root from leaf + auth path.
        let mut new_root = [0u8; N];
        compute_root(
            &mut new_root,
            &leaf,
            idx_leaf,
            0,
            &signature[sig_off..sig_off + TREE_HEIGHT * N],
            TREE_HEIGHT,
            &pub_seed_block,
            &mut tree_addr,
        );
        root = new_root;
        sig_off += TREE_HEIGHT * N;

        // Indices for the next layer up.
        idx_leaf = (tree & ((1u64 << TREE_HEIGHT) - 1)) as u32;
        tree >>= TREE_HEIGHT;
    }

    // Constant-time-ish compare of the recovered root to the public root.
    let mut diff = 0u8;
    for i in 0..N {
        diff |= root[i] ^ pub_root[i];
    }
    if diff == 0 {
        Ok(())
    } else {
        Err(CryptoError::SignatureInvalid)
    }
}

/// Pure-Rust SPHINCS+ verifier (`sphincssha2128fsimple`), `no_std`.
pub struct SphincsPlusPortableVerifier;

impl SignatureVerifier for SphincsPlusPortableVerifier {
    const ALGORITHM: SignatureAlgorithm = SignatureAlgorithm::SphincsPlus;
    const PUBLIC_KEY_LEN: usize = PUBLIC_KEY_LEN;
    const SIGNATURE_MAX_LEN: usize = SIGNATURE_LEN;

    fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        sphincs_verify(public_key, message, signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_lengths() {
        assert!(matches!(
            sphincs_verify(&[0u8; 4], b"m", &[0u8; SIGNATURE_LEN]),
            Err(CryptoError::InvalidKeyLength { .. })
        ));
        assert!(matches!(
            sphincs_verify(&[0u8; PUBLIC_KEY_LEN], b"m", &[0u8; 10]),
            Err(CryptoError::InvalidSignatureLength { .. })
        ));
    }

    #[test]
    fn parameter_sizes_match_spec() {
        assert_eq!(PUBLIC_KEY_LEN, 32);
        assert_eq!(SIGNATURE_LEN, 17088);
        assert_eq!(WOTS_LEN, 35);
        assert_eq!(WOTS_BYTES, 560);
        assert_eq!(FORS_BYTES, 3696);
        assert_eq!(TREE_HEIGHT, 3);
        assert_eq!(DGST_BYTES, 34);
    }

    // Cross-check against the pqcrypto backend: a signature produced by the
    // reference signer must verify under this pure-Rust verifier. Only runs
    // when both this and the pqcrypto backend are compiled in (host std build).
    #[cfg(all(feature = "std", feature = "pqc-sphincs"))]
    #[test]
    fn verifies_pqcrypto_signature() {
        use crate::crypto::backends::sphincs::{generate_keypair, SphincsPlusSigner};
        use crate::crypto::signature::SignatureSigner;
        use alloc::vec::Vec;

        let (pk, sk) = generate_keypair().expect("keypair");
        let msg = b"CIBOS image bytes for cross-impl verification";
        let mut sig = Vec::new();
        SphincsPlusSigner::sign(&sk, msg, &mut sig).expect("sign");

        // The reference-produced signature verifies under our pure-Rust code.
        sphincs_verify(&pk, msg, &sig).expect("portable verify must accept a valid signature");

        // A tampered message is rejected.
        let bad = b"CIBOS image bytes for cross-impl verificatioX";
        assert!(sphincs_verify(&pk, bad, &sig).is_err());

        // A tampered signature is rejected.
        let mut bad_sig = sig.clone();
        bad_sig[100] ^= 0x01;
        assert!(sphincs_verify(&pk, msg, &bad_sig).is_err());
    }
}

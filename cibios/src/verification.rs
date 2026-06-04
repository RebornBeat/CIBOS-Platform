//! # Image Verification
//!
//! The policy-driven gate a CIBOS image must pass before CIBIOS will hand off
//! to it.
//!
//! Verification proceeds in fixed order:
//!
//! 1. **Structural parse** — [`crate::image::ImageView::parse`].
//! 2. **Architecture match** — the image's target architecture must equal the
//!    running architecture.
//! 3. **Integrity** — every component body must match its SHA-256 descriptor
//!    hash. This runs on *every* profile, so corruption is caught even when
//!    signatures are not.
//! 4. **Authenticity** — on the Standard profile the detached SPHINCS+
//!    signature over the signed region must verify against the firmware's
//!    trusted root key. On the Lightweight profile this step is skipped (trust
//!    is established physically).
//!
//! ## Fail-closed
//!
//! If a policy *requires* a signature but this firmware was built without the
//! `handoff-cryptographic` feature (so no verifier exists), verification
//! returns an error rather than silently proceeding. A required check that
//! cannot be performed is a failure, never a pass.

use crate::error::FirmwareError;
use crate::image::ImageView;

/// Policy controlling how strictly an image is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationPolicy {
    /// Whether a valid signature is required (Standard profile) or skipped
    /// (Lightweight profile).
    pub require_signature: bool,
    /// The architecture the firmware is running on, as a
    /// [`shared::ProcessorArchitecture`] discriminant.
    pub running_architecture: u32,
}

/// The result of a successful verification: the facts the loader needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedImage {
    /// Kernel entry point address.
    pub entry_point: u64,
    /// Image load base address.
    pub load_base: u64,
    /// Target kernel profile discriminant.
    pub cibos_profile: u32,
    /// Number of components.
    pub component_count: u32,
    /// Whether a signature was actually verified (false on Lightweight).
    pub signature_verified: bool,
}

/// Verify `image` under `policy`, using `trusted_root_key` for signature
/// verification when required.
///
/// `trusted_root_key` is the firmware's compiled-in SPHINCS+ root public key.
/// It is ignored when the policy does not require a signature.
///
/// # Errors
///
/// Returns the first [`FirmwareError`] encountered: a malformed image, an
/// architecture mismatch, a component hash mismatch, or a signature failure
/// (including the fail-closed case where a signature is required but no
/// verifier is compiled in).
pub fn verify_image(
    image: &[u8],
    policy: &VerificationPolicy,
    trusted_root_key: &[u8],
) -> Result<VerifiedImage, FirmwareError> {
    let view = ImageView::parse(image)?;
    let header = *view.header();

    if header.architecture != policy.running_architecture {
        return Err(FirmwareError::ArchitectureMismatch {
            image: header.architecture,
            running: policy.running_architecture,
        });
    }

    // Integrity always.
    view.verify_component_hashes()?;

    // Authenticity when required.
    let signature_verified = if policy.require_signature {
        verify_signature(&view, trusted_root_key)?;
        true
    } else {
        false
    };

    Ok(VerifiedImage {
        entry_point: header.entry_point,
        load_base: header.load_base,
        cibos_profile: header.cibos_profile,
        component_count: header.component_count,
        signature_verified,
    })
}

#[cfg(feature = "handoff-cryptographic")]
fn verify_signature(
    view: &ImageView<'_>,
    trusted_root_key: &[u8],
) -> Result<(), FirmwareError> {
    use shared::crypto::backends::sphincs::SphincsPlusVerifier;
    use shared::{SharedError, SignatureVerifier};

    let signed = view.signed_region()?;
    let signature = view.signature()?;
    SphincsPlusVerifier::verify(trusted_root_key, signed, signature)
        .map_err(|e| FirmwareError::from(SharedError::from(e)))
}

#[cfg(not(feature = "handoff-cryptographic"))]
fn verify_signature(
    _view: &ImageView<'_>,
    _trusted_root_key: &[u8],
) -> Result<(), FirmwareError> {
    // Fail closed: the policy demanded a signature, but this firmware was built
    // without a signature verifier. Refuse rather than proceed unverified.
    use shared::types::error::CryptoError;
    use shared::SharedError;
    Err(FirmwareError::from(SharedError::from(
        CryptoError::AlgorithmUnavailable {
            algorithm: "SPHINCS+ (handoff-cryptographic feature not compiled)",
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::build::{build_unsigned, finalize_signed, ComponentInput, ImageParams};
    use crate::image::ComponentKind;
    use shared::{ProcessorArchitecture, SignatureAlgorithm};

    fn x86_64() -> u32 {
        ProcessorArchitecture::X86_64.as_u32()
    }

    fn build_lightweight_image() -> std::vec::Vec<u8> {
        let kernel = b"fake kernel bytes for testing only";
        let params = ImageParams {
            architecture: x86_64(),
            cibos_profile: shared::CibosProfile::Compute.as_u32(),
            entry_point: 0x10_0000,
            load_base: 0x10_0000,
            signature_algorithm: SignatureAlgorithm::SphincsPlus.as_u32(),
            signature_len: 0,
        };
        let comps = [ComponentInput {
            kind: ComponentKind::Kernel,
            load_addr: 0x10_0000,
            body: kernel,
        }];
        let unsigned = build_unsigned(&params, &comps);
        finalize_signed(unsigned, &[])
    }

    #[test]
    fn lightweight_accepts_unsigned_image() {
        let image = build_lightweight_image();
        let policy = VerificationPolicy {
            require_signature: false,
            running_architecture: x86_64(),
        };
        let verified = verify_image(&image, &policy, &[]).expect("verify");
        assert_eq!(verified.entry_point, 0x10_0000);
        assert!(!verified.signature_verified);
        assert_eq!(verified.component_count, 1);
    }

    #[test]
    fn architecture_mismatch_rejected() {
        let image = build_lightweight_image();
        let policy = VerificationPolicy {
            require_signature: false,
            running_architecture: ProcessorArchitecture::AArch64.as_u32(),
        };
        assert!(matches!(
            verify_image(&image, &policy, &[]),
            Err(FirmwareError::ArchitectureMismatch { .. })
        ));
    }

    #[test]
    fn corrupted_component_rejected() {
        let mut image = build_lightweight_image();
        let policy = VerificationPolicy {
            require_signature: false,
            running_architecture: x86_64(),
        };
        // Flip a byte in the component body region (after header+descriptor).
        let body_start = crate::image::HEADER_LEN + crate::image::DESCRIPTOR_LEN;
        image[body_start] ^= 0xFF;
        assert!(matches!(
            verify_image(&image, &policy, &[]),
            Err(FirmwareError::ComponentHashMismatch { index: 0 })
        ));
    }

    #[cfg(not(feature = "handoff-cryptographic"))]
    #[test]
    fn required_signature_without_verifier_fails_closed() {
        let image = build_lightweight_image();
        let policy = VerificationPolicy {
            require_signature: true,
            running_architecture: x86_64(),
        };
        // No verifier compiled in, but signature required => must fail.
        assert!(verify_image(&image, &policy, &[]).is_err());
    }

    #[cfg(feature = "handoff-cryptographic")]
    #[test]
    fn standard_verifies_real_sphincs_signature() {
        use shared::crypto::backends::sphincs::{generate_keypair, SphincsPlusSigner};
        use shared::crypto::signature::SignatureSigner;

        let (pk, sk) = generate_keypair().expect("keypair");

        let kernel = b"fake kernel bytes for signed testing";
        let params = ImageParams {
            architecture: x86_64(),
            cibos_profile: shared::CibosProfile::Balanced.as_u32(),
            entry_point: 0x20_0000,
            load_base: 0x20_0000,
            signature_algorithm: SignatureAlgorithm::SphincsPlus.as_u32(),
            signature_len: shared::crypto::backends::sphincs::SIGNATURE_LEN as u32,
        };
        let comps = [ComponentInput {
            kind: ComponentKind::Kernel,
            load_addr: 0x20_0000,
            body: kernel,
        }];
        let unsigned = build_unsigned(&params, &comps);

        // Sign the signed region (the whole unsigned image) and append.
        let mut sig = std::vec::Vec::new();
        SphincsPlusSigner::sign(&sk, &unsigned, &mut sig).expect("sign");
        let image = finalize_signed(unsigned, &sig);

        let policy = VerificationPolicy {
            require_signature: true,
            running_architecture: x86_64(),
        };
        let verified = verify_image(&image, &policy, &pk).expect("verify signed");
        assert!(verified.signature_verified);
        assert_eq!(verified.entry_point, 0x20_0000);

        // A wrong key must be rejected.
        let (other_pk, _) = generate_keypair().unwrap();
        assert!(verify_image(&image, &policy, &other_pk).is_err());
    }
}

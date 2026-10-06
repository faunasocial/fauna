//! Passphrase/credential KDF primitives shared across seal surfaces.
//!
//! Owns the Argon2id (RFC 9106) parameter set + derivation every
//! passphrase-derived AEAD key in the system uses: the MUA-credential
//! wraps (`fauna-mls::wrapped_blob`, which re-exports these) and the
//! headless client credential store (`fauna-credential-store`). One
//! implementation so the parameter envelope and its DoS bounds cannot
//! drift between consumers (priority #2).
//!
//! Key-audience taxonomy: `docs/goal/architecture/key-material-hierarchy.md`.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Argon2 v1.3 (the only version we generate or accept).
pub const ARGON2_VERSION_13: u8 = 0x13;

/// A KDF parameter/derivation failure. Consumers map it into their own
/// error enums (`fauna-mls` → `WrapError::KdfFailed`, the credential
/// store → its open/seal errors).
#[derive(Debug, thiserror::Error)]
#[error("kdf: {0}")]
pub struct KdfError(pub String);

/// Argon2id parameters serialized into each blob's KDF descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Argon2idParams {
    /// Memory cost in KiB.
    pub m: u32,
    /// Iteration count.
    pub t: u32,
    /// Parallelism.
    pub p: u32,
}

impl Argon2idParams {
    /// Provisioning-time default: spec-mandated Interactive parameters
    /// (m=64 MiB, t=2, p=1).
    #[must_use]
    pub const fn interactive() -> Self {
        Self {
            m: 65_536, // KiB → 64 MiB
            t: 2,
            p: 1,
        }
    }

    /// Reject parameter triples that would panic or DoS the argon2
    /// implementation. Adversary-controlled `Argon2idParams` (e.g.
    /// from CBOR-deserialized wrapped-MSEK blobs, or a tampered
    /// credential-store file) can otherwise:
    ///
    /// - Panic in `argon2 0.5`'s `Params::new` on multiply-overflow
    ///   when `m * t` or `p * t` exceed `u32`.
    /// - Allocate up to 4 TiB at `m = u32::MAX`, breaking the consumer.
    /// - Trip argon2's `MemoryTooLittle` error when `m < 8 * p`.
    ///
    /// Bounds enforce a conservative OWASP envelope. Descriptor schemas
    /// (`m, t, p` as bare `uint`) intentionally allow future
    /// tightenings; rejecting an out-of-range triple at decode time
    /// means a future provisioner using larger parameters needs both
    /// code-path support AND bounds tightening here — no silent
    /// acceptance of values the deriver can't run.
    ///
    /// # Errors
    ///
    /// Returns [`KdfError`] if any of `m`, `t`, `p` is outside the
    /// OWASP envelope or violates argon2's `m >= 8*p` constraint.
    pub fn validate(self) -> Result<(), KdfError> {
        const MAX_MEMORY_KIB: u32 = 1_048_576; // 1 GiB
        const MAX_ITERATIONS: u32 = 100;
        const MAX_PARALLELISM: u32 = 64;

        if self.m > MAX_MEMORY_KIB {
            return Err(KdfError(format!(
                "argon2 params out of range: m={} > {}",
                self.m, MAX_MEMORY_KIB
            )));
        }
        if self.t == 0 || self.t > MAX_ITERATIONS {
            return Err(KdfError(format!(
                "argon2 params out of range: t={} not in 1..={}",
                self.t, MAX_ITERATIONS
            )));
        }
        if self.p == 0 || self.p > MAX_PARALLELISM {
            return Err(KdfError(format!(
                "argon2 params out of range: p={} not in 1..={}",
                self.p, MAX_PARALLELISM
            )));
        }
        // argon2's own requirement: m >= 8 * p.
        if self.m < self.p.saturating_mul(8) {
            return Err(KdfError(format!(
                "argon2 params out of range: m={} < 8*p={}",
                self.m,
                self.p.saturating_mul(8)
            )));
        }
        Ok(())
    }
}

/// Derive a 32-byte AEAD key from a passphrase / PLAIN credential.
///
/// `salt` is the per-blob random salt (16 bytes per the wrapped-blob
/// spec; the credential store uses the same length).
/// `password` is the raw UTF-8 bytes the user supplied.
///
/// # Errors
///
/// Returns [`KdfError`] if `params` are out of range
/// ([`Argon2idParams::validate`]), if `argon2::Params::new` rejects
/// the validated triple, or if the upstream derive fails.
pub fn derive_key_argon2id(
    password: &[u8],
    salt: &[u8],
    params: Argon2idParams,
) -> Result<Zeroizing<[u8; 32]>, KdfError> {
    use argon2::{Algorithm, Argon2, Params, Version};

    params.validate()?;

    let p = Params::new(params.m, params.t, params.p, Some(32))
        .map_err(|e| KdfError(format!("argon2 params: {e}")))?;
    let kdf = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);

    let mut out = Zeroizing::new([0u8; 32]);
    kdf.hash_password_into(password, salt, &mut *out)
        .map_err(|e| KdfError(format!("argon2 derive: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interactive default must stay the spec triple — every consumer's
    /// provisioning default couples to it.
    #[test]
    fn interactive_is_the_spec_triple() {
        let p = Argon2idParams::interactive();
        assert_eq!((p.m, p.t, p.p), (65_536, 2, 1));
        p.validate().expect("the default must validate");
    }

    /// Deterministic per (password, salt, params); distinct salt → distinct key.
    #[test]
    fn derivation_is_deterministic_and_salt_separated() {
        let params = Argon2idParams { m: 8, t: 1, p: 1 }; // minimal, fast
        let a = derive_key_argon2id(b"pw", &[1u8; 16], params).unwrap();
        let b = derive_key_argon2id(b"pw", &[1u8; 16], params).unwrap();
        let c = derive_key_argon2id(b"pw", &[2u8; 16], params).unwrap();
        assert_eq!(*a, *b);
        assert_ne!(*a, *c);
    }

    /// The DoS envelope: oversized / zeroed / m<8p triples are refused
    /// before argon2 ever sees them.
    #[test]
    fn out_of_envelope_params_are_refused() {
        for bad in [
            Argon2idParams {
                m: 2_000_000,
                t: 2,
                p: 1,
            },
            Argon2idParams { m: 64, t: 0, p: 1 },
            Argon2idParams {
                m: 64,
                t: 101,
                p: 1,
            },
            Argon2idParams { m: 64, t: 2, p: 0 },
            Argon2idParams { m: 64, t: 2, p: 65 },
            Argon2idParams { m: 8, t: 2, p: 2 }, // m < 8*p
        ] {
            assert!(bad.validate().is_err(), "{bad:?} must be refused");
            assert!(derive_key_argon2id(b"pw", &[0u8; 16], bad).is_err());
        }
    }
}

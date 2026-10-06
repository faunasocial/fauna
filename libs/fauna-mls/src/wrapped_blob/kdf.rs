//! Symmetric KDFs for credential-derived wraps.
//!
//! - PLAIN credentials → Argon2id (RFC 9106)
//! - OAUTHBEARER credentials → HKDF-SHA-256 (RFC 5869) [Task 4]
//!
//! Per spec § Argon2id parameters: provisioning-time default is
//! `m=64 MiB, t=2, p=1, salt=16 random bytes, output=32 bytes`.

use crate::wrapped_blob::format::{UnwrapError, WrapError};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

// The Argon2id parameter set + derivation live in `fauna_core::kdf` (lifted
// 2026-07-14 when the headless credential store became the second consumer —
// a credential store must not pull openmls). Re-exported here so every
// wrapped-blob consumer's `fauna_mls::wrapped_blob::Argon2idParams` import
// keeps working; the CBOR descriptor shape is unchanged.
pub use fauna_core::kdf::{ARGON2_VERSION_13, Argon2idParams};

/// Derive a 32-byte AEAD key from a PLAIN credential.
///
/// `salt` is the per-blob random salt (16 bytes per spec).
/// `password` is the raw UTF-8 bytes the MUA supplied.
///
/// # Errors
///
/// Returns `WrapError::KdfFailed` if `params` are out of range
/// (`Argon2idParams::validate`), if `argon2::Params::new` rejects
/// the validated triple, or if the upstream derive fails.
pub fn derive_key_argon2id(
    password: &[u8],
    salt: &[u8],
    params: Argon2idParams,
) -> Result<Zeroizing<[u8; 32]>, WrapError> {
    fauna_core::kdf::derive_key_argon2id(password, salt, params)
        .map_err(|e| WrapError::KdfFailed(e.to_string()))
}

/// Same as `derive_key_argon2id` but maps errors into `UnwrapError`
/// for the unwrap path (so callers don't need to match between two
/// error enums).
///
/// # Errors
///
/// Returns `UnwrapError::KdfFailed` for any failure in the underlying
/// `derive_key_argon2id`, including out-of-range parameters.
pub fn unwrap_key_argon2id(
    password: &[u8],
    salt: &[u8],
    params: Argon2idParams,
) -> Result<Zeroizing<[u8; 32]>, UnwrapError> {
    derive_key_argon2id(password, salt, params).map_err(|e| UnwrapError::KdfFailed(e.to_string()))
}

/// HKDF-SHA-256 parameters. There are no tunables — the function is
/// fully specified by `salt` (per-blob random) and `info` (context
/// binding). Carried as a unit struct for symmetry with
/// `Argon2idParams` in the `KdfParams` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HkdfSha256Params;

/// Tagged KDF parameter set. Serializes per the CDDL `KdfParams`
/// alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdfParams {
    Argon2id(Argon2idParams),
    HkdfSha256(HkdfSha256Params),
}

/// Credential input from a MUA AUTH. The two arms drive different KDFs:
/// PLAIN → Argon2id, OAUTHBEARER → HKDF-SHA-256.
#[derive(Debug, Clone, Copy)]
pub enum CredentialInput<'a> {
    Plain(&'a [u8]),
    OauthBearer(&'a [u8]),
}

/// Parse a MUA AUTH `credential_kind` string into a [`CredentialInput`].
///
/// The wire/UI contract is exactly `"plain"` (→ [`CredentialInput::Plain`],
/// driving Argon2id) and `"oauthbearer"` (→ [`CredentialInput::OauthBearer`],
/// driving HKDF-SHA-256) per `docs/goal/behavior/mail-credentials.md` § KDF
/// choice. Shared by the per-platform seal bindings (`fauna-ffi` mail surface,
/// `fauna-wasm` wrapped-blob surface) so the credential-kind string set lives
/// in one place (priority #2).
///
/// # Errors
///
/// Returns a human-readable error string for an unrecognized `kind`; both
/// bindings surface it as their own platform error string (UniFFI
/// `FfiError::General`, WASM `JsValue`).
pub fn credential_input<'a>(kind: &str, bytes: &'a [u8]) -> Result<CredentialInput<'a>, String> {
    match kind {
        "plain" => Ok(CredentialInput::Plain(bytes)),
        "oauthbearer" => Ok(CredentialInput::OauthBearer(bytes)),
        other => Err(format!(
            "unknown credential_kind {other:?}; expected \"plain\" or \"oauthbearer\""
        )),
    }
}

/// The library-default [`KdfParams`] for a `credential_kind`, used when a seal
/// caller passes no explicit KDF override: `"plain"` → Argon2id Interactive
/// ([`Argon2idParams::interactive`]), `"oauthbearer"` → HKDF-SHA-256. Per
/// `docs/goal/behavior/mail-credentials.md` § KDF choice.
///
/// Shared by the per-platform seal bindings so the default Argon2id cost can't
/// drift from [`Argon2idParams::interactive`] (priority #2).
///
/// # Errors
///
/// Returns a human-readable error string for an unrecognized `credential_kind`.
pub fn default_kdf_for(credential_kind: &str) -> Result<KdfParams, String> {
    match credential_kind {
        "plain" => Ok(KdfParams::Argon2id(Argon2idParams::interactive())),
        "oauthbearer" => Ok(KdfParams::HkdfSha256(HkdfSha256Params)),
        other => Err(format!(
            "unknown credential_kind {other:?}; expected \"plain\" or \"oauthbearer\""
        )),
    }
}

/// Per spec: `info = utf8("fauna.wrapped-blob.oauth.v1") || actor_id || credential_id`.
const HKDF_OAUTH_INFO_PREFIX: &[u8] = b"fauna.wrapped-blob.oauth.v1";

/// Derive a 32-byte AEAD key from an OAUTHBEARER token using HKDF-SHA-256.
///
/// `salt` is the per-blob random salt (16 bytes).
/// `actor_id` and `credential_id` are bound into the HKDF info string
/// for context separation.
///
/// # Errors
///
/// Returns `WrapError::KdfFailed` if HKDF-Expand rejects the requested
/// length (cannot happen at 32 bytes, but propagated defensively).
pub fn derive_key_hkdf_oauth(
    token: &[u8],
    salt: &[u8],
    actor_id: &[u8; 32],
    credential_id: &str,
) -> Result<Zeroizing<[u8; 32]>, WrapError> {
    use hkdf::Hkdf;
    use sha2::Sha256;

    let mut info = Vec::with_capacity(HKDF_OAUTH_INFO_PREFIX.len() + 32 + credential_id.len());
    info.extend_from_slice(HKDF_OAUTH_INFO_PREFIX);
    info.extend_from_slice(actor_id);
    info.extend_from_slice(credential_id.as_bytes());

    let hk = Hkdf::<Sha256>::new(Some(salt), token);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(&info, &mut *out)
        .map_err(|e| WrapError::KdfFailed(format!("hkdf expand: {e}")))?;
    Ok(out)
}

/// Unwrap-side variant; maps to `UnwrapError`.
///
/// # Errors
///
/// Returns `UnwrapError::KdfFailed` for any failure in the underlying
/// derivation.
pub fn unwrap_key_hkdf_oauth(
    token: &[u8],
    salt: &[u8],
    actor_id: &[u8; 32],
    credential_id: &str,
) -> Result<Zeroizing<[u8; 32]>, UnwrapError> {
    derive_key_hkdf_oauth(token, salt, actor_id, credential_id)
        .map_err(|e| UnwrapError::KdfFailed(e.to_string()))
}

/// Top-level dispatch: derive the AEAD key for whichever credential
/// type was supplied.
///
/// # Errors
///
/// Returns `WrapError::InvalidInput` if the credential and KDF params
/// don't match (PLAIN must use Argon2id; OAUTHBEARER must use HKDF).
/// Returns `WrapError::KdfFailed` for any underlying derivation error.
pub fn derive_key(
    cred: &CredentialInput<'_>,
    salt: &[u8],
    actor_id: &[u8; 32],
    credential_id: &str,
    params: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, WrapError> {
    match (cred, params) {
        (CredentialInput::Plain(pw), KdfParams::Argon2id(p)) => derive_key_argon2id(pw, salt, p),
        (CredentialInput::OauthBearer(tok), KdfParams::HkdfSha256(_)) => {
            derive_key_hkdf_oauth(tok, salt, actor_id, credential_id)
        }
        _ => Err(WrapError::InvalidInput(
            "credential type and KDF params must match".into(),
        )),
    }
}

/// Unwrap-side variant; maps `WrapError` → `UnwrapError`.
///
/// # Errors
///
/// Returns `UnwrapError::KdfFailed` for derivation errors and
/// `UnwrapError::InvalidFormat` for credential/KDF mismatch (decoders
/// surface this as a malformed-blob signal, not an auth signal).
pub fn unwrap_key(
    cred: &CredentialInput<'_>,
    salt: &[u8],
    actor_id: &[u8; 32],
    credential_id: &str,
    params: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, UnwrapError> {
    derive_key(cred, salt, actor_id, credential_id, params).map_err(|e| match e {
        WrapError::KdfFailed(m) => UnwrapError::KdfFailed(m),
        WrapError::InvalidInput(m) => UnwrapError::InvalidFormat(m),
        other => UnwrapError::InvalidFormat(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_defaults_match_spec() {
        let p = Argon2idParams::interactive();
        assert_eq!(p.m, 65_536);
        assert_eq!(p.t, 2);
        assert_eq!(p.p, 1);
    }

    #[test]
    fn credential_input_parses_known_kinds() {
        match credential_input("plain", b"pw").unwrap() {
            CredentialInput::Plain(b) => assert_eq!(b, b"pw"),
            other => panic!("expected Plain, got {other:?}"),
        }
        match credential_input("oauthbearer", b"tok").unwrap() {
            CredentialInput::OauthBearer(b) => assert_eq!(b, b"tok"),
            other => panic!("expected OauthBearer, got {other:?}"),
        }
    }

    #[test]
    fn credential_input_rejects_unknown_kind() {
        let err = credential_input("scram", b"x").unwrap_err();
        assert!(err.contains("unknown credential_kind"), "got: {err}");
    }

    #[test]
    fn default_kdf_for_couples_plain_to_interactive() {
        // The PLAIN default must stay coupled to Argon2idParams::interactive()
        // — this is the drift the shared helper exists to prevent.
        assert_eq!(
            default_kdf_for("plain").unwrap(),
            KdfParams::Argon2id(Argon2idParams::interactive())
        );
        assert_eq!(
            default_kdf_for("oauthbearer").unwrap(),
            KdfParams::HkdfSha256(HkdfSha256Params)
        );
    }

    #[test]
    fn default_kdf_for_rejects_unknown_kind() {
        assert!(
            default_kdf_for("scram")
                .unwrap_err()
                .contains("unknown credential_kind")
        );
    }

    #[test]
    fn derive_is_deterministic_under_same_inputs() {
        let p = Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        }; // small for fast test
        let salt = [0u8; 16];
        let pw = b"correct horse battery staple";
        let k1 = derive_key_argon2id(pw, &salt, p).unwrap();
        let k2 = derive_key_argon2id(pw, &salt, p).unwrap();
        assert_eq!(*k1, *k2);
    }

    #[test]
    fn different_salt_yields_different_key() {
        let p = Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        };
        let pw = b"correct horse battery staple";
        let k1 = derive_key_argon2id(pw, &[0u8; 16], p).unwrap();
        let k2 = derive_key_argon2id(pw, &[1u8; 16], p).unwrap();
        assert_ne!(*k1, *k2);
    }

    #[test]
    fn different_password_yields_different_key() {
        let p = Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        };
        let salt = [0u8; 16];
        let k1 = derive_key_argon2id(b"a", &salt, p).unwrap();
        let k2 = derive_key_argon2id(b"b", &salt, p).unwrap();
        assert_ne!(*k1, *k2);
    }

    /// RFC 9106 § 5.3 known-answer style test (small parameters for
    /// CI speed; we are testing that we wired argon2 correctly, not
    /// that argon2 itself is correct — that's the upstream crate's
    /// job). We exercise the `Interactive` cost in a release-only
    /// test below.
    ///
    /// Vector fixated against `argon2 v0.5` upstream output. If the
    /// dep version moves and this test fails, regenerate the literal
    /// after auditing the version diff.
    #[test]
    fn known_answer_small_params() {
        let p = Argon2idParams {
            m: 8192,
            t: 2,
            p: 1,
        };
        let salt = b"some salt 16 byt"; // 16 bytes
        let pw = b"password";
        let k = derive_key_argon2id(pw, salt, p).unwrap();
        let expected_hex = compute_or_fixate_expected(pw, salt, p);
        assert_eq!(hex::encode(*k), expected_hex);
    }

    /// Fixated expected vector for `pw=b"password", salt=b"some salt 16 byt",
    /// m=8192, t=2, p=1` against `argon2 v0.5` upstream output.
    fn compute_or_fixate_expected(_pw: &[u8], _salt: &[u8], _p: Argon2idParams) -> String {
        "2935271dc85fef69a8d005ef2b8f5b93d032e9fba9fac038ae86c05f25efa889".into()
    }

    #[test]
    #[ignore = "exercises the production Interactive cost (~100ms); run with --ignored"]
    fn interactive_cost_runs() {
        let p = Argon2idParams::interactive();
        let salt = [0u8; 16];
        let _ = derive_key_argon2id(b"realistic-password", &salt, p).unwrap();
    }

    // The per-triple `validate()` envelope tests moved to `fauna_core::kdf`
    // with the parameter set itself (one owner). What this crate still owns —
    // and `derive_does_not_panic_on_extreme_params` below pins — is that the
    // wrapped-blob surface maps every envelope refusal into
    // `WrapError::KdfFailed`.

    #[test]
    fn validate_accepts_interactive_default() {
        let p = Argon2idParams::interactive();
        assert!(p.validate().is_ok());
    }

    /// Regression: `derive_key_argon2id` MUST NOT panic on
    /// adversary-controlled params; it must return WrapError::KdfFailed.
    /// Plan §Task 16 (fuzz harnesses) will require this property.
    #[test]
    fn derive_does_not_panic_on_extreme_params() {
        let salt = [0u8; 16];
        let pw = b"x";

        let attacks = [
            Argon2idParams {
                m: u32::MAX,
                t: 1,
                p: 1,
            },
            Argon2idParams {
                m: 4096,
                t: u32::MAX,
                p: 1,
            },
            Argon2idParams {
                m: 4096,
                t: 1,
                p: u32::MAX,
            },
            Argon2idParams {
                m: 4096,
                t: 0,
                p: 1,
            },
            Argon2idParams {
                m: 4096,
                t: 1,
                p: 0,
            },
            Argon2idParams { m: 4, t: 1, p: 2 },
        ];

        for params in attacks {
            let result = derive_key_argon2id(pw, &salt, params);
            assert!(
                matches!(result, Err(WrapError::KdfFailed(_))),
                "expected KdfFailed for {params:?}, got {result:?}"
            );
        }
    }

    #[test]
    fn hkdf_derive_is_deterministic() {
        let token = b"high-entropy-token-bytes";
        let salt = [0u8; 16];
        let actor = [0x11u8; 32];
        let k1 = derive_key_hkdf_oauth(token, &salt, &actor, "cred-1").unwrap();
        let k2 = derive_key_hkdf_oauth(token, &salt, &actor, "cred-1").unwrap();
        assert_eq!(*k1, *k2);
    }

    #[test]
    fn hkdf_different_credential_id_yields_different_key() {
        let token = b"high-entropy-token-bytes";
        let salt = [0u8; 16];
        let actor = [0x11u8; 32];
        let k1 = derive_key_hkdf_oauth(token, &salt, &actor, "cred-1").unwrap();
        let k2 = derive_key_hkdf_oauth(token, &salt, &actor, "cred-2").unwrap();
        assert_ne!(*k1, *k2);
    }

    #[test]
    fn hkdf_different_actor_yields_different_key() {
        let token = b"high-entropy-token-bytes";
        let salt = [0u8; 16];
        let k1 = derive_key_hkdf_oauth(token, &salt, &[0x11; 32], "cred-1").unwrap();
        let k2 = derive_key_hkdf_oauth(token, &salt, &[0x22; 32], "cred-1").unwrap();
        assert_ne!(*k1, *k2);
    }

    #[test]
    fn dispatch_plain_uses_argon2() {
        let p = KdfParams::Argon2id(Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        });
        let cred = CredentialInput::Plain(b"pw");
        let actor = [0u8; 32];
        let k = derive_key(&cred, &[0u8; 16], &actor, "cid", p).unwrap();
        assert_eq!(k.len(), 32);
    }

    #[test]
    fn dispatch_oauth_uses_hkdf() {
        let p = KdfParams::HkdfSha256(HkdfSha256Params);
        let cred = CredentialInput::OauthBearer(b"token");
        let actor = [0u8; 32];
        let k = derive_key(&cred, &[0u8; 16], &actor, "cid", p).unwrap();
        assert_eq!(k.len(), 32);
    }

    #[test]
    fn dispatch_mismatched_kind_errors() {
        let p = KdfParams::HkdfSha256(HkdfSha256Params);
        let cred = CredentialInput::Plain(b"pw"); // mismatch with HKDF
        let actor = [0u8; 32];
        let err = derive_key(&cred, &[0u8; 16], &actor, "cid", p).unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
    }
}

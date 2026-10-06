//! Thin wrappers around `fauna_mls::wrapped_blob::seal_*`.
//!
//! The state-machine call sites use these wrappers rather than going
//! through `libs/fauna-ffi` (which forces `Vec<u8>` round-trips for
//! per-app UI). The wrappers pick the right KDF for the
//! credential kind and produce the typed blob the `NestClient`
//! trait carries on the wire.

use crate::credential::Credential;
use fauna_mls::wrapped_blob::{
    self, Argon2idParams, HkdfSha256Params, KdfParams, MlsSnapshotBlob, SubmissionToken, WrapError,
    WrappedMsekBlob, WrappedSubmissionTokenBlob,
};

/// Default Argon2id parameters for first-enable / add-credential.
/// Spec § KDF choice § PLAIN: `m = 65_536` KiB (64 MiB), `t = 2`,
/// `p = 1`. Older blobs unwrap with their own stored parameters
/// (`SerKdfParams` rides the blob).
pub fn default_argon2id() -> Argon2idParams {
    Argon2idParams {
        m: 65_536,
        t: 2,
        p: 1,
    }
}

/// KDF params for the given credential kind.
pub fn kdf_for(credential: &Credential) -> KdfParams {
    match credential {
        Credential::Plain(_) => KdfParams::Argon2id(default_argon2id()),
        Credential::OAuthBearer(_) => KdfParams::HkdfSha256(HkdfSha256Params),
    }
}

/// Seal an MSEK under the given credential. Wraps
/// `fauna_mls::wrapped_blob::seal_wrapped_msek` with the kind→KDF
/// mapping.
pub fn seal_msek_under_credential(
    msek: &[u8; 32],
    actor_id: &[u8; 32],
    credential_id: &str,
    credential: &Credential,
) -> Result<WrappedMsekBlob, WrapError> {
    let params = kdf_for(credential);
    wrapped_blob::seal_wrapped_msek(
        msek,
        actor_id,
        credential_id,
        &credential.as_input(),
        params,
    )
}

/// Seal an MLS snapshot under MSEK. Trivial passthrough; here for
/// shape uniformity with the other wrap helpers.
pub fn seal_snapshot_under_msek(
    serialized_state: &[u8],
    actor_id: &[u8; 32],
    msek: &[u8; 32],
) -> Result<MlsSnapshotBlob, WrapError> {
    wrapped_blob::seal_mls_snapshot(serialized_state, actor_id, msek)
}

/// Seal a (presumed already-signed) `SubmissionToken` under a
/// credential. The token's `actor_id` / `credential_id` fields
/// must match the wrap parameters — `seal_submission_token` rejects
/// mismatches.
pub fn seal_token_under_credential(
    token: &SubmissionToken,
    actor_id: &[u8; 32],
    credential_id: &str,
    credential: &Credential,
) -> Result<WrappedSubmissionTokenBlob, WrapError> {
    let params = kdf_for(credential);
    wrapped_blob::seal_submission_token(
        token,
        actor_id,
        credential_id,
        &credential.as_input(),
        params,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::Credential;
    use fauna_mls::wrapped_blob::unseal_wrapped_msek;
    use zeroize::Zeroizing;

    #[test]
    fn plain_seal_round_trip() {
        let msek = [0xABu8; 32];
        let actor = [0x01u8; 32];
        let cred = Credential::Plain(Zeroizing::new(b"hunter2".to_vec()));
        // Use lighter Argon2id params to keep tests fast — the prod
        // helper uses the spec's `m = 64 MiB` setting which slows
        // unit tests. Re-derive params inline.
        let params = KdfParams::Argon2id(Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        });
        let blob =
            wrapped_blob::seal_wrapped_msek(&msek, &actor, "default", &cred.as_input(), params)
                .unwrap();
        let unwrapped = unseal_wrapped_msek(&blob, &cred.as_input()).unwrap();
        assert_eq!(*unwrapped, msek);
    }

    #[test]
    fn oauth_picks_hkdf() {
        let cred = Credential::OAuthBearer(Zeroizing::new(b"tok".to_vec()));
        assert!(matches!(kdf_for(&cred), KdfParams::HkdfSha256(_)));
    }

    #[test]
    fn plain_picks_argon2id() {
        let cred = Credential::Plain(Zeroizing::new(b"pw".to_vec()));
        assert!(matches!(kdf_for(&cred), KdfParams::Argon2id(_)));
    }
}

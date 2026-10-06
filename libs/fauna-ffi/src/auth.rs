use crate::{FfiError, general_err, keypair_from_bytes};
use ed25519_dalek::Signer;

/// Signed registration request.
#[derive(uniffi::Record)]
pub struct FfiRegisterRequest {
    pub actor_id: Vec<u8>,
    pub handle: String,
    pub timestamp: u64,
    pub signature: Vec<u8>,
}

/// Build a signed registration request.
/// Signs the domain-tagged, length-prefixed
/// `fauna_protocol::account::register_signed_message`.
#[uniffi::export]
pub fn build_register_request(
    secret: Vec<u8>,
    handle: String,
    domain: String,
) -> Result<FfiRegisterRequest, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let req = fauna_client_core::auth::build_register_request(&kp, &handle, &domain);
    Ok(FfiRegisterRequest {
        actor_id: req.actor_id.to_vec(),
        handle: req.handle,
        timestamp: req.timestamp,
        signature: req.signature.to_vec(),
    })
}

/// Sign arbitrary bytes with the given Ed25519 secret. Used for pre-encoded
/// payloads where the request shape is not one of the structured `build_*`
/// helpers above (e.g. dag-cbor-encoded posts whose signature is embedded in the
/// post itself). Returns the 64-byte Ed25519 signature.
#[uniffi::export]
pub fn sign_message(secret: Vec<u8>, msg: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    Ok(kp.signing_key().sign(&msg).to_bytes().to_vec())
}

/// Result of a successful silent sign-in over the anonymous WS connection.
/// Mirrors `fauna_protocol::auth::VerifyReply`. The launch path refreshes its
/// cached `handle`/`domain`/`tier` from this; `token` is surfaced so the caller
/// may stash it alongside the direct-auth bearer.
///
/// **This is the UniFFI apps' launch mint** — challenge/verify, not the direct
/// handshake — so it is the path on which those apps learn their own session
/// id at all. It carried everything *but* `token_id` until 2026-09-20, the
/// doc comment here calling it "the opaque `token_id` the launch flow doesn't
/// surface"; not surfacing it is exactly what left the apps unable to name
/// their own session (`docs/goal/behavior/devices.md` § The client's own
/// session).
#[derive(uniffi::Record, Debug)]
pub struct FfiSilentSignInResult {
    pub token: String,
    /// Short 16-hex id of the session this mint created. **Additive** record
    /// field: the UniFFI apps consume this record and never construct it, so
    /// no call site breaks. It is how an app names its own session —
    /// marking "this session" and filling `revoke_all`'s `keep_token_id`
    /// (`docs/goal/behavior/devices.md` § The client's own session). Empty
    /// string only if a nest omitted it (every nest sends it).
    pub token_id: String,
    pub handle: String,
    pub domain: String,
    pub tier: String,
    pub expires_at: u64,
}

/// Silent sign-in over the **pre-identity (anonymous)** WS connection: the
/// `fauna.auth.challenge` → sign(`AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id`) →
/// `fauna.auth.verify` ceremony (`docs/goal/architecture/transport.md`
/// § Pre-identity). The signed message is **domain-tagged** and built by the
/// single source `fauna_protocol::auth::challenge_verify_signed_message` —
/// tagged-only since 2026-08-17, with no untagged accept path left on the nest,
/// so a hand-rolled signer that omits the tag is refused outright with
/// `fauna.auth.signature_failed`. This is the
/// WS-RPC replacement for the deleted HTTP twins `POST /api/v1/auth/{challenge,
/// verify}`. It reuses the shared `fauna_launch_machine::WsAuthConnector` — the
/// same native anonymous-connector path the linux app rides
/// (`apps/fauna-linux/src/client.rs` `do_silent_sign_in`) and the web wasm
/// `challengeVerify` mirrors — so no client hand-rolls the ceremony (#2/#3).
///
/// Outcome mapping (1:1 with `SilentChallengeOutcome`):
///  * `Ok(Some(_))` — registered actor; bearer minted + handle/domain/tier.
///  * `Ok(None)` — `fauna.auth.not_registered` (the HTTP 404 twin): the actor
///    isn't registered on this nest → the caller drops into onboarding.
///  * `Err(FfiError::NestOutdated)` — the nest booted a degraded "needs-update"
///    mode and answered `fauna.nest.outdated` (version-compatibility.md Dim 4);
///    the caller routes to a **non-retry** "update your nest" surface, never the
///    retry loop. Distinct from the generic bucket below.
///  * `Err(FfiError::General)` — every other reachability fault and an invalid
///    secret collapse into a single retryable error (the shared design rejects
///    client-side transient/terminal classification; the retry surface offers a
///    different nest).
#[fauna_uniffi_async::export]
pub async fn silent_challenge(
    nest_url: String,
    secret: Vec<u8>,
) -> Result<Option<FfiSilentSignInResult>, FfiError> {
    use fauna_launch_machine::{AuthConnector, WsAuthConnector};
    // No reach hint here: this is the direct post-launch cache-refresh path
    // (`APIClient.silentSignIn` / android's `AppLaunchVM`), not the
    // `LaunchMachine`-mediated pre-launch dial the hint policy governs
    // (`crate::connector::AuthConnector::silent_challenge` doc comment).
    silent_outcome_to_ffi(
        WsAuthConnector
            .silent_challenge(&nest_url, None, &secret)
            .await,
    )
}

/// Map a [`SilentChallengeOutcome`](fauna_launch_machine::SilentChallengeOutcome)
/// onto the FFI result, keeping the version-mismatch case
/// ([`SilentChallengeOutcome::NeedsUpdate`]) in its **own** non-retry
/// [`FfiError::NestOutdated`] variant rather than the generic retryable bucket
/// (version-compatibility.md Dim 4 — "distinguishable from transient errors").
/// Factored out of [`silent_challenge`] so the mapping is unit-testable without a
/// live WS connection.
fn silent_outcome_to_ffi(
    outcome: fauna_launch_machine::SilentChallengeOutcome,
) -> Result<Option<FfiSilentSignInResult>, FfiError> {
    use fauna_launch_machine::SilentChallengeOutcome;
    match outcome {
        SilentChallengeOutcome::Success(v) => Ok(Some(FfiSilentSignInResult {
            token: v.token,
            token_id: v.token_id,
            handle: v.handle,
            domain: v.domain,
            tier: v.tier,
            expires_at: v.expires_at,
        })),
        SilentChallengeOutcome::NotRegistered => Ok(None),
        SilentChallengeOutcome::Transient { error }
        | SilentChallengeOutcome::SecretInvalid { error } => Err(FfiError::General { msg: error }),
        // `fauna.nest.outdated` — degraded nest. Its own variant so native
        // apps route to a non-retry "update your nest" surface, not the retry
        // loop the generic error feeds (version-compatibility.md Dim 4).
        SilentChallengeOutcome::NeedsUpdate { message } => {
            Err(FfiError::NestOutdated { msg: message })
        }
        // Changed/withdrawn pinned identity — its own variant so native apps
        // block on the `launch_identity_changed` warning surface (explicit
        // re-trust via `forget_nest_identity_pin` + a fresh sign-in, or the
        // wizard fallthrough), never the retry loop (security.md § Transport
        // trust).
        // `fork` (rotation-chain fork evidence) is deliberately NOT a new
        // `FfiError` field yet — the same build-risk trade the Superseded arm
        // below carried until an app actually rendered its affordance, and no
        // app renders a fork one; the FFI apps read the machine's
        // `LaunchSnapshot::identity_fork` when their fork rendering lands.
        SilentChallengeOutcome::IdentityChanged {
            host,
            pinned_hex,
            seen_hex,
            fork: _,
        } => Err(FfiError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        }),
        // The identity was succeeded (`identity-succession.md` § Propagation →
        // *Own device fleet*). This rode the **generic** bucket until
        // 2026-08-21, deliberately: `FfiError` is UniFFI-exported to
        // apple/android/windows, none of which rendered the import affordance,
        // so a typed variant would have been a build risk on two other machines
        // in exchange for nothing those apps could show. That trade flipped with
        // apple's ceremony leg — the same landing that gives this crate
        // `recovery.rs` — so the refusal is typed now and carries the successor
        // id as a field rather than only inside a message a surface would have
        // to parse. The `#[error]` string is verbatim what the generic arm
        // formatted, so an app still rendering `error.localizedDescription`
        // shows exactly what it showed before. tui and linux do not ride this
        // path: they read the successor off `LaunchSnapshot::superseded_successor`.
        SilentChallengeOutcome::Superseded { new_actor_id_hex } => {
            Err(FfiError::IdentitySuperseded { new_actor_id_hex })
        }
        // A locked account (`fauna.auth.account_locked`) — typed, with the
        // unlock time as a field, so a native app can stop retrying until then
        // and name the time (`devices.md` § The locked state). It rode the
        // generic bucket until the lockout's app half landed, the same trade
        // `Superseded` carried until 2026-08-21. The launch path reads the same
        // time off `LaunchSnapshot::locked_until_secs`.
        SilentChallengeOutcome::Locked { locked_until_secs } => {
            Err(FfiError::AccountLocked { locked_until_secs })
        }
    }
}

/// A bearer token minted over the silent challenge, its session id, plus its
/// expiry (unix **seconds**, on this device's clock — anchored at receipt,
/// `login.md` § Token lifetime on the client's clock). FFI mirror of
/// [`fauna_client::ws_challenge_bearer::MintedBearer`].
#[derive(uniffi::Record)]
pub struct FfiBearerToken {
    pub token: String,
    /// Short 16-hex id of the session this mint created. **Additive** record
    /// field: the UniFFI apps consume this record and never construct it, so
    /// no call site breaks. It is how an app names its own session —
    /// marking "this session" and filling `revoke_all`'s `keep_token_id`
    /// (`docs/goal/behavior/devices.md` § The client's own session). Empty
    /// string only if a nest omitted it (every nest sends it).
    pub token_id: String,
    pub expires_at: u64,
}

/// Mint a runtime bearer over the pre-identity WS-RPC silent challenge
/// (`fauna.auth.challenge` + `fauna.auth.verify`) — the ceremony `login.md`
/// § When to use which assigns to every bearer an app holds, refresh included:
/// it signs the nest's nonce rather than a client timestamp, so a device whose
/// clock is hours wrong still mints. Runs over a one-shot anonymous WS
/// connection, graduates the TLS channel binding on `https://`, and returns the
/// 1-hour bearer with `expires_at` on this device's clock.
///
/// For the native UniFFI apps (Apple / Android / Windows) that maintain a
/// bearer for their residual native HTTP calls separately from
/// [`FfiNestClient`](crate::nest_client::FfiNestClient)'s embedded WS auth. It
/// rides the shared
/// `fauna_client::ws_challenge_bearer::mint_bearer_over_silent_challenge`, so
/// every app mints over the same shared-Rust path (#2/#3) — no hand-rolled
/// per-app ceremony.
#[fauna_uniffi_async::export]
pub async fn mint_bearer(nest_url: String, secret: Vec<u8>) -> Result<FfiBearerToken, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let minted = fauna_client::ws_challenge_bearer::mint_bearer_over_silent_challenge(
        &nest_url,
        kp.signing_key(),
    )
    .await
    .map_err(general_err)?;
    Ok(FfiBearerToken {
        token: minted.token,
        token_id: minted.token_id,
        expires_at: minted.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_launch_machine::SilentChallengeOutcome;
    use fauna_protocol::auth::VerifyReply;

    fn verify_reply() -> VerifyReply {
        VerifyReply {
            token: "tok".into(),
            token_id: "id".into(),
            handle: "alice".into(),
            domain: "example.com".into(),
            tier: "free".into(),
            expires_at: 42,
            ..Default::default()
        }
    }

    #[test]
    fn needs_update_maps_to_distinct_nest_outdated_variant() {
        // Track A core (version-compatibility.md Dim 4): a degraded nest's
        // `fauna.nest.outdated` must surface as a *distinct* non-retry FFI signal,
        // NOT collapse into the generic `General` error that native apps retry.
        let out = silent_outcome_to_ffi(SilentChallengeOutcome::NeedsUpdate {
            message: "this nest must be updated".into(),
        });
        match out {
            Err(FfiError::NestOutdated { msg }) => assert_eq!(msg, "this nest must be updated"),
            other => panic!("expected NestOutdated, got {other:?}"),
        }
    }

    /// A locked account crosses the boundary as its own variant, carrying the
    /// unlock time as a field (`devices.md` § The locked state) — in the
    /// generic bucket a native app's launch gate retried it like a network
    /// blip, re-signing a ceremony the nest must refuse until that time. The
    /// description stays what the generic arm formatted.
    #[test]
    fn locked_maps_to_its_own_variant_carrying_the_unlock_time() {
        let out = silent_outcome_to_ffi(SilentChallengeOutcome::Locked {
            locked_until_secs: 1_700_086_400,
        });
        match out {
            Err(e @ FfiError::AccountLocked { locked_until_secs }) => {
                assert_eq!(locked_until_secs, 1_700_086_400);
                assert_eq!(
                    e.to_string(),
                    "account locked until 1700086400 (Unix seconds)"
                );
            }
            other => panic!("expected AccountLocked, got {other:?}"),
        }
    }

    #[test]
    fn transient_and_secret_invalid_stay_generic() {
        // The retryable / terminal-generic faults keep collapsing to `General`;
        // only the version mismatch gets its own actionable variant.
        for outcome in [
            SilentChallengeOutcome::Transient {
                error: "connect refused".into(),
            },
            SilentChallengeOutcome::SecretInvalid {
                error: "bad secret".into(),
            },
        ] {
            match silent_outcome_to_ffi(outcome) {
                Err(FfiError::General { .. }) => {}
                other => panic!("expected General, got {other:?}"),
            }
        }
    }

    #[test]
    fn not_registered_maps_to_none() {
        match silent_outcome_to_ffi(SilentChallengeOutcome::NotRegistered) {
            Ok(None) => {}
            other => panic!("expected Ok(None), got {other:?}"),
        }
    }

    #[test]
    fn success_carries_identity_fields() {
        match silent_outcome_to_ffi(SilentChallengeOutcome::Success(verify_reply())) {
            Ok(Some(r)) => {
                assert_eq!(r.token, "tok");
                assert_eq!(r.handle, "alice");
                assert_eq!(r.domain, "example.com");
                assert_eq!(r.tier, "free");
                assert_eq!(r.expires_at, 42);
            }
            other => panic!("expected Ok(Some(_)), got {other:?}"),
        }
    }
}

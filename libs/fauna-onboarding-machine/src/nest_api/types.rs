//! Wire-level request/response/error types for the `NestApi` trait.
//!
//! Shapes mirror the nest's WS-RPC wire types (`fauna_protocol::*`) that the
//! production `WsRpcNestApi` mapping core composes; their round-trip is pinned
//! by `bins/fauna-nest/tests/onboarding_ws_rpc_roundtrip.rs`.
//!
//! The six per-endpoint error enums here are *refinements* of
//! [`fauna_nest_http::ApiError`] — the shared two-channel nest-HTTP taxonomy
//! (a non-2xx `Status { code, message }`, or a `Transport(_)` failure): each
//! carries `From<ApiError>` (below). `WsRpcNestApi` keeps its own
//! per-endpoint code dispatch in `ws_rpc_impl.rs` (`fauna.invite.closed →
//! Closed`, `…not_found → NotFound`, … — semantics a generic `From` can't
//! know), so those conversions are the *default* lowering, not a rewrite. The
//! `NestApi` trait stays distinct from `fauna-nest-http`'s generic
//! `NestContentApi` — fixed eight-method shape, rich typed responses, wasm
//! (design tracked internally).

use fauna_core::secret::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Shared handle to a `(host, expected nest_actor_id)` pair — the Axis-2
/// pre-resolved identity root a client-provisioned box must present on first
/// contact (security.md § Transport trust). Written by `OnboardingMachine`
/// while an injected deployment seed is live, read by `WsNestApi::core()` to
/// graduate pre-claim connections against it.
pub type ExpectedNestIdentity = std::sync::Arc<std::sync::RwLock<Option<(String, [u8; 32])>>>;

/// The NAT axis (`Public` / `Private`) committed by `submit_nat_mode`. The
/// canonical definition lives in `fauna-core` (re-exported via
/// `crate::state`); this re-export keeps `crate::nest_api::NodeMode`
/// available alongside its `NatModeBody` consumer.
pub use crate::state::NodeMode;

/// The shared nest-HTTP error taxonomy our per-endpoint enums refine. Re-
/// exported so `crate::nest_api::ApiError` resolves alongside them.
pub use fauna_nest_http::ApiError;
/// The handle-check silent-sign-in outcome — the shared challenge/verify
/// ceremony result, re-exported from its `fauna-protocol::auth` home so the
/// `NestApi::silent_challenge` method and the machine's probe can name it
/// under the `crate::nest_api` path the rest of this module uses.
pub use fauna_protocol::auth::SilentChallengeOutcome;

/// Wire shape of `GET /api/v1/setup-status`. The endpoint is
/// unauthenticated and returns `{"claimed": bool, "node_mode": "..."}`.
///
/// `Default` = the fresh-unclaimed-nest shape (`claimed: false`, node mode
/// unset); fixtures grow via struct-update (`..Default::default()`) rather
/// than hand-listing every field, so two branches that independently add a
/// field merge cleanly instead of colliding on the grown axis.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SetupStatus {
    #[serde(default)]
    pub claimed: bool,
    /// The nest's resolved NAT axis (`public` / `private`) — the client-set
    /// `nest_nat_mode` row falling back to the `FAUNA_MODE` seed. `None` until
    /// the setup-status probe has resolved. Seeds the `nat_mode_choice` pre-selection
    /// (`docs/goal/behavior/onboarding.md` § 3b-bis).
    #[serde(default)]
    pub node_mode: Option<NodeMode>,
}

/// `derive(thiserror::Error)` gives `Display` so this can cross the UniFFI
/// boundary as a `Result` error (`OnboardingMachine::probe_setup_status_at` is
/// `#[uniffi::export]`); the `uniffi::Error` derive emits the FFI scaffolding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum ProbeError {
    // NB: the field is `reason`, not `cause` — a `uniffi::Error` variant field
    // named `cause` generates a Kotlin `val cause` that illegally hides
    // `kotlin.Throwable.cause`, breaking every Android/native app build.
    #[error("transient setup-status probe failure: {reason}")]
    Transient { reason: String },
    #[error("invalid setup-status probe response: {reason}")]
    InvalidResponse { reason: String },
    /// The connect-stage first-contact trust graduation failed with an
    /// identity at stake — a held first-contact root, or a changed TOFU pin
    /// (`security.md` § Pre-claim surfacing). Terminal: never retried, never
    /// rendered as a transient — the "Almost ready" poll and the provisioning
    /// claim substep stop on it instead of resting on the DNS message.
    #[error("nest identity mismatch: {reason}")]
    IdentityMismatch { reason: String },
}

/// Projection of the `fauna.auth.claim_admin` reply
/// (`fauna_protocol::claim::ClaimAdminReply`), with the optional shape the
/// wizard needs (token / expires_at / domain are optional so the trait can
/// also represent the "ok=false" branch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAdminResponse {
    pub ok: bool,
    pub token: Option<String>,
    pub expires_at: Option<i64>,
    pub domain: Option<String>,
    /// The nest's deployment signing seed (64-char hex Ed25519), handed off on
    /// the claim-an-existing-box path so the claiming admin's client can custody
    /// it off-box for total-box-loss recovery (`box-recovery.md` § Mechanism —
    /// claim-an-existing-box). `None` when the nest holds no signing key. The machine stashes it into `State.deployment_seed`; no client
    /// consumes it (capture is the custody leg's reconcile, `box-recovery.md`
    /// § The plane-era recovery floor, *(c)*).
    ///
    /// Held as [`SecretString`] (zeroize-on-drop + redacted `Debug`) — the
    /// irreplaceable nest identity, hardened uniformly. Wire
    /// unchanged: `SecretString` serializes identically to `String`.
    pub deployment_seed: Option<SecretString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimAdminError {
    /// 4xx — bad code, signature mismatch. Surface as
    /// `ClaimCodeState::Invalid { reason }`.
    Invalid { reason: String },
    /// 5xx, 408, 429, network errors. Transient.
    Transient { cause: String },
    /// Local validation failure (corrupt secret hex). Surface as
    /// `Error { transient: false, ... }`.
    InvalidIdentity { cause: String },
    /// The nest could not READ its own claim-code file (it is PRESENT but
    /// unreadable — a misprovisioned box, e.g. a root-owned `/data/claim-code`
    /// the uid-1000 nest can't open, or a read-only `:ro` seed bind onto
    /// `/data/claim-code`). Terminal (a retry won't help until the box's
    /// claim-code file is fixed — the nest's Docker entrypoint self-heals the
    /// ownership on restart) and NOT a wrong code, so it gets its own dedicated
    /// message rather than the `Invalid`/already-claimed rendering. Surfaced from
    /// the nest's `fauna.auth.claim_code_unreadable`.
    Misprovisioned { cause: String },
    /// The nest genuinely already has an admin — the code itself was fine, no
    /// different code would succeed. Distinct from `Invalid` so the client can
    /// show its own dedicated `onboarding.claim_code.error.already_claimed`
    /// message instead of leaking the raw wire code through `Invalid`'s
    /// `{reason}` substitution (previously the only path: `already_claimed` fell
    /// into the generic `Some(_) => Invalid` mapping with no `.details`, so
    /// `reason_of()` fell back to the bare `fauna.auth.already_claimed` code).
    /// Surfaced from the nest's `fauna.auth.already_claimed` — the within-grace
    /// pending-factory-reset honor arm's failed-dispatch exit
    /// (`common.md` § Client-state recoverability) is the primary path that
    /// reaches this today.
    AlreadyClaimed,
    /// The connect-stage first-contact trust graduation failed — the box did
    /// not prove the identity this client requires for the host
    /// (`security.md` § Pre-claim surfacing). Terminal, never retryable: the
    /// claim-code page renders its terminal error, and `claim_provisioned_box`
    /// stops instead of polling again.
    IdentityMismatch { reason: String },
}

/// Wire body of `POST /api/v1/invite-requests`. Mirrors the canonical
/// signed payload built by `invite_request_signed_message` in machine.rs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteRequestBody {
    pub actor_id: String,
    pub handle: String,
    pub message: String,
    pub timestamp: u64,
    pub signature: String,
    /// The app's age claim (`family-safety.md` § The account age band) —
    /// whatever [`crate::OnboardingMachine::set_age_claim`] holds at submit
    /// time; `None` from every app without a store age signal. Outside the
    /// signed message by design (the platform attestation binds it).
    pub age_claim: Option<fauna_protocol::age::AgeClaim>,
}

/// Wire shape returned by `POST /api/v1/invite-requests` AND
/// `GET /api/v1/invite-requests/{actor_id}/status`. Matches the
/// internal `InviteSubmitResp` that used to live in machine.rs — pulled
/// out to the trait so `state_from_resp` (the existing snapshot
/// builder) can consume it directly without a conversion layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRequestResponse {
    pub id: i64,
    pub status: String, // "approved" | "pending" | "denied"
    #[serde(default)]
    pub denial_reason: Option<String>,
    #[serde(default)]
    pub quota: Option<crate::snapshots::InviteQuota>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InviteRequestError {
    /// 403 — admission closed.
    Closed { cause: String },
    /// 429 — rate limited.
    RateLimited { cause: String },
    /// 404 — invite request not found (recheck only).
    NotFound,
    /// 5xx / network — retry later.
    Transient { cause: String },
    /// 2xx with a body the wizard can't parse.
    Malformed { cause: String },
    /// The connect-stage first-contact trust graduation failed
    /// (`security.md` § Pre-claim surfacing). Terminal, never retryable.
    IdentityMismatch { reason: String },
    /// `fauna.account.actor_exists` — the nest already holds this key as an
    /// account, a suspended one included (`login.md` § Errors, the
    /// registration doors' ruling). Terminal: only the admin's Restore lifts
    /// it, so it must never render as a "try again" (submit only).
    AlreadyRegistered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteCodeVerification {
    pub invite_id: String,
    /// The guardian's handle if this code carries a supervised designation
    /// (`family-safety.md` § Wire & data shape — `invite-code-supervised-notice`,
    /// rendered before redemption). `None` = an ordinary code.
    pub supervised_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InviteCodeError {
    /// Non-2xx response (most commonly 404 — code not recognized).
    Invalid { reason: String },
    /// Network / connection error.
    Transient { cause: String },
    /// 2xx with an unparseable body.
    Malformed { cause: String },
    /// The connect-stage first-contact trust graduation failed
    /// (`security.md` § Pre-claim surfacing). Terminal, never retryable.
    IdentityMismatch { reason: String },
}

/// Wire body of `POST /api/v1/register`. Optional `invite_code` field
/// distinguishes the OOB path (`Some`) from the approved-invite path
/// (`None`); see `redeem_invite` in machine.rs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterBody {
    pub actor_id: String,
    pub handle: String,
    pub timestamp: u64,
    pub signature: String,
    pub invite_code: Option<String>,
    /// The app's age claim — see [`InviteRequestBody::age_claim`].
    pub age_claim: Option<fauna_protocol::age::AgeClaim>,
}

/// `fauna.account.age_nonce`'s reply — the single-use nonce the mobile app
/// feeds into its platform attestation (`family-safety.md` § The account age
/// band; the payload contract is `fauna_protocol::age::age_claim_signed_message`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgeNonce {
    /// 64-char hex of the 32-byte nonce.
    pub nonce_hex: String,
    /// Seconds the nonce stays redeemable.
    pub expires_in_secs: u64,
    /// The attestation platforms this nest can verify
    /// (`fauna_protocol::age::AgeNonceReply::attestation_platforms`); empty
    /// for an unarmed nest, which holds no verifier — *verifies nothing*.
    #[serde(default)]
    pub attestation_platforms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgeNonceError {
    /// The nest refused the mint (throttled, or any refusal of the kind).
    Refused { reason: String },
    /// Network / connection error.
    Transient { cause: String },
    /// The connect-stage first-contact trust graduation failed
    /// (`security.md` § Pre-claim surfacing). Terminal, never retryable.
    IdentityMismatch { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegisterError {
    /// Any non-2xx / network failure. The wizard treats register as
    /// "succeeded or didn't"; the cause carries the diagnostic.
    Failed { cause: String },
    /// The connect-stage first-contact trust graduation failed
    /// (`security.md` § Pre-claim surfacing). Terminal, never retryable.
    IdentityMismatch { reason: String },
}

/// Wire body of the `fauna.setup.nat_mode` commit. Built by
/// `build_signed_nat_mode_body` in machine.rs; the signature covers the
/// nest-bound canonical bytes `mode_wire_str || "\n" || actor_id_hex ||
/// "\n" || timestamp_decimal || "\n" || nest_id_hex`
/// (`fauna_protocol::nat_mode::nat_mode_signed_message`). Mutable
/// server-side: any valid admin-signed set upserts the `nest_nat_mode` row —
/// no conflict reply. Per `docs/goal/behavior/onboarding.md` § 3b-bis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NatModeBody {
    pub mode: NodeMode,
    pub actor_id: String,
    pub timestamp: i64,
    pub signature: String,
    /// The receiving nest's identity (`NatModeRequest::nest_id`), 64-hex,
    /// read possession-proven off the commit connection before signing.
    pub nest_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NatModeError {
    /// 4xx-class reject (`not_claimed`, `signature_failed`,
    /// `invalid_request`, `forbidden`). Surface as
    /// `NatModeState::Error { transient: false, ... }`.
    Invalid { reason: String },
    /// Transport / internal failure. Transient — user can retry (the set is
    /// mutable, resubmit is always safe).
    Transient { cause: String },
    /// The connect-stage first-contact trust graduation failed
    /// (`security.md` § Pre-claim surfacing). Terminal, never retryable.
    IdentityMismatch { reason: String },
}

// ---------------------------------------------------------------------------
// `From<ApiError>` — the default lowering of the shared taxonomy onto each
// per-endpoint enum. `WsRpcNestApi::*` keeps its own (richer) code
// dispatch; these are for callers funnelling a generic `ApiError` and they
// pin the refinement relationship. A 4xx maps to the enum's "client error"
// variant where it has one (carrying the nest's `{"error": ...}` message),
// 5xx + everything else to its transient variant; `Transport(_)` is always
// transient. Variants that can only arise from a local / parse failure
// (`ProbeError::InvalidResponse`, `ClaimAdminError::InvalidIdentity`,
// `InviteRequestError::Malformed`, `InviteCodeError::Malformed`) are never
// produced here.
//
// `ApiError::NestIdentityChanged` maps to each enum's `IdentityMismatch`
// variant: the typed identity verdict must survive every seam between
// detection and surface (`security.md` § Pre-claim surfacing, which extends
// the § Post-auth surfacing plumbing rule to the wizard's anonymous path).
// The variant is a bearer-path artifact and effectively unreachable on these
// pre-identity endpoints — the wizard's own connect-stage detection happens in
// `WsNestApi::core`, which classifies at the seam — but it is mapped honestly
// anyway so no future funnel revives the flattening this block used to pin
// ("takes each enum's transient arm", retired 2026-08-30).
// Carrying the variant is NOT the "second, softer per-app shape" § Post-auth
// surfacing forbids: each page renders its one machine-shared terminal error
// state — plumbing, not a new surface.
//
// `ApiError::SignInRefused` is the same kind of bearer-path artifact — a held
// identity the nest stopped signing in, raised only by `LaunchMachineBearer` —
// and unreachable on these pre-identity endpoints; each enum takes it as it
// takes a 403, the refusal it is. `ApiError::Superseded` (the held identity
// was succeeded) is the same artifact and is taken the same way.
// ---------------------------------------------------------------------------

fn status_msg(code: u16, message: &str) -> String {
    format!("status {code}: {message}")
}

impl From<ApiError> for ProbeError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Status { code, message } => ProbeError::Transient {
                reason: status_msg(code, &message),
            },
            ApiError::Transport(reason) => ProbeError::Transient { reason },
            e @ ApiError::NestIdentityChanged { .. } => ProbeError::IdentityMismatch {
                reason: e.to_string(),
            },
            e @ (ApiError::SignInRefused | ApiError::Superseded { .. }) => ProbeError::Transient {
                reason: e.to_string(),
            },
        }
    }
}

impl From<ApiError> for ClaimAdminError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Status { code, message } if (400..500).contains(&code) => {
                ClaimAdminError::Invalid { reason: message }
            }
            ApiError::Status { code, message } => ClaimAdminError::Transient {
                cause: status_msg(code, &message),
            },
            ApiError::Transport(cause) => ClaimAdminError::Transient { cause },
            e @ ApiError::NestIdentityChanged { .. } => ClaimAdminError::IdentityMismatch {
                reason: e.to_string(),
            },
            e @ (ApiError::SignInRefused | ApiError::Superseded { .. }) => {
                ClaimAdminError::Invalid {
                    reason: e.to_string(),
                }
            }
        }
    }
}

impl From<ApiError> for InviteRequestError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Status { code: 403, message } => {
                InviteRequestError::Closed { cause: message }
            }
            ApiError::Status { code: 429, message } => {
                InviteRequestError::RateLimited { cause: message }
            }
            ApiError::Status { code: 404, .. } => InviteRequestError::NotFound,
            ApiError::Status { code, message } => InviteRequestError::Transient {
                cause: status_msg(code, &message),
            },
            ApiError::Transport(cause) => InviteRequestError::Transient { cause },
            e @ ApiError::NestIdentityChanged { .. } => InviteRequestError::IdentityMismatch {
                reason: e.to_string(),
            },
            e @ (ApiError::SignInRefused | ApiError::Superseded { .. }) => {
                InviteRequestError::Closed {
                    cause: e.to_string(),
                }
            }
        }
    }
}

impl From<ApiError> for InviteCodeError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Status { message, .. } => InviteCodeError::Invalid { reason: message },
            ApiError::Transport(cause) => InviteCodeError::Transient { cause },
            e @ ApiError::NestIdentityChanged { .. } => InviteCodeError::IdentityMismatch {
                reason: e.to_string(),
            },
            e @ (ApiError::SignInRefused | ApiError::Superseded { .. }) => {
                InviteCodeError::Invalid {
                    reason: e.to_string(),
                }
            }
        }
    }
}

impl From<ApiError> for RegisterError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Status { code, message } => RegisterError::Failed {
                cause: status_msg(code, &message),
            },
            ApiError::Transport(cause) => RegisterError::Failed { cause },
            e @ ApiError::NestIdentityChanged { .. } => RegisterError::IdentityMismatch {
                reason: e.to_string(),
            },
            e @ (ApiError::SignInRefused | ApiError::Superseded { .. }) => RegisterError::Failed {
                cause: e.to_string(),
            },
        }
    }
}

impl From<ApiError> for NatModeError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Status { code, message } if code < 500 => NatModeError::Invalid {
                reason: status_msg(code, &message),
            },
            ApiError::Status { code, message } => NatModeError::Transient {
                cause: status_msg(code, &message),
            },
            ApiError::Transport(cause) => NatModeError::Transient { cause },
            e @ ApiError::NestIdentityChanged { .. } => NatModeError::IdentityMismatch {
                reason: e.to_string(),
            },
            e @ (ApiError::SignInRefused | ApiError::Superseded { .. }) => NatModeError::Invalid {
                reason: e.to_string(),
            },
        }
    }
}

/// How the pre-identity escrow restore behind `recovery_entry` can end
/// (`onboarding.md` § 1 Identity; the ceremony itself is
/// `fauna_client_recovery::restore::restore_seed`).
///
/// The split is by **what the screen tells the user to do next**, which is why
/// it is narrower than [`fauna_client_recovery::RecoveryError`]: that taxonomy
/// separates refusals by which step raised them, while a restore screen only
/// ever routes four ways — re-create the kit from a signed-in device, import
/// the successor identity, fix what was typed, or retry.
///
/// No `uniffi::Error` derive yet, deliberately: the FFI/web faces for this
/// screen are their own tracked leg, and the derive is only earned once one of
/// them exports a function returning it (a uniffi type name is global across
/// the flat Swift/C# module, so it is minted with its consumer, not ahead).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum RestoreSeedError {
    /// No escrow blob rests for this account, so phrase-only recovery is
    /// unavailable until a signed-in device re-creates the kit. The **honest
    /// answer**, not a fault (`identity-succession.md` § Seed escrow →
    /// *Lifecycle on the nest*). An identity that registered no RecoveryKey at
    /// all funnels here too: same message, same and only remedy.
    #[error("no recovery blob rests for this account")]
    NoEscrow,
    /// The identity was succeeded — route to the identity import, uniform with
    /// every other superseded refusal. `successor` is the 64-hex actor id the
    /// refusal named; it is **unverified**, so a screen must not present it as
    /// this account's new identity until the succession chain confirms it.
    #[error("this identity was succeeded by {successor}")]
    Superseded { successor: String },
    /// The handle resolved to a nest, but that nest knows no such account.
    #[error("no account named {handle} on that nest")]
    AccountUnknown { handle: String },
    /// Neither the payload nor the account field named an account to recover —
    /// a bare 64-hex secret does not identify one, and guessing is not an
    /// option (the blob is AAD-bound to the actor).
    #[error("this recovery kit does not name an account — enter the account being recovered")]
    AccountUnnamed,
    /// The nest declined the kit: a replaced/retired kit still signs valid
    /// bytes that authorize nothing, or the phrase belongs to another account.
    #[error("that recovery kit was refused: {reason}")]
    Refused { reason: String },
    /// The nest could not be reached or resolved. The one variant a screen may
    /// invite the user to retry as-is.
    #[error("could not reach that account's nest: {reason}")]
    Transient { reason: String },
    /// The connect-stage first-contact trust graduation failed — the server
    /// reached is not the nest this client requires for the host
    /// (`security.md` § Pre-claim surfacing). Terminal, never retryable.
    #[error("nest identity mismatch: {reason}")]
    IdentityMismatch { reason: String },
}

/// What a phrase-only restore recovered — the FFI-friendly shape of
/// `fauna_client_recovery::RestoredSeed` (hex strings rather than byte arrays,
/// uniform with `silent_challenge`/`claim_admin` and the wizard's own
/// `imported_secret`; this path introduces no second custody convention).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoredIdentity {
    /// The account's identity seed, 64-hex.
    pub seed_hex: String,
    /// Predecessor identities whose seeds rode in the blob's additive section,
    /// oldest-sealed first. Empty on every pre-succession kit — the ordinary
    /// case — and on every blob re-put after the corpus re-seal completed
    /// (`identity-succession.md` § Seed escrow).
    pub predecessors: Vec<RestoredPredecessorSeed>,
    /// Set when a predecessor section was **present but did not open** —
    /// tampering or corruption. Deliberately distinct from an empty
    /// `predecessors`: the account came back either way, but here a corpus
    /// still sealed under a predecessor just became unopenable, and only the
    /// user can act on that. A screen must surface it; swallowing it turns
    /// irrecoverable data loss into silence.
    pub predecessors_unreadable: Option<String>,
}

/// One predecessor identity recovered from the escrow blob's additive section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RestoredPredecessorSeed {
    /// The predecessor's 64-hex actor id — which corpus this seed opens.
    pub actor_id_hex: String,
    /// That identity's seed, 64-hex — a full root identity seed opening a
    /// whole sealed corpus, same custody rule as `State::generated_secret`/
    /// `imported_secret` (`key-material-hierarchy.md` § Carrier shape: this
    /// field is the third member of that family, held here rather than
    /// dropped after read only because the client has not yet persisted it).
    /// Held as [`SecretString`] (zeroize-on-drop + redacted `Debug`). Wire
    /// unchanged: `SecretString` serializes identically to `String`.
    pub seed_hex: SecretString,
}

/// `key-material-hierarchy.md` § Carrier shape pin — reverting `seed_hex` to
/// a bare `String` fails the build here (rather than only wherever a call
/// site happens to be strictly typed), same discipline as `State`'s two
/// pins (`state.rs`).
const _RESTORED_PREDECESSOR_SEED_IS_REDACTED: fn(&RestoredPredecessorSeed) -> &SecretString =
    |p| &p.seed_hex;

#[cfg(test)]
mod from_api_error_tests {
    use super::*;

    fn status(code: u16) -> ApiError {
        ApiError::Status {
            code,
            message: "boom".into(),
        }
    }

    #[test]
    fn invite_request_error_discriminates_status_codes() {
        assert!(matches!(
            InviteRequestError::from(status(403)),
            InviteRequestError::Closed { .. }
        ));
        assert!(matches!(
            InviteRequestError::from(status(429)),
            InviteRequestError::RateLimited { .. }
        ));
        assert!(matches!(
            InviteRequestError::from(status(404)),
            InviteRequestError::NotFound
        ));
        assert!(matches!(
            InviteRequestError::from(status(500)),
            InviteRequestError::Transient { .. }
        ));
        assert!(matches!(
            InviteRequestError::from(ApiError::Transport("net".into())),
            InviteRequestError::Transient { .. }
        ));
    }

    #[test]
    fn claim_and_storage_split_4xx_from_5xx() {
        assert!(matches!(
            ClaimAdminError::from(status(400)),
            ClaimAdminError::Invalid { .. }
        ));
        assert!(matches!(
            ClaimAdminError::from(status(503)),
            ClaimAdminError::Transient { .. }
        ));
        assert!(matches!(
            NatModeError::from(status(403)),
            NatModeError::Invalid { .. }
        ));
        assert!(matches!(
            NatModeError::from(status(500)),
            NatModeError::Transient { .. }
        ));
        assert!(matches!(
            NatModeError::from(ApiError::Transport("net".into())),
            NatModeError::Transient { .. }
        ));
    }

    #[test]
    fn invite_code_and_register_and_probe_funnel() {
        assert!(matches!(
            InviteCodeError::from(status(404)),
            InviteCodeError::Invalid { .. }
        ));
        assert!(matches!(
            InviteCodeError::from(ApiError::Transport("x".into())),
            InviteCodeError::Transient { .. }
        ));
        assert!(matches!(
            RegisterError::from(status(500)),
            RegisterError::Failed { .. }
        ));
        assert!(matches!(
            ProbeError::from(status(502)),
            ProbeError::Transient { .. }
        ));
    }

    /// The typed identity verdict survives the generic funnel on every enum —
    /// `NestIdentityChanged` must never land in a transient/failed arm
    /// (`security.md` § Pre-claim surfacing; this funnel used to flatten it).
    #[test]
    fn nest_identity_changed_maps_to_identity_mismatch_everywhere() {
        fn verdict() -> ApiError {
            ApiError::NestIdentityChanged {
                host: "nest.example.test".into(),
                pinned_hex: "aa".repeat(32),
                seen_hex: Some("bb".repeat(32)),
            }
        }
        assert!(matches!(
            ProbeError::from(verdict()),
            ProbeError::IdentityMismatch { .. }
        ));
        assert!(matches!(
            ClaimAdminError::from(verdict()),
            ClaimAdminError::IdentityMismatch { .. }
        ));
        assert!(matches!(
            InviteRequestError::from(verdict()),
            InviteRequestError::IdentityMismatch { .. }
        ));
        assert!(matches!(
            InviteCodeError::from(verdict()),
            InviteCodeError::IdentityMismatch { .. }
        ));
        assert!(matches!(
            RegisterError::from(verdict()),
            RegisterError::IdentityMismatch { .. }
        ));
        assert!(matches!(
            NatModeError::from(verdict()),
            NatModeError::IdentityMismatch { .. }
        ));
    }
}

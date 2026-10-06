//! The recovery error taxonomy — one shared mapping from the nest's refusal
//! codes onto the states a client UI actually branches on.
//!
//! Every one of these variants corresponds to a screen state the recovery-kit
//! and recovery-entry surfaces must render differently
//! (`docs/goal/behavior/identity-succession.md` § Seed escrow, § Propagation).
//! Keeping the mapping here rather than in each app is what stops seven apps
//! each re-deriving "is `fauna.recovery.no_escrow` a failure or an answer?" —
//! and disagreeing (priority #2).

use std::fmt;

use fauna_protocol::{RpcError, RpcErrorClass};

/// The refusal codes this plane's handlers emit, as shared constants rather
/// than stringly literals repeated per call site.
pub mod codes {
    /// No escrow blob rests for this account — recovery by phrase is not
    /// available until a signed-in device re-creates the kit.
    /// (`identity-succession.md:51` — the honest signal that a re-put is owed.)
    pub const NO_ESCROW: &str = "fauna.recovery.no_escrow";
    /// The identity never registered a RecoveryKey, so no succession capability
    /// exists (`identity-succession.md:53` — honest, surfaced degradation).
    pub const NOT_REGISTERED: &str = "fauna.recovery.not_registered";
    /// The identity was already succeeded; first-succession-wins is structural.
    pub const ALREADY_SUCCEEDED: &str = "fauna.recovery.already_succeeded";
    /// The challenge nonce was unknown, expired, or already spent (single-use).
    pub const INVALID_NONCE: &str = "fauna.recovery.invalid_nonce";
    /// A signature did not verify against the head of the registration chain —
    /// in practice, a retired kit still producing structurally valid bytes.
    pub const SIGNATURE_FAILED: &str = "fauna.recovery.signature_failed";
    /// The named successor identity already exists as an account here.
    pub const SUCCESSOR_EXISTS: &str = "fauna.recovery.successor_exists";
    /// The old identity is homed on this nest, so only `succession.submit` may
    /// supersede it (the peer-side kind refuses).
    pub const OLD_IS_LOCAL: &str = "fauna.recovery.old_is_local";
}

/// What went wrong in a recovery ceremony.
///
/// The split is by **what the user must do next**, not by which layer raised
/// it: `Superseded` routes to the identity import, `NoEscrow` routes to
/// "re-create the kit from a signed-in device", `Transport` is the only
/// variant a client may retry silently.
#[derive(Debug)]
pub enum RecoveryError {
    /// This identity was succeeded. The only way forward is importing the
    /// successor identity (`identity-succession.md:81`) — never a retry, which
    /// is why the shared `RpcError::action()` classifies it `Rejected`.
    Superseded {
        /// The successor, read from the refusal's details via
        /// `RpcError::superseded_by()` — a value the client should still
        /// *verify* against the succession chain, never merely trust. Not
        /// interpolated into the rendered message — a raw hex actor id is
        /// diagnostic detail, not user-facing text.
        new_actor_id: [u8; 32],
    },

    /// No escrow blob rests for this account. Phrase-only recovery is
    /// unavailable until a signed-in device re-creates the kit.
    NoEscrow,

    /// The identity registered no RecoveryKey — no succession capability, and
    /// nothing to restore from.
    NotRegistered,

    /// The identity has already been succeeded once; the row is keyed on the
    /// old actor id, so a second attempt can never win.
    AlreadySucceeded,

    /// The challenge nonce was unknown, expired, or already spent. Recoverable
    /// by starting the challenge/response pair over — but never by re-sending
    /// the same nonce, which is what makes this distinct from `Transport`.
    InvalidNonce,

    /// A signature failed to verify against the head of the registration
    /// chain. The usual cause is a **retired** kit: it still signs valid bytes,
    /// they just authorize nothing any more.
    SignatureFailed,

    /// The entered kit parses but is not the account's **current** one — its
    /// derived pubkey is not the head of the registration chain. The usual
    /// cause is an older kit: paper outlives every replacement, and only the
    /// newest kit may seal the escrow blob (a blob sealed to a retired key
    /// would rest unfetchably — the state the nest's lifecycle deletion exists
    /// to keep unrepresentable). Client-side and pre-put: nothing was sent.
    KitNotCurrent,

    /// The successor identity named by a succession statement already holds an
    /// account on this nest.
    SuccessorExists,

    /// A refusal that reached the nest and was declined for some other reason.
    /// Carries the wire error so the caller can render its localized message
    /// (`RpcError::localized()`, Dimension 4 — the same mechanism every other
    /// client error type routes wire refusals through).
    Rejected(Box<RpcError>),

    /// A transport fault — disconnect, timeout, framing, auth refresh. The one
    /// variant a caller may retry as-is.
    Transport(String),

    /// A local cryptographic step failed: signing, canonical encoding, sealing,
    /// or unsealing. In a ceremony this means nothing was sent, or what came
    /// back could not be opened with the key we hold.
    Crypto(String),

    /// The nest's reply did not have the shape the wire type promises (a nonce
    /// that is not 32 bytes, an actor id that is not 32 bytes, a record that
    /// does not decode).
    Malformed(String),

    /// The caller asked to replace a registered RecoveryKey but supplied no
    /// prior key. That is not a transport condition — it is the wrong ceremony:
    /// without the prior key the caller must use the **seed-alone** replacement
    /// path and wait out its 30-day window (`identity-succession.md:43`).
    PriorKitRequired,

    /// The kit the caller supplied as the prior one is not the kit registered
    /// at the head of this identity's chain — a kit that was already replaced,
    /// or one belonging to a different account. Caught locally before signing,
    /// so the screen can say which of the two it is instead of relaying a bare
    /// "record refused".
    PriorKitMismatch,

    /// A kit **replacement** would have overwritten a resting escrow blob this
    /// ceremony could not read, and nothing else carried the material it may
    /// have held.
    ///
    /// The re-put **replaces** the blob, so a section it cannot see is a
    /// section it destroys — permanent loss of user-irrecoverable material
    /// (`identity-succession.md` § Seed escrow, the no-user-data-loss
    /// invariant). Refusing here costs a retry; proceeding costs a corpus. The
    /// refusal happens **before** the registration is submitted, so nothing has
    /// landed and no secret has been minted: the account is exactly as it was.
    ///
    /// The user action is genuinely different from every other variant — get
    /// this device to a state where the current blob opens (be online, be the
    /// device holding the retired identity's row) and run the replacement
    /// again — which is why it is not folded into `Transport` or `Crypto`.
    PriorEscrowUnreadable {
        /// Why the resting blob could not be read, for the screen's detail line.
        reason: String,
    },

    /// An MLS step of the per-group succession sweep refused
    /// (`crate::group_sweep`). Distinct from [`Self::Crypto`] because these are
    /// overwhelmingly **policy** verdicts, not cipher failures: the old leaf is
    /// not a member of this group, the successor is already one (an interrupted
    /// sweep being safely re-run), or the wrong half of the ceremony was
    /// authored. Carrying them under their own variant is what lets a sweep
    /// report distinguish "this group refused" from "the transport broke".
    GroupCeremony(String),
}

impl fmt::Display for RecoveryError {
    /// The user-facing rendering, localized (`errors.recovery_*` in
    /// `i18n/strings/en.yaml`) — mirrors `NestClientError`/`AnonClientError`/
    /// `WsRpcError`'s `Display` impls: every client error type this crate's
    /// consumers surface through a bare `{e}`/`.to_string()` renders localized
    /// text here, at the source, rather than each of the ~10 call sites across
    /// tui/linux having to remember to localize
    /// (, the same architectural
    /// class already fixed for the RPC-transport error types).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Superseded { .. } => {
                write!(f, "{}", fauna_i18n::strings::errors::RECOVERY_SUPERSEDED)
            }
            Self::NoEscrow => write!(f, "{}", fauna_i18n::strings::errors::RECOVERY_NO_ESCROW),
            Self::NotRegistered => write!(
                f,
                "{}",
                fauna_i18n::strings::errors::RECOVERY_NOT_REGISTERED
            ),
            Self::AlreadySucceeded => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::RECOVERY_ALREADY_SUCCEEDED
                )
            }
            Self::InvalidNonce => {
                write!(f, "{}", fauna_i18n::strings::errors::RECOVERY_INVALID_NONCE)
            }
            Self::SignatureFailed => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::RECOVERY_SIGNATURE_FAILED
                )
            }
            Self::KitNotCurrent => write!(
                f,
                "{}",
                fauna_i18n::strings::errors::RECOVERY_KIT_NOT_CURRENT
            ),
            Self::SuccessorExists => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::RECOVERY_SUCCESSOR_EXISTS
                )
            }
            // Dimension 4: render the wire error's localized, code-keyed
            // message, never the raw wire code — the same rule
            // `NestClientError::Rpc`'s Display follows.
            Self::Rejected(rpc) => write!(f, "{}", rpc.localized()),
            Self::Transport(detail) => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::recovery_transport(detail)
                )
            }
            Self::Crypto(detail) => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::recovery_crypto(detail)
                )
            }
            Self::Malformed(detail) => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::recovery_malformed(detail)
                )
            }
            Self::PriorKitRequired => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::RECOVERY_PRIOR_KIT_REQUIRED
                )
            }
            Self::PriorKitMismatch => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::RECOVERY_PRIOR_KIT_MISMATCH
                )
            }
            Self::PriorEscrowUnreadable { reason } => write!(
                f,
                "{}",
                fauna_i18n::strings::errors::recovery_prior_escrow_unreadable(reason)
            ),
            Self::GroupCeremony(detail) => {
                write!(
                    f,
                    "{}",
                    fauna_i18n::strings::errors::recovery_group_ceremony(detail)
                )
            }
        }
    }
}

impl std::error::Error for RecoveryError {}

impl From<fauna_mls::error::MlsError> for RecoveryError {
    fn from(e: fauna_mls::error::MlsError) -> Self {
        Self::GroupCeremony(e.to_string())
    }
}

impl RecoveryError {
    /// Map a transport error into the taxonomy.
    ///
    /// A server rejection is routed by its stable wire `code`; anything else is
    /// a transport fault. Reading the code through `RpcErrorClass::as_rpc_error`
    /// rather than string-matching a `Display` is what keeps this correct
    /// across both transports (native `NestClientError`, wasm `WsRpcError`).
    pub fn from_transport<E: core::fmt::Display + RpcErrorClass>(err: E) -> Self {
        let Some(rpc) = err.as_rpc_error() else {
            return Self::Transport(err.to_string());
        };
        // Read the successor *before* the code match: `superseded_by` already
        // returns `None` for every other code, so it is safe on any error and
        // cannot pull a successor out of an unrelated refusal.
        if let Some(new_actor_id) = rpc.superseded_by() {
            return Self::Superseded { new_actor_id };
        }
        match rpc.code.as_str() {
            codes::NO_ESCROW => Self::NoEscrow,
            codes::NOT_REGISTERED => Self::NotRegistered,
            codes::ALREADY_SUCCEEDED | codes::OLD_IS_LOCAL => Self::AlreadySucceeded,
            codes::INVALID_NONCE => Self::InvalidNonce,
            codes::SIGNATURE_FAILED => Self::SignatureFailed,
            codes::SUCCESSOR_EXISTS => Self::SuccessorExists,
            _ => Self::Rejected(Box::new(rpc.clone())),
        }
    }

    /// `true` when retrying the identical request could plausibly succeed.
    ///
    /// Deliberately narrow: only a transport fault qualifies. Every refusal in
    /// this taxonomy is a definite answer about state — retrying an
    /// `InvalidNonce` with the same nonce, or a `Superseded` at all, spins
    /// instead of routing the user somewhere useful.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_))
    }
}

pub type Result<T> = core::result::Result<T, RecoveryError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `Display` used to be pure hardcoded English via
    /// `thiserror`'s `#[error(...)]` — every one of the ~10 call sites across
    /// tui/linux that render this error with a bare `{e}` inherited raw text
    /// (). Spot-check a plain
    /// variant and every argument-carrying one against the localized string.
    #[test]
    fn plain_variants_render_localized() {
        assert_eq!(
            format!("{}", RecoveryError::NoEscrow),
            fauna_i18n::strings::errors::RECOVERY_NO_ESCROW
        );
        assert_eq!(
            format!("{}", RecoveryError::PriorKitMismatch),
            fauna_i18n::strings::errors::RECOVERY_PRIOR_KIT_MISMATCH
        );
    }

    #[test]
    fn transport_and_crypto_render_the_localized_wrap() {
        assert_eq!(
            format!("{}", RecoveryError::Transport("boom".into())),
            fauna_i18n::strings::errors::recovery_transport("boom")
        );
        assert_eq!(
            format!("{}", RecoveryError::Crypto("boom".into())),
            fauna_i18n::strings::errors::recovery_crypto("boom")
        );
    }

    /// `Superseded` must never leak the raw hex actor id into user-facing
    /// text — it is diagnostic detail (mirrors `NestClientError::
    /// NestIdentityChanged`'s pinned/seen hex, which the same rule applies
    /// to).
    #[test]
    fn superseded_display_never_leaks_the_raw_actor_id() {
        let new_actor_id = [0xab; 32];
        let s = format!("{}", RecoveryError::Superseded { new_actor_id });
        assert_eq!(s, fauna_i18n::strings::errors::RECOVERY_SUPERSEDED);
        assert!(!s.contains("ab"), "got: {s}");
    }

    /// `Rejected` must render through `RpcError::localized()` — the stable
    /// wire code, not `.0.code`'s raw text — the same Dimension-4 rule every
    /// other client error type's `Rpc(RpcError)` variant follows.
    #[test]
    fn rejected_renders_the_wire_errors_localized_message() {
        let rpc = RpcError::new("fauna.protocol.timeout", "error.protocol.timeout");
        let s = format!("{}", RecoveryError::Rejected(Box::new(rpc)));
        assert_eq!(s, fauna_i18n::strings::error::protocol::TIMEOUT);
        assert!(!s.contains("fauna.protocol.timeout"), "got: {s}");
    }
}

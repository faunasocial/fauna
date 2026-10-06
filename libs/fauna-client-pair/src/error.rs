//! Error taxonomy for the `linked-nests` machine.
//!
//! Mirrors `fauna-client-dns`'s `DnsNestError` / `DnsDispatchError` split, but
//! with no provider/store classes — pairing has a single nest seam.
//!
//! [`PairDispatchError`] is named `Pair*` because it **is** UniFFI-exported and
//! would collide with sibling crates' `DispatchError` in the flat `fauna-ffi`
//! namespace (UniFFI keys on the Rust ident, not the re-export alias). The
//! *inner* seam errors never enter that namespace — `flat_error` represents the
//! dispatch wrapper by its `Display` string — so their `Pair*` names are
//! historical, and [`PairNestError`] is now an alias for the shared
//! [`fauna_protocol::NestSeamError`] rather than a fourth copy of it.

/// Returned by every [`crate::LinkedNestsNest`] seam method (the
/// `fauna.pair.{list,add,revoke}` WS-RPC surface) — the shared
/// [`fauna_protocol::NestSeamError`] under this crate's own name. `Transient` is
/// a WS disconnect / rate limit / 5xx; `Rejected` is a structured refusal
/// (pairing disabled by the admin knob, unknown actor, …).
///
/// ⚠ The name is a **historical alias, not a divergence** — see the module doc
/// above for the flat-namespace claim it was licensed on, and
/// [`fauna_protocol::NestSeamError`] for why that claim never applied to the
/// *inner* seam errors. `fauna-client-bridges`' `DiscoverHoldersError` is now
/// this very type, so the variant-to-variant `From` impl that used to translate
/// it (2026-07-19 lift) is gone: `content_processor_holders` keeps its
/// `PairNestError` signature with no conversion at all.
pub use fauna_protocol::NestSeamError as PairNestError;

/// Returned by the store seams' load/save (the grant-event log's
/// succession-ledger persistence). The **shared** `fauna_client_config::StoreError`
/// — the trust facet uses the same seam + error as dns/mail (priority #3/#4: one
/// concept, one name). Re-exported here so `PairDispatchError`'s `#[from]` and
/// every `fauna_client_pair::StoreError` reference resolve.
pub use fauna_client_config::StoreError;

/// Returned by [`crate::GrantEventSigner::sign_grant_event`]. The per-app
/// glue holds the user's Ed25519 signing key and never exposes it to the
/// machine (upholds `key-material-hierarchy.md` #7 — identity never crosses
/// the FFI seam). Now the **shared**
/// `fauna_client_capabilities::grant_log::GrantEventSignError` — the same seam
/// the labeler catalog's mint signs through (priority #3/#4: one signer seam,
/// one error) — under this crate's historical name.
pub use fauna_client_capabilities::grant_log::GrantEventSignError as TrustSignerError;

/// Wraps every failure mode an action dispatch can produce. The per-app UI
/// surfaces the `Display` form via the `error-message` element. `flat_error`
/// represents it by its `Display` string at the FFI boundary, so the inner
/// seam error needs no annotation.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum PairDispatchError {
    #[error(transparent)]
    Nest(#[from] PairNestError),
    /// Persisting the signed grant-event log failed (trust facet).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Signing a `GrantEvent` behind the signer seam failed (trust facet).
    #[error(transparent)]
    Signer(#[from] TrustSignerError),
    /// Client-side crypto composing/serializing a `GrantBlob` failed — the
    /// mint step's key derivation, HPKE seal, or canonical encode (trust
    /// facet). Class (1) crypto failure, mirroring mail-settings' `Wrap`.
    #[error("grant crypto: {0}")]
    Wrap(String),
    /// A client-side precondition failed — e.g. the linked nest's identity the
    /// user typed/scanned is not a valid 32-byte Ed25519 public key in hex, or
    /// a Renew/Revoke named a grant the local log doesn't know.
    #[error("invalid state: {0}")]
    InvalidState(String),
    /// A both-ends link found the two nests holding RecoveryKey registration
    /// chains of which neither extends the other, and refused: nothing was
    /// written at either nest (`identity-succession.md` § Enforcement on the
    /// home nest → *Every nest the identity is linked to*, clause (a) — a
    /// fork is never linked silently).
    #[error("{}", fauna_i18n::strings::nests::LINK_RECOVERY_KEYS_DIFFER)]
    RecoveryKeysDiffer,
}

impl From<fauna_mls::wrapped_blob::WrapError> for PairDispatchError {
    fn from(e: fauna_mls::wrapped_blob::WrapError) -> Self {
        Self::Wrap(e.to_string())
    }
}

impl From<fauna_client_capabilities::MintGrantError> for PairDispatchError {
    fn from(e: fauna_client_capabilities::MintGrantError) -> Self {
        Self::Wrap(e.to_string())
    }
}

impl From<fauna_client_capabilities::grant_log::RenewError> for PairDispatchError {
    fn from(e: fauna_client_capabilities::grant_log::RenewError) -> Self {
        Self::InvalidState(e.to_string())
    }
}

/// The stored grant log came back without the `Mint` event we just wrote (a CAS
/// merge that dropped it). Depositing anyway would strand a live capability the
/// user's app can neither show nor revoke, so the release refuses and the tap
/// reports the failure instead.
impl From<fauna_client_capabilities::grant_log::UnrecordedGrantError> for PairDispatchError {
    fn from(e: fauna_client_capabilities::grant_log::UnrecordedGrantError) -> Self {
        Self::InvalidState(e.to_string())
    }
}

/// `content_processor_holders` returns `discover_holders`' error straight
/// through, with no conversion — sound only while the two names denote **one**
/// type. This coerces one to the other, so re-splitting either back into a
/// private enum is a compile error here rather than a silent re-divergence of
/// the two `Display` spellings `fauna_protocol::NestSeamError` now owns.
#[cfg(test)]
const _: fn(fauna_client_bridges::DiscoverHoldersError) -> PairNestError = |e| e;

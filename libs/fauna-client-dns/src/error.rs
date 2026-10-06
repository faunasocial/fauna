//! Error taxonomy for the `admin-dns` machine.
//!
//! Mirrors `fauna-client-mail-settings`'s `NestError` / `StoreError` /
//! `DispatchError` split: each seam returns its own failure class, and the
//! dispatch wraps them plus the machine's own client-side precondition class
//! ([`DnsDispatchError::InvalidState`]). Split out of `lib.rs` once the
//! credential / managed-mode actions added client-side variants, as the
//! crate's module-scope note anticipated.

/// Returned by every [`crate::DnsNest`] seam method (the read/verify WS-RPC
/// surface) — the shared [`fauna_protocol::NestSeamError`] under this crate's
/// own name. `Transient` is a WS disconnect / rate limit / 5xx; `Rejected` is a
/// structured refusal (not admin, unknown domain, …).
///
/// ⚠ The name is a **historical alias, not a divergence**. It used to be a
/// separate enum, licensed on the claim that it *"doesn't collide with
/// `fauna-client-mail-settings`'s `NestError` once both crates' types share the
/// flat `fauna-ffi` UniFFI namespace"* — but this type never enters that
/// namespace: [`DnsDispatchError`] is the UniFFI-exported one, and it is
/// `flat_error`, which is exactly why (as its own doc says) *"the inner seam
/// errors need no annotation"*. The flat-namespace constraint is real for
/// `DnsDispatchError`'s name and for that alone.
pub use fauna_protocol::NestSeamError as DnsNestError;

/// Returned by every [`crate::DnsProviderSeam`] method (the client-side
/// DNS-provider API surface — `verify` / `publish`). The real impl over
/// `fauna-provisioning` lands native-gated in Slice 3; tests use a fake.
#[derive(Debug, thiserror::Error)]
pub enum DnsProviderError {
    /// Provider API timeout / 5xx / network error — retryable.
    #[error("dns provider unreachable: {0}")]
    Transient(String),
    /// Provider rejected the credential or request (bad token, unknown
    /// zone, validation failure) — not retryable without a fix.
    #[error("dns provider rejected: {0}")]
    Rejected(String),
}

/// Returned by the store seams — the **shared**
/// `fauna_client_config::StoreError`, re-exported here so `DnsDispatchError`'s
/// `#[from]` and every `fauna_client_dns::StoreError` reference keep resolving
/// after the seam moved to `fauna-client-config` (priority #4: one shared seam).
pub use fauna_client_config::StoreError;

/// Wraps every failure mode an action dispatch can produce. The per-app UI
/// surfaces the `Display` form via the `error-message` element.
///
/// Named `DnsDispatchError` (not `DispatchError`) for the same flat-namespace
/// reason as [`DnsNestError`]. `flat_error` represents it by its `Display`
/// string at the FFI boundary, so the inner seam errors need no annotation.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum DnsDispatchError {
    #[error(transparent)]
    Nest(#[from] DnsNestError),
    #[error(transparent)]
    Provider(#[from] DnsProviderError),
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A client-side precondition failed — e.g. a managed-mode toggle or
    /// publish for a domain no held credential covers, a clear of an
    /// out-of-range credential index, or a credential action on a machine
    /// built without the credential store wired ([`crate::DnsManagementMachine::new`]
    /// vs `with_credentials`).
    #[error("invalid state: {0}")]
    InvalidState(String),
    /// The client-driven DNS-01 ACME order itself failed (account/order/CA/CSR
    /// step, or sealing/delivering the issued cert to the nest). The string is
    /// the underlying `acme_order::Dns01Error` (or seal/delivery error)
    /// `Display`. Held as a `String` rather than a `#[from] Dns01Error` so the
    /// error enum stays target-independent — the native `Dns01Error` is
    /// `#[cfg(not(target_arch = "wasm32"))]` and the wasm `acme_pure` driver has its
    /// own error type, but `DnsDispatchError` is shared (the cross-target `issue_cert`
    /// orchestration maps whichever driver's error into this one). Every
    /// variant is non-fatal: a failed issuance leaves the nest on the Phase-2
    /// self-signed floor until the next attempt.
    #[error("certificate issuance: {0}")]
    Issuance(String),
}

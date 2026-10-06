//! Shared orchestration for the admin unified-DNS page (`admin-dns`) — the
//! per-domain DNS record matrix every domain the deployment hosts needs, with
//! a live red/green "is this record correct in public DNS?" verdict and
//! per-domain managed/manual mode.
//!
//! Authority for behavior: `docs/goal/behavior/dns-management.md`. Implementation
//! design (ratified with the user 2026-05-24, tracked internally) covers the
//! shared-Rust machine.
//! Authority for UX/IDs: `tests/e2e-unified/ui.yaml` `admin-dns` (the IDs are
//! *proposed* in the design — ratify before the per-app page is built).
//!
//! Per priority #2, the snapshot projection + the expected↔observed merge live
//! here, not in any per-app shell: the UI renders [`DnsSnapshot`] and
//! dispatches [`DnsAction`]; the per-app glue implements one WS-RPC seam
//! ([`DnsNest`]) over the admin WS-RPC handle. Mirrors
//! `fauna-client-mail-settings`'s `LocalDomainMachine` / `BridgeApprovalMachine`
//! (snapshot + action + seam + machine, TDD'd against a fake `Nest`). **DNS is
//! broader than mail** — it spans mail records, the nest-host records, and
//! deployment-wide provider credentials — so this is its own crate, not folded
//! into `fauna-client-mail-settings` (design § Shared-rust machine).
//!
//! **Read/verify surface (the nest owns the wire types** — "DNS
//! wire-type ownership — nest defines, rust consumes"; this
//! crate only consumes them):
//!   * [`DnsAction::Refresh`] (`fauna.dns.list_records`) — the per-domain record
//!     matrix the page renders ("here are the records to set, and their exact
//!     expected values"), projected into [`DomainView`] / [`DnsRecordRow`].
//!   * [`DnsAction::VerifyRecords`] (`fauna.dns.verify_records`) — the live
//!     public-DNS red/green verdicts, **merged onto** the rendered matrix by the
//!     `(name, record_type)` key (a domain's `MX` and SPF `TXT` share the
//!     bare-domain `name`, disambiguated by `record_type`). Each matched row
//!     gains a [`RecordVerdict`] (observed value + [`VerifyStatus`]).
//!
//! **Client-side managed-mode surface (this is the credential store + publish
//! the read/verify nest surface deliberately does *not* provide).** Per
//! `dns-management.md` § Where the credential lives + § The two modes, the
//! "Fauna controls DNS" credential is **client-held**, never readable by the
//! nest — it lives in the account's DNS management record (`DnsConfig`,
//! `fauna_core::data`), the fleet-only, tip-sealed `fauna.state.dns` row of
//! the account plane. So the credential actions need **no new nest kind**;
//! they go through two client-side seams:
//!   * [`DnsStore`] — load/save that record ([`AccountDnsStore`] reads and
//!     writes it through the seat's account-store handle).
//!   * [`DnsProviderSeam`] — `verify` / `publish` against a DNS provider. The
//!     real impl over `fauna-provisioning`'s `DnsProvider` is native-gated and
//!     lands in Slice 3; tests use a fake.
//!
//! The actions: [`DnsAction::PutCredentials`] (verify a provider credential,
//! then store it with its zones), [`DnsAction::ClearCredentials`],
//! [`DnsAction::SetMode`] (per-domain managed opt-in, which publishes the
//! domain's records itself on opt-in), [`DnsAction::Publish`] (idempotently
//! create the expected records). **Effective mode is a pure
//! client-side projection**: a domain renders `"managed"` iff it is in the
//! admin's managed set **and** some held credential's cached `zones` cover it;
//! otherwise `"manual"` (the nest's wire `mode` stays informational). A machine
//! built with [`DnsManagementMachine::new`] (read/verify only — the Phase-0
//! page wiring) has no credential store; the credential actions return
//! [`DnsDispatchError::InvalidState`] until the glue switches to
//! [`DnsManagementMachine::with_credentials`].

// `DnsNest`/`DnsProviderSeam`/`DnsStore` are bounded by `MaybeSendSync`
// (`Send + Sync` natively, empty on wasm32), so an `Arc<dyn ...>`-holding seam
// struct is correctly `!Send`/`!Sync` on wasm32 but trips
// `arc_with_non_send_sync` there. wasm32-scoped so native, where the bound
// resolves to `Send + Sync`, keeps the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_dns");

/// Client host-address reporting (`fauna.dns.set_host_address`) — classify the
/// dial-address, never publish a private/LAN one, report the public IP.
pub mod host_address;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_core::data::{
    CnameDelegation, DnsConfig, DnsProviderCredential, DnsZoneRef, PendingChallengeRecord,
    PendingManualIssue, Timestamp,
};
// Re-exported next to [`DnsCredentialField`]: its secret `value` is a
// `SecretString`, so a consumer constructing the field finds the type here
// (mirrors `fauna-client-mail-settings` re-exporting `SecretBytes`).
use ed25519_dalek::SigningKey;
pub use fauna_core::secret::SecretString;
use fauna_protocol::dns::{
    DnsRecordView, DomainDns, DomainVerifyStatus, ListRecordsReply, ListRecordsRequest,
    ProbeTxtVisibleReply, ProbeTxtVisibleRequest, SetHostAddressReply, SetHostAddressRequest,
    VerifyRecordsReply, VerifyRecordsRequest,
};
use fauna_protocol::tls::{
    CertHealthState as WireCertHealthState, CertStatusReply, CertStatusRequest, DomainCertStatus,
    PublishCertReply, PublishCertRequest,
};
use fauna_protocol::{MaybeSendSync, RpcErrorClass, RpcRequester};
// The snapshot/sub-records/enums + the action enum cross the JS boundary as
// `JsValue` for the wasm consumer (`libs/fauna-wasm`'s `WasmDnsManagementMachine`):
// the snapshot via `serde_wasm_bindgen` on `snapshot()`, the action on `dispatch`.
// Mirrors `fauna-client-mail-settings`'s `state.rs` snapshot derives.
use serde::{Deserialize, Serialize};

mod error;
pub use error::{DnsDispatchError, DnsNestError, DnsProviderError, StoreError};

// The **target-independent** DNS-01 order core: the order data types
// (`Dns01OrderConfig`/`Dns01Issued`/`Dns01Error`), the publish/teardown
// choreography (`with_published_challenges`), and the issued-cert bundle helpers —
// everything that is NOT the order-driving, shared by both drivers (native
// `acme_order` over instant-acme; wasm `acme_pure` over RustCrypto). NOT cfg-gated:
// it compiles on both targets.
mod acme_shared;
pub use acme_shared::{Dns01Error, Dns01Issued, Dns01OrderConfig, Dns01ResolvabilityProbe};

// The **in-process** impl of `Dns01ResolvabilityProbe` — the propagation gate's
// production probe on the 5 native apps + tui. Raw DNS (hickory) doesn't exist in
// the browser, so this module is native-only; wasm reaches the *same* query
// through `NestRelayedProbe`, which asks the nest to run it
// (`fauna.dns.probe_txt_visible`). Both sit on one implementation,
// `fauna_core::authoritative_dns::authoritative_txt_visible` — one mechanism, two
// transports, so web is never served a weaker approximation.
#[cfg(not(target_arch = "wasm32"))]
mod resolvability;
#[cfg(not(target_arch = "wasm32"))]
pub use resolvability::AuthoritativeNsProbe;

// The **native** DNS-01 order driver (Phase 3, S4). `instant-acme`'s crypto
// (`ring`/`aws-lc-rs`) + `rcgen` are not browser-WASM-safe (D2), so this driver and
// its deps are native-gated; the wasm twin is `acme_pure`. `lib.rs` re-exports
// `Dns01OrderInProgress` + the order fns per target under the one name (native here;
// wasm from `acme_pure` — see W5 (account-data-plane.md § Workstreams)), so the cross-target `issue_cert` orchestration
// calls the same symbols on both.
#[cfg(not(target_arch = "wasm32"))]
mod acme_order;
#[cfg(not(target_arch = "wasm32"))]
pub use acme_order::{
    Dns01OrderInProgress, begin_dns01_order, complete_dns01_order, obtain_certificate_dns01,
};
// The pebble real-wire acceptance entry (S4b) — injects a CA-trusting HTTP client
// into the order so the test can drive it against pebble's self-signed ACME
// endpoint. `test-helpers` only; never compiled into a release build.
#[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
pub use acme_order::{begin_dns01_order_with_http, obtain_certificate_dns01_with_http};

// The wasm-safe (RustCrypto + reqwest) ACME v2 client — the browser twin of
// `acme_order` that replaces `instant-acme`+`rcgen` on wasm so web cert issuance
// works (`tls-certificates.md` § C). The W1 crypto core is unconditional (both
// targets, CA-free unit tests); the W3 HTTP/order flow (`acme_pure::order`) is
// gated to wasm-production-or-pebble-proof.
mod acme_pure;
pub use acme_pure::{AccountCredentials, AccountKey, AcmePureError, DirectoryUrls, build_csr};
// On **wasm** the production DNS-01 order fns + the suspendable handle ARE the pure
// driver — the same names `acme_order` exposes on native — so the cross-target
// `DnsManagementMachine::issue_cert` orchestration (W5) calls one symbol on both
// targets. (`Dns01Error`/`Dns01Issued`/`Dns01OrderConfig` come from `acme_shared`,
// shared by both.)
#[cfg(target_arch = "wasm32")]
pub use acme_pure::{
    Dns01OrderInProgress, begin_dns01_order, complete_dns01_order, obtain_certificate_dns01,
};
// The pure driver's pebble real-wire proof entries (W6) — `pure_*`-aliased on native
// so they don't collide with the instant-acme driver's production names above.
// `test-helpers` only; never in a release build.
#[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
pub use acme_pure::{
    Dns01OrderInProgress as PureDns01OrderInProgress,
    begin_dns01_order_with_http as pure_begin_dns01_order_with_http,
    complete_dns01_order as pure_complete_dns01_order,
    obtain_certificate_dns01_with_http as pure_obtain_certificate_dns01_with_http,
};

// ── snapshot + action ───────────────────────────────────────────────

/// The live public-DNS verdict for one record. Typed projection of the wire
/// `DnsRecordStatus.status` string the UI matches on for red/green rendering.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum VerifyStatus {
    /// Public DNS already serves the expected value (green).
    Ok,
    /// No record found at this name (red).
    Missing,
    /// A record exists but its value differs from expected (red).
    Mismatch,
    /// Lookup in-flight / rate-limited / transiently errored — not yet a verdict.
    Checking,
}

impl VerifyStatus {
    /// Maps the wire `DnsRecordStatus.status` string (`ok | missing | mismatch |
    /// checking`, `dns.rs`) to the typed verdict. An unrecognized value
    /// (wire/version drift) degrades to [`VerifyStatus::Checking`] — the neutral
    /// "no verdict yet" state, never a false green or red.
    fn from_wire(s: &str) -> Self {
        match s {
            "ok" => VerifyStatus::Ok,
            "missing" => VerifyStatus::Missing,
            "mismatch" => VerifyStatus::Mismatch,
            _ => VerifyStatus::Checking,
        }
    }
}

/// The merged live verdict (`fauna.dns.verify_records`) for one record. Present
/// once [`DnsAction::VerifyRecords`] has covered this record; `None` on a row
/// until then.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RecordVerdict {
    /// What public DNS actually serves (empty when `status == Missing`).
    pub observed: Vec<String>,
    pub status: VerifyStatus,
}

/// One DNS record on the admin page: its expected value (always present, from
/// `list_records`) plus the live public-DNS verdict overlay (`None` until
/// [`DnsAction::VerifyRecords`] has covered it). The merge of a wire
/// [`DnsRecordView`] with the matching wire `DnsRecordStatus` by
/// `(name, record_type)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsRecordRow {
    pub name: String,
    pub record_type: String,
    /// Exact RFC zone-file RDATA the admin pastes / Fauna would publish.
    pub expected: String,
    pub ttl_seconds: u32,
    /// Live verdict overlay; `None` until verified (or for a record the latest
    /// verify reply didn't cover).
    pub verdict: Option<RecordVerdict>,
}

impl From<DnsRecordView> for DnsRecordRow {
    fn from(r: DnsRecordView) -> Self {
        Self {
            name: r.name,
            record_type: r.record_type,
            expected: r.expected,
            ttl_seconds: r.ttl_seconds,
            verdict: None,
        }
    }
}

/// One domain on the page: its management mode + merged record rows. Mirrors the
/// wire `DomainDns`, but each record is a [`DnsRecordRow`] carrying the optional
/// verdict overlay (so the merge of `list_records` + `verify_records` lives in
/// shared Rust, not the per-app UI — priority #2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DomainView {
    pub domain: String,
    /// `"manual"` for every domain today (managed/auto-publish unbuilt — see the
    /// module scope note).
    pub mode: String,
    /// Mirrors the wire [`DomainDns::is_primary`] — the deployment's primary mail
    /// domain, which owns the shared host-level rows (`mail.<primary>` A/AAAA + the
    /// cert-coupled floor-MX `_25._tcp.mail.<primary>` TLSA). The managed reconcile
    /// keys the TLSA withdraw-on-trusted pass on this.
    pub is_primary: bool,
    pub records: Vec<DnsRecordRow>,
    /// Effective automatic-certificate-renewal state — `true` iff this domain is
    /// managed **or** delegated (so a client *can* auto-issue) **and** the admin
    /// has not opted it out (`DnsConfig.auto_renew_off`). Overlaid by the
    /// machine's [`project`], like `mode`; the `admin-dns-domain-auto-renew`
    /// checkbox renders this and a managed/delegated domain is the only one that
    /// shows the control (a manual-non-delegated domain can't auto-renew). Drives
    /// the auto-issue decision ([`DnsSnapshot::domains_needing_auto_renew`]).
    /// `false` on the raw wire matrix until the machine projects it.
    pub auto_renew: bool,
}

impl From<DomainDns> for DomainView {
    fn from(d: DomainDns) -> Self {
        Self {
            domain: d.domain,
            mode: d.mode,
            is_primary: d.is_primary,
            records: d.records.into_iter().map(DnsRecordRow::from).collect(),
            // Overlaid by `project` from the secret-free config state.
            auto_renew: false,
        }
    }
}

/// The two [`DomainView::mode`] projection values. The wire `mode` is a `String`
/// today (managed / auto-publish is only partially built — see the module scope
/// note); these single-source the literals so the `mode == "managed"` magic
/// string lives in exactly one place instead of being re-coded in every app's
/// glue (Rust, TS, Swift, Kotlin, C#).
pub const MODE_MANAGED: &str = "managed";
pub const MODE_MANUAL: &str = "manual";

impl DomainView {
    /// Whether this domain is effectively Fauna-managed — the machine's overlaid
    /// `mode` projection (opted-in ∧ a held credential's zones cover it; see
    /// [`DnsManagementMachine`]). The single typed read of the stringly-typed wire
    /// `mode`, so no client re-codes the `mode == "managed"` magic-string check.
    pub fn is_managed(&self) -> bool {
        self.mode == MODE_MANAGED
    }
}

/// FFI-facing twin of [`DomainView::is_managed`] for native apps that consume
/// the per-domain mode read over UniFFI (windows now; apple/android when they
/// build admin-dns managed mode). `DomainView` is a `uniffi::Record` and so can't
/// carry an exported method — the same constraint that put
/// [`DnsManagementMachine::all_domains_managed`] on the machine Object — so the
/// per-domain projection is a free function instead. Taken by value (Records cross
/// the UniFFI boundary by value); the call sites render off an already-held
/// snapshot, so no client re-codes the `mode == "managed"` magic string.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn domain_is_managed(view: DomainView) -> bool {
    view.is_managed()
}

/// One provider-credential field the admin types into the write-only add-form
/// (`admin-dns-add-credential-*`), keyed by the `providers.yaml` field id
/// (e.g. Hetzner `api-token`, Porkbun `api-key` / `secret-api-key`). The
/// FFI-facing input shape; the machine maps a `Vec<DnsCredentialField>` to the
/// `Vec<(String, SecretString)>` bag `fauna_core::DnsProviderCredential.fields`
/// uses. A named record (not a tuple) so it crosses both the UniFFI and
/// `serde_wasm_bindgen` boundaries cleanly.
///
/// The secret `value` is a [`SecretString`] (zeroized on drop, redacted
/// `Debug`), mirroring `MailSettingsAction`'s `SecretBytes` fields — the text
/// twin, since DNS tokens are text. Its UniFFI custom type marshals as a
/// `string` on the FFI boundary (so the field stays a `uniffi::Record`); the
/// `lower` copy + the foreign-side buffer are un-zeroizable by design (the
/// value-passing FFI boundary — see `SecretString`' module docs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsCredentialField {
    pub id: String,
    pub value: SecretString,
}

// [`SecretString`]'s UniFFI custom-type marshalling (as a `string`) is
// registered once, blanket, in `fauna-core` (`fauna_core::secret`, behind
// `feature = "uniffi"`), so `DnsCredentialField` here — and the same type used
// in `fauna-onboarding-machine`'s `uniffi::Record`s — share one registration
// without a per-crate `remote` reg (a `remote` reg is tag-local and can't be
// reused across crates). This crate's `uniffi` feature forwards `fauna-core/uniffi`.

/// A held DNS-provider credential as the page renders it in the indexed
/// `admin-dns-credentials-list` (`admin-dns-credential-item[i]`): provider +
/// covered zone names + label. **Never carries the secret field values** — the
/// credential form is write-only, mirroring `fauna-client-mail-settings`'s
/// `CredentialSummary`. The list index is the `ClearCredentials` key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CredentialSummary {
    pub provider_id: String,
    /// Zone names the credential's `verify()` reported (the coverage set).
    pub zones: Vec<String>,
    pub label: String,
}

impl From<&DnsProviderCredential> for CredentialSummary {
    fn from(c: &DnsProviderCredential) -> Self {
        Self {
            provider_id: c.provider_id.clone(),
            zones: c.zones.iter().map(|z| z.name.clone()).collect(),
            label: c.label.clone(),
        }
    }
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `LocalDomainStatus` / `BridgeApprovalStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DnsStatus {
    Idle,
    /// Initial matrix fetch (`Refresh`) — the page may show a full-page spinner.
    Loading,
    /// An overlay action (`VerifyRecords`) over the already-rendered matrix — the
    /// page stays visible with an inline / re-check spinner.
    Working,
}

/// A **manual-mode** DNS-01 cert order suspended awaiting the admin's action — the
/// tier-3 manual-paste surface (`tls-certificates.md` § B tier 3). The per-app
/// `admin-dns` page renders the transient `_acme-challenge` TXT record(s) for the
/// admin to paste at their registrar, then enables a "complete" affordance once the
/// page's existing red/green verify shows them live, and a "cancel" affordance
/// (decline → fall back to the self-signed floor).
///
/// The challenge records are plain [`DnsRecordRow`]s — the **same** row shape the
/// `admin-dns-record` component already renders for every managed/manual record, so
/// the paste surface reuses that component rather than a parallel one (UI rule C;
/// `tls-certificates.md` § "The `_acme-challenge` record" — one record type, one
/// path). Their `verdict` is `None` until the client's `VerifyRecords` overlays the
/// live red/green status, exactly like any other row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PendingCertIssue {
    /// The domain a publicly-trusted cert is being issued for.
    pub domain: String,
    /// The transient `_acme-challenge.<domain>` TXT record(s) the admin must
    /// publish — one per still-pending SAN. `expected` carries the order's raw
    /// key-authorization `dns_value` (the exact value to paste).
    pub challenges: Vec<DnsRecordRow>,
}

/// A one-time `_acme-challenge` CNAME delegation for a **manual-mode** domain
/// (`tls-certificates.md` § B tier 3, S6b). After the admin pastes this single
/// CNAME at their no-API registrar, the domain's `_acme-challenge` renewals
/// auto-publish into a zone a held credential controls (so `IssueCert` no longer
/// needs a manual paste). Projected from `DnsConfig.delegations`; the
/// per-app `admin-dns` page renders `cname` (a steady-state record the admin
/// sets once) and labels the domain "renewals automated".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DelegationView {
    /// The manual-mode domain whose `_acme-challenge` renewals are delegated.
    pub domain: String,
    /// The one-time CNAME the admin sets at their registrar — a plain
    /// [`DnsRecordRow`] (the **same** `admin-dns-record` shape every record
    /// renders, not a parallel surface): `name = _acme-challenge.<domain>`,
    /// `record_type = "CNAME"`, `expected = <target_name>`. A steady-state record
    /// (set once, permanent) — unlike the transient TXT, its `ttl_seconds` is the
    /// ordinary default, not the short challenge TTL.
    pub cname: DnsRecordRow,
}

/// The health of the cert the nest **currently serves** for a domain — the
/// three states the `admin-dns` cert-status row renders (`tls-certificates.md`
/// § C.4). The client projection of the wire `fauna_protocol::tls::CertHealthState`
/// (the nest computes the state server-side from the served leaf — it is the
/// authority on what it serves and on `now`); the per-app UI branches on this
/// to pick the badge label + colour, never re-deriving it from raw cert facts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CertHealthState {
    /// A CA-issued (trusted) cert is served and not near expiry.
    ValidTrusted,
    /// The self-signed floor is served — a trusted cert is needed (none yet, or
    /// the trusted one expired / does not cover the domain). The fresh-nest
    /// default until issuance completes.
    OnFloorRenewNeeded,
    /// A trusted cert is served but within the 30-day renewal lead — renew soon.
    Expiring,
}

impl CertHealthState {
    /// The serde variant name, which is also the string
    /// [`fauna_core::format::cert_status_label`] and `cert_status_view` match on.
    ///
    /// ⚠ **This exists because getting it wrong fails quietly and in the alarming
    /// direction.** `cert_status_label`'s fallback arm is `status_on_floor`, so a
    /// stale or misspelled name does not render "unknown" — it renders *"self-signed
    /// — renew needed"* over a perfectly healthy CA-issued cert. tui and linux each
    /// hand-wrote this match against no test at all; a variant rename would have
    /// compiled clean on both and lied on both screens.
    ///
    /// [`cert_status_name_matches_serde`] pins each arm against the actual serde
    /// output, so the shared matcher and this mapping cannot drift apart.
    pub fn as_str(&self) -> &'static str {
        match self {
            CertHealthState::ValidTrusted => "ValidTrusted",
            CertHealthState::OnFloorRenewNeeded => "OnFloorRenewNeeded",
            CertHealthState::Expiring => "Expiring",
        }
    }
}

impl From<WireCertHealthState> for CertHealthState {
    fn from(w: WireCertHealthState) -> Self {
        match w {
            WireCertHealthState::ValidTrusted => Self::ValidTrusted,
            // A state a newer nest added reads as needing attention, never as
            // trusted — and stays a case every app already renders.
            WireCertHealthState::OnFloorRenewNeeded | WireCertHealthState::Unknown => {
                Self::OnFloorRenewNeeded
            }
            WireCertHealthState::Expiring => Self::Expiring,
        }
    }
}

/// One domain's served-cert status on the `admin-dns` cert-status row
/// (`tls-certificates.md` § C.4), projected from the wire
/// [`DomainCertStatus`](fauna_protocol::tls::DomainCertStatus). Populated by
/// [`DnsAction::RefreshCertStatus`] reading `fauna.tls.cert_status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CertStatusRow {
    /// The domain whose served cert this describes.
    pub domain: String,
    /// The three-state health badge.
    pub state: CertHealthState,
    /// `notAfter` of the served leaf, unix seconds; `0` when no cert is served
    /// (the UI shows no expiry then).
    pub not_after_unix: i64,
    /// True when the served leaf is the self-signed floor (an "untrusted /
    /// self-signed" sub-label, distinct from a near-expiry trusted cert).
    pub is_floor: bool,
}

impl From<DomainCertStatus> for CertStatusRow {
    fn from(s: DomainCertStatus) -> Self {
        Self {
            domain: s.domain,
            state: s.state.into(),
            not_after_unix: s.not_after_unix,
            is_floor: s.is_floor,
        }
    }
}

/// The `admin-dns-cert-status` badge text: the shared
/// [`fauna_core::format::cert_status_view`] decision, rendered — `None`
/// means the status hasn't loaded yet ("Certificate: Checking…"). tui and
/// linux each independently assembled this exact label+state+sub-label
/// assembly around that shared decision; the decision was already unified,
/// but the string composition around it was hand-copied twice.
///
/// `expires_at_unix` is always positive when `Some` — `cert_status_view`
/// itself only sets it for `not_after_unix > 0`, so no clamp is needed here
/// (tui's prior copy carried one; it was unreachable dead code, caught by
/// this fn's own test).
///
/// Gated on `local-clock` (native-only — wasm has no OS timezone database
/// for the expiry date's local rendering); tui and linux both already
/// enable `fauna-core/local-clock` themselves, so this only asks them to
/// forward the same switch one crate over.
#[cfg(feature = "local-clock")]
pub fn cert_status_text(cert: Option<&CertStatusRow>) -> String {
    let label = fauna_i18n::strings::admin::dns::cert::LABEL;
    let Some(c) = cert else {
        return format!(
            "{label} {}",
            fauna_i18n::strings::admin::dns::STATUS_CHECKING
        );
    };
    let view = fauna_core::format::cert_status_view(c.state.as_str(), c.is_floor, c.not_after_unix);
    let mut text = format!(
        "{label} {}",
        view.state.resolve(fauna_i18n::strings::lookup)
    );
    if view.show_self_signed {
        text.push_str(&format!(
            " ({})",
            fauna_i18n::strings::admin::dns::cert::SELF_SIGNED
        ));
    } else if let Some(expiry) = view.expires_at_unix {
        text.push_str(&format!(
            " — {}",
            fauna_i18n::strings::admin::dns::cert::expires(
                &fauna_core::format::format_unix_local_date(expiry)
            )
        ));
    }
    text
}

/// Read-only snapshot the per-app UI renders for `admin-dns`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsSnapshot {
    /// Per-domain record matrix — every active local domain and the DNS records
    /// it needs, with each record's exact expected value (`fauna.dns.list_records`)
    /// and, once verified, the live observed value + red/green status
    /// (`fauna.dns.verify_records`). Empty on a fresh nest with no mail domains.
    /// Each domain's `mode` is the **effective** managed/manual projection
    /// (managed iff opted-in ∧ a held credential's zones cover it), overlaid
    /// onto the wire matrix by the machine — see [`DnsManagementMachine`].
    pub domains: Vec<DomainView>,
    /// The admin's held DNS-provider credentials (provider + zones + label,
    /// no secrets), for the `admin-dns-credentials-list`. Empty on a machine
    /// built without the credential store ([`DnsManagementMachine::new`]).
    pub credentials: Vec<CredentialSummary>,
    pub status: DnsStatus,
    /// Last action's error, surfaced via the `error-message` element.
    pub error: Option<String>,
    /// A **manual-mode** DNS-01 cert order awaiting the admin to publish the
    /// surfaced `_acme-challenge` TXT(s) and confirm — the tier-3 manual-paste
    /// surface (`tls-certificates.md` § B tier 3). `None` when no manual issuance
    /// is in flight. Set by [`DnsAction::BeginManualIssueCert`], cleared by
    /// [`DnsAction::CompleteManualIssueCert`] / [`DnsAction::CancelManualIssueCert`].
    /// Managed-mode issuance ([`DnsAction::IssueCert`]) is a single dispatch that
    /// auto-publishes through the provider seam and never populates this.
    /// Also **re-projected from `DnsConfig.pending_manual_issue`** by a
    /// machine that holds no live order, so an issuance interrupted by a
    /// navigation / restart / device switch keeps its paste card
    /// (`tls-certificates.md` § Surviving an interrupted manual issuance).
    /// Populated on web too — the order driver is per-target but the
    /// orchestration is shared, so web issues natively (W5).
    pub pending_cert: Option<PendingCertIssue>,
    /// One-time `_acme-challenge` CNAME delegations for manual-mode domains
    /// (`tls-certificates.md` § B tier 3, S6b), projected from
    /// `DnsConfig.delegations`. Each carries the single CNAME the admin sets
    /// at their no-API registrar; after that, the domain's renewals auto-publish
    /// into the controlled zone (no further paste). Set by
    /// [`DnsAction::DelegateRenewal`], cleared per-domain by
    /// [`DnsAction::RemoveDelegation`]. Empty on a machine built without the
    /// credential store ([`DnsManagementMachine::new`]).
    pub delegations: Vec<DelegationView>,
    /// Per-domain served-cert health for the `admin-dns` cert-status row
    /// (`tls-certificates.md` § C.4) — the nest-reported `valid-trusted` /
    /// `on-floor — renew needed` / `expiring` state per domain. Populated by
    /// [`DnsAction::RefreshCertStatus`] (`fauna.tls.cert_status`); empty until
    /// it has run. A pure read, so it populates on web too (unlike the
    /// native-only issuance actions).
    pub cert_statuses: Vec<CertStatusRow>,
}

impl DnsSnapshot {
    fn empty() -> Self {
        Self {
            domains: Vec::new(),
            credentials: Vec::new(),
            status: DnsStatus::Idle,
            error: None,
            pending_cert: None,
            delegations: Vec::new(),
            cert_statuses: Vec::new(),
        }
    }

    /// The deployment "Fauna controls DNS" master-switch state — what the
    /// `admin-dns-manage-all-toggle` (and its `admin-services` DNS-toggle twin)
    /// reflect: active ⟺ there is ≥1 active domain and every active domain is
    /// effectively managed ([`DomainView::is_managed`]).
    ///
    /// `active_domains` is the authoritative active-domain set — the
    /// `LocalDomainsSnapshot.active` domain names. Pass `Some(&names)` to scope the
    /// fold to exactly those domains (an active domain with no row in this
    /// snapshot's matrix counts as **not** managed); pass `None` to fall back to
    /// this snapshot's own `domains` list (the rendering path before the
    /// local-domains snapshot has arrived).
    ///
    /// Lifted out of the per-app glue (linux `render_admin_dns`, web's
    /// `+page.svelte` `allManaged` derived) so the projection is computed once in
    /// shared Rust rather than re-coded identically per client (priority #2).
    pub fn all_domains_managed(&self, active_domains: Option<&[String]>) -> bool {
        match active_domains {
            Some(active) => {
                let by_name: std::collections::HashMap<&str, &DomainView> = self
                    .domains
                    .iter()
                    .map(|d| (d.domain.as_str(), d))
                    .collect();
                !active.is_empty()
                    && active
                        .iter()
                        .all(|name| by_name.get(name.as_str()).is_some_and(|v| v.is_managed()))
            }
            None => !self.domains.is_empty() && self.domains.iter().all(DomainView::is_managed),
        }
    }

    /// The domains a synced admin device should **auto-issue** a renewal for now:
    /// those whose served cert is at-risk (the `cert_statuses` row is **not**
    /// `ValidTrusted` — on-floor / expiring) **and** whose effective auto-renew is
    /// on ([`DomainView::auto_renew`] — a managed/delegated domain not opted out).
    /// The shared decision the per-app background cadence acts on
    /// (`tls-certificates.md` § C.3, "any synced admin device renews"): for each
    /// returned domain the client dispatches `IssueCert { domain, target_nest_id }`
    /// — with the `target_nest_id` from its own linked-nests state (D7) — and no
    /// admin tap. Lifted into shared Rust so all 7 apps agree on *when* to
    /// auto-renew (priority #2/#3); the per-app glue owns only the cadence and
    /// the `target_nest_id`. A domain with no `cert_statuses` row yet (status not
    /// refreshed) is conservatively **not** auto-issued. Pure read.
    pub fn domains_needing_auto_renew(&self) -> Vec<String> {
        self.domains
            .iter()
            .filter(|d| d.auto_renew)
            .filter(|d| {
                self.cert_statuses
                    .iter()
                    .any(|s| s.domain == d.domain && s.state != CertHealthState::ValidTrusted)
            })
            .map(|d| d.domain.clone())
            .collect()
    }
}

/// Actions the per-app UI dispatches. `Refresh` and `VerifyRecords` are wired
/// in Phase 0; the mode / credential / publish actions land as nest
/// defines their wire types (module scope note).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DnsAction {
    /// Re-read the per-domain record matrix (`fauna.dns.list_records`; page load
    /// / pull-to-refresh). Resets every row's verdict to `None` — the freshly
    /// fetched expected values invalidate any prior live status, so the page
    /// re-verifies after a refresh.
    Refresh,
    /// Overlay live public-DNS verdicts onto the current matrix
    /// (`fauna.dns.verify_records`; page load / "re-check" / background cadence).
    /// `domain: None` → verify every domain; `Some(d)` → just that one (rows of
    /// the other domains keep their prior verdicts).
    VerifyRecords { domain: Option<String> },
    /// Verify a DNS-provider credential (`DnsProviderSeam::verify`) and, on
    /// success, store it — with the zones `verify()` reported — in the admin's
    /// `DnsConfig.credentials`. Replaces an existing credential with the
    /// same provider + identical zone set; otherwise appends. The write-only
    /// add-form (`admin-dns-add-credential-*`) dispatches this.
    PutCredentials {
        provider_id: String,
        fields: Vec<DnsCredentialField>,
        label: String,
    },
    /// Remove a held credential by its `snapshot().credentials` index
    /// (`admin-dns-credential-item[index]`'s clear-button). Domains that lose
    /// coverage as a result re-render `"manual"` via the effective-mode
    /// projection (their managed opt-in stays, but is no longer actionable).
    ClearCredentials { index: u32 },
    /// Opt `domain` in/out of "Fauna controls DNS"
    /// (`admin-dns-domain-mode`). Opting **in** requires a held credential
    /// whose zones cover `domain` (else [`DnsDispatchError::InvalidState`]),
    /// and then publishes the domain's records as [`DnsAction::Publish`] does,
    /// in the same dispatch — so no app follows an opt-in with its own
    /// `Publish`. A publish failure leaves the opt-in committed and returns (and
    /// surfaces) the publish error. Opting out is always allowed and publishes
    /// nothing.
    SetMode { domain: String, managed: bool },
    /// Idempotently publish the expected record matrix for `domain` via the
    /// covering credential (`DnsProviderSeam::publish`). Requires `domain` to
    /// be effectively managed (opted-in ∧ covered), else
    /// [`DnsDispatchError::InvalidState`].
    Publish { domain: String },
    /// Run a client-driven **DNS-01 ACME order** for `domain` and deliver the
    /// issued, publicly-trusted cert to the private nest that serves it
    /// (`target_nest_id` — its 32-byte node id; the cert is HPKE-sealed to that
    /// nest's identity-derived x25519 key and Ed25519-signed by the admin, then
    /// stored on the reachable relay nest for namespace-sync pull —
    /// `tls-certificates.md` § B.2). Requires a held DNS credential whose zones
    /// cover `domain` (else → manual paste, S6). Runs on **all 7 apps**: native
    /// drives the order with `instant-acme`, web with the wasm-safe `acme_pure` twin
    /// (W1–W6) — `issue_cert` calls one symbol on both targets.
    ///
    /// `target_nest_id` is supplied by per-app glue from its pairing /
    /// linked-nests state (the home nest the admin linked); the publish goes over
    /// this machine's own (relay) connection. Carried on the action — rather than
    /// resolved inside the machine — to keep the machine decoupled from
    /// linked-nests while staying uniform across all 7 apps (D7).
    IssueCert {
        domain: String,
        target_nest_id: Vec<u8>,
    },
    /// **Manual-mode** DNS-01 issuance, phase 1 (tier 3 — a domain with no covering
    /// DNS credential, so the order cannot auto-publish `_acme-challenge`): open the
    /// order and surface its transient `_acme-challenge` TXT(s) on
    /// `snapshot.pending_cert` for the admin to paste at their registrar. The live
    /// order is held until [`Self::CompleteManualIssueCert`] (the admin pasted +
    /// the page verified the record green) or [`Self::CancelManualIssueCert`] (the
    /// admin declined → fall back to the floor). `target_nest_id` is the serving
    /// private nest, as for [`Self::IssueCert`]. Runs on **all 7 apps** — native
    /// (`instant-acme`) and web (the wasm-safe `acme_pure` twin, W1–W6). (`IssueCert`
    /// is the managed tier-2 single-call path; per-app glue routes by the domain's
    /// effective `mode`.)
    BeginManualIssueCert {
        domain: String,
        target_nest_id: Vec<u8>,
    },
    /// **Manual-mode** DNS-01 issuance, phase 2: the admin has pasted the
    /// `_acme-challenge` TXT(s) ([`PendingCertIssue::challenges`]) and the page's
    /// red/green verify shows them live — finalize the order with the CA, seal +
    /// deliver the issued cert to the target nest (Half B), persist the ACME account
    /// (D6), and clear `snapshot.pending_cert`. Errors [`DnsDispatchError::InvalidState`]
    /// when no manual order is awaiting confirmation. Native-only.
    CompleteManualIssueCert,
    /// Abandon the suspended manual order (the admin declined to paste / delegate):
    /// drop the order and clear `snapshot.pending_cert`. The nest stays on the
    /// self-signed floor — graceful (`tls-certificates.md` § B tier 3 / § C).
    /// Idempotent (a no-op when nothing is pending). Native-only state; a no-op on
    /// the web app (which never holds a pending order).
    CancelManualIssueCert,
    /// **Manual-mode renewal automation** (tier 3, S6b): set up a one-time
    /// `_acme-challenge` CNAME delegation for `domain` into `target_zone` — a zone
    /// a held credential controls. Validates that a held credential covers
    /// `target_zone` (else [`DnsDispatchError::InvalidState`]), computes the
    /// re-homing target name `_acme-challenge.<domain>.<target_zone>`, persists the
    /// delegation in `DnsConfig.delegations`, and surfaces the one-time CNAME
    /// on `snapshot.delegations` for the admin to paste **once** at their no-API
    /// registrar. Thereafter `IssueCert { domain, .. }` renews automatically by
    /// publishing the `_acme-challenge` TXT at the delegated target name (the CA
    /// follows the CNAME) — no further paste. Config-only (no ACME order), so it
    /// runs on **both** native and web.
    DelegateRenewal { domain: String, target_zone: String },
    /// Remove a `domain`'s CNAME delegation from `DnsConfig.delegations`
    /// (the inverse of [`Self::DelegateRenewal`]); the domain reverts to manual
    /// paste-per-renewal. Idempotent (a no-op when `domain` is not delegated).
    /// The admin should also remove the stale CNAME at their registrar. Both
    /// targets.
    RemoveDelegation { domain: String },
    /// Re-read the served-cert health for the domains currently on the page
    /// (`fauna.tls.cert_status`) and overlay it onto `snapshot.cert_statuses` —
    /// the `admin-dns` cert-status row (`tls-certificates.md` § C.4). Queries the
    /// names already in `snapshot.domains` (run after [`Self::Refresh`]); a no-op
    /// yielding an empty row set when the matrix is empty. A pure Admin read, so
    /// it runs on **both** native and web (unlike the native-only issuance
    /// actions). Dispatch on page load / "re-check" alongside [`Self::VerifyRecords`].
    /// On a credentialed machine it also fires the **cert-coupled floor-MX TLSA
    /// auto-withdraw** when the primary flips floor→trusted (continuous half of the
    /// 5b.5 reconcile — `tls-certificates.md` § D + § Implementation status); a
    /// best-effort side-effect that never fails the read.
    RefreshCertStatus,
    /// Turn **automatic certificate renewal** on/off for `domain`
    /// (`admin-dns-domain-auto-renew`). Auto-renew defaults **on** for every
    /// managed/delegated domain — a synced admin device silently re-issues the
    /// cert when it nears expiry, so the common case needs no admin action
    /// (`tls-certificates.md` § C.3). This persists the opt-OUT in
    /// `DnsConfig.auto_renew_off` (`enabled: false` → opt out; `true` → opt
    /// back in); the effective per-domain state surfaces on
    /// [`DomainView::auto_renew`] and gates [`DnsSnapshot::domains_needing_auto_renew`].
    /// Config-only (no ACME order), so it runs on **both** native and web. A
    /// manual-non-delegated domain can't auto-renew regardless (its `auto_renew`
    /// stays `false`), so per-app glue shows the control only for
    /// managed/delegated domains; the admin only ever needs it to *disable*
    /// hands-off renewal.
    SetAutoRenew { domain: String, enabled: bool },
}

/// WS-RPC seam to nest. Per-app glue implements this over the admin WS-RPC
/// handle. Grows one method per `fauna.dns.*` kind as nest
/// defines them (`put/clear/list_credentials`, `set_domain_mode`, `publish`).
// Native boxes `Send` futures (the seam may be driven by `tokio::spawn`); wasm's
// `Rc`-based `WsRpcClient` yields `!Send` futures, so it needs the `?Send` arm.
// The `MaybeSendSync` supertrait is `Send + Sync` natively (keeping the machine's
// `Arc<dyn DnsNest>` `Send`) and empty on wasm (admitting the `!Send` seam).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait DnsNest: MaybeSendSync {
    /// `fauna.dns.list_records` — the per-domain record matrix (expected values).
    /// `domain: None` → every active local domain; `Some(d)` → just that one.
    async fn list_records(&self, domain: Option<String>) -> Result<Vec<DomainDns>, DnsNestError>;

    /// `fauna.dns.verify_records` — per-domain live public-DNS verdicts (observed
    /// value + status, keyed by `(name, record_type)`), public-recursive and
    /// resolved nest-side. Same `domain` filter as `list_records`.
    async fn verify_records(
        &self,
        domain: Option<String>,
    ) -> Result<Vec<DomainVerifyStatus>, DnsNestError>;

    /// `fauna.tls.publish_cert` — deliver a client-issued, sealed LAN-TLS cert
    /// entry (`fauna_mls::wrapped_blob::seal_lan_tls_cert_entry`'s
    /// `(ciphertext, actor_sig)`) to the reachable relay/public nest, which
    /// stores it in the caller's own actor namespace under the well-known
    /// `LAN_TLS_CERT_ENTRY_ID` for namespace-sync distribution to the paired
    /// private nest that serves it (Half B of the DNS-01 flow). Returns the
    /// handler's `ok`. Admin-gated nest-side (the deployment's cert is an
    /// admin concern). Lives on this read/verify seam — rather than a separate
    /// one — because the `IssueCert` orchestration that drives it is already a
    /// `DnsManagementMachine` action, and the kind is just another nest WS-RPC
    /// the same admin glue speaks.
    async fn publish_cert(&self, req: PublishCertRequest) -> Result<bool, DnsNestError>;

    /// `fauna.tls.cert_status` — the served-cert health (`valid-trusted` /
    /// `on-floor — renew needed` / `expiring`) the nest reports per domain, the
    /// `admin-dns` cert-status row (`tls-certificates.md` § C.4). A pure Admin
    /// read (no mutation), so it works on web as well as native. Lives on this
    /// read/verify seam beside `list_records`/`verify_records` — it is just
    /// another nest read the same admin glue speaks.
    ///
    /// Returns the whole reply rather than just the per-domain rows: it also
    /// carries `desired_sans`, the SAN set the nest's listener cert should cover,
    /// which [`DnsManagementMachine::issue_cert`] orders. Keeping both on one read
    /// means issuance never needs a second round trip, and the badge and the
    /// order can never disagree about what the nest serves.
    async fn cert_status(&self, domains: Vec<String>) -> Result<CertStatusReply, DnsNestError>;

    /// `fauna.dns.probe_txt_visible` — "does every authoritative NS of
    /// `zone_name` serve a TXT at `record_name` with exactly `txt_value`?", the
    /// DNS-01 propagation gate's readiness question resolved nest-side.
    ///
    /// Exists for the **web** arm of the gate: a browser has no raw DNS, so
    /// wasm cannot run [`AuthoritativeNsProbe`] in-process the way the 5 native
    /// apps and the tui do. Routing it through the nest keeps the *mechanism*
    /// identical (the nest runs the same
    /// `fauna_core::authoritative_dns::authoritative_txt_visible`) rather than
    /// giving web a weaker recursive/DoH approximation — and nest-side
    /// resolution is already this admin surface's ratified shape
    /// (`dns-management.md` § Live verification). Consumed by
    /// [`NestRelayedProbe`]; nothing else calls it.
    ///
    /// Lives on this read/verify seam beside `verify_records` for the same
    /// reason `cert_status` does: it is just another nest read the same admin
    /// glue speaks.
    async fn probe_txt_visible(
        &self,
        zone_name: String,
        record_name: String,
        txt_value: String,
    ) -> Result<bool, DnsNestError>;
}

/// The **web** arm of the DNS-01 propagation gate: a
/// [`Dns01ResolvabilityProbe`] that answers by asking the nest
/// ([`DnsNest::probe_txt_visible`]) instead of querying DNS itself.
///
/// A browser has no raw DNS, so wasm cannot run [`AuthoritativeNsProbe`]
/// in-process. Rather than give web a *different, weaker* signal — a public DoH
/// resolver is recursive, and would negative-cache the very miss the gate's own
/// polling plants — web asks the nest to run the identical authoritative-direct
/// query. One mechanism, two transports: in-process on native, one RPC hop away
/// on web (`tls-certificates.md` § B tier 2).
///
/// **Degrades to today's behavior when the nest cannot answer, with no version branch.**
/// [`Dns01ResolvabilityProbe`]'s contract makes every failure `false` — "not
/// visible yet" — so a nest that cannot answer yields a probe that
/// never confirms, the gate polls to its deadline and proceeds best-effort. That
/// is precisely what the fixed wait did, only bounded by a deadline sized for a
/// real provider publish instead of a blind guess.
pub struct NestRelayedProbe<'a> {
    nest: &'a dyn DnsNest,
}

impl<'a> NestRelayedProbe<'a> {
    pub fn new(nest: &'a dyn DnsNest) -> Self {
        Self { nest }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Dns01ResolvabilityProbe for NestRelayedProbe<'_> {
    async fn txt_visible(&self, zone_name: &str, record_name: &str, txt_value: &str) -> bool {
        match self
            .nest
            .probe_txt_visible(
                zone_name.to_string(),
                record_name.to_string(),
                txt_value.to_string(),
            )
            .await
        {
            Ok(visible) => visible,
            // Every failure reads as "not visible yet" — an unreachable nest or a
            // transient error is
            // the same to the gate, which keeps polling until its deadline.
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "acme dns-01: nest-relayed resolvability probe failed; \
                     treating as not-yet-visible"
                );
                false
            }
        }
    }
}

// The DNS management record's persistence seam — the account's
// `fauna.state.dns` row, read and written through the account-store handle.
mod store;
#[cfg(any(test, feature = "test-helpers"))]
pub use store::FakeDnsStore;
use store::update_dns;
pub use store::{AccountDnsStore, AccountHandleSource, DnsStore};

/// One record the client publishes for a managed domain. Projected from a
/// [`DnsRecordRow`] (the nest's expected matrix) by [`parse_rdata`]: the nest's
/// RFC-RDATA `expected` string is split into the provider-API `(value,
/// priority)` form. Passed to [`DnsProviderSeam::publish`]; the native seam
/// maps it 1:1 to `fauna_provisioning::dns::DnsRecord`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRecord {
    /// Bare record name (the wire `name` is already FQDN-without-trailing-dot —
    /// e.g. `example.com`, `_dmarc.example.com`).
    pub name: String,
    pub record_type: String,
    /// Provider-API record value: the MX host (priority split out), the
    /// unquoted TXT content, or the A/AAAA address verbatim.
    pub value: String,
    pub ttl_seconds: u32,
    /// MX priority (`10 mail.example.com` → `10`); `None` for record types
    /// without a priority.
    pub priority: Option<u32>,
}

/// Whether a rendered matrix record type is published through the DNS-provider
/// zone API in managed mode. The nest's matrix carries some rows that are
/// *shown* (manual paste / verify) but not zone-published by the managed
/// reconcile:
/// * `PTR` — reverse DNS is set at the IP owner (`vps.set_ptr`), never the
///   forward zone (`dns-management.md` § Records covered) — mirrors the
///   onboarding orchestrator's `T::Ptr` drop.
/// * `TLSA` — the cert-coupled floor-MX DANE record (`tls-certificates.md` § D)
///   is excluded from this *generic create-only* loop because it needs more than
///   a blind create: it must be **withdrawn** when a trusted cert takes over the
///   MX (a floor-key TLSA against a trusted cert DANE-fails), it is rejected by
///   providers that lack TLSA (Namecheap) — which would `?`-abort the whole
///   matrix publish — and Cloudflare needs structured `data` rather than a
///   content string. So the managed reconcile handles it in a **dedicated,
///   primary-only, best-effort converge pass** (`reconcile_floor_mx_tlsa`) that
///   creates the desired pin, withdraws a stale one, and logs (never aborts) on a
///   provider that can't represent it. Manual mode still shows the row for paste.
fn is_zone_publishable(record_type: &str) -> bool {
    !matches!(record_type, "PTR" | "TLSA")
}

/// Split the nest's RFC-RDATA `expected` value (`fauna.dns.list_records`) into
/// the provider-API `(value, priority)` form. The nest emits MX as
/// `<priority> <host>`, TXT double-quoted, and A/AAAA/… as the bare value
/// (`bins/fauna-nest/src/dns_handlers.rs` + `fauna_mail::dns::per_domain`); the
/// provider API wants the host + separate priority for MX and the unquoted
/// content for TXT. Names arrive bare (no trailing dot), so they pass through
/// unchanged at the call site.
fn parse_rdata(record_type: &str, expected: &str) -> (String, Option<u32>) {
    match record_type {
        "MX" => match expected.split_once(char::is_whitespace) {
            Some((prio, host)) => match prio.parse::<u32>() {
                Ok(p) => (host.trim().to_string(), Some(p)),
                // Unparseable priority → pass the RDATA through verbatim rather
                // than silently dropping it; the provider rejects if it's wrong.
                Err(_) => (expected.to_string(), None),
            },
            None => (expected.to_string(), None),
        },
        // The nest wraps TXT bodies in one pair of double quotes; the provider
        // API wants the raw content.
        "TXT" => (expected.trim_matches('"').to_string(), None),
        // SRV (`_caldavs._tcp` CalDAV autodiscovery): the nest emits the full
        // zone-file RDATA `<priority> <weight> <port> <target>`, which is exactly
        // what the providers want as the record value (Hetzner reconstructs
        // zone-file form from value+priority, so value-as-RDATA with no separate
        // priority round-trips; Cloudflare/Gandi take the RDATA as `content`).
        // Pass it through verbatim — same as the catch-all, made explicit so the
        // autodiscovery record's managed-publish path is intentional.
        "SRV" => (expected.to_string(), None),
        // TLSA (`_25._tcp.mail.<primary>` floor-MX DANE): the nest emits the full
        // `<usage> <selector> <matching> <hex>` RDATA, which is exactly the
        // provider value for Hetzner/Porkbun/Gandi (and the source Cloudflare's
        // TLSA branch parses into structured `data`). Pass through verbatim — same
        // as SRV — so the dedicated cert-coupled TLSA reconcile pass builds the
        // desired `PublishRecord` from one source.
        "TLSA" => (expected.to_string(), None),
        _ => (expected.to_string(), None),
    }
}

/// Converge the cert-coupled floor-MX DANE `TLSA` at `_25._tcp.mail.<primary>`
/// to the matrix's `desired` set (0 or 1 row) — the withdraw-aware half the
/// generic create-only loop can't do (`tls-certificates.md` § D). Called only
/// for the **primary** domain (it owns the one shared deployment slot;
/// `primary_domain` is that domain, so the shared `fauna-mail` builder yields the
/// slot name). **Best-effort:** the DANE pin is one optional row on top of
/// MTA-STS, so any provider error (a provider without TLSA — Namecheap — or a
/// transient API failure) is logged, never propagated, so it can't abort the
/// core SPF/MX/DKIM publish that already succeeded.
///
/// - **Create** the desired pin if not already published (floor state).
/// - **Withdraw** a published TLSA absent from `desired`: a trusted cert took
///   over the MX (`desired` empty → withdraw-on-trusted) or a deliberate
///   floor-key rotation changed the pin (stale value). A floor-key TLSA against a
///   trusted cert DANE-fails senders, so the stale row must go.
///
/// Idempotent and matrix-snapshot-driven (the 5a "matrix is the single source"
/// invariant): a floor↔trusted race against the snapshot resolves on the next
/// publish. Hex comparison is case-insensitive ([`tlsa_value_eq`]).
async fn reconcile_floor_mx_tlsa(
    provider: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    primary_domain: &str,
    desired: &[PublishRecord],
) {
    let slot = fauna_mail::dns::host::build_mail_tlsa_record(primary_domain, &[0u8; 32]).name;
    let published = match provider
        .find_records(provider_id, fields, zone, &slot, "TLSA")
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                target: "fauna_dns",
                "floor-MX TLSA reconcile: find_records({slot}) failed; leaving the slot \
                 as-is (DANE pin unverified, MTA-STS still protects the MX): {e}"
            );
            return;
        }
    };
    let to_create: Vec<PublishRecord> = desired
        .iter()
        .filter(|d| !published.iter().any(|p| tlsa_value_eq(&p.value, &d.value)))
        .cloned()
        .collect();
    if !to_create.is_empty()
        && let Err(e) = provider
            .publish(provider_id, fields, zone, &to_create)
            .await
    {
        tracing::warn!(
            target: "fauna_dns",
            "floor-MX TLSA reconcile: publishing the DANE pin at {slot} failed (the provider \
             may not support TLSA — e.g. Namecheap; MTA-STS still protects the MX): {e}"
        );
    }
    let to_delete: Vec<PublishRecord> = published
        .into_iter()
        .filter(|p| !desired.iter().any(|d| tlsa_value_eq(&p.value, &d.value)))
        .collect();
    if !to_delete.is_empty()
        && let Err(e) = provider
            .teardown(provider_id, fields, zone, &to_delete)
            .await
    {
        tracing::warn!(
            target: "fauna_dns",
            "floor-MX TLSA reconcile: withdrawing the stale DANE pin at {slot} failed (a \
             floor-key TLSA against a trusted cert DANE-fails senders): {e}"
        );
    }
}

/// Case-insensitive TLSA RDATA equality. The body is `<usage> <selector>
/// <matching> <hex>`; only the trailing hex can differ in case across providers
/// (Cloudflare's read-back hex case is undocumented), and the numeric prefix is
/// ASCII-equal to itself case-insensitively, so a whole-string ASCII-case
/// compare is correct and avoids brittle re-parsing of the components.
fn tlsa_value_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// A matrix row that is one of Fauna's DKIM TXT slots
/// (`<selector>._domainkey.<domain>` — `mail-multidomain.md` § DKIM).
fn is_dkim_txt_row(record_type: &str, name: &str) -> bool {
    record_type == "TXT" && name.contains("._domainkey.")
}

/// A matrix row that is one of Fauna's ATProto handle-verification TXT slots
/// (`_atproto.<handle>.<primary>` — `atproto-pds-bridge.md` § Handle). The slot
/// is Fauna-exclusive by construction: `_`-prefixed labels are charset-impossible
/// as Fauna handles (§ Handle, edge 1), so nothing else Fauna manages, and no
/// user-derivable subdomain, can land here.
fn is_atproto_txt_row(record_type: &str, name: &str) -> bool {
    record_type == "TXT" && name.starts_with("_atproto.")
}

/// A matrix row that is Fauna's public-domain identity root
/// (`_fauna.<domain>` TXT `self=` — `dns-management.md` § Records covered; one
/// per public local domain since the 2026-08-13 per-domain ruling, so this
/// predicate legitimately matches on a secondary's matrix too). ⚠ Unlike the
/// other two slots, the NAME is not Fauna-exclusive:
/// `_fauna.<domain>` is the deployment's shared Fauna TXT node, whose grammar
/// also carries `subhandles=` and `cache=` (`fauna_core::resolve`). Only the
/// slot's `self=` marker makes withdrawal safe here — never widen this to a
/// name-scoped sweep.
fn is_fauna_self_txt_row(record_type: &str, name: &str) -> bool {
    record_type == "TXT" && name.starts_with("_fauna.")
}

/// TXT value equality across provider read-back forms: providers differ in
/// whether TXT content comes back quoted, so strip one layer of double quotes
/// on each side, then compare **exactly** — both slot bodies are
/// case-sensitive (DKIM's `p=` is base64, ATProto's `did=` is a DID string), so
/// no case folding (unlike [`tlsa_value_eq`]).
fn txt_slot_value_eq(a: &str, b: &str) -> bool {
    a.trim_matches('"') == b.trim_matches('"')
}

/// One Fauna-exclusive TXT slot the withdraw-aware converge pass can manage.
///
/// The pass is deliberately **per exclusively-owned slot** rather than a
/// matrix-wide diff (`dns-management.md` § Fauna-managed → *Withdraw-aware
/// convergence*: "a whole-matrix withdraw is rejected"), so each slot supplies
/// the marker that scopes withdrawal to values Fauna itself publishes.
#[derive(Clone, Copy)]
struct TxtSlot {
    /// Log prefix identifying the pass in `fauna_dns` traces.
    label: &'static str,
    /// Substring every Fauna-published value at this slot carries; a published
    /// value without it is a foreign record and is never withdrawn.
    marker: &'static str,
}

/// Fauna's DKIM TXT slot — withdrawal scoped to published `p=` values.
const DKIM_SLOT: TxtSlot = TxtSlot {
    label: "DKIM converge",
    marker: "p=",
};

/// Fauna's ATProto handle-verification TXT slot — withdrawal scoped to
/// published `did=` values.
const ATPROTO_SLOT: TxtSlot = TxtSlot {
    label: "ATProto handle converge",
    marker: "did=",
};

/// Fauna's public-domain identity-root TXT slot — withdrawal scoped to
/// published `self=` values. The marker scoping is what makes this slot safe at
/// a name Fauna does **not** exclusively own (see [`is_fauna_self_txt_row`]).
const FAUNA_SELF_SLOT: TxtSlot = TxtSlot {
    label: "Fauna identity-root converge",
    marker: "self=",
};

/// Converge one Fauna-exclusive TXT slot to the matrix's `desired` set — the
/// withdraw-aware pass the generic create-only publish can't do
/// (`dns-management.md` § Fauna-managed → *Withdraw-aware convergence*).
///
/// The visit set is `desired ∪ remembered` names — `remembered` is the
/// client-side memory of names this pass previously managed — which is what
/// lets a name that has **left the matrix** still converge to removal. Scoping
/// is per Fauna-known name, **never** a zone-wide sweep of the slot's prefix: a
/// third-party record at a name Fauna never desired is outside the visit set by
/// construction. Withdrawal is further scoped to values carrying the slot's
/// [`TxtSlot::marker`] — extra safety at an already Fauna-known name.
///
/// **Best-effort**, mirroring [`reconcile_floor_mx_tlsa`]: any provider error
/// is logged, never propagated — the core matrix publish already landed. A
/// name whose read or withdraw failed **stays remembered**, so the next
/// managed publish retries the convergence instead of silently orphaning the
/// slot.
///
/// Returns the new remembered-name set for the domain: the currently-desired
/// names plus any name that could not be verified withdrawn.
async fn reconcile_exclusive_txt_slot(
    slot: TxtSlot,
    provider: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    desired: &[PublishRecord],
    remembered: &BTreeSet<String>,
) -> BTreeSet<String> {
    let TxtSlot { label, marker } = slot;
    let desired_names: BTreeSet<String> = desired.iter().map(|r| r.name.clone()).collect();
    let visit: BTreeSet<&String> = desired_names.iter().chain(remembered.iter()).collect();
    let mut next_remembered: BTreeSet<String> = desired_names.clone();
    for name in visit {
        let published = match provider
            .find_records(provider_id, fields, zone, name, "TXT")
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    target: "fauna_dns",
                    "{label}: find_records({name}) failed; leaving the slot as-is and \
                     keeping it remembered for the next publish: {e}"
                );
                next_remembered.insert(name.clone());
                continue;
            }
        };
        let desired_at: Vec<&PublishRecord> = desired.iter().filter(|d| &d.name == name).collect();
        let to_create: Vec<PublishRecord> = desired_at
            .iter()
            .filter(|d| {
                !published
                    .iter()
                    .any(|p| txt_slot_value_eq(&p.value, &d.value))
            })
            .map(|d| (*d).clone())
            .collect();
        if !to_create.is_empty()
            && let Err(e) = provider
                .publish(provider_id, fields, zone, &to_create)
                .await
        {
            tracing::warn!(
                target: "fauna_dns",
                "{label}: publishing the desired TXT at {name} failed: {e}"
            );
        }
        let to_delete: Vec<PublishRecord> = published
            .into_iter()
            .filter(|p| {
                p.value.contains(marker)
                    && !desired_at
                        .iter()
                        .any(|d| txt_slot_value_eq(&p.value, &d.value))
            })
            .collect();
        if !to_delete.is_empty()
            && let Err(e) = provider
                .teardown(provider_id, fields, zone, &to_delete)
                .await
        {
            tracing::warn!(
                target: "fauna_dns",
                "{label}: withdrawing the stale {marker} value at {name} failed; \
                 keeping it remembered: {e}"
            );
            next_remembered.insert(name.clone());
        }
    }
    next_remembered
}

/// Converge Fauna's DKIM TXT slots ([`reconcile_exclusive_txt_slot`] over
/// [`DKIM_SLOT`]). Two cases need a withdraw at a stable name: **re-mint** (a
/// fresh key under the same selector — the stale `p=` makes external verifiers
/// `dkim=fail`, the 2026-06-21 incident class) and **rotation cleanup** (a
/// revoked `<YYYYMM>` selector's TXT must be unpublished, after its name has
/// left the matrix — which is what `DnsConfig.dkim_published_names`
/// remembers). During a rotation's 24 h overlap both selectors are in the
/// matrix (both desired), so the old one is never withdrawn mid-overlap.
async fn reconcile_dkim_txt(
    provider: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    desired: &[PublishRecord],
    remembered: &BTreeSet<String>,
) -> BTreeSet<String> {
    reconcile_exclusive_txt_slot(
        DKIM_SLOT,
        provider,
        provider_id,
        fields,
        zone,
        desired,
        remembered,
    )
    .await
}

/// Converge Fauna's ATProto handle-verification TXT slots
/// ([`reconcile_exclusive_txt_slot`] over [`ATPROTO_SLOT`];
/// `atproto-pds-bridge.md` § Handle — "a Fauna handle or domain change
/// re-derives, republishes the `_atproto` TXT").
///
/// The withdraw case here is a **rename**. The ATProto handle is derived at
/// read time from the current Fauna handle, so a rename re-derives the matrix
/// row under a *new* name and the old `_atproto.<old>.<primary>` name leaves the
/// matrix entirely — which is exactly why the remembered set
/// (`DnsConfig.atproto_published_names`) is load-bearing: a matrix-only
/// visit set could never reach the stale TXT the rename left behind.
///
/// Leaving it published is not merely untidy. It fails *closed* for the renamed
/// user (resolution finds the DID, then the DID document's updated
/// `alsoKnownAs` no longer claims the old handle), but when a later user claims
/// the freed Fauna handle the create-only publish adds a **second** `did=` TXT
/// at that same name — an ambiguous handle that never resolves, stranding the
/// new user's repo behind the bridge's first-emit gate.
async fn reconcile_atproto_txt(
    provider: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    desired: &[PublishRecord],
    remembered: &BTreeSet<String>,
) -> BTreeSet<String> {
    reconcile_exclusive_txt_slot(
        ATPROTO_SLOT,
        provider,
        provider_id,
        fields,
        zone,
        desired,
        remembered,
    )
    .await
}

/// Converge Fauna's public-domain identity-root TXT
/// ([`reconcile_exclusive_txt_slot`] over [`FAUNA_SELF_SLOT`];
/// `dns-management.md` § Records covered — the `_fauna.<domain>` bullet, one
/// row per public local domain).
///
/// The withdraw case here is a **deployment-seed rotation**, and it is the one
/// shape neither sibling slot has: DKIM re-mints a value under a stable
/// selector, ATProto *moves* a name on rename — a rotation changes the `self=`
/// value at a name that never moves at all (`box-recovery.md` § Deployment-seed
/// rotation → *DNS row and the propagation window*). The nest re-derives the
/// row from its live deployment key, so the matrix simply reads differently
/// after the flip; the create-only publish sees a name it has already published
/// and does nothing, leaving the **superseded** identity resolvable beside the
/// successor. A fresh client resolving that zone would then be free to pin a
/// dead identity — which is exactly the acceptance the record exists to give
/// (`tls-certificates.md` § E).
///
/// ⚠ The visit set's `remembered` half is *not* load-bearing for the rotation
/// case (the name is in the matrix throughout); it carries the slot for the
/// cases where the name leaves — a domain that stops being primary, or a
/// withdraw that failed and must be retried.
async fn reconcile_fauna_self_txt(
    provider: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    desired: &[PublishRecord],
    remembered: &BTreeSet<String>,
) -> BTreeSet<String> {
    reconcile_exclusive_txt_slot(
        FAUNA_SELF_SLOT,
        provider,
        provider_id,
        fields,
        zone,
        desired,
        remembered,
    )
    .await
}

/// Client-side DNS-provider seam — `verify` a credential (returns the zones it
/// can manage) and `publish` records into a zone. The machine drives this with
/// the **decrypted** credential fields; the real impl over `fauna-provisioning`'s
/// `DnsProvider` (build the provider from `(provider_id, fields)`, run
/// `verify`/idempotent `create_record`) lives in the shared [`provider_seam`]
/// module and runs on **both** targets: native (direct reqwest) and wasm (the
/// browser fetch client, with cross-origin-restricted provider APIs routed
/// through `fauna_provisioning::proxy`'s CORS proxy — the same path the web
/// onboarding wizard already uses). Tests use a fake (`FAUNA_DNS_PROVIDER_FAKE`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait DnsProviderSeam: MaybeSendSync {
    /// Verify the credential and return the zones it can manage (`{id, name}`).
    /// The field values stay [`SecretString`] until the impl un-wraps them at
    /// the provider-API boundary.
    async fn verify(
        &self,
        provider_id: &str,
        fields: &[(String, SecretString)],
    ) -> Result<Vec<DnsZoneRef>, DnsProviderError>;

    /// Idempotently create `records` in `zone` (the impl pre-flights with
    /// `find_records` and skips matches, per the orchestrator pattern).
    async fn publish(
        &self,
        provider_id: &str,
        fields: &[(String, SecretString)],
        zone: &DnsZoneRef,
        records: &[PublishRecord],
    ) -> Result<(), DnsProviderError>;

    /// Value-based, idempotent inverse of [`publish`](DnsProviderSeam::publish):
    /// delete each of `records` from `zone` (matched by name + type + value),
    /// treating an already-absent record as success. The DNS-01 flow uses this
    /// to tear down the transient `_acme-challenge` TXT once the CA has
    /// validated; it routes through the same `DnsProviderSeam`, not a parallel
    /// path (`tls-certificates.md` § "_acme-challenge record").
    async fn teardown(
        &self,
        provider_id: &str,
        fields: &[(String, SecretString)],
        zone: &DnsZoneRef,
        records: &[PublishRecord],
    ) -> Result<(), DnsProviderError>;

    /// Read the records currently published at `(name, record_type)` in `zone`.
    /// The withdraw-aware managed reconcile uses this to converge the
    /// cert-coupled floor-MX `TLSA` slot: it diffs the published TLSAs against the
    /// matrix's desired set so a row that *left* the matrix (a trusted cert
    /// withdrew it, or a deliberate floor-key rotation changed it) is deleted via
    /// [`teardown`](DnsProviderSeam::teardown) (`tls-certificates.md` § D, the
    /// withdraw-on-trusted coupling). The bare-`PublishRecord` shape mirrors
    /// `publish`/`teardown`; the impl maps the provider's read model onto it.
    async fn find_records(
        &self,
        provider_id: &str,
        fields: &[(String, SecretString)],
        zone: &DnsZoneRef,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<PublishRecord>, DnsProviderError>;
}

/// Build the `_acme-challenge.<domain>` [`PublishRecord`] for an ACME order's
/// key-authorization `dns_value`, reusing the single shared record-shape builder
/// (`fauna_mail::dns::per_domain::build_acme_challenge_txt_record`) for the
/// record's name and short TTL. The seam wants the **raw** (unquoted) TXT value,
/// so `dns_value` passes through verbatim while the name and TTL come from the
/// canonical builder — one source of the record shape across the native order
/// and the manual-paste surface.
pub fn acme_challenge_publish_record(domain: &str, dns_value: &str) -> PublishRecord {
    acme_challenge_publish_record_named(&acme_challenge_record_name(domain), dns_value)
}

/// The default `_acme-challenge.<domain>` owner name for a domain's DNS-01
/// challenge, from the single shared builder (so the name derivation lives in
/// one place). The delegated-renewal path (S6b) overrides this with the
/// `CnameDelegation.target_name`.
pub fn acme_challenge_record_name(domain: &str) -> String {
    fauna_mail::dns::per_domain::build_acme_challenge_txt_record(domain, "").name
}

/// Build the `_acme-challenge` [`PublishRecord`] at an **explicit** owner `name`
/// (FQDN, bare) for the key-authorization `dns_value`. The default order
/// publishes at `_acme-challenge.<domain>` ([`acme_challenge_publish_record`]);
/// a delegated renewal (S6b) publishes the same record at the
/// `CnameDelegation.target_name` inside a controlled zone. The seam wants the
/// **raw** (unquoted) value, so `dns_value` passes through verbatim while the
/// short transient TTL comes from the canonical `fauna-mail` builder — one
/// source of the record shape across the native order and the manual-paste
/// surface.
pub fn acme_challenge_publish_record_named(name: &str, dns_value: &str) -> PublishRecord {
    PublishRecord {
        name: name.to_string(),
        record_type: "TXT".to_string(),
        value: dns_value.to_string(),
        ttl_seconds: fauna_mail::dns::per_domain::ACME_CHALLENGE_TTL_SECS,
        priority: None,
    }
}

/// Publish the transient DNS-01 `_acme-challenge` TXT for `dns_value` at owner
/// `publish_name` through the managed/manual [`DnsProviderSeam`] — the same path
/// every other managed record takes, never a parallel one (`tls-certificates.md`
/// § "The `_acme-challenge` record"). `publish_name` is `_acme-challenge.<domain>`
/// for an ordinary managed order ([`acme_challenge_record_name`]) or the
/// `CnameDelegation.target_name` for a delegated renewal (S6b). The DNS-01 order
/// core calls this before signalling the challenge ready;
/// [`teardown_acme_challenge`] retracts it after the order finalizes (always,
/// even on failure). Idempotent end to end: `publish` skips an already-present
/// identical value and `teardown`'s delete is a no-op when the record is gone.
pub async fn publish_acme_challenge(
    seam: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    publish_name: &str,
    dns_value: &str,
) -> Result<(), DnsProviderError> {
    seam.publish(
        provider_id,
        fields,
        zone,
        std::slice::from_ref(&acme_challenge_publish_record_named(
            publish_name,
            dns_value,
        )),
    )
    .await
}

/// Tear down the transient `_acme-challenge` TXT for `dns_value` at owner
/// `publish_name` published by [`publish_acme_challenge`] — value-based +
/// idempotent (already gone → `Ok`). Run after the ACME order finalizes
/// regardless of outcome so a failed order does not strand a stale challenge.
pub async fn teardown_acme_challenge(
    seam: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    publish_name: &str,
    dns_value: &str,
) -> Result<(), DnsProviderError> {
    seam.teardown(
        provider_id,
        fields,
        zone,
        std::slice::from_ref(&acme_challenge_publish_record_named(
            publish_name,
            dns_value,
        )),
    )
    .await
}

/// Does `zone_name` (a provider zone, bare — `example.com`) cover `domain`
/// (bare)? True for the apex itself or any subdomain. Both are bare names
/// (no trailing dot): the wire `DomainDns.domain` and the cached
/// `DnsZoneRef.name` are bare; only per-record `name`s carry trailing dots.
///
/// Deliberately **not** expressed on `orchestrator::dns_record_name`, despite
/// sharing a dot-boundary suffix test (considered and declined 2026-08-01).
/// They answer different questions at different times: this one is *zone
/// selection* — which of a credential's zones should a domain's records be
/// published into — decided before any record exists, while `dns_record_name`
/// is *record naming*, deciding the owner that goes on the wire once a zone is
/// chosen. Coupling them would make zone selection here move whenever the
/// provisioning crate's naming primitive changes (a punycode or trailing-dot
/// normalization added for owner names would silently re-scope which zones
/// match). The shared suffix test is an implementation coincidence, not a
/// shared concept.
fn zone_covers(zone_name: &str, domain: &str) -> bool {
    domain == zone_name || domain.ends_with(&format!(".{zone_name}"))
}

/// Sorted zone-name set of a credential — the dedup key for "same provider +
/// identical zone set" (the design's "one record per (provider, set of zones)").
fn zone_name_set(cred: &DnsProviderCredential) -> Vec<String> {
    let mut names: Vec<String> = cred.zones.iter().map(|z| z.name.clone()).collect();
    names.sort();
    names
}

/// Backs [`DnsManagementMachine`]'s mutex. Holds the rendered [`DnsSnapshot`]
/// plus the managed-domain opt-in set — the **secret-free** subset of
/// `fauna.state.dns` needed for the effective-mode projection. The provider
/// secret field values are deliberately **not** retained between operations
/// (the long-lived machine must not keep DNS API tokens resident — mirrors
/// `MailSettingsMachine`, which loads its custody per operation and holds only
/// secret-free `CredentialSummary` state); `Publish` re-loads them from the
/// sealed store on demand. The credential summaries live in `snapshot.credentials`.
struct Inner {
    snapshot: DnsSnapshot,
    managed_domains: BTreeSet<String>,
    /// Domains the admin opted **out** of auto-renew (`DnsConfig.auto_renew_off`)
    /// — the secret-free state [`project`] needs to overlay [`DomainView::auto_renew`].
    /// Empty ⇒ every managed/delegated domain auto-renews (default-on).
    auto_renew_off: BTreeSet<String>,
    /// A **manual-mode** DNS-01 order opened by `BeginManualIssueCert` and held
    /// suspended until `CompleteManualIssueCert` finalizes it (the admin pasted the
    /// surfaced `_acme-challenge` TXT) or `CancelManualIssueCert` drops it. Holds the
    /// live [`Dns01OrderInProgress`] (the order's challenge value is order-specific,
    /// so the order must stay open across the admin's out-of-band paste).
    /// Cross-target — the order driver is per-target (`acme_order` on native,
    /// `acme_pure` on wasm) but both expose `Dns01OrderInProgress`, so a manual order
    /// is now held on web too (web issues natively — W5).
    pending_order: Option<PendingManualOrder>,
}

/// The stash backing a suspended manual DNS-01 order (`Inner::pending_order`): the
/// live order plus the two seal inputs `CompleteManualIssueCert` needs — the serving
/// private nest id and the domain (the actor signing key lives on the machine).
/// `Dns01OrderInProgress` is the per-target order handle (`acme_order`/`acme_pure`).
struct PendingManualOrder {
    in_progress: Dns01OrderInProgress,
    domain: String,
    target_nest_id: [u8; 32],
}

/// Recompute each domain's **effective** `mode` (managed iff opted-in ∧ some
/// held credential's zones cover it) from the secret-free [`Inner`] state.
fn project(inner: &mut Inner) {
    let Inner {
        snapshot,
        managed_domains,
        auto_renew_off,
        ..
    } = inner;
    let DnsSnapshot {
        domains,
        credentials,
        delegations,
        ..
    } = snapshot;
    for dv in domains.iter_mut() {
        let covered = credentials
            .iter()
            .any(|c| c.zones.iter().any(|z| zone_covers(z, &dv.domain)));
        let managed = managed_domains.contains(&dv.domain) && covered;
        dv.mode = if managed { MODE_MANAGED } else { MODE_MANUAL }.to_string();
        // Auto-renew is possible only for a domain a client can auto-issue —
        // managed (held credential publishes `_acme-challenge`) or delegated (the
        // one-time CNAME re-homes it into a controlled zone). Default-on for those
        // unless the admin opted out. A manual-non-delegated domain can never
        // auto-renew, so it stays `false` regardless of the opt-out set.
        let delegated = delegations.iter().any(|d| d.domain == dv.domain);
        dv.auto_renew = (managed || delegated) && !auto_renew_off.contains(&dv.domain);
    }
}

/// Ingest a loaded/saved [`DnsConfig`] into the secret-free [`Inner`] state: the
/// managed-domain set and the credentials list (summaries only — the `dns`'s
/// secret `fields` are dropped here), then re-project effective modes.
fn apply_config(inner: &mut Inner, dns: DnsConfig) {
    // The manual-issuance paste surface. A machine holding the live order owns
    // its own surface and must not have it overwritten mid-flight; a machine
    // that does **not** (a fresh one built after a page navigation, an app
    // restart, or on a second device) re-surfaces it from the persisted
    // breadcrumb — without this, rebuilding the machine silently destroyed an
    // in-flight order and the admin saw the card simply vanish
    // (`tls-certificates.md` § Surviving an interrupted manual issuance). The same assignment clears a stale card once the issuance is
    // completed or cancelled elsewhere, since the breadcrumb is then `None`.
    if inner.pending_order.is_none() {
        inner.snapshot.pending_cert = dns.pending_manual_issue.as_ref().map(pending_issue_view);
    }
    inner.managed_domains = dns.managed_domains;
    inner.snapshot.credentials = dns
        .credentials
        .iter()
        .map(CredentialSummary::from)
        .collect();
    inner.snapshot.delegations = dns.delegations.iter().map(delegation_view).collect();
    inner.auto_renew_off = dns.auto_renew_off;
    project(inner);
}

/// Project a persisted [`CnameDelegation`] into the [`DelegationView`] the
/// `admin-dns` page renders: the one-time CNAME the admin sets at their registrar
/// — `_acme-challenge.<domain>` CNAME → `target_name`. A steady-state record, so
/// the ordinary default TTL (not the short transient challenge TTL).
fn delegation_view(d: &CnameDelegation) -> DelegationView {
    DelegationView {
        domain: d.domain.clone(),
        cname: DnsRecordRow {
            name: acme_challenge_record_name(&d.domain),
            record_type: "CNAME".to_string(),
            expected: d.target_name.clone(),
            ttl_seconds: fauna_mail::dns::per_domain::DEFAULT_TTL_SECS,
            verdict: None,
        },
    }
}

/// Project the persisted breadcrumb of an interrupted manual issuance
/// ([`PendingManualIssue`]) back onto the paste surface the `admin-dns` page
/// already renders. `verdict` is `None` — the live red/green comes from the
/// page's `VerifyRecords` pass and is never persisted.
fn pending_issue_view(p: &PendingManualIssue) -> PendingCertIssue {
    PendingCertIssue {
        domain: p.domain.clone(),
        challenges: p
            .challenges
            .iter()
            .map(|c| DnsRecordRow {
                name: c.name.clone(),
                record_type: c.record_type.clone(),
                expected: c.value.clone(),
                ttl_seconds: c.ttl_seconds,
                verdict: None,
            })
            .collect(),
    }
}

/// Does a freshly-opened order ask for exactly the `_acme-challenge` TXT(s) the
/// admin was already told to publish? Resuming an interrupted manual issuance
/// re-opens the order rather than reviving a handle that did not survive the
/// process, and the CA normally hands back the **same** challenge because the
/// authorization from the first attempt is still pending — in which case the
/// TXT already sitting at the registrar validates and completion just proceeds.
/// When it does *not* match, the admin must be shown the new value instead of
/// being left waiting on a token no CA will ever ask about. Compared as a set of
/// `(name, value)` pairs: order is not meaningful, and a differing count is a
/// mismatch.
fn challenge_values_match(persisted: &[PendingChallengeRecord], fresh: &[DnsRecordRow]) -> bool {
    if persisted.len() != fresh.len() {
        return false;
    }
    let mut want: Vec<(&str, &str)> = persisted
        .iter()
        .map(|c| (c.name.as_str(), c.value.as_str()))
        .collect();
    let mut got: Vec<(&str, &str)> = fresh
        .iter()
        .map(|r| (r.name.as_str(), r.expected.as_str()))
        .collect();
    want.sort_unstable();
    got.sort_unstable();
    want == got
}

/// One instance per admin client. Holds the rendered snapshot + loaded DNS
/// config; drives the seams. Mirrors `LocalDomainMachine` /
/// `BridgeApprovalMachine` (snapshot + dispatch).
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct DnsManagementMachine {
    nest: Arc<dyn DnsNest>,
    /// Client-held credential store + provider seams. `None` on a machine built
    /// with [`Self::new`] (read/verify-only Phase-0 page); the credential /
    /// managed-mode actions then return [`DnsDispatchError::InvalidState`].
    config: Option<Arc<dyn DnsStore>>,
    provider: Option<Arc<dyn DnsProviderSeam>>,
    /// The admin's actor Ed25519 signing key — used to sign the LAN-TLS cert
    /// namespace entry the `IssueCert` orchestration seals to the private nest
    /// (S5). `Some` only on a machine built via [`Self::with_credentials`] (which
    /// always has the keypair); `None` on the read-only [`Self::new`] page, where
    /// `IssueCert` returns [`DnsDispatchError::InvalidState`]. The keypair is
    /// the admin's own session identity, already held by every seat that
    /// builds this machine, so holding it here is no new secret-residency surface. Read on
    /// **both** targets now that web issues natively (`issue_cert` / `require_issuer`
    /// — W5).
    actor_signing_key: Option<SigningKey>,
    inner: Mutex<Inner>,
    /// **`test-helpers` only, never in a release build.** Overrides the manual
    /// DNS-01 order path (`open_manual_order` / `complete_manual_issue`) to drive
    /// against a local pebble CA instead of production Let's Encrypt — the pebble
    /// real-wire proof (S4b) that `resume_manual_issue` really re-opens and
    /// completes against a real CA after the machine is dropped and rebuilt over
    /// the same config store (`open_manual_order` otherwise hardcodes
    /// [`Dns01OrderConfig::lets_encrypt`], which the free-function pebble test
    /// cannot reach because it drives `begin_dns01_order`/`complete_dns01_order`
    /// directly, never the machine).
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
    manual_order_test_seam: Option<ManualOrderTestSeam>,
}

/// See [`DnsManagementMachine::manual_order_test_seam`]. `test-helpers` only.
#[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
pub struct ManualOrderTestSeam {
    /// The ACME directory URL `open_manual_order` orders against, replacing
    /// [`Dns01OrderConfig::lets_encrypt`]'s production directory.
    pub directory_url: String,
    /// Builds a fresh CA-trusting HTTP client for each ACME account/order
    /// round-trip (`begin_dns01_order_with_http`) — a factory, not a single
    /// client, because [`Self`] is consulted twice per resume (the interrupted
    /// `begin` and the rebuilt machine's `resume`) and `instant_acme::HttpClient`
    /// is consumed by value.
    pub http_client: Box<dyn Fn() -> Box<dyn instant_acme::HttpClient> + Send + Sync>,
    /// The propagation-gate probe `complete_manual_issue` polls, replacing the
    /// production [`AuthoritativeNsProbe`] (real system DNS, unreachable for a
    /// pebble test's throwaway zone). `None` skips active polling — the fixed
    /// `propagation_wait` (always `Duration::ZERO` on the manual path) proceeds
    /// immediately, correct for an in-process DNS responder that already serves
    /// the pasted TXT synchronously.
    pub probe: Option<Box<dyn Dns01ResolvabilityProbe>>,
}

// `new`/`with_credentials` take `Arc<dyn …>` seams (not FFI types), so they stay
// in a plain (non-exported) impl alongside the private helpers. The FFI surface
// — `snapshot` (sync) + `hydrate`/`dispatch` (async) — lives in the exported impl
// blocks below. Mirrors the onboarding-machine `#[uniffi::export]`/private-helper
// layout.
impl DnsManagementMachine {
    /// Read/verify-only machine (the Phase-0 `admin-dns` page over
    /// `fauna.dns.{list_records,verify_records}`). No credential store; the
    /// credential / managed-mode actions return [`DnsDispatchError::InvalidState`].
    pub fn new(nest: Arc<dyn DnsNest>) -> Self {
        Self {
            nest,
            config: None,
            provider: None,
            actor_signing_key: None,
            inner: Mutex::new(Inner {
                snapshot: DnsSnapshot::empty(),
                managed_domains: BTreeSet::new(),
                auto_renew_off: BTreeSet::new(),
                pending_order: None,
            }),
            #[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
            manual_order_test_seam: None,
        }
    }

    /// Full machine with the client-held credential store + provider seams —
    /// drives `PutCredentials` / `ClearCredentials` / `SetMode` / `Publish` and
    /// the effective-mode projection. The shared builders wire `config` over
    /// [`AccountDnsStore`] and `provider` over
    /// `fauna-provisioning` (native).
    pub fn with_credentials(
        nest: Arc<dyn DnsNest>,
        config: Arc<dyn DnsStore>,
        provider: Arc<dyn DnsProviderSeam>,
        actor_signing_key: SigningKey,
    ) -> Self {
        Self {
            nest,
            config: Some(config),
            provider: Some(provider),
            actor_signing_key: Some(actor_signing_key),
            inner: Mutex::new(Inner {
                snapshot: DnsSnapshot::empty(),
                managed_domains: BTreeSet::new(),
                auto_renew_off: BTreeSet::new(),
                pending_order: None,
            }),
            #[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
            manual_order_test_seam: None,
        }
    }

    /// Install the pebble real-wire seam (S4b) on an already-built machine — the
    /// only way to exercise `open_manual_order`/`complete_manual_issue` against a
    /// local CA instead of production Let's Encrypt. `test-helpers` only.
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
    pub fn with_manual_order_test_seam(mut self, seam: ManualOrderTestSeam) -> Self {
        self.manual_order_test_seam = Some(seam);
        self
    }

    fn require_config(&self) -> Result<&Arc<dyn DnsStore>, DnsDispatchError> {
        self.config.as_ref().ok_or_else(|| {
            DnsDispatchError::InvalidState(
                "credential store not wired (machine built read-only via `new`)".into(),
            )
        })
    }

    fn require_provider(&self) -> Result<&Arc<dyn DnsProviderSeam>, DnsDispatchError> {
        self.provider.as_ref().ok_or_else(|| {
            DnsDispatchError::InvalidState(
                "dns provider not wired (machine built read-only via `new`)".into(),
            )
        })
    }

    fn require_issuer(&self) -> Result<&SigningKey, DnsDispatchError> {
        self.actor_signing_key.as_ref().ok_or_else(|| {
            DnsDispatchError::InvalidState(
                "actor signing key not wired (machine built read-only via `new`)".into(),
            )
        })
    }

    fn set_status(&self, status: DnsStatus) {
        self.inner.lock().expect("snapshot mutex").snapshot.status = status;
    }

    async fn refresh(&self) -> Result<(), DnsDispatchError> {
        self.set_status(DnsStatus::Loading);
        let domains = self.nest.list_records(None).await?;
        // If the credential store is wired, reload it too so the effective-mode
        // projection + credentials list reflect current state.
        let dns = match &self.config {
            Some(store) => Some(store.load().await?),
            None => None,
        };
        let mut inner = self.inner.lock().expect("snapshot mutex");
        // Fresh matrix → drop any prior verdicts (expected values may have
        // changed; stale red/green must not carry over). The page re-verifies.
        inner.snapshot.domains = domains.into_iter().map(DomainView::from).collect();
        match dns {
            Some(dns) => apply_config(&mut inner, dns),
            None => project(&mut inner),
        }
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }

    /// Overlay `verify_records` verdicts onto the current matrix by
    /// `(name, record_type)`. Rows the reply doesn't cover keep their prior
    /// verdict; verdicts for `(name, record_type)` not in the matrix are ignored
    /// (the two surfaces share one source, so this is defensive only).
    async fn verify(&self, domain: Option<String>) -> Result<(), DnsDispatchError> {
        self.set_status(DnsStatus::Working);
        let statuses = self.nest.verify_records(domain).await?;
        let mut inner = self.inner.lock().expect("snapshot mutex");
        for ds in statuses {
            let Some(view) = inner
                .snapshot
                .domains
                .iter_mut()
                .find(|d| d.domain == ds.domain)
            else {
                continue;
            };
            for rs in ds.records {
                if let Some(row) = view
                    .records
                    .iter_mut()
                    .find(|r| r.name == rs.name && r.record_type == rs.record_type)
                {
                    row.verdict = Some(RecordVerdict {
                        observed: rs.observed,
                        status: VerifyStatus::from_wire(&rs.status),
                    });
                }
            }
        }
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }

    /// Overlay the nest's served-cert health onto `snapshot.cert_statuses` for
    /// the domains currently in the matrix (`fauna.tls.cert_status`). Reads the
    /// domain names from the current snapshot (so it follows a `Refresh`), then
    /// replaces the cert-status rows wholesale — the reply is the authoritative
    /// per-domain set. An empty matrix yields an empty set (no-op). Pure read,
    /// runs on both targets.
    ///
    /// **Side-effect — cert-coupled TLSA auto-withdraw:** on a credentialed
    /// machine, a primary observed to flip **floor→trusted** here fires the
    /// continuous floor-MX TLSA withdraw ([`Self::withdraw_floor_mx_tlsa_on_trust`]),
    /// so a stale floor-key pin can't keep DANE-hard-failing senders until the next
    /// managed `Publish`. Best-effort and off the read path — it never fails the
    /// status read.
    async fn refresh_cert_status(&self) -> Result<(), DnsDispatchError> {
        self.set_status(DnsStatus::Working);
        // Snapshot the domain names to query, then drop the lock before awaiting
        // (the snapshot mutex is sync — never held across an `await`).
        let domains: Vec<String> = {
            let inner = self.inner.lock().expect("snapshot mutex");
            inner
                .snapshot
                .domains
                .iter()
                .map(|d| d.domain.clone())
                .collect()
        };
        let reply = self.nest.cert_status(domains).await?;
        let new_rows: Vec<CertStatusRow> = reply
            .statuses
            .into_iter()
            .map(CertStatusRow::from)
            .collect();
        // Swap in the fresh rows and capture (prior rows, primary domain) under the
        // lock, then run the cert-coupled TLSA withdraw after releasing it (the
        // snapshot mutex is sync — never held across the `await` below).
        let (prior, primary) = {
            let mut inner = self.inner.lock().expect("snapshot mutex");
            let prior = std::mem::replace(&mut inner.snapshot.cert_statuses, new_rows.clone());
            let primary = inner
                .snapshot
                .domains
                .iter()
                .find(|d| d.is_primary)
                .map(|d| d.domain.clone());
            inner.snapshot.status = DnsStatus::Idle;
            (prior, primary)
        };
        if let Some(primary) = primary {
            self.withdraw_floor_mx_tlsa_on_trust(&primary, &prior, &new_rows)
                .await;
        }
        Ok(())
    }

    /// Withdraw the stale floor-MX `_25._tcp.mail.<primary>` TLSA the instant the
    /// primary MX domain's served cert is observed to go **floor→trusted** — the
    /// continuous half of the cert-coupled `reconcile_floor_mx_tlsa` converge
    /// (5b.5) the managed `Publish` pass otherwise does only on its own schedule. A
    /// floor-key TLSA pinned against a now-trusted cert **hard-fails DANE-validating
    /// senders** (`tls-certificates.md` § D), so the published-zone withdrawal must
    /// not wait for the next managed reconcile (`tls-certificates.md`
    /// § Implementation status — the auto-republish-on-cert-flip follow-on).
    ///
    /// **Edge-triggered:** fires once when `current` reports the primary trusted
    /// (`!is_floor`) and `prior` did not — on-floor, or no prior observation at all,
    /// so a cert that flipped while the client was offline is caught on first sight.
    /// A steady trusted→trusted refresh does not re-hit the provider; the managed
    /// `Publish` pass still handles steady-state level convergence.
    ///
    /// **Not gated on `managed_domains` (option (a) — the deferred tangle's
    /// resolution):** the floor-MX TLSA is *automatic and cert-coupled, not an admin
    /// opt-in* (§ D), so its safety withdrawal follows cert reality for any primary
    /// whose **own** zone a held credential covers — including one whose cert was
    /// issued via the delegated-but-unmanaged `issue_cert` path (which skips the
    /// managed opt-in). A primary with no covering credential is a manual-mode zone
    /// the admin clears by hand — a clean no-op here.
    ///
    /// **Best-effort:** a load/provider error is logged (here or inside
    /// `reconcile_floor_mx_tlsa`), never propagated — the `RefreshCertStatus` read
    /// it rides on always succeeds; the stale pin then withdraws on the next pass.
    async fn withdraw_floor_mx_tlsa_on_trust(
        &self,
        primary: &str,
        prior: &[CertStatusRow],
        current: &[CertStatusRow],
    ) {
        // Only a credentialed machine can write DNS; the read-only Phase-0 page
        // can't (and need not) reconcile.
        let (Some(provider), Some(config)) = (self.provider.clone(), self.config.clone()) else {
            return;
        };
        let now_trusted = current.iter().any(|r| r.domain == primary && !r.is_floor);
        let was_trusted = prior.iter().any(|r| r.domain == primary && !r.is_floor);
        if !now_trusted || was_trusted {
            return; // not the floor(/unknown)→trusted edge
        }
        // Load the covering credential for the primary's **own** zone (where
        // `_25._tcp.mail.<primary>` lives) — independent of `managed_domains`.
        let dns = match config.load().await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    target: "fauna_dns",
                    "floor-MX TLSA auto-withdraw: loading the credential store failed; the \
                     stale pin withdraws on the next managed reconcile instead: {e}"
                );
                return;
            }
        };
        let Some(cred) = dns
            .credentials
            .iter()
            .find(|c| c.zones.iter().any(|z| zone_covers(&z.name, primary)))
        else {
            // Manual-mode primary (no held credential for its zone): the admin
            // removes the floor-MX TLSA by hand (§ D) — nothing to do here.
            return;
        };
        let zone = cred
            .zones
            .iter()
            .find(|z| zone_covers(&z.name, primary))
            .expect("a covering zone exists (just matched)");
        // Trusted ⇒ the desired floor-MX TLSA set is empty ⇒ withdraw any stale pin.
        reconcile_floor_mx_tlsa(
            provider.as_ref(),
            &cred.provider_id,
            &cred.fields,
            zone,
            primary,
            &[],
        )
        .await;
    }

    /// Verify a provider credential, then store it (with its `verify()` zones)
    /// in `DnsConfig.credentials`, replacing any existing credential with
    /// the same provider + identical zone set.
    async fn put_credentials(
        &self,
        provider_id: String,
        fields: Vec<DnsCredentialField>,
        label: String,
    ) -> Result<(), DnsDispatchError> {
        let provider = self.require_provider()?.clone();
        let config = self.require_config()?.clone();
        self.set_status(DnsStatus::Working);
        let bag: Vec<(String, SecretString)> =
            fields.into_iter().map(|f| (f.id, f.value)).collect();
        let zones = provider.verify(&provider_id, &bag).await?;

        let cred = DnsProviderCredential {
            provider_id: provider_id.clone(),
            fields: bag,
            zones,
            label,
            created_at: Timestamp::now_secs() as u64,
        };
        let key = zone_name_set(&cred);
        let (dns, ()) = update_dns(config.as_ref(), |dns| {
            match dns
                .credentials
                .iter_mut()
                .find(|c| c.provider_id == provider_id && zone_name_set(c) == key)
            {
                Some(slot) => *slot = cred,
                None => dns.credentials.push(cred),
            }
            Ok(())
        })
        .await?;

        let mut inner = self.inner.lock().expect("snapshot mutex");
        apply_config(&mut inner, dns);
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }

    /// Remove the held credential at `index` (its `snapshot().credentials`
    /// position). Domains that lose coverage re-render `"manual"` via the
    /// projection.
    async fn clear_credentials(&self, index: u32) -> Result<(), DnsDispatchError> {
        let config = self.require_config()?.clone();
        self.set_status(DnsStatus::Working);
        let (dns, ()) = update_dns(config.as_ref(), |dns| {
            let i = index as usize;
            if i >= dns.credentials.len() {
                return Err(DnsDispatchError::InvalidState(format!(
                    "credential index {index} out of range ({} held)",
                    dns.credentials.len()
                )));
            }
            dns.credentials.remove(i);
            Ok(())
        })
        .await?;

        let mut inner = self.inner.lock().expect("snapshot mutex");
        apply_config(&mut inner, dns);
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }

    /// Opt `domain` in/out of Fauna-managed DNS. Opting **in** requires a held
    /// credential whose zones cover `domain`, and publishes the domain's records
    /// once the opt-in is saved (managed mode has no separate publish step,
    /// `dns-management.md` § Fauna-managed). The opt-in is committed before the
    /// publish runs, so a failed publish leaves the domain managed and returns
    /// the publish error for `dispatch` to surface. Opting out is always
    /// allowed.
    async fn set_mode(&self, domain: String, managed: bool) -> Result<(), DnsDispatchError> {
        let config = self.require_config()?.clone();
        self.set_status(DnsStatus::Working);
        let (dns, ()) = update_dns(config.as_ref(), |dns| {
            if managed
                && !dns
                    .credentials
                    .iter()
                    .any(|c| c.zones.iter().any(|z| zone_covers(&z.name, &domain)))
            {
                return Err(DnsDispatchError::InvalidState(format!(
                    "no held DNS credential covers {domain}; add one before enabling managed mode"
                )));
            }
            if managed {
                dns.managed_domains.insert(domain.clone());
            } else {
                dns.managed_domains.remove(&domain);
            }
            Ok(())
        })
        .await?;

        {
            let mut inner = self.inner.lock().expect("snapshot mutex");
            apply_config(&mut inner, dns);
            inner.snapshot.status = DnsStatus::Idle;
        }
        if managed {
            self.publish(domain).await?;
        }
        Ok(())
    }

    /// Turn automatic certificate renewal on/off for `domain` by persisting the
    /// opt-OUT in `DnsConfig.auto_renew_off` (`SetAutoRenew`). Mirrors
    /// [`Self::set_mode`]'s load → mutate → save → re-project flow. No covering-
    /// credential precondition: the toggle is faithfully persisted for any domain
    /// (so the choice survives a later mode change), but only a managed/delegated
    /// domain projects `auto_renew = true` — [`project`] folds in the
    /// can-auto-issue check, so opting a manual-non-delegated domain "in" is a
    /// stored no-op until it becomes managed/delegated.
    async fn set_auto_renew(&self, domain: String, enabled: bool) -> Result<(), DnsDispatchError> {
        let config = self.require_config()?.clone();
        self.set_status(DnsStatus::Working);
        let (dns, ()) = update_dns(config.as_ref(), |dns| {
            if enabled {
                dns.auto_renew_off.remove(&domain);
            } else {
                dns.auto_renew_off.insert(domain);
            }
            Ok(())
        })
        .await?;

        let mut inner = self.inner.lock().expect("snapshot mutex");
        apply_config(&mut inner, dns);
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }

    /// Idempotently publish the expected record matrix for `domain` via the
    /// covering credential. Requires `domain` to be effectively managed
    /// (opted-in client-side ∧ covered by a freshly-loaded credential).
    async fn publish(&self, domain: String) -> Result<(), DnsDispatchError> {
        let provider = self.require_provider()?.clone();
        let config = self.require_config()?.clone();
        self.set_status(DnsStatus::Working);

        // The records come from the already-rendered nest matrix; the
        // credential (with its secret fields) is loaded fresh from the sealed
        // store — secrets are never retained between operations.
        // `records` = the generic create-only set (TLSA + PTR excluded by
        // `is_zone_publishable`); `desired_tlsa` = the matrix's floor-MX TLSA
        // rows (0 or 1) handled by the dedicated cert-coupled converge pass;
        // `is_primary` gates that pass (only the primary owns the shared slot).
        let (records, desired_tlsa, desired_dkim, desired_atproto, desired_fauna_self, is_primary) = {
            let inner = self.inner.lock().expect("snapshot mutex");
            let Some(dv) = inner.snapshot.domains.iter().find(|d| d.domain == domain) else {
                return Err(DnsDispatchError::InvalidState(format!(
                    "{domain} has no record matrix loaded; refresh first"
                )));
            };
            let to_publish_record = |r: &DnsRecordRow| {
                let (value, priority) = parse_rdata(&r.record_type, &r.expected);
                PublishRecord {
                    name: r.name.clone(),
                    record_type: r.record_type.clone(),
                    value,
                    ttl_seconds: r.ttl_seconds,
                    priority,
                }
            };
            let records = dv
                .records
                .iter()
                .filter(|r| {
                    let publishable = is_zone_publishable(&r.record_type);
                    if !publishable {
                        // Shown in the matrix (manual paste / verify) but not
                        // zone-published by the generic create-only loop (TLSA is
                        // handled by `reconcile_floor_mx_tlsa` below).
                        tracing::debug!(
                            target: "fauna_dns",
                            "managed publish skips non-zone-publishable {} {}",
                            r.record_type,
                            r.name
                        );
                    }
                    publishable
                })
                .map(&to_publish_record)
                .collect::<Vec<_>>();
            let desired_tlsa = dv
                .records
                .iter()
                .filter(|r| r.record_type == "TLSA")
                .map(&to_publish_record)
                .collect::<Vec<_>>();
            // The domain's DKIM TXT slots — a subset of `records` (the generic
            // loop creates them); the dedicated converge pass additionally
            // WITHDRAWS a stale/revoked slot's `p=` (`reconcile_dkim_txt`).
            let desired_dkim = dv
                .records
                .iter()
                .filter(|r| is_dkim_txt_row(&r.record_type, &r.name))
                .map(&to_publish_record)
                .collect::<Vec<_>>();
            // The domain's ATProto handle-verification TXT slots — likewise a
            // subset of `records` the generic loop creates; the dedicated pass
            // additionally WITHDRAWS the stale `did=` a rename left behind
            // (`reconcile_atproto_txt`).
            let desired_atproto = dv
                .records
                .iter()
                .filter(|r| is_atproto_txt_row(&r.record_type, &r.name))
                .map(&to_publish_record)
                .collect::<Vec<_>>();
            // The domain's `_fauna` identity-root TXT — likewise a subset of
            // `records` the generic loop creates; the dedicated pass
            // additionally WITHDRAWS the `self=` a deployment-seed ROTATION
            // superseded, which the create-only loop cannot see because the
            // name never moves (`reconcile_fauna_self_txt`).
            let desired_fauna_self = dv
                .records
                .iter()
                .filter(|r| is_fauna_self_txt_row(&r.record_type, &r.name))
                .map(&to_publish_record)
                .collect::<Vec<_>>();
            (
                records,
                desired_tlsa,
                desired_dkim,
                desired_atproto,
                desired_fauna_self,
                dv.is_primary,
            )
        };

        let dns = config.load().await?;
        if !dns.managed_domains.contains(&domain) {
            return Err(DnsDispatchError::InvalidState(format!(
                "{domain} is not opted into Fauna-managed DNS"
            )));
        }
        let Some(cred) = dns
            .credentials
            .iter()
            .find(|c| c.zones.iter().any(|z| zone_covers(&z.name, &domain)))
        else {
            return Err(DnsDispatchError::InvalidState(format!(
                "no held DNS credential covers {domain}"
            )));
        };
        let zone = cred
            .zones
            .iter()
            .find(|z| zone_covers(&z.name, &domain))
            .expect("a covering zone exists (just matched)");

        provider
            .publish(&cred.provider_id, &cred.fields, zone, &records)
            .await?;

        // Cert-coupled floor-MX DANE TLSA: only the primary domain owns the
        // shared `_25._tcp.mail.<primary>` slot, and it must be converged
        // (created on floor, withdrawn once a trusted cert covers the MX) — the
        // create-only loop above can't do that (`tls-certificates.md` § D).
        if is_primary {
            reconcile_floor_mx_tlsa(
                provider.as_ref(),
                &cred.provider_id,
                &cred.fields,
                zone,
                &domain,
                &desired_tlsa,
            )
            .await;
        }

        // Withdraw-aware DKIM converge (every domain owns its own `_domainkey`
        // slots — not primary-gated like the TLSA slot). Visit set = matrix
        // names ∪ the names this client previously managed, so a revoked
        // selector converges to removal; the memory is then updated to what is
        // still live (or still needs a retry) and persisted beside the
        // credential in `fauna.state.dns` (`dns-management.md` § Fauna-managed →
        // Withdraw-aware convergence).
        let remembered = dns
            .dkim_published_names
            .get(&domain)
            .cloned()
            .unwrap_or_default();
        let next_remembered = reconcile_dkim_txt(
            provider.as_ref(),
            &cred.provider_id,
            &cred.fields,
            zone,
            &desired_dkim,
            &remembered,
        )
        .await;

        // Withdraw-aware ATProto handle-TXT converge — the same mechanism over
        // the `_atproto.<handle>.<primary>` slot, whose withdraw case is a
        // **rename**: the handle is derived at read time, so the old name has
        // left the matrix and only the remembered set can still reach the stale
        // `did=` it left published (`atproto-pds-bridge.md` § Handle;
        // `dns-management.md` § Fauna-managed → Withdraw-aware convergence).
        // Not primary-gated, unlike the TLSA slot: the rows only appear on the
        // primary's matrix today, so a secondary's desired ∪ remembered is empty
        // and the pass makes zero provider calls — while a domain that ever
        // stops being primary still converges its remembered names to removal.
        let atproto_remembered = dns
            .atproto_published_names
            .get(&domain)
            .cloned()
            .unwrap_or_default();
        let next_atproto_remembered = reconcile_atproto_txt(
            provider.as_ref(),
            &cred.provider_id,
            &cred.fields,
            zone,
            &desired_atproto,
            &atproto_remembered,
        )
        .await;

        // Withdraw-aware identity-root converge — the same mechanism over the
        // `_fauna.<domain>` slot, whose withdraw case is a **deployment-seed
        // rotation**: the nest re-derives `self=` from its live deployment key,
        // so the VALUE changes at a name that never moves, and the create-only
        // loop (which only asks "does this name exist?") leaves the superseded
        // identity resolvable beside the successor (`box-recovery.md`
        // § Deployment-seed rotation; `dns-management.md` § Records covered).
        // Not primary-gated — and unlike the ATProto pass, that is now
        // load-bearing rather than merely future-proofing: since the 2026-08-13
        // per-domain ruling the row appears on **every public** local domain's
        // matrix, so a secondary really does have a non-empty desired set and
        // publishes here, into its own zone with its own covering credential
        // (both resolved per-domain above). A domain that goes non-public, or
        // leaves the deployment, still converges its remembered row to removal.
        let fauna_self_remembered = dns
            .fauna_self_published_names
            .get(&domain)
            .cloned()
            .unwrap_or_default();
        let next_fauna_self_remembered = reconcile_fauna_self_txt(
            provider.as_ref(),
            &cred.provider_id,
            &cred.fields,
            zone,
            &desired_fauna_self,
            &fauna_self_remembered,
        )
        .await;

        // One save covers all three memories — they are written by the same publish.
        if next_remembered != remembered
            || next_atproto_remembered != atproto_remembered
            || next_fauna_self_remembered != fauna_self_remembered
        {
            // Re-read before writing: the provider round-trips above took real
            // time, and the door replaces the whole record, so writing the copy
            // loaded before them would undo any change another device made
            // meanwhile (`store` module docs).
            let (dns, ()) = update_dns(config.as_ref(), |dns| {
                if next_remembered.is_empty() {
                    dns.dkim_published_names.remove(&domain);
                } else {
                    dns.dkim_published_names
                        .insert(domain.clone(), next_remembered);
                }
                if next_atproto_remembered.is_empty() {
                    dns.atproto_published_names.remove(&domain);
                } else {
                    dns.atproto_published_names
                        .insert(domain.clone(), next_atproto_remembered);
                }
                if next_fauna_self_remembered.is_empty() {
                    dns.fauna_self_published_names.remove(&domain);
                } else {
                    dns.fauna_self_published_names
                        .insert(domain.clone(), next_fauna_self_remembered);
                }
                Ok(())
            })
            .await?;
            let mut inner = self.inner.lock().expect("snapshot mutex");
            apply_config(&mut inner, dns);
        }

        self.set_status(DnsStatus::Idle);
        Ok(())
    }

    /// Set up a one-time `_acme-challenge` CNAME delegation for `domain` into
    /// `target_zone` (S6b — [`DnsAction::DelegateRenewal`]). A held credential
    /// must cover `target_zone` (that is where the renewal TXT auto-publishes);
    /// the re-homing target name is `_acme-challenge.<domain>.<target_zone>`
    /// (persisted authoritative — it is what the admin's CNAME points at). After
    /// the admin pastes the surfaced CNAME once, `issue_cert` renews automatically.
    /// Config-only (no ACME order) so it runs on both native and web.
    async fn delegate_renewal(
        &self,
        domain: String,
        target_zone: String,
    ) -> Result<(), DnsDispatchError> {
        let config = self.require_config()?.clone();
        self.set_status(DnsStatus::Working);

        let (dns, ()) = update_dns(config.as_ref(), |dns| {
            // The renewal TXT is auto-published into `target_zone`, so a held
            // credential must cover it — else the delegation could never renew.
            let covered = dns
                .credentials
                .iter()
                .any(|c| c.zones.iter().any(|z| zone_covers(&z.name, &target_zone)));
            if !covered {
                return Err(DnsDispatchError::InvalidState(format!(
                    "no held DNS credential covers the delegation target zone {target_zone}; \
                     delegate into a zone you control"
                )));
            }
            // Re-homing convention (one source: `tls-certificates.md` § B tier 3):
            // `_acme-challenge.<domain>` re-rooted under the controlled zone.
            let target_name = format!("{}.{}", acme_challenge_record_name(&domain), target_zone);
            dns.delegations.retain(|d| d.domain != domain);
            dns.delegations.push(CnameDelegation {
                domain,
                target_name,
                target_zone,
            });
            Ok(())
        })
        .await?;

        let mut inner = self.inner.lock().expect("snapshot mutex");
        apply_config(&mut inner, dns);
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }

    /// Remove `domain`'s CNAME delegation (S6b — [`DnsAction::RemoveDelegation`]);
    /// idempotent (a no-op when `domain` is not delegated). The domain reverts to
    /// manual paste-per-renewal. Config-only; both targets.
    async fn remove_delegation(&self, domain: String) -> Result<(), DnsDispatchError> {
        let config = self.require_config()?.clone();
        self.set_status(DnsStatus::Working);
        let (dns, ()) = update_dns(config.as_ref(), |dns| {
            dns.delegations.retain(|d| d.domain != domain);
            Ok(())
        })
        .await?;
        let mut inner = self.inner.lock().expect("snapshot mutex");
        apply_config(&mut inner, dns);
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }
}

/// Resolve, for a DNS-01 issuance/renewal of `domain`, the covering credential,
/// the zone the `_acme-challenge` TXT publishes into, and any delegated
/// challenge-publish-name redirects (S6b). Two cases:
///
/// - **CNAME-delegated** (a [`CnameDelegation`] exists for `domain`): the
///   credential covering the delegation's `target_zone` (a zone the admin
///   controls), publish into that zone, and redirect `domain`'s `_acme-challenge`
///   to the delegated `target_name` (the CA follows the admin's one-time CNAME).
///   This is how a manual-mode domain's renewals automate after the single CNAME.
/// - **Direct managed** (no delegation): the credential covering `domain` itself,
///   publish at the default `_acme-challenge.<domain>` in the domain's own zone.
///
/// Returns `InvalidState` when no held credential covers the needed zone — a
/// manual domain with neither a covering credential nor a delegation must instead
/// go through the manual paste path (`BeginManualIssueCert`). Factored out of
/// [`DnsManagementMachine::issue_cert`] so the delegated-vs-direct resolution is
/// unit-testable without a live CA (the CA half is S4b).
///
/// The resolved `(credential, publish-zone, challenge-publish-name redirects)` an
/// `issue_cert` order needs.
type IssuanceTarget = (DnsProviderCredential, DnsZoneRef, Vec<(String, String)>);

fn resolve_issuance_target(
    dns: &DnsConfig,
    domain: &str,
) -> Result<IssuanceTarget, DnsDispatchError> {
    // The zone the renewal TXT publishes into, and any publish-name redirect.
    let (target_zone, publish_names) = match dns.delegations.iter().find(|d| d.domain == domain) {
        Some(deleg) => (
            deleg.target_zone.clone(),
            vec![(domain.to_string(), deleg.target_name.clone())],
        ),
        None => (domain.to_string(), Vec::new()),
    };
    let cred = dns
        .credentials
        .iter()
        .find(|c| c.zones.iter().any(|z| zone_covers(&z.name, &target_zone)))
        .cloned()
        .ok_or_else(|| {
            if publish_names.is_empty() {
                DnsDispatchError::InvalidState(format!(
                    "no held DNS credential covers {domain}; DNS-01 issuance needs a covering \
                     credential (else paste the record manually — S6, or delegate renewal — S6b)"
                ))
            } else {
                DnsDispatchError::InvalidState(format!(
                    "{domain}'s renewal delegation targets zone {target_zone}, but no held DNS \
                     credential covers it; re-delegate into a zone you control or remove the \
                     delegation"
                ))
            }
        })?;
    let zone = cred
        .zones
        .iter()
        .find(|z| zone_covers(&z.name, &target_zone))
        .expect("a covering zone exists (just matched)")
        .clone();
    Ok((cred, zone, publish_names))
}

/// The identifier set a DNS-01 order should request for `domain`.
///
/// An installed client-issued cert becomes the nest's **listener** cert
/// (`store_acme_material` writes `fullchain.pem`/`privkey.pem`), so it must cover
/// what the listener serves — the apex, the single MX host `mail.<primary>`, each
/// active mail domain's apex, any enabled infra host. Ordering the clicked domain
/// alone would silently *narrow* a working cert on renewal: on a mail deployment
/// it drops `mail.<primary>`, and every MUA falls back to the self-signed floor.
/// So the nest reports its desired set (`CertStatusReply::desired_sans`) and this
/// picks the orderable subset.
///
/// Two filters, each load-bearing:
///
/// * **Credential coverage.** An ACME order is all-or-nothing, so a name whose
///   `_acme-challenge` this credential cannot publish would fail the *whole*
///   order — including the domain the admin actually asked for. Names outside the
///   credential's zones are therefore dropped, not attempted.
/// * **Delegation.** A CNAME-delegated domain (S6b) re-homes only *its own*
///   `_acme-challenge` into the controlled zone; a sibling SAN's challenge name
///   has no such CNAME and would have to be published in the very zone the
///   delegation exists because the client cannot write. So a delegated order stays
///   single-name.
///
/// `domain` always leads the result — it is the row the admin clicked and the
/// name the order is *for* — and is included even when the nest omits it (an
/// inactive or just-added domain), so the request can never come back empty.
fn order_san_set(
    domain: &str,
    desired_sans: Vec<String>,
    cred: &DnsProviderCredential,
    challenge_publish_names: &[(String, String)],
) -> Vec<String> {
    let mut out = vec![domain.to_string()];
    // Delegated → single-name (see above).
    if !challenge_publish_names.is_empty() {
        return out;
    }
    for name in desired_sans {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || out.iter().any(|x| x == &name) {
            continue;
        }
        if cred.zones.iter().any(|z| zone_covers(&z.name, &name)) {
            out.push(name);
        }
    }
    out
}

// ── DNS-01 cert issuance (S5) — cross-target: the order driver is per-target
// (`acme_order` native / `acme_pure` wasm), but the orchestration, seal, and
// `fauna.tls.publish_cert` delivery are identical, so web issues natively (W5). ──
impl DnsManagementMachine {
    /// Orchestrate a client-driven DNS-01 issuance for `domain` and deliver the
    /// issued cert to the private nest `target_nest_id` that serves it — the glue
    /// joining the S4a order core (Half A) to the S1 `fauna.tls.publish_cert`
    /// delivery seam (Half B):
    ///
    /// 1. resolve the covering DNS credential + zone for `domain` (reusing the
    ///    `Publish` coverage logic) and load the persisted ACME account (D6);
    /// 2. run [`obtain_certificate_dns01`](crate::obtain_certificate_dns01) —
    ///    publishes/​tears down the transient `_acme-challenge` TXT through the
    ///    provider seam, then finalizes;
    /// 3. seal the issued `TlsCertBundle` to `target_nest_id`'s identity x25519
    ///    key, sign it with the admin's actor key, and deliver it over
    ///    `fauna.tls.publish_cert` (Half B);
    /// 4. persist the (reused-or-created) ACME account back to `fauna.state.dns`
    ///    so any synced device renews against the same account.
    ///
    /// Every failure is non-fatal — the nest stays on the Phase-2 self-signed
    /// floor until the next attempt (`tls-certificates.md` § A/C).
    async fn issue_cert(
        &self,
        domain: String,
        target_nest_id: Vec<u8>,
    ) -> Result<(), DnsDispatchError> {
        let provider = self.require_provider()?.clone();
        let config = self.require_config()?.clone();
        let actor_sk = self.require_issuer()?.clone();
        // Validate the seal target up front — before any CA work — so a bad id
        // fails fast instead of after an order round-trip.
        let target: [u8; 32] = target_nest_id.as_slice().try_into().map_err(|_| {
            DnsDispatchError::InvalidState(format!(
                "target nest id must be 32 bytes, got {}",
                target_nest_id.len()
            ))
        })?;

        self.set_status(DnsStatus::Working);

        // Resolve the covering credential + zone + any delegated publish-name
        // redirect (S6b) + the persisted ACME account. A freshly-loaded
        // credential (with its secret fields) — never retained between
        // operations, mirroring `publish`.
        let dns = config.load().await?;
        let (cred, zone, challenge_publish_names) = resolve_issuance_target(&dns, &domain)?;

        // Which names the cert must carry. An installed client-issued cert becomes
        // the nest's **listener** cert, so ordering the clicked domain alone would
        // narrow it — dropping e.g. `mail.<primary>` and pushing every MUA onto the
        // self-signed floor. Ask the nest what its listener should cover and order
        // that (`order_san_set`).
        //
        // A failed read aborts issuance rather than falling back to `[domain]`:
        // silently narrowing the cert is precisely the regression this prevents,
        // and failing here costs nothing — it is before any CA contact, so no
        // issuance budget is spent, and the publish would need this same
        // connection anyway.
        let order_names = order_san_set(
            &domain,
            self.nest
                .cert_status(vec![domain.clone()])
                .await?
                .desired_sans,
            &cred,
            &challenge_publish_names,
        );

        // Drive the order. Contact email is empty for now (Let's Encrypt does not
        // require one and Fauna drives renewal reminders in-product — § C.4); a
        // contact-email UI knob is a Phase-4 polish. The account creds round-trip
        // in/out (D6). `challenge_publish_names` re-homes a delegated domain's
        // `_acme-challenge` TXT into the controlled zone (S6b); empty otherwise.
        let mut order_cfg = Dns01OrderConfig::lets_encrypt(String::new(), order_names);
        order_cfg.challenge_publish_names = challenge_publish_names;
        // Actively poll the publish zone's authoritative NS and signal the CA the
        // moment every challenge TXT is really served (bounded by
        // `resolvability_deadline`) — a fixed wait cannot cover a slow provider
        // zone-publish (Hetzner measured ≥ 10–15 min, 2026-07-23/24). Same
        // mechanism on every app; only the transport differs, because a browser
        // has no raw DNS: native queries the NS in-process, wasm asks the nest to
        // run the identical query (`NestRelayedProbe`).
        #[cfg(not(target_arch = "wasm32"))]
        let probe = AuthoritativeNsProbe::new();
        #[cfg(target_arch = "wasm32")]
        let probe = NestRelayedProbe::new(self.nest.as_ref());
        let probe: Option<&dyn Dns01ResolvabilityProbe> = Some(&probe);
        let issued = obtain_certificate_dns01(
            &order_cfg,
            dns.acme_account.as_deref(),
            provider.as_ref(),
            &cred.provider_id,
            &cred.fields,
            &zone,
            probe,
        )
        .await
        .map_err(|e| DnsDispatchError::Issuance(e.to_string()))?;

        // Half B: seal + deliver. Then persist the (possibly new) ACME account so
        // the next renewal — on any synced device — reuses it.
        self.deliver_issued_cert(&domain, &target, &actor_sk, &issued)
            .await?;
        // Re-read before writing: the order above can run for many minutes, and
        // the door replaces the whole record (`store` module docs).
        update_dns(config.as_ref(), |dns| {
            dns.acme_account = Some(issued.account_credentials);
            Ok(())
        })
        .await?;

        self.set_status(DnsStatus::Idle);
        Ok(())
    }

    /// Half B in isolation: seal `issued` to the target private nest's identity,
    /// sign it with the admin's actor key, and deliver it over
    /// `fauna.tls.publish_cert`. Factored out of [`Self::issue_cert`] so the
    /// seal→publish choreography is unit-testable with a fake nest seam (and a
    /// rcgen-issued cert), without a live CA — the CA half is S4b.
    async fn deliver_issued_cert(
        &self,
        domain: &str,
        target_nest_id: &[u8; 32],
        actor_sk: &SigningKey,
        issued: &Dns01Issued,
    ) -> Result<(), DnsDispatchError> {
        let bundle = acme_shared::tls_cert_bundle_from_issued(issued)
            .map_err(|e| DnsDispatchError::Issuance(e.to_string()))?;
        // The seal target is the private nest's identity-derived x25519 pubkey
        // (its published, client-pinned Ed25519 node id → x25519) — the same
        // convention the Slice-4 consumer unseals with (`apply_synced_lan_cert`).
        let target_x25519 = fauna_core::identity::ActorId(*target_nest_id)
            .to_x25519_public()
            .to_bytes();
        let (ciphertext, actor_sig) = fauna_mls::wrapped_blob::seal_lan_tls_cert_entry(
            &bundle,
            domain,
            &target_x25519,
            actor_sk,
        )
        .map_err(|e| DnsDispatchError::Issuance(format!("seal cert: {e}")))?;
        let ok = self
            .nest
            .publish_cert(PublishCertRequest {
                ciphertext: fauna_protocol::ByteBuf::from(ciphertext),
                actor_sig: fauna_protocol::ByteBuf::from(actor_sig),
                extra: Default::default(),
            })
            .await?;
        if !ok {
            return Err(DnsDispatchError::Issuance(
                "nest did not store the published cert".to_string(),
            ));
        }
        Ok(())
    }

    /// **Manual-mode** DNS-01 issuance, phase 1 ([`DnsAction::BeginManualIssueCert`]):
    /// open a client-driven DNS-01 order for a domain with **no covering DNS
    /// credential** and surface its transient `_acme-challenge` TXT(s) on
    /// `snapshot.pending_cert` for the admin to paste at their registrar. The live
    /// order is stashed in `Inner::pending_order` and resumed by
    /// [`Self::complete_manual_issue`] (the admin confirmed) or dropped by
    /// [`Self::cancel_manual_issue`] (the admin declined). Mirrors [`Self::issue_cert`]'s
    /// fail-fast preconditions; unlike it there is no covering-credential lookup and
    /// no provider seam — the admin publishes the record by hand.
    async fn begin_manual_issue(
        &self,
        domain: String,
        target_nest_id: Vec<u8>,
    ) -> Result<(), DnsDispatchError> {
        // The full machine is required (ACME-account persistence + the seal signing
        // key the completion step needs) — fail fast before any CA round-trip.
        let config = self.require_config()?.clone();
        self.require_issuer()?;
        let target: [u8; 32] = target_nest_id.as_slice().try_into().map_err(|_| {
            DnsDispatchError::InvalidState(format!(
                "target nest id must be 32 bytes, got {}",
                target_nest_id.len()
            ))
        })?;
        // One manual order at a time — a second begin would orphan the first.
        if self
            .inner
            .lock()
            .expect("snapshot mutex")
            .pending_order
            .is_some()
        {
            return Err(DnsDispatchError::InvalidState(
                "a manual cert order is already awaiting confirmation; complete or cancel it first"
                    .to_string(),
            ));
        }

        self.set_status(DnsStatus::Working);

        let dns = config.load().await?;
        let (in_progress, challenges) = self.open_manual_order(&dns, &domain).await?;
        self.stash_manual_order(&config, in_progress, domain, target, challenges)
            .await
    }

    /// Open a fresh manual DNS-01 order for `domain` and project its transient
    /// `_acme-challenge` TXT(s) onto the shared record-row shape the
    /// `admin-dns-record` component already renders (one writer, one record type
    /// — not a parallel surface). `verdict` is `None` until the client's
    /// `VerifyRecords` overlays the live red/green status.
    ///
    /// Reuses the persisted ACME account (D6; `None` on first issuance → a fresh
    /// account is created and persisted by the completion step). Empty contact
    /// email — Fauna drives reminders in-product (§ C.4). Shared by the begin
    /// step and by a **resume** ([`Self::resume_manual_issue`]), which re-opens
    /// rather than reviving an order handle that did not survive the process.
    async fn open_manual_order(
        &self,
        dns: &DnsConfig,
        domain: &str,
    ) -> Result<(Dns01OrderInProgress, Vec<DnsRecordRow>), DnsDispatchError> {
        #[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
        if let Some(seam) = &self.manual_order_test_seam {
            let mut order_cfg =
                Dns01OrderConfig::lets_encrypt(String::new(), vec![domain.to_string()]);
            order_cfg.directory_url = seam.directory_url.clone();
            let in_progress = begin_dns01_order_with_http(
                &order_cfg,
                dns.acme_account.as_deref(),
                (seam.http_client)(),
            )
            .await
            .map_err(|e| DnsDispatchError::Issuance(e.to_string()))?;
            let challenges = Self::project_manual_challenges(&in_progress);
            return Ok((in_progress, challenges));
        }
        let order_cfg = Dns01OrderConfig::lets_encrypt(String::new(), vec![domain.to_string()]);
        let in_progress = begin_dns01_order(&order_cfg, dns.acme_account.as_deref())
            .await
            .map_err(|e| DnsDispatchError::Issuance(e.to_string()))?;
        let challenges = Self::project_manual_challenges(&in_progress);
        Ok((in_progress, challenges))
    }

    /// Project a manual order's transient `_acme-challenge` TXT(s) onto the
    /// shared record-row shape the `admin-dns-record` component renders. Shared
    /// by [`Self::open_manual_order`]'s production and `test-helpers` paths so
    /// the mapping has one home.
    fn project_manual_challenges(in_progress: &Dns01OrderInProgress) -> Vec<DnsRecordRow> {
        in_progress
            .challenges_to_publish()
            .into_iter()
            .map(|r| DnsRecordRow {
                name: r.name,
                record_type: r.record_type,
                expected: r.value,
                ttl_seconds: r.ttl_seconds,
                verdict: None,
            })
            .collect()
    }

    /// Hold a freshly-opened manual order and surface its paste card. The
    /// **breadcrumb is persisted first**: the live [`Dns01OrderInProgress`] is a
    /// process-local handle, so if the config write fails the admin must not be
    /// shown a card whose order nothing can resume. Back to `Idle` on success —
    /// the machine now waits on the admin, not on the CA.
    async fn stash_manual_order(
        &self,
        config: &Arc<dyn DnsStore>,
        in_progress: Dns01OrderInProgress,
        domain: String,
        target_nest_id: [u8; 32],
        challenges: Vec<DnsRecordRow>,
    ) -> Result<(), DnsDispatchError> {
        let account_credentials = in_progress.account_credentials().to_vec();
        self.persist_pending_manual_issue(
            config,
            &domain,
            &target_nest_id,
            &challenges,
            &account_credentials,
        )
        .await?;
        let mut inner = self.inner.lock().expect("snapshot mutex");
        inner.pending_order = Some(PendingManualOrder {
            in_progress,
            domain: domain.clone(),
            target_nest_id,
        });
        inner.snapshot.pending_cert = Some(PendingCertIssue { domain, challenges });
        inner.snapshot.status = DnsStatus::Idle;
        Ok(())
    }

    /// Write the in-flight manual issuance to `DnsConfig.pending_manual_issue`
    /// — the breadcrumb a *fresh* machine reads to re-surface the paste card
    /// (`apply_config`) and to resume completion. The machine is the store's sole
    /// writer (`dns-management.md` § Storage).
    ///
    /// **Also persists `account_credentials` to `DnsConfig.acme_account`, in
    /// the same write.** Without this a resume after the process is gone (an app
    /// restart, a device switch) creates a brand-new ACME account — RFC 8555 §7.4
    /// scopes pending-authorization reuse to the *same* account, so the "the CA
    /// asks for the same challenge, reuse it" fast path [`Self::resume_manual_issue`]
    /// documents would silently never fire without carrying the account across
    /// the gap too, not just the breadcrumb (measured via the pebble real-wire
    /// proof, S4b: a fresh account got a fresh challenge on every resume).
    async fn persist_pending_manual_issue(
        &self,
        config: &Arc<dyn DnsStore>,
        domain: &str,
        target_nest_id: &[u8; 32],
        challenges: &[DnsRecordRow],
        account_credentials: &[u8],
    ) -> Result<(), DnsDispatchError> {
        update_dns(config.as_ref(), |dns| {
            dns.pending_manual_issue = Some(PendingManualIssue {
                domain: domain.to_string(),
                target_nest_id: target_nest_id.to_vec(),
                challenges: challenges
                    .iter()
                    .map(|r| PendingChallengeRecord {
                        name: r.name.clone(),
                        record_type: r.record_type.clone(),
                        value: r.expected.clone(),
                        ttl_seconds: r.ttl_seconds,
                    })
                    .collect(),
                started_at: Timestamp::now(),
            });
            dns.acme_account = Some(account_credentials.to_vec());
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Drop the persisted breadcrumb — the issuance is no longer in flight
    /// (completed, or the admin cancelled). Idempotent, and a no-op write is
    /// skipped so a cancel on an already-clean config costs nothing.
    async fn clear_pending_manual_issue(
        &self,
        config: &Arc<dyn DnsStore>,
    ) -> Result<(), DnsDispatchError> {
        if config.load().await?.pending_manual_issue.is_none() {
            return Ok(());
        }
        update_dns(config.as_ref(), |dns| {
            dns.pending_manual_issue = None;
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Rebuild a completable order for a machine that holds no live one — the
    /// interrupted-issuance path (`tls-certificates.md`
    /// § Surviving an interrupted manual issuance). Reads the persisted breadcrumb, opens a
    /// **fresh** order for the same domain, and accepts it only if the CA asks
    /// for the same `_acme-challenge` value the admin already published (the
    /// usual case — the first attempt's authorization is still pending, so the
    /// challenge is reused). If the value changed, the new one is surfaced and
    /// persisted and the admin is told to update the record: pressing complete
    /// again then resumes normally.
    async fn resume_manual_issue(
        &self,
        config: &Arc<dyn DnsStore>,
    ) -> Result<PendingManualOrder, DnsDispatchError> {
        let dns = config.load().await?;
        let Some(breadcrumb) = dns.pending_manual_issue.clone() else {
            return Err(DnsDispatchError::InvalidState(
                "no manual cert order is awaiting confirmation".to_string(),
            ));
        };
        let target: [u8; 32] = breadcrumb
            .target_nest_id
            .as_slice()
            .try_into()
            .map_err(|_| {
                DnsDispatchError::InvalidState(format!(
                    "the interrupted issuance for {} recorded a {}-byte target nest id; cancel it and begin again",
                    breadcrumb.domain,
                    breadcrumb.target_nest_id.len()
                ))
            })?;

        self.set_status(DnsStatus::Working);
        let (in_progress, challenges) = self.open_manual_order(&dns, &breadcrumb.domain).await?;
        if !challenge_values_match(&breadcrumb.challenges, &challenges) {
            let domain = breadcrumb.domain.clone();
            self.stash_manual_order(config, in_progress, domain.clone(), target, challenges)
                .await?;
            return Err(DnsDispatchError::Issuance(format!(
                "the interrupted certificate request for {domain} could not be resumed with the \
                 record you published — the certificate authority issued a new challenge value. \
                 The page now shows the new TXT record; update it at your registrar and press \
                 complete again."
            )));
        }
        Ok(PendingManualOrder {
            in_progress,
            domain: breadcrumb.domain,
            target_nest_id: target,
        })
    }

    /// **Manual-mode** DNS-01 issuance, phase 2 ([`DnsAction::CompleteManualIssueCert`]):
    /// the admin has pasted the surfaced `_acme-challenge` TXT(s) and the page's
    /// red/green verify shows them live. Take the suspended order, run the
    /// propagation gate, finalize with the CA, seal + deliver the issued cert to
    /// the target nest (Half B, shared with [`Self::issue_cert`]), and persist the
    /// ACME account (D6). The nest stays on the floor until delivery succeeds
    /// (non-fatal — § A/C).
    ///
    /// **A machine holding no live order resumes from the persisted breadcrumb**
    /// ([`Self::resume_manual_issue`]) rather than telling the admin nothing is
    /// pending — the order handle does not survive a page navigation, an app
    /// restart, or a move to another device, and the propagation gate can hold
    /// this call for up to 45 minutes. On failure the paste surface and the
    /// breadcrumb both **stay**, so the admin can simply press complete again
    /// instead of starting over; only success clears them.
    async fn complete_manual_issue(&self) -> Result<(), DnsDispatchError> {
        let config = self.require_config()?.clone();
        let actor_sk = self.require_issuer()?.clone();
        let live = self
            .inner
            .lock()
            .expect("snapshot mutex")
            .pending_order
            .take();
        let pending = match live {
            Some(pending) => pending,
            None => match self.resume_manual_issue(&config).await {
                Ok(pending) => pending,
                Err(e) => {
                    let mut inner = self.inner.lock().expect("snapshot mutex");
                    inner.snapshot.status = DnsStatus::Idle;
                    return Err(e);
                }
            },
        };

        self.set_status(DnsStatus::Working);
        let PendingManualOrder {
            in_progress,
            domain,
            target_nest_id,
        } = pending;

        // Finalize + seal + deliver + persist. Factored into one fallible block so
        // the paste-surface cleanup below runs on every outcome (the order is already
        // consumed by `take()`).
        // Same active propagation gate as the managed path (`issue_cert`): the admin
        // pasted the TXT at their registrar, which says nothing about whether the
        // authoritative NS serves it yet. The gate's zone for a manual domain is the
        // domain itself — the admin pasted into their own apex zone.
        #[cfg(not(target_arch = "wasm32"))]
        let native_probe = AuthoritativeNsProbe::new();
        // `test-helpers` only: the pebble seam's probe override (`None` = skip
        // active polling — a fixed zero wait is correct for an in-process DNS
        // responder that already serves the pasted TXT synchronously) replaces
        // the real system-DNS `AuthoritativeNsProbe`, which cannot see a pebble
        // test's throwaway zone.
        #[cfg(all(not(target_arch = "wasm32"), feature = "test-helpers"))]
        let probe: Option<&dyn Dns01ResolvabilityProbe> = match &self.manual_order_test_seam {
            Some(seam) => seam.probe.as_deref(),
            None => Some(&native_probe),
        };
        #[cfg(all(not(target_arch = "wasm32"), not(feature = "test-helpers")))]
        let probe: Option<&dyn Dns01ResolvabilityProbe> = Some(&native_probe);
        // Wasm: the nest runs the same authoritative-direct query on the browser's
        // behalf. Until 2026-08-22 this arm was `None`, which — since the caller's
        // fixed wait here is `Duration::ZERO` — meant the web complete-button told
        // the CA to validate with **no wait and no check at all**, the very race
        // that killed the first live manual issuance on 2026-07-29.
        #[cfg(target_arch = "wasm32")]
        let relayed_probe = NestRelayedProbe::new(self.nest.as_ref());
        #[cfg(target_arch = "wasm32")]
        let probe: Option<&dyn Dns01ResolvabilityProbe> = Some(&relayed_probe);

        let result = async {
            let issued =
                complete_dns01_order(in_progress, std::time::Duration::ZERO, &domain, probe)
                    .await
                    .map_err(|e| DnsDispatchError::Issuance(e.to_string()))?;
            self.deliver_issued_cert(&domain, &target_nest_id, &actor_sk, &issued)
                .await?;
            // One write retires the breadcrumb and persists the account — the
            // issuance is done, so nothing is left for a fresh machine to resume.
            update_dns(config.as_ref(), |dns| {
                dns.acme_account = Some(issued.account_credentials);
                dns.pending_manual_issue = None;
                Ok(())
            })
            .await?;
            Ok::<(), DnsDispatchError>(())
        }
        .await;

        let mut inner = self.inner.lock().expect("snapshot mutex");
        // Only success retires the paste card. A failed completion leaves both
        // the card and the breadcrumb in place so the admin can retry against the
        // record they already published — the error itself is surfaced through
        // `snapshot.error`, so nothing disappears silently.
        if result.is_ok() {
            inner.snapshot.pending_cert = None;
        }
        inner.snapshot.status = DnsStatus::Idle;
        result
    }

    /// Drop a suspended manual order ([`DnsAction::CancelManualIssueCert`]) and clear
    /// the paste surface — the admin declined to paste / delegate, so the nest stays
    /// on the self-signed floor (graceful — § B tier 3 / § C). Idempotent.
    ///
    /// Also retires the persisted breadcrumb, so the card does not come back on the
    /// next refresh (here or on another of the admin's devices). A failed config
    /// write is **surfaced**, not swallowed: the breadcrumb would otherwise
    /// re-surface the card with no explanation.
    async fn cancel_manual_issue(&self) -> Result<(), DnsDispatchError> {
        {
            let mut inner = self.inner.lock().expect("snapshot mutex");
            inner.pending_order = None;
            inner.snapshot.pending_cert = None;
            inner.snapshot.status = DnsStatus::Idle;
        }
        match &self.config {
            Some(config) => self.clear_pending_manual_issue(config).await,
            // A read/verify-only machine never opened an order, so there is no
            // breadcrumb of its own to retire.
            None => Ok(()),
        }
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl DnsManagementMachine {
    pub fn snapshot(&self) -> DnsSnapshot {
        fauna_core::clone_locked(&self.inner, |i| &i.snapshot)
    }

    /// The deployment "Fauna controls DNS" master-switch state over the current
    /// snapshot — the FFI-facing convenience so native apps don't re-code the
    /// fold on the `DnsSnapshot` Record (which can't carry exported methods).
    /// `active_domains` = the `LocalDomainsSnapshot.active` domain names the client
    /// already holds; empty ⇒ no active domain ⇒ `false`. See
    /// [`DnsSnapshot::all_domains_managed`].
    pub fn all_domains_managed(&self, active_domains: Vec<String>) -> bool {
        self.snapshot().all_domains_managed(Some(&active_domains))
    }

    /// The at-risk ∧ auto-renew-on domains a synced device should re-issue now —
    /// the FFI-facing convenience so a native app's background auto-renew cadence
    /// (`tls-certificates.md` § C.3) doesn't re-code the fold on the `DnsSnapshot`
    /// Record (which can't carry exported methods over UniFFI). linux calls the
    /// snapshot method directly; the native fan-out clients reach it here. See
    /// [`DnsSnapshot::domains_needing_auto_renew`].
    pub fn domains_needing_auto_renew(&self) -> Vec<String> {
        self.snapshot().domains_needing_auto_renew()
    }
}

/// How often a native app's background auto-renew cadence checks for at-risk
/// certs (`tls-certificates.md` § C.3). The renewal lead is ≥30 days and the
/// nest's self-signed floor covers any gap, so a few checks per day is ample —
/// and a long interval is also what keeps a short-lived session (the e2e harness
/// included) from ever triggering a real CA order.
///
/// Shared policy, not a per-app knob: reach it through [`auto_renew_poll_secs`]
/// rather than re-declaring the number, so a change lands on every app at once.
pub const AUTO_RENEW_CADENCE_SECS: u64 = 6 * 60 * 60;

/// The background auto-renew cadence in seconds — shared policy, so every native
/// app sleeps the same 6 h between [`DnsManagementMachine::auto_renew_scan`]
/// passes instead of hard-coding its own interval.
///
/// The first tick is deliberately fired *after* a full interval on every app: a
/// short-lived session (the e2e harness included) then never triggers a real CA
/// order. Web has no cadence at all — a browser SPA runs no background timer, so
/// it issues on page-open instead (`tls-certificates.md` § C.3).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn auto_renew_poll_secs() -> u64 {
    AUTO_RENEW_CADENCE_SECS
}

/// What one [`DnsManagementMachine::auto_renew_issue`] pass did, so the caller can
/// log per-domain failures and decide whether an open `admin-dns` page needs
/// re-rendering (`issued` non-empty ⇒ the served-cert reality changed).
///
/// Best-effort by construction — there is no `Result`, because no caller should
/// stop pumping over one bad tick; the nest stays gracefully on the floor and the
/// next tick retries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AutoRenewPass {
    /// Domains whose `IssueCert` dispatch succeeded this pass.
    pub issued: Vec<String>,
    /// Domains whose `IssueCert` dispatch failed, with the error rendered for the
    /// caller's log. A failure here is never fatal to the pass.
    pub failed: Vec<AutoRenewFailure>,
}

/// One domain's failed auto-issue within an [`AutoRenewPass`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AutoRenewFailure {
    pub domain: String,
    pub error: String,
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl DnsManagementMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DnsDispatchError> {
        self.refresh().await
    }

    /// Phase 1 of one background auto-renew tick (`tls-certificates.md` § C.3):
    /// re-read the record matrix + served-cert health, then ask the shared
    /// decision which domains are at-risk ∧ auto-renew-on. Returns the domains to
    /// hand straight back to [`Self::auto_renew_issue`].
    ///
    /// **An empty result means the caller must do nothing else this tick** — in
    /// particular it must not resolve a `target_nest_id`, which costs an RPC. That
    /// is why the tick is two calls rather than one: `target_nest_id` comes from
    /// linked-nests state (D7), which this machine deliberately does not know
    /// about, and resolving it eagerly every 6 h would be a needless round-trip on
    /// the overwhelmingly common "nothing due" pass.
    ///
    /// Both refreshes are best-effort: a non-admin connection's Admin-gated RPCs
    /// error, which leaves an empty snapshot, which yields no due domains — so a
    /// non-admin device pumps harmlessly rather than erroring. They go through
    /// [`Self::dispatch`] (not the private helpers) so the error-banner
    /// clear-then-set behaviour is bit-identical to the five hand-rolled ticks
    /// this replaces.
    pub async fn auto_renew_scan(&self) -> Vec<String> {
        let _ = self.dispatch(DnsAction::Refresh).await;
        let _ = self.dispatch(DnsAction::RefreshCertStatus).await;
        self.snapshot().domains_needing_auto_renew()
    }

    /// Phase 2 of one background auto-renew tick: auto-issue each domain
    /// [`Self::auto_renew_scan`] returned, with **no admin tap**, then re-read
    /// cert status so the reported health reflects what just happened.
    ///
    /// Each per-domain failure is non-fatal and recorded in the returned
    /// [`AutoRenewPass`] — one domain whose order fails must never stop the rest,
    /// and the nest stays gracefully on the floor until the next tick retries. The
    /// trailing cert-status re-read runs even when every domain failed, so a
    /// caller that renders health never shows a stale row.
    ///
    /// Calling this instead of hand-rolling the loop is what makes the two
    /// mistakes unrepresentable: issuing in a different order than health is
    /// re-read, and aborting the pass on the first bad domain.
    pub async fn auto_renew_issue(
        &self,
        domains: Vec<String>,
        target_nest_id: Vec<u8>,
    ) -> AutoRenewPass {
        let mut pass = AutoRenewPass::default();
        for domain in domains {
            match self
                .dispatch(DnsAction::IssueCert {
                    domain: domain.clone(),
                    target_nest_id: target_nest_id.clone(),
                })
                .await
            {
                Ok(()) => pass.issued.push(domain),
                Err(e) => pass.failed.push(AutoRenewFailure {
                    domain,
                    error: e.to_string(),
                }),
            }
        }
        let _ = self.dispatch(DnsAction::RefreshCertStatus).await;
        pass
    }

    pub async fn dispatch(&self, action: DnsAction) -> Result<(), DnsDispatchError> {
        // Clear any prior error before the new action runs.
        self.inner.lock().expect("snapshot mutex").snapshot.error = None;
        let result = match action {
            DnsAction::Refresh => self.refresh().await,
            DnsAction::VerifyRecords { domain } => self.verify(domain).await,
            DnsAction::PutCredentials {
                provider_id,
                fields,
                label,
            } => self.put_credentials(provider_id, fields, label).await,
            DnsAction::ClearCredentials { index } => self.clear_credentials(index).await,
            DnsAction::SetMode { domain, managed } => self.set_mode(domain, managed).await,
            DnsAction::Publish { domain } => self.publish(domain).await,
            // Cross-target: the order driver is per-target (`acme_order` native /
            // `acme_pure` wasm), but the orchestration is identical, so web issues
            // certs natively (W5 — `tls-certificates.md` § C, "web issues natively").
            DnsAction::IssueCert {
                domain,
                target_nest_id,
            } => self.issue_cert(domain, target_nest_id).await,
            DnsAction::BeginManualIssueCert {
                domain,
                target_nest_id,
            } => self.begin_manual_issue(domain, target_nest_id).await,
            DnsAction::CompleteManualIssueCert => self.complete_manual_issue().await,
            // No CA work — drop any suspended order, clear the paste surface, and
            // retire the persisted breadcrumb so it does not re-surface.
            DnsAction::CancelManualIssueCert => self.cancel_manual_issue().await,
            // Config-only (no ACME order), so both arms run on native and web.
            DnsAction::DelegateRenewal {
                domain,
                target_zone,
            } => self.delegate_renewal(domain, target_zone).await,
            DnsAction::RemoveDelegation { domain } => self.remove_delegation(domain).await,
            // Pure Admin read (both targets) — refresh the cert-status row.
            DnsAction::RefreshCertStatus => self.refresh_cert_status().await,
            // Config-only (both targets) — toggle hands-off renewal opt-out.
            DnsAction::SetAutoRenew { domain, enabled } => {
                self.set_auto_renew(domain, enabled).await
            }
        };
        if let Err(ref e) = result {
            // Producer-side log for the reactive `error-message` banner: fire
            // once here where the state is set, not in the per-tick render
            // (observability.md § Log on the *event*, not the *paint*).
            let message = e.to_string();
            tracing::warn!(target: "fauna_dns", "{message}");
            let mut inner = self.inner.lock().expect("snapshot mutex");
            inner.snapshot.error = Some(message);
            inner.snapshot.status = DnsStatus::Idle;
        }
        result
    }
}

// ── WS-RPC client wrapper ───────────────────────────────────────────

/// Typed WS-RPC client for the Admin-only `fauna.dns.*` kinds, generic over the
/// shared [`RpcRequester`] transport seam (native `NestClient` / wasm
/// `WsRpcClient`) — the DNS analogue of `fauna-client-bridges`'s
/// `MailAdminClient`. Co-located with [`DnsManagementMachine`] because DNS is
/// its own surface (own crate), not a `fauna.bridges.*` kind.
///
/// Per-app glue holds one of these and implements [`DnsNest`] by delegating +
/// mapping `R::Error` to [`DnsNestError`] (mirrors linux's `LinuxLocalDomainNest`
/// over `MailAdminClient`) — so the `nest.request("fauna.dns.…", …)` kind /
/// payload composition is written once here, not duplicated in every app's
/// glue. All kinds are Admin-gated by `bridge_method_allowlist.rs`; a non-admin
/// caller gets a namespaced permission-denied `RpcError`.
pub struct DnsAdminClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> DnsAdminClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.dns.list_records` — the per-domain expected-record matrix.
    /// `domain: None` → every active local domain; `Some(d)` → just that one.
    /// Replay-safe pure read.
    pub async fn list_records(&self, domain: Option<String>) -> Result<ListRecordsReply, R::Error> {
        self.nest
            .request(
                "fauna.dns.list_records",
                ListRecordsRequest {
                    domain,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.dns.verify_records` — per-domain live public-DNS verdicts (observed
    /// value + status, keyed by `(name, record_type)`), resolved nest-side via a
    /// public-recursive resolver. Same `domain` filter as `list_records`.
    /// Replay-safe pure read.
    pub async fn verify_records(
        &self,
        domain: Option<String>,
    ) -> Result<VerifyRecordsReply, R::Error> {
        self.nest
            .request(
                "fauna.dns.verify_records",
                VerifyRecordsRequest {
                    domain,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.dns.probe_txt_visible` — "is this exact TXT value served by every
    /// authoritative NS of `zone_name` right now?", the DNS-01 propagation
    /// gate's readiness question run nest-side. Replay-safe pure read (it
    /// resolves public DNS and mutates nothing).
    ///
    /// The web app's arm of the gate: a browser has no raw DNS, so it asks the
    /// nest, which runs the *same* authoritative-direct query the native probe
    /// runs in-process ([`AuthoritativeNsProbe`]). See [`NestRelayedProbe`].
    pub async fn probe_txt_visible(
        &self,
        zone_name: String,
        record_name: String,
        txt_value: String,
    ) -> Result<ProbeTxtVisibleReply, R::Error> {
        self.nest
            .request(
                "fauna.dns.probe_txt_visible",
                ProbeTxtVisibleRequest {
                    zone_name,
                    record_name,
                    txt_value,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.tls.publish_cert` — store the client-issued, sealed LAN-TLS cert
    /// entry for namespace-sync delivery to the paired private nest. Not
    /// replay-safe in the strict sense, but idempotent last-writer-wins (the
    /// handler upserts under the fixed `LAN_TLS_CERT_ENTRY_ID`), so a retry is
    /// harmless. Admin-gated nest-side.
    pub async fn publish_cert(
        &self,
        req: PublishCertRequest,
    ) -> Result<PublishCertReply, R::Error> {
        self.nest.request("fauna.tls.publish_cert", req).await
    }

    /// `fauna.tls.cert_status` — per-domain served-cert health for the
    /// `admin-dns` cert-status row. Replay-safe pure read.
    pub async fn cert_status(&self, domains: Vec<String>) -> Result<CertStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.tls.cert_status",
                CertStatusRequest {
                    domains,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.dns.set_host_address` — report the deployment's **public** IP so the
    /// nest gates ACME HTTP-01 on the *strong* resolve-check (DNS points *here*)
    /// and assembles the apex/`mail.` `A`/`AAAA` rows. The admin client is the
    /// authority (it holds the DNS credential + reliably knows the public IP; a
    /// NAT'd nest cannot self-detect) — see [`crate::host_address`] for the
    /// classify/report decision and
    /// `docs/goal/architecture/nest/domains-and-tls-bootstrap.md`
    /// § Host-address acquisition. Idempotent last-writer-wins (the handler
    /// upserts the singleton row), so the onboarding call + every reconnect call
    /// are all safe. Admin-gated nest-side (`bridge_method_allowlist.rs`).
    pub async fn set_host_address(
        &self,
        req: SetHostAddressRequest,
    ) -> Result<SetHostAddressReply, R::Error> {
        self.nest.request("fauna.dns.set_host_address", req).await
    }
}

// ── shared DnsNest seam over the WS-RPC transport ───────────────────

/// Map a transport error into the seam's two-class [`DnsNestError`] via the
/// shared [`fauna_protocol::nest_seam_error`] classifier — the generic
/// replacement for each app's hand-rolled `map_admin_err`. Reused by the native
/// and (Slice 2) wasm seam arms, which differ only in `R`, so the
/// classification lives once.
pub fn nest_error<E: RpcErrorClass + core::fmt::Display>(e: E) -> DnsNestError {
    fauna_protocol::nest_seam_error(e)
}

// ── DnsProviderSeam over fauna-provisioning (the publish/verify path) ──
//
// Shared by BOTH transport seams (native + wasm): the provider-API logic is
// target-agnostic — only the `reqwest::Client` construction differs (native gets
// bounded timeouts; wasm uses the fetch-backed client, whose cross-origin calls
// `fauna_provisioning::proxy` routes through the stateless CORS proxy, the same
// path the web onboarding wizard already uses). Lifting it here keeps the two
// `build_dns_management_machine_with_credentials` factories from duplicating the
// provider glue (priority #2/#4). `fauna-provisioning` + `reqwest` are deps on
// both targets (Cargo `[target …]` tables), so this module compiles everywhere.
mod provider_seam {
    use super::*;
    use fauna_provisioning::ProviderId;
    use fauna_provisioning::dispatch::{Credentials, DnsDispatch, dns_provider};
    use fauna_provisioning::dns::DnsProvider as _;
    use fauna_provisioning::dns::DnsRecord as ProviderRecord;
    use fauna_provisioning::error::ProvisionError;

    /// Map a `fauna-provisioning` failure into the seam's two-class
    /// [`DnsProviderError`]: network + 5xx/429/408 are retryable; everything
    /// else (bad token, 4xx, parse) is a terminal rejection.
    fn provider_error(e: ProvisionError) -> DnsProviderError {
        match &e {
            ProvisionError::Http(_) => DnsProviderError::Transient(e.to_string()),
            ProvisionError::Provider { status, .. }
                if *status >= 500 || *status == 429 || *status == 408 =>
            {
                DnsProviderError::Transient(e.to_string())
            }
            _ => DnsProviderError::Rejected(e.to_string()),
        }
    }

    /// The real [`DnsProviderSeam`] — builds a `fauna-provisioning` provider from
    /// `(provider_id, fields)` and drives `verify` / idempotent `create_record`
    /// over a `reqwest` client. Native uses a bounded-timeout client; wasm uses
    /// the fetch-backed client (timeouts are native-only) with cross-origin
    /// provider APIs proxied by `fauna_provisioning::proxy` — so the web
    /// `admin-dns` managed-mode page verifies/publishes from the browser exactly
    /// like the web onboarding wizard, and the nest never sees the credential.
    pub(crate) struct RpcDnsProvider {
        http: reqwest::Client,
        /// Test-only: routes the provider adapters at a wiremock server via
        /// `dns_provider`'s `override_base_url`. Always `None` in production.
        base_url_override: Option<String>,
    }

    /// The owner name the `DnsProvider` adapters expect for the seam's
    /// fully-qualified `fqdn` in `zone`: zone-relative (`@`, `_acme-challenge`)
    /// for every provider except Cloudflare
    /// (`DnsProvider::record_names_relative_to_zone`) — the same caller-side
    /// relativization the provisioning orchestrator applies. Passing the FQDN
    /// through lets Hetzner's RRset API double-suffix the owner
    /// (`_acme-challenge.zone.tld` is stored as
    /// `_acme-challenge.zone.tld.zone.tld.`): the record looks published on the
    /// control plane (API GET by the same name finds it) but is never
    /// resolvable at the name the ACME CA queries.
    fn owner_name(dns: &DnsDispatch, zone: &DnsZoneRef, fqdn: &str) -> String {
        if dns.record_names_relative_to_zone() {
            fauna_provisioning::orchestrator::dns_record_name(fqdn, &zone.name)
        } else {
            fqdn.to_string()
        }
    }

    impl RpcDnsProvider {
        pub(crate) fn new() -> Self {
            // Native: bound the provider HTTP round-trips — a bare
            // `reqwest::Client` has no timeout, so a provider API that accepts
            // the TCP connection but never answers (or a network that silently
            // drops outbound 443) would hang `verify()` / `publish()`
            // indefinitely, stranding the add-credential dispatch with no error
            // to show. A bounded connect + overall timeout turns that into a
            // prompt `DnsProviderError` the page surfaces in `error-message`.
            #[cfg(not(target_arch = "wasm32"))]
            let http = reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|e| {
                    // Say exactly what was lost. The bare fallback below carries
                    // NEITHER ceiling above — the exact hazard the comment on this
                    // constructor names: a provider API that accepts the TCP
                    // connection but never answers now hangs verify()/publish()
                    // indefinitely instead of surfacing a prompt DnsProviderError.
                    tracing::error!(
                        target: "fauna_dns",
                        "DNS provider client build failed ({e}): falling back to a client \
                         with NO connect/request timeout ceilings — verify()/publish() can \
                         now hang indefinitely on an unresponsive provider API",
                    );
                    reqwest::Client::new()
                });
            // Wasm: the fetch-backed client. `connect_timeout`/`timeout` are
            // native-only builder methods (the browser owns the fetch timeout),
            // so the wasm arm uses the bare client — exactly as the onboarding
            // machine does (`fauna-onboarding-machine`).
            #[cfg(target_arch = "wasm32")]
            let http = reqwest::Client::new();
            Self {
                http,
                base_url_override: None,
            }
        }

        /// Test-only: a seam whose provider adapters talk to `base` (a wiremock
        /// server) instead of the real provider APIs.
        #[cfg(all(test, not(target_arch = "wasm32")))]
        pub(crate) fn with_base_url_override(base: String) -> Self {
            let mut seam = Self::new();
            seam.base_url_override = Some(base);
            seam
        }

        fn build(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
        ) -> Result<DnsDispatch, DnsProviderError> {
            let id = ProviderId::from_str(provider_id).ok_or_else(|| {
                DnsProviderError::Rejected(format!("unknown DNS provider `{provider_id}`"))
            })?;
            // The secret stays `SecretString` into `fauna_provisioning::Credentials`;
            // it's un-wrapped to a plain `String` only inside the per-provider
            // dispatch (`Credentials::get(..).to_string()`), at the external
            // reqwest provider-API boundary.
            let creds = Credentials {
                entries: fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            };
            dns_provider(id, creds, self.base_url_override.clone()).ok_or_else(|| {
                DnsProviderError::Rejected(format!(
                    "provider `{provider_id}` has no DNS capability or is missing a required credential field"
                ))
            })
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl DnsProviderSeam for RpcDnsProvider {
        async fn verify(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
        ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
            let dns = self.build(provider_id, fields)?;
            let zones = dns.verify(&self.http).await.map_err(provider_error)?;
            Ok(zones
                .into_iter()
                .map(|z| DnsZoneRef {
                    id: z.id,
                    name: z.name,
                })
                .collect())
        }

        async fn publish(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            let dns = self.build(provider_id, fields)?;
            for r in records {
                let rec = ProviderRecord {
                    record_type: r.record_type.clone(),
                    // Zone-relative for the providers that require it — see
                    // `owner_name`; the seam surface stays fully-qualified.
                    name: owner_name(&dns, zone, &r.name),
                    value: r.value.clone(),
                    ttl: r.ttl_seconds,
                    priority: r.priority,
                };
                // Idempotent: skip if a record with the same value already
                // exists (mirrors the orchestrator's `create_record_idempotent`).
                let existing = dns
                    .find_records(&self.http, &zone.id, &rec.name, &rec.record_type)
                    .await
                    .unwrap_or_default();
                if existing.iter().any(|e| e.value == rec.value) {
                    continue;
                }
                dns.create_record(&self.http, &zone.id, &rec)
                    .await
                    .map_err(provider_error)?;
            }
            Ok(())
        }

        async fn teardown(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            let dns = self.build(provider_id, fields)?;
            for r in records {
                // `delete_record` is itself value-based + idempotent (a missing
                // record is a no-op `Ok`), so no pre-flight `find_records` is
                // needed here — unlike `publish`'s create-skip-if-exists.
                let owner = owner_name(&dns, zone, &r.name);
                dns.delete_record(&self.http, &zone.id, &owner, &r.record_type, &r.value)
                    .await
                    .map_err(provider_error)?;
            }
            Ok(())
        }

        async fn find_records(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            name: &str,
            record_type: &str,
        ) -> Result<Vec<PublishRecord>, DnsProviderError> {
            let dns = self.build(provider_id, fields)?;
            let found = dns
                .find_records(
                    &self.http,
                    &zone.id,
                    &owner_name(&dns, zone, name),
                    record_type,
                )
                .await
                .map_err(provider_error)?;
            Ok(found
                .into_iter()
                .map(|r| PublishRecord {
                    // The adapters echo the queried (relative) owner back;
                    // re-qualify so the seam stays FQDN-in/FQDN-out.
                    name: name.to_string(),
                    record_type: r.record_type,
                    value: r.value,
                    ttl_seconds: r.ttl,
                    priority: r.priority,
                })
                .collect())
        }
    }

    /// The seam must hand the adapters zone-relative owner names for every
    /// relative-name provider (all but Cloudflare) — the orchestrator always
    /// did; the seam passing FQDNs through is the 2026-07-23 example.com DNS-01
    /// failure (Hetzner stored `_acme-challenge.<zone>` doubled as
    /// `_acme-challenge.<zone>.<zone>.`, so the CA never saw the challenge).
    #[cfg(all(test, not(target_arch = "wasm32")))]
    mod owner_name_tests {
        use super::*;
        use wiremock::matchers::{body_partial_json, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn hetzner_fields() -> Vec<(String, SecretString)> {
            vec![("api-token".to_string(), SecretString::from("test-token"))]
        }

        fn zone() -> DnsZoneRef {
            DnsZoneRef {
                id: "1350948".into(),
                name: "example.com".into(),
            }
        }

        fn challenge_record() -> PublishRecord {
            PublishRecord {
                name: "_acme-challenge.example.com".into(),
                record_type: "TXT".into(),
                value: "tok-value".into(),
                ttl_seconds: 120,
                priority: None,
            }
        }

        /// Pure mapping: relative-name providers get `@`/label owners; a name
        /// outside the zone passes through (the orchestrator's fallback);
        /// Cloudflare keeps fully-qualified owners.
        #[test]
        fn owner_name_relativizes_for_hetzner_not_cloudflare() {
            let hetzner = dns_provider(
                ProviderId::Hetzner,
                Credentials {
                    entries: hetzner_fields(),
                },
                None,
            )
            .expect("hetzner dns dispatch");
            let z = zone();
            assert_eq!(owner_name(&hetzner, &z, "example.com"), "@");
            assert_eq!(
                owner_name(&hetzner, &z, "_acme-challenge.example.com"),
                "_acme-challenge"
            );
            assert_eq!(owner_name(&hetzner, &z, "other.test"), "other.test");

            let cloudflare = dns_provider(
                ProviderId::Cloudflare,
                Credentials {
                    entries: hetzner_fields(),
                },
                None,
            )
            .expect("cloudflare dns dispatch");
            assert_eq!(
                owner_name(&cloudflare, &z, "_acme-challenge.example.com"),
                "_acme-challenge.example.com"
            );
        }

        /// End-to-end through the real seam + dispatch + Hetzner adapter: the
        /// wire `name` (existence probe AND rrset create) is the zone-relative
        /// owner. With the pre-fix FQDN pass-through neither mock matches and
        /// publish errors — a hard regression pin.
        #[tokio::test]
        async fn publish_sends_zone_relative_owner_to_hetzner() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/zones/1350948/rrsets"))
                .and(query_param("name", "_acme-challenge"))
                .and(query_param("type", "TXT"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "rrsets": [] })),
                )
                .mount(&mock)
                .await;
            Mock::given(method("POST"))
                .and(path("/zones/1350948/rrsets"))
                .and(body_partial_json(serde_json::json!({
                    "name": "_acme-challenge",
                    "type": "TXT",
                })))
                .respond_with(ResponseTemplate::new(201))
                .expect(1)
                .mount(&mock)
                .await;

            let seam = RpcDnsProvider::with_base_url_override(mock.uri());
            seam.publish("hetzner", &hetzner_fields(), &zone(), &[challenge_record()])
                .await
                .expect("publish should hit the relative-name mocks");
        }

        /// Teardown posts the remove action at the relative owner's path.
        #[tokio::test]
        async fn teardown_sends_zone_relative_owner_to_hetzner() {
            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path(
                    "/zones/1350948/rrsets/_acme-challenge/TXT/actions/remove_records",
                ))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&mock)
                .await;

            let seam = RpcDnsProvider::with_base_url_override(mock.uri());
            seam.teardown("hetzner", &hetzner_fields(), &zone(), &[challenge_record()])
                .await
                .expect("teardown should hit the relative-name mock");
        }

        /// `find_records` queries with the relative owner but returns the
        /// caller's fully-qualified name (FQDN-in/FQDN-out seam surface).
        #[tokio::test]
        async fn find_records_queries_relative_and_returns_fqdn() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/zones/1350948/rrsets"))
                .and(query_param("name", "_acme-challenge"))
                .and(query_param("type", "TXT"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "rrsets": [{
                        "name": "_acme-challenge",
                        "type": "TXT",
                        "ttl": 120,
                        "records": [{ "value": "\"tok-value\"" }],
                    }],
                })))
                .expect(1)
                .mount(&mock)
                .await;

            let seam = RpcDnsProvider::with_base_url_override(mock.uri());
            let found = seam
                .find_records(
                    "hetzner",
                    &hetzner_fields(),
                    &zone(),
                    "_acme-challenge.example.com",
                    "TXT",
                )
                .await
                .expect("find_records");
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].name, "_acme-challenge.example.com");
            assert_eq!(found[0].value, "tok-value");
        }
    }

    // ── e2e fake-provider seam (native: FAUNA_DNS_PROVIDER_FAKE; wasm: a
    //    test-only enable flag — see `enable_dns_provider_fake_for_test`) ──
    //
    // The managed-publish path (`PutCredentials.verify()` → store, then
    // `SetMode` → `Publish`) has no real DNS provider in the e2e env, so its
    // success halves are untestable in tier_3. A tier_2 harness wires the fake
    // decorator below so a real client driver can drive the path end-to-end. See
    // `tests/e2e-unified/tests/test_admin_dns_managed.py` (native) and the web
    // onboarding-launch-glue test (`test_onboarding_localhost.py`).
    //
    // The decorator is **target-agnostic** — only how its wiring is gated
    // differs. Native gates on the `FAUNA_DNS_PROVIDER_FAKE` env var (the linux
    // e2e launch config sets it); wasm has no process env, so it gates on the
    // off-by-default `DNS_FAKE_ENABLED` flag flipped by the test-only
    // `enable_dns_provider_fake_for_test()` hook (the wasm twin of the env gate).
    // Convention 15 rule (a): the sentinel TYPE itself is compile-gated behind
    // `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`, not just
    // its runtime wiring — a plain `--release` build with no test-helpers carries
    // none of it. See the two `build_dns_management_machine_with_credentials`
    // factories and their `maybe_wrap_fake` twins below.

    /// The e2e sentinel prefix. A DNS-provider credential whose field value
    /// begins with `fake-dns-ok:` is recognized by [`SentinelDnsProvider`] —
    /// `verify()` returns the zone names listed after it (comma-separated) and
    /// `publish()` returns `Ok` after logging [`FAKE_DNS_PUBLISH_WITNESS`], with
    /// no network call.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub(crate) const FAKE_DNS_OK_PREFIX: &str = "fake-dns-ok:";

    /// The log line [`SentinelDnsProvider::publish`] emits for every fake-token
    /// publish — what an e2e reads (from the app's own log) to see that a
    /// managed publish actually reached the provider. Mirrored by
    /// `_PUBLISH_WITNESS` in `tests/e2e-unified/tests/test_admin_dns_managed.py`.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub(crate) const FAKE_DNS_PUBLISH_WITNESS: &str = "e2e fake DNS provider published";

    /// If any credential field carries the e2e sentinel, return the zones it
    /// names (comma-separated after [`FAKE_DNS_OK_PREFIX`]); otherwise `None`
    /// (the decorator then delegates to the real provider). Pure — unit-tested
    /// without a network.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub(crate) fn fake_dns_zones_from_token(
        fields: &[(String, SecretString)],
    ) -> Option<Vec<DnsZoneRef>> {
        for (_k, v) in fields {
            if let Some(rest) = v.strip_prefix(FAKE_DNS_OK_PREFIX) {
                return Some(
                    rest.split(',')
                        .map(str::trim)
                        .filter(|z| !z.is_empty())
                        .map(|z| DnsZoneRef {
                            id: format!("fake-zone-{z}"),
                            name: z.to_string(),
                        })
                        .collect(),
                );
            }
        }
        None
    }

    /// e2e-only [`DnsProviderSeam`] decorator over the real [`RpcDnsProvider`]:
    /// credentials carrying the [`FAKE_DNS_OK_PREFIX`] sentinel are served from
    /// the fake (`verify` → the named zones, `publish` → `Ok`, no network); every
    /// other credential delegates to the wrapped real provider — so the
    /// failure-path tier_3 tests (bogus tokens) still exercise the real provider
    /// unchanged. Wired only when the per-target gate is active (native:
    /// `FAUNA_DNS_PROVIDER_FAKE`; wasm: `enable_dns_provider_fake_for_test()`) —
    /// see the two `build_dns_management_machine_with_credentials` factories;
    /// never in production.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub(crate) struct SentinelDnsProvider {
        pub(crate) real: Arc<dyn DnsProviderSeam>,
    }

    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl DnsProviderSeam for SentinelDnsProvider {
        async fn verify(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
        ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
            if let Some(zones) = fake_dns_zones_from_token(fields) {
                return Ok(zones);
            }
            self.real.verify(provider_id, fields).await
        }

        async fn publish(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            if fake_dns_zones_from_token(fields).is_some() {
                // The e2e witness: the fake writes nothing anywhere, so this
                // line — in the app's own log (native `app.err`, the web
                // console ring) — is the only proof a publish reached the
                // provider. `test_admin_dns_managed.py` asserts on it.
                let names = records
                    .iter()
                    .map(|r| format!("{} {}", r.record_type, r.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                tracing::info!(
                    target: "fauna_dns",
                    "{FAKE_DNS_PUBLISH_WITNESS} {} record(s) into zone {}: {names}",
                    records.len(),
                    zone.name
                );
                return Ok(());
            }
            self.real.publish(provider_id, fields, zone, records).await
        }

        async fn teardown(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            if fake_dns_zones_from_token(fields).is_some() {
                return Ok(());
            }
            self.real.teardown(provider_id, fields, zone, records).await
        }

        async fn find_records(
            &self,
            provider_id: &str,
            fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            name: &str,
            record_type: &str,
        ) -> Result<Vec<PublishRecord>, DnsProviderError> {
            // A fake-token credential has no real zone — report nothing published,
            // so the reconcile's TLSA converge pass finds no stale rows to delete
            // (and its create half is the no-op `publish` above). The tier_2
            // managed-publish e2e exercises the un-skipped create path; the
            // withdraw diff is covered by the tier_1 `FakeProvider` unit tests.
            if fake_dns_zones_from_token(fields).is_some() {
                return Ok(vec![]);
            }
            self.real
                .find_records(provider_id, fields, zone, name, record_type)
                .await
        }
    }

    // ── wasm-only fake-provider enable flag (the wasm twin of the native
    //    `FAUNA_DNS_PROVIDER_FAKE` env gate) ──
    //
    // wasm has no process env, so the web managed-publish e2e enables the
    // `SentinelDnsProvider` decorator by flipping this off-by-default flag from
    // the `enable_dns_provider_fake_for_test()` hook (exposed to JS by
    // `fauna-wasm` and called only from the Playwright e2e bridge). A `Cell` is
    // sufficient — wasm-bindgen is single-threaded. Never flipped in production.
    // Convention 15 rule (a): gated behind debug/test-helpers too, not just the
    // wasm32 target, so the flag and its flippers do not ship in production wasm.
    #[cfg(target_arch = "wasm32")]
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    thread_local! {
        static DNS_FAKE_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[cfg(target_arch = "wasm32")]
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub(crate) fn dns_fake_enabled() -> bool {
        DNS_FAKE_ENABLED.with(std::cell::Cell::get)
    }

    #[cfg(target_arch = "wasm32")]
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub(crate) fn set_dns_fake_enabled() {
        DNS_FAKE_ENABLED.with(|c| c.set(true));
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    mod sentinel_tests {
        use super::*;

        #[test]
        fn sentinel_token_yields_named_zones() {
            let fields = vec![(
                "api-token".to_string(),
                SecretString::from("fake-dns-ok:example.com, sub.example.org"),
            )];
            let zones = fake_dns_zones_from_token(&fields).expect("sentinel recognized");
            assert_eq!(
                zones.iter().map(|z| z.name.as_str()).collect::<Vec<_>>(),
                vec!["example.com", "sub.example.org"]
            );
        }

        #[test]
        fn non_sentinel_token_delegates() {
            let fields = vec![(
                "api-token".to_string(),
                SecretString::from("bogus-token-deadbeef"),
            )];
            assert!(fake_dns_zones_from_token(&fields).is_none());
        }
    }
}

// The seam over a concrete transport. A single generic `impl<R> DnsNest` is
// impossible: `DnsNest` is `#[async_trait]` (boxed `+ Send` futures, required by
// the `Arc<dyn DnsNest>` object the machine holds), but `RpcRequester::request`
// is AFIT with per-impl `Send` inference — its future is not provably `Send` in
// a generic context, and RTN can't bound a method with generic type params. So
// the seam binds the concrete transport per target: native `Arc<NestClient>`
// here (shared by `fauna-ffi` + linux via [`build_dns_management_machine`]), the
// wasm `WsRpcClient` arm in Slice 2. The reply→domain projection + error map are
// still written once; only the stored client type is concrete.
#[cfg(not(target_arch = "wasm32"))]
mod native_seam {
    use super::*;
    use fauna_client::NestClient;
    use fauna_core::identity::ActorKeypair;

    struct RpcDnsNest {
        client: DnsAdminClient<Arc<NestClient>>,
    }

    #[async_trait]
    impl DnsNest for RpcDnsNest {
        async fn list_records(
            &self,
            domain: Option<String>,
        ) -> Result<Vec<DomainDns>, DnsNestError> {
            self.client
                .list_records(domain)
                .await
                .map(|r| r.domains)
                .map_err(nest_error)
        }

        async fn verify_records(
            &self,
            domain: Option<String>,
        ) -> Result<Vec<DomainVerifyStatus>, DnsNestError> {
            self.client
                .verify_records(domain)
                .await
                .map(|r| r.domains)
                .map_err(nest_error)
        }

        async fn publish_cert(&self, req: PublishCertRequest) -> Result<bool, DnsNestError> {
            self.client
                .publish_cert(req)
                .await
                .map(|r| r.ok)
                .map_err(nest_error)
        }

        async fn cert_status(&self, domains: Vec<String>) -> Result<CertStatusReply, DnsNestError> {
            self.client.cert_status(domains).await.map_err(nest_error)
        }

        async fn probe_txt_visible(
            &self,
            zone_name: String,
            record_name: String,
            txt_value: String,
        ) -> Result<bool, DnsNestError> {
            // Wired for seam completeness; native issuance never calls it — it
            // runs `AuthoritativeNsProbe` in-process instead, saving the hop.
            self.client
                .probe_txt_visible(zone_name, record_name, txt_value)
                .await
                .map(|r| r.visible)
                .map_err(nest_error)
        }
    }

    /// Build a read/verify-only [`DnsManagementMachine`] over a native WS-RPC
    /// handle for the admin `admin-dns` page — the constructor `fauna-ffi` and
    /// linux call for the Phase-0 page instead of hand-rolling [`DnsNest`] glue
    /// (priority #2/#4). For the managed-mode credential surface, see
    /// [`build_dns_management_machine_with_credentials`].
    pub fn build_dns_management_machine(nest: Arc<NestClient>) -> DnsManagementMachine {
        DnsManagementMachine::new(Arc::new(RpcDnsNest {
            client: DnsAdminClient::new(nest),
        }))
    }

    // The DNS-provider verify/publish seam (`RpcDnsProvider`) + the e2e
    // fake-provider decorator (`SentinelDnsProvider`) are shared with the wasm
    // seam — see the top-level `provider_seam` module.
    use super::provider_seam::RpcDnsProvider;
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    use super::provider_seam::SentinelDnsProvider;

    /// Build a full managed-mode [`DnsManagementMachine`] over a native WS-RPC
    /// handle — wiring **all** seams internally: the DNS record's store
    /// ([`AccountDnsStore`] over the seat's `account` runtime — the
    /// `fauna.state.dns` row) and the `fauna-provisioning` provider seam. The
    /// per-app glue supplies only `(nest, keypair, account)` — every seam impl
    /// is shared (priority #2/#4). `keypair` signs the `IssueCert` seal.
    /// `fauna-ffi` (windows/macos/ios/android), linux and tui all call this for
    /// the managed-mode `admin-dns` page; the read/verify-only Phase-0 page uses
    /// [`build_dns_management_machine`].
    pub fn build_dns_management_machine_with_credentials(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        account: AccountHandleSource,
    ) -> DnsManagementMachine {
        let actor_sk = keypair.signing_key().clone();
        let config: Arc<dyn DnsStore> = Arc::new(AccountDnsStore::new(account));
        // The real provider over `fauna-provisioning` (reqwest). In e2e
        // (`FAUNA_DNS_PROVIDER_FAKE` set) it is wrapped by the sentinel decorator
        // so a tier_2 harness can drive the managed-publish path (which has no
        // real DNS provider in the test env). The decorator is transparent for
        // non-sentinel credentials, so production behavior is unchanged — the var
        // is never set in production (nest config from clients, no env knobs), and
        // in a plain `--release` build with no test-helpers `maybe_wrap_fake`
        // compiles to a pure passthrough — the env var is never even read.
        let real: Arc<dyn DnsProviderSeam> = Arc::new(RpcDnsProvider::new());
        let provider = maybe_wrap_fake(real);
        DnsManagementMachine::with_credentials(
            Arc::new(RpcDnsNest {
                client: DnsAdminClient::new(nest),
            }),
            config,
            provider,
            actor_sk,
        )
    }

    /// Seal an onboarding-captured DNS-provider credential into the admin's
    /// DNS record (`fauna.state.dns`) — the post-claim launch glue, deduped out of the apps'
    /// independently-written identical bodies (`onboarding-provisioning.md`
    /// § 4. DNS configuration, *Capture at onboarding, seal via the launched
    /// app — one store, one writer*).
    ///
    /// The wizard machine has no account-plane write capability and, on the
    /// fresh-provision path, runs before the nest exists at all — so it only
    /// **captures** the verified credential. The launched app seals it once it
    /// is authenticated on the new nest, through the *same*
    /// [`DnsAction::PutCredentials`] path the post-onboarding "Fauna controls
    /// DNS" mode uses: one credential store with one writer (priority #2), no
    /// second write path in onboarding. `PutCredentials` re-runs the
    /// provider `verify()` to (re)derive the covered zones, so what onboarding
    /// captured cannot go stale.
    ///
    /// `fields` is the captured map exactly as the machine holds it
    /// (`CapturedDnsCredential.fields`: a `providers.yaml` field id → its
    /// secret). The map→[`DnsCredentialField`] transform lives here so no app
    /// re-spells it, and `label` is **passed through rather than
    /// reconstructed** — it is the machine-computed `"{provider} ({domain})"`
    /// that tells several held credentials apart on the `admin-dns` page, and
    /// an app substituting the bare provider id silently degrades it.
    ///
    /// Takes the parts rather than `CapturedDnsCredential` itself: that type
    /// belongs to `fauna-onboarding-machine`, which is not in this crate's
    /// dependency tree (nor this crate in its), and one hand-off channel is
    /// not worth an edge between them.
    ///
    /// **Best-effort and log-only by design**, like every sibling hand-off
    /// beside it (`fauna_client_pair::dispatch_mint_default_trust_set`): the
    /// user has completed sign-in, and a seal failure must not paint an error
    /// over that. The credential is re-enterable any time from `admin-dns`,
    /// which is also where a failure surfaces as `DnsSnapshot.error`.
    ///
    /// Safe to call the moment sign-in completes: the store waits for the
    /// seat's account runtime to come up ([`AccountDnsStore`]), so a credential
    /// captured before the runtime exists is still sealed.
    pub async fn dispatch_seal_captured_dns_credential(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        account: AccountHandleSource,
        provider_id: String,
        fields: std::collections::HashMap<String, SecretString>,
        label: String,
    ) {
        let machine = build_dns_management_machine_with_credentials(nest, keypair, account);
        let fields: Vec<DnsCredentialField> = fields
            .into_iter()
            .map(|(id, value)| DnsCredentialField { id, value })
            .collect();
        match machine
            .dispatch(DnsAction::PutCredentials {
                provider_id,
                fields,
                label,
            })
            .await
        {
            Ok(()) => tracing::info!(
                "onboarding DNS glue: the captured provider credential is sealed into the DNS record"
            ),
            Err(e) => tracing::warn!(
                "onboarding DNS glue: sealing the captured provider credential failed ({e}); \
                 it can be re-entered from the admin DNS page"
            ),
        }
    }

    /// Convention 15 rule (a) twin: the debug/test-helpers arm wraps the real
    /// provider with the sentinel decorator when `FAUNA_DNS_PROVIDER_FAKE` is
    /// set; the production arm (no debug_assertions, no test-helpers) is a pure
    /// passthrough that never references [`SentinelDnsProvider`] or reads the
    /// env var at all.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    fn maybe_wrap_fake(real: Arc<dyn DnsProviderSeam>) -> Arc<dyn DnsProviderSeam> {
        if std::env::var_os("FAUNA_DNS_PROVIDER_FAKE").is_some() {
            Arc::new(SentinelDnsProvider { real })
        } else {
            real
        }
    }

    #[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
    fn maybe_wrap_fake(real: Arc<dyn DnsProviderSeam>) -> Arc<dyn DnsProviderSeam> {
        real
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native_seam::{
    build_dns_management_machine, build_dns_management_machine_with_credentials,
    dispatch_seal_captured_dns_credential,
};

// The wasm twin of `native_seam`, over the browser `WsRpcClient` (the gloo-net
// `RpcRequester`, `type Error = WsRpcError`). `WsRpcClient` is `Rc`-based and
// `!Send`, so the seam impl uses `#[async_trait(?Send)]` and the resulting
// `Arc<dyn DnsNest>` / machine are `!Send` (fine — wasm-bindgen is
// single-threaded). The reply→domain projection + `nest_error` map are the same
// as native; only the stored transport differs.
#[cfg(target_arch = "wasm32")]
mod wasm_seam {
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use fauna_rpc_wasm::WsRpcClient;

    // The DNS-provider verify/publish seam is shared with native — see the
    // top-level `provider_seam` module. The wasm arm wires the same
    // `RpcDnsProvider` (its `reqwest` client is the browser fetch client, with
    // cross-origin provider APIs proxied by `fauna_provisioning::proxy`). The
    // `SentinelDnsProvider` fake is also shared; wasm has no process env, so its
    // wiring is gated on the off-by-default `dns_fake_enabled()` flag (the wasm
    // twin of native's `FAUNA_DNS_PROVIDER_FAKE`), flipped only by the test-only
    // `enable_dns_provider_fake_for_test()` hook. The failure/verify paths the
    // three tier_3 web tests assert need only the real provider (a bogus token →
    // a real provider rejection through the proxy); the managed-publish-*success*
    // + onboarding-launch-glue web tests flip the flag to use the sentinel.
    use super::provider_seam::RpcDnsProvider;
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    use super::provider_seam::{SentinelDnsProvider, dns_fake_enabled, set_dns_fake_enabled};

    struct RpcDnsNest {
        client: DnsAdminClient<WsRpcClient>,
    }

    #[async_trait(?Send)]
    impl DnsNest for RpcDnsNest {
        async fn list_records(
            &self,
            domain: Option<String>,
        ) -> Result<Vec<DomainDns>, DnsNestError> {
            self.client
                .list_records(domain)
                .await
                .map(|r| r.domains)
                .map_err(nest_error)
        }

        async fn verify_records(
            &self,
            domain: Option<String>,
        ) -> Result<Vec<DomainVerifyStatus>, DnsNestError> {
            self.client
                .verify_records(domain)
                .await
                .map(|r| r.domains)
                .map_err(nest_error)
        }

        async fn publish_cert(&self, req: PublishCertRequest) -> Result<bool, DnsNestError> {
            // Web issues natively too (the wasm-safe `acme_pure` order core, W1–W6),
            // so the `IssueCert` arm produces a cert to deliver here — the same
            // `fauna.tls.publish_cert` upsert as native.
            self.client
                .publish_cert(req)
                .await
                .map(|r| r.ok)
                .map_err(nest_error)
        }

        async fn cert_status(&self, domains: Vec<String>) -> Result<CertStatusReply, DnsNestError> {
            // A pure read — the web `admin-dns` page renders the same cert-status row
            // (as does issuance, now that the wasm `acme_pure` order core has landed).
            self.client.cert_status(domains).await.map_err(nest_error)
        }

        async fn probe_txt_visible(
            &self,
            zone_name: String,
            record_name: String,
            txt_value: String,
        ) -> Result<bool, DnsNestError> {
            // The web arm of the DNS-01 propagation gate — the browser has no raw
            // DNS, so the nest runs the authoritative-direct query the native
            // probe runs in-process. Driven by `NestRelayedProbe`.
            self.client
                .probe_txt_visible(zone_name, record_name, txt_value)
                .await
                .map(|r| r.visible)
                .map_err(nest_error)
        }
    }

    /// Build a read/verify-only [`DnsManagementMachine`] over the browser WS-RPC
    /// handle for the web `admin-dns` page — the wasm twin of native's
    /// `build_dns_management_machine`. For the managed-mode credential surface,
    /// see [`build_dns_management_machine_with_credentials`]. `libs/fauna-wasm`'s
    /// `WasmDnsManagementMachine` calls this.
    pub fn build_dns_management_machine(nest: WsRpcClient) -> DnsManagementMachine {
        DnsManagementMachine::new(Arc::new(RpcDnsNest {
            client: DnsAdminClient::new(nest),
        }))
    }

    /// Build a full managed-mode [`DnsManagementMachine`] over the browser WS-RPC
    /// handle — the wasm twin of native's
    /// `build_dns_management_machine_with_credentials`, wiring **all** seams from
    /// `(nest, keypair, account)`: the DNS record's store ([`AccountDnsStore`]
    /// over the tab's account runtime) + the shared `fauna-provisioning`
    /// provider seam (browser fetch + CORS proxy). `libs/fauna-wasm`'s
    /// `WasmDnsManagementMachine` credentialed constructor calls this for the
    /// web `admin-dns` managed page and for onboarding's seal-at-capture.
    pub fn build_dns_management_machine_with_credentials(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        account: AccountHandleSource,
    ) -> DnsManagementMachine {
        // Threaded for the issuance seal (the wasm `acme_pure` order core issues on
        // web too, so `IssueCert` reaches `deliver_issued_cert` here).
        let actor_sk = keypair.signing_key().clone();
        let config: Arc<dyn DnsStore> = Arc::new(AccountDnsStore::new(account));
        // The real provider over `fauna-provisioning` (browser fetch + CORS
        // proxy). When the test-only `dns_fake_enabled()` flag is set it is
        // wrapped by the `SentinelDnsProvider` decorator so a tier_2 e2e can
        // drive the managed-publish / onboarding-launch-glue success path (no
        // real registrar reachable from the sandbox). Transparent for
        // non-sentinel credentials, so production behavior is unchanged — the
        // flag is never flipped in production (the hook is e2e-only), and in a
        // plain `--release` build with no test-helpers `maybe_wrap_fake` compiles
        // to a pure passthrough — `SentinelDnsProvider` is not even in the binary.
        let real: Arc<dyn DnsProviderSeam> = Arc::new(RpcDnsProvider::new());
        let provider = maybe_wrap_fake(real);
        DnsManagementMachine::with_credentials(
            Arc::new(RpcDnsNest {
                client: DnsAdminClient::new(nest),
            }),
            config,
            provider,
            actor_sk,
        )
    }

    /// Convention 15 rule (a) twin: the debug/test-helpers arm wraps the real
    /// provider with the sentinel decorator when the test-only flag is set; the
    /// production arm (no debug_assertions, no test-helpers) is a pure
    /// passthrough that never references [`SentinelDnsProvider`] at all.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    fn maybe_wrap_fake(real: Arc<dyn DnsProviderSeam>) -> Arc<dyn DnsProviderSeam> {
        if dns_fake_enabled() {
            Arc::new(SentinelDnsProvider { real })
        } else {
            real
        }
    }

    #[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
    fn maybe_wrap_fake(real: Arc<dyn DnsProviderSeam>) -> Arc<dyn DnsProviderSeam> {
        real
    }

    /// Test-only: enable the wasm fake DNS provider for the rest of this page's
    /// lifetime — the wasm twin of native's `FAUNA_DNS_PROVIDER_FAKE`. After this
    /// is called, a [`DnsManagementMachine`] built by
    /// [`build_dns_management_machine_with_credentials`] recognizes the
    /// `fake-dns-ok:<zone>` sentinel token (verify → those zones, publish → ok,
    /// no network). Exposed to JS by `fauna-wasm`'s `enableDnsFakeProviderForTest`
    /// and called only from the Playwright e2e bridge; never in production.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn enable_dns_provider_fake_for_test() {
        set_dns_fake_enabled();
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_seam::{build_dns_management_machine, build_dns_management_machine_with_credentials};

#[cfg(target_arch = "wasm32")]
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use wasm_seam::enable_dns_provider_fake_for_test;

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use fauna_protocol::dns::DnsRecordStatus;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn zone_publishable_excludes_advisory_and_cert_coupled_rows() {
        // Zone-published in managed mode. `SRV` is listed explicitly because the
        // DAV autodiscovery rows (`_caldavs._tcp` / `_carddavs._tcp`) are the one
        // type whose managed-publish status is easy to doubt: the *onboarding*
        // path deliberately drops SRV (`fauna-provisioning`'s `to_provider_record`
        // → `T::Srv => return None`, deferring to `admin-dns`), so a reader who
        // finds that arm first can conclude SRV is never auto-published at all.
        // It is — here, by the create-only loop (2026-07-24: a missing
        // `_carddavs._tcp` on a live zone was investigated as a suspected
        // reconcile gap and turned out to be a historical publish artifact, not
        // a code gap; this row is what makes that answerable from a test).
        for t in ["MX", "TXT", "A", "AAAA", "CNAME", "SRV"] {
            assert!(is_zone_publishable(t), "{t} is zone-published");
        }
        // Excluded from the generic create-only loop.
        assert!(!is_zone_publishable("PTR"), "PTR is set at the IP owner");
        assert!(
            !is_zone_publishable("TLSA"),
            "floor-MX DANE TLSA is handled by the dedicated cert-coupled converge \
             pass (create + withdraw-on-trusted), not the generic create loop"
        );
    }

    #[derive(Default)]
    struct FakeNest {
        domains: StdMutex<Vec<DomainDns>>,
        verdicts: StdMutex<Vec<DomainVerifyStatus>>,
        /// Records each `publish_cert` request so the S5 delivery test can assert
        /// the sealed `(ciphertext, actor_sig)` reached the nest seam.
        published_certs: StdMutex<Vec<PublishCertRequest>>,
        /// Seeded served-cert statuses the `cert_status` seam reports (keyed by
        /// domain); the `RefreshCertStatus` test asserts they reach the snapshot.
        cert_statuses: StdMutex<Vec<DomainCertStatus>>,
        /// Seeded `CertStatusReply::desired_sans` — the listener SAN set the nest
        /// reports and `order_san_set` filters. Empty (the default) keeps
        /// existing tests on the single-name order.
        desired_sans: StdMutex<Vec<String>>,
        /// Every `cert_status` call's requested domains, in order — lets a test
        /// observe that `issue_cert` reads the SAN set without letting the order
        /// reach a CA.
        cert_status_calls: StdMutex<Vec<Vec<String>>>,
        /// Make `cert_status` fail. Used to halt `issue_cert` immediately after the
        /// read, which is the only no-network observation point downstream of it.
        cert_status_fails: StdMutex<bool>,
        /// Seeded `probe_txt_visible` answers, consumed one per call (the tail
        /// value repeats once exhausted). Empty → `false`, the "not visible yet"
        /// default. Lets a test drive `NestRelayedProbe` through a
        /// not-yet-then-yes sequence without any DNS.
        probe_answers: StdMutex<Vec<bool>>,
        /// Make `probe_txt_visible` fail — models an unreachable nest or a
        /// transient error, which must read as "not
        /// visible yet" rather than erroring the order.
        probe_fails: StdMutex<bool>,
        /// Every `probe_txt_visible` call's `(zone, record, value)`, in order.
        probe_calls: StdMutex<Vec<(String, String, String)>>,
    }

    #[async_trait]
    impl DnsNest for FakeNest {
        async fn list_records(
            &self,
            _domain: Option<String>,
        ) -> Result<Vec<DomainDns>, DnsNestError> {
            Ok(self.domains.lock().unwrap().clone())
        }
        async fn verify_records(
            &self,
            domain: Option<String>,
        ) -> Result<Vec<DomainVerifyStatus>, DnsNestError> {
            let all = self.verdicts.lock().unwrap().clone();
            Ok(match domain {
                None => all,
                Some(d) => all.into_iter().filter(|v| v.domain == d).collect(),
            })
        }
        async fn publish_cert(&self, req: PublishCertRequest) -> Result<bool, DnsNestError> {
            self.published_certs.lock().unwrap().push(req);
            Ok(true)
        }
        async fn probe_txt_visible(
            &self,
            zone_name: String,
            record_name: String,
            txt_value: String,
        ) -> Result<bool, DnsNestError> {
            self.probe_calls
                .lock()
                .unwrap()
                .push((zone_name, record_name, txt_value));
            if *self.probe_fails.lock().unwrap() {
                return Err(DnsNestError::Transient("no such kind".into()));
            }
            let mut answers = self.probe_answers.lock().unwrap();
            Ok(match answers.len() {
                0 => false,
                1 => answers[0],
                _ => answers.remove(0),
            })
        }
        async fn cert_status(&self, domains: Vec<String>) -> Result<CertStatusReply, DnsNestError> {
            self.cert_status_calls.lock().unwrap().push(domains.clone());
            if *self.cert_status_fails.lock().unwrap() {
                return Err(DnsNestError::Transient("cert_status unavailable".into()));
            }
            // Report the seeded status for each requested domain (request order),
            // mirroring the real nest's per-requested-domain reply.
            let seeded = self.cert_statuses.lock().unwrap().clone();
            Ok(CertStatusReply {
                desired_sans: self.desired_sans.lock().unwrap().clone(),
                extra: Default::default(),
                statuses: domains
                    .into_iter()
                    .filter_map(|d| seeded.iter().find(|s| s.domain == d).cloned())
                    .collect(),
            })
        }
    }

    fn record(name: &str, rtype: &str, expected: &str) -> DnsRecordView {
        DnsRecordView {
            extra: Default::default(),
            name: name.into(),
            record_type: rtype.into(),
            expected: expected.into(),
            ttl_seconds: 3600,
        }
    }

    fn domain(name: &str) -> DomainDns {
        DomainDns {
            extra: Default::default(),
            domain: name.into(),
            mode: "manual".into(),
            // The sole domain in single-domain tests is the primary (it MXes to
            // its own `mail.<name>` host) — so the managed TLSA reconcile pass
            // treats it as owning the `_25._tcp.mail.<name>` slot.
            is_primary: true,
            records: vec![record(
                &format!("{name}."),
                "MX",
                &format!("10 mail.{name}."),
            )],
        }
    }

    fn status(name: &str, rtype: &str, observed: &[&str], st: &str) -> DnsRecordStatus {
        DnsRecordStatus {
            extra: Default::default(),
            name: name.into(),
            record_type: rtype.into(),
            observed: observed.iter().map(|s| s.to_string()).collect(),
            status: st.into(),
        }
    }

    fn machine_with(domains: Vec<DomainDns>) -> DnsManagementMachine {
        DnsManagementMachine::new(Arc::new(FakeNest {
            domains: StdMutex::new(domains),
            ..Default::default()
        }))
    }

    fn machine_with_verdicts(
        domains: Vec<DomainDns>,
        verdicts: Vec<DomainVerifyStatus>,
    ) -> DnsManagementMachine {
        DnsManagementMachine::new(Arc::new(FakeNest {
            domains: StdMutex::new(domains),
            verdicts: StdMutex::new(verdicts),
            ..Default::default()
        }))
    }

    // ── Refresh slice ───────────────────────────────────────────

    #[tokio::test]
    async fn hydrate_projects_record_matrix() {
        let m = machine_with(vec![domain("example.com"), domain("two.example")]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.domains.len(), 2);
        assert_eq!(snap.domains[0].domain, "example.com");
        assert_eq!(snap.domains[0].mode, "manual");
        assert_eq!(snap.domains[0].records[0].record_type, "MX");
        assert_eq!(snap.domains[0].records[0].expected, "10 mail.example.com.");
        // No verdict until verify runs.
        assert!(snap.domains[0].records[0].verdict.is_none());
        assert_eq!(snap.status, DnsStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn refresh_empty_on_fresh_nest() {
        let m = machine_with(vec![]);
        m.dispatch(DnsAction::Refresh).await.unwrap();
        assert!(m.snapshot().domains.is_empty());
        assert_eq!(m.snapshot().status, DnsStatus::Idle);
    }

    #[tokio::test]
    async fn refresh_surfaces_nest_error() {
        struct FailNest;
        #[async_trait]
        impl DnsNest for FailNest {
            async fn list_records(
                &self,
                _domain: Option<String>,
            ) -> Result<Vec<DomainDns>, DnsNestError> {
                Err(DnsNestError::Transient("ws dropped".into()))
            }
            async fn verify_records(
                &self,
                _domain: Option<String>,
            ) -> Result<Vec<DomainVerifyStatus>, DnsNestError> {
                unreachable!("not called in this test")
            }
            async fn publish_cert(&self, _req: PublishCertRequest) -> Result<bool, DnsNestError> {
                unreachable!("not called in this test")
            }
            async fn cert_status(
                &self,
                _domains: Vec<String>,
            ) -> Result<CertStatusReply, DnsNestError> {
                unreachable!("not called in this test")
            }
            async fn probe_txt_visible(
                &self,
                _zone_name: String,
                _record_name: String,
                _txt_value: String,
            ) -> Result<bool, DnsNestError> {
                unreachable!("not called in this test")
            }
        }
        let m = DnsManagementMachine::new(Arc::new(FailNest));
        let err = m.dispatch(DnsAction::Refresh).await.unwrap_err();
        assert!(matches!(err, DnsDispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(
            snap.error.as_deref().unwrap().contains("ws dropped"),
            "error: {:?}",
            snap.error
        );
        assert_eq!(snap.status, DnsStatus::Idle);
    }

    // ── VerifyRecords slice ─────────────────────────────────────

    #[tokio::test]
    async fn verify_overlays_status_onto_matrix() {
        let verdicts = vec![DomainVerifyStatus {
            extra: Default::default(),
            domain: "example.com".into(),
            records: vec![status(
                "example.com.",
                "MX",
                &["10 mail.example.com."],
                "ok",
            )],
        }];
        let m = machine_with_verdicts(vec![domain("example.com")], verdicts);
        m.hydrate().await.unwrap();
        assert!(m.snapshot().domains[0].records[0].verdict.is_none());

        m.dispatch(DnsAction::VerifyRecords { domain: None })
            .await
            .unwrap();
        let snap = m.snapshot();
        let v = snap.domains[0].records[0].verdict.as_ref().unwrap();
        assert_eq!(v.status, VerifyStatus::Ok);
        assert_eq!(v.observed, vec!["10 mail.example.com.".to_string()]);
        assert_eq!(snap.status, DnsStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn verify_merges_by_name_and_record_type() {
        // A domain's MX and SPF TXT share the bare-domain `name` — they must be
        // disambiguated by `record_type` (the documented merge key).
        let mut d = domain("example.com");
        d.records
            .push(record("example.com.", "TXT", "\"v=spf1 mx -all\""));
        let verdicts = vec![DomainVerifyStatus {
            extra: Default::default(),
            domain: "example.com".into(),
            records: vec![
                status("example.com.", "MX", &["10 mail.example.com."], "ok"),
                status("example.com.", "TXT", &[], "missing"),
            ],
        }];
        let m = machine_with_verdicts(vec![d], verdicts);
        m.hydrate().await.unwrap();
        m.dispatch(DnsAction::VerifyRecords { domain: None })
            .await
            .unwrap();

        let snap = m.snapshot();
        let rows = &snap.domains[0].records;
        let mx = rows.iter().find(|r| r.record_type == "MX").unwrap();
        let txt = rows.iter().find(|r| r.record_type == "TXT").unwrap();
        assert_eq!(mx.verdict.as_ref().unwrap().status, VerifyStatus::Ok);
        assert_eq!(txt.verdict.as_ref().unwrap().status, VerifyStatus::Missing);
        // `missing` carries no observed value.
        assert!(txt.verdict.as_ref().unwrap().observed.is_empty());
    }

    #[tokio::test]
    async fn verify_scoped_to_one_domain_leaves_others_untouched() {
        let verdicts = vec![DomainVerifyStatus {
            extra: Default::default(),
            domain: "a.example".into(),
            records: vec![status("a.example.", "MX", &["10 mail.a.example."], "ok")],
        }];
        let m = machine_with_verdicts(vec![domain("a.example"), domain("b.example")], verdicts);
        m.hydrate().await.unwrap();
        m.dispatch(DnsAction::VerifyRecords {
            domain: Some("a.example".into()),
        })
        .await
        .unwrap();

        let snap = m.snapshot();
        let a = snap
            .domains
            .iter()
            .find(|d| d.domain == "a.example")
            .unwrap();
        let b = snap
            .domains
            .iter()
            .find(|d| d.domain == "b.example")
            .unwrap();
        assert_eq!(
            a.records[0].verdict.as_ref().unwrap().status,
            VerifyStatus::Ok
        );
        assert!(b.records[0].verdict.is_none());
    }

    #[tokio::test]
    async fn refresh_resets_prior_verdicts() {
        let verdicts = vec![DomainVerifyStatus {
            extra: Default::default(),
            domain: "example.com".into(),
            records: vec![status(
                "example.com.",
                "MX",
                &["10 mail.example.com."],
                "ok",
            )],
        }];
        let m = machine_with_verdicts(vec![domain("example.com")], verdicts);
        m.hydrate().await.unwrap();
        m.dispatch(DnsAction::VerifyRecords { domain: None })
            .await
            .unwrap();
        assert!(m.snapshot().domains[0].records[0].verdict.is_some());

        // A fresh matrix fetch invalidates the stale verdict.
        m.dispatch(DnsAction::Refresh).await.unwrap();
        assert!(m.snapshot().domains[0].records[0].verdict.is_none());
    }

    #[tokio::test]
    async fn verify_unknown_status_degrades_to_checking() {
        let verdicts = vec![DomainVerifyStatus {
            extra: Default::default(),
            domain: "example.com".into(),
            records: vec![status("example.com.", "MX", &[], "wire-drift")],
        }];
        let m = machine_with_verdicts(vec![domain("example.com")], verdicts);
        m.hydrate().await.unwrap();
        m.dispatch(DnsAction::VerifyRecords { domain: None })
            .await
            .unwrap();
        assert_eq!(
            m.snapshot().domains[0].records[0]
                .verdict
                .as_ref()
                .unwrap()
                .status,
            VerifyStatus::Checking
        );
    }

    #[tokio::test]
    async fn verify_surfaces_nest_error() {
        struct FailVerify {
            domains: Vec<DomainDns>,
        }
        #[async_trait]
        impl DnsNest for FailVerify {
            async fn list_records(
                &self,
                _domain: Option<String>,
            ) -> Result<Vec<DomainDns>, DnsNestError> {
                Ok(self.domains.clone())
            }
            async fn verify_records(
                &self,
                _domain: Option<String>,
            ) -> Result<Vec<DomainVerifyStatus>, DnsNestError> {
                Err(DnsNestError::Transient("ws dropped".into()))
            }
            async fn publish_cert(&self, _req: PublishCertRequest) -> Result<bool, DnsNestError> {
                unreachable!("not called in this test")
            }
            async fn cert_status(
                &self,
                _domains: Vec<String>,
            ) -> Result<CertStatusReply, DnsNestError> {
                unreachable!("not called in this test")
            }
            async fn probe_txt_visible(
                &self,
                _zone_name: String,
                _record_name: String,
                _txt_value: String,
            ) -> Result<bool, DnsNestError> {
                unreachable!("not called in this test")
            }
        }
        let m = DnsManagementMachine::new(Arc::new(FailVerify {
            domains: vec![domain("example.com")],
        }));
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(DnsAction::VerifyRecords { domain: None })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(snap.error.as_deref().unwrap().contains("ws dropped"));
        assert_eq!(snap.status, DnsStatus::Idle);
        // The matrix survives a failed verify (verdicts just stay None).
        assert!(snap.domains[0].records[0].verdict.is_none());
    }

    // ── credential / managed-mode slice (DnsStore + DnsProviderSeam) ──

    fn zone(id: &str, name: &str) -> DnsZoneRef {
        DnsZoneRef {
            id: id.into(),
            name: name.into(),
        }
    }

    fn dns_field(id: &str, value: &str) -> DnsCredentialField {
        DnsCredentialField {
            id: id.into(),
            value: value.into(),
        }
    }

    /// The in-memory DNS record double ([`FakeDnsStore`]).
    type FakeStore = FakeDnsStore;

    /// Fake DNS provider: `verify` returns the configured zones (or a rejection
    /// if `fail_verify`); `publish`/`teardown` record `(zone-name, records)`
    /// into `published`/`torn_down` for assertion; `find_records` answers from
    /// `find_seed` (filtered by name + type), so the managed reconcile's TLSA
    /// withdraw diff can be driven against a simulated "already-published" zone.
    struct FakeProvider {
        zones: Vec<DnsZoneRef>,
        published: StdMutex<Vec<(String, Vec<PublishRecord>)>>,
        torn_down: StdMutex<Vec<(String, Vec<PublishRecord>)>>,
        find_seed: StdMutex<Vec<PublishRecord>>,
        fail_verify: bool,
        /// When set, `find_records` returns a rejection — models a provider that
        /// can't represent TLSA (Namecheap) so the reconcile's best-effort
        /// (non-fatal, logged) catch can be asserted.
        fail_find: StdMutex<bool>,
        /// When set, `teardown` returns a rejection — models a withdraw that
        /// does not land, so the converge passes' "keep the name remembered and
        /// retry on the next publish" contract can be asserted.
        fail_teardown: StdMutex<bool>,
        /// When set, `publish` returns a rejection (recording nothing) — models
        /// a provider write that does not land, so `SetMode`'s "the opt-in
        /// stays committed, the publish error is surfaced" contract can be
        /// asserted.
        fail_publish: StdMutex<bool>,
    }
    impl FakeProvider {
        fn new(zones: Vec<DnsZoneRef>) -> Self {
            Self {
                zones,
                published: StdMutex::new(vec![]),
                torn_down: StdMutex::new(vec![]),
                find_seed: StdMutex::new(vec![]),
                fail_verify: false,
                fail_find: StdMutex::new(false),
                fail_teardown: StdMutex::new(false),
                fail_publish: StdMutex::new(false),
            }
        }
        /// Seed the records `find_records` reports as already published — used to
        /// simulate a stale floor-MX TLSA the reconcile must withdraw.
        fn seed_published(&self, records: Vec<PublishRecord>) {
            *self.find_seed.lock().unwrap() = records;
        }
        /// Make `find_records` reject, modelling a TLSA-incapable provider.
        fn set_fail_find(&self, fail: bool) {
            *self.fail_find.lock().unwrap() = fail;
        }
        /// Make `teardown` reject, modelling a withdraw that does not land.
        fn set_fail_teardown(&self, fail: bool) {
            *self.fail_teardown.lock().unwrap() = fail;
        }
        /// Make `publish` reject, modelling a provider write that does not land.
        fn set_fail_publish(&self, fail: bool) {
            *self.fail_publish.lock().unwrap() = fail;
        }
    }
    #[async_trait]
    impl DnsProviderSeam for FakeProvider {
        async fn verify(
            &self,
            _provider_id: &str,
            _fields: &[(String, SecretString)],
        ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
            if self.fail_verify {
                return Err(DnsProviderError::Rejected("bad token".into()));
            }
            Ok(self.zones.clone())
        }
        async fn publish(
            &self,
            _provider_id: &str,
            _fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            if *self.fail_publish.lock().unwrap() {
                return Err(DnsProviderError::Rejected("publish refused".into()));
            }
            self.published
                .lock()
                .unwrap()
                .push((zone.name.clone(), records.to_vec()));
            Ok(())
        }
        async fn teardown(
            &self,
            _provider_id: &str,
            _fields: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            self.torn_down
                .lock()
                .unwrap()
                .push((zone.name.clone(), records.to_vec()));
            if *self.fail_teardown.lock().unwrap() {
                return Err(DnsProviderError::Rejected("teardown refused".into()));
            }
            Ok(())
        }
        async fn find_records(
            &self,
            _provider_id: &str,
            _fields: &[(String, SecretString)],
            _zone: &DnsZoneRef,
            name: &str,
            record_type: &str,
        ) -> Result<Vec<PublishRecord>, DnsProviderError> {
            if *self.fail_find.lock().unwrap() {
                return Err(DnsProviderError::Rejected(format!(
                    "{record_type} not supported by this provider"
                )));
            }
            Ok(self
                .find_seed
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.name == name && r.record_type == record_type)
                .cloned()
                .collect())
        }
    }

    fn acme_challenge_record() -> PublishRecord {
        PublishRecord {
            name: "_acme-challenge.example.com".to_string(),
            record_type: "TXT".to_string(),
            value: "token-abc".to_string(),
            ttl_seconds: 120,
            priority: None,
        }
    }

    /// The seam's `teardown` reaches the provider with the records to retract —
    /// the inverse of `publish`. (Provider-level value-based delete logic is
    /// pinned by the wiremock conformance suite; this fixes the seam contract.)
    #[tokio::test]
    async fn seam_teardown_reaches_provider() {
        let provider = FakeProvider::new(vec![]);
        let zone = DnsZoneRef {
            id: "z1".to_string(),
            name: "example.com".to_string(),
        };
        let rec = acme_challenge_record();
        provider
            .teardown("cloudflare", &[], &zone, std::slice::from_ref(&rec))
            .await
            .expect("teardown ok");
        let torn = provider.torn_down.lock().unwrap();
        assert_eq!(torn.len(), 1);
        assert_eq!(torn[0].0, "example.com");
        assert_eq!(torn[0].1, vec![rec]);
    }

    /// The e2e sentinel decorator short-circuits `teardown` for a fake-token
    /// credential without ever touching the wrapped real provider (mirrors its
    /// `publish` short-circuit) — so a tier_2 `_acme-challenge` teardown makes
    /// no network call.
    #[tokio::test]
    async fn sentinel_teardown_short_circuits_fake_token() {
        use super::provider_seam::SentinelDnsProvider;

        // A `real` that panics if reached — the fake-token path must not delegate.
        struct PanicProvider;
        #[async_trait]
        impl DnsProviderSeam for PanicProvider {
            async fn verify(
                &self,
                _: &str,
                _: &[(String, SecretString)],
            ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
                panic!("sentinel delegated to real provider on a fake token");
            }
            async fn publish(
                &self,
                _: &str,
                _: &[(String, SecretString)],
                _: &DnsZoneRef,
                _: &[PublishRecord],
            ) -> Result<(), DnsProviderError> {
                panic!("sentinel delegated to real provider on a fake token");
            }
            async fn teardown(
                &self,
                _: &str,
                _: &[(String, SecretString)],
                _: &DnsZoneRef,
                _: &[PublishRecord],
            ) -> Result<(), DnsProviderError> {
                panic!("sentinel delegated to real provider on a fake token");
            }
            async fn find_records(
                &self,
                _: &str,
                _: &[(String, SecretString)],
                _: &DnsZoneRef,
                _: &str,
                _: &str,
            ) -> Result<Vec<PublishRecord>, DnsProviderError> {
                panic!("sentinel delegated to real provider on a fake token");
            }
        }

        let sentinel = SentinelDnsProvider {
            real: Arc::new(PanicProvider),
        };
        let fields = vec![(
            "api-token".to_string(),
            SecretString::from("fake-dns-ok:example.com"),
        )];
        let zone = DnsZoneRef {
            id: "fake-zone-example.com".to_string(),
            name: "example.com".to_string(),
        };
        sentinel
            .teardown(
                "cloudflare",
                &fields,
                &zone,
                std::slice::from_ref(&acme_challenge_record()),
            )
            .await
            .expect("fake-token teardown short-circuits");
    }

    /// The `_acme-challenge` publish record reuses the shared `fauna-mail`
    /// builder for name + short TTL, but carries the **raw** (unquoted) value
    /// the provider API wants — not the quoted zone-file body.
    #[test]
    fn acme_challenge_record_uses_raw_value_and_short_ttl() {
        let r = acme_challenge_publish_record("example.com", "keyauth-xyz");
        assert_eq!(r.name, "_acme-challenge.example.com");
        assert_eq!(r.record_type, "TXT");
        assert_eq!(r.value, "keyauth-xyz");
        assert_eq!(r.ttl_seconds, 120);
        assert_eq!(r.priority, None);
    }

    /// `publish_acme_challenge` / `teardown_acme_challenge` route the challenge
    /// record through the seam's `publish`/`teardown` — the same path as every
    /// managed record, the inverse of each other.
    #[tokio::test]
    async fn publish_and_teardown_acme_challenge_route_through_seam() {
        let provider = FakeProvider::new(vec![]);
        let zone = DnsZoneRef {
            id: "z1".to_string(),
            name: "example.com".to_string(),
        };

        publish_acme_challenge(
            &provider,
            "cloudflare",
            &[],
            &zone,
            "_acme-challenge.example.com",
            "keyauth-xyz",
        )
        .await
        .expect("publish ok");
        teardown_acme_challenge(
            &provider,
            "cloudflare",
            &[],
            &zone,
            "_acme-challenge.example.com",
            "keyauth-xyz",
        )
        .await
        .expect("teardown ok");

        let expected = PublishRecord {
            name: "_acme-challenge.example.com".to_string(),
            record_type: "TXT".to_string(),
            value: "keyauth-xyz".to_string(),
            ttl_seconds: 120,
            priority: None,
        };
        assert_eq!(
            *provider.published.lock().unwrap(),
            vec![("example.com".to_string(), vec![expected.clone()])]
        );
        assert_eq!(
            *provider.torn_down.lock().unwrap(),
            vec![("example.com".to_string(), vec![expected])]
        );
    }

    /// A fixed actor signing key for the credentialed test machines (the seal
    /// key for `IssueCert`; arbitrary for the non-issuance credential tests).
    fn test_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn full_machine(
        domains: Vec<DomainDns>,
        zones: Vec<DnsZoneRef>,
    ) -> (DnsManagementMachine, Arc<FakeStore>, Arc<FakeProvider>) {
        let store = Arc::new(FakeStore::default());
        let provider = Arc::new(FakeProvider::new(zones));
        let nest = Arc::new(FakeNest {
            domains: StdMutex::new(domains),
            ..Default::default()
        });
        let m = DnsManagementMachine::with_credentials(
            nest,
            store.clone(),
            provider.clone(),
            test_signing_key(),
        );
        (m, store, provider)
    }

    /// Like [`full_machine`] but also returns the concrete `Arc<FakeNest>` so the
    /// `IssueCert` delivery tests can inspect the `publish_cert` the machine sent,
    /// and uses the caller's `actor_sk` so the sealed entry's signature is
    /// verifiable against a known key.
    fn issuer_machine(
        domains: Vec<DomainDns>,
        zones: Vec<DnsZoneRef>,
        actor_sk: SigningKey,
    ) -> (
        DnsManagementMachine,
        Arc<FakeStore>,
        Arc<FakeProvider>,
        Arc<FakeNest>,
    ) {
        let store = Arc::new(FakeStore::default());
        let provider = Arc::new(FakeProvider::new(zones));
        let nest = Arc::new(FakeNest {
            domains: StdMutex::new(domains),
            ..Default::default()
        });
        let m = DnsManagementMachine::with_credentials(
            nest.clone(),
            store.clone(),
            provider.clone(),
            actor_sk,
        );
        (m, store, provider, nest)
    }

    fn put_hetzner() -> DnsAction {
        DnsAction::PutCredentials {
            provider_id: "hetzner".into(),
            fields: vec![dns_field("api-token", "secret")],
            label: "Hetzner".into(),
        }
    }

    #[tokio::test]
    async fn put_credentials_verifies_and_stores_with_zones() {
        let (m, store, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();

        let snap = m.snapshot();
        assert_eq!(snap.credentials.len(), 1);
        assert_eq!(snap.credentials[0].provider_id, "hetzner");
        assert_eq!(snap.credentials[0].zones, vec!["example.com".to_string()]);
        assert_eq!(snap.credentials[0].label, "Hetzner");
        assert_eq!(snap.status, DnsStatus::Idle);

        // The secret is persisted in the store (never in the summary).
        let saved = store.current();
        assert_eq!(saved.credentials.len(), 1);
        assert_eq!(
            saved.credentials[0].fields,
            vec![("api-token".to_string(), SecretString::from("secret"))]
        );
        assert_eq!(saved.credentials[0].zones[0].name, "example.com");
    }

    /// **A dns write must not undo what another device stored while it ran.**
    ///
    /// The record is one whole-record latest-wins row, so a write REPLACES what
    /// is stored. `publish` loads it, then spends provider round-trips
    /// converging the zone before it writes the remembered published names
    /// back — and `issue_cert` holds its read across a whole ACME order. Writing
    /// the copy loaded before that work would silently drop anything a sibling
    /// device stored meanwhile; here, a credential the admin just added on
    /// another device. The write therefore re-reads first (`update_dns`).
    #[tokio::test]
    async fn a_dns_write_after_provider_work_keeps_a_concurrent_devices_change() {
        let (m, store, p, nest) = issuer_machine(
            vec![domain_with_dkim("example.com", &[("old", "OLDKEY")])],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        // The nest rotates the selector, so this publish changes the
        // remembered names and has something to write.
        *nest.domains.lock().unwrap() = vec![domain_with_dkim("example.com", &[("new", "NEWKEY")])];
        m.dispatch(DnsAction::Refresh).await.unwrap();
        let held_before = store.current().credentials.len();

        // A sibling device adds a credential after `publish` reads the record
        // and before it writes the remembered DKIM name back.
        store.inject_after_next_load(|dns| {
            dns.credentials.push(DnsProviderCredential {
                provider_id: "cloudflare".into(),
                fields: vec![("api-token".into(), SecretString::from("other"))],
                zones: vec![zone("z2", "other.example")],
                label: "Cloudflare".into(),
                created_at: 1,
            });
        });
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let saved = store.current();
        assert!(
            saved.dkim_published_names["example.com"].contains(&dkim_name("new", "example.com")),
            "the publish must still record the name it published"
        );
        assert_eq!(
            saved.credentials.len(),
            held_before + 1,
            "the publish wrote back the record it read before its provider work \
             and dropped the credential another device had just added"
        );
    }

    #[tokio::test]
    async fn put_credentials_verify_failure_surfaces_and_stores_nothing() {
        let store = Arc::new(FakeStore::default());
        let provider = Arc::new(FakeProvider {
            zones: vec![],
            published: StdMutex::new(vec![]),
            torn_down: StdMutex::new(vec![]),
            find_seed: StdMutex::new(vec![]),
            fail_verify: true,
            fail_find: StdMutex::new(false),
            fail_teardown: StdMutex::new(false),
            fail_publish: StdMutex::new(false),
        });
        let nest = Arc::new(FakeNest {
            domains: StdMutex::new(vec![domain("example.com")]),
            ..Default::default()
        });
        let m = DnsManagementMachine::with_credentials(
            nest,
            store.clone(),
            provider,
            test_signing_key(),
        );
        m.hydrate().await.unwrap();

        let err = m.dispatch(put_hetzner()).await.unwrap_err();
        assert!(matches!(err, DnsDispatchError::Provider(_)));
        assert!(m.snapshot().error.as_deref().unwrap().contains("bad token"));
        assert!(store.current().credentials.is_empty());
    }

    #[tokio::test]
    async fn put_credentials_replaces_same_provider_and_zone_set() {
        let (m, store, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::PutCredentials {
            provider_id: "hetzner".into(),
            fields: vec![dns_field("api-token", "rotated")],
            label: "Hetzner (rotated)".into(),
        })
        .await
        .unwrap();

        assert_eq!(m.snapshot().credentials.len(), 1);
        assert_eq!(m.snapshot().credentials[0].label, "Hetzner (rotated)");
        let saved = store.current();
        assert_eq!(saved.credentials[0].fields[0].1.as_str(), "rotated");
    }

    #[tokio::test]
    async fn set_mode_managed_requires_covering_credential() {
        let (m, _s, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        // No credential held yet → cannot enable managed mode.
        let err = m
            .dispatch(DnsAction::SetMode {
                domain: "example.com".into(),
                managed: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::InvalidState(_)));
        assert_eq!(m.snapshot().domains[0].mode, "manual");
    }

    #[tokio::test]
    async fn effective_mode_is_managed_only_when_opted_in_and_covered() {
        let (m, _s, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().domains[0].mode, "manual");

        // A held credential alone does not flip the mode — opt-in is required.
        m.dispatch(put_hetzner()).await.unwrap();
        assert_eq!(m.snapshot().domains[0].mode, "manual");

        // Opt in → now effectively managed (opted-in ∧ covered).
        m.dispatch(DnsAction::SetMode {
            domain: "example.com".into(),
            managed: true,
        })
        .await
        .unwrap();
        assert_eq!(m.snapshot().domains[0].mode, "managed");
    }

    /// Opting a domain IN publishes its record matrix in the same dispatch — the
    /// sequencing lives in the machine, so no app can opt in without publishing.
    #[tokio::test]
    async fn set_mode_managed_publishes_the_domain() {
        let (m, _s, p) = full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::SetMode {
            domain: "example.com".into(),
            managed: true,
        })
        .await
        .unwrap();

        let published = p.published.lock().unwrap();
        assert_eq!(published.len(), 1, "the opt-in publishes exactly once");
        assert_eq!(published[0].0, "example.com");
        assert_eq!(published[0].1[0].record_type, "MX");
    }

    /// Opting OUT publishes nothing.
    #[tokio::test]
    async fn set_mode_manual_does_not_publish() {
        let (m, _s, p) = full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::SetMode {
            domain: "example.com".into(),
            managed: false,
        })
        .await
        .unwrap();
        assert!(p.published.lock().unwrap().is_empty());
    }

    /// A publish that fails after the opt-in is saved leaves the opt-in
    /// COMMITTED (persisted, projected managed — no fall-back to manual) and its
    /// error visible on the snapshot, the semantics every app had when it
    /// chained `Publish` itself.
    #[tokio::test]
    async fn set_mode_publish_failure_keeps_the_opt_in_and_surfaces_the_error() {
        let (m, store, p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        p.set_fail_publish(true);

        let err = m
            .dispatch(DnsAction::SetMode {
                domain: "example.com".into(),
                managed: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::Provider(_)), "{err:?}");
        let snap = m.snapshot();
        assert!(
            snap.error.as_deref().unwrap().contains("publish refused"),
            "the publish error is surfaced: {:?}",
            snap.error
        );
        assert_eq!(
            snap.domains[0].mode, "managed",
            "the opt-in stays committed"
        );
        assert!(store.current().managed_domains.contains("example.com"));
    }

    // ── Auto-renew (Slice C) ────────────────────────────────────────

    /// A managed domain auto-renews by default; opting out persists the domain in
    /// `auto_renew_off` and flips the effective flag; opting back in clears it.
    #[tokio::test]
    async fn auto_renew_defaults_on_for_managed_and_toggles_off() {
        let (m, store, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::SetMode {
            domain: "example.com".into(),
            managed: true,
        })
        .await
        .unwrap();
        // Default-on: a managed domain auto-renews with no opt-out persisted.
        assert!(
            m.snapshot().domains[0].auto_renew,
            "managed ⇒ auto-renew on by default"
        );
        assert!(store.current().auto_renew_off.is_empty());

        // Opt out → persisted + effective flag off.
        m.dispatch(DnsAction::SetAutoRenew {
            domain: "example.com".into(),
            enabled: false,
        })
        .await
        .unwrap();
        assert!(
            !m.snapshot().domains[0].auto_renew,
            "opted-out ⇒ auto-renew off"
        );
        assert!(
            store.current().auto_renew_off.contains("example.com"),
            "opt-out persisted in DnsConfig.auto_renew_off"
        );

        // Opt back in → cleared + on again.
        m.dispatch(DnsAction::SetAutoRenew {
            domain: "example.com".into(),
            enabled: true,
        })
        .await
        .unwrap();
        assert!(
            m.snapshot().domains[0].auto_renew,
            "opted back in ⇒ auto-renew on"
        );
        assert!(store.current().auto_renew_off.is_empty());
    }

    /// A manual-non-delegated domain can never auto-renew — the effective flag
    /// stays `false` even with an opt-in (stored faithfully, but a no-op until the
    /// domain becomes managed/delegated).
    #[tokio::test]
    async fn auto_renew_false_for_manual_non_delegated() {
        let (m, _s, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().domains[0].mode, "manual");
        assert!(
            !m.snapshot().domains[0].auto_renew,
            "a manual-non-delegated domain can't auto-renew"
        );
        // Even an explicit opt-in does not flip it (no managed/delegated path).
        m.dispatch(DnsAction::SetAutoRenew {
            domain: "example.com".into(),
            enabled: true,
        })
        .await
        .unwrap();
        assert!(!m.snapshot().domains[0].auto_renew);
    }

    /// A delegated manual domain auto-renews by default (the CNAME re-homes its
    /// `_acme-challenge` into a controlled zone, so a client *can* auto-issue).
    #[tokio::test]
    async fn auto_renew_defaults_on_for_delegated() {
        let (m, _s, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        // Delegate example.com's renewals into the controlled zone.
        m.dispatch(DnsAction::DelegateRenewal {
            domain: "example.com".into(),
            target_zone: "example.com".into(),
        })
        .await
        .unwrap();
        // Still effectively manual mode, but delegated → auto-renew on.
        assert_eq!(m.snapshot().domains[0].mode, "manual");
        assert!(
            m.snapshot().domains[0].auto_renew,
            "a delegated domain auto-renews by default"
        );
    }

    /// The pure auto-issue decision: at-risk ∧ auto-renew-on domains, skipping
    /// trusted certs, opted-out domains, and domains with no status row yet.
    #[test]
    fn domains_needing_auto_renew_picks_at_risk_auto_renew_domains() {
        let snap = DnsSnapshot {
            domains: vec![
                domain_view_auto("floor.example", MODE_MANAGED, true),
                domain_view_auto("expiring.example", MODE_MANAGED, true),
                domain_view_auto("trusted.example", MODE_MANAGED, true),
                domain_view_auto("optedout.example", MODE_MANAGED, false),
                domain_view_auto("manual.example", MODE_MANUAL, false),
                // Auto-renew on but no cert-status row yet → conservatively skipped.
                domain_view_auto("unknown.example", MODE_MANAGED, true),
            ],
            cert_statuses: vec![
                cert_row("floor.example", CertHealthState::OnFloorRenewNeeded),
                cert_row("expiring.example", CertHealthState::Expiring),
                cert_row("trusted.example", CertHealthState::ValidTrusted),
                cert_row("optedout.example", CertHealthState::OnFloorRenewNeeded),
                cert_row("manual.example", CertHealthState::OnFloorRenewNeeded),
            ],
            ..DnsSnapshot::empty()
        };
        assert_eq!(
            snap.domains_needing_auto_renew(),
            vec!["floor.example".to_string(), "expiring.example".to_string()],
            "only at-risk ∧ auto-renew-on domains with a status row are auto-issued"
        );
    }

    // ── The shared background auto-renew tick (§ C.3) ────────────────

    /// Build a machine whose matrix holds `names`, each seeded at-risk
    /// (`OnFloorRenewNeeded`) in the nest's cert-status reply, and each delegated
    /// so its effective `auto_renew` is on. That is the exact state a real
    /// background cadence wakes up into with work to do.
    async fn at_risk_machine(names: &[&str]) -> (DnsManagementMachine, Arc<FakeNest>) {
        let nest = Arc::new(FakeNest {
            domains: StdMutex::new(names.iter().map(|n| domain(n)).collect()),
            cert_statuses: StdMutex::new(
                names
                    .iter()
                    .map(|n| DomainCertStatus {
                        extra: Default::default(),
                        domain: (*n).into(),
                        state: WireCertHealthState::OnFloorRenewNeeded,
                        not_after_unix: 0,
                        is_floor: true,
                    })
                    .collect(),
            ),
            ..Default::default()
        });
        let store = Arc::new(FakeStore::default());
        let provider = Arc::new(FakeProvider::new(
            names.iter().map(|n| zone("z1", n)).collect(),
        ));
        let m = DnsManagementMachine::with_credentials(
            nest.clone(),
            store,
            provider,
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        for n in names {
            m.dispatch(DnsAction::DelegateRenewal {
                domain: (*n).into(),
                target_zone: (*n).into(),
            })
            .await
            .unwrap();
        }
        (m, nest)
    }

    /// Phase 1 refreshes the matrix *and* the served-cert health before asking the
    /// decision — the ordering every hand-rolled cadence had to get right. Proven
    /// by the state it depends on: the at-risk verdict is only reachable once
    /// `RefreshCertStatus` has populated `cert_statuses`, so a scan that skipped it
    /// would return nothing.
    #[tokio::test]
    async fn auto_renew_scan_refreshes_health_before_deciding() {
        let (m, _nest) = at_risk_machine(&["a.example", "b.example"]).await;
        // Precondition: the decision is empty until the scan runs, so the
        // assertion below cannot pass on stale state.
        assert!(
            m.snapshot().domains_needing_auto_renew().is_empty(),
            "no cert-status row yet ⇒ nothing is due before the scan"
        );

        let due = m.auto_renew_scan().await;
        assert_eq!(
            due,
            vec!["a.example".to_string(), "b.example".to_string()],
            "the scan refreshes health, then returns the at-risk ∧ auto-renew-on set"
        );
    }

    /// Nothing at risk ⇒ an empty scan, which is the caller's signal to resolve no
    /// `target_nest_id` and issue nothing. This is the overwhelmingly common pass.
    #[tokio::test]
    async fn auto_renew_scan_is_empty_when_nothing_is_at_risk() {
        let nest = Arc::new(FakeNest {
            domains: StdMutex::new(vec![domain("healthy.example")]),
            cert_statuses: StdMutex::new(vec![DomainCertStatus {
                extra: Default::default(),
                domain: "healthy.example".into(),
                state: WireCertHealthState::ValidTrusted,
                not_after_unix: 2_000_000_000,
                is_floor: false,
            }]),
            ..Default::default()
        });
        let m = DnsManagementMachine::new(nest);
        assert!(
            m.auto_renew_scan().await.is_empty(),
            "a trusted cert is not due for auto-renew"
        );
    }

    /// **One domain's failed order must never stop the rest.** Both domains are
    /// attempted and both land in `failed`; a pass that aborted on the first would
    /// report only one. This is the assertion the whole two-phase shape exists to
    /// protect — mutation-checked below by the trailing-refresh test, which fails
    /// independently.
    #[tokio::test]
    async fn auto_renew_issue_attempts_every_domain_after_a_failure() {
        let (m, nest) = at_risk_machine(&["a.example", "b.example"]).await;
        let due = m.auto_renew_scan().await;
        assert_eq!(due.len(), 2);

        // Halt each issuance deterministically at its first nest read — no CA is
        // ever reached, and the failure is per-domain rather than global.
        *nest.cert_status_fails.lock().unwrap() = true;

        let pass = m.auto_renew_issue(due, vec![9u8; 32]).await;
        assert!(pass.issued.is_empty(), "no domain could be issued");
        assert_eq!(
            pass.failed
                .iter()
                .map(|f| f.domain.as_str())
                .collect::<Vec<_>>(),
            vec!["a.example", "b.example"],
            "the second domain is still attempted after the first fails"
        );
        assert!(
            pass.failed.iter().all(|f| !f.error.is_empty()),
            "each failure carries the error for the caller's log"
        );
    }

    /// The trailing cert-status re-read runs **even when every domain failed**, so
    /// a caller that renders health never paints a stale row.
    ///
    /// Pinned through the *effect* the re-read exists to produce — the snapshot
    /// carrying post-issue health — not through a call record. A call-count check
    /// here is worthless and was tried first: `issue_cert` reads `cert_status`
    /// itself, so the counter advances on the issue attempt alone and the
    /// assertion stays green with the trailing re-read deleted. The issued domain
    /// is instead an *uncovered* name, which `resolve_issuance_target` rejects
    /// before any nest read, leaving the trailing re-read as the only thing that
    /// can move the snapshot.
    #[tokio::test]
    async fn auto_renew_issue_rereads_health_even_when_every_domain_failed() {
        let (m, nest) = at_risk_machine(&["a.example"]).await;
        m.auto_renew_scan().await;
        assert_eq!(
            m.snapshot().cert_statuses[0].state,
            CertHealthState::OnFloorRenewNeeded,
            "the scan left the pre-issue health in the snapshot"
        );

        // The nest's served health changes underneath us (what a real renewal
        // does); only a re-read can surface it.
        nest.cert_statuses.lock().unwrap()[0].state = WireCertHealthState::ValidTrusted;

        let pass = m
            .auto_renew_issue(vec!["other.test".into()], vec![9u8; 32])
            .await;
        assert_eq!(
            pass.failed
                .iter()
                .map(|f| f.domain.as_str())
                .collect::<Vec<_>>(),
            vec!["other.test"],
            "an uncovered domain fails fast, before any nest read"
        );
        assert!(pass.issued.is_empty());
        assert_eq!(
            m.snapshot().cert_statuses[0].state,
            CertHealthState::ValidTrusted,
            "post-issue health reached the snapshot, so the trailing re-read ran"
        );
    }

    /// The cadence is one shared number, not a per-app constant.
    #[test]
    fn auto_renew_poll_secs_is_the_shared_six_hour_cadence() {
        assert_eq!(auto_renew_poll_secs(), 6 * 60 * 60);
        assert_eq!(auto_renew_poll_secs(), AUTO_RENEW_CADENCE_SECS);
    }

    fn domain_view(name: &str, mode: &str) -> DomainView {
        DomainView {
            domain: name.into(),
            mode: mode.into(),
            is_primary: false,
            records: Vec::new(),
            auto_renew: false,
        }
    }

    /// A [`DomainView`] with an explicit effective `auto_renew` flag, for the
    /// `domains_needing_auto_renew` projection test (which reads the already-
    /// projected flag rather than re-deriving it).
    fn domain_view_auto(name: &str, mode: &str, auto_renew: bool) -> DomainView {
        DomainView {
            auto_renew,
            ..domain_view(name, mode)
        }
    }

    fn cert_row(domain: &str, state: CertHealthState) -> CertStatusRow {
        CertStatusRow {
            domain: domain.into(),
            state,
            not_after_unix: 0,
            is_floor: matches!(state, CertHealthState::OnFloorRenewNeeded),
        }
    }

    fn snapshot_with(domains: Vec<DomainView>) -> DnsSnapshot {
        DnsSnapshot {
            domains,
            ..DnsSnapshot::empty()
        }
    }

    #[test]
    fn all_domains_managed_scoped_to_active_set() {
        let snap = snapshot_with(vec![
            domain_view("a.example", MODE_MANAGED),
            domain_view("b.example", MODE_MANAGED),
            // Not in the active set below — must not affect the verdict.
            domain_view("c.example", MODE_MANUAL),
        ]);
        let active = vec!["a.example".to_string(), "b.example".to_string()];
        assert!(snap.all_domains_managed(Some(&active)));

        // One active domain still manual ⇒ master switch is off.
        let active_mixed = vec!["a.example".to_string(), "c.example".to_string()];
        assert!(!snap.all_domains_managed(Some(&active_mixed)));
    }

    #[test]
    fn all_domains_managed_active_domain_without_matrix_row_is_unmanaged() {
        let snap = snapshot_with(vec![domain_view("a.example", MODE_MANAGED)]);
        // `b.example` is active but has no row in the record matrix → not managed.
        let active = vec!["a.example".to_string(), "b.example".to_string()];
        assert!(!snap.all_domains_managed(Some(&active)));
    }

    #[test]
    fn all_domains_managed_empty_active_set_is_off() {
        let snap = snapshot_with(vec![domain_view("a.example", MODE_MANAGED)]);
        assert!(!snap.all_domains_managed(Some(&[])));
    }

    #[test]
    fn all_domains_managed_none_falls_back_to_own_domains() {
        let all = snapshot_with(vec![
            domain_view("a.example", MODE_MANAGED),
            domain_view("b.example", MODE_MANAGED),
        ]);
        assert!(all.all_domains_managed(None));

        let mixed = snapshot_with(vec![
            domain_view("a.example", MODE_MANAGED),
            domain_view("b.example", MODE_MANUAL),
        ]);
        assert!(!mixed.all_domains_managed(None));

        // No domains at all ⇒ off (not vacuously true).
        assert!(!snapshot_with(Vec::new()).all_domains_managed(None));
    }

    #[test]
    fn domain_view_is_managed_reads_the_mode_projection() {
        assert!(domain_view("a.example", MODE_MANAGED).is_managed());
        assert!(!domain_view("a.example", MODE_MANUAL).is_managed());
    }

    #[test]
    fn domain_is_managed_free_fn_matches_the_method() {
        // The FFI-facing twin native apps call (DomainView is a Record, so it
        // can't carry the method) agrees with DomainView::is_managed.
        assert!(domain_is_managed(domain_view("a.example", MODE_MANAGED)));
        assert!(!domain_is_managed(domain_view("a.example", MODE_MANUAL)));
    }

    #[tokio::test]
    async fn clear_credentials_removes_and_demotes_to_manual() {
        let (m, _s, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::SetMode {
            domain: "example.com".into(),
            managed: true,
        })
        .await
        .unwrap();
        assert_eq!(m.snapshot().domains[0].mode, "managed");

        m.dispatch(DnsAction::ClearCredentials { index: 0 })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert!(snap.credentials.is_empty());
        // The opt-in survives, but with no covering credential the effective
        // mode falls back to manual.
        assert_eq!(snap.domains[0].mode, "manual");
    }

    #[tokio::test]
    async fn clear_credentials_out_of_range_is_invalid_state() {
        let (m, _s, _p) =
            full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(DnsAction::ClearCredentials { index: 3 })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::InvalidState(_)));
    }

    #[tokio::test]
    async fn publish_requires_managed_then_creates_matrix_records() {
        let (m, _s, p) = full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();

        // Not opted in → publish refused.
        let err = m
            .dispatch(DnsAction::Publish {
                domain: "example.com".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::InvalidState(_)));

        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::SetMode {
            domain: "example.com".into(),
            managed: true,
        })
        .await
        .unwrap();
        // The opt-in published once already (`set_mode_managed_publishes_the_domain`);
        // measure the explicit `Publish` alone.
        p.published.lock().unwrap().clear();
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let published = p.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        // Published into the covering zone, carrying the nest's expected matrix
        // with the MX priority split out of the RDATA (parse_rdata).
        assert_eq!(published[0].0, "example.com");
        assert_eq!(published[0].1.len(), 1);
        assert_eq!(published[0].1[0].record_type, "MX");
        assert_eq!(published[0].1[0].value, "mail.example.com.");
        assert_eq!(published[0].1[0].priority, Some(10));
    }

    // ── Slice 5b.5: managed floor-MX DANE TLSA converge (publish + withdraw) ──

    /// A `3 1 1 <hex>` TLSA body where the 32-byte digest is all `hex_byte`.
    fn tlsa_body(hex_byte: &str) -> String {
        format!("3 1 1 {}", hex_byte.repeat(32))
    }

    /// The shared floor-MX TLSA slot name for `primary` — one source, the
    /// `fauna-mail` builder the nest emits with.
    fn tlsa_slot(primary: &str) -> String {
        fauna_mail::dns::host::build_mail_tlsa_record(primary, &[0u8; 32]).name
    }

    /// A primary domain whose matrix carries the floor-MX TLSA row (floor state),
    /// on top of the plain `domain()` (MX-only) primary.
    fn primary_with_tlsa(name: &str, body: &str) -> DomainDns {
        let mut d = domain(name);
        d.records.push(record(&tlsa_slot(name), "TLSA", body));
        d
    }

    /// A non-primary domain: MXes to the primary's `mail.<primary>` host, carries
    /// no host-level rows of its own (`is_primary == false`).
    fn secondary(name: &str, primary: &str) -> DomainDns {
        DomainDns {
            extra: Default::default(),
            domain: name.into(),
            mode: "manual".into(),
            is_primary: false,
            records: vec![record(
                &format!("{name}."),
                "MX",
                &format!("10 mail.{primary}."),
            )],
        }
    }

    /// A `PublishRecord` simulating an already-published floor-MX TLSA in the zone.
    fn published_tlsa(primary: &str, body: &str) -> PublishRecord {
        PublishRecord {
            name: tlsa_slot(primary),
            record_type: "TLSA".into(),
            value: body.into(),
            ttl_seconds: 3600,
            priority: None,
        }
    }

    /// Opt `domain` into managed mode with a held Hetzner credential covering its
    /// zone — the precondition for `Publish`. The opt-in publishes once itself;
    /// that publish's provider log is cleared so a test's own `Publish` is what
    /// its assertions count.
    async fn opt_into_managed(m: &DnsManagementMachine, p: &FakeProvider, domain: &str) {
        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::SetMode {
            domain: domain.into(),
            managed: true,
        })
        .await
        .unwrap();
        p.published.lock().unwrap().clear();
        p.torn_down.lock().unwrap().clear();
    }

    /// All TLSA `PublishRecord`s across a recorded publish/teardown log.
    fn tlsa_in(log: &[(String, Vec<PublishRecord>)]) -> Vec<PublishRecord> {
        log.iter()
            .flat_map(|(_, rs)| rs.iter())
            .filter(|r| r.record_type == "TLSA")
            .cloned()
            .collect()
    }

    /// Floor: the matrix carries the TLSA and none is published → it is created;
    /// nothing is withdrawn.
    #[tokio::test]
    async fn floor_publishes_the_dane_tlsa() {
        let body = tlsa_body("a");
        let (m, _s, p) = full_machine(
            vec![primary_with_tlsa("example.com", &body)],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let created = tlsa_in(&p.published.lock().unwrap());
        assert_eq!(created.len(), 1, "the floor-MX TLSA is created");
        assert_eq!(created[0].name, tlsa_slot("example.com"));
        assert_eq!(created[0].value, body);
        assert!(
            tlsa_in(&p.torn_down.lock().unwrap()).is_empty(),
            "nothing withdrawn on floor"
        );
    }

    /// Trusted: the matrix dropped the TLSA but a stale floor-key TLSA is still
    /// published → the reconcile withdraws it (a floor-key TLSA against a trusted
    /// cert DANE-fails). No TLSA is re-created.
    #[tokio::test]
    async fn trusted_withdraws_the_stale_dane_tlsa() {
        let stale = tlsa_body("b");
        // Plain primary `domain()` has NO TLSA row — the trusted state.
        let (m, _s, p) = full_machine(vec![domain("example.com")], vec![zone("z1", "example.com")]);
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![published_tlsa("example.com", &stale)]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let torn = tlsa_in(&p.torn_down.lock().unwrap());
        assert_eq!(torn.len(), 1, "the stale floor-key TLSA is withdrawn");
        assert_eq!(torn[0].value, stale);
        assert!(
            tlsa_in(&p.published.lock().unwrap()).is_empty(),
            "no TLSA re-created when trusted"
        );
    }

    /// Floor with the correct pin already published → idempotent no-op (not
    /// re-created, not withdrawn).
    #[tokio::test]
    async fn floor_with_correct_pin_is_idempotent() {
        let body = tlsa_body("c");
        let (m, _s, p) = full_machine(
            vec![primary_with_tlsa("example.com", &body)],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![published_tlsa("example.com", &body)]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        assert!(
            tlsa_in(&p.published.lock().unwrap()).is_empty(),
            "correct pin not re-created"
        );
        assert!(
            tlsa_in(&p.torn_down.lock().unwrap()).is_empty(),
            "correct pin not withdrawn"
        );
    }

    /// Deliberate floor-key rotation: the matrix pin changed → the new pin is
    /// created and the stale one withdrawn (full convergence on the one slot).
    #[tokio::test]
    async fn floor_key_rotation_swaps_the_pin() {
        let old = tlsa_body("1");
        let new = tlsa_body("2");
        let (m, _s, p) = full_machine(
            vec![primary_with_tlsa("example.com", &new)],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![published_tlsa("example.com", &old)]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        let created = tlsa_in(&p.published.lock().unwrap());
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].value, new, "the new pin is created");
        let torn = tlsa_in(&p.torn_down.lock().unwrap());
        assert_eq!(torn.len(), 1);
        assert_eq!(torn[0].value, old, "the stale pin is withdrawn");
    }

    /// A published pin differing only in hex case (Cloudflare read-back is
    /// case-undocumented) is treated as already-correct → no churn.
    #[tokio::test]
    async fn tlsa_hex_case_does_not_churn() {
        let lower = format!("3 1 1 {}", "ab".repeat(32));
        let upper = format!("3 1 1 {}", "AB".repeat(32));
        let (m, _s, p) = full_machine(
            vec![primary_with_tlsa("example.com", &lower)],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![published_tlsa("example.com", &upper)]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        assert!(
            tlsa_in(&p.published.lock().unwrap()).is_empty(),
            "case-only diff: not re-created"
        );
        assert!(
            tlsa_in(&p.torn_down.lock().unwrap()).is_empty(),
            "case-only diff: not withdrawn"
        );
    }

    /// A non-primary domain never touches the shared `_25._tcp.mail.<primary>`
    /// slot — even with a TLSA published in its zone, its publish leaves it alone
    /// (only the primary owns the deployment slot).
    #[tokio::test]
    async fn non_primary_publish_never_touches_the_tlsa_slot() {
        let (m, _s, p) = full_machine(
            vec![secondary("two.example", "example.com")],
            vec![zone("z1", "two.example")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "two.example").await;
        p.seed_published(vec![published_tlsa("example.com", &tlsa_body("d"))]);
        m.dispatch(DnsAction::Publish {
            domain: "two.example".into(),
        })
        .await
        .unwrap();
        assert!(
            tlsa_in(&p.torn_down.lock().unwrap()).is_empty(),
            "a non-primary publish must not withdraw the primary's TLSA"
        );
        assert!(tlsa_in(&p.published.lock().unwrap()).is_empty());
    }

    /// Best-effort: a provider that can't represent TLSA (its `find_records`
    /// rejects, like Namecheap) does NOT abort the core matrix publish — the
    /// `Publish` dispatch still succeeds and the MX/SPF/DKIM rows still land.
    #[tokio::test]
    async fn tlsa_provider_error_does_not_abort_core_publish() {
        let (m, _s, p) = full_machine(
            vec![primary_with_tlsa("example.com", &tlsa_body("e"))],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.set_fail_find(true);
        // The whole publish still succeeds (the TLSA pass swallows the error).
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .expect("core publish succeeds despite a TLSA-incapable provider");
        // The core MX row still published; no TLSA created (find rejected).
        let published = p.published.lock().unwrap();
        assert!(
            published
                .iter()
                .flat_map(|(_, rs)| rs.iter())
                .any(|r| r.record_type == "MX"),
            "the core MX row still publishes"
        );
        assert!(
            tlsa_in(&published).is_empty(),
            "no TLSA on a rejecting provider"
        );
    }

    // ── Track 7: auto-withdraw the floor-MX TLSA the instant a cert goes
    //    floor→trusted (continuous cert-coupled reconcile — `tls-certificates.md`
    //    § Implementation status, auto-republish-on-cert-flip). The trigger is the
    //    `RefreshCertStatus` observation of the primary's floor→trusted edge, so a
    //    stale floor-key TLSA (which DANE-hard-fails senders against a now-trusted
    //    cert) is withdrawn without waiting for the next managed `Publish`. ──

    /// One wire served-cert status the `FakeNest` cert-status seam reports:
    /// on-floor or trusted.
    fn seeded_cert_status(domain: &str, is_floor: bool) -> DomainCertStatus {
        DomainCertStatus {
            extra: Default::default(),
            domain: domain.into(),
            state: if is_floor {
                WireCertHealthState::OnFloorRenewNeeded
            } else {
                WireCertHealthState::ValidTrusted
            },
            not_after_unix: if is_floor { 0 } else { 2_000_000_000 },
            is_floor,
        }
    }

    /// The key Track-7 case (and the design decision — option (a)): the primary's
    /// cert flips floor→trusted while a covering credential is held but the domain
    /// is **not** opted into managed mode (`managed_domains` empty). The stale
    /// floor-key TLSA is still withdrawn — the TLSA is automatic + cert-coupled,
    /// not gated on the managed opt-in (`tls-certificates.md` § D), so its safety
    /// withdrawal follows cert reality regardless. While the cert is still on the
    /// floor, nothing is withdrawn.
    #[tokio::test]
    async fn floor_to_trusted_withdraws_stale_tlsa_even_when_unmanaged() {
        let stale = tlsa_body("a");
        let (m, _s, p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        // A held credential covering the primary's zone, but NO managed opt-in.
        m.dispatch(put_hetzner()).await.unwrap();
        assert!(
            !m.snapshot().domains.iter().any(|d| d.mode == "managed"),
            "precondition: the domain is not opted into managed mode"
        );
        p.seed_published(vec![published_tlsa("example.com", &stale)]);

        // Still on the floor → no withdraw (the pin is correct while on-floor).
        *nest.cert_statuses.lock().unwrap() = vec![seeded_cert_status("example.com", true)];
        m.dispatch(DnsAction::RefreshCertStatus).await.unwrap();
        assert!(
            tlsa_in(&p.torn_down.lock().unwrap()).is_empty(),
            "nothing withdrawn while the primary is on the floor"
        );

        // Cert flips floor→trusted → the stale floor-key TLSA is withdrawn.
        *nest.cert_statuses.lock().unwrap() = vec![seeded_cert_status("example.com", false)];
        m.dispatch(DnsAction::RefreshCertStatus).await.unwrap();
        let torn = tlsa_in(&p.torn_down.lock().unwrap());
        assert_eq!(
            torn.len(),
            1,
            "the stale floor-key TLSA is withdrawn on floor→trusted, unmanaged"
        );
        assert_eq!(torn[0].value, stale);
        assert!(
            tlsa_in(&p.published.lock().unwrap()).is_empty(),
            "no TLSA re-created when trusted"
        );
    }

    // ── Withdraw-aware DKIM converge (`reconcile_dkim_txt`) ──────────────────
    // dns-management.md § Fauna-managed → Withdraw-aware convergence: per
    // Fauna-exclusive `<selector>._domainkey.<domain>` slot, create the desired
    // TXT if absent and withdraw a published `p=` value absent from the desired
    // set. Visit set = matrix-desired ∪ client-remembered names
    // (`DnsConfig.dkim_published_names`) so a revoked selector — whose name
    // has left the matrix — still converges to removal; never a zone-wide
    // `_domainkey` sweep.

    fn dkim_name(selector: &str, domain: &str) -> String {
        format!("{selector}._domainkey.{domain}")
    }

    /// The nest's matrix `expected` for a DKIM TXT (double-quoted RDATA form).
    fn dkim_expected(p: &str) -> String {
        format!("\"v=DKIM1; k=ed25519; p={p}\"")
    }

    /// A domain whose matrix carries one DKIM TXT row per `(selector, p)`.
    fn domain_with_dkim(name: &str, selectors: &[(&str, &str)]) -> DomainDns {
        let mut d = domain(name);
        for (sel, p) in selectors {
            d.records
                .push(record(&dkim_name(sel, name), "TXT", &dkim_expected(p)));
        }
        d
    }

    /// A `PublishRecord` simulating an already-published DKIM TXT (provider
    /// content form — unquoted).
    fn published_dkim(name_fq: &str, p: &str) -> PublishRecord {
        PublishRecord {
            name: name_fq.into(),
            record_type: "TXT".into(),
            value: format!("v=DKIM1; k=ed25519; p={p}"),
            ttl_seconds: 3600,
            priority: None,
        }
    }

    /// All `_domainkey` TXT `PublishRecord`s across a recorded log.
    fn dkim_in(log: &[(String, Vec<PublishRecord>)]) -> Vec<PublishRecord> {
        log.iter()
            .flat_map(|(_, rs)| rs.iter())
            .filter(|r| r.record_type == "TXT" && r.name.contains("._domainkey."))
            .cloned()
            .collect()
    }

    /// Re-mint (the 2026-06-21 example.com incident class): the nest minted a
    /// fresh key under the SAME selector, so the published stale `p=` at that
    /// name must be withdrawn (external verifiers `dkim=fail` against it).
    #[tokio::test]
    async fn dkim_remint_withdraws_the_stale_p_value_at_the_same_selector() {
        let (m, _s, p) = full_machine(
            vec![domain_with_dkim("example.com", &[("default", "NEWKEY")])],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![published_dkim(
            &dkim_name("default", "example.com"),
            "STALEKEY",
        )]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let torn = dkim_in(&p.torn_down.lock().unwrap());
        assert_eq!(
            torn.len(),
            1,
            "the stale p= at the re-minted slot is withdrawn"
        );
        assert!(torn[0].value.contains("STALEKEY"));
        assert!(
            dkim_in(&p.published.lock().unwrap())
                .iter()
                .any(|r| r.value.contains("NEWKEY")),
            "the fresh p= is published"
        );
    }

    /// Rotation overlap: BOTH selectors are in the matrix (both desired), so
    /// nothing is withdrawn mid-overlap.
    #[tokio::test]
    async fn dkim_rotation_overlap_never_withdraws_the_old_selector() {
        let (m, _s, p) = full_machine(
            vec![domain_with_dkim(
                "example.com",
                &[("202605", "OLDKEY"), ("202608", "NEWKEY")],
            )],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![
            published_dkim(&dkim_name("202605", "example.com"), "OLDKEY"),
            published_dkim(&dkim_name("202608", "example.com"), "NEWKEY"),
        ]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        assert!(
            dkim_in(&p.torn_down.lock().unwrap()).is_empty(),
            "both selectors desired during the 24h overlap → nothing withdrawn"
        );
    }

    /// Rotation cleanup — the entrusted success bar: after the nest revokes the
    /// old selector its name LEAVES the matrix, and the pass must still
    /// converge the published TXT to removal (via the client-remembered names),
    /// then forget the name.
    #[tokio::test]
    async fn dkim_revoked_selector_converges_to_removal() {
        let (m, s, p, nest) = issuer_machine(
            vec![domain_with_dkim(
                "example.com",
                &[("202605", "OLDKEY"), ("202608", "NEWKEY")],
            )],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        // First publish: both selectors live → both names remembered.
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        let remembered = s.current().dkim_published_names.clone();
        assert_eq!(
            remembered
                .get("example.com")
                .map(|names| names.len())
                .unwrap_or(0),
            2,
            "both published selector names are remembered"
        );

        // The nest revokes the old selector: its row leaves the matrix.
        *nest.domains.lock().unwrap() =
            vec![domain_with_dkim("example.com", &[("202608", "NEWKEY")])];
        m.dispatch(DnsAction::Refresh).await.unwrap();
        // The old TXT is still published at the provider.
        p.seed_published(vec![
            published_dkim(&dkim_name("202605", "example.com"), "OLDKEY"),
            published_dkim(&dkim_name("202608", "example.com"), "NEWKEY"),
        ]);

        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let torn = dkim_in(&p.torn_down.lock().unwrap());
        assert_eq!(torn.len(), 1, "the revoked selector's TXT is withdrawn");
        assert_eq!(torn[0].name, dkim_name("202605", "example.com"));
        assert!(torn[0].value.contains("OLDKEY"));
        let remembered = s.current().dkim_published_names.clone();
        assert_eq!(
            remembered.get("example.com").cloned().unwrap_or_default(),
            std::iter::once(dkim_name("202608", "example.com")).collect::<BTreeSet<_>>(),
            "the withdrawn name is forgotten; the live one stays remembered"
        );
    }

    /// A third-party mailer's DKIM TXT at the same domain (a selector Fauna
    /// never minted — neither in the matrix nor remembered) is NEVER touched:
    /// the pass visits only Fauna-known names, not the `_domainkey` namespace.
    #[tokio::test]
    async fn dkim_third_party_selector_is_never_touched() {
        let (m, _s, p) = full_machine(
            vec![domain_with_dkim("example.com", &[("default", "OURKEY")])],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![
            published_dkim(&dkim_name("default", "example.com"), "OURKEY"),
            published_dkim(&dkim_name("mailchimp", "example.com"), "THEIRKEY"),
        ]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        assert!(
            dkim_in(&p.torn_down.lock().unwrap()).is_empty(),
            "a third-party _domainkey TXT is outside the pass's visit set"
        );
    }

    /// Best-effort: a provider whose `find_records` rejects must not abort the
    /// core publish (mirrors the TLSA pass's non-fatal contract).
    #[tokio::test]
    async fn dkim_provider_error_does_not_abort_core_publish() {
        let (m, _s, p) = full_machine(
            vec![domain_with_dkim("example.com", &[("default", "K")])],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.set_fail_find(true);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .expect("the DKIM converge is best-effort; core publish already landed");
    }

    // ── Withdraw-aware ATProto handle converge (`reconcile_atproto_txt`) ─────
    // dns-management.md § Fauna-managed → Withdraw-aware convergence +
    // atproto-pds-bridge.md § Handle: the ATProto handle is derived at READ time
    // from the current Fauna handle, so a rename moves the matrix row to a new
    // `_atproto.<handle>.<primary>` name and the old name leaves the matrix
    // entirely. Only the remembered set (`DnsConfig.atproto_published_names`)
    // can still reach the stale `did=` the rename left published.

    fn atproto_name(handle_label: &str, domain: &str) -> String {
        format!("_atproto.{handle_label}.{domain}")
    }

    /// The nest's matrix `expected` for an `_atproto` TXT (double-quoted RDATA
    /// form — `fauna_mail::dns::per_domain::build_txt_record`).
    fn atproto_expected(did: &str) -> String {
        format!("\"did={did}\"")
    }

    /// A domain whose matrix carries one `_atproto` TXT row per `(handle, did)`.
    fn domain_with_atproto(name: &str, identities: &[(&str, &str)]) -> DomainDns {
        let mut d = domain(name);
        for (handle, did) in identities {
            d.records.push(record(
                &atproto_name(handle, name),
                "TXT",
                &atproto_expected(did),
            ));
        }
        d
    }

    /// A `PublishRecord` simulating an already-published `_atproto` TXT
    /// (provider content form — unquoted).
    fn published_atproto(name_fq: &str, did: &str) -> PublishRecord {
        PublishRecord {
            name: name_fq.into(),
            record_type: "TXT".into(),
            value: format!("did={did}"),
            ttl_seconds: 3600,
            priority: None,
        }
    }

    /// All `_atproto` TXT `PublishRecord`s across a recorded log.
    fn atproto_in(log: &[(String, Vec<PublishRecord>)]) -> Vec<PublishRecord> {
        log.iter()
            .flat_map(|(_, rs)| rs.iter())
            .filter(|r| is_atproto_txt_row(&r.record_type, &r.name))
            .cloned()
            .collect()
    }

    /// The headline: a Fauna handle change re-derives the matrix row under the
    /// NEW `_atproto` name, and the stale TXT at the OLD name is withdrawn.
    /// Left published it fails closed for this user (the DID document's updated
    /// `alsoKnownAs` no longer claims the old handle) and, once someone else
    /// claims the freed handle, the create-only publish would add a SECOND
    /// `did=` there — an ambiguous handle that never resolves.
    #[tokio::test]
    async fn atproto_rename_withdraws_the_stale_handle_txt() {
        let (m, s, p, nest) = issuer_machine(
            vec![domain_with_atproto(
                "example.com",
                &[("alice", "did:plc:abc123")],
            )],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        assert_eq!(
            s.current()
                .atproto_published_names
                .get("example.com")
                .cloned()
                .unwrap_or_default(),
            std::iter::once(atproto_name("alice", "example.com")).collect::<BTreeSet<_>>(),
            "the published handle name is remembered"
        );

        // Alice renames to bob: the DID is unchanged (it is never re-minted on a
        // rename), only the derived handle moves.
        *nest.domains.lock().unwrap() = vec![domain_with_atproto(
            "example.com",
            &[("bob", "did:plc:abc123")],
        )];
        m.dispatch(DnsAction::Refresh).await.unwrap();
        // The old TXT is still published at the provider.
        p.seed_published(vec![published_atproto(
            &atproto_name("alice", "example.com"),
            "did:plc:abc123",
        )]);

        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let torn = atproto_in(&p.torn_down.lock().unwrap());
        assert_eq!(torn.len(), 1, "the renamed-away handle's TXT is withdrawn");
        assert_eq!(torn[0].name, atproto_name("alice", "example.com"));
        assert!(torn[0].value.contains("did:plc:abc123"));
        let created = atproto_in(&p.published.lock().unwrap());
        assert!(
            created
                .iter()
                .any(|r| r.name == atproto_name("bob", "example.com")),
            "the new handle's TXT is published"
        );
        assert_eq!(
            s.current()
                .atproto_published_names
                .get("example.com")
                .cloned()
                .unwrap_or_default(),
            std::iter::once(atproto_name("bob", "example.com")).collect::<BTreeSet<_>>(),
            "the withdrawn name is forgotten; the live one stays remembered"
        );
    }

    /// A still-desired handle whose published value already matches is neither
    /// withdrawn nor re-created — the pass converges, it does not churn.
    #[tokio::test]
    async fn atproto_live_handle_is_neither_withdrawn_nor_recreated() {
        let (m, _s, p) = full_machine(
            vec![domain_with_atproto(
                "example.com",
                &[("alice", "did:plc:abc123")],
            )],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![published_atproto(
            &atproto_name("alice", "example.com"),
            "did:plc:abc123",
        )]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        assert!(
            atproto_in(&p.torn_down.lock().unwrap()).is_empty(),
            "a live handle's TXT is never withdrawn"
        );
        // The generic create-only loop always publishes the whole matrix in ONE
        // call, so the row appears there by construction. What must not happen
        // is a SECOND publish call from the converge pass: the published value
        // already matches modulo the matrix's RDATA quoting, and a quote-blind
        // compare would re-create it on every publish forever.
        let publish_calls_touching_the_slot = p
            .published
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, rs)| {
                rs.iter()
                    .any(|r| is_atproto_txt_row(&r.record_type, &r.name))
            })
            .count();
        assert_eq!(
            publish_calls_touching_the_slot, 1,
            "only the generic loop publishes the slot; the converge pass adds no \
             re-create for an already-correct value (quote-insensitive compare)"
        );
    }

    /// Withdrawal is scoped twice: to Fauna-known NAMES (visit set) and, within
    /// them, to values carrying the slot marker. A foreign TXT sharing a
    /// Fauna-desired `_atproto` name survives, and so does an `_atproto` name
    /// Fauna never desired — the pass is never a `_atproto` namespace sweep.
    #[tokio::test]
    async fn atproto_foreign_values_and_unknown_names_are_never_touched() {
        let (m, _s, p) = full_machine(
            vec![domain_with_atproto(
                "example.com",
                &[("alice", "did:plc:abc123")],
            )],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![
            published_atproto(&atproto_name("alice", "example.com"), "did:plc:abc123"),
            // Someone else's verification token parked at the same name.
            PublishRecord {
                name: atproto_name("alice", "example.com"),
                record_type: "TXT".into(),
                value: "some-other-service-verification=token".into(),
                ttl_seconds: 3600,
                priority: None,
            },
            // An `_atproto` name Fauna never desired and never remembered.
            published_atproto(&atproto_name("stranger", "example.com"), "did:plc:zzz999"),
        ]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        assert!(
            atproto_in(&p.torn_down.lock().unwrap()).is_empty(),
            "neither a foreign value at a known name nor an unknown _atproto \
             name is withdrawn"
        );
    }

    /// A withdraw that does not land keeps the name REMEMBERED, so the next
    /// managed publish retries instead of silently orphaning the stale TXT.
    #[tokio::test]
    async fn atproto_failed_withdraw_keeps_the_name_remembered() {
        let (m, s, p, nest) = issuer_machine(
            vec![domain_with_atproto(
                "example.com",
                &[("alice", "did:plc:abc123")],
            )],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        *nest.domains.lock().unwrap() = vec![domain_with_atproto(
            "example.com",
            &[("bob", "did:plc:abc123")],
        )];
        m.dispatch(DnsAction::Refresh).await.unwrap();
        p.seed_published(vec![published_atproto(
            &atproto_name("alice", "example.com"),
            "did:plc:abc123",
        )]);
        p.set_fail_teardown(true);

        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .expect("the converge is best-effort; the core publish already landed");

        assert_eq!(
            s.current()
                .atproto_published_names
                .get("example.com")
                .cloned()
                .unwrap_or_default(),
            [
                atproto_name("alice", "example.com"),
                atproto_name("bob", "example.com"),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>(),
            "the un-withdrawn old name stays remembered for the next retry"
        );
    }

    /// Best-effort: a provider whose `find_records` rejects must not abort the
    /// core publish (mirrors the DKIM + TLSA passes' non-fatal contract).
    #[tokio::test]
    async fn atproto_provider_error_does_not_abort_core_publish() {
        let (m, _s, p) = full_machine(
            vec![domain_with_atproto(
                "example.com",
                &[("alice", "did:plc:abc123")],
            )],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.set_fail_find(true);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .expect("the ATProto converge is best-effort; core publish already landed");
    }

    // ── Withdraw-aware Fauna identity-root converge (`reconcile_fauna_self_txt`) ──
    // dns-management.md § Records covered (`_fauna.<domain>`) +
    // § Fauna-managed → Withdraw-aware convergence. The withdraw case here is
    // neither DKIM's re-mint nor ATProto's rename: it is a **deployment-seed
    // rotation**, which changes the VALUE at a name that never moves
    // (`box-recovery.md` § Deployment-seed rotation → *DNS row and the
    // propagation window*). Left un-withdrawn, the old `self=` sits beside the
    // new one and a fresh client resolving the zone can pin the SUPERSEDED
    // identity — the create-only publish's exact blind spot.

    const OLD_SELF: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const NEW_SELF: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn fauna_self_name(domain: &str) -> String {
        format!("_fauna.{domain}")
    }

    /// The nest's matrix `expected` for the identity-root TXT (double-quoted
    /// RDATA form — `fauna_mail::dns::per_domain::build_txt_record`).
    fn fauna_self_expected(actor_id_hex: &str) -> String {
        format!("\"self={actor_id_hex}\"")
    }

    /// A domain whose matrix carries the `_fauna` identity-root TXT.
    fn domain_with_fauna_self(name: &str, actor_id_hex: &str) -> DomainDns {
        let mut d = domain(name);
        d.records.push(record(
            &fauna_self_name(name),
            "TXT",
            &fauna_self_expected(actor_id_hex),
        ));
        d
    }

    /// A `PublishRecord` simulating an already-published identity root
    /// (provider content form — unquoted).
    fn published_fauna_self(name_fq: &str, actor_id_hex: &str) -> PublishRecord {
        PublishRecord {
            name: name_fq.into(),
            record_type: "TXT".into(),
            value: format!("self={actor_id_hex}"),
            ttl_seconds: 3600,
            priority: None,
        }
    }

    /// All `_fauna` identity-root `PublishRecord`s across a recorded log.
    fn fauna_self_in(log: &[(String, Vec<PublishRecord>)]) -> Vec<PublishRecord> {
        log.iter()
            .flat_map(|(_, rs)| rs.iter())
            .filter(|r| is_fauna_self_txt_row(&r.record_type, &r.name))
            .cloned()
            .collect()
    }

    /// The headline: a deployment-seed **rotation** re-derives the identity root
    /// at the SAME name, and the superseded `self=` is withdrawn. This is the
    /// case the create-only publish structurally cannot reach — the name never
    /// leaves the matrix, so only a value-diffing converge sees the change; left
    /// alone, both values resolve and a fresh client can pin the dead identity.
    #[tokio::test]
    async fn fauna_self_rotation_withdraws_the_superseded_identity_root() {
        let (m, s, p, nest) = issuer_machine(
            vec![domain_with_fauna_self("example.com", OLD_SELF)],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();
        assert_eq!(
            s.current()
                .fauna_self_published_names
                .get("example.com")
                .cloned()
                .unwrap_or_default(),
            std::iter::once(fauna_self_name("example.com")).collect::<BTreeSet<_>>(),
            "the published identity-root name is remembered"
        );

        // The box rotates: the matrix re-derives from the LIVE deployment key,
        // so the value moves at a name that does not.
        *nest.domains.lock().unwrap() = vec![domain_with_fauna_self("example.com", NEW_SELF)];
        m.dispatch(DnsAction::Refresh).await.unwrap();
        // The predecessor's TXT is still published at the provider.
        p.seed_published(vec![published_fauna_self(
            &fauna_self_name("example.com"),
            OLD_SELF,
        )]);

        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let torn = fauna_self_in(&p.torn_down.lock().unwrap());
        assert_eq!(torn.len(), 1, "the superseded identity root is withdrawn");
        assert_eq!(torn[0].name, fauna_self_name("example.com"));
        assert!(
            torn[0].value.contains(OLD_SELF),
            "the withdrawn value is the PREDECESSOR's, not the successor's"
        );
        let created = fauna_self_in(&p.published.lock().unwrap());
        assert!(
            created.iter().any(|r| r.value.contains(NEW_SELF)),
            "the successor's identity root is published"
        );
        assert_eq!(
            s.current()
                .fauna_self_published_names
                .get("example.com")
                .cloned()
                .unwrap_or_default(),
            std::iter::once(fauna_self_name("example.com")).collect::<BTreeSet<_>>(),
            "the name is stable across a rotation — it stays remembered"
        );
    }

    /// Marker scoping is load-bearing HERE in a way it is not for the other two
    /// slots: `_fauna.<domain>` is a **shared** Fauna name whose grammar carries
    /// sibling keys — `subhandles=true` and `cache=` are read at this exact name
    /// by `fauna_core::resolve` (`resolve_handle`). A name-scoped withdraw would
    /// destroy them; scoping to values containing `self=` is what makes the pass
    /// safe to point at a name Fauna does not exclusively own.
    #[tokio::test]
    async fn fauna_self_never_withdraws_a_sibling_key_at_the_shared_name() {
        let (m, _s, p, nest) = issuer_machine(
            vec![domain_with_fauna_self("example.com", OLD_SELF)],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        *nest.domains.lock().unwrap() = vec![domain_with_fauna_self("example.com", NEW_SELF)];
        m.dispatch(DnsAction::Refresh).await.unwrap();
        p.seed_published(vec![
            published_fauna_self(&fauna_self_name("example.com"), OLD_SELF),
            // A sibling Fauna key at the same name — the subhandles flag.
            PublishRecord {
                name: fauna_self_name("example.com"),
                record_type: "TXT".into(),
                value: "subhandles=true".into(),
                ttl_seconds: 3600,
                priority: None,
            },
            // And a third party's verification token parked there.
            PublishRecord {
                name: fauna_self_name("example.com"),
                record_type: "TXT".into(),
                value: "some-other-service-verification=token".into(),
                ttl_seconds: 3600,
                priority: None,
            },
        ]);

        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        let torn = fauna_self_in(&p.torn_down.lock().unwrap());
        assert_eq!(
            torn.len(),
            1,
            "exactly the superseded self= value is withdrawn"
        );
        assert!(torn[0].value.contains(OLD_SELF));
        let all_torn = p.torn_down.lock().unwrap().clone();
        assert!(
            !all_torn
                .iter()
                .flat_map(|(_, rs)| rs.iter())
                .any(|r| r.value.contains("subhandles") || r.value.contains("verification")),
            "sibling Fauna keys and foreign values at the shared name survive"
        );
    }

    /// A still-current identity root is neither withdrawn nor re-created — the
    /// pass converges, it does not churn (quote-insensitive value compare, so
    /// the matrix's RDATA quoting never reads as a difference).
    #[tokio::test]
    async fn fauna_self_current_root_is_neither_withdrawn_nor_recreated() {
        let (m, _s, p) = full_machine(
            vec![domain_with_fauna_self("example.com", OLD_SELF)],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.seed_published(vec![published_fauna_self(
            &fauna_self_name("example.com"),
            OLD_SELF,
        )]);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        assert!(
            fauna_self_in(&p.torn_down.lock().unwrap()).is_empty(),
            "a current identity root is never withdrawn"
        );
        let publish_calls_touching_the_slot = p
            .published
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, rs)| {
                rs.iter()
                    .any(|r| is_fauna_self_txt_row(&r.record_type, &r.name))
            })
            .count();
        assert_eq!(
            publish_calls_touching_the_slot, 1,
            "only the generic loop publishes the slot; the converge pass adds no \
             re-create for an already-correct value"
        );
    }

    /// A withdraw that does not land keeps the name REMEMBERED, so the next
    /// managed publish retries instead of orphaning a superseded identity root
    /// — the one failure mode that would leave a dead identity resolvable.
    #[tokio::test]
    async fn fauna_self_failed_withdraw_keeps_the_name_remembered() {
        let (m, s, p, nest) = issuer_machine(
            vec![domain_with_fauna_self("example.com", OLD_SELF)],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .unwrap();

        *nest.domains.lock().unwrap() = vec![domain_with_fauna_self("example.com", NEW_SELF)];
        m.dispatch(DnsAction::Refresh).await.unwrap();
        p.seed_published(vec![published_fauna_self(
            &fauna_self_name("example.com"),
            OLD_SELF,
        )]);
        p.set_fail_teardown(true);

        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .expect("the converge is best-effort; the core publish already landed");

        assert_eq!(
            s.current()
                .fauna_self_published_names
                .get("example.com")
                .cloned()
                .unwrap_or_default(),
            std::iter::once(fauna_self_name("example.com")).collect::<BTreeSet<_>>(),
            "the un-withdrawn name stays remembered for the next retry"
        );
    }

    /// Best-effort: a provider whose `find_records` rejects must not abort the
    /// core publish (mirrors the DKIM + ATProto + TLSA passes' contract).
    #[tokio::test]
    async fn fauna_self_provider_error_does_not_abort_core_publish() {
        let (m, _s, p) = full_machine(
            vec![domain_with_fauna_self("example.com", OLD_SELF)],
            vec![zone("z1", "example.com")],
        );
        m.hydrate().await.unwrap();
        opt_into_managed(&m, &p, "example.com").await;
        p.set_fail_find(true);
        m.dispatch(DnsAction::Publish {
            domain: "example.com".into(),
        })
        .await
        .expect("the identity-root converge is best-effort; core publish already landed");
    }

    /// No held credential covers the primary's own zone → the withdraw can't run
    /// (the admin owns that zone manually), so the cert-status refresh is a clean
    /// no-op: the read still succeeds and nothing is torn down.
    #[tokio::test]
    async fn floor_to_trusted_without_covering_credential_is_noop() {
        let (m, _s, p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        // No `put_hetzner` → no credential covers example.com's zone.
        p.seed_published(vec![published_tlsa("example.com", &tlsa_body("b"))]);
        *nest.cert_statuses.lock().unwrap() = vec![seeded_cert_status("example.com", true)];
        m.dispatch(DnsAction::RefreshCertStatus).await.unwrap();
        *nest.cert_statuses.lock().unwrap() = vec![seeded_cert_status("example.com", false)];
        m.dispatch(DnsAction::RefreshCertStatus)
            .await
            .expect("the cert-status read succeeds even with no covering credential");
        assert!(
            tlsa_in(&p.torn_down.lock().unwrap()).is_empty(),
            "no covering credential → no auto-withdraw (manual-mode primary)"
        );
    }

    /// The trigger is the floor→trusted **edge**, not a converge-every-refresh: a
    /// fresh first observation that lands on trusted (e.g. the cert flipped while
    /// the client was offline) withdraws once; a subsequent refresh that is still
    /// trusted does **not** re-hit the provider, even if a stale pin reappears.
    #[tokio::test]
    async fn trusted_observation_is_edge_triggered_not_per_refresh() {
        let (m, _s, p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        p.seed_published(vec![published_tlsa("example.com", &tlsa_body("c"))]);

        // First-ever observation lands on trusted (offline flip) → withdraws once.
        *nest.cert_statuses.lock().unwrap() = vec![seeded_cert_status("example.com", false)];
        m.dispatch(DnsAction::RefreshCertStatus).await.unwrap();
        assert_eq!(
            tlsa_in(&p.torn_down.lock().unwrap()).len(),
            1,
            "a fresh observation of trusted withdraws the stale pin"
        );

        // Still trusted → no re-fire even though a stale pin is published again.
        p.seed_published(vec![published_tlsa("example.com", &tlsa_body("c"))]);
        m.dispatch(DnsAction::RefreshCertStatus).await.unwrap();
        assert_eq!(
            tlsa_in(&p.torn_down.lock().unwrap()).len(),
            1,
            "a steady trusted→trusted refresh does not re-run the withdraw"
        );
    }

    #[test]
    fn parse_rdata_splits_mx_unquotes_txt_passes_through_addresses() {
        // MX: `<priority> <host>` → (host, Some(priority)).
        assert_eq!(
            parse_rdata("MX", "10 mail.example.com"),
            ("mail.example.com".to_string(), Some(10))
        );
        // TXT: the nest's single-quoted body → unquoted content.
        assert_eq!(
            parse_rdata("TXT", "\"v=spf1 mx ~all\""),
            ("v=spf1 mx ~all".to_string(), None)
        );
        // A / AAAA: verbatim, no priority.
        assert_eq!(
            parse_rdata("A", "203.0.113.7"),
            ("203.0.113.7".to_string(), None)
        );
        assert_eq!(
            parse_rdata("AAAA", "2001:db8::9"),
            ("2001:db8::9".to_string(), None)
        );
        // Malformed MX priority → passed through verbatim (no silent drop).
        assert_eq!(
            parse_rdata("MX", "mail.example.com"),
            ("mail.example.com".to_string(), None)
        );
        // SRV (CalDAV autodiscovery): full zone-file RDATA passes through as the
        // provider value, no separate priority (providers take SRV RDATA whole).
        assert_eq!(
            parse_rdata("SRV", "0 1 443 mail.example.com."),
            ("0 1 443 mail.example.com.".to_string(), None)
        );
    }

    #[tokio::test]
    async fn credential_actions_on_readonly_machine_are_invalid_state() {
        // Built via `new` (the Phase-0 read/verify page) — no credential store.
        let m = machine_with(vec![domain("example.com")]);
        m.hydrate().await.unwrap();
        assert!(matches!(
            m.dispatch(put_hetzner()).await.unwrap_err(),
            DnsDispatchError::InvalidState(_)
        ));
        assert!(matches!(
            m.dispatch(DnsAction::SetMode {
                domain: "example.com".into(),
                managed: true,
            })
            .await
            .unwrap_err(),
            DnsDispatchError::InvalidState(_)
        ));
        // Read/verify still works on a read-only machine.
        assert_eq!(m.snapshot().domains[0].mode, "manual");
    }

    // ── DnsAdminClient (WS-RPC kind/payload composition) ────────

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.dns.list_records" => fauna_protocol::encode_canonical(&ListRecordsReply {
                extra: Default::default(),
                domains: vec![domain("example.com")],
            }),
            "fauna.dns.verify_records" => fauna_protocol::encode_canonical(&VerifyRecordsReply {
                extra: Default::default(),
                domains: vec![DomainVerifyStatus {
                    extra: Default::default(),
                    domain: "example.com".into(),
                    records: vec![status(
                        "example.com.",
                        "MX",
                        &["10 mail.example.com."],
                        "ok",
                    )],
                }],
            }),
            other => panic!("unexpected kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn list_records_composes_kind_and_payload() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = DnsAdminClient::new(rec.clone());

        let reply =
            block_on(client.list_records(Some("example.com".into()))).expect("infallible mock");
        assert_eq!(reply.domains.len(), 1);
        assert_eq!(reply.domains[0].domain, "example.com");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.dns.list_records");
        let req: ListRecordsRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn verify_records_composes_kind_and_payload() {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = DnsAdminClient::new(rec.clone());

        let reply = block_on(client.verify_records(None)).expect("infallible mock");
        assert_eq!(reply.domains.len(), 1);
        assert_eq!(reply.domains[0].records[0].status, "ok");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.dns.verify_records");
        let req: VerifyRecordsRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.domain, None);
    }

    // ── nest_error (shared two-class transport-error classifier) ──────

    /// A transport error of a chosen class, so `nest_error`'s `RpcErrorClass`
    /// mapping can be asserted without a concrete native/wasm error type.
    #[derive(Debug)]
    struct FakeErr {
        rejection: bool,
    }
    impl std::fmt::Display for FakeErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "fake error (rejection={})", self.rejection)
        }
    }
    impl RpcErrorClass for FakeErr {
        fn is_rejection(&self) -> bool {
            self.rejection
        }
    }

    #[test]
    fn nest_error_maps_rejection_vs_transient() {
        assert!(matches!(
            nest_error(FakeErr { rejection: true }),
            DnsNestError::Rejected(_)
        ));
        assert!(matches!(
            nest_error(FakeErr { rejection: false }),
            DnsNestError::Transient(_)
        ));
    }

    // ── IssueCert / DNS-01 issuance orchestration (S5) ──────────────
    //
    // The CA half (account → order → finalize) needs a live ACME server and is
    // exercised by S4b's pebble real-wire test; these unit tests cover the
    // S5-specific logic that needs no CA: the testable Half-B delivery
    // choreography (seal → `fauna.tls.publish_cert`) and the fail-fast
    // precondition checks that short-circuit before any order is attempted.

    /// A valid 32-byte private-nest node id (a generated actor's id) for the
    /// `IssueCert` seal target.
    fn valid_target_id() -> Vec<u8> {
        fauna_core::identity::ActorKeypair::from_secret([9u8; 32])
            .actor_id()
            .0
            .to_vec()
    }

    /// Half B end to end (no CA): the machine seals the issued cert to the
    /// **target** nest's identity, signs it with the **issuer**'s actor key, and
    /// sends it over `fauna.tls.publish_cert` — and the recorded entry unseals +
    /// verifies back to the right cert + domain. This is the producer side of the
    /// Slice-4 namespace-sync consumer (`apply_synced_lan_cert`).
    #[tokio::test]
    async fn deliver_issued_cert_seals_to_target_and_publishes() {
        use fauna_core::identity::ActorKeypair;
        use fauna_mls::wrapped_blob::open_lan_tls_cert_entry;

        let target = ActorKeypair::from_secret([3u8; 32]);
        let issuer = ActorKeypair::from_secret([5u8; 32]);
        let target_id = target.actor_id().0;

        let (m, _store, _p, nest) = issuer_machine(
            vec![domain("home.example.com")],
            vec![zone("z1", "home.example.com")],
            issuer.signing_key().clone(),
        );

        // A real rcgen cert standing in for the DNS-01 order output.
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec!["home.example.com".to_string()]).unwrap();
        params.not_after = rcgen::date_time_ymd(2099, 1, 1);
        let cert = params.self_signed(&key).unwrap();
        let issued = Dns01Issued {
            cert_chain_pem: cert.pem(),
            privkey_pem: key.serialize_pem(),
            account_credentials: b"acct".to_vec(),
        };

        m.deliver_issued_cert(
            "home.example.com",
            &target_id,
            issuer.signing_key(),
            &issued,
        )
        .await
        .expect("deliver ok");

        let sent = nest.published_certs.lock().unwrap();
        assert_eq!(sent.len(), 1, "exactly one cert published");
        let req = &sent[0];

        // Unseal on the TARGET nest's identity x25519 secret, verifying the
        // ISSUER's actor signature — the exact consumer-side operation.
        let target_x25519_sec = target.to_x25519_secret().to_bytes();
        let opened = open_lan_tls_cert_entry(
            req.ciphertext.as_ref(),
            req.actor_sig.as_ref(),
            &issuer.verifying_key(),
            &target_x25519_sec,
        )
        .expect("verified + unsealed");
        assert_eq!(opened.domain, "home.example.com");
        assert_eq!(opened.bundle.cert_chain, issued.cert_chain_pem.as_bytes());
        assert_eq!(opened.bundle.priv_key, issued.privkey_pem.as_bytes());
    }

    /// `IssueCert` for a domain no held credential covers fails fast with
    /// `InvalidState` — before any ACME order — and publishes nothing.
    #[tokio::test]
    async fn issue_cert_uncovered_domain_errors_before_any_order() {
        let (m, _store, _p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        // Store a credential covering example.com — but issue for an uncovered name.
        m.dispatch(put_hetzner()).await.unwrap();

        let err = m
            .dispatch(DnsAction::IssueCert {
                domain: "other.test".into(),
                target_nest_id: valid_target_id(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::InvalidState(_)));
        assert!(
            nest.published_certs.lock().unwrap().is_empty(),
            "no cert published for an uncovered domain"
        );
    }

    /// A `target_nest_id` that is not exactly 32 bytes is rejected up front
    /// (before credential resolution or any order).
    #[tokio::test]
    async fn issue_cert_bad_target_id_errors_fast() {
        let (m, _store, _p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();

        let err = m
            .dispatch(DnsAction::IssueCert {
                domain: "example.com".into(),
                target_nest_id: vec![1, 2, 3],
            })
            .await
            .unwrap_err();
        match err {
            DnsDispatchError::InvalidState(msg) => assert!(msg.contains("32 bytes")),
            other => panic!("expected InvalidState about the id length, got {other:?}"),
        }
        assert!(nest.published_certs.lock().unwrap().is_empty());
    }

    /// `IssueCert` on a read/verify-only machine (built via `new`, no credential
    /// store / provider / issuer wired) returns `InvalidState`.
    #[tokio::test]
    async fn issue_cert_on_read_only_machine_errors() {
        let m = machine_with(vec![]);
        let err = m
            .dispatch(DnsAction::IssueCert {
                domain: "example.com".into(),
                target_nest_id: valid_target_id(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::InvalidState(_)));
    }

    // ── Manual-mode DNS-01 issuance (S6) ────────────────────────────
    //
    // The CA half (begin → admin pastes the surfaced `_acme-challenge` TXT →
    // complete → finalize) needs a live ACME server and is exercised by the pebble
    // real-wire test; these unit tests cover the S6-specific state machine that
    // needs no CA: the fail-fast preconditions on `BeginManualIssueCert` and the
    // `Complete`/`Cancel` transitions against the `pending_cert` paste surface.
    // (`IssueCert` stays the managed tier-2 single-call path — an uncovered domain
    // there still errors, covered above; the per-app glue routes an uncovered /
    // manual-mode domain to `BeginManualIssueCert` instead.)

    /// `BeginManualIssueCert` on a read/verify-only machine (no credential store /
    /// issuer wired) returns `InvalidState` before any CA round-trip.
    #[tokio::test]
    async fn begin_manual_issue_on_read_only_machine_errors() {
        let m = machine_with(vec![]);
        let err = m
            .dispatch(DnsAction::BeginManualIssueCert {
                domain: "example.com".into(),
                target_nest_id: valid_target_id(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::InvalidState(_)));
        assert!(
            m.snapshot().pending_cert.is_none(),
            "no paste surface on a failed begin"
        );
    }

    /// A `target_nest_id` that is not exactly 32 bytes is rejected up front — before
    /// the persisted-account load or any order — so no paste surface is shown.
    #[tokio::test]
    async fn begin_manual_issue_bad_target_id_errors_fast() {
        let (m, _store, _p, _nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();

        let err = m
            .dispatch(DnsAction::BeginManualIssueCert {
                domain: "manual.test".into(),
                target_nest_id: vec![1, 2, 3],
            })
            .await
            .unwrap_err();
        match err {
            DnsDispatchError::InvalidState(msg) => assert!(msg.contains("32 bytes")),
            other => panic!("expected InvalidState about the id length, got {other:?}"),
        }
        assert!(m.snapshot().pending_cert.is_none());
    }

    /// `CompleteManualIssueCert` with no order awaiting confirmation is
    /// `InvalidState` (and publishes nothing).
    #[tokio::test]
    async fn complete_manual_issue_with_no_pending_errors() {
        let (m, _store, _p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        let err = m
            .dispatch(DnsAction::CompleteManualIssueCert)
            .await
            .unwrap_err();
        match err {
            DnsDispatchError::InvalidState(msg) => assert!(msg.contains("awaiting confirmation")),
            other => panic!("expected InvalidState, got {other:?}"),
        }
        assert!(nest.published_certs.lock().unwrap().is_empty());
    }

    /// `CancelManualIssueCert` is an idempotent no-op when nothing is pending.
    #[tokio::test]
    async fn cancel_manual_issue_with_no_pending_is_ok() {
        let (m, _store, _p, _nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.dispatch(DnsAction::CancelManualIssueCert)
            .await
            .expect("cancel is a no-op");
        assert!(m.snapshot().pending_cert.is_none());
    }

    /// `CancelManualIssueCert` clears the surfaced `pending_cert` paste surface (the
    /// admin declined). White-box-injects the surface a real `begin` would set,
    /// since populating it for real needs a CA (the pebble test does the live
    /// round-trip).
    #[tokio::test]
    async fn cancel_manual_issue_clears_the_paste_surface() {
        let (m, _store, _p, _nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        {
            let mut inner = m.inner.lock().unwrap();
            inner.snapshot.pending_cert = Some(PendingCertIssue {
                domain: "manual.example".into(),
                challenges: vec![DnsRecordRow {
                    name: "_acme-challenge.manual.example".into(),
                    record_type: "TXT".into(),
                    expected: "challenge-token".into(),
                    ttl_seconds: 120,
                    verdict: None,
                }],
            });
        }
        assert!(m.snapshot().pending_cert.is_some(), "surface set");

        m.dispatch(DnsAction::CancelManualIssueCert)
            .await
            .expect("cancel ok");
        assert!(
            m.snapshot().pending_cert.is_none(),
            "cancel clears the paste surface"
        );
    }

    // ── Surviving an interrupted manual issuance (C2) ────────────────
    //
    // The live `Dns01OrderInProgress` is a process-local handle, so a machine
    // rebuilt mid-issuance (page navigation, app restart, second device) used to
    // drop the order with no error at all — the admin's paste card simply
    // vanished. The durable half is `DnsConfig.pending_manual_issue`; these
    // cover every link of it that needs no CA. The one link that does — a resume
    // re-opening the order against a live ACME server — stays with the pebble
    // real-wire test, the same boundary the whole S6 flow already has.

    fn a_pending_manual_issue(domain: &str, value: &str) -> PendingManualIssue {
        PendingManualIssue {
            domain: domain.into(),
            target_nest_id: valid_target_id(),
            challenges: vec![PendingChallengeRecord {
                name: format!("_acme-challenge.{domain}"),
                record_type: "TXT".into(),
                value: value.into(),
                ttl_seconds: 120,
            }],
            started_at: Timestamp::now(),
        }
    }

    /// **The C2 bug, headless.** A machine built after the admin navigated away
    /// mid-issuance holds no live order; hydrating it must re-surface the paste
    /// card from the persisted breadcrumb, showing the exact TXT they were asked
    /// to publish — not an empty page.
    #[tokio::test]
    async fn a_fresh_machine_resurfaces_the_interrupted_paste_card() {
        let (m, store, _p, _nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        store.mutate(|c| {
            c.pending_manual_issue = Some(a_pending_manual_issue("manual.example", "the-token"))
        });

        m.hydrate().await.unwrap();

        let pending = m
            .snapshot()
            .pending_cert
            .expect("the interrupted issuance re-surfaces on a fresh machine");
        assert_eq!(pending.domain, "manual.example");
        assert_eq!(pending.challenges.len(), 1);
        assert_eq!(pending.challenges[0].name, "_acme-challenge.manual.example");
        assert_eq!(
            pending.challenges[0].expected, "the-token",
            "the admin must see the value they were told to paste"
        );
        assert!(
            pending.challenges[0].verdict.is_none(),
            "the live red/green comes from VerifyRecords, never from the breadcrumb"
        );
    }

    /// The mirror image: no breadcrumb ⇒ no card. A completion or cancel on
    /// another of the admin's devices retires the breadcrumb, so this device's
    /// next refresh must drop the stale card rather than keep offering a
    /// complete button for an issuance that is over.
    #[tokio::test]
    async fn a_refresh_clears_a_card_whose_issuance_ended_elsewhere() {
        let (m, _store, _p, _nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.inner.lock().unwrap().snapshot.pending_cert = Some(PendingCertIssue {
            domain: "manual.example".into(),
            challenges: vec![],
        });

        m.dispatch(DnsAction::Refresh).await.unwrap();

        assert!(
            m.snapshot().pending_cert.is_none(),
            "an issuance retired elsewhere must not keep a card on this device"
        );
    }

    /// The write half of the breadcrumb — what a real `begin` persists once the
    /// CA hands back a challenge. Covers the projection `DnsRecordRow` →
    /// `PendingChallengeRecord` (it is `expected` that carries the paste value)
    /// **and** that the account the order was opened against rides along in the
    /// same write — a resume that outlives the process must restore this exact
    /// account, not create a fresh one (see the doc comment on the fn).
    #[tokio::test]
    async fn persisting_an_in_flight_issuance_records_the_record_to_paste() {
        let (m, store, _p, _nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        let config = m.config.clone().unwrap();
        m.persist_pending_manual_issue(
            &config,
            "manual.example",
            &[7u8; 32],
            &[DnsRecordRow {
                name: "_acme-challenge.manual.example".into(),
                record_type: "TXT".into(),
                expected: "the-token".into(),
                ttl_seconds: 120,
                verdict: None,
            }],
            b"the-account-credentials",
        )
        .await
        .unwrap();

        let saved = store.current();
        let pending = saved
            .pending_manual_issue
            .expect("begin persists a breadcrumb");
        assert_eq!(pending.domain, "manual.example");
        assert_eq!(pending.target_nest_id, vec![7u8; 32]);
        assert_eq!(pending.challenges[0].value, "the-token");
        assert_eq!(pending.challenges[0].ttl_seconds, 120);
        assert_eq!(
            saved.acme_account.as_deref(),
            Some(b"the-account-credentials".as_slice()),
            "the account the order was opened against is persisted alongside the breadcrumb, \
             so a resume after the process is gone restores the SAME account rather than \
             creating a fresh one"
        );
    }

    /// Cancel retires the breadcrumb too — otherwise the card the admin just
    /// dismissed comes straight back on the next refresh.
    #[tokio::test]
    async fn cancel_manual_issue_clears_the_persisted_breadcrumb() {
        let (m, store, _p, _nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        store.mutate(|c| {
            c.pending_manual_issue = Some(a_pending_manual_issue("manual.example", "the-token"))
        });
        m.hydrate().await.unwrap();
        assert!(m.snapshot().pending_cert.is_some(), "card re-surfaced");

        m.dispatch(DnsAction::CancelManualIssueCert).await.unwrap();

        assert!(m.snapshot().pending_cert.is_none());
        assert!(
            store.current().pending_manual_issue.is_none(),
            "cancel must retire the breadcrumb, not just the card"
        );
        // And it stays gone across a rebuild.
        m.hydrate().await.unwrap();
        assert!(m.snapshot().pending_cert.is_none());
    }

    /// A resume validates the breadcrumb's `target_nest_id` **before** opening an
    /// order, so a corrupt one fails fast with an actionable message instead of
    /// burning a CA round trip and then failing at the seal step.
    #[tokio::test]
    async fn resuming_a_breadcrumb_with_a_bad_target_errors_before_any_ca_work() {
        let (m, store, _p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        let mut breadcrumb = a_pending_manual_issue("manual.example", "the-token");
        breadcrumb.target_nest_id = vec![1, 2, 3];
        store.mutate(|c| c.pending_manual_issue = Some(breadcrumb));

        let err = m
            .dispatch(DnsAction::CompleteManualIssueCert)
            .await
            .unwrap_err();
        match err {
            DnsDispatchError::InvalidState(msg) => {
                assert!(msg.contains("3-byte"), "says what is wrong: {msg}");
                assert!(msg.contains("begin again"), "says what to do: {msg}");
            }
            other => panic!("expected InvalidState, got {other:?}"),
        }
        assert!(nest.published_certs.lock().unwrap().is_empty());
        assert!(
            matches!(m.snapshot().status, DnsStatus::Idle),
            "a failed resume must not strand the page in Working"
        );
    }

    /// A re-opened order that asks for the same value the admin already published
    /// resumes silently; ordering across multiple SANs is not meaningful.
    #[test]
    fn challenge_values_match_ignores_order() {
        let persisted = vec![
            PendingChallengeRecord {
                name: "_acme-challenge.a.example".into(),
                record_type: "TXT".into(),
                value: "token-a".into(),
                ttl_seconds: 120,
            },
            PendingChallengeRecord {
                name: "_acme-challenge.b.example".into(),
                record_type: "TXT".into(),
                value: "token-b".into(),
                ttl_seconds: 120,
            },
        ];
        let fresh = vec![
            challenge_row("_acme-challenge.b.example", "token-b"),
            challenge_row("_acme-challenge.a.example", "token-a"),
        ];
        assert!(challenge_values_match(&persisted, &fresh));
    }

    /// A CA that hands back a *new* challenge is a mismatch — the admin's record
    /// no longer validates and they must be told, not left waiting.
    #[test]
    fn challenge_values_match_rejects_a_changed_value_or_count() {
        let persisted = vec![PendingChallengeRecord {
            name: "_acme-challenge.a.example".into(),
            record_type: "TXT".into(),
            value: "token-a".into(),
            ttl_seconds: 120,
        }];
        assert!(!challenge_values_match(
            &persisted,
            &[challenge_row("_acme-challenge.a.example", "token-NEW")]
        ));
        assert!(!challenge_values_match(
            &persisted,
            &[challenge_row("_acme-challenge.OTHER.example", "token-a")]
        ));
        assert!(!challenge_values_match(&persisted, &[]));
        assert!(!challenge_values_match(
            &persisted,
            &[
                challenge_row("_acme-challenge.a.example", "token-a"),
                challenge_row("_acme-challenge.b.example", "token-b"),
            ]
        ));
    }

    fn challenge_row(name: &str, value: &str) -> DnsRecordRow {
        DnsRecordRow {
            name: name.into(),
            record_type: "TXT".into(),
            expected: value.into(),
            ttl_seconds: 120,
            verdict: None,
        }
    }

    // ── CNAME renewal-delegation (S6b) ──────────────────────────────
    //
    // The renewal-automation half of the manual tier: set up a one-time CNAME so
    // a manual domain's `_acme-challenge` renewals auto-publish into a controlled
    // zone. Config-only (no CA), so these are CA-free unit tests; the real-wire
    // publish-redirect is proven by the S6b pebble phase.

    /// `DelegateRenewal` refuses when no held credential covers the target zone —
    /// the renewal TXT could never be published there. Nothing is persisted.
    #[tokio::test]
    async fn delegate_renewal_requires_covering_credential_for_target_zone() {
        let (m, store, _p) = full_machine(
            vec![domain("controlled.example")],
            vec![zone("zc", "controlled.example")],
        );
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap(); // covers controlled.example

        let err = m
            .dispatch(DnsAction::DelegateRenewal {
                domain: "home.example.test".into(),
                target_zone: "unrelated.example".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::InvalidState(_)));
        assert!(m.snapshot().delegations.is_empty());
        assert!(store.current().delegations.is_empty());
    }

    /// `DelegateRenewal` persists the delegation and surfaces the one-time CNAME
    /// (`_acme-challenge.<domain>` → re-homed target name inside the controlled
    /// zone) on `snapshot.delegations` for the admin to paste once.
    #[tokio::test]
    async fn delegate_renewal_persists_and_surfaces_one_time_cname() {
        let (m, store, _p) = full_machine(
            vec![domain("controlled.example")],
            vec![zone("zc", "controlled.example")],
        );
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();

        m.dispatch(DnsAction::DelegateRenewal {
            domain: "home.example.test".into(),
            target_zone: "controlled.example".into(),
        })
        .await
        .expect("delegate ok");

        // Surfaced CNAME row.
        let snap = m.snapshot();
        assert_eq!(snap.delegations.len(), 1);
        let dv = &snap.delegations[0];
        assert_eq!(dv.domain, "home.example.test");
        assert_eq!(dv.cname.name, "_acme-challenge.home.example.test");
        assert_eq!(dv.cname.record_type, "CNAME");
        assert_eq!(
            dv.cname.expected,
            "_acme-challenge.home.example.test.controlled.example"
        );

        // Persisted authoritative delegation.
        let persisted = store.current().delegations.clone();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].domain, "home.example.test");
        assert_eq!(persisted[0].target_zone, "controlled.example");
        assert_eq!(
            persisted[0].target_name,
            "_acme-challenge.home.example.test.controlled.example"
        );
    }

    /// `RemoveDelegation` clears a domain's delegation and is idempotent (a no-op
    /// when nothing is delegated).
    #[tokio::test]
    async fn remove_delegation_clears_it_and_is_idempotent() {
        let (m, _store, _p) = full_machine(
            vec![domain("controlled.example")],
            vec![zone("zc", "controlled.example")],
        );
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();
        m.dispatch(DnsAction::DelegateRenewal {
            domain: "home.example.test".into(),
            target_zone: "controlled.example".into(),
        })
        .await
        .unwrap();
        assert_eq!(m.snapshot().delegations.len(), 1);

        m.dispatch(DnsAction::RemoveDelegation {
            domain: "home.example.test".into(),
        })
        .await
        .expect("remove ok");
        assert!(m.snapshot().delegations.is_empty());

        // Idempotent: removing again (nothing pending) is still Ok.
        m.dispatch(DnsAction::RemoveDelegation {
            domain: "home.example.test".into(),
        })
        .await
        .expect("idempotent remove ok");
    }

    // ── Cert-status row (Phase 4, A2) ───────────────────────────────

    /// `RefreshCertStatus` queries the served-cert health for the domains in the
    /// current matrix and projects the wire reply onto `snapshot.cert_statuses`
    /// (wire `CertHealthState` → client enum; per-domain `is_floor`/`notAfter`).
    #[tokio::test]
    async fn refresh_cert_status_overlays_served_health() {
        let nest = Arc::new(FakeNest {
            domains: StdMutex::new(vec![domain("alice.example"), domain("bob.example")]),
            cert_statuses: StdMutex::new(vec![
                DomainCertStatus {
                    extra: Default::default(),
                    domain: "alice.example".into(),
                    state: WireCertHealthState::ValidTrusted,
                    not_after_unix: 2_000_000_000,
                    is_floor: false,
                },
                DomainCertStatus {
                    extra: Default::default(),
                    domain: "bob.example".into(),
                    state: WireCertHealthState::OnFloorRenewNeeded,
                    not_after_unix: 0,
                    is_floor: true,
                },
            ]),
            ..Default::default()
        });
        let m = DnsManagementMachine::new(nest);
        m.hydrate().await.unwrap();
        assert!(
            m.snapshot().cert_statuses.is_empty(),
            "cert-status row is empty until RefreshCertStatus runs"
        );

        m.dispatch(DnsAction::RefreshCertStatus).await.unwrap();
        let rows = m.snapshot().cert_statuses;
        assert_eq!(rows.len(), 2, "one row per domain in the matrix");
        // Request order follows snapshot.domains.
        assert_eq!(rows[0].domain, "alice.example");
        assert_eq!(rows[0].state, CertHealthState::ValidTrusted);
        assert!(!rows[0].is_floor);
        assert_eq!(rows[0].not_after_unix, 2_000_000_000);
        assert_eq!(rows[1].domain, "bob.example");
        assert_eq!(rows[1].state, CertHealthState::OnFloorRenewNeeded);
        assert!(rows[1].is_floor);
        assert_eq!(rows[1].not_after_unix, 0);
    }

    /// An empty matrix → an empty cert-status set (no domains to query).
    #[tokio::test]
    async fn refresh_cert_status_empty_matrix_is_noop() {
        let m = machine_with(vec![]);
        m.hydrate().await.unwrap();
        m.dispatch(DnsAction::RefreshCertStatus).await.unwrap();
        assert!(m.snapshot().cert_statuses.is_empty());
    }

    /// `resolve_issuance_target` (the CA-free resolution `issue_cert` uses):
    /// a delegated domain resolves to the **target-zone** credential + a publish
    /// redirect; a direct managed domain resolves to its **own**-zone credential
    /// with no redirect; an uncovered, undelegated domain errors.
    #[test]
    fn resolve_issuance_target_delegated_vs_direct() {
        let cred = |zone_name: &str| DnsProviderCredential {
            provider_id: "hetzner".into(),
            fields: vec![],
            zones: vec![zone("z", zone_name)],
            label: zone_name.into(),
            created_at: 0,
        };
        let dns = DnsConfig {
            credentials: vec![cred("controlled.example"), cred("direct.example")],
            managed_domains: Default::default(),
            acme_account: None,
            delegations: vec![CnameDelegation {
                domain: "home.example.test".into(),
                target_name: "_acme-challenge.home.example.test.controlled.example".into(),
                target_zone: "controlled.example".into(),
            }],
            ..Default::default()
        };

        // Delegated: publishes into the controlled zone, redirected to target_name.
        let (c, z, names) =
            resolve_issuance_target(&dns, "home.example.test").expect("delegated resolves");
        assert_eq!(z.name, "controlled.example");
        assert_eq!(c.label, "controlled.example");
        assert_eq!(
            names,
            vec![(
                "home.example.test".to_string(),
                "_acme-challenge.home.example.test.controlled.example".to_string()
            )]
        );

        // Direct managed: own-zone credential, no redirect.
        let (c, z, names) =
            resolve_issuance_target(&dns, "direct.example").expect("direct resolves");
        assert_eq!(z.name, "direct.example");
        assert_eq!(c.label, "direct.example");
        assert!(names.is_empty());

        // Uncovered + undelegated → error (must go through manual paste instead).
        assert!(matches!(
            resolve_issuance_target(&dns, "orphan.example"),
            Err(DnsDispatchError::InvalidState(_))
        ));
    }

    /// `order_san_set` — which identifiers a DNS-01 order requests.
    ///
    /// The regression it exists to prevent: ordering the clicked domain alone
    /// installs a listener cert that **drops** `mail.<primary>`, silently pushing
    /// every MUA onto the self-signed floor on the next renewal.
    #[test]
    fn order_san_set_covers_the_listener_but_only_what_the_credential_can_publish() {
        let cred = DnsProviderCredential {
            provider_id: "hetzner".into(),
            fields: vec![],
            zones: vec![zone("z", "example.com")],
            label: "Hetzner".into(),
            created_at: 0,
        };
        let nest_wants = || {
            vec![
                "example.com".to_string(),
                "mail.example.com".to_string(),
                // An active *secondary* mail domain, in a zone this credential
                // does NOT cover: including it would fail the whole all-or-nothing
                // order, taking the primary's cert down with it.
                "other.example".to_string(),
            ]
        };

        // Managed/direct: the clicked domain leads, the covered listener names
        // follow, the uncoverable one is dropped.
        assert_eq!(
            order_san_set("example.com", nest_wants(), &cred, &[]),
            vec!["example.com".to_string(), "mail.example.com".to_string()]
        );

        // Delegated (S6b): only THIS domain's `_acme-challenge` is re-homed by the
        // CNAME, so a sibling SAN's challenge could not be published — stay
        // single-name even though the nest wants more.
        let delegated = [(
            "example.com".to_string(),
            "_acme-challenge.example.com.controlled.example".to_string(),
        )];
        assert_eq!(
            order_san_set("example.com", nest_wants(), &cred, &delegated),
            vec!["example.com".to_string()]
        );

        // An empty desired set → the clicked domain alone.
        assert_eq!(
            order_san_set("example.com", vec![], &cred, &[]),
            vec!["example.com".to_string()]
        );

        // The clicked domain is always present and always first, even when the
        // nest's set omits it (a just-added / inactive domain) — the request can
        // never come back empty, and never loses the name the admin asked for.
        assert_eq!(
            order_san_set(
                "example.com",
                vec!["mail.example.com".into(), "example.com".into()],
                &cred,
                &[]
            ),
            vec!["example.com".to_string(), "mail.example.com".to_string()]
        );
    }

    /// `issue_cert` must **ask the nest** what its listener cert should cover
    /// before ordering. `order_san_set` above proves the composition; this proves
    /// the wiring — that the reply is actually fetched. Without it a refactor
    /// could drop the read, `order_san_set` would silently see `None`, and every
    /// renewal would narrow the cert back to one name with no test going red.
    ///
    /// Deliberately asserts on the recorded seam call and **never lets an order
    /// reach a CA**: `Dns01OrderConfig::lets_encrypt` targets Let's Encrypt
    /// *production*, so a test that got as far as the ACME round-trip would create
    /// real accounts and burn real failed-validation budget on every `cargo test`.
    /// The CA half belongs to the pebble acceptance (`tests/pebble_dns01.rs`).
    #[tokio::test]
    async fn issue_cert_reads_the_listener_san_set_before_ordering() {
        let (m, _store, _p, nest) = issuer_machine(
            vec![domain("example.com")],
            vec![zone("z1", "example.com")],
            test_signing_key(),
        );
        m.hydrate().await.unwrap();
        m.dispatch(put_hetzner()).await.unwrap();

        // Halt the dispatch AT the read: a failing `cert_status` seam propagates
        // out of `issue_cert` before it ever builds an order, so the assertion
        // below observes the read with no network contact. (The seal-target and
        // credential-resolution checks both sit *upstream* of the read, so they
        // cannot serve as the observation point.)
        *nest.cert_status_fails.lock().unwrap() = true;
        let err = m
            .dispatch(DnsAction::IssueCert {
                domain: "example.com".into(),
                target_nest_id: vec![7u8; 32],
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DnsDispatchError::Nest(_)), "got {err:?}");
        assert_eq!(
            nest.cert_status_calls.lock().unwrap().as_slice(),
            &[vec!["example.com".to_string()]],
            "issue_cert must read the nest's desired SAN set before ordering"
        );
    }

    // ── The browser's arm of the DNS-01 propagation gate ────────────────────
    //
    // Web has no raw DNS, so its probe asks the nest to run the identical
    // authoritative-direct query (`NestRelayedProbe` →
    // `fauna.dns.probe_txt_visible`). These pin the translation layer: the
    // nest's verdict *is* the probe's verdict, the question reaches the nest
    // unmangled, and every failure
    // reads as "not visible yet" rather than erroring the order.

    /// The relay reports the nest's verdict verbatim, and asks the exact
    /// question it was given. The argument assertion is the load-bearing half:
    /// the gate's zone is the *publish* zone (a CNAME-delegated renewal's
    /// challenge lives in the delegation target's zone, not the SAN's own), so a
    /// relay that substituted the record's parent domain would silently probe
    /// the wrong nameservers and never confirm.
    #[tokio::test]
    async fn nest_relayed_probe_reports_the_nests_verdict() {
        let nest = Arc::new(FakeNest {
            probe_answers: StdMutex::new(vec![true]),
            ..Default::default()
        });
        let probe = NestRelayedProbe::new(nest.as_ref());
        assert!(
            probe
                .txt_visible("delegated.example", "_acme-challenge.example.com", "tok-1")
                .await
        );
        assert_eq!(
            nest.probe_calls.lock().unwrap().as_slice(),
            &[(
                "delegated.example".to_string(),
                "_acme-challenge.example.com".to_string(),
                "tok-1".to_string(),
            )],
            "the relay must forward (publish zone, record name, value) unchanged"
        );

        let nest = Arc::new(FakeNest {
            probe_answers: StdMutex::new(vec![false]),
            ..Default::default()
        });
        let probe = NestRelayedProbe::new(nest.as_ref());
        assert!(
            !probe
                .txt_visible("example.com", "_acme-challenge.example.com", "tok-1")
                .await,
            "a nest reporting not-yet-served must not read as visible"
        );
    }

    /// **The failure degradation, stated as a test.** An unreachable or erroring
    /// nest fails the `fauna.dns.probe_txt_visible` call; the relay must read that as
    /// "not visible yet" so the gate polls to its deadline and proceeds
    /// best-effort — the fixed wait's behavior. If this ever propagated the error
    /// instead, a transient nest failure would *fail* issuance.
    #[tokio::test]
    async fn nest_relayed_probe_reads_a_nest_error_as_not_yet_visible() {
        let nest = Arc::new(FakeNest {
            probe_fails: StdMutex::new(true),
            ..Default::default()
        });
        let probe = NestRelayedProbe::new(nest.as_ref());
        assert!(
            !probe
                .txt_visible("example.com", "_acme-challenge.example.com", "tok-1")
                .await,
            "an unknown-kind / unreachable nest must read as not-yet-visible, never as an error"
        );
    }

    /// The composition the web order actually runs: a real [`PropagationGate`]
    /// driven by the relay over a nest that reports not-yet twice and then yes.
    /// The gate must keep polling through the not-yet phase and proceed on the
    /// confirmation — with the fixed wait poisoned to an hour, so a green run
    /// also proves the browser no longer takes a blind sleep.
    #[tokio::test]
    async fn the_gate_polls_through_the_relay_until_the_nest_confirms() {
        let nest = Arc::new(FakeNest {
            probe_answers: StdMutex::new(vec![false, false, true]),
            ..Default::default()
        });
        let probe = NestRelayedProbe::new(nest.as_ref());
        let gate = crate::acme_shared::PropagationGate {
            probe: Some(&probe),
            fixed_wait: std::time::Duration::from_secs(3600),
            poll_interval: std::time::Duration::from_millis(1),
            deadline: std::time::Duration::from_secs(1),
        };
        let challenges = vec![crate::acme_shared::Dns01Challenge {
            domain: "example.com".into(),
            publish_name: "_acme-challenge.example.com".into(),
            dns_value: "tok-1".into(),
            challenge_url: "https://ca.test/chall/1".into(),
        }];
        gate.wait("example.com", &challenges).await;
        assert_eq!(
            nest.probe_calls.lock().unwrap().len(),
            3,
            "the gate polled the relay through both not-yet answers and stopped on the confirmation"
        );
    }

    /// `CertHealthState::as_str` must equal the serde variant name, because that
    /// is the string `fauna_core::format::cert_status_label` matches on.
    ///
    /// Derived from serde rather than hard-coded literals on purpose: a literal
    /// list would be a second hand-written copy of exactly the thing that drifted.
    /// Rename a variant and this fails; keep it and the badge stays honest.
    #[test]
    fn cert_status_name_matches_serde() {
        for state in [
            CertHealthState::ValidTrusted,
            CertHealthState::OnFloorRenewNeeded,
            CertHealthState::Expiring,
        ] {
            let serialized = serde_json::to_string(&state).expect("serializes");
            let expected = serialized.trim_matches('"');
            assert_eq!(
                state.as_str(),
                expected,
                "{state:?}: as_str must be the serde name the shared matcher reads"
            );
        }
    }

    /// The consequence, stated as a test so the reason survives: the shared
    /// matcher's fallback arm is the *alarming* label, so a drifted name paints
    /// "renew needed" over a healthy trusted cert rather than failing visibly.
    #[test]
    fn a_drifted_cert_status_name_would_read_as_on_floor() {
        assert_eq!(
            fauna_core::format::cert_status_label("VallidTrusted").key,
            "admin.dns.cert.status_on_floor",
            "a typo'd name must be shown to fall into the alarming arm"
        );
        assert_eq!(
            fauna_core::format::cert_status_label(CertHealthState::ValidTrusted.as_str()).key,
            "admin.dns.cert.status_valid",
            "and the real name must not"
        );
    }

    #[cfg(feature = "local-clock")]
    #[test]
    fn cert_status_text_renders_the_four_reachable_shapes() {
        let row = |state: CertHealthState, is_floor: bool, not_after_unix: i64| CertStatusRow {
            domain: "example.com".to_string(),
            state,
            not_after_unix,
            is_floor,
        };

        let checking = cert_status_text(None);
        assert!(
            checking.starts_with("Certificate: Checking…"),
            "no read yet must render the checking placeholder, never a guess: {checking:?}"
        );

        let self_signed =
            cert_status_text(Some(&row(CertHealthState::OnFloorRenewNeeded, true, 0)));
        assert!(
            self_signed.contains("(self-signed)"),
            "a floor cert must carry the self-signed sub-label: {self_signed:?}"
        );
        assert!(
            !self_signed.contains("expires"),
            "a floor cert's far-future expiry must be withheld: {self_signed:?}"
        );

        // 2026-07-22T00:00:00Z — a plausible future expiry, same fixed literal
        // `crate::format::epoch_secs_date`'s own test pins.
        let expiring =
            cert_status_text(Some(&row(CertHealthState::Expiring, false, 1_784_678_400)));
        assert!(
            expiring.contains("expires 2026-07-"),
            "a trusted (non-floor) cert must carry its expiry date: {expiring:?}"
        );
        assert!(
            !expiring.contains("self-signed"),
            "a trusted cert must not carry the self-signed sub-label: {expiring:?}"
        );

        // `cert_status_view` treats a non-positive `not_after_unix` as "no TLS
        // resolver reported an expiry at all" (its own doc comment), so a
        // negative value renders the bare state word — never a clamped or
        // nonsense date. This is the reachable-shapes count in the fn name:
        // checking / self-signed / dated-expiry / bare-state-no-expiry.
        let no_expiry_reported = cert_status_text(Some(&row(CertHealthState::Expiring, false, -1)));
        assert_eq!(
            no_expiry_reported, "Certificate: Expiring soon",
            "a non-positive not_after_unix must render the bare state, no expiry clause: {no_expiry_reported:?}"
        );
    }
}

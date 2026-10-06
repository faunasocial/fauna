//! UniFFI façade for the **offline co-present share ceremony**
//! — the boundary android, macOS, iOS and windows reach the ceremony
//! through. tui and linux are Rust-native and call the shared orchestration
//! directly; this module is the same act, crossed for a non-Rust-native app.
//! Reference implementation, read in full:
//! `fauna_sync_engine::offline_share`.
//!
//! # What is left below, and why
//!
//! ⚠ The claim that once stood here — that a function needing BOTH
//! `AccountStoreHandle` and the ceremony types "cannot live in either without
//! inverting that layering" — was **refuted 2026-08-22 and is now false in
//! the tree**: `fauna-sync-engine` already depends on
//! `fauna-client-capabilities` (its `account-runtime` feature; the reverse
//! edge does not exist, so there is no cycle), which makes it a legal home for
//! exactly that pair. The orchestration was lifted there, and tui and linux
//! now re-export it instead of holding ~800 duplicated lines each.
//!
//! What is left below is the **UniFFI face** — `uniffi::Record` shapes and
//! `#[uniffi::export]` entry points, which genuinely cannot move into a crate
//! that carries no UniFFI surface. The internals this face used to duplicate
//! (`flush`, `spawn_config_persist`, `now_fn`/`now_secs`, `write_through` and
//! its `WriteThroughSide`) followed the orchestration onto
//! `fauna_sync_engine::offline_share` too (2026-08-25, this crate's
//! `offline-share` feature now enables `fauna-sync-engine/p2p-share`) — every
//! call below goes there directly rather than through a local copy. Treat
//! every ordering rule below as documentation whose owner is
//! `fauna_sync_engine::offline_share` — fix a rule there first.
//!
//! The face was still one layer thicker than that until 2026-08-26:
//! `offline_share_initiate`/`_consent`/`_decline` re-implemented the shared
//! `initiate`/`consent`/`decline` orchestration instead of calling it, and
//! [`FfiCeremonySeat`] held its own `node`/`config` pair instead of wrapping
//! [`fauna_sync_engine::offline_share::CeremonySeat`]. Both are fixed now —
//! `FfiCeremonySeat` composes the shared seat type, and every ceremony act
//! below is a thin pass-through, matching the "reference implementation, read
//! in full" pointer above literally rather than only in spirit.
//!
//! # The three load-bearing orderings (do not re-derive, do not reorder)
//!
//! Copied from the tui reference, which paid for each with a real defect:
//! 1. **The reception keypair rests BEFORE the accept/consent is recorded.**
//!    Its public half rides the accept and the initiator seals the admission
//!    bundle to it; a crash between the two must leave an unused key, never
//!    an unopenable delivery.
//! 2. **The held-root row lands BEFORE the machinery rows.** The rows seal
//!    under that root, so adopting first risks entries this device could not
//!    re-open.
//! 3. **Every monotone marker is set AFTER its write returned.** A marker
//!    that outran its row would tell a resuming driver the scope is readable
//!    when it is not.

use std::sync::{Arc, Mutex};

use fauna_client_capabilities::group_ceremony_view::{
    CeremonyStatus, OfflineSharePanel, OfflineShareView, PeerCodeError, code_error_label,
    parse_peer_code, status_label,
};
use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use fauna_sync_engine::offline_share::{CeremonySeat, SessionSeat};

use crate::nest_client::FfiNestClient;
use crate::{FfiError, bytes_to_actor_id, general_err, keypair_from_bytes};

fn scope32(bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| general_err("a scope id must be exactly 32 bytes"))
}

/// One bound ceremony seat: the listener plus the config it records into.
/// Opaque to the app — a held handle plus the six methods below. Thin
/// composition over [`CeremonySeat`] — the shared struct's own fields serve
/// tui/linux directly, so it stays the canonical type; this face only adds
/// the `uniffi::Object` boundary.
#[derive(uniffi::Object)]
pub struct FfiCeremonySeat {
    inner: Arc<CeremonySeat>,
}

#[uniffi::export]
impl FfiCeremonySeat {
    /// This seat's own compare code — the bare actor key (no addressing:
    /// callers render the addressed form through [`offline_share_view`],
    /// which is what a panel actually paints).
    pub fn own_code(&self) -> String {
        self.inner.node.own_code()
    }

    /// The receive act: after the in-person compare, admit exactly this
    /// initiator's ceremony frames for the expectation's TTL.
    pub fn expect_from(&self, initiator: Vec<u8>) -> Result<(), FfiError> {
        fauna_sync_engine::offline_share::expect_from(&self.inner, bytes_to_actor_id(&initiator)?);
        Ok(())
    }

    /// Withdraw the receive act (rule 6).
    pub fn cancel_expectation(&self, initiator: Vec<u8>) -> Result<(), FfiError> {
        fauna_sync_engine::offline_share::cancel_expectation(
            &self.inner,
            &bytes_to_actor_id(&initiator)?,
        );
        Ok(())
    }
}

/// The `offline_share_drop_connections` agent command's door — drop every
/// connection a counterpart has open to this seat's listener, keeping the
/// listener up, and return how many were dropped
/// (`CeremonyNode::close_inbound`). It is the link between two devices failing
/// part-way through a ceremony, which a journey cannot otherwise cause: the
/// witness that "the share picks up again without either person entering the
/// code a second time" (`p2p.md` § Offline share initiation). tui and linux
/// call the same method on their own seat.
///
/// Compiled only into the test-flavored native FFI build (convention 15),
/// like [`offline_share_advance_clock`].
#[cfg(feature = "test-helpers")]
#[uniffi::export]
impl FfiCeremonySeat {
    pub fn drop_connections_for_test(&self) -> u32 {
        u32::try_from(self.inner.node.close_inbound()).unwrap_or(u32::MAX)
    }
}

/// The `offline_share_drop_connections` command's door for a leg with no live
/// [`FfiCeremonySeat`] handle to call the method above on — apple: the seat
/// is `DevicesMachineVM.offlineShareSeat`, a UI-held handle on the session's
/// one view model, which neither app shell's `handleTestCommand` reads.
/// Routes through this session's own seat slot instead,
/// exactly what tui/linux's arm reads off their own session
/// (`app.settings.offline_share.seat()`) rather than a UI-held handle.
/// [`session_seat_for`] mints an empty slot when none is bound yet, so a
/// `None` seat refuses rather than binding one just to report it empty.
///
/// Same act as [`FfiCeremonySeat::drop_connections_for_test`], reached a
/// different way — every other detail (what it drops, why, the return count)
/// is that method's own doc.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn offline_share_drop_connections_for_test(owner_secret: Vec<u8>) -> Result<u32, FfiError> {
    let actor = keypair_from_bytes(&owner_secret)?.actor_id();
    let seat = session_seat_for(actor).seat().ok_or_else(|| {
        general_err(
            "offline_share_drop_connections_for_test: no ceremony seat is bound for this session",
        )
    })?;
    Ok(u32::try_from(seat.node.close_inbound()).unwrap_or(u32::MAX))
}

/// The `offline_share_advance_clock` agent command's door — move the
/// co-present ceremony's ADMISSION clock
/// (`fauna_sync_engine::ceremony_clock`), the `now` a receive-act expectation
/// is minted and judged against. The window is a 15-minute Rust constant, so a
/// journey reaches "someone arriving after it has lapsed is refused like a
/// stranger" (`p2p.md` § Offline share initiation) only by moving this clock
/// (convention 14's fake clock, never a sleep). Seconds; `0` resets.
///
/// ⚠ **Process-wide, and nothing auto-resets it** — a leftover offset lapses
/// the next expectation this process mints.
///
/// Compiled only into the test-flavored native FFI build (convention 15):
/// `test-helpers` forwards `fauna-sync-engine/e2e-agent`, which the setter is
/// gated on, and the `*-ffi-test` flavors are `--release`.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn offline_share_advance_clock(now_offset_secs: i64) {
    fauna_sync_engine::ceremony_clock::set_clock_offset_secs(now_offset_secs);
}

/// The `share_serve_tally` state key's body — FFI mirror of
/// [`fauna_sync_engine::share_serve_tally::ServeTally`]: what this process's
/// share plane has served to peers, per path, plus whether the serve hold
/// ([`offline_share_hold_serves`]) is on and how many requests are parked on
/// it.
#[cfg(feature = "test-helpers")]
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiServeTally {
    /// path → manifests answered for it.
    pub manifests: std::collections::HashMap<String, u64>,
    /// path → chunk bodies answered for it.
    pub chunks: std::collections::HashMap<String, u64>,
    /// Requests currently parked on the hold. Above zero, a transfer is
    /// provably in flight and waiting.
    pub parked: u64,
    /// Whether the hold is on.
    pub held: bool,
}

#[cfg(feature = "test-helpers")]
impl From<fauna_sync_engine::share_serve_tally::ServeTally> for FfiServeTally {
    fn from(t: fauna_sync_engine::share_serve_tally::ServeTally) -> Self {
        Self {
            manifests: t.manifests.into_iter().collect(),
            chunks: t.chunks.into_iter().collect(),
            parked: t.parked,
            held: t.held,
        }
    }
}

/// The `offline_share_hold_serves` agent command's door — turn the share
/// plane's SERVE hold on or off (`fauna_sync_engine::share_serve_tally`):
/// while it is on, the next manifest request a peer asks this seat's serve
/// side for parks unanswered instead of being answered, and
/// [`share_serve_tally`]'s `parked` count says so. That turns "part-way
/// through a transfer" into a state a journey can wait for
/// (state-defined, never a sleep — convention 14), cut with
/// [`offline_share_drop_connections_for_test`] /
/// `offline_share_drop_connections`, and then release: the parked request
/// fails and is never counted as served. The witness for "an interrupted
/// device-to-device transfer picks up where it stopped without re-sending
/// what arrived" (`p2p.md` § Cross-user shared-set transfer).
///
/// ⚠ **Process-wide, and nothing auto-resets it** — a leftover hold parks the
/// very next request this process's serve side answers.
///
/// Compiled only into the test-flavored native FFI build (convention 15),
/// like [`offline_share_advance_clock`].
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn offline_share_hold_serves(on: bool) {
    fauna_sync_engine::share_serve_tally::set_hold(on);
}

/// The `share_serve_tally` agent command's door — read the tally
/// [`offline_share_hold_serves`] holds and records against, as its FFI
/// mirror [`FfiServeTally`]. A plain lock-and-clone (convention 11's
/// corollary), legal on a paint or state path.
///
/// Compiled only into the test-flavored native FFI build (convention 15).
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn share_serve_tally() -> FfiServeTally {
    fauna_sync_engine::share_serve_tally::snapshot().into()
}

/// The `offline_share_probe_set` agent command's machine result — FFI mirror
/// of [`fauna_sync_engine::share_probe::ShareProbeReport`].
#[cfg(feature = "test-helpers")]
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiShareProbeReport {
    /// The dial reached the peer's listener. `false` means nothing below ran,
    /// and the report proves nothing about the serve door.
    pub dialed: bool,
    pub dial_error: Option<String>,
    /// The peer named the set among the ones it admits this seat to.
    pub admitted: bool,
    pub admit_error: Option<String>,
    /// Change rows the peer served for the set on its first page.
    pub rows: u64,
    /// Those rows' plaintext paths — what an admitted member legitimately
    /// sees, and what a stranger must never see.
    pub paths: Vec<String>,
    pub rows_error: Option<String>,
    /// The manifest hashes the probe asked for: every one it was handed plus
    /// every one the served rows named.
    pub manifest_hashes: Vec<String>,
    /// Manifest bodies the peer answered.
    pub manifests: u64,
    pub manifest_errors: Vec<String>,
}

#[cfg(feature = "test-helpers")]
impl From<fauna_sync_engine::share_probe::ShareProbeReport> for FfiShareProbeReport {
    fn from(r: fauna_sync_engine::share_probe::ShareProbeReport) -> Self {
        Self {
            dialed: r.dialed,
            dial_error: r.dial_error,
            admitted: r.admitted,
            admit_error: r.admit_error,
            rows: r.rows,
            paths: r.paths,
            rows_error: r.rows_error,
            manifest_hashes: r.manifest_hashes,
            manifests: r.manifests,
            manifest_errors: r.manifest_errors,
        }
    }
}

/// The `offline_share_probe_set` agent command's door — dial the peer named
/// by `peer_code` as this seat's own identity, claim `group_id_hex` in the
/// admission exchange, and ask for each of `manifest_hashes` as well as any
/// the served rows name, reporting what came back rather than acting on it
/// (`fauna_sync_engine::share_probe`). It keeps asking after a refused
/// admission, which the pump never does, so from a seat the set was never
/// shared with it is the witness that "a person the folder was never shared
/// with gets nothing readable from your device" (`p2p.md` § Cross-user
/// shared-set transfer); from a member it is that witness's control. Needs
/// this seat's listener bound (either panel opens it) and fails loudly
/// without one (convention 11) — the same seat
/// [`offline_share_drop_connections_for_test`] reads off this session.
///
/// Compiled only into the test-flavored native FFI build (convention 15).
#[cfg(feature = "test-helpers")]
#[fauna_uniffi_async::export]
pub async fn offline_share_probe_set(
    owner_secret: Vec<u8>,
    peer_code: String,
    group_id_hex: String,
    manifest_hashes: Vec<String>,
) -> Result<FfiShareProbeReport, FfiError> {
    let actor = keypair_from_bytes(&owner_secret)?.actor_id();
    let seat = session_seat_for(actor).seat().ok_or_else(|| {
        general_err("offline_share_probe_set: no ceremony seat is bound for this session")
    })?;
    fauna_sync_engine::share_probe::probe_set_from_args(
        &seat.node,
        &peer_code,
        &group_id_hex,
        &manifest_hashes,
    )
    .await
    .map(Into::into)
    .map_err(general_err)
}

/// This sign-in's ONE ceremony seat slot — the FFI's
/// [`fauna_sync_engine::offline_share::SessionSeat`], the slot tui and linux
/// each hold per sign-in, so the panel's bind and the share plane's bind open
/// ONE listener on this actor's NodeId in either order (`p2p.md` § Offline
/// share initiation → *One seat per session*).
///
/// Process-wide because every UniFFI app is single-login-per-process (the
/// account-runtime host's own reasoning, `crate::account_runtime::HOST`), and
/// keyed by the actor so a slot can never be handed to a different identity:
/// a call for another actor mints a fresh slot. [`reset_session_seat`] empties
/// it at the account runtime's teardown, which is where a sign-out or switch
/// ends the session, so the next sign-in binds fresh, as tui's `sign_out`
/// does.
static SESSION_SEAT: Mutex<Option<(ActorId, SessionSeat)>> = Mutex::new(None);

/// The slot for `actor`, minted empty when none is held or it belongs to
/// another actor.
pub(crate) fn session_seat_for(actor: ActorId) -> SessionSeat {
    let mut held = SESSION_SEAT.lock().unwrap_or_else(|e| e.into_inner());
    match held.as_ref() {
        Some((owner, seat)) if *owner == actor => seat.clone(),
        _ => {
            let seat = SessionSeat::default();
            *held = Some((actor, seat.clone()));
            seat
        }
    }
}

/// Drop this session's seat slot — the account runtime's teardown calls it,
/// so a sign-out or switch leaves no listener for the next session to inherit.
pub(crate) fn reset_session_seat() {
    *SESSION_SEAT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Bind the ceremony seat through this session's slot: handed back when the
/// share plane (or an earlier panel open) already bound it, else bound — the
/// brake read live, the ceremony record loaded if this device's account
/// runtime has lent it (`p2p.md` § Offline share initiation → *The seat's
/// record is lent late* — the bind never waits for the runtime), the
/// actor-keyed transport built, the listener opened. One implementation for
/// all seven apps:
/// [`fauna_sync_engine::offline_share::bind_seat`] over
/// [`fauna_iroh::ceremony_transport`], exactly what tui and linux call.
///
/// Until 2026-09-24 this door bound its own `CeremonyNode` directly, outside
/// any session slot, so a second door (the share plane's) would have opened a
/// second listener on the same NodeId.
///
/// The transport is built from the **actor** secret (PT-1b), a different
/// endpoint from the same-account peer-sync leg's device-principal one. No
/// relay URL today — by ruling, until the nest's relay serves address
/// discovery; the ceremony's dials never attach one either way (`p2p.md`
/// § The relay → *The cross-user seat and the relay*).
#[fauna_uniffi_async::export]
pub async fn offline_share_bind_seat(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<Arc<FfiCeremonySeat>, FfiError> {
    let keypair = keypair_from_bytes(&owner_secret)?;
    let secret_hex = hex::encode(&owner_secret);
    let seat = fauna_sync_engine::offline_share::bind_seat(
        &session_seat_for(keypair.actor_id()),
        nest.nest_arc(),
        &secret_hex,
        FFI_DEVICE_LABEL,
        &fauna_iroh::ceremony_transport(),
    )
    .await
    .map_err(general_err)?;
    Ok(Arc::new(FfiCeremonySeat { inner: seat }))
}

/// The label this device's seat carries to the person across the table.
pub(crate) const FFI_DEVICE_LABEL: &str = "fauna-ffi";

/// The whole paint decision for the offline-share affordance — the panel,
/// this device's compare code (bare key before a seat binds, addressed once
/// it does), the typed counterpart, and status.
#[uniffi::export]
pub fn offline_share_view(
    panel: OfflineSharePanel,
    owner_secret: Vec<u8>,
    seat: Option<Arc<FfiCeremonySeat>>,
    peer_code_input: String,
    status: CeremonyStatus,
) -> Result<OfflineShareView, FfiError> {
    let own = keypair_from_bytes(&owner_secret)?.actor_id();
    let endpoints = seat
        .as_ref()
        .map(|s| s.inner.node.local_endpoints())
        .unwrap_or_default();
    Ok(OfflineShareView::new(
        panel,
        Some(&own),
        &endpoints,
        &peer_code_input,
        status,
    ))
}

/// Which of the affordance's elements render and which acts are live —
/// [`OfflineShareView`]'s own predicates, as data.
///
/// The Record boundary carries a view's FIELDS but never its methods, so a
/// non-Rust leg that had only [`offline_share_view`] was left to re-derive
/// `can_begin` & co. from the parse and in-flight doors — the apple and
/// windows legs each hold such a copy. This is the door that makes the copy
/// unnecessary: every gate below is the shared method's answer verbatim.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfflineShareGates {
    /// `offline-share-button` / `offline-receive-button` render.
    pub shows_entry_buttons: bool,
    /// The open panel's code widgets render.
    pub shows_code_widgets: bool,
    /// `offline-share-begin-button` is enabled.
    pub can_begin: bool,
    /// `offline-receive-expect-button` is enabled.
    pub can_expect: bool,
    /// `offline-share-cancel-button` renders.
    pub shows_cancel: bool,
}

/// The gates for a view [`offline_share_view`] returned — pure, no seat, no
/// secret: everything it needs is already on the view.
#[uniffi::export]
pub fn offline_share_gates(view: OfflineShareView) -> OfflineShareGates {
    OfflineShareGates {
        shows_entry_buttons: view.shows_entry_buttons(),
        shows_code_widgets: view.shows_code_widgets(),
        can_begin: view.can_begin(),
        can_expect: view.can_expect(),
        shows_cancel: view.shows_cancel(),
    }
}

/// Parse the typed counterpart code against this seat's own identity —
/// `offline-share-peer-code-input`'s validity gate.
#[uniffi::export]
pub fn offline_share_parse_peer_code(
    input: String,
    owner_secret: Vec<u8>,
) -> Result<PeerCodeParsed, FfiError> {
    let own = keypair_from_bytes(&owner_secret)?.actor_id();
    match parse_peer_code(&input, &own) {
        Ok(code) => Ok(PeerCodeParsed {
            actor: code.actor.0.to_vec(),
            error: None,
        }),
        Err(e) => Ok(PeerCodeParsed {
            actor: Vec::new(),
            error: Some(e),
        }),
    }
}

/// The `offline-share-status` reading for a state, as a `LocalizedText` the
/// leg resolves through its own localization pipeline.
///
/// A leg MUST call this rather than writing its own `CeremonyStatus` →
/// string `match`: the state → key decision is shared
/// (`fauna_client_capabilities::group_ceremony_view::status_label`,
/// `p2p.md` § Offline share initiation), and linux and tui each holding a
/// byte-identical copy of it is exactly the drift this door exists to stop.
#[uniffi::export]
pub fn offline_share_status_label(status: CeremonyStatus) -> LocalizedText {
    status_label(status)
}

/// Whether this side's ceremony just landed a scope this device can list —
/// the FFI door onto [`CeremonyStatus::lands_a_scope`], which the enum
/// boundary cannot carry as a method (only the variants cross). A leg calls
/// this after every status progression and re-reads its group listing on
/// `true`, so a landed scope lists as an ordinary set row without the user
/// navigating away and back (mirrors linux's `app.rs` call site).
#[uniffi::export]
pub fn offline_share_status_lands_a_scope(status: CeremonyStatus) -> bool {
    status.lands_a_scope()
}

/// Whether a ceremony is in flight — the FFI door onto
/// [`CeremonyStatus::in_flight`], same boundary limitation as
/// [`offline_share_status_lands_a_scope`]. Gates the cancel affordance and
/// stops a second Begin from minting a competing scope.
#[uniffi::export]
pub fn offline_share_status_in_flight(status: CeremonyStatus) -> bool {
    status.in_flight()
}

/// The reading for a refused compare code — `None` when the refusal is not
/// one to shout ([`PeerCodeError::Empty`]: nothing typed yet is not a
/// mistake, and the disabled act button is the honest signal).
///
/// Same rule as [`offline_share_status_label`]: the keys, and the silence,
/// are the shared crate's.
#[uniffi::export]
pub fn offline_share_code_error_label(error: PeerCodeError) -> Option<LocalizedText> {
    code_error_label(error)
}

/// [`offline_share_parse_peer_code`]'s outcome: `actor` is the 32-byte
/// counterpart id on success (empty on failure — never both). `error` is
/// `None` for both a genuine success AND an empty/not-yet-typed input
/// ([`PeerCodeError::Empty`] renders no error text, per the shared type's
/// own doc), so a leg checks `actor.is_empty()` for validity, not `error`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PeerCodeParsed {
    pub actor: Vec<u8>,
    pub error: Option<PeerCodeError>,
}

/// Run the initiator's whole side against the co-present counterpart, and
/// persist what it records.
///
/// `peer_code_input` is the counterpart's code exactly as typed
/// (`offline-share-peer-code-input`'s live value) — parsed here rather than
/// pre-parsed by the caller because the initiator needs BOTH halves the
/// typed code carries (who to dial AND where), and
/// [`offline_share_parse_peer_code`]'s own return only carries the actor
/// half (the recipient-side acts below need no more than that). `account` is
/// this device's W3 (account-data-plane.md § Workstreams) runtime handle ([`crate::account_runtime::handle`]) —
/// `None` refuses honestly ("this device has no account runtime yet"), never
/// silently. `_nest` is unread since the ceremony record moved to the
/// account plane (the record rests on this device's own store); it stays in
/// the exported signature so the four platform legs' calls are unchanged.
#[fauna_uniffi_async::export]
pub async fn offline_share_initiate(
    _nest: Arc<FfiNestClient>,
    seat: Arc<FfiCeremonySeat>,
    owner_secret: Vec<u8>,
    peer_code_input: String,
) -> Result<CeremonyStatus, FfiError> {
    let keypair = keypair_from_bytes(&owner_secret)?;
    let peer = parse_peer_code(&peer_code_input, &keypair.actor_id())
        .map_err(|e| general_err(e.to_string()))?;

    fauna_sync_engine::offline_share::initiate(
        Arc::clone(&seat.inner),
        crate::account_runtime::handle(),
        keypair,
        peer,
    )
    .await
    .map_err(general_err)
}

/// The recipient's **consent**: mint the reception keypair, rest it durably,
/// record the accept, wait for the delivery, admit it, and write the
/// machinery through. One act, because a user pressing Accept is making ONE
/// decision and every step after it is owed unconditionally. `_nest` is
/// unread, as [`offline_share_initiate`]'s.
#[fauna_uniffi_async::export]
pub async fn offline_share_consent(
    _nest: Arc<FfiNestClient>,
    seat: Arc<FfiCeremonySeat>,
    owner_secret: Vec<u8>,
    scope_id: Vec<u8>,
) -> Result<CeremonyStatus, FfiError> {
    let keypair = keypair_from_bytes(&owner_secret)?;
    let scope_id = scope32(&scope_id)?;

    fauna_sync_engine::offline_share::consent(
        Arc::clone(&seat.inner),
        crate::account_runtime::handle(),
        keypair,
        scope_id,
    )
    .await
    .map_err(general_err)
}

/// Decline an offered share — monotone, fleet-wide, and terminal (rule 6).
/// `_nest` and `_owner_secret` are unread, as [`offline_share_initiate`]'s
/// `_nest`: the seat carries the record it declines into.
#[fauna_uniffi_async::export]
pub async fn offline_share_decline(
    _nest: Arc<FfiNestClient>,
    seat: Arc<FfiCeremonySeat>,
    _owner_secret: Vec<u8>,
    scope_id: Vec<u8>,
) -> Result<(), FfiError> {
    let scope_id = scope32(&scope_id)?;
    fauna_sync_engine::offline_share::decline(Arc::clone(&seat.inner), scope_id)
        .await
        .map_err(general_err)
}

/// One offered set awaiting consent, display-ready — the
/// `folder-pending-share` knock trio's group arm.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiPendingGroupShare {
    /// The accept/decline target — by id, never by row position.
    pub scope_id: Vec<u8>,
    /// Who offered it, in the canonical short-id form every other surface
    /// uses for an actor with no handle to hand.
    pub initiator: String,
    /// The nameless set's short scope id.
    pub short_id: String,
}

/// One shared set this device holds the machinery for. No `scope_id` field:
/// nothing on this row is addressable yet — a group set has no per-row
/// gesture in v1.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiGroupScope {
    /// The short scope id — the set's only name in v1.
    pub short_id: String,
    /// How many verified members the roster carries.
    pub member_count: u32,
    /// `Some(who)` when someone else minted the scope — the recipient's
    /// "Shared by ‹them›" reading. `None` on the initiator's own set.
    pub shared_by: Option<String>,
}

/// Both halves of the folders page's group surface, read in one pass because
/// they come from one record and must never disagree about a scope.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiGroupShareViews {
    pub invitations: Vec<FfiPendingGroupShare>,
    pub scopes: Vec<FfiGroupScope>,
}

/// The shared sets this device can actually read, and the invitations still
/// awaiting consent — the folders page's group listing.
///
/// A pass-through to [`fauna_sync_engine::offline_share::load_group_shares`],
/// the read tui and linux call directly: the record comes off this device's
/// account runtime store (which answers with the nest unreachable — the
/// co-present ceremony's whole case) joined with `seat`'s replica (which
/// answers for a frame ingested before the runtime lent the record).
/// Fail-safe empty on a read error — this rides a page's background hydrate,
/// where a failed read must not blank the whole page. `_nest` is unread since
/// the record moved to the account plane; it stays in the exported signature
/// so the four platform legs' calls are unchanged.
#[fauna_uniffi_async::export]
pub async fn offline_share_load_group_shares(
    _nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    seat: Option<Arc<FfiCeremonySeat>>,
) -> Result<FfiGroupShareViews, FfiError> {
    let own = keypair_from_bytes(&owner_secret)?.actor_id();
    let views = fauna_sync_engine::offline_share::load_group_shares(
        crate::account_runtime::handle(),
        seat.as_ref().map(|s| s.inner.as_ref()),
        &own,
    )
    .await;
    Ok(FfiGroupShareViews {
        invitations: views
            .invitations
            .into_iter()
            .map(|i| FfiPendingGroupShare {
                scope_id: i.scope_id.to_vec(),
                initiator: i.initiator,
                short_id: i.short_id,
            })
            .collect(),
        scopes: views
            .scopes
            .into_iter()
            .map(|s| FfiGroupScope {
                short_id: s.short_id,
                member_count: u32::try_from(s.member_count).unwrap_or(u32::MAX),
                shared_by: s.shared_by,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One slot per sign-in: both bind doors get the same slot for the same
    /// actor (so they open one listener between them), another actor never
    /// inherits it, and the teardown's reset leaves nothing for the next
    /// session.
    ///
    /// Folded into THIS test rather than a sibling `#[test]`, on purpose:
    /// every case here shares the one process-wide `SESSION_SEAT` static, and
    /// `cargo test` runs `#[test]`s in parallel by default — a second test
    /// touching the same global would race this one's `reset_session_seat()`
    /// calls. [`offline_share_drop_connections_for_test`]'s no-seat-bound
    /// refusal is exercised right after the reset below, where the slot is
    /// mounted but not yet bound (`session_seat_for`'s `SessionSeat` never
    /// gets a real `CeremonySeat` here — that needs `bind_seat`'s network
    /// round trip, which this test never makes) — the exact state the e2e
    /// journeys never reach, since every one of them opens a panel first.
    #[test]
    fn the_session_seat_slot_is_one_per_actor_and_ends_with_the_session() {
        let alice = fauna_core::identity::ActorKeypair::from_secret([3u8; 32]).actor_id();
        let bob = fauna_core::identity::ActorKeypair::from_secret([4u8; 32]).actor_id();

        reset_session_seat();
        let first = session_seat_for(alice);
        assert!(
            session_seat_for(alice).is_same_slot(&first),
            "same actor, same slot"
        );
        // The refusal door is a `test-helpers` export, so this arm compiles only
        // with that feature on; a default-feature `cargo test -p fauna-ffi --lib`
        // (the crate alone, no sibling unifying the feature in) still builds and
        // runs the rest of the pin instead of failing at E0425.
        #[cfg(feature = "test-helpers")]
        {
            assert!(
                offline_share_drop_connections_for_test([3u8; 32].to_vec()).is_err(),
                "no seat is bound yet (the slot exists but nothing has bound it), so the \
                 door must refuse rather than report a phantom drop"
            );
        }

        let other = session_seat_for(bob);
        assert!(
            !other.is_same_slot(&first),
            "another actor gets its own slot"
        );
        assert!(
            !session_seat_for(alice).is_same_slot(&first),
            "a slot handed to another actor is gone for good"
        );

        let before = session_seat_for(alice);
        reset_session_seat();
        assert!(
            !session_seat_for(alice).is_same_slot(&before),
            "a reset ends the slot"
        );
    }

    #[test]
    fn scope32_rejects_wrong_length() {
        assert!(scope32(&[1u8; 33]).is_err());
        assert!(scope32(&[1u8; 32]).is_ok());
    }

    fn hex_of(secret: u8) -> String {
        fauna_core::identity::ActorKeypair::from_secret([secret; 32])
            .actor_id()
            .to_hex()
    }

    fn view(panel: OfflineSharePanel, peer: &str, status: CeremonyStatus) -> OfflineShareView {
        let me = fauna_core::identity::ActorKeypair::from_secret([7u8; 32]).actor_id();
        OfflineShareView::new(panel, Some(&me), &[], peer, status)
    }

    /// The gates door is a pass-through, never a second decision: across
    /// every panel × status × code shape it answers exactly what the shared
    /// view's own predicates answer — so a leg that paints from it cannot
    /// drift from tui/linux, which call those predicates directly.
    #[test]
    fn the_gates_door_answers_what_the_shared_view_answers() {
        let panels = [
            OfflineSharePanel::Closed,
            OfflineSharePanel::Initiate,
            OfflineSharePanel::Receive,
        ];
        let statuses = [
            CeremonyStatus::Idle,
            CeremonyStatus::Expecting,
            CeremonyStatus::OfferSent,
            CeremonyStatus::AwaitingConsent,
            CeremonyStatus::Delivering,
            CeremonyStatus::Delivered,
            CeremonyStatus::Admitted,
            CeremonyStatus::Failed,
        ];
        let codes = [String::new(), "not a code".into(), hex_of(7), hex_of(9)];
        for panel in panels {
            for status in statuses {
                for code in &codes {
                    let v = view(panel, code, status);
                    assert_eq!(
                        offline_share_gates(v.clone()),
                        OfflineShareGates {
                            shows_entry_buttons: v.shows_entry_buttons(),
                            shows_code_widgets: v.shows_code_widgets(),
                            can_begin: v.can_begin(),
                            can_expect: v.can_expect(),
                            shows_cancel: v.shows_cancel(),
                        },
                        "{panel:?} / {status:?} / {code:?}"
                    );
                }
            }
        }
    }

    /// Non-vacuity for the sweep above: the three refusals a user can meet
    /// at the act button, and the one acceptance, spelled out.
    #[test]
    fn begin_opens_only_on_a_counterparts_code_with_nothing_in_flight() {
        let them = hex_of(9);
        let gates = |panel, code: &str, status| offline_share_gates(view(panel, code, status));

        assert!(gates(OfflineSharePanel::Initiate, &them, CeremonyStatus::Idle).can_begin);
        // This device's own code is a mis-paste, not an intent.
        assert!(
            !gates(
                OfflineSharePanel::Initiate,
                &hex_of(7),
                CeremonyStatus::Idle
            )
            .can_begin
        );
        assert!(!gates(OfflineSharePanel::Initiate, "", CeremonyStatus::Idle).can_begin);
        // A second Begin would mint a competing scope.
        assert!(
            !gates(
                OfflineSharePanel::Initiate,
                &them,
                CeremonyStatus::AwaitingConsent
            )
            .can_begin
        );
        // Begin and Expect are one panel each, never both.
        let receive = gates(OfflineSharePanel::Receive, &them, CeremonyStatus::Idle);
        assert!(receive.can_expect && !receive.can_begin);
        let closed = gates(OfflineSharePanel::Closed, "", CeremonyStatus::Idle);
        assert!(closed.shows_entry_buttons && !closed.shows_code_widgets && !closed.shows_cancel);
    }

    /// [`offline_share_hold_serves`] and [`share_serve_tally`] are thin calls
    /// over `fauna_sync_engine::share_serve_tally` — this pins the FFI
    /// mirror's field-for-field shape, not the tally's own hold/park
    /// semantics (already pinned in that crate's own test). Process-wide
    /// state, like [`share_serve_tally::tests`]'s own note, so this stays the
    /// one test touching it in this crate.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn hold_serves_and_tally_round_trip_through_the_ffi_mirror() {
        fauna_sync_engine::share_serve_tally::record_manifest_served("offline-share-test/a.txt");
        let snap = share_serve_tally();
        assert_eq!(
            snap.manifests.get("offline-share-test/a.txt"),
            Some(&1),
            "the FFI mirror carries the same per-path counts the shared tally holds"
        );
        assert!(!snap.held, "the hold starts off");

        offline_share_hold_serves(true);
        assert!(
            share_serve_tally().held,
            "the door's `on` flag reaches the tally"
        );
        offline_share_hold_serves(false);
        assert!(!share_serve_tally().held, "and so does turning it back off");
    }

    /// [`offline_share_probe_set`] reads its seat off THIS session's slot,
    /// the same one [`offline_share_drop_connections_for_test`] reads —
    /// unbound, it refuses rather than dialing nothing (convention 11).
    /// Folded into the session-seat test above's actor rather than a fresh
    /// one, since both share the one process-wide `SESSION_SEAT`.
    #[cfg(feature = "test-helpers")]
    #[tokio::test]
    async fn probe_set_refuses_with_no_seat_bound() {
        reset_session_seat();
        let secret = [5u8; 32];
        let err =
            offline_share_probe_set(secret.to_vec(), String::new(), String::new(), Vec::new())
                .await
                .expect_err(
                    "no seat is bound yet, so the probe must refuse rather than dial nothing",
                );
        assert!(err.to_string().contains("no ceremony seat is bound"));
    }
}

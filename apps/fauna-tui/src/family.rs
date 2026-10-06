//! The gated **Family** page (`docs/goal/behavior/family-safety.md` § Client
//! surface) — one page rendering three role-dependent sections, exactly as the
//! six sibling clients render them (priority #1/#3; windows is the reference
//! leg, linux the direct-Rust one this module lifts):
//!
//! - **Guardian side** (the caller guards ≥1 account): the wards list
//!   (`family-ward-item`, indexed, each carrying `family-ward-handle` and — when
//!   the ward has Guardian Notify counts today — `family-ward-content-notices`),
//!   the ONE shared reach/content-policy editor for the *selected* ward, contact
//!   pre-approval, the transfer handshake, graduation, and the cross-ward
//!   approvals queue (`family-approval-item`, indexed).
//! - **Supervised side** (the caller is supervised): `family-guardian-handle` +
//!   the read-only `family-policy-summary`, plus the global
//!   `supervised-indicator` chrome ([`crate::ui::register_frame`]).
//! - **Incoming-transfer prompt** (the caller is a *proposed* guardian):
//!   `family-incoming-transfer-item`, indexed — independent of both roles, which
//!   is exactly why the `family-tab` gate widens to cover it.
//!
//! ui.yaml's `family:` block carries exactly ONE (non-indexed) policy-editor
//! element set, so a guardian with several wards edits one at a time: clicking a
//! `family-ward-item` row loads that ward's policy into the shared editor. The
//! approvals queue is **not** ward-scoped — `fauna.family.approvals.list` returns
//! every ward's queue in one read.
//!
//! **GATED like [`crate::admin`], not a member of [`Page::ALL`].** The
//! `family-tab` sidebar row joins the visible set only when the post-auth
//! `fauna.family.status` read reports a relationship — supervised, guarding, or
//! named on an incoming transfer ([`crate::app::App::has_family`], fail-closed
//! until the read lands, like `am_i_admin`).
//!
//! **Where the logic lives.** All of it is already shared: the typed
//! `fauna_client_family::FamilyClient` over the `fauna.family.*` wire (direct
//! Rust, no FFI hop — the linux posture, priority #2), plus
//! `fauna_core::format`'s option catalogs, fail-closed label lookups and summary
//! composers. This page composes **zero** policy logic of its own.
//!
//! Two hard e2e contracts live here, the same two linux's `views/family.rs`
//! documents:
//!
//! 1. **Every id'd toggle carries `.attr("state", "on"|"off")`** — `actions/
//!    family.py` reads a toggle through `get_attr(id, "state")`, never its text.
//! 2. **Both kinds of select round-trip the LOCALIZED LABEL, never the wire
//!    value.** `family.py` compares `get_text` against `UNKNOWN_SENDER_LABELS` /
//!    `FEED_SOURCES_LABELS` / `CONTENT_FLOOR_LABELS` and calls `select(id,
//!    LABEL)`. So an option list is the shared catalog resolved through
//!    [`crate::wizard::localized`], and a committed label maps back to its wire
//!    value here — degrading, when it matches nothing, to the knob's
//!    **fail-closed** option (`hold` / `block`), never the permissive one
//!    (`family-safety.md` § Implementation status: a safety rule, not a cosmetic
//!    one).
//!
//! **Non-optimistic.** Every successful mutation ends in [`refresh`], so a
//! painted control always reflects what the nest persisted rather than the tap —
//! the same rule `crate::bridges` states and linux's `dispatch` → re-`load`
//! enforces.
//!
//! **The screen-time pillar (Slice E) is built here**, lifting the linux + web
//! reference legs: the guardian's three typed inputs
//! (`family-policy-screen-{window-start,window-end,daily-minutes}-input`), the
//! per-ward `family-ward-usage-today` readout, and the ward's own figure folded
//! into `family-policy-summary`. The ward-side lock itself is
//! [`crate::screen_lock`] — a global surface, not a Family-page element. The
//! guardian device-mark control (`family-device-mark-item`/`-toggle`) is built
//! too — see [`device_mark_elements`], the lift of the pattern linux + web
//! proved 2026-08-01.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_family::FamilyClient;
use fauna_client_family::SupervisionSnapshot;
use fauna_client_family::family::{
    FamilyApprovalEntry, FamilyContactRequestInfo, FamilyContentNotice, FamilyFeedRequestInfo,
    FamilyIncomingTransferInfo, FamilyWardInfo, ReachPolicy,
};
use fauna_client_family::row_text::{own_age_band_text, ward_age_band_text};
use fauna_core::data::{FeedSources, UnknownPeerDm, UnknownSenderMail};
use fauna_core::obligation::{ContentFloor, ContentPolicy, GUARDIAN_FLOOR_CATEGORIES};
use fauna_i18n::strings::family as t;
use fauna_protocol::family::FamilyAgeBandInfo;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, PageOutcome, UiMessage};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;
use crate::wizard::localized;

// ── State ────────────────────────────────────────────────────────────────────

/// The working buffer behind the ONE shared policy editor — the guardian's edits
/// before `family-policy-save-button` commits them.
///
/// Holds **parsed** knob values rather than raw wire strings: every value that
/// enters here has already been through the shared fail-closed parse
/// ([`UnknownSenderMail::from_wire`] / [`FeedSources::from_wire`] /
/// [`ContentFloor::from_wire`]), so it is always a member of its picker catalog
/// and the save direction cannot write back a value the nest would reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEditor {
    pub contact_approval: bool,
    pub unknown_sender: UnknownSenderMail,
    pub federation: bool,
    pub feed_sources: FeedSources,
    /// The bridge-DM gate's knob (`family-safety.md` § The bridge-DM gate).
    /// Parsed like its siblings, but paired with [`Self::unknown_peer_dm_edited`]
    /// because it is the one reach knob that rides the wire as `Option<String>`.
    pub unknown_peer_dm: UnknownPeerDm,
    /// Whether the guardian has touched `family-policy-unknown-peer-dm-select`
    /// this editing session — the gate on whether [`Self::to_policy`] sends the
    /// knob at all.
    ///
    /// An **absent** `unknown_peer_dm` means *leave it unchanged*
    /// (`family-safety.md` § Policy-update compatibility), and that is the
    /// safe default to send: the nest reports a value only a *newer* nest could
    /// have written verbatim, which this build fails closed to `hold` on render
    /// — echoing that render back on an unrelated save would rewrite the ward's
    /// knob to a value the guardian never chose. No sibling knob needs this: the
    /// other reach knobs are non-optional on the wire, and the content floors'
    /// fail-closed option is already their strictest.
    pub unknown_peer_dm_edited: bool,
    /// The four per-category content floors, index-aligned with
    /// [`GUARDIAN_FLOOR_CATEGORIES`] (nsfw / spam / phishing / commercial).
    pub content: [ContentFloor; 4],
    pub content_notify: bool,
    /// Screen time, held as the guardian's **raw typed text** rather than
    /// parsed minutes — unlike every knob above.
    ///
    /// The reason is that an empty field is a meaningful value here ("this
    /// control is unset", which is how a guardian clears a limit), and so is a
    /// half-typed one: parsing per keystroke would either reject the guardian
    /// mid-word or silently discard what they typed. The text is converted —
    /// and validated — once, in [`Self::to_policy`], through the same shared
    /// rules the nest refuses by.
    pub screen_window_start: String,
    pub screen_window_end: String,
    pub screen_daily_minutes: String,
}

impl Default for PolicyEditor {
    /// The unsupervised-equivalent policy — the same defaults
    /// [`ReachPolicy::default`] carries, so an editor with no ward loaded never
    /// shows a stricter (or laxer) posture than the wire's own default.
    fn default() -> Self {
        Self::from_policy(&ReachPolicy::default())
    }
}

impl PolicyEditor {
    /// Load a ward's persisted policy into the editor, parsing every string knob
    /// through its shared fail-closed rule.
    fn from_policy(policy: &ReachPolicy) -> Self {
        let content = policy.content_policy.unwrap_or_default();
        // An absent pillar is the unsupervised-equivalent default: every field
        // empty, i.e. no limit. Stored minutes render back through the same
        // shared formatter the parse side inverts, so what a guardian sees is
        // exactly what they could retype.
        let screen = policy.screen_time.unwrap_or_default();
        Self {
            contact_approval: policy.contact_approval,
            unknown_sender: UnknownSenderMail::from_wire(&policy.unknown_sender_mail),
            federation: policy.federation_contact,
            feed_sources: FeedSources::from_wire(&policy.feed_sources),
            // Absent is the `allow` DEFAULT, not the fail-closed value: the nest
            // reports `None` for a knob sitting at its default, so failing an
            // absent knob closed would show the guardian a policy stricter than
            // the one actually enforced (`fauna_core::format`'s
            // `an_absent_unknown_peer_dm_renders_its_allow_default_…` pins the
            // same rule for the summary line).
            unknown_peer_dm: policy
                .unknown_peer_dm
                .as_deref()
                .map(UnknownPeerDm::from_wire)
                .unwrap_or(UnknownPeerDm::Allow),
            unknown_peer_dm_edited: false,
            content: GUARDIAN_FLOOR_CATEGORIES.map(|c| match content.floor_for(c) {
                // `Unknown` is not a picker option — a floor this build cannot
                // parse renders (and re-saves) at the strict `block` position.
                ContentFloor::Unknown => ContentFloor::FAIL_CLOSED,
                v => v,
            }),
            content_notify: policy.content_notify.unwrap_or(false),
            screen_window_start: screen
                .window_start
                .map(fauna_core::screen_time::format_time_of_day)
                .unwrap_or_default(),
            screen_window_end: screen
                .window_end
                .map(fauna_core::screen_time::format_time_of_day)
                .unwrap_or_default(),
            screen_daily_minutes: screen
                .daily_minutes
                .map(|m| m.to_string())
                .unwrap_or_default(),
        }
    }

    /// The editor's current state as the `fauna.family.policy.update` document,
    /// or the reason it cannot be one.
    ///
    /// All three pillars are `Some(..)` because this editor authors all three.
    /// For screen time that is load-bearing rather than incidental: absent means
    /// "leave unchanged" (`family-safety.md` § Policy-update compatibility), so
    /// an all-empty editor sending absent could never *clear* a limit — the
    /// guardian would have no way to remove a bedtime they set. Sending the
    /// all-`None` default is what makes the empty field mean "unset".
    ///
    /// The screen-time text is parsed and validated here through the very rules
    /// the nest refuses by (`fauna_core::screen_time`), so a refusal (half-set
    /// bounds, `start == end`, an out-of-range budget) reaches the guardian as
    /// its own reason on `error-message`, with no round trip, instead of a
    /// generic transport failure.
    fn to_policy(&self) -> Result<ReachPolicy, &'static str> {
        use fauna_core::screen_time::{parse_daily_minutes, parse_time_of_day};
        let screen_time = fauna_core::screen_time::ScreenTimePolicy {
            window_start: parse_time_of_day(&self.screen_window_start)?,
            window_end: parse_time_of_day(&self.screen_window_end)?,
            daily_minutes: parse_daily_minutes(&self.screen_daily_minutes)?,
        };
        screen_time.validate()?;
        Ok(ReachPolicy {
            contact_approval: self.contact_approval,
            unknown_sender_mail: self.unknown_sender.as_str().to_string(),
            federation_contact: self.federation,
            feed_sources: self.feed_sources.as_str().to_string(),
            content_policy: Some(ContentPolicy {
                nsfw: self.content[0],
                spam: self.content[1],
                phishing: self.content[2],
                commercial: self.content[3],
            }),
            content_notify: Some(self.content_notify),
            screen_time: Some(screen_time),
            // Sent only once the guardian has touched the select — absent means
            // "leave unchanged" (see `unknown_peer_dm_edited`).
            unknown_peer_dm: self
                .unknown_peer_dm_edited
                .then(|| self.unknown_peer_dm.as_str().to_string()),
            ..Default::default()
        })
    }
}

/// The Family page's state, hung off [`App`]. Mirrors the one
/// `fauna.family.status` + `fauna.family.approvals.list` read that fills every
/// section (`family-safety.md` § App surface — "sections render from one
/// status read") rather than inventing a page-local model.
#[derive(Default)]
pub struct FamilyState {
    /// The live WS-RPC channel, installed at the post-auth hook. `None` pre-auth
    /// — every reader degrades gracefully.
    pub nest: Option<Arc<NestClient>>,
    /// The caller's guardian handle, when supervised — drives the supervised
    /// section AND the global `supervised-indicator`.
    pub supervised_by: Option<String>,
    /// The caller's own active policy, when supervised (the read-only summary).
    /// The caller's OWN established band (`family-age-band-summary`), from the
    /// last successful status read; `None` = absent surface.
    pub own_age_band: Option<FamilyAgeBandInfo>,
    pub own_policy: Option<ReachPolicy>,
    /// The caller's OWN cross-device screen-time total for their local day, off
    /// the same status read — folded into `family-policy-summary`. `None`
    /// without a daily budget, which renders no usage line at all
    /// (`family-safety.md` § Screen time).
    pub own_usage_today_minutes: Option<u32>,
    /// The accounts this caller guards — the paint iterates this directly, so a
    /// row index `i` is stable against `elements()` and the scoped queries.
    pub wards: Vec<FamilyWardInfo>,
    /// Every ward's pending approvals; the queue is NOT ward-scoped.
    pub approvals: Vec<FamilyApprovalEntry>,
    /// The supervised caller's OWN pending contact asks, off the same status
    /// read (`family-safety.md` § Child-initiated contact requests → *Ward
    /// transparency*). Lives here, not on the contacts page, because it rides
    /// the one `fauna.family.status` reply this state already owns — the
    /// contacts page reads it back through [`Self::contact_ask_pending`], the
    /// same borrow-the-family-read shape the feed uses for the content floor.
    pub own_contact_requests: Vec<FamilyContactRequestInfo>,
    /// The supervised caller's OWN feed-source asks — pending *and*
    /// approved-but-unredeemed — off the same status read
    /// (`family-safety.md` § Feed-source approvals). Lives here beside
    /// [`Self::own_contact_requests`] for the identical reason: it rides the
    /// one `fauna.family.status` reply this state already owns, and the
    /// bridges page reads it back through [`Self::feed_request_state`].
    pub own_feed_requests: Vec<FamilyFeedRequestInfo>,
    /// Transfer proposals awaiting THIS caller's consent as proposed guardian.
    pub incoming: Vec<FamilyIncomingTransferInfo>,
    /// Which ward the shared editor is loaded for, by actor id. Re-selected
    /// after a refresh if still present, else the first ward (the windows
    /// `LoadAsync` rule, lifted).
    pub selected_ward: Option<Vec<u8>>,
    /// The shared editor's working buffer for [`Self::selected_ward`].
    pub editor: PolicyEditor,
    /// `family-contact-add-input` — a hex actor id (v1 takes no handle).
    pub contact_input: String,
    /// `family-transfer-input` — the proposed guardian's hex actor id.
    pub transfer_input: String,
    /// The graduate confirm step is revealed (`family-graduate-button` clicked).
    /// Reset on every editor load, so a ward switch never carries a live
    /// destructive confirm across.
    pub graduate_revealed: bool,
    /// The nest's advertised capability set, read beside the status whenever
    /// the caller guards someone — what hides a gated feature this nest build
    /// does not carry from `family-policy-feature-limits-row`. `None` until
    /// read (or when the read failed): the section then paints no rows rather
    /// than rows for planes it cannot vouch for.
    pub capabilities: Option<Vec<String>>,
    /// `feature-policy-editor`, open at the guardian tier for
    /// [`Self::selected_ward`] — the draft is the shared `PolicyEditor`'s.
    /// Closed whenever the selection moves to another ward.
    pub feature_editor: Option<fauna_client_features::PolicyEditor>,
    /// `feature-policy-editor-status` — the last write's verdict line.
    pub feature_editor_status: Option<String>,
}

/// The two live states a ward's feed-source ask can be in — the shared
/// definition (`fauna_client_family::ward_asks`), re-exported under its
/// long-standing tui path.
pub use fauna_client_family::FeedRequestState;

impl FamilyState {
    /// Whether this (supervised) account has an ask outstanding for
    /// `peer_actor_id_hex` — what `contact-request-pending` renders from
    /// (`family-safety.md` § Child-initiated contact requests → *Ward
    /// transparency*: "the refused-send surface can render 'asked — waiting for
    /// your guardian' instead of a dead refusal").
    ///
    /// Case-insensitive, because the caller's id is whatever the Find User form
    /// resolved and the wire's is canonical lowercase hex — the same compare
    /// `fauna_core::format::contact_row_blocks_actor` makes for the same reason.
    pub fn contact_ask_pending(&self, peer_actor_id_hex: &str) -> bool {
        // The id compare (decoded, so case-insensitive; a non-hex id matches
        // nothing) is the shared rule every app renders from.
        fauna_client_family::contact_ask_pending(&self.own_contact_requests, peer_actor_id_hex)
    }

    /// The live ask state for one feed-source operation, or `None` when this
    /// account has no live ask for it — what `bridge-source-request-state`
    /// renders, and what decides whether `bridge-source-request-button` is
    /// offered at all (`family-safety.md` § Feed-source approvals).
    ///
    /// Keyed on the whole `(bridge_id, operation, target)` triple because that
    /// is what the grant is scoped to: a `follow` ask for one account must not
    /// light up the row of a different follow on the same bridge, and a `link`
    /// ask (whose `target` is empty by construction) must not match either.
    ///
    /// The nest lists only LIVE rows — an expired pending ask or a lapsed grant
    /// is filtered out there rather than surfaced dead — so a row present here
    /// is always one of the two states this returns, and absence is always
    /// "nothing outstanding", never "something stale".
    pub fn feed_request_state(
        &self,
        bridge_id: &str,
        operation: &str,
        target: &str,
    ) -> Option<FeedRequestState> {
        fauna_client_family::feed_request_state(
            &self.own_feed_requests,
            bridge_id,
            operation,
            target,
        )
    }

    /// The currently-selected ward, if it is still in the loaded list.
    fn selected(&self) -> Option<&FamilyWardInfo> {
        let id = self.selected_ward.as_deref()?;
        self.wards.iter().find(|w| w.actor_id.as_slice() == id)
    }

    /// The guardian host's feature-limits surface for `ward` — the shared
    /// fold of its status entry (`family-safety.md` § App surface → *Feature
    /// limits*). `None` until the capability set is read.
    fn feature_limits(
        &self,
        ward: &FamilyWardInfo,
    ) -> Option<fauna_client_features::AuthoredSurface> {
        self.capabilities
            .as_ref()
            .map(|capabilities| fauna_client_features::ward_surface(ward, capabilities))
    }

    /// Point the editor at `ward` — the tui twin of linux's `load_editor`.
    fn load_editor(&mut self, ward_index: usize) {
        let Some(ward) = self.wards.get(ward_index) else {
            return;
        };
        // A feature editor is one ward's: it must never stay open over another.
        if self.selected_ward.as_deref() != Some(ward.actor_id.as_slice()) {
            self.feature_editor = None;
            self.feature_editor_status = None;
        }
        self.selected_ward = Some(ward.actor_id.to_vec());
        self.editor = PolicyEditor::from_policy(&ward.policy);
        self.graduate_revealed = false;
        self.transfer_input.clear();
    }
}

/// Build the page state at the post-auth hook. Unlike the ungated pages, entering
/// the tab is NOT the only trigger: the gate itself has to run at login (it is
/// what reveals the `family-tab` row at all), so [`spawn_status_check`] fires the
/// same `Op::Refresh` the nav edge does — the `crate::admin` gate-check shape.
pub fn init(nest: Arc<NestClient>) -> FamilyState {
    FamilyState {
        nest: Some(nest),
        ..FamilyState::default()
    }
}

/// Fire-and-forget the post-auth `fauna.family.status` read whose result reveals
/// (or, fail-closed, hides) the gated `family-tab` row and the global
/// `supervised-indicator`. No driver ack to honour on this path — the outcome
/// lands through the channel and folds via [`apply_outcome`], exactly as
/// `admin::spawn_gate_check` does for `am_i_admin`.
///
/// `session_generation` is the caller's `App::session_generation`, captured
/// synchronously and carried on [`DataMessage::Page`]'s envelope — the identity
/// seam that drops a result landing after an actor change (that variant's own
/// doc comment).
pub fn spawn_status_check(
    state: &FamilyState,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) {
    let Some(nest) = state.nest.clone() else {
        return;
    };
    let tx = tx.clone();
    tokio::spawn(async move {
        let outcome = Op::Refresh { nest }.run().await;
        let _ = tx.send(UiMessage::Data(DataMessage::Page(
            session_generation,
            PageOutcome::Family(outcome),
        )));
    });
}

/// The refetch entering this tab implies — the page's leg of the one nav-edge
/// hook (`crate::app::on_nav_enter`). Returns the op; the caller runs it (the
/// agent awaits, the keyboard spawns), never a fire-and-forget spawn that could
/// race a same-visit mutation. The guardian's queue is nest-authoritative and a
/// knock/hold can land while another page is up, so entering always re-reads.
pub fn nav_enter_op(state: &FamilyState) -> Option<Op> {
    Some(Op::Refresh {
        nest: state.nest.clone()?,
    })
}

// ── Field access ─────────────────────────────────────────────────────────────

/// A Family-page editable field. All are local buffers committed by an explicit
/// button, never per keystroke — the three screen-time ones by
/// `family-policy-save-button` along with the rest of the editor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FamilyField {
    /// `family-contact-add-input` — the hex actor id to pre-approve.
    ContactAdd,
    /// `family-transfer-input` — the proposed new guardian's hex actor id.
    Transfer,
    /// `family-policy-screen-window-start-input` — typed `HH:MM`.
    ScreenWindowStart,
    /// `family-policy-screen-window-end-input` — typed `HH:MM`.
    ScreenWindowEnd,
    /// `family-policy-screen-daily-minutes-input` — whole minutes.
    ScreenDailyMinutes,
    /// `feature-policy-editor-cell-input[cell]` — one bound of the open
    /// guardian-tier feature editor, owned by the shared `PolicyEditor` and
    /// committed by that editor's own save button, not the policy form's.
    FeatureLimitCell { cell: usize },
}

pub fn field(state: &FamilyState, field: &FamilyField) -> String {
    match field {
        FamilyField::ContactAdd => state.contact_input.clone(),
        FamilyField::Transfer => state.transfer_input.clone(),
        FamilyField::ScreenWindowStart => state.editor.screen_window_start.clone(),
        FamilyField::ScreenWindowEnd => state.editor.screen_window_end.clone(),
        FamilyField::ScreenDailyMinutes => state.editor.screen_daily_minutes.clone(),
        FamilyField::FeatureLimitCell { cell } => state
            .feature_editor
            .as_ref()
            .map(|editor| editor.cell_text(*cell).to_string())
            .unwrap_or_default(),
    }
}

pub fn set_field(state: &mut FamilyState, field: FamilyField, value: String) {
    match field {
        FamilyField::ContactAdd => state.contact_input = value,
        FamilyField::Transfer => state.transfer_input = value,
        FamilyField::ScreenWindowStart => state.editor.screen_window_start = value,
        FamilyField::ScreenWindowEnd => state.editor.screen_window_end = value,
        FamilyField::ScreenDailyMinutes => state.editor.screen_daily_minutes = value,
        FamilyField::FeatureLimitCell { cell } => {
            if let Some(editor) = state.feature_editor.as_mut() {
                editor.set_cell(cell, value);
            }
        }
    }
}

// ── Gestures ─────────────────────────────────────────────────────────────────

/// A gesture on the Family page. Each maps onto one `fauna.family.*` call or one
/// local editor-buffer change — never a navigation decision of its own.
#[derive(Debug, Clone)]
pub enum Action {
    /// `family-ward-item[i]` — load that ward's policy into the shared editor.
    /// Purely local: the ward's policy already rode the last status read.
    SelectWard { index: usize },
    /// `family-policy-contact-approval-toggle` — local; `value` is what the tap
    /// should PRODUCE, so a click is idempotent from a driver's side (the
    /// cross-page toggle convention).
    SetContactApproval(bool),
    /// `family-policy-federation-toggle` — local.
    SetFederation(bool),
    /// `family-policy-content-notify-toggle` — local.
    SetContentNotify(bool),
    /// `family-policy-unknown-sender-select` — carries the **localized label**
    /// the driver picked (contract 2); mapped back to its wire value here.
    SetUnknownSender(String),
    /// `family-policy-feed-sources-select` — same label convention.
    SetFeedSources(String),
    /// `family-policy-unknown-peer-dm-select` — same label convention. Also
    /// marks the knob edited, which is what lets it ride the save at all
    /// (`PolicyEditor::unknown_peer_dm_edited`).
    SetUnknownPeerDm(String),
    /// `family-policy-content-{nsfw,spam,phishing,commercial}-select` — the
    /// category is the [`GUARDIAN_FLOOR_CATEGORIES`] index (a `usize`, so
    /// [`SelectTarget`] stays `Copy`), the value the localized label.
    SetContentFloor { category: usize, label: String },
    /// `family-policy-save-button` — `fauna.family.policy.update`, then refresh.
    SavePolicy,
    /// `family-contact-add-button` — `fauna.family.contact.add` from the hex
    /// buffer; a parse failure surfaces on `error-message` with no round trip.
    AddContact,
    /// `family-transfer-button` — propose a new guardian (pending until accepted).
    Transfer,
    /// `family-transfer-cancel-button` — withdraw the selected ward's proposal.
    TransferCancel,
    /// `family-graduate-button` — reveal the confirm step. Purely local.
    RevealGraduate,
    /// `family-graduate-confirm-button` — `fauna.family.graduate`, then refresh.
    ConfirmGraduate,
    /// `family-approval-{approve,deny}-button[i]` — decide one queue row. Carried
    /// by index into the live snapshot, like the row-scoped selects; a stale
    /// index (the queue moved under a slow click) resolves to nothing and drops
    /// the gesture rather than deciding the wrong item.
    Decide { index: usize, approve: bool },
    /// `family-incoming-transfer-{accept,decline}-button[i]` — the proposed
    /// guardian's answer, same index convention.
    DecideIncoming { index: usize, accept: bool },
    /// `family-device-mark-toggle[i]` — set/clear the guardian-enrolled marker
    /// on one of the selected ward's devices (`family-safety.md` § Full
    /// visibility for young children, Slice F). Carries the ward actor id +
    /// `device_id` **by value**, never a row index: a concurrent refetch that
    /// reorders the ward's devices must never route a flip at the wrong
    /// device — this flag is a security promise ("the child cannot remove the
    /// guardian's device"). Not batched behind `SavePolicy`: `device.mark` is
    /// its own per-device RPC, so the flip dispatches immediately.
    DeviceMark {
        ward: Vec<u8>,
        device_id: String,
        marked: bool,
    },
    /// `family-blocked-peer-allow-button` — flip a guardian's earlier DM denial
    /// back to `allow` (`family-safety.md` § The bridge-DM gate → *The un-deny
    /// surface*). Rides the shared `FamilyClient::allow_blocked_dm_peer` (the
    /// ordinary approving `dm_hold` decide), which is **idempotent and not queue-scoped**: it works long after the
    /// hold row that prompted the deny is gone, which is exactly what makes an
    /// un-deny possible at all.
    AllowBlockedPeer {
        ward: Vec<u8>,
        bridge_id: String,
        peer_id: String,
    },
    /// `family-policy-feature-limits-edit-button[i]` — open the shared editor
    /// at the guardian tier for the selected ward, over that member's authored
    /// document. Carries the member's stable key. Purely local: the seed rode
    /// the last status read.
    OpenFeatureLimitEditor(String),
    /// `feature-policy-editor-on-radio` (`true`) / `-off-radio` (`false`).
    FeatureLimitOn(bool),
    /// `feature-policy-editor-cancel-button` — close, touching nothing.
    CancelFeatureLimitEditor,
    /// `feature-policy-editor-save-button` — the ward's freshly-read policy
    /// with its `features` sub-document replaced, then refresh.
    SaveFeatureLimit,
    /// `feature-policy-editor-remove-button` — the same write with the member
    /// removed from the sub-document.
    RemoveFeatureLimit,
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`), exhaustive with no fallback arm
    /// so a new variant cannot skip the question.
    ///
    /// The page splits cleanly, and the split is the guardian/ward asymmetry
    /// rather than anything about this screen: **editing** a ward's reach
    /// policy is writing the guardian's own document
    /// (`fauna.family.policy.update`, `OfflineSafe`) and pre-approving a
    /// contact or deciding a queued ask are replayable intents
    /// (`OfflineQueued`), so the editor stays live with no nest — while every
    /// gesture that **re-points guardianship** is `OnlineOnly`, because the
    /// nest arbitrates who a ward answers to and a second device must not be
    /// able to compose a conflicting answer offline (`family-safety.md`
    /// § Graduation & transfer).
    ///
    /// [`Self::DecideIncoming`] is the closed case of caution 1 — accept and
    /// decline are two different kinds, and the discriminant already rides the
    /// action, so each arm answers its own (`admin::Action::IssueDnsCert`, the
    /// worked example). [`Self::Decide`] needs no such split: approve and deny
    /// are one kind carrying a boolean.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // ── Editor-local: no call is issued until `SavePolicy` ──────────
            // The ward's policy already rode the last `fauna.family.status`
            // read, so loading it into the editor and every field edit are
            // pure buffer writes.
            Action::SelectWard { .. }
            | Action::SetContactApproval(_)
            | Action::SetFederation(_)
            | Action::SetContentNotify(_)
            | Action::SetUnknownSender(_)
            | Action::SetFeedSources(_)
            | Action::SetUnknownPeerDm(_)
            | Action::SetContentFloor { .. }
            // Revealing the graduate confirm step is a view-local flip; the
            // call is on `ConfirmGraduate`.
            | Action::RevealGraduate
            // Opening the feature editor seeds from the last status read; the
            // radios and cancel are draft-local.
            | Action::OpenFeatureLimitEditor(_)
            | Action::FeatureLimitOn(_)
            | Action::CancelFeatureLimitEditor => None,

            // ── The guardian's own documents + replayable intents ───────────
            Action::SavePolicy => Some("fauna.family.policy.update"),
            // The guardian tier mints no kind: a feature limit rides the same
            // policy write as the reach knobs (`family-safety.md` § Wire &
            // data shape), so these declare the kind they actually issue. It
            // is `OfflineSafe`, which leaves the two buttons live with no nest
            // — unlike the admin and self hosts, whose kinds are `OnlineOnly`;
            // a save with no nest then fails at its fresh status read and
            // lands on `error-message`, writing nothing.
            Action::SaveFeatureLimit | Action::RemoveFeatureLimit => {
                Some("fauna.family.policy.update")
            }
            Action::AddContact => Some("fauna.family.contact.add"),
            Action::Decide { .. } => Some("fauna.family.approvals.decide"),
            Action::DeviceMark { .. } => Some("fauna.family.device.mark"),
            Action::AllowBlockedPeer { .. } => Some("fauna.family.approvals.decide"),

            // ── Re-pointing guardianship: the nest arbitrates ───────────────
            Action::Transfer => Some("fauna.family.transfer"),
            Action::TransferCancel => Some("fauna.family.transfer.cancel"),
            Action::ConfirmGraduate => Some("fauna.family.graduate"),
            Action::DecideIncoming { accept, .. } => Some(if *accept {
                "fauna.family.transfer.accept"
            } else {
                "fauna.family.transfer.decline"
            }),
        }
    }
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    match action {
        // ── local editor state ──
        Action::SelectWard { index } => {
            app.family.load_editor(index);
            None
        }
        Action::SetContactApproval(value) => {
            app.family.editor.contact_approval = value;
            None
        }
        Action::SetFederation(value) => {
            app.family.editor.federation = value;
            None
        }
        Action::SetContentNotify(value) => {
            app.family.editor.content_notify = value;
            None
        }
        Action::SetUnknownSender(label) => {
            app.family.editor.unknown_sender = unknown_sender_for_label(&label);
            None
        }
        Action::SetFeedSources(label) => {
            app.family.editor.feed_sources = feed_sources_for_label(&label);
            None
        }
        Action::SetUnknownPeerDm(label) => {
            app.family.editor.unknown_peer_dm = unknown_peer_dm_for_label(&label);
            app.family.editor.unknown_peer_dm_edited = true;
            None
        }
        Action::SetContentFloor { category, label } => {
            if let Some(slot) = app.family.editor.content.get_mut(category) {
                *slot = content_floor_for_label(&label);
            }
            None
        }
        Action::RevealGraduate => {
            app.family.graduate_revealed = true;
            None
        }
        Action::OpenFeatureLimitEditor(feature) => {
            app.family.feature_editor = app.family.selected().and_then(|ward| {
                let tier = fauna_client_features::AuthoringTier::Guardian {
                    ward: ward.actor_id.as_slice().try_into().ok()?,
                };
                app.family.feature_limits(ward)?.editor(tier, &feature)
            });
            app.family.feature_editor_status = None;
            None
        }
        Action::FeatureLimitOn(on) => {
            if let Some(editor) = app.family.feature_editor.as_mut() {
                editor.set_on(on);
            }
            None
        }
        Action::CancelFeatureLimitEditor => {
            app.family.feature_editor = None;
            app.family.feature_editor_status = None;
            None
        }

        // ── network halves ──
        // The draft is parsed HERE, synchronously, through the shared seam: a
        // cell it cannot read goes to `error-message` and nothing is dispatched
        // (the admin host's shape; `dynamic-features.md` § Authoring surfaces,
        // *Refusals and failure*).
        Action::SaveFeatureLimit | Action::RemoveFeatureLimit => {
            let remove = matches!(action, Action::RemoveFeatureLimit);
            let editor = app.family.feature_editor.clone()?;
            if !remove && let Err(reason) = editor.draft() {
                app.errors
                    .insert(Page::Family, reason.resolve(fauna_i18n::strings::lookup));
                return None;
            }
            app.errors.remove(&Page::Family);
            Some(Op::WriteFeatureLimit {
                nest: app.family.nest.clone()?,
                editor: Box::new(editor),
                remove,
            })
        }
        Action::SavePolicy => {
            let nest = app.family.nest.clone()?;
            let ward = app.family.selected()?.actor_id.to_vec();
            // A screen-time entry the policy cannot hold is refused *here*, on
            // the shared rule the nest would refuse it by, and surfaced on
            // `error-message` with no round trip — so the guardian reads the
            // reason instead of a generic RPC failure, and the rest of the
            // editor is left untouched (linux's `submit_policy`, lifted).
            let policy = match app.family.editor.to_policy() {
                Ok(policy) => policy,
                Err(reason) => {
                    app.errors.insert(Page::Family, reason.to_string());
                    return None;
                }
            };
            app.errors.remove(&Page::Family);
            Some(Op::PolicyUpdate {
                nest,
                ward,
                policy: Box::new(policy),
            })
        }
        Action::AddContact => {
            let nest = app.family.nest.clone()?;
            let ward = app.family.selected()?.actor_id.to_vec();
            let input = app.family.contact_input.trim().to_string();
            if input.is_empty() {
                return None;
            }
            // v1 takes a hex actor id, no handle resolution (the windows seam's
            // rule, lifted): a parse failure is a client-side refusal, surfaced
            // on the page error rather than sent to the nest.
            let Ok(peer) = fauna_core::identity::ActorId::from_hex(&input).map(|a| a.0.to_vec())
            else {
                app.errors
                    .insert(Page::Family, t::CONTACT_ADD_INVALID_ACTOR_ID.to_string());
                return None;
            };
            app.errors.remove(&Page::Family);
            app.family.contact_input.clear();
            Some(Op::ContactAdd { nest, ward, peer })
        }
        Action::Transfer => {
            let nest = app.family.nest.clone()?;
            let ward = app.family.selected()?.actor_id.to_vec();
            let input = app.family.transfer_input.trim().to_string();
            if input.is_empty() {
                return None;
            }
            let Ok(target) = fauna_core::identity::ActorId::from_hex(&input).map(|a| a.0.to_vec())
            else {
                app.errors
                    .insert(Page::Family, t::CONTACT_ADD_INVALID_ACTOR_ID.to_string());
                return None;
            };
            app.errors.remove(&Page::Family);
            Some(Op::Transfer { nest, ward, target })
        }
        Action::TransferCancel => Some(Op::TransferCancel {
            nest: app.family.nest.clone()?,
            ward: app.family.selected()?.actor_id.to_vec(),
        }),
        Action::ConfirmGraduate => {
            let nest = app.family.nest.clone()?;
            let ward = app.family.selected()?.actor_id.to_vec();
            app.family.graduate_revealed = false;
            Some(Op::Graduate { nest, ward })
        }
        Action::Decide { index, approve } => {
            let nest = app.family.nest.clone()?;
            let entry = app.family.approvals.get(index)?.clone();
            Some(Op::Decide {
                nest,
                entry: Box::new(entry),
                approve,
            })
        }
        Action::DecideIncoming { index, accept } => {
            let nest = app.family.nest.clone()?;
            let ward = app.family.incoming.get(index)?.supervised_actor_id.to_vec();
            Some(if accept {
                Op::TransferAccept { nest, ward }
            } else {
                Op::TransferDecline { nest, ward }
            })
        }
        Action::AllowBlockedPeer {
            ward,
            bridge_id,
            peer_id,
        } => Some(Op::AllowBlockedPeer {
            nest: app.family.nest.clone()?,
            ward,
            bridge_id,
            peer_id,
        }),
        Action::DeviceMark {
            ward,
            device_id,
            marked,
        } => Some(Op::DeviceMark {
            nest: app.family.nest.clone()?,
            ward,
            device_id,
            marked,
        }),
    }
}

// ── Network half ─────────────────────────────────────────────────────────────

/// The network half of a Family gesture — owns only `Arc`s + owned data, so it
/// can be awaited on the agent's path or spawned on the keyboard's.
pub enum Op {
    /// `fauna.family.status` + `fauna.family.approvals.list` — the post-auth
    /// gate check, the nav edge, and the tail of every mutation.
    Refresh { nest: Arc<NestClient> },
    /// Boxed policy: the reach document inlines two optional pillars, and an
    /// unboxed variant would balloon this enum past `large_enum_variant`.
    PolicyUpdate {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
        policy: Box<ReachPolicy>,
    },
    /// Boxed for the same reason: an approval entry carries eleven owned fields,
    /// and each kind names its item with a different one — so the whole entry
    /// travels and `run` passes every key through (`family-safety.md` § Reach
    /// approvals), rather than this page deciding which key a kind uses.
    Decide {
        nest: Arc<NestClient>,
        entry: Box<FamilyApprovalEntry>,
        approve: bool,
    },
    ContactAdd {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
        peer: Vec<u8>,
    },
    Graduate {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
    },
    Transfer {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
        target: Vec<u8>,
    },
    TransferCancel {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
    },
    TransferAccept {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
    },
    TransferDecline {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
    },
    DeviceMark {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
        device_id: String,
        marked: bool,
    },
    /// The un-deny — the shared `FamilyClient::allow_blocked_dm_peer` for one
    /// `(bridge, peer)` on one ward.
    AllowBlockedPeer {
        nest: Arc<NestClient>,
        ward: Vec<u8>,
        bridge_id: String,
        peer_id: String,
    },
    /// A guardian-tier feature limit, saved or removed through the shared seam
    /// (`FeaturesClient::save` / `remove`): re-read the ward's policy, replace
    /// its `features` sub-document, write, re-read — then refresh the page, so
    /// the row above the editor shows what the nest now stores.
    WriteFeatureLimit {
        nest: Arc<NestClient>,
        editor: Box<fauna_client_features::PolicyEditor>,
        remove: bool,
    },
    /// `fauna.family.usage_report` — the ward's own foreground heartbeat
    /// (`family-safety.md` § Screen time). Unlike every op above it is NOT a
    /// gesture: nothing on any page dispatches it, the one-minute tick does,
    /// and it never refreshes the page — its reply is the day's cross-device
    /// total, which goes to [`crate::screen_lock`] rather than to the paint.
    UsageReport {
        nest: Arc<NestClient>,
        minutes: u32,
        utc_offset_minutes: i32,
    },
    /// `fauna.family.notify_report` — the ward's batched Guardian Notify counts
    /// (`family-safety.md` § Guardian Notify). Like [`Self::UsageReport`] it is
    /// not a gesture: the one-minute tick dispatches it, it never refreshes the
    /// page, and it carries **category + count only, never a content id**.
    NotifyReport {
        nest: Arc<NestClient>,
        entries: Vec<FamilyContentNotice>,
        utc_offset_minutes: i32,
    },
}

/// The one read that fills every section — the linux `FamilyView`, lifted.
#[derive(Debug, Default)]
pub struct FamilyView {
    pub supervised_by: Option<String>,
    /// The caller's OWN established band (`FamilyStatusReply.age_band`) —
    /// `family-age-band-summary`; `None` = no band row (a band-less admission,
    /// or unsupervised), and the surface is then absent.
    pub age_band: Option<FamilyAgeBandInfo>,
    /// The at-rest supervision snapshot derived from this same successful read
    /// (`family-safety.md` § Content policy, the unfetched-policy ruling clause
    /// 2). Carried on the view rather than re-derived in `apply_outcome`
    /// because the raw reply — and in particular the guardian's **actor id**,
    /// which [`Self::supervised_by`] reduces to a handle — exists only at the
    /// read site. `Default` (the unsupervised snapshot) in fixtures.
    pub snapshot: SupervisionSnapshot,
    pub policy: Option<ReachPolicy>,
    /// The caller's OWN screen-time total for their local day (§ Screen time).
    pub usage_today_minutes: Option<u32>,
    pub wards: Vec<FamilyWardInfo>,
    pub approvals: Vec<FamilyApprovalEntry>,
    pub incoming: Vec<FamilyIncomingTransferInfo>,
    /// The supervised caller's OWN pending contact asks (`family-safety.md`
    /// § Child-initiated contact requests → *Ward transparency*). Read here
    /// rather than on the contacts page because it rides the same
    /// `fauna.family.status` reply as everything else on this page — the
    /// contacts page reads it back off [`FamilyState`], the way the feed reads
    /// the content floor.
    pub contact_requests: Vec<FamilyContactRequestInfo>,
    /// The supervised caller's OWN feed-source asks, pending and
    /// approved-but-unredeemed (`family-safety.md` § Feed-source approvals).
    /// Rides the same reply, read back off [`FamilyState`] by the bridges page.
    pub feed_requests: Vec<FamilyFeedRequestInfo>,
    /// The nest's capability set ([`FamilyState::capabilities`]) — read only
    /// when the caller guards someone, `None` otherwise or when it failed.
    pub capabilities: Option<Vec<String>>,
}

/// What an op resolved to.
#[derive(Debug)]
pub enum Outcome {
    /// A fresh whole-page view. Every successful op ends here — the page is
    /// non-optimistic by construction, so what paints is always what the nest
    /// persisted. Boxed like `admin::Outcome::MailSnapshot`, so the
    /// `PageOutcome`/`DataMessage` chain it rides stays small.
    Loaded(Box<FamilyView>),
    /// A guardian-tier feature limit landed: the fresh whole-page view, the
    /// editor re-seeded from what the nest now stores (`None` when the read no
    /// longer carries the member) and its verdict line.
    FeatureLimitWritten {
        view: Box<FamilyView>,
        editor: Box<Option<fauna_client_features::PolicyEditor>>,
        status: String,
    },
    /// A transport or authorization failure — lands on `error-message`.
    Failed(String),
    /// A `fauna.family.usage_report` reply: the nest's stamped local-day bucket
    /// and that day's cross-device total. Folds into [`crate::screen_lock`],
    /// never into the page — it repaints no Family element.
    UsageReported { day: i64, day_total_minutes: u32 },
    /// The heartbeat did not land. The engine re-credits its in-flight minutes,
    /// so a dropped report cannot quietly forgive a ward's screen time.
    UsageReportFailed,
    /// A `fauna.family.notify_report` round-trip finished. It folds **nowhere**:
    /// the accumulator already cleared its pending counts when the batch was
    /// taken, and unlike the usage heartbeat a lost batch is deliberately not
    /// re-credited — § Guardian Notify states the count's trust bound up front
    /// ("the count comes from the ward's conforming client — a modified client
    /// under-reports"), so an occasional dropped batch is inside the design's
    /// own honesty, whereas retaining counts across a failure would let one
    /// unreachable hour inflate the next report's category totals. The variant
    /// exists so this op rides the same dispatch chain as every other.
    NotifyReported,
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh { nest } => refresh(nest).await,
            Op::WriteFeatureLimit {
                nest,
                editor,
                remove,
            } => {
                let client = fauna_client_features::FeaturesClient::new(Arc::clone(&nest));
                let written = if remove {
                    client.remove(&editor).await
                } else {
                    client.save(&editor).await
                };
                let saved = match written {
                    Ok(saved) => saved,
                    Err(e) => {
                        return Outcome::Failed(e.text().resolve(fauna_i18n::strings::lookup));
                    }
                };
                match refresh(nest).await {
                    Outcome::Loaded(view) => Outcome::FeatureLimitWritten {
                        view,
                        editor: Box::new(editor.reseeded(&saved.reply)),
                        status: saved.status.resolve(fauna_i18n::strings::lookup),
                    },
                    other => other,
                }
            }
            Op::PolicyUpdate { nest, ward, policy } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.policy_update(ward, *policy).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::Decide {
                nest,
                entry,
                approve,
            } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                // Each kind names its item with a different key, and the entry
                // carries all of them (`FamilyClient::approvals_decide` docs) —
                // pass them straight through rather than re-deriving the kind →
                // key map per client.
                let result = client
                    .approvals_decide(
                        entry.supervised_actor_id.to_vec(),
                        entry.kind.clone(),
                        entry.peer_actor_id.to_vec(),
                        entry.message_id.to_vec(),
                        entry.bridge_id.clone(),
                        entry.operation.clone(),
                        entry.target.clone(),
                        entry.peer_address.clone(),
                        approve,
                    )
                    .await;
                match result {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::ContactAdd { nest, ward, peer } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.contact_add(ward, peer).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::Graduate { nest, ward } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.graduate(ward).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::Transfer { nest, ward, target } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.transfer(ward, target).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::TransferCancel { nest, ward } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.transfer_cancel(ward).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::TransferAccept { nest, ward } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.transfer_accept(ward).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::TransferDecline { nest, ward } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.transfer_decline(ward).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::DeviceMark {
                nest,
                ward,
                device_id,
                marked,
            } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                match client.device_mark(ward, device_id, marked).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::AllowBlockedPeer {
                nest,
                ward,
                bridge_id,
                peer_id,
            } => {
                let client = FamilyClient::new(Arc::clone(&nest));
                // The shared un-deny owns the wire shape (the approving
                // `dm_hold` decide with the peer riding `peer_address`); this
                // row only hands it its own `(bridge_id, peer_id)` pair.
                let result = client.allow_blocked_dm_peer(ward, bridge_id, peer_id).await;
                match result {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::UsageReport {
                nest,
                minutes,
                utc_offset_minutes,
            } => {
                let client = FamilyClient::new(nest);
                match client.usage_report(minutes, utc_offset_minutes).await {
                    Ok(reply) => {
                        // Logged on the EVENT, not the paint (observability.md).
                        // A ward's accounting quietly reporting zero forever is
                        // invisible from the UI — the readout just sits at 0,
                        // which reads as "not used today" rather than "broken".
                        tracing::info!(
                            minutes,
                            utc_offset_minutes,
                            day = reply.day,
                            day_total_minutes = reply.day_total_minutes,
                            "fauna.family.usage_report landed"
                        );
                        Outcome::UsageReported {
                            day: reply.day,
                            day_total_minutes: reply.day_total_minutes,
                        }
                    }
                    // Deliberately NOT `Outcome::Failed`: a heartbeat is
                    // best-effort telemetry the ward never asked for, so a nest
                    // blip must not paint an error on whatever page they are
                    // reading. The engine re-credits the minutes instead, and
                    // the next tick retries.
                    // WARN rather than debug: the minutes are re-credited and
                    // the next tick retries, so this is recoverable — but a ward
                    // whose reports never land has silently stopped being
                    // accounted for, and the only UI symptom is a readout that
                    // sits at zero, which reads as "not used today".
                    Err(e) => {
                        tracing::warn!("fauna.family.usage_report failed (retried next tick): {e}");
                        Outcome::UsageReportFailed
                    }
                }
            }
            Op::NotifyReport {
                nest,
                entries,
                utc_offset_minutes,
            } => {
                let client = FamilyClient::new(nest);
                // Logged on the EVENT (observability.md), by category count only —
                // never a content id, which is the whole point of Notify. A batch
                // that never reports is invisible from the guardian's surface: it
                // just reads zero, which looks like "nothing was flagged".
                let categories = entries.len();
                match client.notify_report(entries, utc_offset_minutes).await {
                    Ok(()) => {
                        tracing::info!(
                            categories,
                            utc_offset_minutes,
                            "fauna.family.notify_report landed"
                        );
                    }
                    // Best-effort like the heartbeat, and for the same reason: a
                    // ward never asked for this report, so a nest blip must not
                    // paint an error over whatever page they are reading.
                    Err(e) => {
                        tracing::warn!("fauna.family.notify_report failed (batch dropped): {e}");
                    }
                }
                Outcome::NotifyReported
            }
        }
    }
}

/// `fauna.family.status` (both roles) + the guardian's
/// `fauna.family.approvals.list`. The queue read is unconditional: a
/// supervised-only account gets an empty list, which keeps one code path.
async fn refresh(nest: Arc<NestClient>) -> Outcome {
    let client = FamilyClient::new(Arc::clone(&nest));
    let status = match client.status().await {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let approvals = match client.approvals_list().await {
        Ok(r) => r.approvals,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    // Clause 2 of the unfetched-policy ruling, derived HERE — the one point
    // where the raw reply still exists — and by the SHARED fold, so tui cannot
    // drift from the other six apps on what a snapshot holds or on the
    // graduation gate inside it. Both early returns above are `Outcome::Failed`,
    // so a snapshot can only ever be built from a read that actually succeeded,
    // which is clause 1 satisfied by construction rather than by discipline.
    let snapshot = SupervisionSnapshot::from_status(&status);
    // The guardian host's feature-limits rows hide a member this nest build
    // does not carry, which only the capability set can say. Read it only for
    // a caller who guards someone; a failure leaves the rows unpainted rather
    // than failing the page the reach knobs live on.
    let capabilities = if status.wards.is_empty() {
        None
    } else {
        fauna_client_features::FeaturesClient::new(Arc::clone(&nest))
            .node_capabilities()
            .await
            .ok()
    };
    Outcome::Loaded(Box::new(FamilyView {
        capabilities,
        supervised_by: status.supervised_by.map(|g| g.handle),
        age_band: status.age_band,
        snapshot,
        policy: status.policy,
        usage_today_minutes: status.usage_today_minutes,
        wards: status.wards,
        approvals,
        incoming: status.incoming_transfers,
        contact_requests: status.contact_requests,
        feed_requests: status.feed_requests,
    }))
}

/// Persist the last-known supervision snapshot for the signed-in account
/// (`family-safety.md` § Content policy clause 2).
///
/// A no-op with no session — there is no account to key the slot by, and the
/// only caller runs off an authenticated read anyway.
pub(crate) fn persist_supervision_snapshot(app: &App, snapshot: &SupervisionSnapshot) {
    let Some(actor_id) = app.session.as_ref().map(|s| s.actor_id.clone()) else {
        return;
    };
    crate::session::registry(app).set_supervision_snapshot_json(&actor_id, &snapshot.to_json());
}

/// Restore the last-known supervision snapshot into the enforcing surfaces, at
/// launch, **ahead of** the first `fauna.family.status` read
/// (`family-safety.md` § Content policy clause 2).
///
/// Without this a supervised ward who launches offline renders unsupervised
/// until a read lands — and because § Screen time is enforced by pure
/// client-local clock, that made airplane mode a bedtime-lock bypass.
///
/// Three deliberate bounds:
///
/// * **It only ever restores; it never invents.** An absent or malformed slot
///   leaves every surface exactly as it was — the same "no information" a
///   failed read yields under clause 1.
/// * **`own_policy` is deliberately NOT restored.** The snapshot carries only
///   the three client-enforced pillars, never the reach knobs (which the nest
///   enforces and no client decision depends on). Synthesizing a `ReachPolicy`
///   from it would render *default* reach knobs in the ward's policy summary —
///   a lie about what their guardian set. The summary's own
///   `unwrap_or_default()` renders the unsupervised-equivalent line until the
///   read lands, and the accurate knobs arrive with it.
/// * **The `family-tab` gate opens when the snapshot is supervised.** § Screen
///   time requires the Family page stay reachable read-only while the lock is
///   up ("the ward must always be able to see who supervises them and what the
///   policy is"), so restoring a lock without its explanation surface would
///   strand the ward behind a lock screen naming a page they cannot open.
pub(crate) fn restore_supervision_snapshot(app: &mut App, actor_id: &str) {
    let Some(snapshot) = crate::session::registry(app)
        .supervision_snapshot_json(actor_id)
        .as_deref()
        .and_then(SupervisionSnapshot::from_json)
    else {
        return;
    };
    let guardian = snapshot.supervised_by.as_ref().map(|g| g.handle.clone());
    // `usage_today_minutes` is deliberately `None`: the day's cross-device
    // total is nest-accounted and cannot be known offline. The window half of
    // the policy still binds (pure local clock); the budget half resumes
    // counting at the first heartbeat reply, which is the same state a fresh
    // launch has always had.
    app.screen_lock
        .set_ward_screen_time(snapshot.screen_time, guardian.clone(), None);
    app.content_policy
        .set_ward_content_policy(snapshot.content_policy);
    app.content_policy
        .set_ward_content_notify(snapshot.content_notify);
    if snapshot.is_supervised() {
        app.family.supervised_by = guardian;
        app.has_family = true;
    }
}

/// Fold an op's result back into the page. One function for both dispatch paths,
/// so they cannot disagree about what an outcome means.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Loaded(view) => {
            let FamilyView {
                supervised_by,
                age_band,
                snapshot,
                policy,
                usage_today_minutes,
                wards,
                approvals,
                incoming,
                contact_requests,
                feed_requests,
                capabilities,
            } = *view;
            app.family.capabilities = capabilities;
            // Clause 2: this successful read is the moment to remember. Persist
            // BEFORE folding into the in-memory surfaces below, so a crash
            // between the two loses nothing a following launch needed — the
            // in-memory halves are rebuilt from this same snapshot anyway.
            persist_supervision_snapshot(app, &snapshot);
            // This page's own `fauna.family.status` read is the freshest view of
            // the ward's OWN policy anywhere in the app, so it also refreshes
            // the global `screen-time-lock` inputs (`family-safety.md` § Screen
            // time). Without this the lock would only ever see the policy as it
            // stood at login, and a guardian's edit would not bind until the
            // ward restarted — while the very page the ward is told to visit had
            // just read the new one.
            app.screen_lock.set_ward_screen_time(
                policy.as_ref().and_then(|p| p.screen_time),
                supervised_by.clone(),
                usage_today_minutes,
            );
            // Same read, same reason, for the content pillar (`family-safety.md`
            // § Content policy, § Guardian Notify): the guardian's floor and the
            // `content_notify` knob bind on the ward's next status read rather
            // than needing a restart.
            //
            // Gated on `supervised_by` — NOT merely on the policy being present.
            // The floor is only legitimate while a guardianship exists, so a
            // graduated ward whose last read still carried a policy document must
            // not keep having their feed censored by a guardian they no longer
            // have. This is the same guard `screen_lock` applies to the lock.
            let supervised = supervised_by.is_some();
            app.content_policy.set_ward_content_policy(
                policy
                    .as_ref()
                    .filter(|_| supervised)
                    .and_then(|p| p.content_policy),
            );
            app.content_policy.set_ward_content_notify(
                supervised && policy.as_ref().and_then(|p| p.content_notify) == Some(true),
            );
            // The `family-tab` gate: ANY relationship — supervised, guarding, or
            // named on an incoming proposal (`family-safety.md` § Graduation &
            // transfer → Visibility: a proposed guardian with no other family
            // relationship must still reach the prompt).
            app.has_family = supervised_by.is_some() || !wards.is_empty() || !incoming.is_empty();
            app.family.supervised_by = supervised_by;
            app.family.own_age_band = age_band;
            app.family.own_policy = policy;
            app.family.own_usage_today_minutes = usage_today_minutes;
            app.family.wards = wards;
            app.family.approvals = approvals;
            app.family.incoming = incoming;
            // Gated on `supervised_by` for the same reason the content floor and
            // the screen lock are: a graduated account has no guardian to be
            // waiting on, so a stale ask from the last read must not keep
            // showing "asked — waiting" on a contacts page that now sends
            // freely. (The nest drops the rows at graduation too; this is the
            // client half of the same rule.)
            app.family.own_contact_requests = if supervised {
                contact_requests
            } else {
                Vec::new()
            };
            // Same `supervised` gate, same reason: a graduated account has no
            // guardian to be waiting on, so a stale grant must not keep offering
            // a "try again" prompt on a bridges page that now links freely.
            app.family.own_feed_requests = if supervised {
                feed_requests
            } else {
                Vec::new()
            };
            // Re-select the previously-selected ward if it survived the refresh,
            // else the first one (the windows `LoadAsync` rule).
            let previous = app.family.selected_ward.clone();
            let index = previous
                .as_deref()
                .and_then(|id| {
                    app.family
                        .wards
                        .iter()
                        .position(|w| w.actor_id.as_slice() == id)
                })
                .or(if app.family.wards.is_empty() {
                    None
                } else {
                    Some(0)
                });
            match index {
                Some(i) => app.family.load_editor(i),
                None => {
                    app.family.selected_ward = None;
                    app.family.editor = PolicyEditor::default();
                    app.family.graduate_revealed = false;
                    app.family.feature_editor = None;
                    app.family.feature_editor_status = None;
                }
            }
            app.errors.remove(&Page::Family);
        }
        Outcome::FeatureLimitWritten {
            view,
            editor,
            status,
        } => {
            apply_outcome(app, Outcome::Loaded(view));
            // Only while the ward the write was for is still the selected one —
            // a selection that moved while the write was in flight must not
            // have another ward's editor painted under it.
            let written_for = editor.as_ref().as_ref().and_then(|e| match e.tier() {
                fauna_client_features::AuthoringTier::Guardian { ward } => Some(ward),
                _ => None,
            });
            if written_for
                .is_some_and(|ward| app.family.selected_ward.as_deref() == Some(&ward[..]))
            {
                app.family.feature_editor = *editor;
                app.family.feature_editor_status = Some(status);
            } else {
                app.family.feature_editor = None;
                app.family.feature_editor_status = None;
            }
        }
        Outcome::Failed(msg) => {
            app.errors.insert(Page::Family, msg);
        }
        Outcome::UsageReported {
            day,
            day_total_minutes,
        } => app.screen_lock.report_succeeded(day, day_total_minutes),
        Outcome::UsageReportFailed => app.screen_lock.report_failed(),
        // Folds nowhere by design — see the variant's own doc comment.
        Outcome::NotifyReported => {}
    }
}

/// The ward's foreground heartbeat, due or not — the one-minute tick's leg
/// (`family-safety.md` § Screen time). Returns the op to run when the shared
/// engine says a report is due, `None` otherwise; **the caller must run every
/// op it is handed**, because the engine has already moved those minutes to
/// in-flight and only [`Outcome::UsageReported`]/[`Outcome::UsageReportFailed`]
/// settles them — a dropped op would wedge the heartbeat for the rest of the
/// session, silently ending a ward's accounting.
///
/// `focused` is whether the ward is actually looking at the app. A TUI has no
/// window to lose focus: it owns its terminal for as long as it runs, and a
/// terminal that is not on screen is indistinguishable from one that is — so
/// this is always `true` in production, and the parameter exists for the tests
/// that need the other value. Over-counting a backgrounded terminal is the
/// conservative direction (it can only *reduce* a ward's remaining budget, and
/// only on the device they left running); silently crediting nothing while a
/// ward really used the app would defeat the pillar.
pub fn due_usage_report(app: &mut App, focused: bool) -> Option<Op> {
    let nest = app.family.nest.clone()?;
    let (minutes, utc_offset_minutes) = app.screen_lock.take_due_report(focused)?;
    Some(Op::UsageReport {
        nest,
        minutes,
        utc_offset_minutes,
    })
}

/// The ward's batched Guardian Notify report, due or not — the other leg of the
/// one-minute tick (`family-safety.md` § Guardian Notify). Returns the op to run
/// when the accumulator says a batch is due (≤ hourly, its own gate), `None`
/// otherwise.
///
/// Unlike [`due_usage_report`] a dropped op costs only that batch: the counts are
/// already cleared at take time and are deliberately not re-credited (see
/// [`Outcome::NotifyReported`]). The tick still runs it like any other op.
pub fn due_notify_report(app: &mut App) -> Option<Op> {
    let nest = app.family.nest.clone()?;
    let (entries, utc_offset_minutes) = app.content_policy.take_notify_report()?;
    Some(Op::NotifyReport {
        nest,
        entries,
        utc_offset_minutes,
    })
}

// ── The shared select catalogs (contract 2) ──────────────────────────────────

/// The ordered `unknown_sender_mail` catalog resolved to display labels — the
/// picker's option list AND what `get_text` reports. The option *set*, its order,
/// and the label keys are shared Rust ([`fauna_core::format::unknown_sender_options`]);
/// this module owns only the label ↔ wire bridge.
fn unknown_sender_labels() -> Vec<String> {
    fauna_core::format::unknown_sender_option_labels(fauna_i18n::strings::lookup)
}

fn feed_sources_labels() -> Vec<String> {
    fauna_core::format::feed_sources_option_labels(fauna_i18n::strings::lookup)
}

fn unknown_peer_dm_labels() -> Vec<String> {
    fauna_core::format::unknown_peer_dm_option_labels(fauna_i18n::strings::lookup)
}

fn content_floor_labels() -> Vec<String> {
    fauna_core::format::content_floor_option_labels(fauna_i18n::strings::lookup)
}

/// Map a committed **label** back to its wire value, using the same shared
/// catalog the option list came from. `None` when the label is not in the
/// catalog — the caller then applies its knob's fail-closed rule.
fn wire_for_label(
    options: Vec<fauna_core::format::ReachPolicyOption>,
    label: &str,
) -> Option<String> {
    options
        .into_iter()
        .find(|o| localized(&o.label) == label)
        .map(|o| o.value)
}

/// `family-policy-unknown-sender-select`'s label → knob mapping. An unmatched
/// label degrades to [`UnknownSenderMail::FAIL_CLOSED`] (`hold`), never the
/// permissive `allow` — the same defense-in-depth linux's `unknown_sender_at`
/// out-of-range fallback states (a bare "keep index 0" would silently downgrade
/// the ward's protection on the next save).
fn unknown_sender_for_label(label: &str) -> UnknownSenderMail {
    match wire_for_label(fauna_core::format::unknown_sender_options(), label) {
        Some(wire) => UnknownSenderMail::from_wire(&wire),
        None => UnknownSenderMail::FAIL_CLOSED,
    }
}

/// Same shape, this knob's strict option being `block`.
fn feed_sources_for_label(label: &str) -> FeedSources {
    match wire_for_label(fauna_core::format::feed_sources_options(), label) {
        Some(wire) => FeedSources::from_wire(&wire),
        None => FeedSources::FAIL_CLOSED,
    }
}

/// Same shape; `hold` again, never the permissive `allow`.
fn unknown_peer_dm_for_label(label: &str) -> UnknownPeerDm {
    match wire_for_label(fauna_core::format::unknown_peer_dm_options(), label) {
        Some(wire) => UnknownPeerDm::from_wire(&wire),
        None => UnknownPeerDm::FAIL_CLOSED,
    }
}

/// Same shape; `block` again, never the permissive `inherit`.
fn content_floor_for_label(label: &str) -> ContentFloor {
    match wire_for_label(fauna_core::format::content_floor_options(), label) {
        Some(wire) => match ContentFloor::from_wire(&wire) {
            ContentFloor::Unknown => ContentFloor::FAIL_CLOSED,
            v => v,
        },
        None => ContentFloor::FAIL_CLOSED,
    }
}

/// The label a stored knob value renders as — always through the shared
/// fail-closed lookup, so an unrecognized wire value shows the STRICT option.
fn unknown_sender_label(value: UnknownSenderMail) -> String {
    localized(&fauna_core::format::unknown_sender_label(value.as_str()))
}

fn feed_sources_label(value: FeedSources) -> String {
    localized(&fauna_core::format::feed_sources_label(value.as_str()))
}

fn unknown_peer_dm_label(value: UnknownPeerDm) -> String {
    localized(&fauna_core::format::unknown_peer_dm_label(value.as_str()))
}

fn content_floor_label(value: ContentFloor) -> String {
    localized(&fauna_core::format::content_floor_label(value.as_str()))
}

// ── Paint ────────────────────────────────────────────────────────────────────

/// The supervised side's read-only `family-policy-summary` — one
/// "{label}: {value}" line per rule. The lines (order, labels, and the
/// fail-closed value rendering) come from shared Rust via
/// [`ReachPolicy::summary_lines`]; this module owns only the join, exactly like
/// linux's `policy_summary`.
/// `usage_today_minutes` folds the screen-time readout into this same summary
/// rather than claiming a new ui.yaml ID. The supervised section's element set
/// is `family-guardian-handle` + `family-policy-summary`, and the ward's usage
/// *is* a line of "the active policy, read-only" — the same shape as every
/// other rule shown here. `None` renders nothing at all: no budget, no
/// accounting.
fn policy_summary_text(policy: &ReachPolicy, usage_today_minutes: Option<u32>) -> String {
    let mut lines = policy
        .summary_lines()
        .into_iter()
        .map(|l| format!("{}: {}", localized(&l.label), localized(&l.value)))
        .collect::<Vec<_>>();
    if let Some(used) = usage_today_minutes {
        lines.push(usage_today_text(used, policy.screen_time));
    }
    lines.join("\n")
}

/// The global `supervised-indicator` text, or `None` when this account is not
/// supervised. Read by [`crate::ui::register_frame`], which registers the
/// indicator on every authenticated frame — it is ui.yaml `global`, not a
/// Family-page element, so it must be present even while another page paints.
pub fn supervised_indicator_text(state: &FamilyState) -> Option<String> {
    state.supervised_by.as_deref().map(t::supervised_indicator)
}

pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.family;
    // The e2e's page landmark is `family-heading`, not `page-heading`
    // (`FamilyActions.navigate` waits on it), and ui.yaml lists both — so both
    // paint, always.
    let mut out = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::label(ids::FAMILY_HEADING, t::TITLE),
    ];

    supervised_elements(st, &mut out);
    guardian_elements(st, &mut out);
    incoming_elements(st, &mut out);
    out
}

/// The supervised side: who guards this account, and the read-only policy.
fn supervised_elements(st: &FamilyState, out: &mut Vec<Element>) {
    let Some(guardian) = st.supervised_by.as_deref() else {
        return;
    };
    out.push(Element::chrome(t::POLICY_SUMMARY_HEADING));
    out.push(Element::label(
        ids::FAMILY_GUARDIAN_HANDLE,
        t::guardian_label(guardian),
    ));
    // The ward always sees the policy (transparency by construction); before the
    // status read lands, or after an offline snapshot restore (which does not
    // restore it), `own_policy` is `None` and the unsupervised-equivalent
    // default renders rather than a blank line.
    let policy = st.own_policy.clone().unwrap_or_default();
    out.push(Element::label(
        ids::FAMILY_POLICY_SUMMARY,
        policy_summary_text(&policy, st.own_usage_today_minutes),
    ));
    // The ward's own band + how it was established — absent, never
    // placeholdered, when there is no band (`family-safety.md` § App surface
    // → *Age-band surfaces*); the shared resolved line, so the guardian's row
    // and the ward's summary cannot disagree.
    if let Some(text) = st.own_age_band.as_ref().and_then(own_age_band_text) {
        out.push(Element::label(ids::FAMILY_AGE_BAND_SUMMARY, text));
    }
}

/// The guardian side: the wards list, the ONE shared editor, and the queue.
fn guardian_elements(st: &FamilyState, out: &mut Vec<Element>) {
    if st.wards.is_empty() {
        return;
    }
    out.push(Element::chrome(t::WARDS_HEADING));
    for (i, ward) in st.wards.iter().enumerate() {
        ward_row(i, ward, out);
    }
    if let Some(ward) = st.selected() {
        policy_editor_elements(st, ward, out);
    }
    approvals_elements(st, out);
}

/// One ward row. Clicking it loads that ward's policy into the shared editor
/// (`FamilyActions.select_ward`), and its children register
/// `.within(ids::FAMILY_WARD_ITEM, i)` so a scoped read resolves exactly this
/// ward — a nested indexed list that skips the containment declaration silently
/// reads empty in every scoped query (the A6 nesting lesson).
fn ward_row(i: usize, ward: &FamilyWardInfo, out: &mut Vec<Element>) {
    out.push(Element::gesture_button(
        ids::FAMILY_WARD_ITEM,
        ward.handle.clone(),
        true,
        Gesture::Family(Action::SelectWard { index: i }),
    ));
    out.push(
        Element::label(ids::FAMILY_WARD_HANDLE, ward.handle.clone())
            .within(ids::FAMILY_WARD_ITEM, i),
    );
    // The ward's band + provenance, only when the ward has a nameable band
    // (a band-less admission paints no row — never a placeholder).
    if let Some(text) = ward.age_band.as_ref().and_then(ward_age_band_text) {
        out.push(Element::label(ids::FAMILY_WARD_AGE_BAND, text).within(ids::FAMILY_WARD_ITEM, i));
    }
    // Rendered only for a ward with counts today, so a quiet ward's row stays
    // clean (linux renders the same way) — and `ward_content_notices_text`
    // never reports an empty readout as "0 flagged".
    if !ward.content_notices.is_empty() {
        out.push(
            Element::label(
                ids::FAMILY_WARD_CONTENT_NOTICES,
                fauna_client_family::row_text::ward_content_notices_text(&ward.content_notices),
            )
            .within(ids::FAMILY_WARD_ITEM, i),
        );
    }
    // The screen-time readout, on the same terms: rendered only when the nest
    // actually accounted a figure for this ward, which it does only while a
    // daily budget is set (§ Screen time — "no usage accounting without a
    // declared policy"). No budget → `None` → no row clutter. The wording is
    // the shared `usage_today_line`, the same call the ward's own summary
    // makes, so guardian and child cannot be shown different numbers.
    if let Some(used) = ward.usage_today_minutes {
        out.push(
            Element::label(
                ids::FAMILY_WARD_USAGE_TODAY,
                usage_today_text(used, ward.policy.screen_time),
            )
            .within(ids::FAMILY_WARD_ITEM, i),
        );
    }
}

/// One "{label}: {value}" screen-time usage line. Shared Rust composes it
/// ([`fauna_core::format::usage_today_line`]); this owns only the join, and
/// BOTH readouts call it — the guardian's per-ward row and the ward's own
/// summary — which is what makes the transparency rule ("the ward's summary
/// shows the same number") structural rather than a convention two call sites
/// could drift from.
fn usage_today_text(
    used_minutes: u32,
    screen_time: Option<fauna_core::screen_time::ScreenTimePolicy>,
) -> String {
    let line = fauna_core::format::usage_today_line(
        used_minutes,
        screen_time.and_then(|s| s.daily_minutes),
    );
    format!("{}: {}", localized(&line.label), localized(&line.value))
}

/// The ONE shared reach/content-policy editor, loaded for the selected ward.
fn policy_editor_elements(st: &FamilyState, ward: &FamilyWardInfo, out: &mut Vec<Element>) {
    let ed = &st.editor;
    // Whose policy is being edited — untagged chrome (ui.yaml scopes no id to
    // the editor heading).
    out.push(Element::chrome(ward.handle.clone()));

    out.push(
        Element::checkbox_gesture(
            ids::FAMILY_POLICY_CONTACT_APPROVAL_TOGGLE,
            t::POLICY_CONTACT_APPROVAL_LABEL,
            ed.contact_approval,
            Gesture::Family(Action::SetContactApproval(!ed.contact_approval)),
        )
        .attr("state", if ed.contact_approval { "on" } else { "off" }),
    );
    out.push(
        Element::select(
            ids::FAMILY_POLICY_UNKNOWN_SENDER_SELECT,
            unknown_sender_label(ed.unknown_sender),
            SelectTarget::FamilyUnknownSender,
            unknown_sender_labels(),
        )
        .labelled(t::POLICY_UNKNOWN_SENDER_LABEL),
    );
    out.push(
        Element::checkbox_gesture(
            ids::FAMILY_POLICY_FEDERATION_TOGGLE,
            t::POLICY_FEDERATION_LABEL,
            ed.federation,
            Gesture::Family(Action::SetFederation(!ed.federation)),
        )
        .attr("state", if ed.federation { "on" } else { "off" }),
    );
    out.push(
        Element::select(
            ids::FAMILY_POLICY_FEED_SOURCES_SELECT,
            feed_sources_label(ed.feed_sources),
            SelectTarget::FamilyFeedSources,
            feed_sources_labels(),
        )
        .labelled(t::POLICY_FEED_SOURCES_LABEL),
    );
    // The bound `family-safety.md` § Guardian policy pillar 1 requires be stated
    // on ANY surface exposing the `feed_sources` knob. It no longer states an
    // absence: the closure — the `unknown_peer_dm` knob below — is now
    // settable here, so the caption points at it instead. Prose, so it carries
    // no id — chrome, like linux's caption.
    out.push(Element::chrome(t::POLICY_FEED_SOURCES_CAVEAT));
    out.push(
        Element::select(
            ids::FAMILY_POLICY_UNKNOWN_PEER_DM_SELECT,
            unknown_peer_dm_label(ed.unknown_peer_dm),
            SelectTarget::FamilyUnknownPeerDm,
            unknown_peer_dm_labels(),
        )
        .labelled(t::POLICY_UNKNOWN_PEER_DM_LABEL),
    );

    // The four per-category content floors, index-aligned with
    // GUARDIAN_FLOOR_CATEGORIES and all rendering the ONE shared catalog.
    for (category, id, label) in [
        (
            0usize,
            "family-policy-content-nsfw-select",
            t::POLICY_CONTENT_NSFW_LABEL,
        ),
        (
            1,
            "family-policy-content-spam-select",
            t::POLICY_CONTENT_SPAM_LABEL,
        ),
        (
            2,
            "family-policy-content-phishing-select",
            t::POLICY_CONTENT_PHISHING_LABEL,
        ),
        (
            3,
            "family-policy-content-commercial-select",
            t::POLICY_CONTENT_COMMERCIAL_LABEL,
        ),
    ] {
        out.push(
            Element::select(
                id,
                content_floor_label(ed.content[category]),
                SelectTarget::FamilyContentFloor { category },
                content_floor_labels(),
            )
            .labelled(label),
        );
    }
    out.push(
        Element::checkbox_gesture(
            ids::FAMILY_POLICY_CONTENT_NOTIFY_TOGGLE,
            t::POLICY_CONTENT_NOTIFY_LABEL,
            ed.content_notify,
            Gesture::Family(Action::SetContentNotify(!ed.content_notify)),
        )
        .attr("state", if ed.content_notify { "on" } else { "off" }),
    );

    // Screen time (§ Screen time, Slice E) — the guardian's three typed inputs,
    // in the same editor and committed by the same Save button as every knob
    // above, exactly as linux and web place them.
    //
    // **Text inputs, not pickers**, and that is the design rather than terminal
    // pragmatism: an EMPTY field is how a guardian says "this control is unset"
    // (i.e. removes a limit), and a picker has no empty position to offer. The
    // two bounds are typed `HH:MM` while the wire stores minutes from local
    // midnight — the conversion is the shared `parse_time_of_day` /
    // `format_time_of_day` pair, so no app invents its own reading of `"9:5"`,
    // `"24:00"` or `"08:60"`.
    out.push(Element::chrome(t::POLICY_SCREEN_HEADING));
    for (id, value, field, label) in [
        (
            "family-policy-screen-window-start-input",
            ed.screen_window_start.clone(),
            FamilyField::ScreenWindowStart,
            t::POLICY_SCREEN_WINDOW_START_LABEL,
        ),
        (
            "family-policy-screen-window-end-input",
            ed.screen_window_end.clone(),
            FamilyField::ScreenWindowEnd,
            t::POLICY_SCREEN_WINDOW_END_LABEL,
        ),
        (
            "family-policy-screen-daily-minutes-input",
            ed.screen_daily_minutes.clone(),
            FamilyField::ScreenDailyMinutes,
            t::POLICY_SCREEN_DAILY_MINUTES_LABEL,
        ),
    ] {
        out.push(Element::input(id, value, Field::Family(field)).labelled(label));
    }
    // The conforming-client bound, stated on the editor surface itself
    // (§ Screen time's last bullet) — and the empty-clears rule, which is not
    // discoverable from an empty box.
    out.push(Element::chrome(t::POLICY_SCREEN_CAVEAT));

    out.push(Element::gesture_button(
        ids::FAMILY_POLICY_SAVE_BUTTON,
        t::POLICY_SAVE_BUTTON,
        true,
        Gesture::Family(Action::SavePolicy),
    ));

    feature_limits_elements(st, ward, out);
    device_mark_elements(ward, out);
    blocked_peer_elements(ward, out);

    // Contact pre-approval for the selected ward (v1: a hex actor id).
    out.push(
        Element::input(
            ids::FAMILY_CONTACT_ADD_INPUT,
            st.contact_input.clone(),
            Field::Family(FamilyField::ContactAdd),
        )
        .labelled(t::CONTACT_ADD_PLACEHOLDER),
    );
    out.push(Element::gesture_button(
        ids::FAMILY_CONTACT_ADD_BUTTON,
        t::CONTACT_ADD_BUTTON,
        true,
        Gesture::Family(Action::AddContact),
    ));

    // The transfer handshake: a pending proposal swaps the initiate row for the
    // pending marker + cancel, so `family-transfer-pending` is present exactly
    // when a proposal is outstanding (the e2e reads it with `is_visible`).
    match &ward.pending_transfer {
        Some(pending) => {
            out.push(Element::label(
                ids::FAMILY_TRANSFER_PENDING,
                t::transfer_pending(&pending.proposed_guardian_handle),
            ));
            out.push(Element::gesture_button(
                ids::FAMILY_TRANSFER_CANCEL_BUTTON,
                t::TRANSFER_CANCEL_BUTTON,
                true,
                Gesture::Family(Action::TransferCancel),
            ));
        }
        None => {
            out.push(
                Element::input(
                    ids::FAMILY_TRANSFER_INPUT,
                    st.transfer_input.clone(),
                    Field::Family(FamilyField::Transfer),
                )
                .labelled(t::TRANSFER_PLACEHOLDER),
            );
            out.push(Element::gesture_button(
                ids::FAMILY_TRANSFER_BUTTON,
                t::TRANSFER_BUTTON,
                true,
                Gesture::Family(Action::Transfer),
            ));
        }
    }

    // Graduation is reveal-then-confirm (the windows convention): the confirm
    // button does not exist until `family-graduate-button` is clicked, and its
    // label names the ward.
    out.push(Element::gesture_button(
        ids::FAMILY_GRADUATE_BUTTON,
        t::GRADUATE_BUTTON,
        true,
        Gesture::Family(Action::RevealGraduate),
    ));
    if st.graduate_revealed {
        out.push(Element::gesture_button(
            ids::FAMILY_GRADUATE_CONFIRM_BUTTON,
            t::graduate_confirm_button(&ward.handle),
            true,
            Gesture::Family(Action::ConfirmGraduate),
        ));
    }
}

/// The guardian-enrolled-device marker (`family-safety.md` § Full visibility
/// for young children, Slice F). One `family-device-mark-item` per device of
/// the SELECTED ward, each carrying its own `family-device-mark-toggle` as a
/// CHILD (`.within`) — never flat: a toggle painted outside its row makes a
/// scoped read return nothing for the marked *and* the unmarked device, a
/// false pass on the negative half, which is exactly how tui's own ward-side
/// badge shipped broken (fixed). Lives inside the per-ward
/// editor, not on the ward rows, so the index space belongs to exactly one
/// ward at a time — mirrors linux/web (`family-safety.md` § Implementation
/// status today, "Built — Slice F, guardian half").
/// The guardian's DENIED bridge-DM peers, and the one-click flip back
/// (`family-safety.md` § The bridge-DM gate → *The un-deny surface*).
///
/// Exists because a deny was otherwise a **one-way door in the UI**: the flip
/// has always been wire-supported (`approvals_decide { kind: "dm_hold",
/// approve: true }`, idempotent and not queue-scoped, so it works long after
/// the hold row is gone), but nothing named the peer to flip. `FamilyWardInfo::
/// blocked_dm_peers` is that read, added with this leg.
///
/// Inside the per-ward editor, beside the device list, so the index space
/// belongs to exactly one ward at a time — the same reason `device_mark_elements`
/// lives there rather than on the ward rows.
///
/// Only `block` verdicts arrive here (the nest filters), so there is no
/// "allowed peers" roster: an allowed peer is simply un-held, and listing them
/// would read as a roster the guardian must curate rather than a list of
/// decisions they can undo.
/// Feature limits (`family-policy-feature-limits-*`) — the guardian tier's
/// authoring host (`family-safety.md` § App surface → *Feature limits*),
/// directly after the policy save button: the same document's fourth pillar,
/// saved by the shared editor's own button rather than the form's. One row per
/// gated feature the nest carries — its name, the guardian's AUTHORED document
/// for this ward in words, and the edit button that opens the shared
/// `feature-policy-editor` right under that row.
///
/// Paint only: every word is `fauna_client_features`' (`ward_surface`, the
/// editor's view-model).
fn feature_limits_elements(st: &FamilyState, ward: &FamilyWardInfo, out: &mut Vec<Element>) {
    use fauna_i18n::strings::features as f;

    out.push(Element::label(
        ids::FAMILY_POLICY_FEATURE_LIMITS_SECTION,
        f::GUARDIAN_SECTION_TITLE,
    ));
    out.push(Element::chrome(f::GUARDIAN_SECTION_DESC));
    // Capability set not read: no rows, never rows for a plane this nest
    // build may not carry.
    let Some(surface) = st.feature_limits(ward) else {
        return;
    };
    for (i, row) in surface.rows().iter().enumerate() {
        out.push(
            Element::label(ids::FAMILY_POLICY_FEATURE_LIMITS_ROW, " ")
                .within(ids::FAMILY_POLICY_FEATURE_LIMITS_ROW, i),
        );
        out.push(
            Element::label(ids::FAMILY_POLICY_FEATURE_LIMITS_NAME, localized(&row.name))
                .within(ids::FAMILY_POLICY_FEATURE_LIMITS_ROW, i),
        );
        out.push(
            Element::label(
                ids::FAMILY_POLICY_FEATURE_LIMITS_SUMMARY,
                localized(&row.summary),
            )
            .within(ids::FAMILY_POLICY_FEATURE_LIMITS_ROW, i),
        );
        out.push(
            Element::gesture_button(
                ids::FAMILY_POLICY_FEATURE_LIMITS_EDIT_BUTTON,
                f::ADMIN_EDIT,
                true,
                Gesture::Family(Action::OpenFeatureLimitEditor(row.feature.clone())),
            )
            .within(ids::FAMILY_POLICY_FEATURE_LIMITS_ROW, i),
        );
        if let Some(editor) = st
            .feature_editor
            .as_ref()
            .filter(|e| e.feature_key() == row.feature)
        {
            out.extend(crate::feature_editor::editor_elements(
                editor,
                st.feature_editor_status.as_deref(),
                crate::feature_editor::EditorGestures {
                    on: Gesture::Family(Action::FeatureLimitOn(true)),
                    off: Gesture::Family(Action::FeatureLimitOn(false)),
                    save: Gesture::Family(Action::SaveFeatureLimit),
                    remove: Gesture::Family(Action::RemoveFeatureLimit),
                    cancel: Gesture::Family(Action::CancelFeatureLimitEditor),
                    cell: |cell| Field::Family(FamilyField::FeatureLimitCell { cell }),
                },
            ));
        }
    }
}

fn blocked_peer_elements(ward: &FamilyWardInfo, out: &mut Vec<Element>) {
    out.push(Element::chrome(t::BLOCKED_PEERS_HEADING));
    out.push(Element::chrome(t::blocked_peers_hint(&ward.handle)));
    if ward.blocked_dm_peers.is_empty() {
        out.push(Element::chrome(t::NO_BLOCKED_PEERS));
        return;
    }
    for (i, peer) in ward.blocked_dm_peers.iter().enumerate() {
        // The row's own text IS the peer id — the only name this nest has for
        // an external bridge peer (it has no actor here, which is why the gate
        // exists). Read by id, never by position, so the nest's row order is
        // not silently part of the contract.
        out.push(Element::label(
            ids::FAMILY_BLOCKED_PEER_ITEM,
            peer.peer_id.clone(),
        ));
        out.push(
            Element::gesture_button(
                ids::FAMILY_BLOCKED_PEER_ALLOW_BUTTON,
                t::BLOCKED_PEER_ALLOW,
                true,
                Gesture::Family(Action::AllowBlockedPeer {
                    ward: ward.actor_id.to_vec(),
                    bridge_id: peer.bridge_id.clone(),
                    peer_id: peer.peer_id.clone(),
                }),
            )
            .within(ids::FAMILY_BLOCKED_PEER_ITEM, i),
        );
    }
}

fn device_mark_elements(ward: &FamilyWardInfo, out: &mut Vec<Element>) {
    out.push(Element::chrome(t::WARD_DEVICES_HEADING));
    out.push(Element::chrome(t::ward_devices_hint(&ward.handle)));
    if ward.devices.is_empty() {
        out.push(Element::chrome(t::NO_WARD_DEVICES));
        return;
    }
    for (i, device) in ward.devices.iter().enumerate() {
        // The row's own text IS the device's label — `device_mark_labels()`
        // reads it via `get_text`, order-independent (a row is found by its
        // device's name, never by position, so the nest's device order is
        // not silently part of the contract).
        out.push(Element::label(
            ids::FAMILY_DEVICE_MARK_ITEM,
            device.label.clone(),
        ));
        out.push(
            Element::checkbox_gesture(
                ids::FAMILY_DEVICE_MARK_TOGGLE,
                t::DEVICE_MARK_LABEL,
                device.guardian_marked,
                Gesture::Family(Action::DeviceMark {
                    ward: ward.actor_id.to_vec(),
                    device_id: device.device_id.clone(),
                    marked: !device.guardian_marked,
                }),
            )
            .attr("state", if device.guardian_marked { "on" } else { "off" })
            .within(ids::FAMILY_DEVICE_MARK_ITEM, i),
        );
    }
}

/// The cross-ward approvals queue. Each row's two buttons register
/// `.within(ids::FAMILY_APPROVAL_ITEM, i)`, so a scoped read resolves exactly that
/// row while the shared unscoped `click(id, index=i)` still lands on it.
fn approvals_elements(st: &FamilyState, out: &mut Vec<Element>) {
    out.push(Element::chrome(t::APPROVALS_HEADING));
    if st.approvals.is_empty() {
        out.push(Element::chrome(t::NO_APPROVALS));
        return;
    }
    for (i, entry) in st.approvals.iter().enumerate() {
        // A `mail_hold` row shows `peer_address` and every other kind its
        // `summary` — the rule itself is shared
        // (`fauna_core::format::approval_display_text`, reached through
        // `FamilyApprovalEntry::display_text`); a truthfully-empty null-path
        // sender falls back to the localized no-sender label rather than
        // rendering blank beside live Approve/Deny buttons.
        let text = entry.display_text().unwrap_or(t::APPROVAL_NO_SENDER);
        out.push(Element::label(ids::FAMILY_APPROVAL_ITEM, text));
        out.push(
            Element::gesture_button(
                ids::FAMILY_APPROVAL_APPROVE_BUTTON,
                t::APPROVE,
                true,
                Gesture::Family(Action::Decide {
                    index: i,
                    approve: true,
                }),
            )
            .within(ids::FAMILY_APPROVAL_ITEM, i),
        );
        out.push(
            Element::gesture_button(
                ids::FAMILY_APPROVAL_DENY_BUTTON,
                t::DENY,
                true,
                Gesture::Family(Action::Decide {
                    index: i,
                    approve: false,
                }),
            )
            .within(ids::FAMILY_APPROVAL_ITEM, i),
        );
    }
}

/// The proposed-guardian prompt — independent of both roles, which is why the
/// `family-tab` gate widens to cover an account with no other relationship.
fn incoming_elements(st: &FamilyState, out: &mut Vec<Element>) {
    if st.incoming.is_empty() {
        return;
    }
    out.push(Element::chrome(t::INCOMING_TRANSFERS_HEADING));
    for (i, entry) in st.incoming.iter().enumerate() {
        // The CURRENT guardian is the fact the prompt renders (the initiator may
        // have been the admin).
        out.push(Element::label(
            ids::FAMILY_INCOMING_TRANSFER_ITEM,
            t::incoming_transfer_text(&entry.guardian_handle, &entry.supervised_handle),
        ));
        out.push(
            Element::gesture_button(
                ids::FAMILY_INCOMING_TRANSFER_ACCEPT_BUTTON,
                t::INCOMING_TRANSFER_ACCEPT_BUTTON,
                true,
                Gesture::Family(Action::DecideIncoming {
                    index: i,
                    accept: true,
                }),
            )
            .within(ids::FAMILY_INCOMING_TRANSFER_ITEM, i),
        );
        out.push(
            Element::gesture_button(
                ids::FAMILY_INCOMING_TRANSFER_DECLINE_BUTTON,
                t::INCOMING_TRANSFER_DECLINE_BUTTON,
                true,
                Gesture::Family(Action::DecideIncoming {
                    index: i,
                    accept: false,
                }),
            )
            .within(ids::FAMILY_INCOMING_TRANSFER_ITEM, i),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::authed_app;
    use fauna_protocol::ByteBuf;
    use fauna_protocol::family::{
        FamilyBlockedPeerInfo, FamilyGuardianInfo, FamilyPendingTransferInfo, FamilyStatusReply,
        FamilyWardDeviceInfo,
    };

    fn dummy_nest() -> Arc<NestClient> {
        NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([9u8; 32]),
        )
    }

    // ── The un-deny surface (family-safety.md § The bridge-DM gate) ──────────
    //
    // The property under test is that a deny is REVERSIBLE from the UI. Before
    // this leg the flip existed on the wire and nowhere else, so the guardian
    // could not name the peer to un-deny. A test that only checked "a row
    // rendered" would miss the half that matters: the row must carry the
    // (bridge, peer) pair the un-deny call addresses, or the button un-denies
    // somebody else.

    fn blocked(bridge: &str, peer: &str) -> FamilyBlockedPeerInfo {
        FamilyBlockedPeerInfo {
            bridge_id: bridge.to_string(),
            peer_id: peer.to_string(),
            extra: Default::default(),
        }
    }

    #[test]
    fn a_ward_with_nothing_denied_says_so() {
        let mut app = family_app();
        app.family.wards = vec![ward(1, "kid", ReachPolicy::default())];
        app.family.load_editor(0);

        let out = elements(&app);

        assert!(
            !out.iter().any(|e| e.id == ids::FAMILY_BLOCKED_PEER_ITEM),
            "no denials means no rows"
        );
        assert!(
            out.iter().any(|e| e.text == t::NO_BLOCKED_PEERS),
            "the empty case must be STATED, not a blank gap — a guardian who \
             denied nobody and a surface that failed to load look identical \
             otherwise"
        );
    }

    #[test]
    fn each_denied_peer_gets_a_row_and_an_allow_button() {
        let mut app = family_app();
        let mut w = ward(1, "kid", ReachPolicy::default());
        w.blocked_dm_peers = vec![blocked("nostr", "npub1aaa"), blocked("nostr", "npub1bbb")];
        app.family.wards = vec![w];
        app.family.load_editor(0);

        let out = elements(&app);

        let rows: Vec<&str> = out
            .iter()
            .filter(|e| e.id == ids::FAMILY_BLOCKED_PEER_ITEM)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(
            rows,
            vec!["npub1aaa", "npub1bbb"],
            "the row text IS the peer id — the only name this nest has for an \
             external bridge peer, and what the guardian recognises"
        );
        assert_eq!(
            out.iter()
                .filter(|e| e.id == ids::FAMILY_BLOCKED_PEER_ALLOW_BUTTON)
                .count(),
            2,
            "every denial must be reversible; a row without its button is the \
             one-way door this leg exists to close"
        );
    }

    /// ⚠ The assertion that actually protects the user: the button carries the
    /// row's OWN (bridge, peer) pair. Getting this wrong un-denies the wrong
    /// person — a silent, guardian-invisible failure, since the surface would
    /// still look correct.
    #[test]
    fn the_allow_button_addresses_its_own_rows_peer() {
        let mut app = family_app();
        let mut w = ward(7, "kid", ReachPolicy::default());
        w.blocked_dm_peers = vec![blocked("nostr", "npub1aaa"), blocked("nostr", "npub1bbb")];
        app.family.wards = vec![w];
        app.family.load_editor(0);

        let out = elements(&app);
        let peers: Vec<String> = out
            .iter()
            .filter(|e| e.id == ids::FAMILY_BLOCKED_PEER_ALLOW_BUTTON)
            .filter_map(|e| match e.gesture() {
                Some(Gesture::Family(Action::AllowBlockedPeer { peer_id, .. })) => {
                    Some(peer_id.clone())
                }
                _ => None,
            })
            .collect();

        assert_eq!(
            peers,
            vec!["npub1aaa".to_string(), "npub1bbb".to_string()],
            "each button must carry its own row's peer, not the first row's"
        );
    }

    /// The un-deny resolves to the shared un-deny op carrying its row's own
    /// `(bridge_id, peer_id)` — the nest's `dm_hold` arm refuses outright
    /// without both, and the shared call owns the rest of the wire shape.
    #[test]
    fn the_allow_gesture_resolves_to_the_shared_un_deny_for_its_peer() {
        let mut app = family_app();
        app.family.nest = Some(dummy_nest());
        let mut w = ward(3, "kid", ReachPolicy::default());
        w.blocked_dm_peers = vec![blocked("nostr", "npub1aaa")];
        app.family.wards = vec![w];
        app.family.load_editor(0);

        let op = apply_local(
            &mut app,
            Action::AllowBlockedPeer {
                ward: vec![3u8; 32],
                bridge_id: "nostr".to_string(),
                peer_id: "npub1aaa".to_string(),
            },
        );

        match op {
            Some(Op::AllowBlockedPeer {
                ward,
                bridge_id,
                peer_id,
                ..
            }) => {
                assert_eq!(ward, vec![3u8; 32]);
                assert_eq!(bridge_id, "nostr");
                assert_eq!(peer_id, "npub1aaa");
            }
            Some(_) => panic!("expected an AllowBlockedPeer op, got a different op"),
            None => panic!("the allow gesture produced no op at all"),
        }
        assert_eq!(
            Action::AllowBlockedPeer {
                ward: Vec::new(),
                bridge_id: String::new(),
                peer_id: String::new(),
            }
            .wire_kind(),
            Some("fauna.family.approvals.decide"),
            "the un-deny rides the ordinary decide call — idempotent and not \
             queue-scoped, which is what lets it work after the hold row is gone"
        );
    }

    fn ward(actor: u8, handle: &str, policy: ReachPolicy) -> FamilyWardInfo {
        FamilyWardInfo {
            actor_id: ByteBuf::from(vec![actor; 32]),
            handle: handle.to_string(),
            policy,
            ..Default::default()
        }
    }

    // ── Feature limits — the guardian host (family-safety.md § App surface) ──

    /// A ward entry as a nest that sends the ceiling would: tier 1 only above
    /// the guardian, `authored` the guardian's own sub-document.
    fn limited_ward(
        authored: Option<fauna_protocol::features::GuardianFeaturePolicies>,
    ) -> FamilyWardInfo {
        use fauna_core::feature_gate::{GatedFeature, effective_policy};
        let mut w = ward(
            7,
            "kid",
            ReachPolicy {
                features: authored,
                ..Default::default()
            },
        );
        w.features_ceiling = GatedFeature::ALL
            .iter()
            .map(|f| fauna_protocol::features::FeatureCeilingItem {
                feature: *f,
                ceiling: effective_policy(*f, &[], &[]),
                extra: Default::default(),
            })
            .collect();
        w
    }

    fn limits_app(ward: FamilyWardInfo) -> App {
        use fauna_protocol::discovery::capability;
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                wards: vec![ward],
                capabilities: Some(vec![
                    capability::SUBSCRIPTIONS.to_string(),
                    capability::P2P_SHARE.to_string(),
                ]),
                ..Default::default()
            },
        );
        app
    }

    fn texts_of<'a>(els: &'a [Element], id: &str) -> Vec<&'a str> {
        els.iter()
            .filter(|e| e.id == id)
            .map(|e| e.text.as_str())
            .collect()
    }

    #[test]
    fn the_feature_limits_section_follows_the_save_button_with_a_row_per_member() {
        use fauna_core::feature_gate::FeaturePolicy;
        let authored = [("p2p-share".to_string(), FeaturePolicy::DENIED)]
            .into_iter()
            .collect();
        let app = limits_app(limited_ward(Some(authored)));
        let els = elements(&app);

        let at = |id: &str| els.iter().position(|e| e.id == id);
        let save = at(ids::FAMILY_POLICY_SAVE_BUTTON).expect("save button");
        assert_eq!(
            at(ids::FAMILY_POLICY_FEATURE_LIMITS_SECTION),
            Some(save + 1),
            "the section sits directly after the policy save button"
        );
        assert_eq!(
            texts_of(&els, ids::FAMILY_POLICY_FEATURE_LIMITS_SUMMARY),
            vec!["No limit set", "No limit set", "Turned off"],
            "registry order, the guardian's AUTHORED document in words"
        );
        assert_eq!(
            texts_of(&els, ids::FAMILY_POLICY_FEATURE_LIMITS_EDIT_BUTTON).len(),
            3
        );
        assert!(
            at(ids::FEATURE_POLICY_EDITOR).is_none(),
            "the editor is present only while open"
        );
    }

    #[test]
    fn the_edit_button_opens_the_shared_editor_at_the_guardian_tier_for_that_ward() {
        let mut app = limits_app(limited_ward(None));
        assert!(
            apply_local(&mut app, Action::OpenFeatureLimitEditor("zaps".into())).is_none(),
            "opening is local — the seed rode the status read"
        );
        let editor = app.family.feature_editor.as_ref().expect("open");
        assert_eq!(
            editor.tier(),
            fauna_client_features::AuthoringTier::Guardian { ward: [7; 32] }
        );
        let els = elements(&app);
        let title = texts_of(&els, ids::FEATURE_POLICY_EDITOR_TITLE);
        assert_eq!(title.len(), 1);
        assert!(title[0].ends_with("limits only for kid"), "{title:?}");

        // A cell the shared parser cannot read goes to error-message and
        // dispatches nothing.
        set_field(
            &mut app.family,
            FamilyField::FeatureLimitCell { cell: 0 },
            "lots".into(),
        );
        assert!(apply_local(&mut app, Action::SaveFeatureLimit).is_none());
        assert!(app.errors.contains_key(&Page::Family));

        set_field(
            &mut app.family,
            FamilyField::FeatureLimitCell { cell: 0 },
            "3".into(),
        );
        assert!(matches!(
            apply_local(&mut app, Action::SaveFeatureLimit),
            Some(Op::WriteFeatureLimit { remove: false, .. })
        ));
    }

    #[test]
    fn an_unreadable_ward_document_says_so_and_never_reads_as_no_limit() {
        use fauna_core::feature_gate::{FeaturePolicy, GatedFeature};
        let mut w = limited_ward(Some(
            GatedFeature::ALL
                .iter()
                .map(|f| (f.as_str().to_string(), FeaturePolicy::DENIED))
                .collect(),
        ));
        w.features_unreadable = true;
        let app = limits_app(w);
        let els = elements(&app);
        let summaries = texts_of(&els, ids::FAMILY_POLICY_FEATURE_LIMITS_SUMMARY);
        assert_eq!(summaries.len(), 3);
        for summary in summaries {
            assert_eq!(
                summary,
                fauna_i18n::strings::lookup("features.authored_unreadable").unwrap()
            );
        }
    }

    #[test]
    fn selecting_another_ward_closes_the_feature_editor() {
        let mut app = limits_app(limited_ward(None));
        app.family
            .wards
            .push(ward(8, "sibling", ReachPolicy::default()));
        apply_local(&mut app, Action::OpenFeatureLimitEditor("zaps".into()));
        assert!(app.family.feature_editor.is_some());
        apply_local(&mut app, Action::SelectWard { index: 1 });
        assert!(
            app.family.feature_editor.is_none(),
            "one ward's editor must never stay open over another"
        );
    }

    /// `family-ward-age-band` paints only for a ward with a nameable band, scoped
    /// under its row; `family-age-band-summary` only for a supervised caller
    /// with one — a band-less admission paints neither (absent, never a
    /// placeholder).
    #[test]
    fn age_band_readouts_are_absent_without_a_band() {
        let mut banded = ward(2, "kid", ReachPolicy::default());
        banded.age_band = Some(FamilyAgeBandInfo {
            band: "13-15".into(),
            provenance: "guardian-asserted".into(),
            ..Default::default()
        });
        let app = guardian_app(vec![banded, ward(3, "other", ReachPolicy::default())]);
        let els = elements(&app);
        let rows: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "family-ward-age-band")
            .collect();
        assert_eq!(rows.len(), 1, "only the banded ward paints the row");
        assert!(
            rows[0].text.contains("13–15") && rows[0].text.contains("set by guardian"),
            "{}",
            rows[0].text
        );
        assert_eq!(rows[0].path.len(), 1, "scoped under its family-ward-item");
        assert!(
            !ids(&app).iter().any(|id| id == "family-age-band-summary"),
            "a guardian who is not supervised has no summary"
        );

        let mut app = guardian_app(vec![]);
        app.family.supervised_by = Some("mum".into());
        app.family.own_age_band = None;
        assert!(
            !ids(&app).iter().any(|id| id == "family-age-band-summary"),
            "no band → no summary"
        );
        app.family.own_age_band = Some(FamilyAgeBandInfo {
            band: "u13".into(),
            provenance: "attested-ios".into(),
            ..Default::default()
        });
        let summary = elements(&app)
            .into_iter()
            .find(|e| e.id == "family-age-band-summary")
            .expect("a supervised caller with a band paints the summary");
        assert!(
            summary.text.starts_with("Your age band") && summary.text.contains("verified on iOS"),
            "{}",
            summary.text
        );
    }

    fn device(id: &str, label: &str, marked: bool) -> FamilyWardDeviceInfo {
        FamilyWardDeviceInfo {
            device_id: id.to_string(),
            label: label.to_string(),
            guardian_marked: marked,
            ..Default::default()
        }
    }

    fn family_app() -> App {
        let mut app = authed_app();
        app.page = Page::Family;
        app.family.nest = Some(dummy_nest());
        app
    }

    /// Fold a whole-page view in, exactly as a resolved `Op` would — so every
    /// test below exercises the real `apply_outcome` path (ward re-selection and
    /// editor load included) rather than poking state by hand.
    fn load(app: &mut App, view: FamilyView) {
        apply_outcome(app, Outcome::Loaded(Box::new(view)));
    }

    fn guardian_app(wards: Vec<FamilyWardInfo>) -> App {
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                wards,
                ..Default::default()
            },
        );
        app
    }

    fn ids(app: &App) -> Vec<String> {
        elements(app).into_iter().map(|e| e.id).collect()
    }

    fn count_id(app: &App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    /// Count occurrences of `id` scoped to one `container[index]` — the
    /// registry's own containment rule, asked through `Registry` itself, so this
    /// proves the scoped e2e query would read exactly that row.
    fn count_scoped(app: &App, id: &str, container: &str, index: usize) -> usize {
        crate::automation::Registry::of(elements(app))
            .count_scoped(id, &[(container.to_string(), index)])
    }

    fn text_of(app: &App, id: &str) -> String {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .map(|e| e.text)
            .unwrap_or_default()
    }

    fn attr_of(app: &App, id: &str, key: &str) -> String {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .and_then(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
            })
            .unwrap_or_default()
    }

    /// A guardian with one ward paints every BUILT ui.yaml `family` id — which
    /// since Slice E includes the guardian's three screen-time inputs.
    #[test]
    fn the_page_paints_the_built_ui_yaml_ids_and_omits_the_unbuilt_ones() {
        let app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);
        let ids = ids(&app);
        for id in [
            "page-heading",
            "family-heading",
            "family-ward-item",
            "family-ward-handle",
            "family-policy-contact-approval-toggle",
            "family-policy-unknown-sender-select",
            "family-policy-federation-toggle",
            "family-policy-feed-sources-select",
            "family-policy-unknown-peer-dm-select",
            "family-policy-content-nsfw-select",
            "family-policy-content-spam-select",
            "family-policy-content-phishing-select",
            "family-policy-content-commercial-select",
            "family-policy-content-notify-toggle",
            "family-policy-screen-window-start-input",
            "family-policy-screen-window-end-input",
            "family-policy-screen-daily-minutes-input",
            "family-policy-save-button",
            "family-contact-add-input",
            "family-contact-add-button",
            "family-transfer-input",
            "family-transfer-button",
            "family-graduate-button",
        ] {
            assert!(ids.iter().any(|i| i == id), "missing {id:?}; have {ids:?}");
        }
        // `family-ward-usage-today` is built, but renders only while the nest
        // accounted a figure for that ward — which it does only under a daily
        // budget. This ward has none, so its absence here is the "no accounting
        // without a declared policy" rule, not a missing feature (the populated
        // case is `the_ward_usage_readout_renders_only_under_a_budget`).
        assert_eq!(count_id(&app, "family-ward-usage-today"), 0);
        // family-device-mark-item/-toggle ARE built (Slice-G lift, 2026-08-01)
        // but this ward carries no devices, so neither paints — see
        // `device_mark_rows_are_scoped_inside_their_own_item` below for the
        // populated case.
        assert_eq!(count_id(&app, "family-device-mark-item"), 0);
        assert_eq!(count_id(&app, "family-device-mark-toggle"), 0);
        // Same shape for the un-deny surface (built 2026-08-28): the ids ARE
        // built, but this ward has nothing denied, so neither paints. The
        // populated case is `each_denied_peer_gets_a_row_and_an_allow_button`;
        // the stated-empty case is `a_ward_with_nothing_denied_says_so`.
        assert_eq!(count_id(&app, "family-blocked-peer-item"), 0);
        assert_eq!(count_id(&app, "family-blocked-peer-allow-button"), 0);
        // Reveal-gated / state-gated ids are correctly ABSENT on a fresh load.
        assert_eq!(count_id(&app, "family-graduate-confirm-button"), 0);
        assert_eq!(count_id(&app, "family-transfer-pending"), 0);
        assert_eq!(count_id(&app, "family-guardian-handle"), 0);
    }

    /// One `family-device-mark-item` per device of the selected ward, its own
    /// text carrying the device's label (`device_mark_labels()` reads it via
    /// `get_text`) — and the toggle's `state` attr mirrors `guardian_marked`.
    #[test]
    fn device_mark_rows_paint_one_per_device_with_the_label_as_row_text() {
        let mut ward = ward(2, "kid", ReachPolicy::default());
        ward.devices = vec![
            device("aa", "parents-tablet", false),
            device("bb", "kids-phone", true),
        ];
        let app = guardian_app(vec![ward]);

        assert_eq!(count_id(&app, "family-device-mark-item"), 2);
        assert_eq!(count_id(&app, "family-device-mark-toggle"), 2);
        let labels: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "family-device-mark-item")
            .map(|e| e.text)
            .collect();
        assert_eq!(labels, vec!["parents-tablet", "kids-phone"]);

        assert_eq!(attr_of(&app, "family-device-mark-toggle", "state"), "off");
    }

    /// Slice F's structural contract (mirrors linux's
    /// `device_mark_toggle_is_scoped_inside_its_own_row`, mutation-verified
    /// there): the toggle is a CHILD of its own `family-device-mark-item`, not
    /// a flat sibling. A flat paint makes the e2e's scoped read
    /// (`scope="family-device-mark-item[i]"`) return nothing for the marked
    /// device *and* nothing for the unmarked one — a false pass on the
    /// negative half, exactly how tui's ward-side badge shipped broken (fixed).
    #[test]
    fn device_mark_toggle_is_scoped_inside_its_own_item() {
        let mut ward = ward(2, "kid", ReachPolicy::default());
        ward.devices = vec![
            device("aa", "parents-tablet", false),
            device("bb", "kids-phone", true),
        ];
        let app = guardian_app(vec![ward]);

        assert_eq!(
            count_scoped(
                &app,
                "family-device-mark-toggle",
                "family-device-mark-item",
                0
            ),
            1
        );
        assert_eq!(
            count_scoped(
                &app,
                "family-device-mark-toggle",
                "family-device-mark-item",
                1
            ),
            1
        );
    }

    /// The flip carries the ward actor id + `device_id` **by value**, never a
    /// row index (`family-safety.md` § Full visibility for young children:
    /// "the child cannot remove the guardian's device" is a security promise,
    /// so a concurrent refetch reordering devices must never route a flip at
    /// the wrong one). The dispatched op also negates the CURRENT state, so a
    /// click is idempotent from the driver's side.
    #[test]
    fn device_mark_dispatches_the_flip_by_ward_and_device_id_not_index() {
        let mut ward = ward(2, "kid", ReachPolicy::default());
        ward.devices = vec![device("bb", "kids-phone", true)];
        let mut app = guardian_app(vec![ward]);

        let op = apply_local(
            &mut app,
            Action::DeviceMark {
                ward: vec![2u8; 32],
                device_id: "bb".to_string(),
                marked: false,
            },
        )
        .expect("device mark returns an op");
        match op {
            Op::DeviceMark {
                ward,
                device_id,
                marked,
                ..
            } => {
                assert_eq!(ward, vec![2u8; 32]);
                assert_eq!(device_id, "bb");
                assert!(!marked, "unmarking a currently-marked device");
            }
            _ => panic!("expected Op::DeviceMark"),
        }
    }

    /// Contract 2: every select's options + rendered text are the LOCALIZED
    /// LABELS the e2e drives (`family.py`'s three label maps), never the wire
    /// values — and the local option ORDER is the shared catalog's, so the two
    /// cannot drift.
    #[test]
    fn selects_round_trip_localized_labels_in_the_shared_catalog_order() {
        assert_eq!(
            unknown_sender_labels(),
            ["Allow", "Hold for review", "Reject"]
        );
        assert_eq!(feed_sources_labels(), ["Allow", "Block"]);
        assert_eq!(
            content_floor_labels(),
            ["Use my settings", "Collapse", "Block"]
        );
        // Anti-drift: the labels this page paints are index-aligned with the
        // shared catalogs' wire values, in the ratified order.
        let wires: Vec<String> = fauna_core::format::unknown_sender_options()
            .into_iter()
            .map(|o| o.value)
            .collect();
        assert_eq!(wires, ["allow", "hold", "reject"]);
        assert_eq!(
            UnknownSenderMail::ORDER.map(|v| v.as_str()).to_vec(),
            wires,
            "the local knob order must equal the shared catalog order"
        );
        assert_eq!(
            ContentFloor::ORDER.map(|v| v.as_str()).to_vec(),
            fauna_core::format::content_floor_options()
                .into_iter()
                .map(|o| o.value)
                .collect::<Vec<_>>()
        );

        // The painted select carries the persisted knob's LABEL.
        let app = guardian_app(vec![ward(
            2,
            "kid",
            ReachPolicy {
                unknown_sender_mail: "hold".into(),
                feed_sources: "block".into(),
                ..Default::default()
            },
        )]);
        assert_eq!(
            text_of(&app, "family-policy-unknown-sender-select"),
            "Hold for review"
        );
        assert_eq!(text_of(&app, "family-policy-feed-sources-select"), "Block");
        let options = elements(&app)
            .into_iter()
            .find(|e| e.id == "family-policy-unknown-sender-select")
            .and_then(|e| match e.role {
                crate::element::Role::Select { options, .. } => Some(options),
                _ => None,
            })
            .expect("the unknown-sender picker is a select");
        assert_eq!(options, unknown_sender_labels());
    }

    /// Committing a label maps it back to its wire value; a label outside the
    /// catalog degrades to the knob's FAIL-CLOSED option (`hold`/`block`), never
    /// the permissive one — a save writes the editor straight back, so the
    /// permissive fallback would silently downgrade the ward's protection.
    #[test]
    fn an_unknown_select_label_falls_closed_never_to_the_permissive_option() {
        let mut app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);

        apply_local(&mut app, Action::SetUnknownSender("Reject".into()));
        assert_eq!(app.family.editor.unknown_sender, UnknownSenderMail::Reject);
        apply_local(&mut app, Action::SetUnknownSender("Quarantine".into()));
        assert_eq!(
            app.family.editor.unknown_sender,
            UnknownSenderMail::Hold,
            "an unmatched label must fail closed to hold, not allow"
        );

        apply_local(&mut app, Action::SetFeedSources("Allow".into()));
        assert_eq!(app.family.editor.feed_sources, FeedSources::Allow);
        apply_local(&mut app, Action::SetFeedSources("Curated".into()));
        assert_eq!(app.family.editor.feed_sources, FeedSources::Block);

        apply_local(
            &mut app,
            Action::SetContentFloor {
                category: 1,
                label: "Collapse".into(),
            },
        );
        assert_eq!(app.family.editor.content[1], ContentFloor::Collapse);
        apply_local(
            &mut app,
            Action::SetContentFloor {
                category: 1,
                label: "Nonsense".into(),
            },
        );
        assert_eq!(app.family.editor.content[1], ContentFloor::Block);
    }

    /// The bridge-DM knob renders from the ward's stored policy through the
    /// same shared catalog the two v1 knobs use, and an unmatched label fails
    /// closed to `hold` — the strict option, never `allow`
    /// (`family-safety.md` § The bridge-DM gate).
    ///
    /// The knob rides the wire as `Option<String>`, and **absent is the `allow`
    /// default, not the fail-closed value**: the nest reports `None` for a knob
    /// sitting at its default, so rendering absence as `hold` would show the
    /// guardian a policy stricter than the one enforced.
    #[test]
    fn the_bridge_dm_select_renders_the_catalog_and_fails_closed_on_an_unknown_label() {
        assert_eq!(
            unknown_peer_dm_labels(),
            fauna_core::format::unknown_peer_dm_options()
                .into_iter()
                .map(|o| localized(&o.label))
                .collect::<Vec<_>>()
        );

        // Absent (the nest's shape for a knob at its default) renders `allow`.
        let app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);
        assert_eq!(
            text_of(&app, "family-policy-unknown-peer-dm-select"),
            "Allow"
        );

        let app = guardian_app(vec![ward(
            2,
            "kid",
            ReachPolicy {
                unknown_peer_dm: Some("hold".into()),
                ..Default::default()
            },
        )]);
        assert_eq!(
            text_of(&app, "family-policy-unknown-peer-dm-select"),
            "Hold for review"
        );
        let options = elements(&app)
            .into_iter()
            .find(|e| e.id == "family-policy-unknown-peer-dm-select")
            .and_then(|e| match e.role {
                crate::element::Role::Select { options, .. } => Some(options),
                _ => None,
            })
            .expect("the bridge-DM picker is a select");
        assert_eq!(options, unknown_peer_dm_labels());

        let mut app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);
        apply_local(&mut app, Action::SetUnknownPeerDm("Hold for review".into()));
        assert_eq!(app.family.editor.unknown_peer_dm, UnknownPeerDm::Hold);
        apply_local(&mut app, Action::SetUnknownPeerDm("Whenever".into()));
        assert_eq!(
            app.family.editor.unknown_peer_dm,
            UnknownPeerDm::Hold,
            "an unmatched label must fail closed to hold, not allow"
        );
    }

    /// `family-safety.md` § Policy-update compatibility — an **absent**
    /// `unknown_peer_dm` on `policy.update` means *leave it unchanged*, so the
    /// editor sends the knob only once the guardian has actually touched its
    /// select.
    ///
    /// This is not a micro-optimization. The nest reports a value only a
    /// *newer* nest could have written verbatim, and the client fails it closed
    /// to `hold` on render; echoing that render back on an unrelated save
    /// (fixing a bedtime, say) would silently rewrite the ward's knob to a value
    /// the guardian never chose. Untouched means untouched.
    #[test]
    fn an_untouched_bridge_dm_select_leaves_the_stored_knob_alone_on_save() {
        let mut app = guardian_app(vec![ward(
            2,
            "kid",
            ReachPolicy {
                unknown_peer_dm: Some("hold".into()),
                ..Default::default()
            },
        )]);
        assert_eq!(
            app.family.editor.to_policy().unwrap().unknown_peer_dm,
            None,
            "an untouched knob rides absent — the nest leaves it as stored"
        );

        // Editing something else still leaves the knob absent.
        apply_local(&mut app, Action::SetContactApproval(true));
        assert_eq!(app.family.editor.to_policy().unwrap().unknown_peer_dm, None);

        // Touching the select itself is what makes it ride.
        apply_local(&mut app, Action::SetUnknownPeerDm("Allow".into()));
        assert_eq!(
            app.family.editor.to_policy().unwrap().unknown_peer_dm,
            Some("allow".to_string()),
        );
    }

    /// Contract 1: every id'd toggle carries `state` = on/off, which is the ONLY
    /// thing `family.py` reads from a toggle (`get_attr(id, "state")`), and the
    /// gesture it dispatches produces the FLIPPED value, so a driver click is
    /// idempotent from its side.
    #[test]
    fn the_three_toggles_carry_the_state_attr_and_flip() {
        let mut app = guardian_app(vec![ward(
            2,
            "kid",
            ReachPolicy {
                contact_approval: true,
                federation_contact: false,
                content_notify: Some(true),
                ..Default::default()
            },
        )]);
        assert_eq!(
            attr_of(&app, "family-policy-contact-approval-toggle", "state"),
            "on"
        );
        assert_eq!(
            attr_of(&app, "family-policy-federation-toggle", "state"),
            "off"
        );
        assert_eq!(
            attr_of(&app, "family-policy-content-notify-toggle", "state"),
            "on"
        );

        // Each toggle's gesture carries the value the tap should PRODUCE.
        let gesture = elements(&app)
            .into_iter()
            .find(|e| e.id == "family-policy-federation-toggle")
            .and_then(|e| match e.role {
                crate::element::Role::Checkbox { gesture, checked } => {
                    assert!(!checked, "reads the persisted false");
                    Some(gesture)
                }
                _ => None,
            })
            .expect("the federation toggle is a checkbox");
        match gesture {
            Gesture::Family(Action::SetFederation(value)) => assert!(value, "the tap flips it on"),
            other => panic!("expected SetFederation, got {other:?}"),
        }

        apply_local(&mut app, Action::SetFederation(true));
        assert_eq!(
            attr_of(&app, "family-policy-federation-toggle", "state"),
            "on"
        );
    }

    /// Save sends the editor buffer for the SELECTED ward, with all three v1.x
    /// pillars authored (`Some`). Screen time is `Some` even when every field is
    /// empty: absent would mean "leave unchanged" and could never CLEAR a limit,
    /// so an untouched editor must still send the all-`None` policy.
    #[test]
    fn save_sends_the_edited_buffer_for_the_selected_ward() {
        let mut app = guardian_app(vec![
            ward(2, "kid", ReachPolicy::default()),
            ward(3, "sibling", ReachPolicy::default()),
        ]);
        // Select the SECOND ward, then edit.
        apply_local(&mut app, Action::SelectWard { index: 1 });
        apply_local(&mut app, Action::SetContactApproval(true));
        apply_local(&mut app, Action::SetUnknownSender("Reject".into()));
        apply_local(&mut app, Action::SetFeedSources("Block".into()));
        apply_local(&mut app, Action::SetContentNotify(true));
        apply_local(
            &mut app,
            Action::SetContentFloor {
                category: 0,
                label: "Block".into(),
            },
        );

        let op = apply_local(&mut app, Action::SavePolicy).expect("save returns an op");
        match op {
            Op::PolicyUpdate { ward, policy, .. } => {
                assert_eq!(ward, vec![3u8; 32], "the SELECTED ward, not the first");
                assert!(policy.contact_approval);
                assert_eq!(policy.unknown_sender_mail, "reject");
                assert_eq!(policy.feed_sources, "block");
                assert_eq!(policy.content_notify, Some(true));
                let content = policy
                    .content_policy
                    .expect("the content pillar is authored");
                assert_eq!(content.nsfw, ContentFloor::Block);
                assert_eq!(content.spam, ContentFloor::Inherit);
                assert_eq!(
                    policy.screen_time,
                    Some(fauna_core::screen_time::ScreenTimePolicy::default()),
                    "an untouched screen-time editor sends the all-None policy \
                     PRESENT — absent would mean leave-unchanged and could never \
                     clear a limit"
                );
            }
            _ => panic!("expected Op::PolicyUpdate"),
        }
    }

    /// Two wards' children must be scope-addressable per row (the A6 nesting
    /// lesson), and the notices readout renders only for a ward that has counts.
    #[test]
    fn ward_rows_scope_their_children_and_notices_render_only_when_present() {
        let mut noisy = ward(2, "kid", ReachPolicy::default());
        noisy.content_notices = vec![
            FamilyContentNotice {
                category: "spam".into(),
                count: 3,
                ..Default::default()
            },
            FamilyContentNotice {
                category: "nsfw".into(),
                count: 1,
                ..Default::default()
            },
        ];
        let app = guardian_app(vec![noisy, ward(3, "sibling", ReachPolicy::default())]);

        assert_eq!(count_id(&app, "family-ward-item"), 2);
        assert_eq!(
            count_scoped(&app, "family-ward-handle", "family-ward-item", 0),
            1
        );
        assert_eq!(
            count_scoped(&app, "family-ward-handle", "family-ward-item", 1),
            1
        );
        assert_eq!(
            count_id(&app, "family-ward-content-notices"),
            1,
            "only the ward with counts renders the readout"
        );
        assert_eq!(
            count_scoped(&app, "family-ward-content-notices", "family-ward-item", 0),
            1
        );
        // Category + count only — never content, never an id.
        let readout = text_of(&app, "family-ward-content-notices");
        assert!(readout.contains("Spam: 3 flagged today"), "got {readout:?}");
        assert!(readout.contains("Adult content: 1 flagged today"));
    }

    /// A `mail_hold` row renders its `peer_address` (its `summary` is always
    /// empty — a subject line is content), and deciding it passes the entry's own
    /// key through: the message id, never the sender address, so approving one
    /// held message never sweeps every message from that sender.
    #[test]
    fn approval_rows_scope_their_buttons_and_decide_carries_the_entrys_key() {
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                wards: vec![ward(2, "kid", ReachPolicy::default())],
                approvals: vec![
                    FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                        kind: "contact".into(),
                        peer_actor_id: ByteBuf::from(vec![7u8; 32]),
                        summary: "hi from bob".into(),
                        ..Default::default()
                    },
                    FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                        kind: "mail_hold".into(),
                        peer_address: "stranger@example.com".into(),
                        message_id: ByteBuf::from(vec![9u8; 32]),
                        ..Default::default()
                    },
                    // A truthfully-empty null-path sender (`MAIL FROM:<>`).
                    FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                        kind: "mail_hold".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );

        assert_eq!(count_id(&app, "family-approval-item"), 3);
        assert_eq!(count_id(&app, "family-approval-approve-button"), 3);
        for i in 0..3 {
            assert_eq!(
                count_scoped(
                    &app,
                    "family-approval-approve-button",
                    "family-approval-item",
                    i
                ),
                1
            );
            assert_eq!(
                count_scoped(
                    &app,
                    "family-approval-deny-button",
                    "family-approval-item",
                    i
                ),
                1
            );
        }
        let texts: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "family-approval-item")
            .map(|e| e.text)
            .collect();
        assert_eq!(texts[0], "hi from bob");
        assert_eq!(texts[1], "stranger@example.com");
        assert_eq!(texts[2], t::APPROVAL_NO_SENDER, "never a blank row");

        let op = apply_local(
            &mut app,
            Action::Decide {
                index: 1,
                approve: true,
            },
        )
        .expect("approve returns an op");
        match op {
            Op::Decide { entry, approve, .. } => {
                assert!(approve);
                assert_eq!(entry.kind, "mail_hold");
                assert_eq!(entry.message_id.as_slice(), [9u8; 32]);
                assert!(
                    entry.peer_actor_id.is_empty(),
                    "a mail sender has no actor on this nest"
                );
            }
            _ => panic!("expected Op::Decide"),
        }
        // A stale index (the queue moved under a slow click) drops the gesture
        // rather than deciding the wrong row.
        assert!(
            apply_local(
                &mut app,
                Action::Decide {
                    index: 9,
                    approve: true
                }
            )
            .is_none()
        );
    }

    /// The v1.x kinds the queue gained with the three follow-on flows render
    /// through the same shared rule, each off its own field: a
    /// `contact_request`'s nest-joined `peer_handle` (the ask names *who*, never
    /// *why*, so it carries no summary at all — `family-safety.md`
    /// § Child-initiated contact requests), a `dm_hold`'s external
    /// `peer_address` (the message is sealed to the ward — § The bridge-DM
    /// gate), and a `feed_source`'s ward-authored label on `summary`. Binding
    /// `summary` blindly would leave the first two blank beside live
    /// Approve/Deny buttons — the defect windows once shipped for `mail_hold`.
    #[test]
    fn the_v1x_approval_kinds_each_render_their_own_field_never_a_blank_row() {
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                wards: vec![ward(2, "kid", ReachPolicy::default())],
                approvals: vec![
                    FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                        kind: "contact_request".into(),
                        peer_actor_id: ByteBuf::from(vec![7u8; 32]),
                        peer_handle: "alice".into(),
                        ..Default::default()
                    },
                    FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                        kind: "dm_hold".into(),
                        bridge_id: "nostr".into(),
                        peer_address: "npub1stranger".into(),
                        ..Default::default()
                    },
                    FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                        kind: "feed_source".into(),
                        bridge_id: "bluesky".into(),
                        operation: "follow".into(),
                        target: "did:plc:example".into(),
                        summary: "a science feed".into(),
                        ..Default::default()
                    },
                    // An off-nest peer has no local handle to join, so the ask
                    // arrives nameless — the localized label, never a blank row.
                    FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                        kind: "contact_request".into(),
                        peer_actor_id: ByteBuf::from(vec![8u8; 32]),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );

        let texts: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "family-approval-item")
            .map(|e| e.text)
            .collect();
        assert_eq!(texts[0], "alice");
        assert_eq!(texts[1], "npub1stranger");
        assert_eq!(texts[2], "a science feed");
        assert_eq!(texts[3], t::APPROVAL_NO_SENDER, "never a blank row");

        // Deciding a `dm_hold` carries the `(bridge_id, peer_address)` key the
        // nest matches on — an external peer has no actor on this nest.
        let op = apply_local(
            &mut app,
            Action::Decide {
                index: 1,
                approve: true,
            },
        )
        .expect("approve returns an op");
        match op {
            Op::Decide { entry, approve, .. } => {
                assert!(approve);
                assert_eq!(entry.kind, "dm_hold");
                assert_eq!(entry.bridge_id, "nostr");
                assert_eq!(entry.peer_address, "npub1stranger");
                assert!(entry.peer_actor_id.is_empty());
            }
            _ => panic!("expected Op::Decide"),
        }
    }

    /// The guardian's three typed screen-time inputs round-trip: a stored
    /// policy renders back through the shared formatter as text the guardian
    /// could retype, and editing them saves the parsed minutes.
    ///
    /// The `HH:MM` ↔ minutes conversion is the whole reason those inputs are
    /// text: the wire stores 1260, no guardian would ever type it.
    #[test]
    fn the_screen_time_inputs_round_trip_stored_minutes_as_typed_text() {
        let stored = ReachPolicy {
            screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                window_start: Some(9 * 60),
                window_end: Some(21 * 60),
                daily_minutes: Some(90),
            }),
            ..Default::default()
        };
        let mut app = guardian_app(vec![ward(2, "kid", stored)]);
        assert_eq!(
            text_of(&app, "family-policy-screen-window-start-input"),
            "09:00"
        );
        assert_eq!(
            text_of(&app, "family-policy-screen-window-end-input"),
            "21:00"
        );
        assert_eq!(
            text_of(&app, "family-policy-screen-daily-minutes-input"),
            "90"
        );

        // Retype the bedtime window and save.
        set_field(
            &mut app.family,
            FamilyField::ScreenWindowStart,
            "7:30".into(),
        );
        set_field(
            &mut app.family,
            FamilyField::ScreenWindowEnd,
            "20:00".into(),
        );
        set_field(
            &mut app.family,
            FamilyField::ScreenDailyMinutes,
            "45".into(),
        );
        let op = apply_local(&mut app, Action::SavePolicy).expect("save returns an op");
        match op {
            Op::PolicyUpdate { policy, .. } => {
                let screen = policy.screen_time.expect("the pillar is authored");
                assert_eq!(screen.window_start, Some(7 * 60 + 30));
                assert_eq!(screen.window_end, Some(20 * 60));
                assert_eq!(screen.daily_minutes, Some(45));
            }
            _ => panic!("expected Op::PolicyUpdate"),
        }
    }

    /// Clearing every screen-time field is how a guardian REMOVES the limits —
    /// so an all-empty editor must still save, sending the all-`None` policy
    /// present rather than refusing or sending absent.
    #[test]
    fn emptying_the_screen_time_fields_clears_the_limits() {
        let stored = ReachPolicy {
            screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                window_start: Some(9 * 60),
                window_end: Some(21 * 60),
                daily_minutes: Some(90),
            }),
            ..Default::default()
        };
        let mut app = guardian_app(vec![ward(2, "kid", stored)]);
        for field in [
            FamilyField::ScreenWindowStart,
            FamilyField::ScreenWindowEnd,
            FamilyField::ScreenDailyMinutes,
        ] {
            set_field(&mut app.family, field, String::new());
        }
        let op = apply_local(&mut app, Action::SavePolicy).expect("an empty editor still saves");
        match op {
            Op::PolicyUpdate { policy, .. } => assert_eq!(
                policy.screen_time,
                Some(fauna_core::screen_time::ScreenTimePolicy::default()),
                "every limit removed"
            ),
            _ => panic!("expected Op::PolicyUpdate"),
        }
    }

    /// A screen-time entry the policy cannot hold is refused HERE, on the shared
    /// rule the nest would refuse it by, and lands on `error-message` with no
    /// round trip — so the guardian reads the reason rather than a generic
    /// transport failure, and the rest of the editor is left untouched.
    #[test]
    fn an_unholdable_screen_time_entry_is_refused_client_side() {
        let mut app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);
        // A half-set window: bounds come in pairs (§ Screen time's write
        // validation), so this is refused before any RPC.
        set_field(
            &mut app.family,
            FamilyField::ScreenWindowStart,
            "09:00".into(),
        );
        assert!(
            apply_local(&mut app, Action::SavePolicy).is_none(),
            "a refused policy never reaches the nest"
        );
        let error = app
            .errors
            .get(&Page::Family)
            .expect("the reason lands on error-message");
        assert!(
            !error.is_empty(),
            "the guardian must be told WHY, not just refused"
        );

        // Completing the pair clears the refusal and the save proceeds.
        set_field(
            &mut app.family,
            FamilyField::ScreenWindowEnd,
            "21:00".into(),
        );
        assert!(apply_local(&mut app, Action::SavePolicy).is_some());
        assert!(!app.errors.contains_key(&Page::Family));
    }

    /// Both usage readouts render only under a declared budget, and render the
    /// SAME figure through the same shared composer — the transparency rule
    /// (§ Screen time: "the ward's summary shows the same number").
    #[test]
    fn the_ward_usage_readout_renders_only_under_a_budget() {
        let budgeted = ReachPolicy {
            screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                daily_minutes: Some(90),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Guardian side: one indexed readout, scoped inside its own ward row.
        let mut ward_row = ward(2, "kid", budgeted.clone());
        ward_row.usage_today_minutes = Some(30);
        let app = guardian_app(vec![ward_row]);
        assert_eq!(
            count_scoped(&app, "family-ward-usage-today", "family-ward-item", 0),
            1
        );
        let guardian_text = text_of(&app, "family-ward-usage-today");

        // Ward side: the same figure, folded into `family-policy-summary`
        // rather than claiming a new ui.yaml id.
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                supervised_by: Some("mum".into()),
                policy: Some(budgeted),
                usage_today_minutes: Some(30),
                ..Default::default()
            },
        );
        let summary = text_of(&app, "family-policy-summary");
        assert!(
            summary.contains(&guardian_text),
            "the ward's summary must show the guardian's exact line; \
             summary={summary:?} guardian={guardian_text:?}"
        );

        // No budget → no accounting → no readout on either surface.
        let app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);
        assert_eq!(count_id(&app, "family-ward-usage-today"), 0);
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                supervised_by: Some("mum".into()),
                policy: Some(ReachPolicy::default()),
                usage_today_minutes: None,
                ..Default::default()
            },
        );
        assert!(!text_of(&app, "family-policy-summary").contains(&guardian_text));
    }

    /// A status read is what binds a guardian's screen-time edit to the ward's
    /// LOCK, without a restart — the page the ward is told to visit had just
    /// read the new policy, so the global surface must take it from there.
    #[test]
    fn a_status_read_feeds_the_global_screen_lock() {
        let mut app = family_app();
        assert!(
            crate::screen_lock::lock_message(&app.screen_lock).is_none(),
            "nothing locks before a status read"
        );
        load(
            &mut app,
            FamilyView {
                supervised_by: Some("mum".into()),
                policy: Some(ReachPolicy {
                    screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                        daily_minutes: Some(10),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                usage_today_minutes: Some(10),
                ..Default::default()
            },
        );
        assert!(
            crate::screen_lock::lock_message(&app.screen_lock).is_some(),
            "an exhausted budget from the status read locks the ward"
        );
        // And the lock REPLACES the page pane everywhere but Family, which stays
        // readable — the goal-doc invariant that a locked ward can always see
        // who supervises them and what the policy says.
        app.page = Page::Feed;
        let ids: Vec<String> = app.page_elements().into_iter().map(|e| e.id).collect();
        assert!(ids.iter().any(|i| i == "screen-time-lock"));
        assert!(ids.iter().any(|i| i == "screen-time-lock-message"));
        app.page = Page::Family;
        let ids: Vec<String> = app.page_elements().into_iter().map(|e| e.id).collect();
        assert!(
            !ids.iter().any(|i| i == "screen-time-lock"),
            "the Family page is exempt: {ids:?}"
        );
        assert!(ids.iter().any(|i| i == "family-policy-summary"));
    }

    /// **A same-page nav is a RELOAD**, not a no-op — the contract every app's
    /// e2e spells `reload = navigate` ("a fresh Page_Loaded -> LoadAsync
    /// refetch"), and the one this page depends on hardest: the guardian's
    /// approvals queue and the ward's usage figure are both nest-authoritative
    /// and change while no client is looking.
    ///
    /// Pinned here because tui gated the whole nav hook on the page EDGE until
    /// 2026-08-01, which made every `reload()` silently do nothing while the
    /// other apps refetched — a green driver ack for work never done
    /// (convention 11). It cost this slice a full e2e cycle before it was found.
    #[test]
    fn re_entering_the_family_tab_refetches_rather_than_no_opping() {
        let mut app = family_app();
        app.page = Page::Feed;
        assert!(
            matches!(
                app.apply(Page::Family),
                Some(crate::app::PageOp::Family(Op::Refresh { .. }))
            ),
            "the nav edge refetches"
        );
        assert!(
            matches!(
                app.apply(Page::Family),
                Some(crate::app::PageOp::Family(Op::Refresh { .. }))
            ),
            "and so does re-entering the tab you are already on — that is reload"
        );
    }

    /// The heartbeat seam actually yields a report: a supervised ward under a
    /// budget, after foreground use, produces an `Op::UsageReport` carrying the
    /// minutes and the device's UTC offset.
    ///
    /// This is the seam the `screen_time_heartbeat` agent command drives, and
    /// the one place a wiring break is invisible from the shared engine's own
    /// tier_1 tests — those prove the cadence rules, not that THIS client asks.
    #[test]
    fn the_heartbeat_seam_yields_a_report_for_a_budgeted_ward() {
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                supervised_by: Some("mum".into()),
                policy: Some(ReachPolicy {
                    screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                        daily_minutes: Some(120),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                usage_today_minutes: Some(0),
                ..Default::default()
            },
        );
        app.screen_lock.advance_test_clock(15 * 60);
        match due_usage_report(&mut app, true) {
            Some(Op::UsageReport { minutes, .. }) => assert_eq!(minutes, 15),
            other => panic!(
                "a budgeted ward with 15 foreground minutes owes a report, got {:?}",
                other.is_some()
            ),
        }

        // An UNSUPERVISED account owes nothing — no budget, no accounting, so
        // the tick costs an ordinary user nothing at all.
        let mut app = family_app();
        load(&mut app, FamilyView::default());
        app.screen_lock.advance_test_clock(15 * 60);
        assert!(due_usage_report(&mut app, true).is_none());
    }

    /// An identity change drops the lock. Without this a new account inherits
    /// the previous ward's lock — accusing a guardian the user does not have,
    /// and walling them out of every page but Family with no way to appeal.
    ///
    /// A tokio test because it drives the REAL `drop_authenticated_state`, whose
    /// sign-out leg needs a reactor: asserting against a hand-reset field would
    /// prove only that `Default` works, not that the teardown path calls it.
    #[tokio::test]
    async fn an_identity_change_drops_the_ward_lock() {
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                supervised_by: Some("mum".into()),
                policy: Some(ReachPolicy {
                    screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                        daily_minutes: Some(1),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                usage_today_minutes: Some(600),
                ..Default::default()
            },
        );
        assert!(crate::screen_lock::lock_message(&app.screen_lock).is_some());
        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert!(
            crate::screen_lock::lock_message(&app.screen_lock).is_none(),
            "the next identity must not inherit a lock"
        );
    }

    /// Contact pre-approval takes a hex actor id: a valid one reaches the nest
    /// and clears the buffer; a non-hex one is refused client-side onto
    /// `error-message` with no round trip.
    #[test]
    fn contact_add_refuses_a_non_hex_actor_id_client_side() {
        let mut app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);

        set_field(&mut app.family, FamilyField::ContactAdd, "zznothex".into());
        assert!(apply_local(&mut app, Action::AddContact).is_none());
        assert_eq!(
            app.errors.get(&Page::Family).map(String::as_str),
            Some(t::CONTACT_ADD_INVALID_ACTOR_ID)
        );

        set_field(
            &mut app.family,
            FamilyField::ContactAdd,
            format!("  {}  ", "07".repeat(32)),
        );
        let op = apply_local(&mut app, Action::AddContact).expect("a hex id returns an op");
        match op {
            Op::ContactAdd { ward, peer, .. } => {
                assert_eq!(ward, vec![2u8; 32]);
                assert_eq!(peer, vec![7u8; 32], "trimmed and hex-decoded");
            }
            _ => panic!("expected Op::ContactAdd"),
        }
        assert!(app.family.contact_input.is_empty(), "the buffer clears");
        assert!(!app.errors.contains_key(&Page::Family));
    }

    /// A value that is valid hex but not exactly 32 bytes must be refused the
    /// same as non-hex input — `hex::decode` alone accepts any length, only
    /// `ActorId::from_hex` enforces the actor-id contract. Without the length
    /// check this reaches `Op::ContactAdd`/`Op::Transfer` and is sent to the
    /// nest instead of refused client-side as the field's own doc promises.
    #[test]
    fn contact_add_and_transfer_refuse_a_wrong_length_hex_actor_id_client_side() {
        let mut app = guardian_app(vec![ward(2, "kid", ReachPolicy::default())]);
        let too_short = "07".repeat(31);

        set_field(&mut app.family, FamilyField::ContactAdd, too_short.clone());
        assert!(apply_local(&mut app, Action::AddContact).is_none());
        assert_eq!(
            app.errors.get(&Page::Family).map(String::as_str),
            Some(t::CONTACT_ADD_INVALID_ACTOR_ID)
        );

        app.errors.remove(&Page::Family);
        set_field(&mut app.family, FamilyField::Transfer, too_short);
        assert!(apply_local(&mut app, Action::Transfer).is_none());
        assert_eq!(
            app.errors.get(&Page::Family).map(String::as_str),
            Some(t::CONTACT_ADD_INVALID_ACTOR_ID)
        );
    }

    /// A pending proposal swaps the initiate row for the pending marker + cancel
    /// (so `is_visible("family-transfer-pending")` is the honest witness), and
    /// graduation is reveal-then-confirm.
    #[test]
    fn transfer_pending_swaps_the_row_and_graduation_is_reveal_then_confirm() {
        let mut pending_ward = ward(2, "kid", ReachPolicy::default());
        pending_ward.pending_transfer = Some(FamilyPendingTransferInfo {
            proposed_guardian_actor_id: ByteBuf::from(vec![3u8; 32]),
            proposed_guardian_handle: "otherparent".into(),
            created_at: 1,
            ..Default::default()
        });
        let mut app = guardian_app(vec![pending_ward]);

        assert_eq!(count_id(&app, "family-transfer-input"), 0);
        assert_eq!(count_id(&app, "family-transfer-button"), 0);
        assert_eq!(count_id(&app, "family-transfer-cancel-button"), 1);
        assert!(
            text_of(&app, "family-transfer-pending").contains("otherparent"),
            "the pending marker names the proposed guardian"
        );
        match apply_local(&mut app, Action::TransferCancel).expect("cancel returns an op") {
            Op::TransferCancel { ward, .. } => assert_eq!(ward, vec![2u8; 32]),
            _ => panic!("expected Op::TransferCancel"),
        }

        // Graduate: hidden until revealed, and the confirm label names the ward.
        assert_eq!(count_id(&app, "family-graduate-confirm-button"), 0);
        assert!(apply_local(&mut app, Action::RevealGraduate).is_none());
        assert_eq!(count_id(&app, "family-graduate-confirm-button"), 1);
        assert_eq!(
            text_of(&app, "family-graduate-confirm-button"),
            t::graduate_confirm_button("kid")
        );
        match apply_local(&mut app, Action::ConfirmGraduate).expect("confirm returns an op") {
            Op::Graduate { ward, .. } => assert_eq!(ward, vec![2u8; 32]),
            _ => panic!("expected Op::Graduate"),
        }
        assert!(!app.family.graduate_revealed, "the confirm step re-arms");
    }

    /// The incoming prompt renders for a proposed guardian with NO other family
    /// relationship — which is precisely what the widened `family-tab` gate
    /// exists for — and its buttons are row-scoped.
    #[test]
    fn incoming_transfer_rows_render_for_a_proposed_guardian_and_scope_their_buttons() {
        let mut app = family_app();
        load(
            &mut app,
            FamilyView {
                incoming: vec![FamilyIncomingTransferInfo {
                    supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                    supervised_handle: "kid".into(),
                    guardian_handle: "parent".into(),
                    created_at: 1,
                    ..Default::default()
                }],
                ..Default::default()
            },
        );

        assert!(app.has_family, "an incoming proposal alone opens the gate");
        assert!(app.sidebar_pages().contains(&Page::Family));
        assert_eq!(count_id(&app, "family-incoming-transfer-item"), 1);
        assert_eq!(
            count_scoped(
                &app,
                "family-incoming-transfer-accept-button",
                "family-incoming-transfer-item",
                0
            ),
            1
        );
        let text = text_of(&app, "family-incoming-transfer-item");
        assert!(
            text.contains("parent") && text.contains("kid"),
            "got {text:?}"
        );

        match apply_local(
            &mut app,
            Action::DecideIncoming {
                index: 0,
                accept: true,
            },
        )
        .expect("accept returns an op")
        {
            Op::TransferAccept { ward, .. } => assert_eq!(ward, vec![2u8; 32]),
            _ => panic!("expected Op::TransferAccept"),
        }
        match apply_local(
            &mut app,
            Action::DecideIncoming {
                index: 0,
                accept: false,
            },
        )
        .expect("decline returns an op")
        {
            Op::TransferDecline { ward, .. } => assert_eq!(ward, vec![2u8; 32]),
            _ => panic!("expected Op::TransferDecline"),
        }
    }

    /// The supervised side: the guardian handle + the read-only summary, the
    /// global `supervised-indicator` text, and the gate. An ordinary account
    /// gets none of it and no `family-tab` row (fail-closed).
    #[test]
    fn the_supervised_side_renders_the_summary_and_opens_the_gate() {
        let mut app = family_app();
        assert!(!app.has_family, "fail-closed before the status read lands");
        assert!(!app.sidebar_pages().contains(&Page::Family));
        assert_eq!(supervised_indicator_text(&app.family), None);

        let reply = FamilyStatusReply {
            supervised_by: Some(FamilyGuardianInfo {
                actor_id: ByteBuf::from(vec![1u8; 32]),
                handle: "parent".into(),
                ..Default::default()
            }),
            policy: Some(ReachPolicy {
                contact_approval: true,
                unknown_sender_mail: "hold".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        load(
            &mut app,
            FamilyView {
                supervised_by: reply.supervised_by.map(|g| g.handle),
                policy: reply.policy,
                ..Default::default()
            },
        );

        assert!(app.has_family);
        assert!(app.sidebar_pages().contains(&Page::Family));
        assert_eq!(
            supervised_indicator_text(&app.family).as_deref(),
            Some(t::supervised_indicator("parent").as_str())
        );
        assert!(text_of(&app, "family-guardian-handle").contains("parent"));
        let summary = text_of(&app, "family-policy-summary");
        assert!(!summary.is_empty(), "the ward always sees the policy");
        assert!(
            summary.contains("Hold for review"),
            "string knobs render their localized label; got {summary:?}"
        );
        // Supervised-only: no guardian editor.
        assert_eq!(count_id(&app, "family-policy-save-button"), 0);

        // A later read reporting no relationship closes the gate again.
        load(&mut app, FamilyView::default());
        assert!(!app.has_family);
        assert!(!app.sidebar_pages().contains(&Page::Family));
    }

    /// A refresh keeps the editor pointed at the SAME ward when it survives, and
    /// falls back to the first ward when it does not — the windows `LoadAsync`
    /// rule, and what stops a save from landing on the wrong child.
    #[test]
    fn a_refresh_re_selects_the_same_ward_or_falls_back_to_the_first() {
        let mut app = guardian_app(vec![
            ward(2, "kid", ReachPolicy::default()),
            ward(3, "sibling", ReachPolicy::default()),
        ]);
        assert_eq!(app.family.selected_ward.as_deref(), Some(&[2u8; 32][..]));

        apply_local(&mut app, Action::SelectWard { index: 1 });
        assert_eq!(app.family.selected_ward.as_deref(), Some(&[3u8; 32][..]));

        // Same two wards, reordered: the selection follows the ward, not the row.
        load(
            &mut app,
            FamilyView {
                wards: vec![
                    ward(3, "sibling", ReachPolicy::default()),
                    ward(2, "kid", ReachPolicy::default()),
                ],
                ..Default::default()
            },
        );
        assert_eq!(app.family.selected_ward.as_deref(), Some(&[3u8; 32][..]));

        // The selected ward graduated away: fall back to the first remaining one.
        load(
            &mut app,
            FamilyView {
                wards: vec![ward(2, "kid", ReachPolicy::default())],
                ..Default::default()
            },
        );
        assert_eq!(app.family.selected_ward.as_deref(), Some(&[2u8; 32][..]));

        // The last ward graduated: no selection, no editor.
        load(&mut app, FamilyView::default());
        assert_eq!(app.family.selected_ward, None);
        assert_eq!(count_id(&app, "family-policy-save-button"), 0);
    }

    /// A ward's persisted policy loads into the editor through the shared
    /// fail-closed parse, so a value this build cannot name renders (and
    /// re-saves) as the STRICT option rather than the permissive one.
    #[test]
    fn an_unparseable_persisted_knob_loads_as_the_strict_option() {
        let app = guardian_app(vec![ward(
            2,
            "kid",
            ReachPolicy {
                unknown_sender_mail: "quarantine".into(),
                feed_sources: "curated".into(),
                content_policy: Some(ContentPolicy {
                    nsfw: ContentFloor::Unknown,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )]);
        assert_eq!(app.family.editor.unknown_sender, UnknownSenderMail::Hold);
        assert_eq!(app.family.editor.feed_sources, FeedSources::Block);
        assert_eq!(app.family.editor.content[0], ContentFloor::Block);
        assert_eq!(
            text_of(&app, "family-policy-unknown-sender-select"),
            "Hold for review"
        );
        assert_eq!(text_of(&app, "family-policy-content-nsfw-select"), "Block");
    }

    /// Folding a `Failed` outcome lands on the page error; a `Loaded` clears it.
    #[test]
    fn the_fold_bridges_a_failure_onto_the_page_error() {
        let mut app = family_app();
        apply_outcome(&mut app, Outcome::Failed("nope".to_string()));
        assert_eq!(
            app.errors.get(&Page::Family).map(String::as_str),
            Some("nope")
        );
        load(&mut app, FamilyView::default());
        assert!(
            !app.errors.contains_key(&Page::Family),
            "a successful load clears it"
        );
    }

    // ── The offline gate's family declarations (W4 (account-data-plane.md § Workstreams) phase 4, row 43) ─────────

    /// One instance of **every** [`Action`] variant, plus a second
    /// `DecideIncoming` because its two `accept` arms answer different kinds.
    ///
    /// Hand-built for the reason the admin corpus records: walk invariants
    /// I6/I7 see only what a page *paints*, and this page paints nothing at all
    /// without a nest — it is gated on a `fauna.family.status` reply reporting a
    /// relationship, so on the offline fixture there is no ward, no queue and no
    /// editor. Every declaration below would therefore go unchecked, and a typo
    /// in any of them reads as `Available` forever (`affordance`'s ruling 2).
    fn every_action() -> Vec<Action> {
        vec![
            Action::SelectWard { index: 0 },
            Action::SetContactApproval(true),
            Action::SetFederation(true),
            Action::SetContentNotify(true),
            Action::SetUnknownSender("Block".to_string()),
            Action::SetFeedSources("Block".to_string()),
            Action::SetContentFloor {
                category: 0,
                label: "Block".to_string(),
            },
            Action::SavePolicy,
            Action::AddContact,
            Action::Transfer,
            Action::TransferCancel,
            Action::RevealGraduate,
            Action::ConfirmGraduate,
            Action::Decide {
                index: 0,
                approve: true,
            },
            Action::DecideIncoming {
                index: 0,
                accept: true,
            },
            Action::DecideIncoming {
                index: 0,
                accept: false,
            },
            Action::DeviceMark {
                ward: vec![1u8; 32],
                device_id: "d".to_string(),
                marked: true,
            },
        ]
    }

    /// `Action` has this many variants; [`every_action`] carries one instance of
    /// each, plus the second `DecideIncoming`.
    const ACTION_COUNT: usize = 16;

    #[test]
    fn every_family_action_is_in_the_corpus() {
        assert_eq!(
            every_action().len(),
            ACTION_COUNT + 1,
            "a new `Action` variant must be added to `every_action` — otherwise \
             its wire-kind declaration is never checked against the registry"
        );
    }

    /// I7 at the type level: every kind this page declares must be one the
    /// shared table knows. An unregistered kind reads as `Available` by design
    /// (ruling 2 — forward compatibility), so a misspelling here silently
    /// *ungates* that affordance and nothing reports it.
    #[test]
    fn every_declared_family_kind_is_registered() {
        crate::test_support::assert_every_wire_kind_is_registered(every_action(), |a| {
            a.wire_kind()
        });
    }

    /// The guardianship-re-pointing half desensitizes offline; the editor half
    /// does not. The **exact kind** is asserted, not just its class: with four
    /// sibling `fauna.family.transfer*` kinds sharing `OnlineOnly`, a class-only
    /// assertion passes when two arms are swapped, and the declaration's whole
    /// value is that a later reclassification reaches this page for free.
    #[test]
    fn family_declares_the_exact_kind_per_gesture() {
        use fauna_protocol::offline_class::{OfflineClass, offline_class};
        for (action, expected, class) in [
            (
                Action::ConfirmGraduate,
                "fauna.family.graduate",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::Transfer,
                "fauna.family.transfer",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::TransferCancel,
                "fauna.family.transfer.cancel",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::DecideIncoming {
                    index: 0,
                    accept: true,
                },
                "fauna.family.transfer.accept",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::DecideIncoming {
                    index: 0,
                    accept: false,
                },
                "fauna.family.transfer.decline",
                OfflineClass::OnlineOnly,
            ),
            // The editor half — a guardian may compose a ward's policy, add a
            // contact and decide the queue with no nest in reach.
            (
                Action::SavePolicy,
                "fauna.family.policy.update",
                OfflineClass::OfflineSafe,
            ),
            // A guardian-tier feature limit rides the same policy write.
            (
                Action::SaveFeatureLimit,
                "fauna.family.policy.update",
                OfflineClass::OfflineSafe,
            ),
            (
                Action::RemoveFeatureLimit,
                "fauna.family.policy.update",
                OfflineClass::OfflineSafe,
            ),
            (
                Action::DeviceMark {
                    ward: vec![1u8; 32],
                    device_id: "d".to_string(),
                    marked: true,
                },
                "fauna.family.device.mark",
                OfflineClass::OfflineSafe,
            ),
            (
                Action::AddContact,
                "fauna.family.contact.add",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::Decide {
                    index: 0,
                    approve: true,
                },
                "fauna.family.approvals.decide",
                OfflineClass::OfflineQueued,
            ),
        ] {
            assert_eq!(
                action.wire_kind(),
                Some(expected),
                "{action:?} must declare {expected}"
            );
            assert_eq!(
                offline_class(expected),
                Some(class),
                "{expected} changed class — the gate's behaviour on this page \
                 changed with it, so re-read the declaration's reasoning"
            );
        }
    }

    /// The editor's own buffer writes issue nothing, and must stay `None`:
    /// declaring a kind for them would gate the very controls a guardian uses
    /// to compose a policy they can save later.
    #[test]
    fn the_family_editor_buffers_declare_nothing() {
        for action in [
            Action::SelectWard { index: 0 },
            Action::SetContactApproval(true),
            Action::SetFederation(true),
            Action::SetContentNotify(true),
            Action::SetUnknownSender("Block".to_string()),
            Action::SetFeedSources("Block".to_string()),
            Action::SetContentFloor {
                category: 0,
                label: "Block".to_string(),
            },
            Action::RevealGraduate,
        ] {
            assert_eq!(
                action.wire_kind(),
                None,
                "{action:?} issues no call — declaring one would gate a buffer \
                 write"
            );
        }
    }

    // ---- clause 2: the last-known supervision snapshot -------------------
    // `family-safety.md` § Content policy, the unfetched-policy ruling. These
    // drive the real seam (the app's own credential store through
    // `session::registry`), never a stub — the bug the ruling exists to close
    // is precisely a cold launch reading persistence that isn't there.

    fn bedtime_status() -> FamilyStatusReply {
        FamilyStatusReply {
            supervised_by: Some(FamilyGuardianInfo {
                actor_id: ByteBuf::from(vec![7u8; 32]),
                handle: "parent".to_string(),
                ..Default::default()
            }),
            policy: Some(ReachPolicy {
                content_policy: Some(ContentPolicy {
                    nsfw: ContentFloor::Block,
                    ..Default::default()
                }),
                content_notify: Some(true),
                screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                    window_start: Some(1260),
                    window_end: Some(420),
                    daily_minutes: Some(90),
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// A ward whose last successful read saw a floor gets that floor back at
    /// the NEXT launch, before any read — the whole point of clause 2. Without
    /// it, launching offline renders unsupervised and the bedtime lock (pure
    /// client-local clock) simply does not come up.
    #[test]
    fn a_cold_launch_restores_the_last_known_floor_before_any_read() {
        let mut app = authed_app();
        let actor = app.session.as_ref().unwrap().actor_id.clone();
        persist_supervision_snapshot(&app, &SupervisionSnapshot::from_status(&bedtime_status()));

        // A fresh launch of the same account: nothing in memory yet, exactly
        // as `session::establish` leaves it before the read is fired.
        let mut fresh = authed_app();
        std::mem::swap(&mut app.credentials, &mut fresh.credentials);
        fresh.has_family = false;
        restore_supervision_snapshot(&mut fresh, &actor);

        assert!(
            fresh.has_family,
            "the Family page must be reachable while a restored lock is up — \
             § Screen time requires the ward can always see who supervises them"
        );
        assert_eq!(fresh.family.supervised_by.as_deref(), Some("parent"));
        assert_eq!(
            fresh
                .content_policy
                .verdict_for(&[fauna_core::content_category::ContentLabelEntry {
                    category: "nsfw".to_string(),
                    confidence_per_mille: 1000,
                }])
                .verdict,
            fauna_core::obligation::RenderVerdict::Block,
            "the restored floor actually binds on a render, not just in a field"
        );
    }

    /// The `content_notify` half of the restore, asserted through the behaviour
    /// it drives rather than the field it sets: Guardian Notify counting must
    /// resume at a cold launch too, or the guardian silently stops being told
    /// their floor is acting for every ward whose device restarts offline.
    ///
    /// ⚠ Added because a mutation run found this half **uncovered** — the
    /// restore could drop `set_ward_content_notify` entirely and all six of the
    /// other pins here stayed green.
    #[test]
    fn a_cold_launch_resumes_guardian_notify_counting() {
        let mut app = authed_app();
        let actor = app.session.as_ref().unwrap().actor_id.clone();
        persist_supervision_snapshot(&app, &SupervisionSnapshot::from_status(&bedtime_status()));

        let mut fresh = authed_app();
        std::mem::swap(&mut app.credentials, &mut fresh.credentials);
        restore_supervision_snapshot(&mut fresh, &actor);

        fresh.content_policy.note_enforcement(
            "post-1",
            &[fauna_core::content_category::ContentLabelEntry {
                category: "nsfw".to_string(),
                confidence_per_mille: 1000,
            }],
        );
        // The accumulator's first report is eager, so this needs no clock
        // control and no wait — `None` here means nothing was ever counted.
        let (entries, _offset) = fresh
            .content_policy
            .take_notify_report()
            .expect("a restored `content_notify` counts the guardian floor biting");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, "nsfw");
        assert_eq!(entries[0].count, 1);
    }

    /// Clause 3's declared residual, and the guard that keeps this feature free
    /// for everyone else: a device that has never completed one successful read
    /// has nothing to remember and enforces nothing.
    #[test]
    fn a_device_that_has_never_read_restores_nothing() {
        let mut app = authed_app();
        app.has_family = false;
        restore_supervision_snapshot(&mut app, "never-read-this-actor");
        assert!(!app.has_family);
        assert_eq!(app.family.supervised_by, None);
        assert!(crate::screen_lock::lock_message(&app.screen_lock).is_none());
    }

    /// The fail direction: a corrupted slot is "no information", never a
    /// fabricated floor and never a panic.
    #[test]
    fn a_malformed_slot_restores_nothing() {
        let mut app = authed_app();
        let actor = app.session.as_ref().unwrap().actor_id.clone();
        crate::session::registry(&app).set_supervision_snapshot_json(&actor, "{{ not json");
        app.has_family = false;
        restore_supervision_snapshot(&mut app, &actor);
        assert!(!app.has_family);
        assert_eq!(app.family.supervised_by, None);
    }

    /// A graduated ward: the last successful read reported no guardianship, so
    /// the persisted snapshot is the unsupervised one and a later launch must
    /// NOT revive the floor their guardian used to set.
    #[test]
    fn a_graduated_wards_snapshot_revives_no_floor() {
        let mut app = authed_app();
        let actor = app.session.as_ref().unwrap().actor_id.clone();
        // First the supervised read, then the graduation read — the second
        // overwrites, which is clause 3's clearing rule.
        persist_supervision_snapshot(&app, &SupervisionSnapshot::from_status(&bedtime_status()));
        persist_supervision_snapshot(
            &app,
            &SupervisionSnapshot::from_status(&FamilyStatusReply::default()),
        );

        app.has_family = false;
        app.family.supervised_by = None;
        restore_supervision_snapshot(&mut app, &actor);
        assert!(!app.has_family, "no guardian, no Family gate");
        assert_eq!(app.family.supervised_by, None);
        assert!(
            crate::screen_lock::lock_message(&app.screen_lock).is_none(),
            "a graduated ward is never locked by their old policy"
        );
    }

    /// The write half, driven through the real fold: a successful read persists
    /// the snapshot as a side effect of `apply_outcome`, with no separate call
    /// for a per-app leg to forget.
    #[test]
    fn a_successful_read_persists_the_snapshot_through_apply_outcome() {
        let mut app = authed_app();
        let actor = app.session.as_ref().unwrap().actor_id.clone();
        assert_eq!(
            crate::session::registry(&app).supervision_snapshot_json(&actor),
            None,
            "nothing persisted before the read"
        );

        let status = bedtime_status();
        apply_outcome(
            &mut app,
            Outcome::Loaded(Box::new(FamilyView {
                supervised_by: status.supervised_by.clone().map(|g| g.handle),
                snapshot: SupervisionSnapshot::from_status(&status),
                policy: status.policy.clone(),
                ..Default::default()
            })),
        );

        let raw = crate::session::registry(&app)
            .supervision_snapshot_json(&actor)
            .expect("a successful read persists");
        let snap = SupervisionSnapshot::from_json(&raw).expect("and it round-trips");
        assert!(snap.is_supervised());
        assert_eq!(snap.content_policy.unwrap().nsfw, ContentFloor::Block);
        assert_eq!(snap.screen_time.unwrap().daily_minutes, Some(90));
        assert!(snap.content_notify);
    }

    /// Clause 1, at this seam: a FAILED read must not move enforcement state —
    /// including the persisted half, which would otherwise turn one transient
    /// outage into a floor no later launch can restore.
    #[test]
    fn a_failed_read_never_touches_the_persisted_snapshot() {
        let mut app = authed_app();
        let actor = app.session.as_ref().unwrap().actor_id.clone();
        persist_supervision_snapshot(&app, &SupervisionSnapshot::from_status(&bedtime_status()));

        apply_outcome(&mut app, Outcome::Failed("nest unreachable".to_string()));

        let raw = crate::session::registry(&app)
            .supervision_snapshot_json(&actor)
            .expect("the snapshot survives a failed read");
        assert!(
            SupervisionSnapshot::from_json(&raw)
                .unwrap()
                .is_supervised(),
            "a failed read yields no information — it does not clear the floor"
        );
    }
}

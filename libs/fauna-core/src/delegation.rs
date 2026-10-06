//! Task-delegation policy — the pure participant-class model and the automatic
//! policy-order eligibility function.
//!
//! Authority: `docs/goal/behavior/participants.md` § Task delegation (Q-B/Q-C,
//! ratified with the user 2026-07-06). This module owns the *live* half of task
//! delegation — the participant **class** and the pure decision of **who should
//! currently run a task kind** — while the *at-rest* half (the user's pins,
//! [`crate::data::DelegationConfig`]) lives in [`crate::data`] beside the other
//! account-state records.
//!
//! # What this module is (and is not)
//!
//! [`current_candidates`] is a **pure, liveness-agnostic** function: given the
//! set of participants a caller currently knows about (each tagged with its
//! class and, for nests, whether it holds a sufficient content-processing grant
//! for the kind) plus the user's [`DelegationConfig`], it returns the
//! participants that should contend to run a task kind *right now*. It does
//! **not** model network liveness, hold a lease, or pick a single winner among
//! equals — those are the heartbeat-lease layer's job (participants.md § Data
//! shape: "the lease is live advisory state, never persisted"). The caller
//! decides which participants to pass in; the lease breaks ties within the
//! returned set by liveness.
//!
//! # Policy order (participants.md § Task delegation → Policy order)
//!
//! A task kind runs on: **(1)** a nest holding a sufficient grant for it, else
//! **(2)** a plugged-in desktop, else **(3)** it waits. **Battery-mobile
//! participants never run heavy task kinds** — not even as a last resort, not
//! even when explicitly pinned (the strongest reading of "never … not even as a
//! last resort"; the UI additionally prevents pinning to mobile — slice 4). A
//! **pin** (participants.md § Concepts → Assignment) is the escape hatch: it
//! collapses the candidate set to exactly the pinned participant, which runs iff
//! currently eligible, else the kind waits for it (we never silently run a
//! pinned kind elsewhere — that would defeat the user's explicit choice).

use std::collections::HashMap;

use crate::data::{DelegationConfig, ParticipantRef};
use crate::localized::LocalizedText;

/// A participant's delegation-eligibility class (participants.md § The
/// participant model). Live/derived state — a nest is always [`AlwaysOnNest`];
/// a desktop app reports its current plugged-in/on-AC state
/// ([`PluggedInDesktop`] while on mains, [`BatteryMobile`] on battery); iOS and
/// Android are always [`BatteryMobile`]. Serde-ready because a participant
/// reports its class in the lease heartbeat (slice 2).
///
/// [`AlwaysOnNest`]: ParticipantClass::AlwaysOnNest
/// [`PluggedInDesktop`]: ParticipantClass::PluggedInDesktop
/// [`BatteryMobile`]: ParticipantClass::BatteryMobile
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ParticipantClass {
    /// A nest — always powered, always network-reachable. Tier 1 (runs a task
    /// kind only when it also holds a sufficient grant for it).
    AlwaysOnNest,
    /// A desktop app currently on mains/AC power. Tier 2.
    PluggedInDesktop,
    /// A phone/tablet, or a desktop currently on battery. **Never** runs a
    /// heavy task kind (participants.md § Don't do these).
    BatteryMobile,
    /// A class a newer build reports and this one does not name — the exact
    /// string read, which the nest stores and returns verbatim
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*). It is treated as [`Self::BatteryMobile`], the most restrictive
    /// class ([`Self::effective`]).
    #[serde(untagged)]
    Other(String),
}

impl ParticipantClass {
    /// The known class this one behaves as: itself, or
    /// [`Self::BatteryMobile`] for a class this build does not name.
    pub fn effective(&self) -> Self {
        match self {
            Self::Other(_) => Self::BatteryMobile,
            known => known.clone(),
        }
    }
}

/// One participant as an input to [`current_candidates`]. The caller builds one
/// per participant it currently knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantDescriptor {
    /// The participant's key into the shared participant model.
    pub reference: ParticipantRef,
    /// Its current delegation class.
    pub class: ParticipantClass,
    /// **Nests only:** whether this nest holds a grant sufficient to run *this*
    /// task kind (the caller computes it per-kind — participants.md § Dispatch
    /// by kind). Ignored for client participants, which run with their own keys
    /// and need no grant.
    ///
    /// Every live kind now has a nest-held sufficient grant: `content-rescore`
    /// takes the `content.read{mail}` + `content.label-write` pair, and
    /// `backup-upload` — since the nest-side segment-backup redesign made the
    /// source nest the writer — takes the owner's `NestBackupKey` plus ≥1
    /// registered destination (backup-restore.md § Background Tasks). *(This
    /// doc previously said the opposite for `backup-upload`, from the era when
    /// its upload ran client-side under the client's own `BackupKey`.)*
    ///
    /// The client-side caller derives this from the observed lease holder's
    /// class rather than from its own grant-event log: the `NestBackupKey` row
    /// is deliberately outside the signed log (`ui/nests.md` § Trust facet —
    /// backup rows), and a nest heartbeats a lease only when its own
    /// sufficiency scan passed. See `fauna_client_delegation::LeaseCoordinator`.
    pub holds_grant: bool,
}

impl ParticipantDescriptor {
    /// The policy tier this participant occupies for a task kind, or `None` if
    /// it is ineligible. Lower number = higher priority. Battery-mobile is
    /// always ineligible; a nest is eligible only while it holds a sufficient
    /// grant.
    fn tier(&self) -> Option<u8> {
        match self.class.effective() {
            ParticipantClass::AlwaysOnNest if self.holds_grant => Some(1),
            ParticipantClass::AlwaysOnNest => None,
            ParticipantClass::PluggedInDesktop => Some(2),
            ParticipantClass::BatteryMobile | ParticipantClass::Other(_) => None,
        }
    }
}

/// The participants that should currently contend to run `task_kind`, ranked
/// into a single policy tier. Empty ⇒ the task kind **waits** (no eligible
/// participant, or a pin to an ineligible/unknown one).
///
/// - **Pinned** (`config` has a [`TaskAssignment`] for `task_kind` with
///   `pinned_to = Some(_)`): returns just the pinned participant iff it is
///   present in `participants` and eligible; else empty (the kind waits for it).
/// - **Automatic** (no assignment, or `pinned_to = None`): returns *all* members
///   of the single highest non-empty tier — tier 1 (eligible nests) if any,
///   else tier 2 (plugged-in desktops), else empty. The lease races among the
///   returned set to enforce exactly-one-runner.
///
/// The result never mixes tiers (the policy never falls to tier 2 while a tier-1
/// participant exists) and never includes a battery-mobile participant. Input
/// order is preserved within the winning tier for determinism.
///
/// [`TaskAssignment`]: crate::data::TaskAssignment
pub fn current_candidates(
    task_kind: &str,
    participants: &[ParticipantDescriptor],
    config: &DelegationConfig,
) -> Vec<ParticipantRef> {
    // A pin collapses the decision to exactly the pinned participant.
    if let Some(pin) = pinned_participant(task_kind, config) {
        return participants
            .iter()
            .find(|p| &p.reference == pin && p.tier().is_some())
            .map(|p| vec![p.reference.clone()])
            .unwrap_or_default();
    }

    // Automatic: the single best non-empty tier.
    let best_tier = participants
        .iter()
        .filter_map(ParticipantDescriptor::tier)
        .min();
    match best_tier {
        Some(tier) => participants
            .iter()
            .filter(|p| p.tier() == Some(tier))
            .map(|p| p.reference.clone())
            .collect(),
        None => Vec::new(),
    }
}

/// The participant a task kind is pinned to, if any.
fn pinned_participant<'a>(
    task_kind: &str,
    config: &'a DelegationConfig,
) -> Option<&'a ParticipantRef> {
    config
        .assignments
        .iter()
        .find(|a| a.task_kind == task_kind)
        .and_then(|a| a.pinned_to.as_ref())
}

// ── The heartbeat-lease decision (slice 2) ────────────────────────────────
//
// `current_candidates` (above) says *who should contend* for a task kind;
// `decide` (below) turns that set + the observed advisory lease into a single
// per-cycle action for *this* participant. The nest is a dumb per-actor
// last-writer-wins blackboard (participants.md § Coordination primitive,
// :58/:115) — all convergence logic is here, on the client. Wire + rationale
// tracked internally.

/// How often the current holder renews its lease. Hard-coded per the product
/// invariant (no human configures a heartbeat cadence — participants.md).
pub const HEARTBEAT_PERIOD_MS: u64 = 30_000;

/// A lease with no heartbeat for this long is **stale** ⇒ the next eligible
/// participant takes over (participants.md § Coordination primitive). Three
/// missed heartbeats — long enough to tolerate a transient reconnect, short
/// enough that a dead runner hands off within ~90 s.
pub const LEASE_STALE_MS: u64 = 90_000;

/// A standing-by participant re-observes at least this often, as the
/// correctness backstop to the best-effort `fauna.delegation.lease_changed`
/// push (which only nudges an earlier re-observe).
pub const OBSERVE_POLL_MS: u64 = 60_000;

/// What this participant should do with a task kind's lease this cycle — the
/// pure output of [`decide`]. The lease loop maps it to a heartbeat call plus
/// starting/stopping the task runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAction {
    /// I already hold this lease — heartbeat to renew and keep the task running.
    Renew,
    /// Claim (or take over) this lease — heartbeat as the new holder and run.
    Acquire,
    /// Do not run this kind — stand by and keep observing.
    Yield,
}

/// The advisory lease record as a participant currently observes it (from a
/// `fauna.delegation.observe` reply, or a re-observe triggered by a
/// `lease_changed` push). The mirror of the wire `LeaseState`.
///
/// `age_ms` is **nest-computed** (`server_now - last_write`, one clock), so it
/// is comparable to [`LEASE_STALE_MS`] without trusting any client clock —
/// the freshness question is answered on the nest's monotonic clock, the
/// *decision* stays here on the client (participants.md:58).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedLease {
    /// Who the nest last recorded as holding the lease.
    pub holder: ParticipantRef,
    /// The holder's self-reported class at that heartbeat.
    pub holder_class: ParticipantClass,
    /// Milliseconds since the nest last recorded a heartbeat for this lease.
    pub age_ms: u64,
}

/// The pure lease decision (participants.md § Coordination primitive). Given
/// this participant's ref, the current candidate set for the kind
/// ([`current_candidates`] — a single policy tier), and the observed lease (if
/// the nest holds one), return what this participant should do this cycle.
///
/// Convergence to a single runner needs **no** cross-device agreement on
/// ordering (which matters because `current_candidates` preserves *input*
/// order, not a shared canonical order):
///
/// - **Ineligible self** (not in `cands` — battery-mobile, a desktop that just
///   unplugged, or a kind pinned elsewhere) always [`Yield`](LeaseAction::Yield)s.
///   Checked first, so a holder that *loses* eligibility stops running (its
///   lease then goes stale and an eligible peer takes over).
/// - **Fresh lease held by me** → [`Renew`](LeaseAction::Renew).
/// - **Fresh lease held by a co-tier peer** (present in `cands`) → `Yield`
///   — *sticky*: exactly one of a set of equals holds it, the rest stand by,
///   no churn. This is what removes the need for a shared ranking.
/// - **Fresh lease held by someone not in `cands`** (an ineligible or
///   lower-tier holder — the latter arises automatically because
///   `current_candidates` returns only the single winning tier, so a tier-2
///   desktop holder is absent from a tier-1 nest's candidate set) →
///   [`Acquire`](LeaseAction::Acquire): the tier-1-nest-over-tier-2-desktop
///   preemption, with no explicit tier comparison here.
/// - **Free (absent) or stale lease** → `Acquire`. A brief cold/takeover
///   overlap of >1 runner is accepted — the lease is advisory over idempotent,
///   resumable tasks (participants.md:57); the sticky rule collapses the
///   overlap to one holder within ~one observe cycle.
pub fn decide(
    self_ref: &ParticipantRef,
    cands: &[ParticipantRef],
    observed: Option<&ObservedLease>,
    stale_ms: u64,
) -> LeaseAction {
    // Eligibility gate first: an ineligible participant never runs, even if it
    // is the recorded holder (e.g. a desktop that unplugged mid-run).
    if !cands.contains(self_ref) {
        return LeaseAction::Yield;
    }
    match observed {
        // A fresh holder exists.
        Some(lease) if lease.age_ms < stale_ms => {
            if &lease.holder == self_ref {
                LeaseAction::Renew
            } else if cands.contains(&lease.holder) {
                LeaseAction::Yield // co-tier peer holds it → sticky, don't churn
            } else {
                LeaseAction::Acquire // ineligible / lower-tier holder → preempt
            }
        }
        // Free (no record) or stale ⇒ claim it; races converge via sticky.
        _ => LeaseAction::Acquire,
    }
}

// ── The Task-delegation surface view-model (slice 4) ──────────────────────
//
// `current_candidates`/`decide` above are the *runtime* half (who should run a
// kind). The view-model below is the *display* half: it composes the user's
// pins ([`DelegationConfig`]) with the observed advisory lease into the
// per-kind rows the Task-delegation Settings sub-page renders on all 7 apps
// (participants.md § Task delegation; ui.yaml page `task-delegation`). Pure —
// the client calls it after a `fauna.delegation.observe` + a `fauna.state.delegation`
// read; the async orchestration + the pin write live in
// `fauna-client-delegation`, and the FFI/wasm surface mirrors these types
// (they embed the serde-only `ParticipantRef`, so — like `ParticipantRef`
// itself — they carry no UniFFI derive; the native FFI layer defines `Ffi*`
// mirrors, the wasm layer serializes via serde).

/// One entry of [`LIVE_TASK_KINDS`]: a heavy task kind, the i18n key for its
/// display name, and which participant side can run it today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskKindSpec {
    /// Stable kind string (`"backup-upload"`) — the lease / pin / wire key.
    pub kind: &'static str,
    /// The kind's display name as an i18n key (no args).
    pub name_key: &'static str,
    /// Whether **any** client can ever run this kind — the kind half of the
    /// picker rule; [`HeavyTaskCapability::runs`] is the per-app half, and
    /// a self-pin needs both. `false` for a kind no client ships a runner loop
    /// for, where a self-pin would strand the kind forever (see [`PinOption`] —
    /// the option list is a correctness surface). Two kinds today:
    /// `content-rescore`, nest-run permanently this major, and — since
    /// **2026-08-16** — `backup-upload`, whose last in-app driver was deleted
    /// with the slice-5 flip (see its entry below).
    ///
    /// **`index` flipped to `true` on 2026-08-03**, when the client builder
    /// started running end-to-end (linux + tui resume a builder at login and
    /// publish; `content-index.md` § Where the index is built). It was never
    /// nest-run: the content index is built at client capability positions,
    /// never the nest, and no index grant is minted this major (a nest cannot
    /// even derive an index key — `key-material-hierarchy.md` rule #7), so its
    /// `false` was purely transitional, exactly as this comment promised. Do
    /// not "fix" anything here by minting a nest grant.
    ///
    /// **This flag is deliberately NOT per-app** — which client ships which
    /// runner is [`HeavyTaskCapability`]'s job. Flipping `index` was safe with
    /// only two of the seven apps building precisely because that split exists:
    /// windows/macos drive the lease loop for `backup-upload` and correctly
    /// withhold the `index` self-pin until they wire the builder.
    pub client_runnable: bool,
}

/// The stable kind string of the segment-backup upload loop — the key every
/// app names when declaring what it runs ([`HeavyTaskCapability::runner_for`]).
pub const KIND_BACKUP_UPLOAD: &str = "backup-upload";
/// The stable kind string of the nest-run re-score drain.
pub const KIND_CONTENT_RESCORE: &str = "content-rescore";
/// The stable kind string of the client-side content-index builder.
pub const KIND_INDEX: &str = "index";

/// The heavy task kinds the Task-delegation surface lists (participants.md
/// § Concepts). The single canonical list, so every app shows the same
/// kinds in the same order (priority #1/#3). `backup-upload` runs on clients
/// (slice 3) and, since the slice-7 flip, on the nest too; `content-rescore` is
/// nest-run under a user-minted capability grant (slice 6) — the nest
/// heartbeats its lease while it holds a sufficient grant. **`index` is
/// client-run and never nest-run:** it is built at *client* capability
/// positions and no index grant is minted this major (`content-index.md`
/// § Where the index is built), which is why it became client-runnable when
/// the client builder shipped rather than waiting on any nest work.
pub const LIVE_TASK_KINDS: &[TaskKindSpec] = &[
    TaskKindSpec {
        kind: KIND_BACKUP_UPLOAD,
        name_key: "task_delegation.kind_backup_upload",
        // Flipped to `false` 2026-08-16 — the slice-5 flip, third and last piece.
        // The source nest has been the segment-backup writer since 2026-07-24, and
        // as of today NO app ships an in-app upload driver: linux 2026-07-29,
        // android + apple 2026-08-15, windows 2026-08-16. This flag could only be
        // flipped once the LAST of them landed, because an existing self-pin to a
        // still-driverless device would otherwise have been stranded
        // (`backup-restore.md` § Background Tasks → *Flip status (slice 5)*).
        // `FfiHeavyTaskCapability`'s `Runner` arm was retired in the same commit,
        // so no client can declare the kind either — the per-app half and the kind
        // half now agree. Re-adding a client driver means flipping BOTH back.
        client_runnable: false,
    },
    TaskKindSpec {
        kind: KIND_CONTENT_RESCORE,
        name_key: "task_delegation.kind_content_rescore",
        client_runnable: false,
    },
    TaskKindSpec {
        kind: KIND_INDEX,
        name_key: "task_delegation.kind_index",
        client_runnable: true,
    },
];

/// The canonical `'static` spelling of `task_kind` when it is one of
/// [`LIVE_TASK_KINDS`], else `None`. The nest's lease blackboard keys on this,
/// so it can only ever hold a slot per listed kind — never a kind a caller
/// made up (unbounded growth).
pub fn live_task_kind(task_kind: &str) -> Option<&'static str> {
    LIVE_TASK_KINDS
        .iter()
        .map(|s| s.kind)
        .find(|k| *k == task_kind)
}

/// Whether any client can ever run `task_kind` (see
/// [`TaskKindSpec::client_runnable`]). Unknown kinds are conservatively not
/// client-runnable — a self-pin to a kind this build doesn't know would
/// strand it just the same.
pub fn kind_client_runnable(task_kind: &str) -> bool {
    LIVE_TASK_KINDS
        .iter()
        .find(|s| s.kind == task_kind)
        .is_some_and(|s| s.client_runnable)
}

/// Who currently runs a task kind, for display on the Task-delegation surface.
/// Derived from the observed advisory lease: a fresh holder ⇒ running (this
/// device or another), no fresh holder ⇒ waiting. The client renders each arm
/// through its i18n pipeline; `Other` additionally resolves the participant to
/// a display name from the roster (which is why the name is not baked in here —
/// device names are inherently client-side state).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RunnerStatus {
    /// This device holds a fresh lease (client renders
    /// `task_delegation.runner_this_device`).
    ThisDevice,
    /// Another participant holds a fresh lease; the client resolves `who` to a
    /// name (`task_delegation.runner_other_device` with `{device}`).
    Other { who: ParticipantRef },
    /// No fresh lease holder — the kind waits for an eligible participant
    /// (`task_delegation.runner_waiting`).
    Waiting,
}

/// Which heavy task kinds **this build of this app** ships a runner for — the
/// client half of participants.md § The assignment picker's rule ("a client
/// offers as a pin target only a participant that can run the kind").
///
/// This is a *static per-app capability*, not the live [`ParticipantClass`]:
/// an unplugged laptop is momentarily [`ParticipantClass::BatteryMobile`] yet
/// remains a perfectly good pin target (the kind simply waits until it is
/// plugged in again). What this type distinguishes is the client that can
/// **never** run a given kind, so pinning to it would strand the work forever —
/// see [`PinOption`].
///
/// **It is per-(client, kind), not one Runner/ViewerOnly bit, because the two
/// sets genuinely differ per app** (corrected 2026-08-03). A single bit was a
/// faithful encoding only while every lease-driving client shipped every
/// client-runnable kind's runner; it stopped being one the moment the sets
/// diverged, and then it made the picker lie in both directions:
///
/// - linux drives no `backup-upload` loop since its upload driver was deleted
///   (2026-07-29) yet reported the old `Runner`, so the picker offered a
///   self-pin that could only ever wait — the exact stranding the rule forbids;
/// - linux and tui *do* run the content-index builder, so once `index` became
///   client-runnable they had to be offerable for it while windows/macos —
///   which drive the lease loop but ship no index builder yet — must not be.
///
/// Kinds are the stable [`TaskKindSpec::kind`] strings; an entry naming a kind
/// outside [`LIVE_TASK_KINDS`] is inert (it can only ever *withhold* an option,
/// which is the safe direction).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeavyTaskCapability {
    runs: Vec<String>,
}

impl HeavyTaskCapability {
    /// This client renders the surface but never runs *any* heavy task kind —
    /// the web SPA (no lease driver at all) and the mobiles (always
    /// [`ParticipantClass::BatteryMobile`], which [`current_candidates`] never
    /// returns). The former `ViewerOnly`.
    pub fn viewer_only() -> Self {
        Self { runs: Vec::new() }
    }

    /// This client ships a runner for exactly `kinds` and is a legal self-pin
    /// target for those and no others.
    pub fn runner_for<I, S>(kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            runs: kinds.into_iter().map(Into::into).collect(),
        }
    }

    /// Whether this client ships a runner for `task_kind` — the only question
    /// the picker ever asks of it. There is deliberately no "is this a viewer"
    /// accessor: every decision on this surface is per-kind, and a blanket
    /// question is exactly what the old encoding got wrong.
    pub fn runs(&self, task_kind: &str) -> bool {
        self.runs.iter().any(|k| k == task_kind)
    }
}

/// One selectable option in a task kind's assignment picker (ui.yaml
/// `task-delegation-assignment-picker`). Self-relative, so no client has to
/// re-derive "is this pin me or someone else?" to label it (priority #2).
///
/// **The option list is a correctness surface, not a cosmetic one.** A pin
/// collapses [`current_candidates`] to exactly the pinned participant, which
/// runs iff currently eligible — and a participant that is *never* eligible
/// (a battery mobile, a browser tab) yields the empty set, so the kind waits
/// **forever**. Offering such a target would let the user silently strand their
/// own backups. [`delegation_rows`] therefore emits the legal option set once,
/// here, for all seven apps (participants.md § Task delegation → Policy order;
/// this module's header).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PinOption {
    /// No pin — the automatic policy order picks the runner. Always offered,
    /// always first: the zero-configuration default and the escape from any
    /// pin. Label: `task_delegation.assignment_automatic`.
    Automatic,
    /// Pin to this device. Offered only when this client ships a runner for
    /// *this* kind ([`HeavyTaskCapability::runs`]). Label:
    /// `task_delegation.assignment_this_device`.
    ThisDevice,
    /// Pin to some other participant. Never offered as a *fresh* target (this
    /// client cannot know whether a participant it has only seen holding a
    /// lease is still eligible); present only when the kind is **already**
    /// pinned there, so the picker renders the user's actual choice and lets
    /// them switch away. The client resolves `who` to a display name.
    Other { who: ParticipantRef },
}

/// Resolve a participant ref to a display name from the device roster. A
/// device is keyed by its hex id directly (`ParticipantRef::Device.device_id`
/// matches `DeviceSummary.device_id`); an unknown device or a nest ref (can't
/// occur for `backup-upload` today, but must not panic) falls back to a
/// short-hex abbreviation. The roster (`device_id` → display name) is
/// inherently client-side state, so it is always an input, never derived here.
fn participant_name(who: &ParticipantRef, labels: &HashMap<String, String>) -> String {
    match who {
        ParticipantRef::Device { device_id } => labels
            .get(device_id)
            .filter(|l| !l.is_empty())
            .cloned()
            .unwrap_or_else(|| crate::format::short_id(device_id)),
        ParticipantRef::Nest { actor_pubkey } => {
            crate::format::short_id(&hex::encode(actor_pubkey))
        }
        // A participant kind a newer build names: no roster entry can match it.
        ParticipantRef::Unknown(_) => "?".to_string(),
    }
}

/// Label a runner status for display (ui.yaml `task-delegation-kind-item`'s
/// current-runner line). `Other` resolves the participant to a roster display
/// name via [`participant_name`] (`task_delegation.runner_other_device` with
/// `{device}`). One shared decision for all seven apps (priority #2) — each
/// resolves the returned key through its own i18n pipeline.
pub fn runner_label(runner: &RunnerStatus, labels: &HashMap<String, String>) -> LocalizedText {
    match runner {
        RunnerStatus::ThisDevice => LocalizedText::key("task_delegation.runner_this_device"),
        RunnerStatus::Waiting => LocalizedText::key("task_delegation.runner_waiting"),
        RunnerStatus::Other { who } => LocalizedText::key_arg(
            "task_delegation.runner_other_device",
            "device",
            participant_name(who, labels),
        ),
    }
}

/// Label one assignment-picker option (ui.yaml
/// `task-delegation-assignment-picker`). `Automatic` / `ThisDevice` are the
/// bare shared i18n keys; `Other` resolves the participant display name via
/// [`participant_name`] — carried as a `{name}` substitution
/// (`task_delegation.assignment_other_name`) rather than baked into the key
/// text, so `LocalizedText` stays the one return shape for both label
/// functions (no per-app raw-vs-localized branch). One shared decision for
/// all seven apps (priority #2).
pub fn option_label(option: &PinOption, labels: &HashMap<String, String>) -> LocalizedText {
    match option {
        PinOption::Automatic => LocalizedText::key("task_delegation.assignment_automatic"),
        PinOption::ThisDevice => LocalizedText::key("task_delegation.assignment_this_device"),
        PinOption::Other { who } => LocalizedText::key_arg(
            "task_delegation.assignment_other_name",
            "name",
            participant_name(who, labels),
        ),
    }
}

/// One row of the Task-delegation surface (ui.yaml `task-delegation-kind-item`):
/// a heavy task kind, its localizable display name, the current runner, and the
/// user's assignment together with the options the picker may offer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskDelegationRow {
    /// Stable kind string (`"backup-upload"`) — the key the pin write + the
    /// assignment picker value use.
    pub task_kind: String,
    /// The kind's display name as an i18n key (no args) for the client to
    /// resolve through its localization pipeline.
    pub name: LocalizedText,
    /// Who currently runs it (this device / another participant / waiting).
    pub runner: RunnerStatus,
    /// The user's current assignment. Always an element of [`Self::pin_options`],
    /// so "which option is selected" is always representable.
    pub assignment: PinOption,
    /// Every option the picker may offer, in display order (`Automatic` first).
    /// Never contains a target that can never run the kind — see [`PinOption`].
    pub pin_options: Vec<PinOption>,
}

/// The caller asked to pin a task kind to a participant that can never run it,
/// which would make the kind wait forever (see [`PinOption`]). Returned by
/// [`resolve_pin`]; unreachable through any shipped client UI, because
/// [`delegation_rows`] never offers such a target — this is the type-level
/// backstop for a client that renders its own picker anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotPinnable;

impl core::fmt::Display for NotPinnable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("this client can never run heavy task kinds, so it cannot be pinned to one")
    }
}

impl core::error::Error for NotPinnable {}

/// Turn a picker selection into the [`crate::data::TaskAssignment::pinned_to`]
/// value to persist: `Automatic` ⇒ `None` (clear the pin), `ThisDevice` ⇒ this
/// participant, `Other` ⇒ that participant. Pure — the write-side twin of the
/// `assignment` field [`delegation_rows`] produces.
///
/// Rejects a self-pin with [`NotPinnable`] when this client can never run the
/// kind — either because `task_kind` is not client-runnable at all
/// ([`TaskKindSpec::client_runnable`] — the nest-run kinds), or because *this*
/// client ships no runner for it ([`HeavyTaskCapability::runs`]) — closing the
/// write path that [`delegation_rows`] already closes on the read path.
pub fn resolve_pin(
    task_kind: &str,
    self_ref: &ParticipantRef,
    self_capability: &HeavyTaskCapability,
    option: &PinOption,
) -> Result<Option<ParticipantRef>, NotPinnable> {
    match option {
        PinOption::Automatic => Ok(None),
        PinOption::ThisDevice if !kind_client_runnable(task_kind) => Err(NotPinnable),
        PinOption::ThisDevice if !self_capability.runs(task_kind) => Err(NotPinnable),
        PinOption::ThisDevice => Ok(Some(self_ref.clone())),
        PinOption::Other { who } => Ok(Some(who.clone())),
    }
}

/// Build the Task-delegation surface rows (slice 4): one [`TaskDelegationRow`]
/// per [`LIVE_TASK_KINDS`] entry, composing the user's pin (`config`) with the
/// per-kind observed lease (`observed`, from a `fauna.delegation.observe`
/// reply) into a display row. Pure.
///
/// - `self_ref` — this device's [`ParticipantRef`] (marks the this-device
///   runner, and the this-device pin).
/// - `self_capability` — which kinds this client ships a runner for, and so
///   which of them it may be pinned for ([`HeavyTaskCapability`]).
/// - `observed` — `(task_kind, lease)` pairs; a kind with no entry, or one at
///   or past `stale_ms`, reads as [`RunnerStatus::Waiting`].
/// - `stale_ms` — [`LEASE_STALE_MS`] (a lease older than this has no live
///   runner).
///
/// Order follows [`LIVE_TASK_KINDS`] (stable across clients — priority #1/#3).
///
/// A kind whose lease holder or pin is a participant this build cannot name
/// ([`ParticipantRef::Unknown`], written by a newer build) gets **no row**: the
/// surface cannot say who that is, and offering the row would let a picker
/// render a choice it cannot represent. The pin itself stays stored untouched
/// and the kind waits for it ([`current_candidates`]) — the row is withheld,
/// never rewritten (`transport.md` § Schema and forward-compat discipline →
/// *Rule 3 in full*).
pub fn delegation_rows(
    self_ref: &ParticipantRef,
    self_capability: &HeavyTaskCapability,
    config: &DelegationConfig,
    observed: &[(String, ObservedLease)],
    stale_ms: u64,
) -> Vec<TaskDelegationRow> {
    LIVE_TASK_KINDS
        .iter()
        .filter_map(|spec| {
            let kind = &spec.kind;
            let runner = observed
                .iter()
                .find(|(k, _)| k == kind)
                .map(|(_, lease)| lease)
                .filter(|lease| lease.age_ms < stale_ms)
                .map_or(RunnerStatus::Waiting, |lease| {
                    if &lease.holder == self_ref {
                        RunnerStatus::ThisDevice
                    } else {
                        RunnerStatus::Other {
                            who: lease.holder.clone(),
                        }
                    }
                });
            let assignment = config
                .assignments
                .iter()
                .find(|a| a.task_kind == *kind)
                .and_then(|a| a.pinned_to.clone())
                .map_or(PinOption::Automatic, |to| {
                    if &to == self_ref {
                        PinOption::ThisDevice
                    } else {
                        PinOption::Other { who: to }
                    }
                });

            // Automatic always; this device only if it can ever run *this*
            // kind — both the kind's client-runnability (a nest-run kind is
            // never a legal self-pin target on any client) AND this build
            // actually shipping a runner for it must hold; and finally the
            // current assignment if it is neither of those — a foreign pin (another
            // device's pin on this kind) must stay visible and
            // de-selectable rather than be silently rendered as Automatic.
            let mut pin_options = vec![PinOption::Automatic];
            if spec.client_runnable && self_capability.runs(spec.kind) {
                pin_options.push(PinOption::ThisDevice);
            }
            if !pin_options.contains(&assignment) {
                pin_options.push(assignment.clone());
            }

            let names_unknown = |who: &ParticipantRef| matches!(who, ParticipantRef::Unknown(_));
            if matches!(&runner, RunnerStatus::Other { who } if names_unknown(who))
                || matches!(&assignment, PinOption::Other { who } if names_unknown(who))
            {
                return None;
            }

            Some(TaskDelegationRow {
                task_kind: spec.kind.to_string(),
                name: LocalizedText::key(spec.name_key),
                runner,
                assignment,
                pin_options,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::TaskAssignment;

    const KIND: &str = "backup-upload";

    fn nest(pubkey: u8, holds_grant: bool) -> ParticipantDescriptor {
        ParticipantDescriptor {
            reference: ParticipantRef::Nest {
                actor_pubkey: [pubkey; 32],
            },
            class: ParticipantClass::AlwaysOnNest,
            holds_grant,
        }
    }

    fn desktop(id: &str) -> ParticipantDescriptor {
        ParticipantDescriptor {
            reference: ParticipantRef::Device {
                device_id: id.to_string(),
            },
            class: ParticipantClass::PluggedInDesktop,
            holds_grant: false,
        }
    }

    fn mobile(id: &str) -> ParticipantDescriptor {
        ParticipantDescriptor {
            reference: ParticipantRef::Device {
                device_id: id.to_string(),
            },
            class: ParticipantClass::BatteryMobile,
            holds_grant: false,
        }
    }

    fn pin(kind: &str, to: ParticipantRef) -> DelegationConfig {
        DelegationConfig {
            assignments: vec![TaskAssignment {
                task_kind: kind.to_string(),
                pinned_to: Some(to),
            }],
        }
    }

    #[test]
    fn empty_config_yields_pure_policy() {
        // Zero configuration ⇒ the automatic policy order, works out of the box.
        assert_eq!(DelegationConfig::default().assignments, vec![]);
    }

    #[test]
    fn no_participants_waits() {
        let got = current_candidates(KIND, &[], &DelegationConfig::default());
        assert!(got.is_empty(), "no participants ⇒ the task waits");
    }

    #[test]
    fn nest_with_grant_is_tier_one_over_desktop() {
        let participants = [desktop("dev-a"), nest(1, true)];
        let got = current_candidates(KIND, &participants, &DelegationConfig::default());
        assert_eq!(
            got,
            vec![ParticipantRef::Nest {
                actor_pubkey: [1; 32]
            }]
        );
    }

    #[test]
    fn nest_without_grant_is_not_a_candidate_desktop_takes_over() {
        // A nest that holds no sufficient grant (e.g. backup-upload, which needs
        // the client's BackupKey) is never a candidate; the plugged desktop runs.
        let participants = [nest(1, false), desktop("dev-a")];
        let got = current_candidates(KIND, &participants, &DelegationConfig::default());
        assert_eq!(
            got,
            vec![ParticipantRef::Device {
                device_id: "dev-a".into()
            }]
        );
    }

    #[test]
    fn desktop_is_tier_two_when_no_eligible_nest() {
        let participants = [desktop("dev-a"), desktop("dev-b")];
        let got = current_candidates(KIND, &participants, &DelegationConfig::default());
        // Both plugged desktops contend; the lease breaks the tie by liveness.
        assert_eq!(
            got,
            vec![
                ParticipantRef::Device {
                    device_id: "dev-a".into()
                },
                ParticipantRef::Device {
                    device_id: "dev-b".into()
                },
            ]
        );
    }

    #[test]
    fn battery_mobile_never_runs_even_as_last_resort() {
        // Only a mobile is present ⇒ waits (never falls back to mobile).
        let participants = [mobile("phone-1"), mobile("tablet-2")];
        let got = current_candidates(KIND, &participants, &DelegationConfig::default());
        assert!(got.is_empty(), "battery-mobile never runs heavy kinds");
    }

    #[test]
    fn battery_mobile_never_runs_even_when_pinned() {
        // The strongest reading of "never, not even as a last resort": an
        // explicit pin to a battery-mobile participant still does not run.
        let phone = ParticipantRef::Device {
            device_id: "phone-1".into(),
        };
        let participants = [mobile("phone-1"), desktop("dev-a")];
        let got = current_candidates(KIND, &participants, &pin(KIND, phone));
        assert!(
            got.is_empty(),
            "a pin to a battery-mobile participant waits"
        );
    }

    #[test]
    fn pin_honored_when_eligible() {
        // Pin to a specific plugged desktop even though a nest-with-grant exists:
        // the pin overrides the automatic tier ordering.
        let dev_b = ParticipantRef::Device {
            device_id: "dev-b".into(),
        };
        let participants = [nest(1, true), desktop("dev-a"), desktop("dev-b")];
        let got = current_candidates(KIND, &participants, &pin(KIND, dev_b.clone()));
        assert_eq!(got, vec![dev_b]);
    }

    #[test]
    fn pin_to_absent_participant_waits() {
        // Pinned to a device that is not in the known set ⇒ waits for it; never
        // silently runs elsewhere.
        let ghost = ParticipantRef::Device {
            device_id: "unplugged-laptop".into(),
        };
        let participants = [nest(1, true), desktop("dev-a")];
        let got = current_candidates(KIND, &participants, &pin(KIND, ghost));
        assert!(
            got.is_empty(),
            "pin to an absent participant waits, doesn't reroute"
        );
    }

    #[test]
    fn pin_to_ineligible_nest_waits() {
        // Pinned to a nest that holds no grant for the kind ⇒ waits (it can't run
        // it), never reroutes to the eligible desktop.
        let nest_ref = ParticipantRef::Nest {
            actor_pubkey: [1; 32],
        };
        let participants = [nest(1, false), desktop("dev-a")];
        let got = current_candidates(KIND, &participants, &pin(KIND, nest_ref));
        assert!(got.is_empty(), "pin to a grant-less nest waits");
    }

    #[test]
    fn assignment_for_other_kind_does_not_affect_this_kind() {
        // A pin on a different task kind leaves this kind on the automatic order.
        let other = pin(
            "content-rescore",
            ParticipantRef::Device {
                device_id: "dev-z".into(),
            },
        );
        let participants = [nest(1, true), desktop("dev-a")];
        let got = current_candidates(KIND, &participants, &other);
        assert_eq!(
            got,
            vec![ParticipantRef::Nest {
                actor_pubkey: [1; 32]
            }]
        );
    }

    #[test]
    fn automatic_assignment_with_none_pin_is_pure_policy() {
        // An assignment row that exists but has pinned_to = None ⇒ automatic.
        let config = DelegationConfig {
            assignments: vec![TaskAssignment {
                task_kind: KIND.to_string(),
                pinned_to: None,
            }],
        };
        let participants = [nest(1, true), desktop("dev-a")];
        let got = current_candidates(KIND, &participants, &config);
        assert_eq!(
            got,
            vec![ParticipantRef::Nest {
                actor_pubkey: [1; 32]
            }]
        );
    }

    // ── decide() — the heartbeat-lease decision (slice 2) ─────────────────

    fn dref(id: &str) -> ParticipantRef {
        ParticipantRef::Device {
            device_id: id.to_string(),
        }
    }
    fn nref(pk: u8) -> ParticipantRef {
        ParticipantRef::Nest {
            actor_pubkey: [pk; 32],
        }
    }
    fn observed(holder: ParticipantRef, class: ParticipantClass, age_ms: u64) -> ObservedLease {
        ObservedLease {
            holder,
            holder_class: class,
            age_ms,
        }
    }
    const STALE: u64 = LEASE_STALE_MS;
    const FRESH: u64 = 1_000; // < STALE
    const OLD: u64 = LEASE_STALE_MS + 1; // ≥ STALE

    #[test]
    fn ineligible_self_always_yields() {
        // A participant not in the candidate set (battery-mobile, unplugged,
        // pinned-elsewhere) never runs — regardless of the observed lease.
        let me = dref("phone-1");
        let cands = vec![dref("dev-a")]; // I'm not in it
        assert_eq!(decide(&me, &cands, None, STALE), LeaseAction::Yield);
        let l = observed(dref("dev-a"), ParticipantClass::PluggedInDesktop, FRESH);
        assert_eq!(decide(&me, &cands, Some(&l), STALE), LeaseAction::Yield);
    }

    #[test]
    fn free_lease_eligible_acquires() {
        // No record yet ⇒ any eligible participant claims it.
        let me = dref("dev-a");
        let cands = vec![dref("dev-a"), dref("dev-b")];
        assert_eq!(decide(&me, &cands, None, STALE), LeaseAction::Acquire);
    }

    #[test]
    fn stale_lease_eligible_takes_over() {
        // The recorded holder went silent (age ≥ stale) ⇒ takeover.
        let me = dref("dev-b");
        let cands = vec![dref("dev-a"), dref("dev-b")];
        let l = observed(dref("dev-a"), ParticipantClass::PluggedInDesktop, OLD);
        assert_eq!(decide(&me, &cands, Some(&l), STALE), LeaseAction::Acquire);
    }

    #[test]
    fn holding_a_fresh_lease_renews() {
        let me = dref("dev-a");
        let cands = vec![dref("dev-a"), dref("dev-b")];
        let l = observed(dref("dev-a"), ParticipantClass::PluggedInDesktop, FRESH);
        assert_eq!(decide(&me, &cands, Some(&l), STALE), LeaseAction::Renew);
    }

    #[test]
    fn fresh_cotier_peer_holder_is_sticky_yield() {
        // A co-tier peer holds a fresh lease ⇒ stand by, don't churn. This is
        // what makes exactly-one-runner converge among equals with no shared
        // ranking.
        let me = dref("dev-a");
        let cands = vec![dref("dev-a"), dref("dev-b")];
        let l = observed(dref("dev-b"), ParticipantClass::PluggedInDesktop, FRESH);
        assert_eq!(decide(&me, &cands, Some(&l), STALE), LeaseAction::Yield);
    }

    #[test]
    fn nest_preempts_a_lower_tier_desktop_holder() {
        // A nest-with-grant is the sole tier-1 candidate; a desktop that was
        // holding is not in the nest's candidate set ⇒ the nest preempts (the
        // tier handover, for free — no explicit tier comparison in decide).
        let nest_me = nref(1);
        let participants = [nest(1, true), desktop("dev-a")];
        let cands = current_candidates(KIND, &participants, &DelegationConfig::default());
        assert_eq!(cands, vec![nref(1)]); // single winning tier
        let l = observed(dref("dev-a"), ParticipantClass::PluggedInDesktop, FRESH);
        assert_eq!(
            decide(&nest_me, &cands, Some(&l), STALE),
            LeaseAction::Acquire
        );
    }

    #[test]
    fn preempted_desktop_yields_once_the_nest_is_a_candidate() {
        // The other side of the handover: the desktop, now seeing the nest as
        // the sole candidate, is no longer eligible ⇒ it yields (stops running),
        // whether the fresh holder is still itself or already the nest.
        let desk_me = dref("dev-a");
        let participants = [nest(1, true), desktop("dev-a")];
        let cands = current_candidates(KIND, &participants, &DelegationConfig::default());
        // still shows itself holding
        let self_held = observed(dref("dev-a"), ParticipantClass::PluggedInDesktop, FRESH);
        assert_eq!(
            decide(&desk_me, &cands, Some(&self_held), STALE),
            LeaseAction::Yield
        );
        // or the nest already took over
        let nest_held = observed(nref(1), ParticipantClass::AlwaysOnNest, FRESH);
        assert_eq!(
            decide(&desk_me, &cands, Some(&nest_held), STALE),
            LeaseAction::Yield
        );
    }

    #[test]
    fn unplugged_holder_stops_renewing() {
        // A desktop holding the lease unplugs mid-run → it becomes battery-
        // mobile → its own candidate computation drops it → it yields even
        // though it is the fresh recorded holder (the eligibility-first gate).
        let me = dref("dev-a");
        // After unplugging, dev-a is not a candidate (only the still-plugged
        // dev-b is). It still sees itself as the recorded holder.
        let cands = vec![dref("dev-b")];
        let l = observed(dref("dev-a"), ParticipantClass::BatteryMobile, FRESH);
        assert_eq!(decide(&me, &cands, Some(&l), STALE), LeaseAction::Yield);
    }

    #[test]
    fn fresh_holder_no_longer_eligible_is_preempted() {
        // The recorded holder is fresh but not in my candidate set (e.g. it
        // lost its grant, or a higher tier appeared) ⇒ I preempt.
        let me = dref("dev-a");
        let cands = vec![dref("dev-a")];
        let gone = observed(
            dref("stale-participant"),
            ParticipantClass::PluggedInDesktop,
            FRESH,
        );
        assert_eq!(
            decide(&me, &cands, Some(&gone), STALE),
            LeaseAction::Acquire
        );
    }

    #[test]
    fn two_equal_desktops_converge_to_one_holder() {
        // Sanity: simulate the sticky convergence. Both eligible; on a fresh
        // lease held by one of them, that one renews and the other yields —
        // a single stable holder, no oscillation, no ranking.
        let a = dref("dev-a");
        let b = dref("dev-b");
        let cands = vec![a.clone(), b.clone()];
        let held_by_b = observed(b.clone(), ParticipantClass::PluggedInDesktop, FRESH);
        assert_eq!(
            decide(&a, &cands, Some(&held_by_b), STALE),
            LeaseAction::Yield
        );
        assert_eq!(
            decide(&b, &cands, Some(&held_by_b), STALE),
            LeaseAction::Renew
        );
    }

    // ── delegation_rows() — the Task-delegation surface view-model (slice 4) ──

    /// A client that ships a runner for `KIND` (`backup-upload`) — what the
    /// native desktops were before the capability became per-kind. Tests that
    /// care about the per-kind split build their own set instead.
    fn runner() -> HeavyTaskCapability {
        HeavyTaskCapability::runner_for([KIND])
    }

    /// A client that ships a runner for `index` — since the slice-5 flip
    /// (2026-08-16) the ONLY kind any app still runs, `backup-upload` having
    /// become nest-run. The picker-*mechanics* tests below use this: they are
    /// about the shape of the option set, not about which kind it is, and
    /// `backup-upload` can no longer offer `ThisDevice` for any client at all.
    fn index_runner() -> HeavyTaskCapability {
        HeavyTaskCapability::runner_for([KIND_INDEX])
    }

    /// A client that ships no runner at all — web and the mobiles.
    fn viewer_only() -> HeavyTaskCapability {
        HeavyTaskCapability::viewer_only()
    }

    fn kind_lease(kind: &str, holder: ParticipantRef, age_ms: u64) -> (String, ObservedLease) {
        (
            kind.to_string(),
            observed(holder, ParticipantClass::PluggedInDesktop, age_ms),
        )
    }

    #[test]
    fn rows_list_all_live_kinds_in_order() {
        let rows = delegation_rows(
            &dref("me"),
            &runner(),
            &DelegationConfig::default(),
            &[],
            STALE,
        );
        assert_eq!(rows.len(), LIVE_TASK_KINDS.len());
        assert_eq!(rows[0].task_kind, "backup-upload");
        assert_eq!(rows[0].name.key, "task_delegation.kind_backup_upload");
        assert_eq!(rows[1].task_kind, "content-rescore");
        assert_eq!(rows[1].name.key, "task_delegation.kind_content_rescore");
        assert_eq!(rows[2].task_kind, "index");
        assert_eq!(rows[2].name.key, "task_delegation.kind_index");
    }

    #[test]
    fn no_lease_no_pin_is_waiting_automatic() {
        // Zero configuration + no observed lease ⇒ waiting, automatic (works
        // out of the box).
        let rows = delegation_rows(
            &dref("me"),
            &runner(),
            &DelegationConfig::default(),
            &[],
            STALE,
        );
        assert_eq!(rows[0].runner, RunnerStatus::Waiting);
        assert_eq!(rows[0].assignment, PinOption::Automatic);
    }

    #[test]
    fn fresh_self_holder_is_this_device() {
        let me = dref("me");
        let obs = [kind_lease(KIND, me.clone(), FRESH)];
        let rows = delegation_rows(&me, &runner(), &DelegationConfig::default(), &obs, STALE);
        assert_eq!(rows[0].runner, RunnerStatus::ThisDevice);
    }

    #[test]
    fn fresh_other_holder_is_other() {
        let obs = [kind_lease(KIND, dref("dev-b"), FRESH)];
        let rows = delegation_rows(
            &dref("me"),
            &runner(),
            &DelegationConfig::default(),
            &obs,
            STALE,
        );
        assert_eq!(rows[0].runner, RunnerStatus::Other { who: dref("dev-b") });
    }

    #[test]
    fn stale_lease_is_waiting_even_with_holder() {
        // A recorded-but-stale holder has gone silent ⇒ no live runner shown.
        let obs = [kind_lease(KIND, dref("dev-b"), OLD)];
        let rows = delegation_rows(
            &dref("me"),
            &runner(),
            &DelegationConfig::default(),
            &obs,
            STALE,
        );
        assert_eq!(rows[0].runner, RunnerStatus::Waiting);
    }

    #[test]
    fn lease_for_other_kind_leaves_this_kind_waiting() {
        // An observed lease for a different kind must not bleed into this row.
        let obs = [kind_lease("index", dref("dev-b"), FRESH)];
        let rows = delegation_rows(
            &dref("me"),
            &runner(),
            &DelegationConfig::default(),
            &obs,
            STALE,
        );
        assert_eq!(rows[0].runner, RunnerStatus::Waiting);
    }

    #[test]
    fn pin_shows_pinned_assignment() {
        let rows = delegation_rows(
            &dref("me"),
            &runner(),
            &pin(KIND, dref("dev-b")),
            &[],
            STALE,
        );
        assert_eq!(rows[0].assignment, PinOption::Other { who: dref("dev-b") });
    }

    #[test]
    fn none_pin_row_is_automatic() {
        // An assignment row with pinned_to = None reads as automatic.
        let config = DelegationConfig {
            assignments: vec![TaskAssignment {
                task_kind: KIND.to_string(),
                pinned_to: None,
            }],
        };
        let rows = delegation_rows(&dref("me"), &runner(), &config, &[], STALE);
        assert_eq!(rows[0].assignment, PinOption::Automatic);
    }

    #[test]
    fn runner_and_assignment_are_independent() {
        // Pinned to dev-b, but dev-a currently holds the fresh lease (a takeover
        // hasn't landed): runner reflects the live holder, assignment the pin.
        let obs = [kind_lease(KIND, dref("dev-a"), FRESH)];
        let rows = delegation_rows(
            &dref("me"),
            &runner(),
            &pin(KIND, dref("dev-b")),
            &obs,
            STALE,
        );
        assert_eq!(rows[0].runner, RunnerStatus::Other { who: dref("dev-a") });
        assert_eq!(rows[0].assignment, PinOption::Other { who: dref("dev-b") });
    }

    // ── the assignment picker's legal option set (slice 4) ────────────────
    //
    // A pin to a participant that can never run the kind collapses
    // `current_candidates` to the empty set — the kind then waits *forever*
    // (`current_candidates`, the pinned arm). So the picker must never offer a
    // new pin target that cannot run. This is the shared enforcement of
    // participants.md:51 / this module's header ("the UI additionally prevents
    // pinning to mobile — slice 4"); no client re-implements it.

    #[test]
    fn a_runner_client_offers_automatic_and_this_device() {
        // On `index` — the kind a client can still run. This used to assert the
        // same shape on `backup-upload`, which went nest-run at the slice-5 flip.
        let rows = delegation_rows(
            &dref("me"),
            &index_runner(),
            &DelegationConfig::default(),
            &[],
            STALE,
        );
        let index = rows
            .iter()
            .find(|r| r.task_kind == KIND_INDEX)
            .expect("index row");
        assert_eq!(
            index.pin_options,
            vec![PinOption::Automatic, PinOption::ThisDevice]
        );
    }

    #[test]
    fn a_nest_run_kind_never_offers_this_device_even_on_a_client_claiming_to_run_it() {
        // content-rescore runs on the nest under a grant; no client ships a
        // runner loop for it, so a self-pin would strand the kind forever (the
        // slice-4 invariant, kind-aware since slice 6).
        //
        // The client here declares it runs *every* live kind, so the withheld
        // option can only come from `client_runnable` — otherwise this test
        // would pass on the client half alone and assert nothing about the
        // kind half.
        let claims_everything =
            HeavyTaskCapability::runner_for(LIVE_TASK_KINDS.iter().map(|s| s.kind));
        let rows = delegation_rows(
            &dref("me"),
            &claims_everything,
            &DelegationConfig::default(),
            &[],
            STALE,
        );
        let rescore = rows
            .iter()
            .find(|r| r.task_kind == KIND_CONTENT_RESCORE)
            .expect("content-rescore row");
        assert_eq!(
            rescore.pin_options,
            vec![PinOption::Automatic],
            "a nest-run kind must not offer a self-pin even to a client that claims it"
        );
    }

    #[test]
    fn a_client_that_runs_the_index_builder_may_pin_index_to_itself() {
        // The 2026-08-03 flip: `index` is client-runnable now that linux + tui
        // resume a builder at login. This is the read-path half of the flip.
        let cap = HeavyTaskCapability::runner_for([KIND_INDEX]);
        let rows = delegation_rows(&dref("me"), &cap, &DelegationConfig::default(), &[], STALE);
        let index = rows
            .iter()
            .find(|r| r.task_kind == KIND_INDEX)
            .expect("index row");
        assert_eq!(
            index.pin_options,
            vec![PinOption::Automatic, PinOption::ThisDevice],
            "a client shipping the index builder must be offerable for `index`"
        );
    }

    #[test]
    fn the_retired_backup_upload_kind_is_withheld_even_from_a_client_declaring_it() {
        // Was `a_lease_driving_client_without_the_index_builder_is_not_offered_index`,
        // whose premise — a desktop driving the `backup-upload` lease loop while
        // shipping no index builder — became UNREPRESENTABLE at the slice-5 flip
        // (2026-08-16): no app ships an upload driver any more, and the FFI arm
        // that declared the kind is retired with it.
        //
        // What survives is the strictly stronger claim, and it is the one the flip
        // itself depends on: the KIND half withholds `ThisDevice` for
        // `backup-upload` however loudly a client declares it. Without this, a
        // client that (wrongly, or on a future build) declared the kind would be
        // offered a self-pin that collapses `current_candidates` to empty and
        // strands the kind forever — the user silently stops being backed up.
        let claims_everything = HeavyTaskCapability::runner_for([KIND_BACKUP_UPLOAD, KIND_INDEX]);
        let rows = delegation_rows(
            &dref("me"),
            &claims_everything,
            &DelegationConfig::default(),
            &[],
            STALE,
        );
        let backup = rows
            .iter()
            .find(|r| r.task_kind == KIND_BACKUP_UPLOAD)
            .expect("backup-upload row");
        assert_eq!(
            backup.pin_options,
            vec![PinOption::Automatic],
            "`backup-upload` is nest-run since the slice-5 flip — the picker must \
             withhold `ThisDevice` no matter what the client declares"
        );
        // ...and the per-kind split still holds the other way: the kind this client
        // really does run is still offered, so this is not a blanket withdrawal.
        let index = rows
            .iter()
            .find(|r| r.task_kind == KIND_INDEX)
            .expect("index row");
        assert!(index.pin_options.contains(&PinOption::ThisDevice));
    }

    #[test]
    fn a_client_whose_upload_driver_was_deleted_is_no_longer_offered_backup_upload() {
        // linux: its in-app upload driver went 2026-07-29 (participants.md
        // § The assignment picker) and it drives no lease loop, yet it kept
        // reporting the old blanket `Runner` — so the picker offered a
        // `backup-upload` self-pin that could only ever wait. That defect was
        // live on main until the capability became per-kind.
        let cap = HeavyTaskCapability::runner_for([KIND_INDEX]);
        let rows = delegation_rows(&dref("me"), &cap, &DelegationConfig::default(), &[], STALE);
        let backup = rows
            .iter()
            .find(|r| r.task_kind == KIND_BACKUP_UPLOAD)
            .expect("backup-upload row");
        assert_eq!(
            backup.pin_options,
            vec![PinOption::Automatic],
            "a client that ships no upload driver must not be offered a \
             `backup-upload` self-pin, however client-runnable the kind is"
        );
    }

    #[test]
    fn a_viewer_only_client_never_offers_this_device() {
        // web / iOS / Android: pinning here would strand the kind forever.
        let rows = delegation_rows(
            &dref("me"),
            &viewer_only(),
            &DelegationConfig::default(),
            &[],
            STALE,
        );
        assert_eq!(rows[0].pin_options, vec![PinOption::Automatic]);
        assert!(!rows[0].pin_options.contains(&PinOption::ThisDevice));
    }

    #[test]
    fn pin_to_self_reads_as_this_device() {
        let me = dref("me");
        let rows = delegation_rows(&me, &runner(), &pin(KIND, me.clone()), &[], STALE);
        assert_eq!(rows[0].assignment, PinOption::ThisDevice);
    }

    #[test]
    fn a_pin_to_another_participant_is_offered_so_the_picker_shows_the_truth() {
        // The user pinned dev-b from another client. This client must render
        // that selection (and let the user switch away), even though it would
        // never *offer* dev-b as a fresh target.
        // On `index`, for the same reason as
        // `a_runner_client_offers_automatic_and_this_device` above: this asserts
        // the option-set shape, and `backup-upload` no longer offers `ThisDevice`.
        let rows = delegation_rows(
            &dref("me"),
            &index_runner(),
            &pin(KIND_INDEX, dref("dev-b")),
            &[],
            STALE,
        );
        let index = rows
            .iter()
            .find(|r| r.task_kind == KIND_INDEX)
            .expect("index row");
        assert_eq!(
            index.pin_options,
            vec![
                PinOption::Automatic,
                PinOption::ThisDevice,
                PinOption::Other { who: dref("dev-b") },
            ]
        );
    }

    #[test]
    fn a_viewer_only_client_still_renders_a_foreign_pin() {
        let rows = delegation_rows(
            &dref("me"),
            &viewer_only(),
            &pin(KIND, dref("dev-b")),
            &[],
            STALE,
        );
        assert_eq!(
            rows[0].pin_options,
            vec![
                PinOption::Automatic,
                PinOption::Other { who: dref("dev-b") },
            ]
        );
    }

    #[test]
    fn a_viewer_only_client_renders_a_stale_self_pin_without_offering_it_fresh() {
        // A self-pin on a kind this device cannot run (e.g. pinned from another
        // device): show it as selected so the user can switch away, rather
        // than silently rendering "Automatic" over a config that says otherwise.
        let me = dref("me");
        let rows = delegation_rows(&me, &viewer_only(), &pin(KIND, me.clone()), &[], STALE);
        assert_eq!(rows[0].assignment, PinOption::ThisDevice);
        assert_eq!(
            rows[0].pin_options,
            vec![PinOption::Automatic, PinOption::ThisDevice]
        );
    }

    #[test]
    fn the_current_assignment_is_always_a_selectable_option() {
        // The picker invariant every renderer relies on: `assignment` is always
        // an element of `pin_options`, so "selected" is always representable.
        let me = dref("me");
        let configs = [
            DelegationConfig::default(),
            pin(KIND, me.clone()),
            pin(KIND, dref("dev-b")),
            pin(KIND, nref(7)),
        ];
        for cap in [&runner(), &viewer_only()] {
            for config in &configs {
                let rows = delegation_rows(&me, cap, config, &[], STALE);
                assert!(
                    rows[0].pin_options.contains(&rows[0].assignment),
                    "assignment {:?} missing from options {:?} (cap {cap:?})",
                    rows[0].assignment,
                    rows[0].pin_options,
                );
            }
        }
    }

    // ── resolve_pin() — the write-side twin of `assignment` ───────────────

    #[test]
    fn resolve_pin_automatic_clears_the_pin() {
        assert_eq!(
            resolve_pin(KIND, &dref("me"), &runner(), &PinOption::Automatic),
            Ok(None)
        );
    }

    #[test]
    fn resolve_pin_this_device_pins_to_self() {
        // On `index`: a `ThisDevice` pin for `backup-upload` is now refused
        // outright (`NotPinnable`), which is what
        // `resolve_pin_refuses_a_self_pin_for_a_nest_run_or_unknown_kind` covers.
        let me = dref("me");
        assert_eq!(
            resolve_pin(KIND_INDEX, &me, &index_runner(), &PinOption::ThisDevice),
            Ok(Some(me.clone()))
        );
    }

    #[test]
    fn resolve_pin_other_pins_to_that_participant() {
        assert_eq!(
            resolve_pin(
                KIND,
                &dref("me"),
                &runner(),
                &PinOption::Other { who: dref("dev-b") }
            ),
            Ok(Some(dref("dev-b")))
        );
    }

    #[test]
    fn resolve_pin_refuses_to_pin_a_viewer_only_client_to_itself() {
        // The write-path backstop: a client that can never run the kind must
        // never persist a self-pin — the kind would wait forever.
        assert_eq!(
            resolve_pin(KIND, &dref("me"), &viewer_only(), &PinOption::ThisDevice),
            Err(NotPinnable)
        );
    }

    #[test]
    fn resolve_pin_refuses_a_self_pin_for_a_nest_run_or_unknown_kind() {
        // The kind-aware half of the write backstop. The client claims to run
        // both kinds, so a refusal can only come from `client_runnable` (for
        // content-rescore) or from the kind being unknown to this build.
        let claims_everything =
            HeavyTaskCapability::runner_for([KIND_CONTENT_RESCORE, "some-unknown-kind"]);
        for kind in [KIND_CONTENT_RESCORE, "some-unknown-kind"] {
            assert_eq!(
                resolve_pin(
                    kind,
                    &dref("me"),
                    &claims_everything,
                    &PinOption::ThisDevice
                ),
                Err(NotPinnable),
                "self-pin for {kind} must be rejected"
            );
        }
    }

    #[test]
    fn resolve_pin_refuses_a_self_pin_for_a_kind_this_client_ships_no_runner_for() {
        // The write-path twin of the read-path split: `index` is client-runnable,
        // but a client with no index builder must still be refused — otherwise a
        // client rendering its own picker could strand the kind.
        let no_index = HeavyTaskCapability::runner_for([KIND_BACKUP_UPLOAD]);
        assert_eq!(
            resolve_pin(KIND_INDEX, &dref("me"), &no_index, &PinOption::ThisDevice),
            Err(NotPinnable),
            "a client with no index builder must not be able to self-pin `index`"
        );
        // The client that does ship it is accepted, so the refusal is per-kind
        // and not a blanket one.
        let with_index = HeavyTaskCapability::runner_for([KIND_INDEX]);
        let me = dref("me");
        assert_eq!(
            resolve_pin(KIND_INDEX, &me, &with_index, &PinOption::ThisDevice),
            Ok(Some(me.clone()))
        );
    }

    #[test]
    fn resolve_pin_lets_a_viewer_only_client_clear_a_pin() {
        // Escaping a pin must always work, from any client.
        assert_eq!(
            resolve_pin(KIND, &dref("me"), &viewer_only(), &PinOption::Automatic),
            Ok(None)
        );
    }

    #[test]
    fn every_offered_option_resolves_on_the_client_that_offered_it() {
        // The read/write contract: whatever `delegation_rows` puts in
        // `pin_options`, `resolve_pin` accepts on that same client — except a
        // self-pin shown only because it already exists (rendered, not offered).
        let me = dref("me");
        for cap in [&runner(), &viewer_only()] {
            for config in [DelegationConfig::default(), pin(KIND, dref("dev-b"))] {
                let rows = delegation_rows(&me, cap, &config, &[], STALE);
                for row in &rows {
                    for option in &row.pin_options {
                        assert!(
                            resolve_pin(&row.task_kind, &me, cap, option).is_ok(),
                            "offered option {option:?} for {} rejected on cap {cap:?}",
                            row.task_kind,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn automatic_is_always_offered_first() {
        // "Automatic" is the zero-configuration default and the always-available
        // escape from any pin (participants.md § Data shape).
        let me = dref("me");
        for cap in [&runner(), &viewer_only()] {
            for config in [DelegationConfig::default(), pin(KIND, dref("dev-b"))] {
                let rows = delegation_rows(&me, cap, &config, &[], STALE);
                assert_eq!(rows[0].pin_options[0], PinOption::Automatic);
            }
        }
    }

    // ── runner_label / option_label (the shared label decision, priority #2) ──

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn runner_label_this_device_is_the_bare_key() {
        assert_eq!(
            runner_label(&RunnerStatus::ThisDevice, &HashMap::new()),
            LocalizedText::key("task_delegation.runner_this_device")
        );
    }

    #[test]
    fn runner_label_waiting_is_the_bare_key() {
        assert_eq!(
            runner_label(&RunnerStatus::Waiting, &HashMap::new()),
            LocalizedText::key("task_delegation.runner_waiting")
        );
    }

    #[test]
    fn runner_label_other_resolves_the_roster_name() {
        let who = dref("dev-b");
        let got = runner_label(
            &RunnerStatus::Other { who },
            &labels(&[("dev-b", "Ada's laptop")]),
        );
        assert_eq!(
            got,
            LocalizedText::key_arg(
                "task_delegation.runner_other_device",
                "device",
                "Ada's laptop"
            )
        );
    }

    #[test]
    fn runner_label_other_falls_back_to_short_id_when_unlabeled() {
        let who = dref("dev-b");
        let got = runner_label(&RunnerStatus::Other { who }, &HashMap::new());
        assert_eq!(
            got,
            LocalizedText::key_arg(
                "task_delegation.runner_other_device",
                "device",
                crate::format::short_id("dev-b")
            )
        );
    }

    #[test]
    fn option_label_automatic_and_this_device_are_bare_keys() {
        assert_eq!(
            option_label(&PinOption::Automatic, &HashMap::new()),
            LocalizedText::key("task_delegation.assignment_automatic")
        );
        assert_eq!(
            option_label(&PinOption::ThisDevice, &HashMap::new()),
            LocalizedText::key("task_delegation.assignment_this_device")
        );
    }

    #[test]
    fn option_label_other_resolves_the_roster_name() {
        let who = dref("dev-b");
        let got = option_label(
            &PinOption::Other { who },
            &labels(&[("dev-b", "Ada's laptop")]),
        );
        assert_eq!(
            got,
            LocalizedText::key_arg(
                "task_delegation.assignment_other_name",
                "name",
                "Ada's laptop"
            )
        );
    }

    #[test]
    fn option_label_other_falls_back_to_short_id_when_unlabeled() {
        let who = dref("dev-b");
        let got = option_label(&PinOption::Other { who }, &HashMap::new());
        assert_eq!(
            got,
            LocalizedText::key_arg(
                "task_delegation.assignment_other_name",
                "name",
                crate::format::short_id("dev-b")
            )
        );
    }

    #[test]
    fn a_nest_participant_falls_back_to_short_id_of_its_hex_pubkey() {
        // ParticipantRef::Nest is never labeled by the device roster (it's keyed
        // by device_id only) — always the short-hex fallback of the hex-encoded
        // pubkey, matching the client implementations this lifts.
        let who = ParticipantRef::Nest {
            actor_pubkey: [0xab; 32],
        };
        let got = option_label(&PinOption::Other { who }, &HashMap::new());
        assert_eq!(
            got,
            LocalizedText::key_arg(
                "task_delegation.assignment_other_name",
                "name",
                crate::format::short_id(&hex::encode([0xab; 32]))
            )
        );
    }
}

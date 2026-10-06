//! The succession ceremony's app-side orchestration — extracted from tui
//! (`apps/fauna-tui/src/settings/mod.rs`) so the other
//! apps consume it instead of re-deriving the nine-item defect-avoidance
//! contract (`identity-succession.md` § Implementation status today).
//!
//! **Split by transport, not by app** (narrowed 2026-08-20, the web leg): the
//! three *driving* functions below are native-only — the reconcile arm dials
//! `fauna_anon_client::AnonymousNestClient` and the sweep builds a
//! `fauna_client::NestClient` + on-disk `fauna_mls` engines, none of which
//! exist on wasm32. Everything that is **not** a transport — [`SweepStatus`],
//! [`LandedSuccession`] and [`StolenOutcome`] — is wasm-clean and compiled
//! everywhere, because web drives the same ceremony over its own transport and
//! must reach the same outcome vocabulary and, above all, the same *wording*:
//! [`StolenOutcome::message`] decides which of the two ways back into the
//! account a user is told about, and a second copy of that sentence is a second
//! place for it to be wrong (priority #1/#2).
//! Web's driver is `libs/fauna-wasm/src/succession.rs`.
//!
//! The one thing an app still supplies is its per-account store-path resolver
//! (`db_path_for`): every app scopes its MLS store its own way, and the
//! resolver runs AFTER the successor's connect on purpose — resolution may
//! write (it may create the store), and an unreachable nest must
//! fail before anything is written.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use fauna_client::NestClient;

#[cfg(not(target_arch = "wasm32"))]
use crate::RecoveryClient;
use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// [`SweepView::kind`]'s three tokens — the human-facing vocabulary, hyphenated
/// (the e2e state protocol's `no_engine` is a different vocabulary on purpose;
/// see [`SweepStatus::state_json`]).
pub const SWEEP_KIND_NO_ENGINE: &str = "no-engine";
pub const SWEEP_KIND_FAILED: &str = "failed";
pub const SWEEP_KIND_RAN: &str = "ran";

// The sweep's own lines (`i18n/strings/en.yaml` → `settings.recovery_kit`).
// Keys, never English: the app resolves them through its own pipeline, exactly
// as `RecoveryKitStatus::status_line`'s.
const KEY_SWEEP_NONE: &str = "settings.recovery_kit.sweep_none";
const KEY_SWEEP_NONE_NO_RETRY: &str = "settings.recovery_kit.sweep_none_no_retry";
const KEY_SWEEP_FAILED: &str = "settings.recovery_kit.sweep_failed";
const KEY_SWEEP_ALL_REMOVED: &str = "settings.recovery_kit.sweep_all_removed";
const KEY_SWEEP_PARTIAL: &str = "settings.recovery_kit.sweep_partial";
const KEY_SWEEP_PARTIAL_NO_RETRY: &str = "settings.recovery_kit.sweep_partial_no_retry";
const KEY_SWEEP_UNATTESTED: &str = "settings.recovery_kit.sweep_unattested";

// The retry's four answers (`recovery-kit-sweep-retry-button`). Keys for the
// same reason as the lines above — and here the reason is sharper: the button
// "must answer in words on every press" (`settings.md` § Recovery kit →
// *Finishing an unfinished group sweep*), so these sentences are the whole
// gesture on three of its four arms.
const KEY_SWEEP_RETRY_NO_OLD_STATE: &str = "settings.recovery_kit.sweep_retry_no_old_state";
const KEY_SWEEP_RETRY_NOT_LANDED: &str = "settings.recovery_kit.sweep_retry_not_landed";
const KEY_SWEEP_RETRY_LANDED_FOR_ANOTHER: &str =
    "settings.recovery_kit.sweep_retry_landed_for_another";
const KEY_SWEEP_RETRY_FAILED: &str = "settings.recovery_kit.sweep_retry_failed";

/// [`SweepRetryAnswer::kind`]'s five tokens — the same hyphenated, human-facing
/// vocabulary as [`SWEEP_KIND_RAN`] and its siblings.
pub const RETRY_KIND_SWEPT: &str = "swept";
pub const RETRY_KIND_NO_OLD_STATE: &str = "no-old-state";
pub const RETRY_KIND_NOT_LANDED: &str = "not-landed";
pub const RETRY_KIND_LANDED_FOR_ANOTHER: &str = "landed-for-another";
pub const RETRY_KIND_FAILED: &str = "failed";

/// [`StolenOutcome::kind`]'s four tokens — the same hyphenated, human-facing
/// vocabulary as the retry's, for the boundaries that carry the outcome rather
/// than paint it (the FFI record, the wasm JSON, the e2e state protocol).
pub const STOLEN_KIND_LANDED: &str = "landed";
pub const STOLEN_KIND_NOT_LANDED: &str = "not-landed";
pub const STOLEN_KIND_LANDED_FOR_ANOTHER: &str = "landed-for-another";
pub const STOLEN_KIND_UNDECIDED: &str = "undecided";

// The ceremony's non-landed sentences (`settings.md` § Recovery kit → *The
// ceremony's outcome is headlined by its arm*). Each carries its own headline,
// so an app paints it verbatim and wraps nothing.
const KEY_STOLEN_CEREMONY_FAILED: &str = "settings.recovery_kit.stolen_ceremony_failed";
const KEY_STOLEN_OUTCOME_UNKNOWN_SAVED: &str = "settings.recovery_kit.stolen_outcome_unknown_saved";
const KEY_STOLEN_OUTCOME_UNKNOWN_UNSAVED: &str =
    "settings.recovery_kit.stolen_outcome_unknown_unsaved";
const KEY_STOLEN_LANDED_FOR_ANOTHER: &str = "settings.recovery_kit.stolen_landed_for_another";

/// What `identity-stolen-confirm-field` must equal before
/// `identity-stolen-button` goes live (`settings.md` § Recovery kit — "the same
/// type-to-confirm idiom as `settings-delete-account-button`").
///
/// Here rather than per-app because it is a **gate condition, not copy**: the
/// placeholder that tells the user what to type is an i18n string and is
/// translated, while the word the handler compares against is not — so an app
/// that localized the wrong one would silently disarm an irreversible ceremony's
/// only gate. Lifted 2026-09-01 from `apps/fauna-tui/src/settings/recovery.rs`,
/// which had been the only definition while windows hand-wrote its own copy and
/// linux was about to add a third.
pub const STOLEN_CONFIRM_WORD: &str = "SUCCEED";

/// What `sessions-lockout-confirm-field` must equal before
/// `sessions-lockout-button` locks the account for 24 hours
/// (`docs/goal/ui/sessions.md` § The ruling 3). Beside
/// [`STOLEN_CONFIRM_WORD`] for its reason: a gate condition, not copy — the
/// prompt naming it is translated, the word is never localized, and every app
/// re-checks it in the action arm, not only in the render. Shared by the
/// signed-in page and the signed-out door.
pub const LOCKOUT_CONFIRM_WORD: &str = "LOCK";

/// What the post-succession group sweep did — the ceremony's own outcome, which
/// the surface renders in the flow that ran it (`identity-succession.md`
/// § Propagation → *MLS groups*).
/// `Clone` because an app that parks this across its account switch reads it
/// out of a message it only borrows (linux's `DataMessage` fold). Cheap and
/// safe: every payload under it is already `Clone`, and none of them is a
/// secret — the sweep describes groups, never keys.
#[derive(Debug, Clone)]
pub enum SweepStatus {
    /// Conversations were never up, so there was no engine to sweep from. The
    /// groups (if any) still hold the old leaf.
    NoEngine,
    /// The sweep could not start — the successor's engine or keypair would not
    /// build. Distinct from `Ran`: nothing was attempted, so nothing partial.
    Failed(String),
    /// It ran. The report says per group what happened, and carries the roster
    /// the user must adjudicate — boxed because it is much the largest variant
    /// and this enum rides app fold outcomes.
    Ran(Box<crate::SweepReport>),
}

/// A [`SweepStatus`] as a surface sees it — the render view, shared so every
/// app says the same thing about the same outcome.
///
/// Deliberately **not** [`SweepStatus::state_json`]: that one is the e2e state
/// provider's machine vocabulary (`no_engine`, underscored) and carries the
/// whole per-group report; this one is what a human reads on the screen that
/// just ran the ceremony. The two already read the same arm by different names,
/// which is exactly why they are separate functions rather than one.
///
/// **What it is for, since 2026-08-27: an app holds
/// this, not the enum, by the time it paints.** The ceremony runs before the
/// account switch and the lines render after it, on a surface the switch has
/// rebuilt; a native app carries the view across (apple's `SuccessionHandoff`,
/// windows' twin) and web parks it in `sessionStorage`. So the copy is selected
/// **off the view** ([`Self::copy`]) — the counts it carries are exactly the
/// ones the selection reads — and [`SweepStatus::render_copy`] is the same
/// answer for a caller still holding the enum. Serde-derived for the parking,
/// never as a wire type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepView {
    /// [`SWEEP_KIND_NO_ENGINE`] / [`SWEEP_KIND_FAILED`] / [`SWEEP_KIND_RAN`] —
    /// the arm, as a stable token.
    pub kind: String,
    /// The one extra fact the arm carries, or `None` when it carries none: the
    /// failure reason on `Failed`, and on `Ran` the count of groups that still
    /// owe work (absent when none do).
    pub detail: Option<String>,
    /// On `ran`, how many groups the sweep enumerated; `0` on the other arms
    /// (nothing was enumerated) and on a succession over an account with no
    /// groups — which is the one `ran` the copy says nothing about.
    pub groups: u32,
    /// On `ran`, how many of those the succeeded credential is gone from.
    pub groups_old_leaf_removed: u32,
    /// The size of the roster the sweep can vouch for nothing about
    /// ([`SweepStatus::review_roster`]) — `0` on every arm that swept nobody.
    pub unattested_members: u32,
}

/// Whether the surface painting a [`SweepCopy`] also paints
/// `recovery-kit-sweep-retry-button` beside it.
///
/// The two degraded arms (`no-engine`, a partial `ran`) tell the user how to
/// finish the job, and `settings.md` § Recovery kit rules that a degraded line
/// must never name a control that is not on screen — so which remedy they name
/// is decided by this, and by nothing an app writes itself.
///
/// ⚠ **`Absent` is a parity gap, never a product choice.** tui built the button
/// 2026-08-16; every other app passes `Absent` only until it builds its own
/// (`retry_group_sweep` has no FFI or wasm face yet). When the seventh app lands
/// it, this enum and the two `*_no_retry` strings retire together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepRetryAffordance {
    /// The app renders the button on every arm [`SweepView::owes_work`] answers
    /// true for — and MUST, since the copy will name it.
    Rendered,
    /// The app has not built the button. The owing arms carry the member-side
    /// remedy alone.
    Absent,
}

/// What a press of `recovery-kit-sweep-retry-button` answered — the retry's
/// whole outcome vocabulary, decided here rather than per app.
///
/// **Every arm but [`Self::Swept`] is a sentence and nothing else.** The button
/// "must answer in words on every press" (`settings.md` § Recovery kit →
/// *Finishing an unfinished group sweep*), because its render gate is unfinished
/// work rather than "this device can retry" — so a device that cannot retry is
/// on screen and has to say so. None of the three terminal answers is reported
/// as a sweep: an empty [`crate::SweepReport`] would render as *"removed from
/// all 0 of your groups"*, the reassurance-by-vacuity the sweep's own copy
/// refuses everywhere else.
///
/// ⚠ **The refusals are not failures.** [`Self::NoOldState`] means this device
/// holds no conversation history for the retired identity, [`Self::NotLanded`]
/// that no move of the account was ever recorded, and
/// [`Self::LandedForAnother`] that the account moved to a different successor —
/// each names a next step of its own, and none of them posted anything.
#[derive(Debug, Clone)]
pub enum SweepRetryAnswer {
    /// The retry ran the sweep. The report replaces the view the surface
    /// carried, and both engines were persisted before this returned.
    Swept(Box<crate::SweepReport>),
    /// This device has no conversation history for the retired identity — no
    /// predecessor seed here, or no store behind it — so there is nothing to
    /// rebuild the sweep from.
    NoOldState,
    /// No succession for this identity has landed; there is nothing to finish.
    NotLanded,
    /// The account was re-pointed to a **different** successor than the one
    /// signed in here.
    LandedForAnother,
    /// The retry reached for the nest (or for local state) and could not
    /// finish. Safe to press again — nothing was lost, and nothing partial was
    /// left behind that a second press would double.
    Failed(String),
}

impl SweepRetryAnswer {
    /// The sentence this answer renders as, or `None` for [`Self::Swept`] —
    /// whose outcome renders through the sweep's own lines ([`SweepView::copy`])
    /// instead, exactly as the ceremony's own sweep does.
    pub fn message(&self) -> Option<LocalizedText> {
        match self {
            Self::Swept(_) => None,
            Self::NoOldState => Some(LocalizedText::key(KEY_SWEEP_RETRY_NO_OLD_STATE)),
            Self::NotLanded => Some(LocalizedText::key(KEY_SWEEP_RETRY_NOT_LANDED)),
            Self::LandedForAnother => Some(LocalizedText::key(KEY_SWEEP_RETRY_LANDED_FOR_ANOTHER)),
            Self::Failed(reason) => Some(LocalizedText::key_arg(
                KEY_SWEEP_RETRY_FAILED,
                "reason",
                reason.clone(),
            )),
        }
    }

    /// The arm as a stable token, for a boundary that carries the answer rather
    /// than paints it (the FFI record, the e2e state protocol).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Swept(_) => RETRY_KIND_SWEPT,
            Self::NoOldState => RETRY_KIND_NO_OLD_STATE,
            Self::NotLanded => RETRY_KIND_NOT_LANDED,
            Self::LandedForAnother => RETRY_KIND_LANDED_FOR_ANOTHER,
            Self::Failed(_) => RETRY_KIND_FAILED,
        }
    }

    /// The sweep report an **owed** sweep parks once its unbidden press
    /// answers — the relaunch adoption's discharge
    /// (`succession-propagation.md` § Propagation → *Own device fleet*, the
    /// relaunch-adoption clause): `Swept` parks as the ceremony's own report
    /// would have; every other answer parks an arm that still
    /// [owes work](SweepView::owes_work), so `recovery-kit-sweep-retry-button`
    /// renders and the answer's own sentence says why.
    ///
    /// ⚠ **Never an empty `Ran`.** Nothing swept, so nothing may be reported
    /// as done: *removed from all 0 groups* over a device whose groups still
    /// seat the retired leaf is the one alert-shaped succession condition
    /// painted as its opposite. A transport failure keeps its reason
    /// (`Failed`); the rest — no old state here, nothing landed, landed for
    /// another — park `NoEngine`, the arm whose fact is exactly "this sweep did
    /// not run; the groups may still hold the old leaf".
    pub fn into_owed_status(self) -> SweepStatus {
        match self {
            Self::Swept(report) => SweepStatus::Ran(report),
            Self::Failed(reason) => SweepStatus::Failed(reason),
            Self::NoOldState | Self::NotLanded | Self::LandedForAnother => SweepStatus::NoEngine,
        }
    }
}

/// The retired identity a retry sweeps **from**, resolved off the account
/// registry: the direct predecessor of `successor_actor_hex`, paired with the
/// seed for it when this device still holds one.
///
/// ⚠ **The direct hop, never merely the nearest one whose seed happens to be
/// here.** `.next()`'s safety does not come from `predecessors_of` walking
/// nearest-hop-first (it does, but that is incidental here) — it comes from the
/// walk being *seeded with `successor_actor_hex` itself*: the frontier's first
/// pass only ever pushes rows whose `succeeded_by` is that seed, so index 0 is
/// always a direct predecessor regardless of walk order. Only the immediate
/// predecessor owns the leaf the current groups still seat — the hop before it
/// was already evicted by the succession that produced this one — so a sweep
/// authored from an older seed names a pair the chain does not authorize, which
/// every member verifies and refuses.
///
/// **Unaddressed when a successor has more than one direct predecessor**
/// (nothing in `record_succession` guards against two `old` rows naming the
/// same `new`): `.next()` then picks whichever direct predecessor comes first
/// in account-index order, not whichever one this device happens to hold a
/// seed for. Still fail-closed —
/// the wrong pick still names a pair the chain refuses — just not
/// fail-available for a device holding the *other* one's seed.
///
/// `None` means no succession into this identity is recorded on this device at
/// all — nothing to finish, and a surface says so rather than offering the
/// retry. `Some((hex, None))` means it is recorded but the seed is not here,
/// which [`retry_sweep_as_successor`] answers with the member-side remedy.
///
/// Native-only because `fauna-client-accounts` is (this crate's `Cargo.toml`
/// scopes it to `cfg(not(target_arch = "wasm32"))`), and web needs none of it:
/// no browser can hold the retired identity's conversation store, so its answer
/// is settled before any registry walk would matter.
#[cfg(not(target_arch = "wasm32"))]
pub fn retry_predecessor(
    accounts: &fauna_client_accounts::AccountRegistry,
    successor_actor_hex: &str,
) -> Option<(String, Option<fauna_core::secret::SecretString>)> {
    let old_actor_hex = accounts
        .predecessors_of(successor_actor_hex)
        .into_iter()
        .next()?;
    let seed = accounts
        .secrets(&old_actor_hex)
        .map(|stored| stored.secret_hex);
    Some((old_actor_hex, seed))
}

/// The sweep's own lines — what a surface says about a [`SweepStatus`],
/// decided once here (`settings.md` § Recovery kit → *The sweep's own lines*).
///
/// **Two facts, never one verdict.** Eviction of the stolen identity and the
/// roster the sweep cannot vouch for are separate claims, so they are separate
/// lines, and there is deliberately no field a surface could round into "you
/// are safe" (`SweepReport::old_leaf_removed_everywhere`'s own warning).
/// `None` is a real value: a line the surface must not paint.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct SweepCopy {
    /// What the sweep did — one of the four outcome arms — or `None` when
    /// there is nothing to say: a succession on an account with **no groups at
    /// all**, where *"removed from all 0 of your groups"* would be reassurance
    /// by vacuity. The suppression is the projection's, not any app's.
    pub outcome: Option<LocalizedText>,
    /// The residual the app genuinely cannot classify, as its own line, or
    /// `None` when the roster is empty. Never phrased as an alarm — a healthy
    /// group has members the ceremony did not add.
    pub unattested: Option<LocalizedText>,
}

impl SweepView {
    /// Whether the sweep left work a retry could still finish — the render gate
    /// for `recovery-kit-sweep-retry-button`, and the one place that question
    /// is answered.
    ///
    /// Three arms owe work, and they are exactly the three the retry was built
    /// for: the sweep never ran (`no-engine` — conversations were down at the
    /// ceremony), it failed outright, or it swept some groups and not others
    /// (the common network-flake arm). A sweep that removed the old leaf from
    /// every group owes nothing, and a sweep over an account with **no groups
    /// at all** owes nothing either — "finish moving your groups" would offer
    /// to finish something that never started.
    ///
    /// ⚠ Deliberately says nothing about the **unattested roster**. That is the
    /// second, independent fact a sweep produces, it is adjudicated by the
    /// member-review pass rather than by re-running anything, and folding it in
    /// here would put a *Finish Moving Your Groups* button under a sweep that
    /// already finished moving every group.
    pub fn owes_work(&self) -> bool {
        match self.kind.as_str() {
            SWEEP_KIND_NO_ENGINE | SWEEP_KIND_FAILED => true,
            SWEEP_KIND_RAN => self.groups_old_leaf_removed < self.groups,
            _ => false,
        }
    }

    /// Select the sweep's lines — see [`SweepCopy`] for the two-facts rule and
    /// [`SweepRetryAffordance`] for which remedy the degraded arms name.
    ///
    /// ⚠ **Lifted here 2026-08-27 from `apps/fauna-tui/src/settings/recovery.rs`**,
    /// where the selection — including the `groups == 0` suppression — was one
    /// app's local judgment while web painted nothing, and apple and windows
    /// were about to re-derive it in Swift and C#. Same shape as the aftermath
    /// legs' `status_line()`s: the projection decides, an app only localizes.
    pub fn copy(&self, retry: SweepRetryAffordance) -> SweepCopy {
        match self.kind.as_str() {
            SWEEP_KIND_NO_ENGINE => SweepCopy {
                outcome: Some(LocalizedText::key(match retry {
                    SweepRetryAffordance::Rendered => KEY_SWEEP_NONE,
                    SweepRetryAffordance::Absent => KEY_SWEEP_NONE_NO_RETRY,
                })),
                unattested: None,
            },
            SWEEP_KIND_FAILED => SweepCopy {
                outcome: Some(LocalizedText::key_arg(
                    KEY_SWEEP_FAILED,
                    "reason",
                    self.detail.clone().unwrap_or_default(),
                )),
                unattested: None,
            },
            SWEEP_KIND_RAN => {
                // A succession on an account with no groups at all has nothing
                // to report — saying "removed from all 0 of your groups" would
                // be noise dressed as reassurance.
                if self.groups == 0 {
                    return SweepCopy::default();
                }
                let outcome = if self.groups_old_leaf_removed >= self.groups {
                    LocalizedText::key_arg(KEY_SWEEP_ALL_REMOVED, "groups", self.groups.to_string())
                } else {
                    LocalizedText::key_args(
                        match retry {
                            SweepRetryAffordance::Rendered => KEY_SWEEP_PARTIAL,
                            SweepRetryAffordance::Absent => KEY_SWEEP_PARTIAL_NO_RETRY,
                        },
                        [
                            ("removed", self.groups_old_leaf_removed.to_string()),
                            ("groups", self.groups.to_string()),
                        ],
                    )
                };
                // The residual, as its own line. Deliberately NOT folded into
                // the line above: it is not a qualifier on the eviction, it is
                // a separate fact the eviction says nothing about.
                let unattested = (self.unattested_members > 0).then(|| {
                    LocalizedText::key_arg(
                        KEY_SWEEP_UNATTESTED,
                        "count",
                        self.unattested_members.to_string(),
                    )
                });
                SweepCopy {
                    outcome: Some(outcome),
                    unattested,
                }
            }
            // A token this build does not know is a token this build did not
            // mint — unreachable, and the safe direction is silence over a
            // fabricated claim about the user's groups.
            _ => SweepCopy::default(),
        }
    }
}

/// `usize` → `u32` for the view's counts, saturating rather than truncating: a
/// group count past `u32::MAX` is not a real case, and a wrapped count would be
/// a wrong claim about the user's groups.
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl SweepStatus {
    /// [`SweepView::owes_work`], for a caller still holding the enum.
    pub fn owes_work(&self) -> bool {
        self.render_view().owes_work()
    }

    /// [`SweepView::copy`], for a caller still holding the enum.
    pub fn render_copy(&self, retry: SweepRetryAffordance) -> SweepCopy {
        self.render_view().copy(retry)
    }

    /// Project this into the view a surface paints from — and selects its copy off
    /// ([`SweepView::copy`]; `settings.md` § Recovery kit → *The sweep's own lines*).
    ///
    /// The detail on the `Ran` arm is deliberately a count of the groups that
    /// did NOT take rather than the whole report: the surface's job here is to
    /// say whether propagation finished, and a per-group dump on the one screen
    /// the user is reading a secret key off is noise at the worst possible
    /// moment.
    ///
    /// ⚠ **Lifted here 2026-08-21 from `libs/fauna-wasm/src/succession.rs`,
    /// where it was a private `sweep_view` fn.** It is a projection *of this
    /// enum*, so one copy per driver is one more answer to the same question —
    /// and the native drivers (apple/windows/android, over `fauna-ffi`) were
    /// about to add a third. Wasm-clean like its neighbours, so web keeps
    /// consuming it across the same boundary it always did.
    pub fn render_view(&self) -> SweepView {
        match self {
            Self::NoEngine => SweepView {
                kind: SWEEP_KIND_NO_ENGINE.to_string(),
                detail: None,
                groups: 0,
                groups_old_leaf_removed: 0,
                unattested_members: 0,
            },
            Self::Failed(e) => SweepView {
                kind: SWEEP_KIND_FAILED.to_string(),
                detail: Some(e.clone()),
                groups: 0,
                groups_old_leaf_removed: 0,
                unattested_members: 0,
            },
            Self::Ran(report) => {
                // The report's own accounting, never a re-derived filter: "which
                // groups still owe work" is this crate's judgment (it is what
                // decides the retry/escalate split), and a second definition in
                // a driver would be a second answer to the same question.
                let unfinished = report.groups_owing_ceremony().len();
                SweepView {
                    kind: SWEEP_KIND_RAN.to_string(),
                    detail: (unfinished > 0).then(|| unfinished.to_string()),
                    groups: count(report.outcomes.len()),
                    groups_old_leaf_removed: count(report.groups_old_leaf_removed()),
                    unattested_members: count(report.unattested_members().len()),
                }
            }
        }
    }

    /// The roster the aftermath's member-review raise is fed — the people this
    /// sweep can vouch for nothing about, or empty on the arms that swept
    /// nobody.
    ///
    /// **Empty is meaningful rather than absent**, on every arm: a sweep that
    /// found nobody, an engine that was never up, and a sweep that could not
    /// start all report "no one to review", and the raise no-ops on it. The two
    /// non-`Ran` arms are exactly the same shape of trap, so they are
    /// spelled out here once rather than left to each caller's `_ =>`.
    ///
    /// ⚠ **This is the report's own roster, never a re-derivation.** The report
    /// is the membership as it stood across the compromise window; asking the
    /// successor's engine later would flag people who joined afterwards and
    /// miss people who have since left (`succession-aftermath.md` § Propagation
    /// — *the sweep's roster is written down*).
    pub fn review_roster(&self) -> Vec<fauna_core::identity::ActorId> {
        match self {
            Self::Ran(report) => report.unattested_members(),
            Self::NoEngine | Self::Failed(_) => Vec::new(),
        }
    }

    /// This sweep as the e2e state protocol's `succession_sweep` object — the
    /// **cross-app contract** every app publishes under that key (convention
    /// 11: the command/state table is a contract, not a per-app courtesy).
    ///
    /// **Why it is here rather than in each app.** The ceremony renders its
    /// outcome as ID-less prose, so this object is the *only* way a journey can
    /// assert that a real succession re-pointed the user's groups — and the
    /// journeys assert its whole shape (`groups`, `old_leaf_removed_everywhere`,
    /// `outcomes`), not just `status`. Written twice it would drift twice; it
    /// already had, in the one spelling two apps happened to share
    /// (tui published `no_engine` while web's *render* view says `no-engine`,
    /// which is why the render view is deliberately NOT this).
    ///
    /// `Null` for a `None` sweep is the caller's job, and load-bearing: a
    /// journey must be able to tell "no succession ran on this app run" from
    /// "one ran and swept nothing" — see [`Self::state_json_or_null`].
    pub fn state_json(&self) -> serde_json::Value {
        match self {
            Self::NoEngine => serde_json::json!({ "status": "no_engine" }),
            Self::Failed(reason) => serde_json::json!({
                "status": "failed",
                "error": reason,
            }),
            Self::Ran(report) => serde_json::json!({
                "status": "ran",
                "groups": report.outcomes.len(),
                "groups_old_leaf_removed": report.groups_old_leaf_removed(),
                // `all()` over an empty report is vacuously true, which is
                // honest for "no group still holds the old leaf" but must never
                // be read as "the sweep did something" — pair it with `groups`.
                "old_leaf_removed_everywhere": report.old_leaf_removed_everywhere(),
                "unattested_members": report.unattested_members().len(),
                // Per group, WITH the failure reason. The counts above say a
                // sweep fell short; only this says why, and
                // `GroupSweepState::Failed` carries its reason verbatim
                // precisely so a transport fault stays distinguishable from a
                // claimed folder channel's roster refusal. Without it a red
                // journey reports "0 of 1 re-pointed" and sends the next
                // session guessing (convention 6: a failure diagnoses itself).
                "outcomes": report
                    .outcomes
                    .iter()
                    .map(|o| serde_json::json!({
                        "channel": fauna_core::hex32::encode(&o.channel_id.0),
                        "state": format!("{:?}", o.state),
                        "old_leaf_removed": o.state.old_leaf_removed(),
                    }))
                    .collect::<Vec<_>>(),
            }),
        }
    }

    /// [`Self::state_json`] over an `Option`, with the absent case spelled once.
    ///
    /// Absent is **`Null`, never an empty object**: a journey must be able to
    /// tell "no succession ran on this app run" from "one ran and swept
    /// nothing", and every app answering that the same way is the point of
    /// this living here.
    pub fn state_json_or_null(sweep: Option<&Self>) -> serde_json::Value {
        sweep.map_or(serde_json::Value::Null, Self::state_json)
    }
}

/// What a landed succession hands the fold.
///
/// A named payload rather than a fourth tuple slot because that member is the
/// easy one to mis-read: `succeeded_at` is the **nest's own commit stamp**, and
/// the two arms that build this differ in whether they have one at all.
///
/// `Clone` because an app whose fold only BORROWS its messages has to take a
/// copy to act on one (linux's `DataMessage`). Safe by construction rather than
/// by care: the seed is [`zeroize::Zeroizing`], so a clone zeroizes on drop
/// exactly as the original does — and [`Self::fmt`] redacts it on both.
#[derive(Clone)]
pub struct LandedSuccession {
    /// The successor identity's 64-hex secret. At the instant this arrives it
    /// exists nowhere else in the world and it *is* the account.
    pub successor_secret_hex: zeroize::Zeroizing<String>,
    /// The identity the account now belongs to.
    pub new_actor_id: fauna_core::identity::ActorId,
    /// What the pre-switch group sweep managed.
    pub sweep: SweepStatus,
    /// Unix seconds the nest applied the succession
    /// (`actor_successions.succeeded_at`), when the submit reply carried it.
    ///
    /// **`None` on the reconcile arm**, where that reply never arrived — this
    /// ceremony runs over an `AnonymousNestClient`, and the kind that serves the
    /// stamp (`fauna.recovery.succession.status`) is authenticated by design, so
    /// it is genuinely not knowable *here*. It is knowable later: the aftermath's
    /// email-filter raise runs on the successor's signed-in connection and asks
    /// for it when this is `None`, so the reconcile arm classifies too. Neither
    /// fallback is ever taken — not the statement's own clock (stamped at
    /// authoring, so a rule the thief adds before the commit escapes the mark)
    /// nor "every filter I own right now" (wrong on every re-run).
    /// See the app's post-switch aftermath state (tui: `App::succession_succeeded_at`).
    pub succeeded_at: Option<i64>,
}

impl LandedSuccession {
    /// Plain constructor — it exists so a consumer (an app fold, a test) can
    /// build one without depending on `zeroize` directly; the secret is wrapped
    /// here.
    pub fn new(
        successor_secret_hex: String,
        new_actor_id: fauna_core::identity::ActorId,
        sweep: SweepStatus,
        succeeded_at: Option<i64>,
    ) -> Self {
        Self {
            successor_secret_hex: zeroize::Zeroizing::new(successor_secret_hex),
            new_actor_id,
            sweep,
            succeeded_at,
        }
    }
}

impl std::fmt::Debug for LandedSuccession {
    /// **Hand-written and redacted**, for the reason
    /// [`crate::SuccessionHandoff`]'s is: this holds an identity
    /// seed, which *is* the account. `Outcome` derives `Debug` and outcomes are
    /// traced, so a derived impl here would put the successor's seed in a log
    /// line at the one moment it exists nowhere else.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LandedSuccession")
            .field("successor_secret_hex", &"<redacted>")
            .field("new_actor_id", &self.new_actor_id)
            .field("sweep", &self.sweep)
            .field("succeeded_at", &self.succeeded_at)
            .finish()
    }
}

/// How `identity-stolen-button`'s ceremony ended — typed end to end
/// (`identity-succession.md` § Implementation status today, the *typed
/// outcome* ruling), so no surface tells the arms apart by sniffing English.
///
/// **Only [`Self::NotLanded`] is a failure**, and only its sentence reads as
/// one. The other two non-landed arms are outcomes of a ceremony that ran: the
/// account moved to another device's successor ([`Self::LandedForAnother`]), or
/// this device cannot tell whether it moved ([`Self::Undecided`]) — headlining
/// either as "couldn't recover" is the false headline the ruling retired.
///
/// Every app paints [`Self::message`] verbatim on `error-message` and wraps
/// nothing (`settings.md` § Recovery kit → *The ceremony's outcome is headlined
/// by its arm*).
#[derive(Clone)]
pub enum StolenOutcome {
    /// The succession landed (confirmed, or reconciled after a lost reply).
    Landed(LandedSuccession),
    /// Nothing moved; trying again is safe. Every pre-submit refusal of
    /// [`crate::succeed_with_held_kit`] (wrong phrase, a kit for another
    /// account, the nest refusing to author) lands here too, so a surface has
    /// ONE match. `reported` is the ceremony's own error text.
    NotLanded { reported: String },
    /// Someone else's ceremony won: the account was re-pointed to a different
    /// successor. Final — never "try again".
    LandedForAnother {
        new_actor_id: fauna_core::identity::ActorId,
    },
    /// This device cannot decide whether the account moved. `cause` is the
    /// diagnostic clause, `persisted` whether a read-back proved the successor
    /// seed is saved here — which decides the way back the sentence names.
    Undecided {
        cause: String,
        persisted: bool,
        successor_secret_hex: zeroize::Zeroizing<String>,
        reported: String,
    },
}

impl StolenOutcome {
    /// The nothing-moved arm over any error the ceremony reported.
    pub fn not_landed(reported: impl std::fmt::Display) -> Self {
        Self::NotLanded {
            reported: reported.to_string(),
        }
    }

    /// The undecidable arm, shared by every driver that reaches it — the
    /// native one below and web's (`libs/fauna-wasm/src/succession.rs`) — so
    /// the cell of the succession matrix where wording is the whole safety
    /// property is built in one place. Which way back the sentence names is
    /// [`Self::message`]'s.
    pub fn undecided(
        cause: String,
        persisted: bool,
        successor_secret_hex: &str,
        reported: &crate::RecoveryError,
    ) -> Self {
        Self::Undecided {
            cause,
            persisted,
            successor_secret_hex: zeroize::Zeroizing::new(successor_secret_hex.to_string()),
            reported: reported.to_string(),
        }
    }

    /// The arm as a stable token ([`STOLEN_KIND_LANDED`] and its siblings).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Landed(_) => STOLEN_KIND_LANDED,
            Self::NotLanded { .. } => STOLEN_KIND_NOT_LANDED,
            Self::LandedForAnother { .. } => STOLEN_KIND_LANDED_FOR_ANOTHER,
            Self::Undecided { .. } => STOLEN_KIND_UNDECIDED,
        }
    }

    /// The sentence this outcome renders as, or `None` for [`Self::Landed`] —
    /// whose outcome is the switch (or the persist-failure message), not a line
    /// here.
    ///
    /// The undecided arm is split by the way back, and wording is the whole
    /// safety property there: **verified** persist → point at the store;
    /// **not verified** → put the seed on screen, the only way back into an
    /// account that may already be the successor's. Telling a user the wrong
    /// one costs them the account.
    pub fn message(&self) -> Option<LocalizedText> {
        match self {
            Self::Landed(_) => None,
            Self::NotLanded { reported } => Some(LocalizedText::key_arg(
                KEY_STOLEN_CEREMONY_FAILED,
                "message",
                reported.clone(),
            )),
            Self::LandedForAnother { new_actor_id } => Some(LocalizedText::key_arg(
                KEY_STOLEN_LANDED_FOR_ANOTHER,
                "actor",
                new_actor_id.to_hex(),
            )),
            Self::Undecided {
                cause,
                persisted: true,
                reported,
                ..
            } => Some(LocalizedText::key_args(
                KEY_STOLEN_OUTCOME_UNKNOWN_SAVED,
                [("cause", cause.clone()), ("reported", reported.clone())],
            )),
            Self::Undecided {
                cause,
                persisted: false,
                successor_secret_hex,
                reported,
            } => Some(LocalizedText::key_args(
                KEY_STOLEN_OUTCOME_UNKNOWN_UNSAVED,
                [
                    ("cause", cause.clone()),
                    ("secret", successor_secret_hex.to_string()),
                    ("reported", reported.clone()),
                ],
            )),
        }
    }

    /// Whether [`Self::message`] carries the only copy of the successor seed —
    /// the undecided arm whose persist was not verified. A surface parks such a
    /// line exactly as it parks the persist-failure message (`settings.md`
    /// § Recovery kit), never on an ordinary `error-message` write the next
    /// event on the page can clobber.
    pub fn carries_the_only_seed(&self) -> bool {
        matches!(
            self,
            Self::Undecided {
                persisted: false,
                ..
            }
        )
    }
}

impl std::fmt::Debug for StolenOutcome {
    /// **Hand-written and redacted**, for [`LandedSuccession`]'s reason: the
    /// undecided arm holds the successor seed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Landed(landed) => f.debug_tuple("Landed").field(landed).finish(),
            Self::NotLanded { reported } => f
                .debug_struct("NotLanded")
                .field("reported", reported)
                .finish(),
            Self::LandedForAnother { new_actor_id } => f
                .debug_struct("LandedForAnother")
                .field("new_actor_id", new_actor_id)
                .finish(),
            Self::Undecided {
                cause,
                persisted,
                reported,
                ..
            } => f
                .debug_struct("Undecided")
                .field("cause", cause)
                .field("persisted", persisted)
                .field("successor_secret_hex", &"<redacted>")
                .field("reported", reported)
                .finish(),
        }
    }
}

/// The ceremony's outcome vocabulary, pinned at the type that decides it.
#[cfg(test)]
mod stolen_outcome_tests {
    use super::*;

    fn undecided(persisted: bool) -> StolenOutcome {
        StolenOutcome::Undecided {
            cause: "the nest could not be reached to check (refused)".to_string(),
            persisted,
            successor_secret_hex: zeroize::Zeroizing::new("ab".repeat(32)),
            reported: "rpc disconnected".to_string(),
        }
    }

    /// Each non-landed arm has its own sentence, and the landed arm none — the
    /// shape that lets every app paint `message()` with one rule.
    #[test]
    fn every_non_landed_arm_has_its_own_sentence_and_landed_has_none() {
        let landed = StolenOutcome::Landed(LandedSuccession::new(
            "cd".repeat(32),
            fauna_core::identity::ActorKeypair::generate().actor_id(),
            SweepStatus::NoEngine,
            None,
        ));
        assert_eq!(landed.kind(), STOLEN_KIND_LANDED);
        assert!(
            landed.message().is_none(),
            "the landed arm renders the switch"
        );

        let arms = [
            (
                StolenOutcome::not_landed("nest refused"),
                STOLEN_KIND_NOT_LANDED,
            ),
            (
                StolenOutcome::LandedForAnother {
                    new_actor_id: fauna_core::identity::ActorKeypair::generate().actor_id(),
                },
                STOLEN_KIND_LANDED_FOR_ANOTHER,
            ),
            (undecided(true), STOLEN_KIND_UNDECIDED),
            (undecided(false), STOLEN_KIND_UNDECIDED),
        ];
        let mut keys = Vec::new();
        for (outcome, kind) in &arms {
            assert_eq!(outcome.kind(), *kind);
            keys.push(outcome.message().expect("a non-landed arm speaks").key);
        }
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(
            unique.len(),
            keys.len(),
            "no two arms share a sentence: {keys:?}"
        );
        assert_eq!(
            keys[0], KEY_STOLEN_CEREMONY_FAILED,
            "only nothing-moved reads as a failure"
        );
    }

    /// The undecided arm names exactly one way back: the store when the persist
    /// was verified, the seed on screen when it was not — never both.
    #[test]
    fn the_undecided_arm_names_exactly_one_way_back() {
        let saved = undecided(true).message().unwrap();
        assert_eq!(saved.key, KEY_STOLEN_OUTCOME_UNKNOWN_SAVED);
        assert!(
            !saved.args.contains_key("secret"),
            "a saved seed is never put on screen"
        );
        assert!(!undecided(true).carries_the_only_seed());

        let unsaved = undecided(false).message().unwrap();
        assert_eq!(unsaved.key, KEY_STOLEN_OUTCOME_UNKNOWN_UNSAVED);
        assert_eq!(
            unsaved.args.get("secret").map(String::as_str),
            Some("ab".repeat(32).as_str()),
            "an unverified persist puts the seed on screen"
        );
        assert!(undecided(false).carries_the_only_seed());
        for line in [&saved, &unsaved] {
            assert_eq!(
                line.args.get("reported").map(String::as_str),
                Some("rpc disconnected")
            );
            assert!(line.args.contains_key("cause"));
        }
    }

    /// The seed never reaches a log line through `Debug`.
    #[test]
    fn the_undecided_arm_redacts_its_seed() {
        let rendered = format!("{:?}", undecided(false));
        assert!(!rendered.contains(&"ab".repeat(32)), "{rendered}");
    }
}

/// The sweep's own lines, pinned at the projection that decides them
/// (`settings.md` § Recovery kit → *The sweep's own lines*). Pure, so every
/// platform runs these; the resolved English is pinned where the resolver
/// lives (tui's `the_degraded_sweep_arms_name_only_remedies_that_exist`).
#[cfg(test)]
mod copy_tests {
    use super::*;
    use crate::{GroupSweepOutcome, GroupSweepState, SweepReport};

    fn group(state: GroupSweepState, marker: u8, unattested: u8) -> GroupSweepOutcome {
        GroupSweepOutcome {
            channel_id: fauna_mls::types::ChannelId([marker; 32]),
            state,
            unattested_members: (1..=unattested)
                .map(|i| fauna_core::identity::ActorId([i; 32]))
                .collect(),
        }
    }

    fn ran(groups: Vec<GroupSweepOutcome>) -> SweepStatus {
        SweepStatus::Ran(Box::new(SweepReport { outcomes: groups }))
    }

    fn partial() -> SweepStatus {
        ran(vec![
            group(GroupSweepState::Swept, 1, 0),
            group(GroupSweepState::Failed("nest unreachable".into()), 2, 0),
            group(GroupSweepState::NeedsMemberReAdd, 3, 0),
        ])
    }

    /// A succession on an account with no groups at all has nothing to report:
    /// "removed from all 0 of your groups" is the reassurance-by-vacuity the
    /// copy refuses everywhere else — and it is a SHARED rule now, not one
    /// app's local judgment.
    #[test]
    fn a_sweep_over_no_groups_says_nothing() {
        let sweep = ran(Vec::new());
        for retry in [SweepRetryAffordance::Rendered, SweepRetryAffordance::Absent] {
            assert_eq!(sweep.render_copy(retry), SweepCopy::default());
        }
        assert!(
            !sweep.owes_work(),
            "nothing to move, so nothing to finish — the retry must not render"
        );
    }

    /// Eviction of the stolen identity and the roster it cannot vouch for are
    /// two facts, and there is no combined "you are safe" verdict to render.
    #[test]
    fn a_completed_sweep_reports_eviction_and_the_roster_as_two_facts() {
        let copy = ran(vec![group(GroupSweepState::Swept, 7, 1)])
            .render_copy(SweepRetryAffordance::Rendered);
        assert_eq!(
            copy.outcome,
            Some(LocalizedText::key_arg(KEY_SWEEP_ALL_REMOVED, "groups", "1"))
        );
        assert_eq!(
            copy.unattested,
            Some(LocalizedText::key_arg(KEY_SWEEP_UNATTESTED, "count", "1")),
            "the roster is its OWN line — never a qualifier folded into the eviction, and \
             never absent because the eviction succeeded"
        );
    }

    /// A clean sweep with nobody to review renders the eviction alone.
    #[test]
    fn a_clean_sweep_with_nobody_to_review_has_no_roster_line() {
        let copy = ran(vec![group(GroupSweepState::AlreadySwept, 7, 0)])
            .render_copy(SweepRetryAffordance::Absent);
        assert!(copy.outcome.is_some());
        assert_eq!(copy.unattested, None);
    }

    /// The owing arms are exactly the three the retry was built for — never
    /// ran, failed outright, swept some groups and not others — and the two
    /// silent ones are the two where a retry could only mislead.
    #[test]
    fn the_owing_arms_are_exactly_the_three_the_retry_was_built_for() {
        assert!(SweepStatus::NoEngine.owes_work());
        assert!(SweepStatus::Failed("engine would not build".into()).owes_work());
        assert!(partial().owes_work());
        assert!(!ran(vec![group(GroupSweepState::Swept, 1, 3)]).owes_work());
        assert!(!ran(Vec::new()).owes_work());
        // The same answer off the view alone, which is all an FFI app holds
        // by the time it paints.
        assert!(SweepStatus::NoEngine.render_view().owes_work());
        assert!(
            !ran(vec![group(GroupSweepState::Swept, 1, 0)])
                .render_view()
                .owes_work()
        );
    }

    /// The two degraded arms name `recovery-kit-sweep-retry-button` only where
    /// the app renders it; elsewhere they carry the member-side remedy alone —
    /// the same FACT under a different remedy, never a hidden arm
    /// (`settings.md` § Recovery kit: a degraded line must not name a control
    /// that is not on screen).
    #[test]
    fn the_degraded_arms_name_the_retry_button_only_where_it_is_rendered() {
        let none = SweepStatus::NoEngine;
        assert_eq!(
            none.render_copy(SweepRetryAffordance::Rendered).outcome,
            Some(LocalizedText::key(KEY_SWEEP_NONE))
        );
        assert_eq!(
            none.render_copy(SweepRetryAffordance::Absent).outcome,
            Some(LocalizedText::key(KEY_SWEEP_NONE_NO_RETRY))
        );

        let with = partial().render_copy(SweepRetryAffordance::Rendered);
        let without = partial().render_copy(SweepRetryAffordance::Absent);
        let counts = [("removed", "1"), ("groups", "3")];
        assert_eq!(
            with.outcome,
            Some(LocalizedText::key_args(KEY_SWEEP_PARTIAL, counts))
        );
        assert_eq!(
            without.outcome,
            Some(LocalizedText::key_args(KEY_SWEEP_PARTIAL_NO_RETRY, counts))
        );
    }

    /// The arms that never named the button read identically with or without
    /// it: the failure reason travels as an argument, and the roster line is
    /// the same fact on both.
    #[test]
    fn the_other_arms_do_not_depend_on_the_retry_affordance() {
        let failed = SweepStatus::Failed("nest unreachable".into());
        let expected = Some(LocalizedText::key_arg(
            KEY_SWEEP_FAILED,
            "reason",
            "nest unreachable",
        ));
        assert_eq!(
            failed.render_copy(SweepRetryAffordance::Rendered).outcome,
            expected
        );
        assert_eq!(
            failed.render_copy(SweepRetryAffordance::Absent).outcome,
            expected
        );
        assert_eq!(
            failed.render_copy(SweepRetryAffordance::Absent).unattested,
            None,
            "a sweep that could not start swept nobody and reviews nobody"
        );

        let roster = ran(vec![group(GroupSweepState::Swept, 1, 2)]);
        assert_eq!(
            roster
                .render_copy(SweepRetryAffordance::Rendered)
                .unattested,
            roster.render_copy(SweepRetryAffordance::Absent).unattested,
        );
    }

    /// The view carries the counts the copy is selected from, so an app
    /// holding only the view (across an account switch) selects the same
    /// lines the enum would.
    #[test]
    fn the_view_selects_the_same_copy_as_the_status() {
        for sweep in [
            SweepStatus::NoEngine,
            SweepStatus::Failed("x".into()),
            partial(),
            ran(vec![group(GroupSweepState::Swept, 1, 2)]),
            ran(Vec::new()),
        ] {
            for retry in [SweepRetryAffordance::Rendered, SweepRetryAffordance::Absent] {
                assert_eq!(sweep.render_copy(retry), sweep.render_view().copy(retry));
            }
        }
        let view = partial().render_view();
        assert_eq!((view.groups, view.groups_old_leaf_removed), (3, 1));
        assert_eq!(
            view.detail.as_deref(),
            Some("2"),
            "the owing count is unchanged"
        );
    }

    /// Every retry answer that is not a sweep carries a sentence, and the one
    /// that is carries none.
    ///
    /// `settings.md` § Recovery kit → *Finishing an unfinished group sweep*:
    /// the button "must answer in words on every press", because its render
    /// gate is unfinished work rather than "this device can retry" — so a
    /// silent arm is a dropped command (testing.md point 11), and a `Swept`
    /// arm that also carried a sentence would say two things about one press.
    #[test]
    fn every_non_sweeping_retry_answer_speaks_and_the_sweeping_one_does_not() {
        let answers = [
            SweepRetryAnswer::NoOldState,
            SweepRetryAnswer::NotLanded,
            SweepRetryAnswer::LandedForAnother,
            SweepRetryAnswer::Failed("rpc disconnected".to_string()),
        ];
        let mut keys = Vec::new();
        for answer in &answers {
            let line = answer
                .message()
                .unwrap_or_else(|| panic!("{} answers with nothing", answer.kind()));
            keys.push(line.key.clone());
        }
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(
            unique.len(),
            keys.len(),
            "each arm names what is left to do, so no two share a sentence: {keys:?}"
        );
        assert!(
            SweepRetryAnswer::Swept(Box::default()).message().is_none(),
            "a sweep that RAN renders through the sweep's own lines, not a second sentence"
        );
    }

    /// An owed sweep's unbidden press parks the fresh report when it swept, and
    /// otherwise an arm that still owes work — so the retry button renders
    /// rather than a finished-looking report over groups nothing touched.
    #[test]
    fn an_owed_sweep_parks_the_report_or_an_arm_that_still_owes_work() {
        let swept = SweepRetryAnswer::Swept(Box::default()).into_owed_status();
        assert!(
            matches!(swept, SweepStatus::Ran(_)),
            "a press that swept parks its report as the ceremony's would be"
        );
        for answer in [
            SweepRetryAnswer::NoOldState,
            SweepRetryAnswer::NotLanded,
            SweepRetryAnswer::LandedForAnother,
            SweepRetryAnswer::Failed("rpc disconnected".to_string()),
        ] {
            let kind = answer.kind();
            let parked = answer.into_owed_status();
            assert!(
                !matches!(parked, SweepStatus::Ran(_)),
                "{kind}: nothing swept, so nothing may be reported as done"
            );
            assert!(
                parked.owes_work(),
                "{kind}: the parked arm must still owe work, or the retry button never renders"
            );
        }
        assert!(
            matches!(
                SweepRetryAnswer::Failed("rpc disconnected".to_string()).into_owed_status(),
                SweepStatus::Failed(reason) if reason == "rpc disconnected"
            ),
            "a transport failure keeps its reason on the parked line"
        );
    }

    /// The transport arm's sentence carries the reason it was given, rather
    /// than dropping it into a log the user cannot see.
    #[test]
    fn the_failed_retry_answer_carries_its_reason() {
        let line = SweepRetryAnswer::Failed("rpc disconnected".to_string())
            .message()
            .expect("the failed arm speaks");
        assert_eq!(
            line.args.get("reason").map(String::as_str),
            Some("rpc disconnected"),
            "the reason must reach the sentence: {line:?}"
        );
    }
}

/// The whole "my identity was stolen" ceremony as an app runs it — over an
/// **anonymous** connection it dials itself, so the one composition serves the
/// signed-in Settings section and the locked-out launch surface alike
/// (`devices.md` § The two panic buttons, point 5: the ceremony "must be
/// reachable from a locked-out app and must ride an anonymous connection";
/// § The locked state).
///
/// Composes, in the only safe order: [`crate::succeed_with_held_kit`] (parse the
/// kit, mint the successor, author and submit the statement) → **persist the
/// successor seed** into `accounts` → the group sweep on the confirmed arm, or
/// the reconcile on the unconfirmed one. Every arm comes back as a
/// [`StolenOutcome`]; nothing here returns an `Err` that could drop a seed.
///
/// ## Why it never takes the app's own connection
///
/// Every kind the ceremony sends is pre-identity (`registration.chain`,
/// `succession.submit`, `succession.lookup`), the kit is the whole
/// authorization, and a thief holding the seed can revoke every session and
/// lock the account — so a ceremony that needed the app's bearer would be
/// disabled by the attack it remedies. A signed-in caller gains nothing from
/// riding its session either: the succession transaction revokes that bearer
/// before the reply is sent.
///
/// ## Parameters
///
/// `old_identity` is the seed this device still holds; it rides as the
/// statement's informational `old_sig` and names the account. `old_engine` is
/// the retired identity's **live** MLS engine when the app has one — never a
/// second engine opened over the same store (the app's ceremony op carries the
/// reasoning) — and `None` on a locked-out app, which has no session: the sweep
/// then reports that it did not run and the successor finishes it from Settings
/// (`retry_sweep_as_successor`). `db_path_for` is the app's per-account store
/// resolver, invoked only after the successor's connect.
///
/// ## What the caller still owes
///
/// On [`StolenOutcome::Landed`]: record the predecessor → successor link and
/// switch to the successor (tui: `session::adopt_successor`), then the closing
/// kit. On [`StolenOutcome::Undecided`]: paint [`StolenOutcome::message`], which
/// puts the seed on screen when its persist did not read back.
#[cfg(not(target_arch = "wasm32"))]
pub async fn succeed_stolen_identity(
    nest_url: &str,
    old_identity: &fauna_core::identity::ActorKeypair,
    kit_input: &str,
    old_engine: Option<&fauna_mls::engine::MlsEngine>,
    accounts: &fauna_client_accounts::AccountRegistry,
    db_path_for: impl FnOnce(&str) -> std::path::PathBuf,
) -> StolenOutcome {
    // Unreachable before anything was minted: nothing moved.
    let anon = match fauna_anon_client::AnonymousNestClient::connect(nest_url).await {
        Ok(anon) => anon,
        Err(e) => return StolenOutcome::not_landed(e),
    };
    let client = crate::RecoveryClient::new(anon);
    // No status re-read first: the kit is the whole authorization, and a chain
    // read here would only add a round trip a locked-out owner can fail on. A
    // refusal before the submit is the not-landed arm — the seed it minted
    // authorizes nothing.
    let attempt = match crate::succeed_with_held_kit(
        &client,
        old_identity.actor_id(),
        kit_input,
        Some(old_identity),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(e) => return StolenOutcome::not_landed(e),
    };

    // FIRST, before anything that can fail or block: make the successor seed
    // durable. From the nest's commit until this line that seed is the only
    // copy of the key the account now belongs to, and the sweep below makes
    // network calls over every group. Read off the attempt rather than a
    // matched arm: BOTH arms carry the seed, and the unconfirmed one is
    // precisely the case where the nest may have committed while telling us it
    // did not. Through the caller's own registry, because the unconfirmed arm
    // READS BACK through it to decide whether it may claim the seed is saved.
    if let Err(e) = accounts.add_account(attempt.successor_secret_hex(), Some(nest_url), None) {
        // Not fatal: the app's adoption tries again and, failing that, puts
        // the seed on screen as the only way back. Logged because this is the
        // moment of maximum exposure.
        tracing::error!(
            "[recovery] persisting the successor seed straight after the succession landed: {e}"
        );
    }

    // Everything below is the *propagation* half — it can fail without
    // unmaking the succession, so its errors ride the outcome as a report.
    match attempt {
        crate::SuccessionAttempt::Confirmed(handoff) => {
            let sweep = sweep_after_succession(nest_url, old_engine, &handoff, db_path_for).await;
            StolenOutcome::Landed(LandedSuccession::new(
                handoff.successor_secret_hex().to_string(),
                handoff.new_actor_id,
                sweep,
                handoff.succeeded_at,
            ))
        }
        crate::SuccessionAttempt::Unconfirmed(unconfirmed) => {
            finish_unconfirmed_succession(
                nest_url,
                old_engine,
                unconfirmed.old_actor_id,
                unconfirmed.successor_secret_hex(),
                &unconfirmed.error,
                accounts,
                db_path_for,
            )
            .await
        }
    }
}

/// Finish a succession whose submit reply never arrived — the app half of
/// the fix (`identity-succession.md` § Implementation status today).
///
/// The nest commits the succession *before* it replies, so the ceremony can
/// return "failed" about an account that has already moved. The shared crate
/// keeps the successor seed on that arm instead of dropping it; this decides
/// what actually happened and, if the account did move, finishes exactly as the
/// confirmed path does. The caller has **already persisted the seed** by the
/// time this runs — that ordering is the whole point, and nothing here may be
/// allowed to precede it.
///
/// ⚠ **The reconcile gets its own anonymous connection, deliberately.** The
/// overwhelmingly likely reason we are here at all is that the connection which
/// ran the ceremony died — and even when it did not, the succession transaction
/// revokes its bearer. `succession.lookup` and `registration.chain` are both
/// pre-identity kinds, so an anonymous client is all the verification needs;
/// re-using the dying one would turn "did it land?" into a second transport
/// failure and strand the user for the same reason twice.
/// Split over the four things it actually needs, for the reason
/// [`sweep_as_successor`] is: a
/// [`crate::UnconfirmedSuccession`] is minted only by a real
/// ceremony against a real nest and has no public constructor, so a body taking
/// the whole value would be reachable only from a multi-minute tier_3 run — and
/// the arm that matters most here is the one where the nest is *unreachable*,
/// which no journey can stage.
///
/// `accounts` is here for one reason: the two arms below that cannot decide the
/// outcome tell the user the successor identity **is saved on this device**, and
/// that sentence has to be downstream of a persist this function actually
/// verified. It is a *read-back*, not a threaded return code, because
/// `SecretStore::set` is infallible by signature — a clean `add_account` return
/// proves the call was made, never that anything landed.
#[cfg(not(target_arch = "wasm32"))]
pub async fn finish_unconfirmed_succession(
    nest_url: &str,
    old_engine: Option<&fauna_mls::engine::MlsEngine>,
    old_actor_id: fauna_core::identity::ActorId,
    successor_secret_hex: &str,
    reported: &crate::RecoveryError,
    accounts: &fauna_client_accounts::AccountRegistry,
    db_path_for: impl FnOnce(&str) -> std::path::PathBuf,
) -> StolenOutcome {
    // The persisted seed as a keypair: the reconcile matches on the successor we
    // hold the KEY for, never on a successor the nest names, so a nest that
    // invents one cannot make this device adopt it.
    let successor = match fauna_core::identity::ActorKeypair::from_secret_hex(successor_secret_hex)
    {
        Ok(successor) => successor,
        Err(e) => {
            // Unreachable in practice (this device minted the seed a moment
            // ago), and undecidable if reached. `persisted: false` on purpose:
            // with no key there is no actor id to read the store back under, so
            // "saved on this device" is licensed by nothing.
            return StolenOutcome::undecided(
                format!("the successor key this device minted is unreadable ({e})"),
                false,
                successor_secret_hex,
                reported,
            );
        }
    };

    // ⚠ Verified by READ-BACK, before either undecidable arm below can claim it.
    // The caller persists the seed just before calling us, but `SecretStore::set`
    // is infallible by signature, so its success proves nothing — only asking the
    // registry for the secret back does. A device that failed to save it
    // is the one case where the user MUST be shown the seed, and it is exactly
    // the case a bare "saved on this device" would hide.
    // ⚠ The equality half is defense-in-depth and is deliberately NOT pinned:
    // staging it needs a store that keeps a *different* secret under this actor
    // id, a state `add_account` cannot produce (the id is derived from the seed).
    // Presence is what the tests exercise; the comparison guards corruption.
    let persisted = accounts
        .secrets(&successor.actor_id().to_hex())
        .is_some_and(|stored| stored.secret_hex.as_str() == successor_secret_hex);

    let anon = match fauna_anon_client::AnonymousNestClient::connect(nest_url).await {
        Ok(anon) => anon,
        Err(e) => {
            // Undecided, and undecidable from here. Say so plainly rather than
            // reporting the original failure as final.
            return StolenOutcome::undecided(
                format!("the nest could not be reached to check ({e})"),
                persisted,
                successor_secret_hex,
                reported,
            );
        }
    };
    let client = crate::RecoveryClient::new(anon);
    match crate::reconcile_succession(&client, old_actor_id, &successor, None).await {
        // It landed after all. Finish the ceremony as if the reply had arrived.
        Ok(crate::ReconciledSuccession::Landed(handoff)) => {
            tracing::info!(
                "[settings/recovery] the succession landed despite the lost reply; finishing"
            );
            let sweep = sweep_after_succession(nest_url, old_engine, &handoff, db_path_for).await;
            StolenOutcome::Landed(LandedSuccession {
                successor_secret_hex: zeroize::Zeroizing::new(
                    handoff.successor_secret_hex().to_string(),
                ),
                new_actor_id: handoff.new_actor_id,
                sweep,
                // Read off the handoff rather than hard-coded `None`: the field
                // is already `Option` for exactly this arm, and reading it keeps
                // the one place that decides "unknown" inside the shared crate.
                succeeded_at: handoff.succeeded_at,
            })
        }
        // Nothing committed: the minted seed authorizes nothing, the account is
        // still the user's old identity, and the honest thing to show is the
        // failure that actually happened. They may simply try again.
        Ok(crate::ReconciledSuccession::NotLanded) => StolenOutcome::not_landed(reported),
        // Someone else's ceremony won. `old_actor_id` is the succession table's
        // primary key, so this is final — and must never read as "try again".
        Ok(crate::ReconciledSuccession::LandedForAnother { new_actor_id }) => {
            StolenOutcome::LandedForAnother { new_actor_id }
        }
        Err(e) => StolenOutcome::undecided(
            format!("checking with the nest failed ({e})"),
            persisted,
            successor_secret_hex,
            reported,
        ),
    }
}

/// Re-point every MLS group the succeeded identity held, straight after the
/// succession landed (`identity-succession.md` § Propagation → *MLS groups*).
///
/// ## Why here, and why before the account switch
///
/// The driver needs the two co-resident engines, and this is the only point in
/// the app's life where both are available at once under the one-engine-per-
/// store invariant:
///
/// * the **old** engine is the live conversations engine, still open over the
///   succeeded account's `mls_state.db` (see the app's ceremony op (tui: `Op::RecoveryStolen::old_engine`)
///   for why it must be reused, not re-opened);
/// * the **successor's** store has never been opened by anything, so this is
///   the one engine we may legitimately construct.
///
/// A moment later the app's account switch (tui: `session::adopt_successor`) switches the account,
/// which drops the conversations session and rebuilds it over the *successor's*
/// scope — at which point the old engine is gone and the pair no longer exists.
///
/// ## Failure is reported, never fatal
///
/// The succession has already landed when this runs: the account belongs to the
/// successor whatever happens here. So every failure below degrades to a
/// [`SweepReport`] the surface renders, and none of them turn the ceremony into
/// an `Err` — losing group re-key is bad, but reading it as "the succession
/// failed" would be worse, since the user's next move (write the successor seed
/// down) is the same either way.
#[cfg(not(target_arch = "wasm32"))]
pub async fn sweep_after_succession(
    nest_url: &str,
    old_engine: Option<&fauna_mls::engine::MlsEngine>,
    handoff: &crate::SuccessionHandoff,
    db_path_for: impl FnOnce(&str) -> std::path::PathBuf,
) -> SweepStatus {
    sweep_as_successor(
        nest_url,
        old_engine,
        handoff.successor_secret_hex(),
        &handoff.statement,
        db_path_for,
    )
    .await
}

/// Mirror a landed registration's chain head onto the profile — the one step
/// the kit ceremonies deliberately leave to their caller
/// (`identity-succession.md` § The RecoveryKey: peers cache the binding with the
/// profile they already hold, so a succession verifies with no chain fetch).
///
/// **One home, three callers.** `fauna_client_profile::publish_recovery_head`
/// already shares the read-modify-write; what kept getting re-derived around it
/// is this wrapper — the home-nest entry, the best-effort arm, and *when* to
/// call it — which tui and linux had each written out and which every FFI app
/// was about to write a third time. Web keeps its own (`libs/fauna-wasm`): it
/// derives the same two facts from browser primitives, so its difference is
/// transport, not policy.
///
/// **Best-effort by design, and it must stay that way.** The field is a cache —
/// the registration chain is authoritative — so a failure here costs peers one
/// chain fetch. Turning it into a ceremony failure would report "no kit" for a
/// kit that is minted, registered, and whose secret is already on its way to the
/// screen.
///
/// ⚠ **Call it only where the chain actually MOVED** (create / replace, and the
/// successor's own closing-act mint), never after a seed-alone request: that
/// opens the 30-day window without advancing the head, so mirroring there would
/// publish a binding no consumer should yet honor.
///
/// `predecessors` is the caller's registry record of whom `identity` succeeded
/// from (`AccountRegistry::predecessors_of`, through
/// `fauna_client_profile::predecessors_from_hex`). It is what lets the closing
/// act's mirror re-publish a profile the succession moved onto the successor;
/// any other base the nest serves is refused, and the refusal lands in the
/// same best-effort log line as every other mirror failure.
#[cfg(not(target_arch = "wasm32"))]
pub async fn mirror_recovery_head(
    nest: Arc<NestClient>,
    identity: &fauna_core::identity::ActorKeypair,
    predecessors: &[fauna_core::identity::ActorId],
    kit: &crate::RecoveryKit,
) {
    let home = home_nest(&nest);
    if let Err(e) = fauna_client_profile::publish_recovery_head(
        Arc::clone(&nest),
        identity,
        predecessors,
        kit.chain_head(),
        Some(home),
    )
    .await
    {
        tracing::warn!("recovery-head profile mirror: {e}");
    }
}

/// Register the kit the onboarding `recovery_kit` screen minted, at the
/// wizard's signed-in handoff — the one point custody permits it
/// (`identity-succession.md` § The RecoveryKey → *Creation UX*: no nest existed
/// at the screen's position, and the root is never persisted, so it cannot
/// survive to a later session). `kit_hex` is what
/// `OnboardingMachine::take_pending_recovery_secret` handed over.
///
/// A first registration (`prior: None`) — the screen is reachable only on the
/// create-identity path, so the chain is empty — carrying no predecessor seeds:
/// a brand-new identity succeeded from nobody. The landed head is then mirrored
/// into the signed profile ([`mirror_recovery_head`]); this account has almost
/// certainly never published one, which is the not-found arm that mints a
/// minimal profile and gives a future peer harvest its dial domain.
///
/// Never an error the user sees: a failure here leaves Settings telling the
/// truth (never-created, or registered-no-escrow when only the put failed), so
/// every arm is logged and the caller fires it and forgets it. One body for
/// every app that runs the shared crate in-process (tui, linux).
#[cfg(not(target_arch = "wasm32"))]
pub async fn register_deferred_kit(
    nest: Arc<NestClient>,
    identity: &fauna_core::identity::ActorKeypair,
    kit_hex: &fauna_core::secret::SecretString,
) {
    let root = match fauna_core::recovery::RecoveryKey::from_hex(kit_hex.as_str()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("deferred recovery kit: minted root unreadable: {e}");
            return;
        }
    };
    let client = RecoveryClient::new(Arc::clone(&nest));
    match crate::create_kit_with_root(&client, identity, None, root, &[]).await {
        Ok(kit) => {
            mirror_recovery_head(nest, identity, &[], &kit).await;
            match &kit.escrow {
                crate::EscrowOutcome::Stored { .. } => {
                    tracing::info!(seq = kit.seq, "deferred recovery kit registered");
                }
                crate::EscrowOutcome::Failed { reason } => {
                    tracing::warn!(
                        seq = kit.seq,
                        %reason,
                        "deferred recovery kit registered but the escrow put failed; \
                         Settings will show registered-no-escrow"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "deferred recovery kit registration failed ({e}); \
                 Settings will show the never-created warning"
            );
        }
    }
}

/// The home-nest facts a mirror publishes as the profile's primary `nests`
/// entry when none exists — the peer-profile harvest's dial-domain producer.
/// The URL is the one this session dialed; the identity is the TOFU pin the
/// connect graduated, when one rests.
#[cfg(not(target_arch = "wasm32"))]
fn home_nest(nest: &NestClient) -> fauna_client_profile::HomeNest {
    let url = nest.nest_url();
    let nest_id =
        fauna_anon_client::trust::pinned_identity(&fauna_anon_client::trust::authority_of(&url));
    fauna_client_profile::HomeNest { url, nest_id }
}

/// Learn the succession link this device's registry does not hold, and persist
/// it — the per-sign-in hop that un-strands a successor's inherited profile on
/// a device that never held the predecessor's row, or lost it when the user
/// removed the retired account (`profile.md` § After an identity succession →
/// *A successor device with no recorded succession link*).
///
/// **One home.** The proof is `fauna_client_profile::learn_inherited_predecessors`
/// (what is asked, what is believed, and why the successor's own `new_sig` is
/// enough); the record is `AccountRegistry::record_predecessors`. This is only
/// the seam that holds both, so no app re-derives "read, prove, persist". Web
/// keeps its own two lines in `libs/fauna-wasm`, for the same reason
/// [`mirror_recovery_head`] does: this crate's registry dependency is native.
///
/// Returns whether a link was newly recorded. Once it is, every reader of
/// `AccountRegistry::predecessors_of` sees it with no further lookup: the
/// profile writers admit the inherited base, and the post-succession aftermath
/// stops answering "not a successor" on this device.
///
/// **It also restores the harvest anchors an external-app edit left absent**,
/// over the same base read: `fauna_client_profile::restore_delegated_anchors`
/// owns what it checks, where the head comes from and why it cannot loop
/// (`identity-succession.md` § the peer-profile harvest, rule 1).
///
/// **Best-effort, and nearly free.** One `fauna.profile.get` per sign-in, and a
/// lookup only over a base this device cannot place — or, over a delegated
/// one, a chain read and one re-publish. Every failure is a log line and a
/// retry at the next sign-in; nothing a user waits on hangs off it.
#[cfg(not(target_arch = "wasm32"))]
pub async fn learn_succession_link(
    nest: Arc<NestClient>,
    accounts: &fauna_client_accounts::AccountRegistry,
    identity: &fauna_core::identity::ActorKeypair,
) -> bool {
    match read_own_profile_learning_link(Arc::clone(&nest), accounts, identity).await {
        Ok((base, learned)) => {
            let predecessors = fauna_client_profile::predecessors_from_hex(
                &accounts.predecessors_of(&identity.actor_id_hex()),
            );
            let home = home_nest(&nest);
            match fauna_client_profile::restore_delegated_anchors(
                nest,
                identity,
                &predecessors,
                base.as_deref(),
                Some(home),
            )
            .await
            {
                Ok(fauna_client_profile::AnchorRestore::Published) => {
                    tracing::info!("restored the profile's anchors after a delegated edit");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("restoring the profile's anchors: {e}"),
            }
            learned
        }
        Err(e) => {
            tracing::debug!("succession link: {e}");
            false
        }
    }
}

/// The profile edit form's base load: `identity`'s own stored profile, read
/// through the same hop as [`learn_succession_link`], so a link that base needs
/// is proven and persisted **before** the form can save over it.
///
/// Without this a linkless successor's first save races the sign-in hop, which
/// is spawned rather than awaited, and a hop that failed would refuse every
/// edit until the next sign-in
/// (`fauna_client_profile::fetch_own_profile_base` has the reasoning). Once it
/// returns, `AccountRegistry::predecessors_of` names every predecessor the base
/// needs, so a writer handed that list admits it.
///
/// `Ok(None)` is a never-published profile, which is a first publish. An `Err`
/// is the read failing; recording the link is best-effort, as in the hop.
///
/// Wasm-clean and generic over the transport, unlike its neighbours: all 7
/// apps' edit forms load their base here — tui and linux directly, the FFI apps
/// through `FfiProfileClient::load_edit_base`, web through the wasm
/// `loadProfileEditBase` face.
pub async fn load_profile_edit_base<R>(
    nest: R,
    accounts: &fauna_client_accounts::AccountRegistry,
    identity: &fauna_core::identity::ActorKeypair,
) -> Result<Option<Vec<u8>>, fauna_client_profile::LearnPredecessorsError<R::Error>>
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    Ok(read_own_profile_learning_link(nest, accounts, identity)
        .await?
        .0)
}

/// Read, prove, persist — the one body behind [`learn_succession_link`],
/// [`load_profile_edit_base`] and web's sign-in hop (`fauna-wasm`'s
/// `run_aftermath_web`, which cannot reach the native
/// [`learn_succession_link`]). Returns the stored bytes and whether a link was
/// newly recorded.
pub async fn read_own_profile_learning_link<R>(
    nest: R,
    accounts: &fauna_client_accounts::AccountRegistry,
    identity: &fauna_core::identity::ActorKeypair,
) -> Result<(Option<Vec<u8>>, bool), fauna_client_profile::LearnPredecessorsError<R::Error>>
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    let actor_hex = identity.actor_id_hex();
    let recorded =
        fauna_client_profile::predecessors_from_hex(&accounts.predecessors_of(&actor_hex));
    let base = fauna_client_profile::fetch_own_profile_base(nest, identity, &recorded).await?;
    if base.proven.is_empty() {
        return Ok((base.body, false));
    }
    let hexes: Vec<String> = base.proven.iter().map(|id| id.to_hex()).collect();
    let learned = match accounts.record_predecessors(&actor_hex, &hexes) {
        Ok(()) => {
            tracing::info!(
                predecessors = hexes.len(),
                "learned this identity's succession link from its landed statement"
            );
            true
        }
        Err(e) => {
            tracing::warn!("recording the learned succession link: {e}");
            false
        }
    };
    Ok((base.body, learned))
}

/// [`sweep_after_succession`]'s body, over the two things it actually needs from
/// the handoff.
///
/// Split out because a [`crate::SuccessionHandoff`] is minted
/// only by a real ceremony against a real nest — it has no public constructor —
/// so the whole-sweep contracts below would otherwise be reachable only from a
/// ~20-minute tier_3 run. These two arguments are its entire content here.
#[cfg(not(target_arch = "wasm32"))]
pub async fn sweep_as_successor(
    nest_url: &str,
    old_engine: Option<&fauna_mls::engine::MlsEngine>,
    successor_secret_hex: &str,
    statement: &fauna_core::recovery::SignedIdentitySuccession,
    db_path_for: impl FnOnce(&str) -> std::path::PathBuf,
) -> SweepStatus {
    let Some(old_engine) = old_engine else {
        return SweepStatus::NoEngine;
    };
    // Built twice from the one secret rather than cloned: `ActorKeypair` is
    // deliberately not `Clone` (it is signing key material), and the engine and
    // the client each need their own.
    let build_successor =
        || fauna_core::identity::ActorKeypair::from_secret_hex(successor_secret_hex);
    // The successor's OWN session, established BEFORE its engine: the succession
    // revoked the old identity's bearers inside its own transaction, so the
    // client that ran the ceremony is already dead.
    //
    // Connecting first is deliberate. The next step creates the successor's MLS
    // store on disk, and a nest we cannot reach makes that store an orphan the
    // post-switch session would then adopt as its own — so an unreachable nest
    // must fail before anything is written, not after.
    let signer = match build_successor() {
        Ok(keypair) => keypair,
        Err(e) => return SweepStatus::Failed(format!("{e}")),
    };
    let nest = NestClient::new(nest_url.to_string(), signer);
    // `NestClient::new` performs **no I/O** — it only builds the client (the
    // same note `conv_backend.rs` carries over its own offline client).
    // Authentication *and* the reconnect supervisor that populates the
    // dispatcher both live in `connect()`, so an unconnected client has no
    // dispatcher at all: every request takes `request_inner`'s wait-for-
    // reconnect path, waits the full 30 s spec deadline for a supervisor that
    // was never spawned, and then fails `RpcDisconnected{was_in_flight:false}`
    // having never reached the nest. That is exactly what killed the sweep on
    // its first real group — 30 s per group, no sign-in in the nest log, and a
    // per-group `Failed("transport error: rpc disconnected")` that reads like a
    // nest refusal but never left the app.
    if let Err(e) = nest.connect().await {
        // The account has already moved; only propagation is lost. Reported as
        // a whole-sweep failure rather than per-group noise: nothing was
        // attempted, because the successor never got a session.
        return SweepStatus::Failed(format!("connecting as the successor: {e}"));
    }
    let successor = match build_successor() {
        Ok(keypair) => keypair,
        Err(e) => return SweepStatus::Failed(format!("{e}")),
    };
    // The successor's own scoped store — the same path
    // `conv_backend::start_conversations_session` will open after the switch,
    // so the groups this sweep joins are the ones the rebuilt session finds.
    // Resolved HERE, after the connect above: the resolver may write (it may
    // create the store), and an unreachable nest must fail before then.
    let db_path = db_path_for(&successor.actor_id_hex());
    let successor_engine = match fauna_mls::engine::MlsEngine::new(successor, &db_path) {
        Ok(engine) => engine,
        Err(e) => return SweepStatus::Failed(format!("{e}")),
    };
    let client = RecoveryClient::new(Arc::clone(&nest));
    let report = crate::sweep_groups(&client, old_engine, &successor_engine, statement).await;
    // This client is the sweep's alone. Its supervisor is a spawned task
    // holding its own `Arc`s, so dropping the client does NOT stop it — without
    // this it would outlive the ceremony and keep a second, unobserved WS for
    // the successor reconnecting for the life of the process, alongside the one
    // the post-switch session is about to build.
    nest.disconnect().await;
    // Persistence is deliberately the caller's (the driver's module doc): a web
    // engine persists to its nest replica, a native one to SQLite. Both engines
    // moved — the old one ratcheted past remove-old, the successor joined every
    // group — so both must land before the switch tears this pair down.
    if let Err(e) = old_engine.save_state() {
        tracing::error!("[settings/recovery] persisting the succeeded engine after the sweep: {e}");
    }
    if let Err(e) = successor_engine.save_state() {
        tracing::error!("[settings/recovery] persisting the successor engine after the sweep: {e}");
    }
    SweepStatus::Ran(Box::new(report))
}

/// Finish a sweep the ceremony left unfinished — `recovery-kit-sweep-retry-button`'s
/// one call, over the shared [`crate::retry_group_sweep`]
/// (`succession-aftermath.md` § Propagation → *MLS groups*).
///
/// **Lifted here 2026-08-27 from `apps/fauna-tui/src/settings/mod.rs::retry_sweep_op`**, where it was one app's private orchestration
/// while the FFI apps had no route to the retry at all and were about to write
/// it a second and third time in Swift and C#. What was tui-shaped about it was
/// only the two store paths, and those are now the caller's two resolvers; every
/// judgment — which refusals are terminal, which arm is a failure, what each one
/// says — is here.
///
/// ## The app's half
///
/// * `nest` — the **successor's own live, connected session**. Unlike
///   [`sweep_as_successor`] this rides the session the app already has rather
///   than building and connecting one: the retry runs long after the account
///   switch, so by definition the successor is signed in. It follows that the
///   "connect before anything is written" ordering that shapes the ceremony's
///   sweep has no analogue here.
/// * `old_secret_hex` — the retired identity's seed, off the app's account
///   registry (`fauna_client_recovery::predecessor_seeds_from_rows` over its
///   predecessor rows). `None` is an ordinary answer, not an error: a device
///   that never held the retired identity says so in words.
/// * `old_db_path_for` / `successor_db_path_for` — the two MLS stores, resolved
///   **by the app** and, as everywhere else in this module, as callbacks rather
///   than eager paths, because a resolver is allowed to create or adopt. They
///   are given the actor hex each store belongs to; the retired one is derived
///   from `old_secret_hex` here rather than passed alongside it, so a caller
///   cannot hand in a seed and a hex that disagree.
/// * `live_successor_engine` — the running conversations engine when there is
///   one, so the retry never opens a second engine over a store MLS already has
///   open. `None` builds one over `successor_db_path_for`, exactly as the
///   ceremony's sweep does.
///
/// Persistence is **not** the caller's here, unlike [`crate::sweep_groups`]:
/// both engines moved and both are saved before the [`SweepRetryAnswer::Swept`]
/// arm returns, because there is no post-sweep step left for a caller to hang
/// them off — the ceremony's sweep saves inside the fold for the same reason.
#[cfg(not(target_arch = "wasm32"))]
pub async fn retry_sweep_as_successor(
    nest: Arc<NestClient>,
    successor_secret_hex: &str,
    old_secret_hex: Option<&str>,
    old_db_path_for: impl FnOnce(&str) -> std::path::PathBuf,
    successor_db_path_for: impl FnOnce(&str) -> std::path::PathBuf,
    live_successor_engine: Option<&fauna_mls::engine::MlsEngine>,
) -> SweepRetryAnswer {
    // No seed for the retired identity on this device: the honest answer, not a
    // failure. The user succeeded elsewhere, and the sentence names what is
    // left to do.
    let Some(old_secret_hex) = old_secret_hex else {
        return SweepRetryAnswer::NoOldState;
    };
    let old_keypair = match fauna_core::identity::ActorKeypair::from_secret_hex(old_secret_hex) {
        Ok(keypair) => keypair,
        Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
    };
    let old_db = old_db_path_for(&old_keypair.actor_id_hex());
    // The existence check the whole op turns on. `try_exists` rather than
    // `exists` so an unreadable path reports itself instead of masquerading as
    // "no state here" and sending the user to another device for nothing.
    match old_db.try_exists() {
        Ok(true) => {}
        // The seed is here but the conversation store is not — same answer, and
        // the sentence is written to hold for both (it names another device
        // conditionally rather than asserting one has it, since on this arm
        // THIS device may well be the one that ran the ceremony).
        Ok(false) => return SweepRetryAnswer::NoOldState,
        Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
    }
    let old_engine = match fauna_mls::engine::MlsEngine::new(old_keypair, &old_db) {
        Ok(engine) => engine,
        Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
    };

    // Built twice from the one secret rather than cloned, for the same reason
    // `sweep_as_successor` does it: `ActorKeypair` is signing key material and
    // deliberately not `Clone`, and the engine and the sweep each need their own.
    let successor = match fauna_core::identity::ActorKeypair::from_secret_hex(successor_secret_hex)
    {
        Ok(keypair) => keypair,
        Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
    };
    // The successor's engine: the live one when conversations are up — never a
    // second over the same store — else one constructed over the successor's own
    // scope, exactly as the ceremony's sweep does.
    let owned_engine = match live_successor_engine {
        Some(_) => None,
        None => {
            let signer =
                match fauna_core::identity::ActorKeypair::from_secret_hex(successor_secret_hex) {
                    Ok(keypair) => keypair,
                    Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
                };
            let db = successor_db_path_for(&successor.actor_id_hex());
            match fauna_mls::engine::MlsEngine::new(signer, &db) {
                Ok(engine) => Some(engine),
                Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
            }
        }
    };
    let successor_engine: &fauna_mls::engine::MlsEngine =
        match (live_successor_engine, &owned_engine) {
            (Some(live), _) => live,
            (None, Some(owned)) => owned,
            // Unreachable: exactly one of the two is built above.
            (None, None) => return SweepRetryAnswer::Failed("no successor engine".to_string()),
        };

    let client = RecoveryClient::new(nest);
    let outcome =
        match crate::retry_group_sweep(&client, &old_engine, successor_engine, &successor).await {
            Ok(outcome) => outcome,
            // The transport arm gets the retry's OWN copy rather than a bare
            // error string: it is the arm a user meets on an ordinary network
            // flake, it is safe to press again, and the sentence has to say so —
            // every other arm here names a next step, and this one must too.
            Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
        };

    match outcome {
        crate::SweepRetryOutcome::Swept(report) => {
            // Both engines moved — the old one ratcheted past remove-old, the
            // successor joined every group — so both must land.
            if let Err(e) = old_engine.save_state() {
                tracing::error!(
                    "[settings/recovery] persisting the retired engine after the retry: {e}"
                );
            }
            if let Err(e) = successor_engine.save_state() {
                tracing::error!(
                    "[settings/recovery] persisting the successor engine after the retry: {e}"
                );
            }
            SweepRetryAnswer::Swept(Box::new(report))
        }
        // The two terminal no-post answers, each its own sentence rather than a
        // sweep report: neither swept anything, and a `Ran` over an empty report
        // would render as "removed from all 0 of your groups".
        crate::SweepRetryOutcome::NotLanded => SweepRetryAnswer::NotLanded,
        crate::SweepRetryOutcome::LandedForAnother { .. } => SweepRetryAnswer::LandedForAnother,
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A `SecretStore` a test can make succeed or fail, for the succession's
    /// undecidable arms.
    ///
    /// It exists because `SecretStore::set` is **infallible by signature** — the
    /// trait cannot report a failure — so "the store did not keep it" can only
    /// be staged by a store that silently drops writes, which is also exactly
    /// how a real one fails (a full disk, a locked keychain). Read-back is the
    /// only detector on either side.
    #[derive(Default)]
    struct FakeSecretStore {
        rows: std::sync::Mutex<std::collections::HashMap<String, String>>,
        /// When true, `set` accepts every write and keeps none of them.
        drop_writes: bool,
    }

    impl fauna_client_accounts::SecretStore for FakeSecretStore {
        fn get(&self, key: &str) -> Option<String> {
            self.rows.lock().unwrap().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) {
            if self.drop_writes {
                return;
            }
            self.rows
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
        }
        fn delete(&self, key: &str) {
            self.rows.lock().unwrap().remove(key);
        }
    }

    /// Build a registry over a fake store, optionally with the successor's seed
    /// already persisted the way the production call site persists it.
    fn registry_for(
        successor: &fauna_core::identity::ActorKeypair,
        persisted: bool,
    ) -> fauna_client_accounts::AccountRegistry {
        let store = std::sync::Arc::new(FakeSecretStore {
            drop_writes: !persisted,
            ..Default::default()
        });
        let registry = fauna_client_accounts::AccountRegistry::new(
            store as std::sync::Arc<dyn fauna_client_accounts::SecretStore>,
        );
        // The same call the ceremony makes just before the undecidable arms.
        // With `drop_writes` it RETURNS OK and keeps nothing — the whole point.
        let _ = registry.add_account(
            &fauna_core::hex32::encode(successor.secret_bytes()),
            Some("http://127.0.0.1:1"),
            None,
        );
        registry
    }

    /// Pin for `retry_predecessor`'s actual safety property: `.next()` is safe because the walk
    /// is seeded with the successor itself, never because of
    /// `predecessors_of`'s ordering — pinning that ordering alone does not
    /// pin this use. Same branched shape as `fauna_client_accounts`'s
    /// companion pin (A→B→C, F→E→D→C, so C has two direct predecessors B and
    /// D plus deeper ancestors A, E, F): `retry_predecessor` must return a
    /// DIRECT predecessor, never a deeper one, even though `predecessors_of`
    /// legitimately offers all five.
    ///
    /// **Also pins the documented, currently undecided tie-break** for the
    /// "several direct predecessors" case (this fn's own doc comment): B was
    /// registered before D, so B wins, whether or not this device holds D's
    /// seed instead. Deliberate — deciding whether a seed-aware pick should
    /// replace this is future work, not silently redesigned here.
    #[test]
    fn retry_predecessor_always_returns_a_direct_predecessor_never_a_deeper_ancestor() {
        let store = std::sync::Arc::new(FakeSecretStore::default());
        let registry = fauna_client_accounts::AccountRegistry::new(
            store as std::sync::Arc<dyn fauna_client_accounts::SecretStore>,
        );
        let secret = |b: u8| fauna_core::hex32::encode(&[b; 32]);
        let a = registry.add_account(&secret(1), None, None).unwrap();
        let b = registry.add_account(&secret(2), None, None).unwrap();
        let c = registry.add_account(&secret(3), None, None).unwrap();
        let d = registry.add_account(&secret(4), None, None).unwrap();
        let e = registry.add_account(&secret(5), None, None).unwrap();
        let f = registry.add_account(&secret(6), None, None).unwrap();
        registry.record_succession(&a, &b).unwrap();
        registry.record_succession(&b, &c).unwrap();
        registry.record_succession(&f, &e).unwrap();
        registry.record_succession(&e, &d).unwrap();
        registry.record_succession(&d, &c).unwrap();

        let (picked, _seed) =
            retry_predecessor(&registry, &c).expect("C was succeeded — a retry is offered");
        assert!(
            picked == b || picked == d,
            "retry_predecessor must pick a DIRECT predecessor (B or D), never \
             a deeper ancestor (A, E or F) even though predecessors_of(c) \
             legitimately offers all five: picked {picked}"
        );
        assert_eq!(
            picked, b,
            "today's documented tie-break is account-index order (B was \
             registered before D) — a change here is a deliberate redesign, \
             not a drift; update this pin alongside it"
        );
    }

    /// This is the worst cell of the matrix and the one no journey can stage: a
    /// submit whose reply was lost, followed by a nest that stays unreachable.
    /// The account may already belong to the successor this device just minted,
    /// so a message reading "identity succession: transport error" is actively
    /// harmful — the user concludes nothing happened, and the key to their own
    /// account is sitting unmentioned in the store. The honest message says the
    /// outcome is unknown **and** names the way back in.
    #[tokio::test]
    async fn an_unreachable_nest_never_reports_an_unconfirmed_succession_as_a_plain_failure() {
        let successor = fauna_core::identity::ActorKeypair::generate();
        let old = fauna_core::identity::ActorKeypair::generate();
        let reported = crate::RecoveryError::Transport("rpc disconnected".to_string());

        // Port 1 is never listening — the reconcile cannot reach anyone.
        let outcome = finish_unconfirmed_succession(
            "http://127.0.0.1:1",
            None,
            old.actor_id(),
            &fauna_core::hex32::encode(successor.secret_bytes()),
            &reported,
            &registry_for(&successor, true),
            |hex| std::env::temp_dir().join(format!("never-reached-{hex}.db")),
        )
        .await;

        assert_eq!(
            outcome.kind(),
            STOLEN_KIND_UNDECIDED,
            "the outcome is genuinely unknown and must never read as nothing-moved: {outcome:?}"
        );
        let line = outcome.message().expect("the undecided arm speaks");
        assert_eq!(
            line.key, KEY_STOLEN_OUTCOME_UNKNOWN_SAVED,
            "the successor seed IS persisted here (read-back confirms it) — the sentence must \
             point at it, or the user is locked out of an account whose key they hold: {line:?}"
        );
        assert_eq!(
            line.args.get("reported").map(String::as_str),
            Some(reported.to_string().as_str()),
            "the original failure still belongs in the sentence: {line:?}"
        );
    }

    /// **The same worst cell**, with the one variable that decides
    /// which way back is real: the persist did not stick.
    ///
    /// The compound trigger is not exotic. A lost submit reply (network) plus a
    /// failed persist (ENOSPC is a recurring reality on these boxes) plus an
    /// unreachable nest at reconcile (correlated with the first) — and the
    /// earlier message told the user the identity was safe on the device and
    /// they could close the app. If the succession HAD landed, that is the
    /// account gone: the only key to it was the seed nobody printed.
    ///
    /// Both directions are asserted. The seed must appear, and the unlicensed
    /// sentence must NOT — a message carrying both would still read as "it is
    /// saved, and here is a copy", which is the reassurance the finding is about.
    #[tokio::test]
    async fn an_unverified_persist_puts_the_successor_seed_on_screen_instead_of_claiming_it_is_saved()
     {
        let successor = fauna_core::identity::ActorKeypair::generate();
        let old = fauna_core::identity::ActorKeypair::generate();
        let secret_hex = fauna_core::hex32::encode(successor.secret_bytes());
        let reported = crate::RecoveryError::Transport("rpc disconnected".to_string());

        let outcome = finish_unconfirmed_succession(
            "http://127.0.0.1:1",
            None,
            old.actor_id(),
            &secret_hex,
            &reported,
            &registry_for(&successor, false),
            |hex| std::env::temp_dir().join(format!("never-reached-{hex}.db")),
        )
        .await;

        assert_eq!(
            outcome.kind(),
            STOLEN_KIND_UNDECIDED,
            "the outcome is still genuinely unknown: {outcome:?}"
        );
        assert!(
            outcome.carries_the_only_seed(),
            "the surface must park this line"
        );
        let line = outcome.message().expect("the undecided arm speaks");
        assert_eq!(
            line.args.get("secret"),
            Some(&secret_hex),
            "with no verified persist the seed on screen is the ONLY way back into an \
             account that may already be the successor's: {line:?}"
        );
        assert_eq!(
            line.key, KEY_STOLEN_OUTCOME_UNKNOWN_UNSAVED,
            "the saved-on-this-device claim is licensed by nothing here and must not be the \
             sentence picked — a line carrying both still reads as reassurance: {line:?}"
        );
    }

    /// The successor's client is **connected** before the sweep rides it — and a
    /// nest it cannot reach is reported as a failure, never as a sweep that
    /// "ran".
    ///
    /// `NestClient::new` performs no I/O: it builds the client, and `connect()`
    /// is what authenticates and spawns the reconnect supervisor that populates
    /// the dispatcher. Skipping it does not fail fast — it fails *slowly and
    /// misleadingly*: `request_inner` takes its wait-for-reconnect path, waits
    /// the full 30 s spec deadline per group for a supervisor that was never
    /// spawned, and returns `RpcDisconnected{was_in_flight:false}`. The sweep
    /// then reports `Ran` with a per-group
    /// `Failed("transport error: rpc disconnected")` that reads like a nest-side
    /// refusal while nothing ever left the app — which is exactly how this
    /// shipped, and what made the tier_3 journey's first real group fail with no
    /// sign-in in the nest log.
    ///
    /// Nothing listens on port 1, so a connected client is the only way this
    /// reaches the `Failed` arm: with the connect removed it returns `Ran` (the
    /// engine holds no groups, so the sweep makes no request to discover the
    /// nest is dead).
    #[tokio::test]
    async fn the_sweep_connects_the_successor_before_it_rides_the_client() {
        use fauna_core::data::Timestamp;
        use fauna_core::identity::ActorKeypair;
        use fauna_core::recovery::{IdentitySuccession, RecoveryKey};

        let old_kp = ActorKeypair::from_secret([1u8; 32]);
        let successor_kp = ActorKeypair::from_secret([2u8; 32]);
        let old_engine =
            fauna_mls::engine::MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32]))
                .expect("the old engine builds in memory");

        let recovery = RecoveryKey::generate();
        let statement = IdentitySuccession {
            old_actor_id: old_kp.actor_id(),
            new_actor_id: successor_kp.actor_id(),
            recovery_pubkey: recovery.public(),
            seq: 2,
            created_at: Timestamp::now(),
        }
        .sign(
            &recovery,
            successor_kp.signing_key(),
            Some(old_kp.signing_key()),
        )
        .expect("the fixture statement signs");

        let status = sweep_as_successor(
            "http://127.0.0.1:1",
            Some(&old_engine),
            &fauna_core::hex32::encode(&[2u8; 32]),
            &statement,
            // Never reached: the connect fails first, which is the assertion.
            |hex| std::env::temp_dir().join(format!("never-reached-{hex}.db")),
        )
        .await;

        let SweepStatus::Failed(reason) = status else {
            panic!(
                "an unreachable nest must be reported as a failed sweep, not as one that ran: \
                 {status:?}"
            );
        };
        assert!(
            reason.contains("connecting as the successor"),
            "the failure must come from the successor's own connect — any other reason means \
             this test stopped exercising it: {reason}"
        );
    }

    /// The edit form's base load on a linkless successor — the registry holds
    /// only the successor (it never held the predecessor's row), and the nest
    /// still serves the predecessor-signed profile. The load must prove the
    /// link from the landed statement and RECORD it, so the save that follows
    /// reads it back from `predecessors_of` and admits the base. This is the
    /// one body behind every app's edit form (tui and linux directly, the FFI
    /// apps' `load_edit_base`, web's `loadProfileEditBase`).
    #[test]
    fn the_edit_base_load_records_the_link_its_save_then_reads() {
        use fauna_core::data::{InboxMode, Profile, Timestamp};
        use fauna_core::identity::ActorKeypair;
        use fauna_protocol::{ByteBuf, RpcError};

        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();
        let registry = registry_for(&successor, true);
        let successor_hex = successor.actor_id_hex();
        assert!(registry.predecessors_of(&successor_hex).is_empty());

        let inherited = fauna_client_profile::build_profile(
            &predecessor,
            &Profile {
                actor_id: predecessor.actor_id(),
                display_name: Some("Ada".into()),
                bio: Some("counts things".into()),
                avatar: None,
                banner: None,
                links: vec![],
                nests: vec![],
                admin_nests: vec![],
                load_hint: None,
                inbox_mode: InboxMode::ContactsOnly,
                recovery_head: None,
                updated_at: Timestamp(0),
            },
        )
        .expect("the predecessor's own publish");
        // The premise: without the link, the save refuses the inherited base.
        assert!(
            fauna_client_profile::build_edited_profile(
                &successor,
                Some(&inherited),
                &[],
                Some("Ada".into()),
                None,
                vec![],
            )
            .is_err(),
            "a base signed by an identity this registry cannot place must be refused"
        );

        let recovery = fauna_core::recovery::RecoveryKey::generate();
        let statement = fauna_core::recovery::IdentitySuccession {
            old_actor_id: predecessor.actor_id(),
            new_actor_id: successor.actor_id(),
            recovery_pubkey: recovery.public(),
            seq: 2,
            created_at: Timestamp(0),
        }
        .sign(&recovery, successor.signing_key(), None)
        .expect("sign the succession");
        let nest = std::sync::Arc::new(
            fauna_client_testkit::RejectingRequester::new()
                .reply(
                    "fauna.profile.get",
                    &fauna_protocol::profile::ProfileGetReply {
                        body: ByteBuf::from(inherited.clone()),
                        extra: Default::default(),
                    },
                )
                .reply(
                    RpcError::SUCCESSION_LOOKUP_KIND,
                    &fauna_protocol::recovery::SuccessionLookupReply {
                        statements: vec![ByteBuf::from(
                            fauna_core::encoding::canonical_encode(&statement)
                                .expect("encode statement"),
                        )],
                        extra: Default::default(),
                    },
                ),
        );

        let base = fauna_client_testkit::block_on(load_profile_edit_base(
            std::sync::Arc::clone(&nest),
            &registry,
            &successor,
        ))
        .expect("the base reads");
        assert_eq!(base.as_deref(), Some(inherited.as_slice()));
        assert_eq!(
            registry.predecessors_of(&successor_hex),
            vec![predecessor.actor_id_hex()],
            "the proven link is recorded before the form can save"
        );

        // The save, exactly as every app's writes it: the predecessors come from
        // the registry, not from the load's return value.
        let recorded =
            fauna_client_profile::predecessors_from_hex(&registry.predecessors_of(&successor_hex));
        fauna_client_profile::build_edited_profile(
            &successor,
            base.as_deref(),
            &recorded,
            Some("Ada".into()),
            Some("still counts things".into()),
            vec![],
        )
        .expect("the recorded link admits the inherited base");
    }

    /// The other two answers of the edit-base load: a never-published profile
    /// is a first publish (`Ok(None)`, no lookup, nothing recorded), and a read
    /// that fails is an error, never an empty base the form would overwrite.
    #[test]
    fn the_edit_base_load_answers_first_publish_and_a_failed_read() {
        use fauna_core::identity::ActorKeypair;
        use fauna_protocol::RpcError;

        let successor = ActorKeypair::generate();
        let registry = registry_for(&successor, true);

        let unpublished =
            std::sync::Arc::new(fauna_client_testkit::RejectingRequester::new().reject(
                "fauna.profile.get",
                RpcError::new(RpcError::CODE_PROFILE_NOT_FOUND, "error.test"),
            ));
        assert_eq!(
            fauna_client_testkit::block_on(load_profile_edit_base(
                std::sync::Arc::clone(&unpublished),
                &registry,
                &successor,
            ))
            .expect("not-found is a first publish, not an error"),
            None
        );
        assert_eq!(unpublished.kinds(), vec!["fauna.profile.get"]);
        assert!(
            registry
                .predecessors_of(&successor.actor_id_hex())
                .is_empty()
        );

        // Unmapped kinds are transport faults in this double.
        let offline = fauna_client_testkit::RejectingRequester::new();
        assert!(
            fauna_client_testkit::block_on(load_profile_edit_base(&offline, &registry, &successor))
                .is_err()
        );
    }
}

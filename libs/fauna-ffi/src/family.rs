//! UniFFI façade for the family-safety WS-RPC kinds (`fauna.family.*` —
//! `docs/goal/behavior/family-safety.md` § Wire & data shape) — the Family
//! surface + supervised indicator the four native apps drive.
//!
//! [`FfiFamilyClient`] wraps `fauna_client_family::FamilyClient`; the mirror
//! records below are the FFI-visible shape of `fauna_protocol::family::*`.
//! Linux calls the same `FamilyClient` directly; the web SPA reaches it
//! through the wasm twins (`libs/fauna-wasm/src/rpc.rs`). One shared client,
//! exposed once at each boundary (priority #2).
//!
//! Protocol → FFI conversions are exhaustive `From` impls, so a new wire
//! field the UI should see is a compile error here (mirror-drift guard). The
//! wire `extra` catch-alls are intentionally dropped.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_family::FamilyClient;
use fauna_client_family::family::{
    FamilyApprovalEntry, FamilyContentNotice, FamilyGuardianInfo, FamilyIncomingTransferInfo,
    FamilyPendingTransferInfo, FamilyStatusReply, FamilyWardInfo, ReachPolicy,
};
#[cfg(feature = "value-format")]
use fauna_core::format::{PolicySummaryLine, ReachPolicyOption};
use fauna_core::obligation::{ContentFloor, ContentPolicy};
use fauna_core::screen_time::ScreenTimePolicy;

use crate::{FfiError, stringify};

// ── The account age band — store-signal fold (client-consumption) ───────

/// `fauna_protocol::age::AgeBand::from_age_range` — the one fold both mobile
/// store age signals feed (Play Age Signals' `ageLower`/`ageUpper`, iOS
/// Declared Age Range's bounds) into the account age band's wire token
/// (`family-safety.md` § The account age band, D3). `None` when the store
/// shared no bound. The band token is what `OnboardingMachine::set_age_claim`
/// takes.
#[uniffi::export]
pub fn age_band_from_age_range(lower: Option<u32>, upper: Option<u32>) -> Option<String> {
    fauna_protocol::age::AgeBand::from_age_range(lower, upper).map(|b| b.as_str().to_string())
}

/// `fauna_protocol::age::FAUNA_IOS_APP_ID` — the team-prefixed App ID the iOS
/// store-age arm passes as `age_claim_digest`'s `application_id`, and nothing
/// more: whether an attestation is worth attaching is the addressed nest's
/// `attestation_platforms` list (`AgeNoncePlain`), never this build's arming
/// (`family-safety.md` § The account age band → *An attestation the nest
/// cannot check*). `None` only means the signed message cannot be formed.
#[uniffi::export]
pub fn ios_age_attestation_app_id() -> Option<String> {
    fauna_protocol::age::FAUNA_IOS_APP_ID.map(str::to_string)
}

// ── Reach-policy option catalogs (client-consumption) ──────────────────
//
// Thin re-exports of `fauna_core::format`'s reach-policy catalog/label/summary
// functions (`family-safety.md` § Where logic lives) — the single source of
// the `unknown_sender_mail` / `feed_sources` picker option sets, their
// fail-closed label resolution, and the four-line read-only policy summary.
// Mirrors `folders.rs`'s `conflict_policy_options` pattern: shared Rust owns
// the option *set* and the fail-closed rule, the client owns the widget.
//
// Gated behind `value-format` (default-on, dropped from the Go mail-bridge
// `--no-default-features` build): a `#[uniffi::export]` returning a
// `fauna_core` type (`LocalizedText`/`ReachPolicyOption`/`PolicySummaryLine`
// here) makes uniffi-bindgen-go emit an uncompilable bare `import
// "fauna_core"` — the value-format footgun (see `provisioning.rs`'s
// `provisioning_elapsed`). The mail-bridge has no use for client-picker
// label strings; `FfiFamilyClient`/`FfiReachPolicy` stay ungated for its
// actual reach-policy read.

/// The canonical `unknown_sender_mail` picker options (wire value + i18n label
/// key), in the ratified order.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn unknown_sender_options() -> Vec<ReachPolicyOption> {
    fauna_core::format::unknown_sender_options()
}

/// The canonical `feed_sources` picker options, in the ratified order.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn feed_sources_options() -> Vec<ReachPolicyOption> {
    fauna_core::format::feed_sources_options()
}

/// The localized label for a stored `unknown_sender_mail` wire value, failing
/// closed to `hold` for anything unrecognized — never the permissive `allow`.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn unknown_sender_label(value: String) -> fauna_core::localized::LocalizedText {
    fauna_core::format::unknown_sender_label(&value)
}

/// The localized label for a stored `feed_sources` wire value, failing closed
/// to `block` for anything unrecognized.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn feed_sources_label(value: String) -> fauna_core::localized::LocalizedText {
    fauna_core::format::feed_sources_label(&value)
}

/// The canonical `unknown_peer_dm` picker options, in the ratified order.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn unknown_peer_dm_options() -> Vec<ReachPolicyOption> {
    fauna_core::format::unknown_peer_dm_options()
}

/// The localized label for a stored `unknown_peer_dm` wire value, failing closed
/// to `hold` for anything unrecognized — never the permissive `allow`.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn unknown_peer_dm_label(value: String) -> fauna_core::localized::LocalizedText {
    fauna_core::format::unknown_peer_dm_label(&value)
}

/// The five-line read-only reach-policy summary the supervised side sees
/// (`family-policy-summary`), in the ratified display order. All three string
/// knobs fail closed via [`unknown_sender_label`] / [`feed_sources_label`] /
/// [`unknown_peer_dm_label`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn reach_policy_summary(policy: FfiReachPolicy) -> Vec<PolicySummaryLine> {
    let policy: ReachPolicy = policy.into();
    policy.summary_lines()
}

// ── Content-policy faces (v1.x content pillar) ─────────────────────────
//
// The native twins of the web `contentFloorOptions` / `contentFloorLabel` /
// `contentRenderVerdict` wasm exports and linux's `crate::content_policy` module
// (`family-safety.md` § Content policy). The guardian editor's four per-category
// content selects (`family-policy-content-*-select`) render one shared option
// catalog; the feed + conversations render paths call `content_render_verdict` to
// collapse/block a labeled item. All `value-format`-gated for the same
// uniffi-bindgen-go reason as the reach-policy catalogs above (they cross a
// `fauna_core` type / would drag `ContentLabelEntry` into the Go build). Rule
// assembly stays entirely in shared Rust (priority #2) — no client re-derives the
// strictest-wins compose.

/// The canonical content-floor picker options (`inherit / collapse / block`, in
/// the ratified order), wire value + i18n label key. The guardian's four
/// per-category content selects render this one catalog, so their value list
/// cannot drift from what `fauna.family.policy.update` accepts. Mirrors
/// [`unknown_sender_options`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn content_floor_options() -> Vec<ReachPolicyOption> {
    fauna_core::format::content_floor_options()
}

/// The localized label for a stored content-floor wire value, failing closed to
/// `block` (this knob's strict option) for anything unrecognized — never the
/// permissive `inherit`. See [`unknown_sender_label`] for the shared safety rule.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn content_floor_label(value: String) -> fauna_core::localized::LocalizedText {
    fauna_core::format::content_floor_label(&value)
}

/// The client render verdict for one piece of content — the native twin of the
/// web `contentRenderVerdict` / linux `content_policy::verdict_for`
/// (`family-safety.md` § Content policy). Composes, **strictest-wins**, the
/// viewer's OWN spam/phishing thresholds (→ `collapse`, the every-user un-darking
/// of moderation.md § item 1) with, when supervised, the guardian's per-category
/// floor (`collapse`/`block`), and returns one of
/// `"show" | "badge" | "collapse" | "block"`.
///
/// Rule assembly stays entirely in shared Rust (priority #2) — this is a thin
/// marshalling shell over `fauna_core::obligation::render_verdict_composed`,
/// which every app's render path routes through. The caller passes
/// only the ingredients it already holds: `labels` (the reduced
/// `{ category, confidence_per_mille }` shape already on the feed post /
/// message snapshots), the guardian `content_policy` straight off `familyStatus`
/// (`None` for an unsupervised viewer — an unparseable floor stays fail-closed to
/// `block` via [`ContentFloor::from_wire`]), and the viewer's own per-mille
/// thresholds (both `None` until the client has spam preferences → no own-threshold
/// rule composes; a half-known pair is likewise no rule, the shared
/// `ViewerThresholds` pairing).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn content_render_verdict(
    labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    content_policy: Option<FfiContentPolicy>,
    own_spam_permille: Option<u16>,
    own_phishing_permille: Option<u16>,
) -> String {
    content_render_composed(
        labels,
        content_policy,
        own_spam_permille,
        own_phishing_permille,
        Vec::new(),
    )
    .verdict
}

/// The full render decision — the verdict **and** the source that drove it —
/// composing all THREE strictest-wins sources, the region content policy
/// included (`region-blocking.md` § Where it composes — the render seam).
///
/// A separate face rather than a fifth argument and a widened return on
/// [`content_render_verdict`], for two reasons that point the same way: this
/// uniffi version supports `#[uniffi(default = …)]` only on **record fields**,
/// not on exported-function parameters, so a fifth argument could not be
/// defaulted and would break the Kotlin, Swift, C# and Go call sites at once;
/// and a blocked item needs the attribution while an ordinary render needs only
/// the verb. An app moves from [`content_render_verdict`] to this one when it
/// gains its region render — the same swap on every app, so the two faces never
/// become a per-app divergence.
///
/// `region_policies` is every region on the device's declared ancestor chain,
/// **most specific first**, as
/// `fauna_core::region_authority::RegionRegistry::chain` orders them and
/// `rules_from_region_policy` assembles them. Empty for a device whose regions
/// enrol nobody, which is every device today.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn content_render_composed(
    labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    content_policy: Option<FfiContentPolicy>,
    own_spam_permille: Option<u16>,
    own_phishing_permille: Option<u16>,
    region_policies: Vec<FfiRegionRuleSet>,
) -> FfiComposedVerdict {
    use fauna_core::obligation::{ViewerThresholds, render_verdict_composed};
    let policy = render_guardian_policy(content_policy);
    let own = own_spam_permille
        .zip(own_phishing_permille)
        .map(|(s, p)| ViewerThresholds {
            spam_permille: s,
            phishing_permille: p,
        });
    let regions: Vec<fauna_core::region_policy::RegionRuleSet> = region_policies
        .into_iter()
        .filter_map(FfiRegionRuleSet::into_shared)
        .collect();
    ffi_composed(&render_verdict_composed(
        &labels,
        policy.as_ref(),
        own,
        &regions,
    ))
}

/// [`content_render_composed`] for one identified item, honouring the viewer's
/// own reports (`moderation.md` § Corollary — block also hides): an item the
/// viewer reported, or whose author they reported, comes back `block` with
/// `reported` set. `hidden_content` is the viewer's
/// the `fauna.state.moderation` record's hidden-content list ([`crate::muted_keywords::load_hidden_content`]);
/// `item_id` is the post's or message's id, `author_id` its author's actor id.
/// A new face, not new parameters on the old one, for the reason
/// [`content_render_composed`] gives; an app moves to it when it paints the
/// reported placeholder.
#[cfg(feature = "value-format")]
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn content_render_for_item(
    hidden_content: Vec<String>,
    item_id: String,
    author_id: Option<String>,
    labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    content_policy: Option<FfiContentPolicy>,
    own_spam_permille: Option<u16>,
    own_phishing_permille: Option<u16>,
    region_policies: Vec<FfiRegionRuleSet>,
) -> FfiComposedVerdict {
    use fauna_core::obligation::{ViewerThresholds, render_verdict_for_item};
    let policy = render_guardian_policy(content_policy);
    let own = own_spam_permille
        .zip(own_phishing_permille)
        .map(|(s, p)| ViewerThresholds {
            spam_permille: s,
            phishing_permille: p,
        });
    let regions: Vec<fauna_core::region_policy::RegionRuleSet> = region_policies
        .into_iter()
        .filter_map(FfiRegionRuleSet::into_shared)
        .collect();
    ffi_composed(&render_verdict_for_item(
        &hidden_content,
        &item_id,
        author_id.as_deref(),
        &labels,
        policy.as_ref(),
        own,
        &regions,
    ))
}

/// The compiled-in content floor THIS build carries: the kids flavor's
/// four-categories-at-`block` floor under the `kids-floor` feature
/// (`family-safety.md` § The account age band → the kids-app bullet, item (4)),
/// none in every other build.
#[cfg(all(feature = "value-format", feature = "kids-floor"))]
const COMPILED_CONTENT_FLOOR: Option<&ContentPolicy> =
    Some(&fauna_core::obligation::KIDS_CONTENT_FLOOR);
#[cfg(all(feature = "value-format", not(feature = "kids-floor")))]
const COMPILED_CONTENT_FLOOR: Option<&ContentPolicy> = None;

/// The guardian policy every FFI render face composes: the caller's guardian
/// `content_policy` with this build's [`COMPILED_CONTENT_FLOOR`] applied
/// strictest-wins (`fauna_core::obligation::with_compiled_floor`). The one
/// place a face resolves its guardian input, so no face can render past the
/// kids floor. Guardian Notify ([`guardian_enforced_categories`]) deliberately
/// does NOT come through here — `content_notify` is not floored.
#[cfg(feature = "value-format")]
pub(crate) fn render_guardian_policy(
    content_policy: Option<FfiContentPolicy>,
) -> Option<ContentPolicy> {
    let policy: Option<ContentPolicy> = content_policy.map(Into::into);
    fauna_core::obligation::with_compiled_floor(policy.as_ref(), COMPILED_CONTENT_FLOOR)
}

/// May this account use the kids app? The native face of the shared
/// `fauna_client_family::kids_app_eligible` verdict (`family-safety.md` § The
/// account age band → the kids-app bullet, item (3)): `true` exactly when the
/// `familyStatus` read names a guardian. The kids flavor keys sign-in on it; a
/// graduated account flips it on its next status read. The web twin is the
/// `kidsAppEligible` field wasm's `familyStatus` attaches to its reply.
#[uniffi::export]
pub fn kids_app_eligible(status: FfiFamilyStatus) -> bool {
    // The verdict reads `supervised_by` alone; the wire reply is rebuilt from
    // exactly that field so the rule itself stays in the shared crate.
    fauna_client_family::kids_app_eligible(&FamilyStatusReply {
        supervised_by: status.supervised_by.map(|g| FamilyGuardianInfo {
            actor_id: g.actor_id.into(),
            handle: g.handle,
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[cfg(feature = "value-format")]
fn ffi_composed(composed: &fauna_core::obligation::ComposedVerdict) -> FfiComposedVerdict {
    FfiComposedVerdict {
        verdict: composed.verdict.as_str().to_string(),
        region: composed.region().map(|a| FfiRegionAttribution {
            region: a.region.as_str().to_string(),
            authority_name: a.authority_name.clone(),
            reason_code: a.reason_code.clone(),
            reason: a.reason.clone().into_iter().collect(),
        }),
        reported: composed.reported(),
    }
}

// ── Guardian Notify faces (v1.x content pillar) ────────────────────────
//
// The native twins of web's `guardianEnforcedCategories`/`contentNoticeLine`
// wasm exports and linux's `content_policy::NotifyAccumulator` +
// `note_enforcement` (`family-safety.md` § Guardian Notify): *which*
// categories a rendered item's labels trip against the guardian's content
// floor, and the guardian's per-ward readout line for one `(category,
// count)` notice. Rule assembly stays entirely in shared Rust (priority
// #2) so no client's Notify accumulator can drift from what
// `content_render_verdict` renders. `value-format`-gated for the same
// uniffi-bindgen-go reason as the content-policy faces above.

/// The guardian-floor categories a piece of content triggers, for **Guardian
/// Notify** counting — the native twin of the web `guardianEnforcedCategories`
/// wasm export and linux's direct call into
/// [`fauna_core::obligation::guardian_enforced_categories`]. Same reduced
/// `labels` shape as [`content_render_verdict`]; `content_policy` is the
/// guardian's policy straight off `familyStatus` — `None` for an
/// unsupervised viewer, which returns an empty result (mirrors the wasm
/// face's null-policy short-circuit; the ward's own-threshold collapses are
/// deliberately never inputs here — Notify is a lens on the *guardian's*
/// policy, not the ward's own choices).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn guardian_enforced_categories(
    labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    content_policy: Option<FfiContentPolicy>,
) -> Vec<String> {
    let Some(content_policy) = content_policy else {
        return Vec::new();
    };
    let policy: ContentPolicy = content_policy.into();
    fauna_core::obligation::guardian_enforced_categories(&labels, &policy)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The guardian's per-ward **Guardian Notify** readout line
/// (`family-ward-content-notices`) for one `(category, count)` notice off a
/// ward's `content_notices` — the native twin of the web `contentNoticeLine`
/// wasm export (linux calls [`fauna_core::format::content_notice_line`]
/// directly). Category + count only — never content, never an id.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn content_notice_line(category: String, count: u32) -> PolicySummaryLine {
    fauna_core::format::content_notice_line(&category, count)
}

/// `fauna_core::obligation::NOTIFY_REPORT_MIN_INTERVAL_SECS` → the ≤hourly
/// **Guardian Notify** batch interval, so a native app's flush accumulator
/// gates on the same cadence as web/linux (priority #2 — one shared constant,
/// no per-app drift). `u32`, matching the web `notifyReportMinIntervalSecs`
/// face's return shape. No `value-format` gate needed — a plain integer
/// crosses no `fauna_core` type.
#[uniffi::export]
pub fn notify_report_min_interval_secs() -> u32 {
    fauna_core::obligation::NOTIFY_REPORT_MIN_INTERVAL_SECS as u32
}

/// One drained **Guardian Notify** batch — [`FfiNotifyAccumulator::take_due`]'s
/// `Some` case. `entries` maps 1:1 onto `fauna.family.notify_report`'s wire
/// list; `offset_minutes` is the device offset recorded at the last
/// [`FfiNotifyAccumulator::record`] call, which the RPC needs alongside it
/// (the § Screen time day-bucket rule this pillar shares).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiNotifyDue {
    pub entries: Vec<FfiFamilyContentNotice>,
    pub offset_minutes: i32,
}

/// Ward-side **Guardian Notify** counter (`family-safety.md` § Guardian
/// Notify) — a UniFFI object wrapping [`fauna_core::obligation::NotifyAccumulator`]
/// directly, so a native app hand-rolls NEITHER the dedup/day-bucket/batching
/// state machine NOR the "which categories count" rule (priority #2): windows
/// and android each built their own accumulator around the three free
/// functions above (`guardian_enforced_categories`/`content_notice_line`/
/// `notify_report_min_interval_secs`) BEFORE this type existed in shared Rust
/// (2026-08-11, once tui and linux's independent private copies were lifted
/// here) — this is the first native leg built after that lift, so it uses the
/// whole state machine instead of re-deriving it a third time.
///
/// **Interior `Mutex`, not `&mut self`:** the same shape as
/// [`FfiUsageHeartbeat`] — a UniFFI object is shared across the binding's
/// threads and this is stateful. A poisoned lock is recovered rather than
/// surfaced: the worst case is one render event's count, and refusing to
/// count the rest of the session is strictly worse.
///
/// `content_policy` is a per-[`record`](Self::record) PARAMETER, not object
/// state — the app's own `ContentPolicyStore`-equivalent cache already holds
/// it (apple's `ContentPolicyInputs`), and a second copy here would be a
/// second cache to keep in sync. `enabled` (the `content_notify` knob) IS
/// object state via [`set_enabled`](Self::set_enabled): `NotifyAccumulator`
/// drops pending counts the moment it turns off, and that must fire at the
/// SAME trigger as the policy refresh (post-auth + reconnect), not lazily at
/// the next render.
#[cfg(feature = "value-format")]
#[derive(uniffi::Object)]
pub struct FfiNotifyAccumulator {
    inner: std::sync::Mutex<fauna_core::obligation::NotifyAccumulator>,
}

#[cfg(feature = "value-format")]
#[uniffi::export]
impl FfiNotifyAccumulator {
    /// A fresh, disabled accumulator — construct once per session, alongside
    /// the ward's family wiring.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(fauna_core::obligation::NotifyAccumulator::default()),
        })
    }

    /// Set whether the guardian's `content_notify` knob is on — call on every
    /// `fauna.family.status` read (the same post-auth + reconnect trigger as
    /// the content-policy cache refresh). Turning it off drops any pending
    /// (not-yet-flushed) counts: they were accrued under consent just
    /// withdrawn, so they must not arrive after the fact.
    pub fn set_enabled(&self, on: bool) {
        self.lock().set_enabled(on);
    }

    /// Record any **guardian-floor** render-enforcement on `item_id` for the
    /// given `labels` under the guardian's `content_policy`. A no-op unless
    /// Notify is on ([`set_enabled`](Self::set_enabled)) AND the guardian
    /// floor bites one of `labels` — never the ward's own-threshold collapses
    /// (`content_policy: None`, an unsupervised viewer, short-circuits to a
    /// no-op the same way the `guardian_enforced_categories` free function
    /// does). Deduped per item per local day, so a re-render never re-counts.
    pub fn record(
        &self,
        item_id: String,
        labels: Vec<fauna_core::content_category::ContentLabelEntry>,
        content_policy: Option<FfiContentPolicy>,
        now_secs: i64,
        offset_minutes: i32,
    ) {
        let Some(content_policy) = content_policy else {
            return;
        };
        let policy: ContentPolicy = content_policy.into();
        let cats = fauna_core::obligation::guardian_enforced_categories(&labels, &policy);
        if cats.is_empty() {
            return;
        }
        self.lock()
            .record(&item_id, &cats, now_secs, offset_minutes);
    }

    /// Drain the batched report if a flush is due (≤ hourly; the first report
    /// is eager) — `None` means nothing to send. The caller reports the
    /// result via `fauna.family.notify_report`; best-effort, fire-and-forget
    /// (a modified client under-reports, never over-reports — Notify's trust
    /// bound), so there is no `report_failed` counterpart to
    /// [`FfiUsageHeartbeat`]'s: a failed send just waits for the next due
    /// check to re-batch anything recorded meanwhile.
    pub fn take_due(&self, now_secs: i64) -> Option<FfiNotifyDue> {
        let (entries, offset_minutes) = self.lock().take_due(now_secs)?;
        Some(FfiNotifyDue {
            entries: entries
                .into_iter()
                .map(|(category, count)| FfiFamilyContentNotice {
                    category: category.to_string(),
                    count,
                })
                .collect(),
            offset_minutes,
        })
    }

    /// Drop everything on an identity change — sign-out, account switch,
    /// factory reset. Pending counts are dropped rather than flushed: they
    /// were accrued under the outgoing actor, and Guardian Notify is
    /// explicitly coarse/best-effort, so losing a partial bucket at a switch
    /// is within its contract — attributing it to the wrong actor would not
    /// be.
    pub fn reset(&self) {
        *self.lock() = fauna_core::obligation::NotifyAccumulator::default();
    }
}

#[cfg(feature = "value-format")]
impl FfiNotifyAccumulator {
    /// Recover a poisoned lock rather than surfacing it, mirroring
    /// [`FfiUsageHeartbeat`]: a panic mid-record can cost at most one event's
    /// bookkeeping, and refusing to count the rest of the session is
    /// strictly worse than resuming.
    fn lock(&self) -> std::sync::MutexGuard<'_, fauna_core::obligation::NotifyAccumulator> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `fauna_core::format::approval_display_text` → what a `family-approval-item`
/// row should display verbatim, or `None` when the caller should render its
/// own localized no-sender placeholder (`family.approval_no_sender`). A plain
/// `Option<String>`, not `LocalizedText` — no `value-format` gate needed.
#[uniffi::export]
pub fn approval_display_text(entry: FfiFamilyApprovalEntry) -> Option<String> {
    fauna_core::format::approval_display_text(
        &entry.kind,
        &entry.peer_address,
        &entry.peer_handle,
        &entry.summary,
        &entry.bridge_id,
        &entry.operation,
        &entry.target,
    )
    .map(std::borrow::Cow::into_owned)
}

// ── Screen-time faces (v1.x screen-time pillar, Slice E) ───────────────
//
// The native twins of the web `screenLockMessage` / `usageTodayLine` /
// `parseTimeOfDay` / `formatTimeOfDay` / `parseDailyMinutes` / `UsageHeartbeat`
// wasm exports (`libs/fauna-wasm/src/lib.rs`) and linux/tui's `screen_lock`
// modules, which call `fauna_core::screen_time` directly with no FFI hop
// (`family-safety.md` § Screen time).
//
// **Nothing here decides anything.** Every rule that governs a child's screen
// time — whether to lock and why, what the lock says, how a typed `"21:00"`
// becomes a stored minute count, what counts as use, when to flush a
// heartbeat, what a failed report owes, and how the nest's cross-device total
// combines with minutes this device has not sent yet — lives in
// `fauna_core::screen_time` behind these thin faces. That is the goal doc's own
// split (§ Where logic lives: *"Write validation. Shared Rust —
// `ScreenTimePolicy::validate` … so clients pre-validate identically to the
// nest"*), and it is what keeps two apps from handing one child two different
// bedtimes. A native app that re-implements any of this in Swift/Kotlin/C#
// has introduced a divergence, not a shortcut (priority #2).
//
// All `value-format`-gated. The two that return a `fauna_core` type
// (`LocalizedText` / `PolicySummaryLine`) **must** be, for the
// uniffi-bindgen-go footgun the reach catalogs above document; the parsers and
// the heartbeat cross only primitives and local records, so they ride the same
// gate by choice — a Go mail-bridge with no guardian editor and no ward screen
// has no use for a `"HH:MM"` parser, and sharing one gate keeps this whole
// pillar at zero Go binding churn.

/// `fauna_core::screen_time::screen_lock_message` → the ward's full-screen
/// `screen-time-lock` decision **and** its `screen-time-lock-message`, in one
/// call: `None` = render no lock, otherwise the [`LocalizedText`] the client
/// resolves through its own i18n pipeline.
///
/// The gating decision deliberately does not cross the boundary as data — a
/// native ward client asks this one question exactly as web and linux do, so
/// the legs cannot drift on *when* a child is locked out. Pass the ward's
/// `screen_time` policy off `family_status`; `None` (absent) is the
/// unsupervised-equivalent default and never locks. `now_local_minutes` is
/// minutes since the device's local midnight; `used_today_minutes` is the day's
/// cross-device total (from [`FfiUsageHeartbeat::used_today_minutes`], which
/// adds this device's unreported minutes), or `None` when no total has been
/// heard yet — the ratified fail-open on the budget arm.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn screen_lock_message(
    policy: Option<FfiScreenTimePolicy>,
    now_local_minutes: u16,
    used_today_minutes: Option<u32>,
    guardian_handle: String,
) -> Option<fauna_core::localized::LocalizedText> {
    let policy: ScreenTimePolicy = policy.map(Into::into).unwrap_or_default();
    fauna_core::screen_time::screen_lock_message(
        &policy,
        now_local_minutes,
        used_today_minutes,
        &guardian_handle,
    )
}

/// `fauna_core::format::usage_today_line` → the screen-time usage readout, as a
/// [`PolicySummaryLine`] the caller lays out itself (structured, never
/// pre-joined — the same rule as [`reach_policy_summary`]).
///
/// **One call serves both readouts** — the guardian's per-ward line and the
/// ward's own copy — which is what makes the goal doc's transparency rule
/// structural rather than a convention two surfaces could drift apart on.
/// `budget_minutes` `None` renders the no-budget wording.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn usage_today_line(used_minutes: u32, budget_minutes: Option<u16>) -> PolicySummaryLine {
    fauna_core::format::usage_today_line(used_minutes, budget_minutes)
}

/// `fauna_core::screen_time::parse_time_of_day` → a guardian-typed `"HH:MM"`
/// window bound as minutes from local midnight, for the two
/// `family-policy-screen-window-*-input`s. `Ok(None)` for an empty input (how a
/// guardian clears the window).
///
/// The storage unit is minutes but no guardian would ever type `1260` for 9pm,
/// so all 7 apps need this conversion — and a per-app parser is a divergence
/// waiting to disagree about `"9:5"`, `"24:00"` or `"08:60"`. The `Err` is the
/// shared engine's own static reason string, carried in
/// [`FfiError::General`] for the caller to surface on its `error-message`
/// (web hands the identical string to `JsValue::from_str`; linux and tui
/// propagate it with `?`).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn parse_time_of_day(input: String) -> Result<Option<u16>, FfiError> {
    fauna_core::screen_time::parse_time_of_day(&input).map_err(|msg| FfiError::General {
        msg: msg.to_string(),
    })
}

/// `fauna_core::screen_time::format_time_of_day` → minutes from local midnight
/// back as `"HH:MM"`, to fill a guardian's editor from the stored policy. The
/// inverse of [`parse_time_of_day`]; total (values at or beyond a day wrap).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn format_time_of_day(minutes_from_midnight: u16) -> String {
    fauna_core::screen_time::format_time_of_day(minutes_from_midnight)
}

/// `fauna_core::screen_time::parse_daily_minutes` → a guardian-typed daily
/// budget in whole minutes, for `family-policy-screen-daily-input`. `Ok(None)`
/// for an empty input (no budget); `0` is a real, accepted value — the
/// deliberate full lock, which is the disambiguation the empty-window refusal
/// points guardians at. See [`parse_time_of_day`] for the `Err` shape.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn parse_daily_minutes(input: String) -> Result<Option<u16>, FfiError> {
    fauna_core::screen_time::parse_daily_minutes(&input).map_err(|msg| FfiError::General {
        msg: msg.to_string(),
    })
}

/// `fauna_core::screen_time::UsageHeartbeat` → the ward client's screen-time
/// heartbeat, as a stateful handle the app holds for the session (the native
/// twin of web's `UsageHeartbeat` wasm class).
///
/// The client contributes only what the platform alone knows: the clock,
/// whether the app is foregrounded, and the reply to each
/// `fauna.family.usage_report`. The cadence itself is shared — drive it as:
/// [`set_policy`](Self::set_policy) on every `family_status` read,
/// [`seed_total`](Self::seed_total) from `usage_today_minutes` so the first
/// paint can evaluate the budget, [`set_active`](Self::set_active) on every
/// foreground/lock transition, then tick [`take_due`](Self::take_due) and
/// answer **every** `Some` with [`report_succeeded`](Self::report_succeeded) or
/// [`report_failed`](Self::report_failed).
///
/// ⚠ **Tick at least every 2 minutes while active.** A suspended device credits
/// at most `MAX_ACCRUAL_STEP_SECS` per step, so a slower tick under-counts —
/// that clamp is what stops a backgrounded device from billing a child for
/// hours it never spent.
///
/// **Interior `Mutex`, not `&mut self`:** a UniFFI object is shared across the
/// binding's threads and this is stateful; the same shape as
/// [`FfiCueTracker`](crate::feed_manager::FfiCueTracker). A poisoned lock is
/// recovered rather than surfaced — the worst case is one tick's bookkeeping,
/// and refusing to account for the rest of the session is strictly worse.
///
/// Epoch values cross as `i64` here, unlike web's `f64` — that wasm workaround
/// exists only because a wasm `i64` arrives in JS as a `bigint`; Swift and
/// Kotlin take `i64` natively, so this face does not inherit the trap.
#[cfg(feature = "value-format")]
#[derive(uniffi::Object)]
pub struct FfiUsageHeartbeat {
    inner: std::sync::Mutex<fauna_core::screen_time::UsageHeartbeat>,
}

#[cfg(feature = "value-format")]
#[uniffi::export]
impl FfiUsageHeartbeat {
    /// A heartbeat with no policy, no accrual and no known total — the state an
    /// unsupervised (or budget-less) account stays in forever. Construct once
    /// per session, alongside the ward's family wiring.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(fauna_core::screen_time::UsageHeartbeat::new()),
        })
    }

    /// Adopt the ward's screen-time policy from `family_status`. `None`, or a
    /// policy with no daily budget, **drops all accounting state** — a stale
    /// total left behind would keep locking a ward whose guardian just lifted
    /// the limit.
    pub fn set_policy(&self, policy: Option<FfiScreenTimePolicy>) {
        let policy: Option<ScreenTimePolicy> = policy.map(Into::into);
        self.lock().set_policy(policy.as_ref());
    }

    /// Whether a daily budget is set — i.e. whether the heartbeat should run at
    /// all. No budget, no accounting.
    pub fn is_accounting(&self) -> bool {
        self.lock().is_accounting()
    }

    /// Record whether the app is being used: `active` = foregrounded AND the
    /// screen-time lock is not showing. Lock-screen time is not use — crediting
    /// it would report minutes the child never spent.
    pub fn set_active(&self, active: bool, now_secs: i64) {
        self.lock().set_active(active, now_secs);
    }

    /// Seed the cross-device total from `family_status`'s
    /// `usage_today_minutes`, so the very first paint can evaluate the budget
    /// instead of waiting out a heartbeat.
    pub fn seed_total(&self, usage_today_minutes: Option<u32>) {
        self.lock().seed_total(usage_today_minutes);
    }

    /// The minutes to send with `fauna.family.usage_report` now, or `None` for
    /// "nothing due". `0` is a real answer — a zero-minute report is a *read*,
    /// and it is what lifts the lock at local midnight or when the guardian
    /// raises the budget. The caller MUST answer every `Some` with
    /// [`report_succeeded`](Self::report_succeeded) or
    /// [`report_failed`](Self::report_failed).
    pub fn take_due(&self, now_secs: i64) -> Option<u32> {
        self.lock().take_due(now_secs)
    }

    /// Land a `fauna.family.usage_report` reply (`day` + `day_total_minutes`).
    pub fn report_succeeded(&self, day: i64, day_total_minutes: u32, now_secs: i64) {
        self.lock()
            .report_succeeded(day, day_total_minutes, now_secs);
    }

    /// A report that never landed — its minutes go back on the pile, because
    /// the delta is defined as "since the last **successful** report".
    pub fn report_failed(&self) {
        self.lock().report_failed();
    }

    /// The figure to hand [`screen_lock_message`] and to display: the nest
    /// total plus what this device has accrued since. `None` until a total has
    /// been heard — the ratified fail-open on the budget arm.
    pub fn used_today_minutes(&self, now_secs: i64) -> Option<u32> {
        self.lock().used_today_minutes(now_secs)
    }

    /// The local-day bucket the current total belongs to, as stamped by the
    /// nest. Informational — kept so a day change is observable without a
    /// second read.
    pub fn nest_day(&self) -> Option<i64> {
        self.lock().nest_day()
    }

    /// Drop everything on an identity change — sign-out, account switch, reset.
    pub fn reset(&self) {
        self.lock().reset();
    }
}

#[cfg(feature = "value-format")]
impl FfiUsageHeartbeat {
    /// Recover a poisoned lock rather than surfacing it, mirroring
    /// [`FfiCueTracker`](crate::feed_manager::FfiCueTracker): a panic mid-tick
    /// can cost at most one tick's bookkeeping, and refusing to account for the
    /// rest of the session is strictly worse than resuming.
    fn lock(&self) -> std::sync::MutexGuard<'_, fauna_core::screen_time::UsageHeartbeat> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ── Record mirrors ─────────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::family::ReachPolicy`] — the per-ward
/// nest-enforced reach-policy document. String knobs are closed enums:
/// `unknown_sender_mail` is one of `allow` / `hold` / `reject`,
/// `feed_sources` one of `allow` / `block` (the nest rejects anything else).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReachPolicy {
    pub contact_approval: bool,
    pub unknown_sender_mail: String,
    pub federation_contact: bool,
    pub feed_sources: String,
    /// v1.x content pillar (`family-safety.md` § Content policy) — the
    /// guardian's per-category render floor. `None` when no floor is set (the
    /// unsupervised-equivalent). Client-enforced post-decrypt.
    pub content_policy: Option<FfiContentPolicy>,
    /// v1.x screen-time pillar (`family-safety.md` § Screen time) — usage window
    /// + daily budget. `None` when unset. Client-enforced.
    pub screen_time: Option<FfiScreenTimePolicy>,
    /// v1.x Guardian Notify knob (`family-safety.md` § Guardian Notify) — when
    /// on, the ward's client reports coarse per-category enforcement counts and
    /// the guardian is notified (category + count, never content). `None` =
    /// leave unchanged on an update; the default is off.
    pub content_notify: Option<bool>,
    /// v1.x bridge-DM gate knob (`family-safety.md` § The bridge-DM gate) —
    /// `allow` / `hold` for inbound DMs from external peers the ward has never
    /// corresponded with. Nest-enforced (a routing-floor knob), but a later
    /// field, so `None` = leave unchanged on an update; the default is `allow`.
    pub unknown_peer_dm: Option<String>,
}

impl From<ReachPolicy> for FfiReachPolicy {
    fn from(p: ReachPolicy) -> Self {
        // Exhaustive destructure: a new named wire field the UI should see is a
        // compile error here (the mirror-drift guard the module header promises).
        // The wire `extra` catch-all is intentionally dropped.
        let ReachPolicy {
            contact_approval,
            unknown_sender_mail,
            federation_contact,
            feed_sources,
            content_policy,
            screen_time,
            content_notify,
            unknown_peer_dm,
            // The guardian tier's controversial-class feature limits
            // (`dynamic-features.md` § Wire & data shape). Dropped here BY
            // RULE, in both directions (`family-safety.md` § Wire & data shape
            // — the sole-writer rule): the feature editor is the sub-document's
            // only writer, and the plane crosses the FFI through the shared
            // seam's own faces (`FfiFeaturesClient::ward_authored_rows` /
            // `open_editor_for_ward`), never through the reach form.
            //
            // Dropping it is safe in this direction *only* because it is safe in
            // the other one: the reverse conversion sends `None`, which the
            // update handler reads as absent-means-unchanged, so an app that
            // loads a policy, edits the reach knobs and saves it back can
            // neither clear a guardian's feature limits nor re-assert an
            // enforced deny over them. That is the whole reason this field
            // took the v1.x pillars' semantics rather than the v1 knobs'
            // replace semantics. Do not mirror it onto `FfiReachPolicy`.
            features: _,
            extra: _,
        } = p;
        FfiReachPolicy {
            contact_approval,
            unknown_sender_mail,
            federation_contact,
            feed_sources,
            content_policy: content_policy.map(Into::into),
            screen_time: screen_time.map(Into::into),
            content_notify,
            unknown_peer_dm,
        }
    }
}

impl From<FfiReachPolicy> for ReachPolicy {
    fn from(p: FfiReachPolicy) -> Self {
        let FfiReachPolicy {
            contact_approval,
            unknown_sender_mail,
            federation_contact,
            feed_sources,
            content_policy,
            screen_time,
            content_notify,
            unknown_peer_dm,
        } = p;
        ReachPolicy {
            contact_approval,
            unknown_sender_mail,
            federation_contact,
            feed_sources,
            content_policy: content_policy.map(Into::into),
            screen_time: screen_time.map(Into::into),
            content_notify,
            unknown_peer_dm,
            ..Default::default()
        }
    }
}

/// FFI mirror of [`fauna_core::obligation::ContentPolicy`] — the guardian's
/// per-category render floor (`family-safety.md` § Content policy). Each field
/// is a closed-enum string `inherit` | `collapse` | `block`, the same
/// string-knob convention as [`FfiReachPolicy`]'s `feed_sources`; a floor value
/// the client build cannot name renders fail-closed as `block`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiContentPolicy {
    pub nsfw: String,
    pub spam: String,
    pub phishing: String,
    pub commercial: String,
}

/// FFI mirror of one region's assembled rule set
/// (`fauna_core::region_policy::RegionRuleSet`) — what one region on the
/// device's declared ancestor chain contributes to a render decision
/// (`region-blocking.md` § The content plane).
///
/// The app never *builds* one of these: shared Rust assembles it from the
/// verified document (`rules_from_region_policy`), the app's region client holds
/// it, and it crosses back here to be folded.
///
/// It carries what the **fold** reads and nothing more. A document's
/// applicability status (applied / inert-version / malformed) is deliberately
/// absent: an inapplicable document assembles to *no rules*, so it contributes
/// exactly nothing here, and the status is a **transparency-surface** fact
/// rather than a render input — read through that surface's own face, where it
/// can be shown with the authority and version beside it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRegionRuleSet {
    /// The region code, e.g. `NO` or the ISO 3166-2 `NO-03`.
    pub region: String,
    /// As the curated registry names the administering authority.
    pub authority_name: String,
    /// Empty for a region that published nothing this build can apply.
    pub rules: Vec<FfiRegionRule>,
}

/// FFI mirror of `fauna_core::region_policy::RegionRule`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRegionRule {
    /// A canonical label category, or a `region:<region>/<name>` scorer factor.
    pub factor: String,
    pub min_permille: u16,
    /// A closed-enum string, `collapse` | `block` — the same string-knob
    /// convention as [`FfiContentPolicy`]'s floors.
    pub verdict: String,
    pub reason_code: String,
    /// `{ lang → text }`, with a required `default` entry. The authority's own
    /// words, shown verbatim under the app's frame.
    pub reason: std::collections::HashMap<String, String>,
}

/// FFI mirror of `fauna_core::obligation::ComposedVerdict` — the render verb
/// plus, when a region rule drove it, the authority to name in the placeholder.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiComposedVerdict {
    /// One of `show` | `badge` | `collapse` | `block`.
    pub verdict: String,
    /// `None` when no region rule drove this verdict — including every
    /// family-policy and own-threshold verdict, which the app already names in
    /// its own words.
    pub region: Option<FfiRegionAttribution>,
    /// The viewer's own report hid this item — paint the "you reported this"
    /// placeholder (`moderation.report.hidden_placeholder`). Only
    /// [`content_render_for_item`] can set it.
    #[uniffi(default = false)]
    pub reported: bool,
}

/// FFI mirror of `fauna_core::obligation::RegionAttribution`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRegionAttribution {
    pub region: String,
    pub authority_name: String,
    pub reason_code: String,
    pub reason: std::collections::HashMap<String, String>,
}

impl From<fauna_core::region_policy::RegionRuleSet> for FfiRegionRuleSet {
    fn from(set: fauna_core::region_policy::RegionRuleSet) -> Self {
        FfiRegionRuleSet {
            region: set.region.as_str().to_string(),
            authority_name: set.authority_name,
            rules: set
                .rules
                .into_iter()
                .map(|r| FfiRegionRule {
                    factor: r.rule.category,
                    min_permille: r.rule.min_confidence_permille,
                    verdict: match r.rule.action {
                        fauna_core::obligation::ObligationAction::Block => "block".into(),
                        _ => "collapse".into(),
                    },
                    reason_code: r.reason_code,
                    reason: r.reason.into_iter().collect(),
                })
                .collect(),
        }
    }
}

impl FfiRegionRuleSet {
    /// Back into the shared type the engine folds.
    ///
    /// `None` when the region code does not parse: such a code cannot name an
    /// enrolled region, so its rules could never have come from a verified
    /// artifact. Dropping the set is the safe direction — this plane only ever
    /// *restricts*, so applying nothing is harmless where inventing a region
    /// code would not be.
    ///
    /// Gated with its only caller, `content_render_composed`: a consumer that
    /// takes this crate without `value-format` (the nest, for its file-provider
    /// host face) compiles the face out and would otherwise see dead code.
    #[cfg(feature = "value-format")]
    fn into_shared(self) -> Option<fauna_core::region_policy::RegionRuleSet> {
        use fauna_core::obligation::{AttestationLevel, ObligationAction, ObligationRule};
        use fauna_core::region_policy::{PolicyStatus, RegionRule, RegionRuleSet};

        let region = fauna_core::region_authority::RegionCode::parse(self.region).ok()?;
        Some(RegionRuleSet {
            region,
            authority_name: self.authority_name,
            // Accurate rather than assumed: whatever rules crossed the boundary
            // are the ones that apply, and a set that carries none contributes
            // nothing — which is exactly what an inert document assembles to.
            status: PolicyStatus::Applied,
            rules: self
                .rules
                .into_iter()
                .map(|r| RegionRule {
                    rule: ObligationRule {
                        category: r.factor,
                        min_confidence_permille: r.min_permille,
                        // Anything but the exact `block` string is the *less*
                        // strict verb — a client too old to name a future verb
                        // must never escalate on it.
                        action: if r.verdict == "block" {
                            ObligationAction::Block
                        } else {
                            ObligationAction::Collapse
                        },
                        requires_attestation: AttestationLevel::Any,
                    },
                    reason_code: r.reason_code,
                    reason: r.reason.into_iter().collect(),
                })
                .collect(),
        })
    }
}

impl From<ContentPolicy> for FfiContentPolicy {
    fn from(p: ContentPolicy) -> Self {
        let ContentPolicy {
            nsfw,
            spam,
            phishing,
            commercial,
        } = p;
        FfiContentPolicy {
            nsfw: nsfw.as_str().to_string(),
            spam: spam.as_str().to_string(),
            phishing: phishing.as_str().to_string(),
            commercial: commercial.as_str().to_string(),
        }
    }
}

impl From<FfiContentPolicy> for ContentPolicy {
    fn from(p: FfiContentPolicy) -> Self {
        ContentPolicy {
            nsfw: ContentFloor::from_wire(&p.nsfw),
            spam: ContentFloor::from_wire(&p.spam),
            phishing: ContentFloor::from_wire(&p.phishing),
            commercial: ContentFloor::from_wire(&p.commercial),
        }
    }
}

/// FFI mirror of [`fauna_core::screen_time::ScreenTimePolicy`] — the guardian's
/// usage window + daily budget (`family-safety.md` § Screen time). Minutes from
/// local midnight; a wrapping window (`window_start > window_end`) is the
/// bedtime case. `None` on any field = that control is unset.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiScreenTimePolicy {
    pub window_start: Option<u16>,
    pub window_end: Option<u16>,
    pub daily_minutes: Option<u16>,
}

impl From<ScreenTimePolicy> for FfiScreenTimePolicy {
    fn from(p: ScreenTimePolicy) -> Self {
        let ScreenTimePolicy {
            window_start,
            window_end,
            daily_minutes,
        } = p;
        FfiScreenTimePolicy {
            window_start,
            window_end,
            daily_minutes,
        }
    }
}

impl From<FfiScreenTimePolicy> for ScreenTimePolicy {
    fn from(p: FfiScreenTimePolicy) -> Self {
        ScreenTimePolicy {
            window_start: p.window_start,
            window_end: p.window_end,
            daily_minutes: p.daily_minutes,
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyGuardianInfo`] — who
/// supervises the caller (the supervised indicator's data).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyGuardianInfo {
    pub actor_id: Vec<u8>,
    pub handle: String,
}

impl From<FamilyGuardianInfo> for FfiFamilyGuardianInfo {
    fn from(g: FamilyGuardianInfo) -> Self {
        FfiFamilyGuardianInfo {
            actor_id: g.actor_id.into_vec(),
            handle: g.handle,
        }
    }
}

/// The restorable view of `fauna_client_family::SupervisionSnapshot` — the
/// persisted last-known supervision (family-safety.md § Content policy,
/// clause 2), as the restore-at-launch call site reads it back
/// ([`crate::FfiAccountRegistry::supervision_snapshot`]). Deliberately reuses
/// the exact component mirrors a live [`FfiFamilyStatus`] carries, so an
/// app's restore path feeds the same store code its successful-read path
/// does — no restore-only shapes to drift on.
///
/// `supervised_by` is **non-optional by construction**: a slot with nothing
/// enforceable — absent, malformed, or whose last read said unsupervised —
/// surfaces as no snapshot at all (the getter returns `None`). That keeps the
/// graduation-direction refusal in shared Rust: no app leg can seed a floor
/// off a guardian-less record, because the type cannot express one.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSupervisionSnapshot {
    /// The guardian whose floor this snapshot restores (the lock names them;
    /// the supervised indicator renders the handle).
    pub supervised_by: FfiFamilyGuardianInfo,
    pub content_policy: Option<FfiContentPolicy>,
    pub content_notify: bool,
    pub screen_time: Option<FfiScreenTimePolicy>,
}

impl FfiSupervisionSnapshot {
    /// The one conversion from the shared fold to this app-facing shape. A live
    /// read ([`FfiFamilyStatus::supervision`]) and a cold-launch restore
    /// ([`crate::FfiAccountRegistry::supervision_snapshot`]) both route through
    /// it, so the two cannot disagree on what an app's stores are fed. `None`
    /// when the fold names no guardian: the graduation gate has already emptied
    /// every other field, and this type cannot carry a guardian-less record.
    pub(crate) fn from_fold(snap: fauna_client_family::SupervisionSnapshot) -> Option<Self> {
        let guardian = snap.supervised_by?;
        Some(Self {
            supervised_by: guardian.to_wire().into(),
            content_policy: snap.content_policy.map(Into::into),
            content_notify: snap.content_notify,
            screen_time: snap.screen_time.map(Into::into),
        })
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyPendingTransferInfo`] — the
/// outstanding transfer proposal for a ward, as the initiating side sees it
/// (`family-safety.md` § Graduation & transfer).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyPendingTransfer {
    pub proposed_guardian_actor_id: Vec<u8>,
    pub proposed_guardian_handle: String,
    pub created_at: i64,
}

impl From<FamilyPendingTransferInfo> for FfiFamilyPendingTransfer {
    fn from(p: FamilyPendingTransferInfo) -> Self {
        FfiFamilyPendingTransfer {
            proposed_guardian_actor_id: p.proposed_guardian_actor_id.into_vec(),
            proposed_guardian_handle: p.proposed_guardian_handle,
            created_at: p.created_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyIncomingTransferInfo`] — a
/// proposal awaiting the caller's consent as proposed guardian (the
/// incoming-transfer prompt's row).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyIncomingTransfer {
    pub supervised_actor_id: Vec<u8>,
    pub supervised_handle: String,
    /// The ward's *current* guardian.
    pub guardian_handle: String,
    pub created_at: i64,
}

impl From<FamilyIncomingTransferInfo> for FfiFamilyIncomingTransfer {
    fn from(t: FamilyIncomingTransferInfo) -> Self {
        FfiFamilyIncomingTransfer {
            supervised_actor_id: t.supervised_actor_id.into_vec(),
            supervised_handle: t.supervised_handle,
            guardian_handle: t.guardian_handle,
            created_at: t.created_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyWardInfo`] — one supervised
/// account the caller guards, with its active policy and any pending
/// transfer proposal.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyWardInfo {
    pub actor_id: Vec<u8>,
    pub handle: String,
    pub policy: FfiReachPolicy,
    pub pending_transfer: Option<FfiFamilyPendingTransfer>,
    /// v1.x Guardian Notify (`family-safety.md` § Guardian Notify) — the ward's
    /// coarse per-category enforcement counts for the current day (category +
    /// count, never content). Empty when Notify is off or nothing reported.
    pub content_notices: Vec<FfiFamilyContentNotice>,
    /// v1.x screen time (`family-safety.md` § Screen time) — the ward's
    /// cross-device foreground total for their current local day. `None` when
    /// the policy sets no daily budget (no accounting without a declared
    /// policy); `Some(0)` when a budget is set and nothing was reported yet.
    pub usage_today_minutes: Option<u32>,
    /// v1.x device marker (`family-safety.md` § Full visibility for young
    /// children / Slice F) — the ward's registered devices in the slim
    /// guardian-facing projection, so the guardian's Family page renders the
    /// per-device mark toggle (`family-device-mark-toggle`) from this one
    /// `familyStatus` read. Empty for a caller who guards no one.
    pub devices: Vec<FfiFamilyWardDevice>,
    /// The ward's established age band (`family-safety.md` § The account age
    /// band). `None` = admitted before the band existed (render nothing).
    pub age_band: Option<FfiFamilyAgeBand>,
    /// v1.x — the bridge-DM peers this guardian has DENIED for the ward
    /// (`family-safety.md` § The bridge-DM gate → *The un-deny surface*), in
    /// the order the nest sent them. Feeds `family-blocked-peer-item` /
    /// `family-blocked-peer-allow-button` inside the per-ward editor. Empty
    /// when nothing is denied.
    pub blocked_dm_peers: Vec<FfiFamilyBlockedPeer>,
}

/// FFI mirror of [`fauna_protocol::family::FamilyBlockedPeerInfo`] — one denied
/// bridge-DM peer on [`FfiFamilyWardInfo::blocked_dm_peers`].
///
/// Carries the same `(bridge_id, peer_id)` pair `approvals_decide` takes for
/// `kind: "dm_hold"` (`bridge_id` → `bridge_id`, `peer_id` → `peer_address`), so
/// the allow button addresses exactly the row the guardian read.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyBlockedPeer {
    pub bridge_id: String,
    pub peer_id: String,
}

impl From<fauna_protocol::family::FamilyBlockedPeerInfo> for FfiFamilyBlockedPeer {
    fn from(p: fauna_protocol::family::FamilyBlockedPeerInfo) -> Self {
        FfiFamilyBlockedPeer {
            bridge_id: p.bridge_id,
            peer_id: p.peer_id,
        }
    }
}

impl From<FamilyWardInfo> for FfiFamilyWardInfo {
    fn from(w: FamilyWardInfo) -> Self {
        FfiFamilyWardInfo {
            actor_id: w.actor_id.into_vec(),
            handle: w.handle,
            policy: w.policy.into(),
            pending_transfer: w.pending_transfer.map(Into::into),
            content_notices: w.content_notices.into_iter().map(Into::into).collect(),
            usage_today_minutes: w.usage_today_minutes,
            devices: w.devices.into_iter().map(Into::into).collect(),
            age_band: w.age_band.map(Into::into),
            blocked_dm_peers: w.blocked_dm_peers.into_iter().map(Into::into).collect(),
            // `features_unreadable` / `features_ceiling` are the feature
            // editor's seed and cross the FFI through its own face
            // (`FfiFeaturesClient::ward_authored_rows`), not this mirror.
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyAgeBandInfo`] — an
/// established account age band + its provenance (`family-safety.md` § The
/// account age band). Present only where a band row exists; an account with
/// no guardianship link is `18+`/`none` **by construction** and reports
/// nothing here.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyAgeBand {
    /// The band's wire token (`u13` | `13-15` | `16-17` | `18+`).
    pub band: String,
    /// How it was established (`attested-ios` | `attested-android` |
    /// `guardian-asserted` | `none`).
    pub provenance: String,
}

impl From<fauna_protocol::family::FamilyAgeBandInfo> for FfiFamilyAgeBand {
    fn from(b: fauna_protocol::family::FamilyAgeBandInfo) -> Self {
        FfiFamilyAgeBand {
            band: b.band,
            provenance: b.provenance,
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyWardDeviceInfo`] — one of a
/// ward's devices in the guardian-facing projection: the id to pass to
/// `fauna.family.device.mark`, a label to show, and the current mark state the
/// toggle renders. Not the full `SyncDevice`: a guardian marks a ward's
/// devices, it does not manage the ward's sync.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyWardDevice {
    /// Hex-encoded 32-byte device id — the `device.mark` key.
    pub device_id: String,
    pub label: String,
    pub guardian_marked: bool,
}

impl From<fauna_protocol::family::FamilyWardDeviceInfo> for FfiFamilyWardDevice {
    fn from(d: fauna_protocol::family::FamilyWardDeviceInfo) -> Self {
        FfiFamilyWardDevice {
            device_id: d.device_id,
            label: d.label,
            guardian_marked: d.guardian_marked,
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyContentNotice`] — one coarse
/// per-category enforcement count on the guardian's Family surface. `category`
/// is one of `nsfw` | `spam` | `phishing` | `commercial`; `count` carries no
/// content identifier.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyContentNotice {
    pub category: String,
    pub count: u32,
}

impl From<fauna_protocol::family::FamilyContentNotice> for FfiFamilyContentNotice {
    fn from(n: fauna_protocol::family::FamilyContentNotice) -> Self {
        FfiFamilyContentNotice {
            category: n.category,
            count: n.count,
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyStatusReply`] — both roles
/// in one read: `supervised_by` + `policy` when the caller is supervised,
/// `wards` when the caller guards someone, `incoming_transfers` when a
/// proposal awaits the caller's consent (clients widen the family-tab gate
/// on it).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyStatus {
    pub supervised_by: Option<FfiFamilyGuardianInfo>,
    pub policy: Option<FfiReachPolicy>,
    pub wards: Vec<FfiFamilyWardInfo>,
    pub incoming_transfers: Vec<FfiFamilyIncomingTransfer>,
    /// v1.x screen time — the *caller's own* cross-device foreground total for
    /// their current local day, when supervised under a daily budget (the
    /// ward's summary shows the same number the guardian sees — transparency).
    pub usage_today_minutes: Option<u32>,
    /// v1.x — the supervised caller's own pending contact asks
    /// (`family-safety.md` § Child-initiated contact requests). Empty for
    /// everyone unsupervised or with nothing pending.
    pub contact_requests: Vec<FfiFamilyContactRequest>,
    /// v1.x — the supervised caller's own feed-source asks, pending *and*
    /// approved-but-unredeemed (`family-safety.md` § Feed-source approvals).
    /// Empty for everyone unsupervised or with nothing live.
    pub feed_requests: Vec<FfiFamilyFeedRequest>,
    /// The **caller's own** established age band, when a band row exists
    /// (`family-safety.md` § The account age band — the account always sees
    /// its own band, like its policy). `None` for the common cases: no row
    /// (unsupervised = `18+`/`none` by construction) or no band row for the account.
    pub age_band: Option<FfiFamilyAgeBand>,
    /// This same reply's **supervision fold** — app-side, never on the wire
    /// (`family-client-enforcement.md` § Implementation status today). The
    /// client-enforced inputs (the content floor, `content_notify`, the
    /// screen-time policy) move from this and never from the raw
    /// [`Self::policy`], so no app re-derives the graduation gate: the shared
    /// `fauna_client_family::SupervisionSnapshot::from_status` gates every
    /// supervised field on `supervised_by`, and a reply that still carries a
    /// policy document but names no guardian yields `None` here, exactly as it
    /// persists nothing. The same shape a cold launch restores
    /// ([`crate::FfiAccountRegistry::supervision_snapshot`]), so a live read and
    /// a restore feed an app's stores one value. The web twin is the
    /// `supervision` field wasm's `familyStatus` attaches to its reply.
    pub supervision: Option<FfiSupervisionSnapshot>,
}

impl From<FamilyStatusReply> for FfiFamilyStatus {
    fn from(r: FamilyStatusReply) -> Self {
        // Folded first, while `r` is still whole.
        let supervision = FfiSupervisionSnapshot::from_fold(
            fauna_client_family::SupervisionSnapshot::from_status(&r),
        );
        FfiFamilyStatus {
            supervised_by: r.supervised_by.map(Into::into),
            policy: r.policy.map(Into::into),
            wards: r.wards.into_iter().map(Into::into).collect(),
            incoming_transfers: r.incoming_transfers.into_iter().map(Into::into).collect(),
            usage_today_minutes: r.usage_today_minutes,
            contact_requests: r.contact_requests.into_iter().map(Into::into).collect(),
            feed_requests: r.feed_requests.into_iter().map(Into::into).collect(),
            age_band: r.age_band.map(Into::into),
            supervision,
        }
    }
}

#[cfg(test)]
mod supervision_fold_tests {
    use super::*;

    fn guardian() -> FamilyGuardianInfo {
        FamilyGuardianInfo {
            actor_id: vec![0xab, 0xcd, 0x01].into(),
            handle: "parent@example.org".into(),
            ..Default::default()
        }
    }

    fn a_full_policy() -> ReachPolicy {
        ReachPolicy {
            content_policy: Some(ContentPolicy {
                nsfw: ContentFloor::Block,
                ..Default::default()
            }),
            content_notify: Some(true),
            screen_time: Some(ScreenTimePolicy {
                window_start: Some(1260),
                window_end: Some(420),
                daily_minutes: Some(90),
            }),
            ..Default::default()
        }
    }

    /// An app names an operation by picking a member, and gets the wire string
    /// the nest's grant is scoped to — the round trip through the shared parser
    /// proves the FFI enum and `FeedSourceOperation` cannot drift apart.
    #[test]
    fn a_feed_source_operation_spells_the_shared_wire_value() {
        for (op, wire) in [
            (FfiFeedSourceOperation::Link, "link"),
            (FfiFeedSourceOperation::Follow, "follow"),
            (FfiFeedSourceOperation::Feed, "feed"),
        ] {
            assert_eq!(feed_source_operation_wire(op), wire);
            assert_eq!(
                fauna_core::data::FeedSourceOperation::from_wire(wire),
                Some(fauna_core::data::FeedSourceOperation::from(op)),
            );
        }
    }

    /// The un-deny surface's data (`family-safety.md` § The bridge-DM gate →
    /// *The un-deny surface*): each denied peer keeps its OWN `(bridge_id,
    /// peer_id)` pair and the order the nest sent, because the allow button
    /// addresses its row's pair and nothing else.
    #[test]
    fn a_wards_denied_dm_peers_cross_the_boundary_pair_for_pair() {
        let ward = FfiFamilyWardInfo::from(FamilyWardInfo {
            blocked_dm_peers: vec![
                fauna_protocol::family::FamilyBlockedPeerInfo {
                    bridge_id: "nostr".into(),
                    peer_id: "npub1first".into(),
                    ..Default::default()
                },
                fauna_protocol::family::FamilyBlockedPeerInfo {
                    bridge_id: "nostr".into(),
                    peer_id: "npub1second".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let pairs: Vec<(&str, &str)> = ward
            .blocked_dm_peers
            .iter()
            .map(|p| (p.bridge_id.as_str(), p.peer_id.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("nostr", "npub1first"), ("nostr", "npub1second")]
        );
        assert!(
            FfiFamilyWardInfo::from(FamilyWardInfo::default())
                .blocked_dm_peers
                .is_empty(),
            "a ward with nothing denied has an empty list, not a missing one"
        );
    }

    /// What an app's enforcement stores read off a live read: the guardian,
    /// the floor, the Notify knob and the bedtime window all ride the fold.
    #[test]
    fn a_supervised_reply_hands_the_stores_its_fold() {
        let status = FfiFamilyStatus::from(FamilyStatusReply {
            supervised_by: Some(guardian()),
            policy: Some(a_full_policy()),
            ..Default::default()
        });
        let fold = status
            .supervision
            .expect("a supervised reply carries its fold");
        assert_eq!(fold.supervised_by.handle, "parent@example.org");
        assert_eq!(fold.supervised_by.actor_id, vec![0xab, 0xcd, 0x01]);
        assert_eq!(
            fold.content_policy.expect("the floor rides the fold").nsfw,
            "block"
        );
        assert!(fold.content_notify, "the Notify knob rides the fold");
        assert_eq!(
            fold.screen_time
                .expect("the window rides the fold")
                .daily_minutes,
            Some(90)
        );
    }

    /// The graduation gate on the app-facing value: a reply still carrying a
    /// policy document but naming NO guardian hands the stores nothing — the
    /// exact shape an app reading the raw `policy` would enforce against an
    /// unsupervised viewer (linux was fixed away from it, web after).
    #[test]
    fn a_policy_naming_no_guardian_hands_the_stores_nothing_enforceable() {
        let status = FfiFamilyStatus::from(FamilyStatusReply {
            supervised_by: None,
            policy: Some(a_full_policy()),
            ..Default::default()
        });
        assert!(
            status
                .policy
                .as_ref()
                .and_then(|p| p.content_policy.as_ref())
                .is_some(),
            "precondition: the raw document still names a floor"
        );
        assert_eq!(status.supervision, None, "no guardian, nothing enforceable");
        assert_eq!(
            FfiFamilyStatus::from(FamilyStatusReply::default()).supervision,
            None,
            "an unsupervised reply hands the stores nothing either"
        );
    }

    /// The native kids-app verdict follows the guardian, never a leftover
    /// policy document (the graduation flip).
    #[test]
    fn the_kids_app_verdict_follows_the_guardian() {
        let supervised = FfiFamilyStatus::from(FamilyStatusReply {
            supervised_by: Some(guardian()),
            ..Default::default()
        });
        assert!(kids_app_eligible(supervised));
        let graduated = FfiFamilyStatus::from(FamilyStatusReply {
            supervised_by: None,
            policy: Some(a_full_policy()),
            ..Default::default()
        });
        assert!(!kids_app_eligible(graduated));
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyContactRequestInfo`] — one
/// of the supervised caller's own pending contact asks (`family-safety.md`
/// § Child-initiated contact requests): what lets the refused-send surface
/// render "asked — waiting for your guardian".
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyContactRequest {
    pub peer_actor_id: Vec<u8>,
    /// The peer's handle, nest-joined for a local peer; empty when unknown.
    pub peer_handle: String,
    pub created_at: i64,
}

impl From<fauna_protocol::family::FamilyContactRequestInfo> for FfiFamilyContactRequest {
    fn from(i: fauna_protocol::family::FamilyContactRequestInfo) -> Self {
        FfiFamilyContactRequest {
            peer_actor_id: i.peer_actor_id.into_vec(),
            peer_handle: i.peer_handle,
            created_at: i.created_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::family::FamilyFeedRequestInfo`] — one of the
/// supervised caller's own feed-source asks (`family-safety.md` § Feed-source
/// approvals): what lets the blocked bridges surface render the ask/approved
/// state in place of a dead refusal.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyFeedRequest {
    pub bridge_id: String,
    /// `"link"` | `"follow"` | `"feed"` — parse with
    /// `fauna_core::data::FeedSourceOperation::from_wire` rather than matching
    /// literals, so a client cannot disagree with the nest about the set.
    pub operation: String,
    /// Empty for a `link`; the follow id / feed URI otherwise.
    pub target: String,
    /// The ward's own display label for the ask; may be empty.
    pub label: String,
    pub created_at: i64,
    /// `None` while pending, `Some(instant)` once granted — the `pending |
    /// approved` state, carried as the approval instant so the surface can also
    /// show how long is left to redeem (a grant lapses 7 days after approval).
    pub approved_at: Option<i64>,
}

impl From<fauna_protocol::family::FamilyFeedRequestInfo> for FfiFamilyFeedRequest {
    fn from(i: fauna_protocol::family::FamilyFeedRequestInfo) -> Self {
        FfiFamilyFeedRequest {
            bridge_id: i.bridge_id,
            operation: i.operation,
            target: i.target,
            label: i.label,
            created_at: i.created_at,
            approved_at: i.approved_at,
        }
    }
}

/// FFI mirror of [`fauna_core::data::FeedSourceOperation`] — the closed set of
/// operations a feed-source ask can name (`family-safety.md` § Feed-source
/// approvals). An enum so an app picks a member rather than typing a string: a
/// grant matches `(bridge_id, operation, target)` **exactly**, so an invented
/// spelling would mint an ask no gate could ever redeem.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiFeedSourceOperation {
    /// Connect the bridge account itself — its ask carries an empty target.
    Link,
    /// Add a follow on a connected bridge — the target is the follow id.
    Follow,
    /// Subscribe to an external feed — the target is the feed URI.
    Feed,
}

impl From<FfiFeedSourceOperation> for fauna_core::data::FeedSourceOperation {
    fn from(op: FfiFeedSourceOperation) -> Self {
        match op {
            FfiFeedSourceOperation::Link => Self::Link,
            FfiFeedSourceOperation::Follow => Self::Follow,
            FfiFeedSourceOperation::Feed => Self::Feed,
        }
    }
}

/// The canonical wire string for a feed-source operation — the value
/// [`FfiFamilyClient::feed_source_request`]'s `operation` takes and
/// [`FfiFamilyFeedRequest::operation`] carries back. Shared
/// `FeedSourceOperation::as_str`, so no app writes `"follow"` as a literal.
#[uniffi::export]
pub fn feed_source_operation_wire(operation: FfiFeedSourceOperation) -> String {
    fauna_core::data::FeedSourceOperation::from(operation)
        .as_str()
        .to_string()
}

/// The live state of a ward's feed-source ask, as `bridge-source-request-state`
/// renders it — FFI mirror of [`fauna_client_family::FeedRequestState`].
/// `Approved` is the try-again prompt; an app never retries on its own.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiFeedRequestState {
    Pending,
    Approved,
}

/// Whether the ward's own `contact_requests` (from `FfiFamilyStatus`) hold an
/// outstanding ask for `peer_actor_id_hex` — what `contact-request-pending`
/// renders from. Over the shared
/// [`fauna_client_family::contact_ask_pending`] (case-insensitive; a non-hex id
/// matches nothing), so no app re-derives the compare.
#[uniffi::export]
pub fn ward_contact_ask_pending(
    asks: Vec<FfiFamilyContactRequest>,
    peer_actor_id_hex: String,
) -> bool {
    let asks: Vec<fauna_protocol::family::FamilyContactRequestInfo> = asks
        .into_iter()
        .map(|a| fauna_protocol::family::FamilyContactRequestInfo {
            peer_actor_id: fauna_protocol::ByteBuf::from(a.peer_actor_id),
            peer_handle: a.peer_handle,
            created_at: a.created_at,
            ..Default::default()
        })
        .collect();
    fauna_client_family::contact_ask_pending(&asks, &peer_actor_id_hex)
}

/// The live state of the ward's feed-source ask for one
/// `(bridge_id, operation, target)` triple, or `None` when no live ask covers
/// it. Over the shared [`fauna_client_family::feed_request_state`], keyed on the
/// whole triple (the grant's scope).
#[uniffi::export]
pub fn ward_feed_request_state(
    asks: Vec<FfiFamilyFeedRequest>,
    bridge_id: String,
    operation: String,
    target: String,
) -> Option<FfiFeedRequestState> {
    let asks: Vec<fauna_protocol::family::FamilyFeedRequestInfo> = asks
        .into_iter()
        .map(|a| fauna_protocol::family::FamilyFeedRequestInfo {
            bridge_id: a.bridge_id,
            operation: a.operation,
            target: a.target,
            label: a.label,
            created_at: a.created_at,
            approved_at: a.approved_at,
            ..Default::default()
        })
        .collect();
    fauna_client_family::feed_request_state(&asks, &bridge_id, &operation, &target).map(|s| match s
    {
        fauna_client_family::FeedRequestState::Pending => FfiFeedRequestState::Pending,
        fauna_client_family::FeedRequestState::Approved => FfiFeedRequestState::Approved,
    })
}

/// FFI mirror of [`fauna_protocol::family::FamilyUsageReportReply`] — the
/// `usage_report` heartbeat's reply: the local-day bucket the nest stamped and
/// that day's cross-device foreground total (`family-safety.md` § Screen
/// time), the number the ward-client lock surface passes to
/// `ScreenTimePolicy::lock_state`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyUsageReport {
    pub day: i64,
    pub day_total_minutes: u32,
}

/// FFI mirror of [`fauna_protocol::family::FamilyApprovalEntry`] — one
/// pending reach approval. `kind` is `contact` | `mail_hold` |
/// `contact_request` | `feed_source`; a `mail_hold` entry carries envelope
/// metadata only, never content.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFamilyApprovalEntry {
    pub supervised_actor_id: Vec<u8>,
    pub supervised_handle: String,
    pub kind: String,
    /// The knock sender's actor. Empty for a `mail_hold`.
    pub peer_actor_id: Vec<u8>,
    /// The `mail_hold` sender's envelope address. Empty for a `contact`.
    pub peer_address: String,
    /// The held message's id — what `approvals_decide` names for a
    /// `mail_hold`. Empty for a `contact`.
    pub message_id: Vec<u8>,
    /// The knock's summary line. Always empty for a `mail_hold`: a subject is
    /// content, and the message is sealed to the ward. For a `feed_source` this
    /// is the ward's own display label for the thing they asked for.
    pub summary: String,
    /// v1.x — the peer's handle, nest-joined for a local peer so a
    /// `contact_request` row renders without a second lookup. Empty for the
    /// v1 kinds and for a peer with no local handle.
    pub peer_handle: String,
    /// v1.x — with [`Self::operation`] and [`Self::target`], the `feed_source`
    /// item's key: pass all three back to `approvals_decide`. Empty for every
    /// other kind. (`target` is empty for a `link`; `summary`/`label` is
    /// display-only and never part of the key.)
    pub bridge_id: String,
    /// v1.x — see [`Self::bridge_id`]. `"link"` | `"follow"` | `"feed"`; parse
    /// with `fauna_core::data::FeedSourceOperation::from_wire`.
    pub operation: String,
    /// v1.x — see [`Self::bridge_id`].
    pub target: String,
    pub created_at: i64,
}

impl From<FamilyApprovalEntry> for FfiFamilyApprovalEntry {
    fn from(e: FamilyApprovalEntry) -> Self {
        FfiFamilyApprovalEntry {
            supervised_actor_id: e.supervised_actor_id.into_vec(),
            supervised_handle: e.supervised_handle,
            kind: e.kind,
            peer_actor_id: e.peer_actor_id.into_vec(),
            peer_address: e.peer_address,
            message_id: e.message_id.into_vec(),
            summary: e.summary,
            peer_handle: e.peer_handle,
            bridge_id: e.bridge_id,
            operation: e.operation,
            target: e.target,
            created_at: e.created_at,
        }
    }
}

// ── The client object ──────────────────────────────────────────────────

/// Typed-call client for the `fauna.family.*` kinds over an authenticated
/// nest connection. Obtain via [`crate::FfiNestClient::family`].
#[derive(uniffi::Object)]
pub struct FfiFamilyClient {
    nest: Arc<NestClient>,
}

impl FfiFamilyClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> FamilyClient<Arc<NestClient>> {
        FamilyClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiFamilyClient {
    /// `fauna.family.status` — the caller's family relationships, both roles
    /// in one read (drives the supervised indicator + the Family surface).
    pub async fn status(&self) -> Result<FfiFamilyStatus, FfiError> {
        let reply = self.client().status().await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.family.policy.update` — replace a ward's reach policy.
    /// Guardian-only nest-side.
    pub async fn policy_update(
        &self,
        supervised_actor_id: Vec<u8>,
        policy: FfiReachPolicy,
    ) -> Result<(), FfiError> {
        self.client()
            .policy_update(supervised_actor_id, policy.into())
            .await
            .map_err(stringify)
    }

    /// `fauna.family.notify_report` — the **supervised** caller reports coarse
    /// per-category enforcement counts (`family-safety.md` § Guardian Notify).
    /// Each `count` is a delta the nest accumulates; carries no content ids.
    /// `utc_offset_minutes` is the device's UTC offset (nest-clamped to
    /// `-720..=840` — the § Screen time day-bucket rule). A no-op nest-side
    /// unless the caller is supervised with `content_notify` on.
    pub async fn notify_report(
        &self,
        entries: Vec<FfiFamilyContentNotice>,
        utc_offset_minutes: i32,
    ) -> Result<(), FfiError> {
        let entries = entries
            .into_iter()
            .map(|e| FamilyContentNotice {
                category: e.category,
                count: e.count,
                ..Default::default()
            })
            .collect();
        self.client()
            .notify_report(entries, utc_offset_minutes)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.usage_report` — the **supervised** caller heartbeats
    /// coarse foreground minutes for the daily screen-time budget
    /// (`family-safety.md` § Screen time). `minutes` is the foreground delta
    /// since the last successful report (`0` = a pure read — the unlock-screen
    /// check); the reply carries the day bucket and the day's **cross-device**
    /// total, the number the lock surface locks on. A silent zero-reply no-op
    /// nest-side unless the caller is supervised with a daily budget set.
    pub async fn usage_report(
        &self,
        minutes: u32,
        utc_offset_minutes: i32,
    ) -> Result<FfiFamilyUsageReport, FfiError> {
        let reply = self
            .client()
            .usage_report(minutes, utc_offset_minutes)
            .await
            .map_err(stringify)?;
        Ok(FfiFamilyUsageReport {
            day: reply.day,
            day_total_minutes: reply.day_total_minutes,
        })
    }

    /// `fauna.family.approvals.list` — the guardian's pending reach approvals.
    pub async fn approvals_list(&self) -> Result<Vec<FfiFamilyApprovalEntry>, FfiError> {
        let reply = self.client().approvals_list().await.map_err(stringify)?;
        Ok(reply.approvals.into_iter().map(Into::into).collect())
    }

    /// `fauna.family.approvals.decide` — approve or deny one pending item on
    /// the ward's behalf.
    ///
    /// **Each kind names its item with a different key** — pass that kind's key
    /// and leave the others empty; an entry from `approvals_list` carries all of
    /// them. `contact`/`contact_request` → `peer_actor_id`; `mail_hold` →
    /// `message_id` (the message, never the address); `feed_source` → the whole
    /// `(bridge_id, operation, target)` triple, whose `target` is empty for a
    /// `link` and whose `label` is never part of the key; `dm_hold` →
    /// `(bridge_id, peer_address)`, the external peer that is not an actor here.
    // One method carrying every kind's key mirrors the single wire request; see
    // `FamilyClient::approvals_decide` for why a key struct is not worth a
    // hand-written mirror in four binding languages.
    #[allow(clippy::too_many_arguments)]
    pub async fn approvals_decide(
        &self,
        supervised_actor_id: Vec<u8>,
        kind: String,
        peer_actor_id: Vec<u8>,
        message_id: Vec<u8>,
        bridge_id: String,
        operation: String,
        target: String,
        peer_address: String,
        approve: bool,
    ) -> Result<(), FfiError> {
        self.client()
            .approvals_decide(
                supervised_actor_id,
                kind,
                peer_actor_id,
                message_id,
                bridge_id,
                operation,
                target,
                peer_address,
                approve,
            )
            .await
            .map_err(stringify)
    }

    /// The guardian's **un-deny** of one bridge-DM peer
    /// (`family-safety.md` § The bridge-DM gate → *The un-deny surface*) — the
    /// allow button's call. Takes the denied row **whole** (a
    /// [`FfiFamilyBlockedPeer`] read off `FfiFamilyWardInfo::blocked_dm_peers`),
    /// so a button addresses exactly its own row's `(bridge_id, peer_id)`; the
    /// `dm_hold` wire kind and the peer riding `peer_address` are the shared
    /// client's, never an app's.
    pub async fn allow_blocked_peer(
        &self,
        supervised_actor_id: Vec<u8>,
        peer: FfiFamilyBlockedPeer,
    ) -> Result<(), FfiError> {
        self.client()
            .allow_blocked_dm_peer(supervised_actor_id, peer.bridge_id, peer.peer_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.contact.add` — pre-approve a contact on the ward's behalf.
    pub async fn contact_add(
        &self,
        supervised_actor_id: Vec<u8>,
        peer_actor_id: Vec<u8>,
    ) -> Result<(), FfiError> {
        self.client()
            .contact_add(supervised_actor_id, peer_actor_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.contact.request` — the **supervised** caller's in-app
    /// ask to contact a peer (`family-safety.md` § Child-initiated contact
    /// requests). Pending in the guardian's queue as kind `contact_request`;
    /// the caller's own pending asks ride `status().contact_requests`.
    pub async fn contact_request(&self, peer_actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .contact_request(peer_actor_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.feed_source.request` — the **supervised** caller's in-app
    /// ask to add an external source their `feed_sources = "block"` policy just
    /// refused (`family-safety.md` § Feed-source approvals). Pending in the
    /// guardian's queue as kind `feed_source`; the caller's own asks ride
    /// `status().feed_requests` with their `pending | approved` state.
    ///
    /// Approving mints a **single-use grant**, it does not perform the
    /// operation — a bridge link is interactive, so the nest never replays it.
    /// On the approved doorbell the client retries the original call, which
    /// passes the gate exactly once.
    ///
    /// `operation` is `"link" | "follow" | "feed"` — build it from
    /// `fauna_core::data::FeedSourceOperation::as_str`, never a literal. `target`
    /// is the follow id / feed URI and is **empty for `link`**; `label` is
    /// display-only and never authorizing.
    pub async fn feed_source_request(
        &self,
        bridge_id: String,
        operation: String,
        target: String,
        label: String,
    ) -> Result<(), FfiError> {
        self.client()
            .feed_source_request(bridge_id, operation, target, label)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.graduate` — supervised → full account, in place.
    pub async fn graduate(&self, supervised_actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .graduate(supervised_actor_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.device.mark` — set/clear the guardian-enrolled-device
    /// marker on one of the ward's devices. Guardian-only nest-side.
    ///
    /// A marked device cannot be removed by the ward and is auto-revoked at
    /// graduation; clearing the mark is the guardian's un-enroll step. The flag
    /// is rendered back on the ward's own device list — the pattern is
    /// transparent by construction.
    pub async fn device_mark(
        &self,
        supervised_actor_id: Vec<u8>,
        device_id: String,
        marked: bool,
    ) -> Result<(), FfiError> {
        self.client()
            .device_mark(supervised_actor_id, device_id, marked)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.transfer` — propose a new guardian for a ward; pending
    /// until the proposed guardian accepts (a self-proposal completes
    /// immediately). The policy document rides the link intact.
    pub async fn transfer(
        &self,
        supervised_actor_id: Vec<u8>,
        new_guardian_actor_id: Vec<u8>,
    ) -> Result<(), FfiError> {
        self.client()
            .transfer(supervised_actor_id, new_guardian_actor_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.transfer.accept` — consent to a proposal naming the
    /// caller as new guardian; completes the re-point.
    pub async fn transfer_accept(&self, supervised_actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .transfer_accept(supervised_actor_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.transfer.decline` — refuse a proposal naming the caller.
    pub async fn transfer_decline(&self, supervised_actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .transfer_decline(supervised_actor_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.family.transfer.cancel` — withdraw the ward's pending proposal.
    pub async fn transfer_cancel(&self, supervised_actor_id: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .transfer_cancel(supervised_actor_id)
            .await
            .map_err(stringify)
    }
}

// The unfloored build's verdicts (a bare label badges, an `inherit` floor lets
// the item show); the kids flavor's are `kids_floor_tests` below.
#[cfg(all(test, feature = "value-format", not(feature = "kids-floor")))]
mod content_policy_tests {
    use super::*;
    use fauna_core::content_category::ContentLabelEntry;

    fn label(category: &str, permille: u16) -> ContentLabelEntry {
        ContentLabelEntry {
            category: category.into(),
            confidence_per_mille: permille,
        }
    }

    fn floor(nsfw: &str, spam: &str, phishing: &str, commercial: &str) -> FfiContentPolicy {
        FfiContentPolicy {
            nsfw: nsfw.into(),
            spam: spam.into(),
            phishing: phishing.into(),
            commercial: commercial.into(),
        }
    }

    #[test]
    fn options_and_label_pass_through_shared() {
        // The catalog is the ratified `inherit / collapse / block`, in order.
        let opts = content_floor_options();
        assert_eq!(
            opts.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
            ["inherit", "collapse", "block"]
        );
        // An unparseable stored value labels fail-closed as `block`, never `inherit`.
        assert_eq!(
            content_floor_label("collapse".into()).key,
            "family.value_collapse"
        );
        assert_eq!(
            content_floor_label("garbage".into()).key,
            "family.value_block"
        );
    }

    #[test]
    fn verdict_no_rules_no_labels_shows() {
        assert_eq!(content_render_verdict(vec![], None, None, None), "show");
    }

    /// One region rule set, marshalled as the app's region client hands it over.
    fn region_set(code: &str, factor: &str, permille: u16, verdict: &str) -> FfiRegionRuleSet {
        FfiRegionRuleSet {
            region: code.into(),
            authority_name: format!("{code} authority"),
            rules: vec![FfiRegionRule {
                factor: factor.into(),
                min_permille: permille,
                verdict: verdict.into(),
                reason_code: format!("{code}-1"),
                reason: std::collections::HashMap::from([(
                    "default".to_string(),
                    format!("Restricted in {code}."),
                )]),
            }],
        }
    }

    #[test]
    fn the_composed_face_folds_the_region_source_and_names_its_authority() {
        // The native half of region-blocking.md § Where it composes: the region
        // is the third strictest-wins source, and a blocked item can name the
        // authority rather than a generic "policy".
        let composed = content_render_composed(
            vec![label("nsfw", 900)],
            None,
            None,
            None,
            vec![region_set("NO", "nsfw", 800, "block")],
        );
        assert_eq!(composed.verdict, "block");
        let region = composed.region.expect("attributed to the region");
        assert_eq!(region.region, "NO");
        assert_eq!(region.authority_name, "NO authority");
        assert_eq!(region.reason_code, "NO-1");
        assert_eq!(
            region.reason.get("default").map(String::as_str),
            Some("Restricted in NO.")
        );
    }

    #[test]
    fn the_composed_face_with_no_region_matches_the_verdict_only_face() {
        // The two faces must never disagree — the verdict-only one is exactly
        // this one with an empty chain, which is what keeps an app that has not
        // yet gained its region render correct rather than merely compiling.
        let policy = FfiContentPolicy {
            nsfw: "collapse".into(),
            spam: "block".into(),
            phishing: "inherit".into(),
            commercial: "inherit".into(),
        };
        for labels in [
            vec![],
            vec![label("nsfw", 900)],
            vec![label("spam", 900)],
            vec![label("commercial", 10)],
        ] {
            assert_eq!(
                content_render_composed(
                    labels.clone(),
                    Some(policy.clone()),
                    Some(500),
                    Some(500),
                    Vec::new()
                )
                .verdict,
                content_render_verdict(labels, Some(policy.clone()), Some(500), Some(500)),
            );
        }
    }

    #[test]
    fn a_region_set_whose_code_does_not_parse_is_dropped_whole() {
        // Such a code cannot name an enrolled region, so its rules could never
        // have come from a verified artifact. This plane only ever restricts, so
        // applying nothing is the safe direction.
        let composed = content_render_composed(
            vec![label("nsfw", 900)],
            None,
            None,
            None,
            vec![region_set("not a region", "nsfw", 800, "block")],
        );
        assert_eq!(composed.verdict, "badge");
        assert_eq!(composed.region, None);
    }

    #[test]
    fn an_unnameable_region_verb_does_not_escalate() {
        // A client too old to name a future verb must never escalate on it —
        // anything but the exact `block` string is the less strict verb.
        let composed = content_render_composed(
            vec![label("nsfw", 900)],
            None,
            None,
            None,
            vec![region_set("NO", "nsfw", 800, "obliterate")],
        );
        assert_eq!(composed.verdict, "collapse");
    }

    #[test]
    fn verdict_label_present_but_unescalated_badges() {
        // A label with no matching rule badges (moderation.md floor), never hides.
        assert_eq!(
            content_render_verdict(vec![label("nsfw", 900)], None, None, None),
            "badge"
        );
    }

    #[test]
    fn verdict_guardian_floor_blocks_and_collapses() {
        let policy = floor("block", "collapse", "inherit", "inherit");
        // nsfw floored to block → block wins.
        assert_eq!(
            content_render_verdict(vec![label("nsfw", 700)], Some(policy.clone()), None, None),
            "block"
        );
        // spam floored to collapse (no block present) → collapse.
        assert_eq!(
            content_render_verdict(vec![label("spam", 700)], Some(policy), None, None),
            "collapse"
        );
    }

    #[test]
    fn verdict_own_threshold_collapses_only_when_both_present() {
        // Own spam threshold at 500‰: a 700‰ spam label collapses for every viewer.
        assert_eq!(
            content_render_verdict(vec![label("spam", 700)], None, Some(500), Some(500)),
            "collapse"
        );
        // A single threshold (the other `None`) composes no own-preference rule —
        // the label only badges.
        assert_eq!(
            content_render_verdict(vec![label("spam", 700)], None, Some(500), None),
            "badge"
        );
    }

    #[test]
    fn verdict_unparseable_floor_fails_closed_to_block() {
        // A floor value this build cannot name renders fail-closed as `block`
        // (the guardian floor triggers at GUARDIAN_FLOOR_TRIGGER_PERMILLE = 500‰).
        let policy = floor("weirdnewvalue", "inherit", "inherit", "inherit");
        assert_eq!(
            content_render_verdict(vec![label("nsfw", 600)], Some(policy), None, None),
            "block"
        );
    }

    #[test]
    fn guardian_enforced_categories_empty_without_policy() {
        // No guardian policy (unsupervised viewer) → empty, mirrors the wasm
        // face's null-policy short-circuit.
        assert_eq!(
            guardian_enforced_categories(vec![label("nsfw", 900)], None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn guardian_enforced_categories_only_counts_enforcing_floors() {
        // nsfw floored to block (enforces); spam left `inherit` (does not) —
        // Notify counts only what the guardian's own policy is acting on.
        let policy = floor("block", "inherit", "inherit", "inherit");
        assert_eq!(
            guardian_enforced_categories(
                vec![label("nsfw", 700), label("spam", 700)],
                Some(policy)
            ),
            vec!["nsfw".to_string()]
        );
    }

    #[test]
    fn content_notice_line_reports_category_and_count() {
        let line = content_notice_line("nsfw".into(), 3);
        assert_eq!(line.label.key, "family.policy_content_nsfw_label");
        assert_eq!(line.value.key, "family.ward_content_notice_count");
        assert_eq!(line.value.args.get("count").map(String::as_str), Some("3"));
    }
}

/// The kids flavor (`family-safety.md` § The account age band → the kids-app
/// bullet, item (4)): every render face composes the compiled floor, whatever
/// the guardian policy says, and Guardian Notify still counts only the
/// guardian's own floor. Run with `--features kids-floor`.
#[cfg(all(test, feature = "value-format", feature = "kids-floor"))]
mod kids_floor_tests {
    use super::*;
    use fauna_core::content_category::ContentLabelEntry;

    fn label(category: &str, permille: u16) -> ContentLabelEntry {
        ContentLabelEntry {
            category: category.into(),
            confidence_per_mille: permille,
        }
    }

    fn inherit_all() -> FfiContentPolicy {
        FfiContentPolicy {
            nsfw: "inherit".into(),
            spam: "inherit".into(),
            phishing: "inherit".into(),
            commercial: "collapse".into(),
        }
    }

    #[test]
    fn every_face_renders_a_floored_category_blocked() {
        for category in fauna_core::obligation::GUARDIAN_FLOOR_CATEGORIES {
            let labels = vec![label(category, 900)];
            assert_eq!(
                content_render_verdict(labels.clone(), Some(inherit_all()), None, None),
                "block",
                "{category}: a policy below the floor renders at the floor"
            );
            assert_eq!(
                content_render_verdict(labels.clone(), None, None, None),
                "block",
                "{category}: no policy at all still renders at the floor"
            );
            let composed = content_render_for_item(
                vec![],
                "item".into(),
                None,
                labels,
                Some(inherit_all()),
                None,
                None,
                vec![],
            );
            assert_eq!(composed.verdict, "block");
        }
        // Below the trigger nothing bites; an unlabeled item still shows.
        assert_eq!(
            content_render_verdict(vec![label("nsfw", 10)], None, None, None),
            "badge"
        );
        assert_eq!(content_render_verdict(vec![], None, None, None), "show");
    }

    #[test]
    fn guardian_notify_is_not_floored() {
        assert!(
            guardian_enforced_categories(vec![label("nsfw", 900)], Some(inherit_all())).is_empty(),
            "an inherit guardian floor reports nothing, kids floor or not"
        );
    }
}

#[cfg(all(test, feature = "value-format"))]
mod notify_accumulator_tests {
    use super::*;
    use fauna_core::content_category::ContentLabelEntry;

    // The dedup/day-bucket/batching rules themselves are pinned by
    // `fauna_core::obligation`'s own `NotifyAccumulator` tests. These pin what
    // the FFI OBJECT adds on top: the disabled-by-default start, the
    // `content_policy: None` short-circuit, the labels→cats→record wiring,
    // the `take_due` wire conversion, and — RED-proven by neutering the
    // `Mutex` wrapper — that it really threads state across calls the way
    // `FfiUsageHeartbeat` does.

    fn label(category: &str, permille: u16) -> ContentLabelEntry {
        ContentLabelEntry {
            category: category.into(),
            confidence_per_mille: permille,
        }
    }

    fn floor(nsfw: &str, spam: &str, phishing: &str, commercial: &str) -> FfiContentPolicy {
        FfiContentPolicy {
            nsfw: nsfw.into(),
            spam: spam.into(),
            phishing: phishing.into(),
            commercial: commercial.into(),
        }
    }

    #[test]
    fn disabled_by_default_records_nothing() {
        let acc = FfiNotifyAccumulator::new();
        let policy = floor("block", "inherit", "inherit", "inherit");
        // No `set_enabled(true)` call — mirrors a fresh session before the
        // first `fauna.family.status` read lands.
        acc.record("p1".into(), vec![label("nsfw", 900)], Some(policy), 0, 0);
        assert!(acc.take_due(0).is_none());
    }

    #[test]
    fn no_policy_records_nothing_even_when_enabled() {
        let acc = FfiNotifyAccumulator::new();
        acc.set_enabled(true);
        // Unsupervised viewer / policy not yet loaded — the same short-circuit
        // as the `guardian_enforced_categories` free function.
        acc.record("p1".into(), vec![label("nsfw", 900)], None, 0, 0);
        assert!(acc.take_due(0).is_none());
    }

    #[test]
    fn enabled_with_enforcing_floor_records_and_flushes_eagerly() {
        let acc = FfiNotifyAccumulator::new();
        acc.set_enabled(true);
        let policy = floor("block", "inherit", "inherit", "inherit");
        acc.record(
            "p1".into(),
            vec![label("nsfw", 900), label("spam", 900)],
            Some(policy),
            1_000,
            120,
        );
        // spam is `inherit` (not enforcing) so only nsfw counts — the same
        // "which categories count" rule `guardian_enforced_categories` pins.
        let due = acc.take_due(1_000).expect("first flush is eager");
        assert_eq!(
            due.entries,
            vec![FfiFamilyContentNotice {
                category: "nsfw".into(),
                count: 1,
            }]
        );
        assert_eq!(due.offset_minutes, 120);
        // Draining clears the pending batch.
        assert!(acc.take_due(1_000).is_none());
    }

    #[test]
    fn dedup_within_the_same_local_day_does_not_double_count() {
        let acc = FfiNotifyAccumulator::new();
        acc.set_enabled(true);
        let policy = floor("block", "inherit", "inherit", "inherit");
        acc.record(
            "p1".into(),
            vec![label("nsfw", 900)],
            Some(policy.clone()),
            0,
            0,
        );
        acc.record("p1".into(), vec![label("nsfw", 900)], Some(policy), 0, 0);
        let due = acc.take_due(0).expect("first flush is eager");
        assert_eq!(
            due.entries,
            vec![FfiFamilyContentNotice {
                category: "nsfw".into(),
                count: 1,
            }]
        );
    }

    #[test]
    fn second_flush_within_the_interval_returns_none() {
        let acc = FfiNotifyAccumulator::new();
        acc.set_enabled(true);
        let policy = floor("block", "inherit", "inherit", "inherit");
        acc.record(
            "p1".into(),
            vec![label("nsfw", 900)],
            Some(policy.clone()),
            0,
            0,
        );
        assert!(acc.take_due(0).is_some());
        acc.record("p2".into(), vec![label("nsfw", 900)], Some(policy), 1, 1);
        // A second flush before a full interval elapses is not due, even
        // though there is now a pending count.
        assert!(acc.take_due(1).is_none());
    }

    #[test]
    fn disabling_drops_pending_counts() {
        let acc = FfiNotifyAccumulator::new();
        acc.set_enabled(true);
        let policy = floor("block", "inherit", "inherit", "inherit");
        acc.record("p1".into(), vec![label("nsfw", 900)], Some(policy), 0, 0);
        acc.set_enabled(false);
        // Consent withdrawn before the flush tick — the pending count must
        // not survive to be reported.
        assert!(acc.take_due(0).is_none());
    }

    #[test]
    fn reset_drops_everything_for_an_identity_change() {
        let acc = FfiNotifyAccumulator::new();
        acc.set_enabled(true);
        let policy = floor("block", "inherit", "inherit", "inherit");
        acc.record(
            "p1".into(),
            vec![label("nsfw", 900)],
            Some(policy.clone()),
            0,
            0,
        );
        acc.reset();
        // The reset accumulator is disabled again (default), so even a
        // record for the SAME item under the incoming actor is not silently
        // swallowed by the outgoing actor's dedup set.
        acc.record(
            "p1".into(),
            vec![label("nsfw", 900)],
            Some(policy.clone()),
            0,
            0,
        );
        assert!(
            acc.take_due(0).is_none(),
            "reset must disable, not just clear pending"
        );
        acc.set_enabled(true);
        acc.record("p1".into(), vec![label("nsfw", 900)], Some(policy), 0, 0);
        assert!(
            acc.take_due(0).is_some(),
            "reset must clear the dedup set — the incoming actor's first \
             enforcement on a reused item id must still count"
        );
    }
}

#[cfg(all(test, feature = "value-format"))]
mod screen_time_face_tests {
    use super::*;

    fn policy(start: Option<u16>, end: Option<u16>, daily: Option<u16>) -> FfiScreenTimePolicy {
        FfiScreenTimePolicy {
            window_start: start,
            window_end: end,
            daily_minutes: daily,
        }
    }

    // The rules themselves are pinned by `fauna_core::screen_time`'s own tests.
    // These pin what the *faces* add on top: the absent-policy default, the
    // record conversion, the error mapping, and — for the heartbeat — that the
    // interior `Mutex` really threads state across calls.

    #[test]
    fn absent_policy_never_locks() {
        // `None` is the unsupervised-equivalent default (goal doc § Screen
        // time: "all defaults = unsupervised-equivalent, i.e. absent"), so a
        // ward whose guardian set no screen-time control is never locked out.
        // This is the face's own `unwrap_or_default()`, not the engine's.
        assert!(screen_lock_message(None, 0, None, "guardian".into()).is_none());
        assert!(screen_lock_message(None, 1439, Some(9999), "guardian".into()).is_none());
    }

    #[test]
    fn window_policy_crosses_the_boundary_and_locks_outside_it() {
        // A 21:00–07:00 bedtime window: the wrap case, and the reason the
        // conversion has to survive the boundary intact.
        let bedtime = policy(Some(21 * 60), Some(7 * 60), None);
        assert!(
            screen_lock_message(Some(bedtime.clone()), 22 * 60, None, "guardian".into()).is_none(),
            "22:00 is inside a 21:00-07:00 window"
        );
        let locked = screen_lock_message(Some(bedtime), 12 * 60, None, "guardian".into())
            .expect("noon is outside a 21:00-07:00 window, so the ward is locked");
        assert_eq!(locked.key, "family.screen_lock_window");
    }

    #[test]
    fn budget_lock_reads_the_usage_total_the_caller_passes() {
        // The budget arm fails OPEN until a total has been heard — `None` must
        // not lock, or a ward would be locked out before the first heartbeat.
        let budget = policy(None, None, Some(60));
        assert!(
            screen_lock_message(Some(budget.clone()), 12 * 60, None, "guardian".into()).is_none(),
            "no total heard yet must not lock (the ratified fail-open)"
        );
        assert!(
            screen_lock_message(Some(budget.clone()), 12 * 60, Some(30), "guardian".into())
                .is_none(),
            "under budget does not lock"
        );
        let locked = screen_lock_message(Some(budget), 12 * 60, Some(60), "guardian".into())
            .expect("at the budget, the ward is locked");
        assert_eq!(locked.key, "family.screen_lock_budget");
    }

    #[test]
    fn parse_time_of_day_maps_empty_value_and_error() {
        // `FfiError` has no `PartialEq` (it is the crate-wide error type), so
        // compare the `Ok` payloads rather than widening it for a test.
        assert_eq!(parse_time_of_day("".into()).unwrap(), None);
        assert_eq!(parse_time_of_day("   ".into()).unwrap(), None);
        assert_eq!(parse_time_of_day("21:00".into()).unwrap(), Some(21 * 60));
        assert_eq!(parse_time_of_day("7:05".into()).unwrap(), Some(7 * 60 + 5));

        // The engine's static reason string must reach the client verbatim —
        // it is what the caller surfaces on its `error-message`.
        let err = parse_time_of_day("24:00".into()).expect_err("24:00 is out of range");
        let FfiError::General { msg } = err else {
            panic!("a parse failure must carry the engine's reason, not another variant");
        };
        assert_eq!(msg, "a time of day runs from 00:00 to 23:59");
    }

    #[test]
    fn parse_daily_minutes_accepts_zero_and_refuses_over_a_day() {
        // `0` is a real, accepted value — the deliberate full lock the empty
        // window refusal points guardians at.
        assert_eq!(parse_daily_minutes("0".into()).unwrap(), Some(0));
        assert_eq!(parse_daily_minutes("".into()).unwrap(), None);
        assert_eq!(parse_daily_minutes("1440".into()).unwrap(), Some(1440));

        let err = parse_daily_minutes("1441".into()).expect_err("over a day is refused");
        let FfiError::General { msg } = err else {
            panic!("a parse failure must carry the engine's reason");
        };
        assert_eq!(msg, "daily_minutes is at most 1440 (one day)");
    }

    #[test]
    fn format_time_of_day_is_the_parser_inverse() {
        for text in ["00:00", "07:05", "21:00", "23:59"] {
            let minutes = parse_time_of_day(text.into())
                .expect("valid")
                .expect("non-empty");
            assert_eq!(format_time_of_day(minutes), text);
        }
    }

    #[test]
    fn usage_today_line_renders_both_readouts() {
        // One call serves the guardian's line and the ward's own copy; the
        // budget-less wording is a different key, not a blank.
        let with_budget = usage_today_line(30, Some(60));
        let without = usage_today_line(30, None);
        assert_eq!(with_budget.label, without.label);
        assert_ne!(
            with_budget.value.key, without.value.key,
            "a set budget must read differently from no budget"
        );
    }

    #[test]
    fn heartbeat_threads_state_through_the_interior_mutex() {
        // The whole point of the `Mutex` wrapper: `&self` methods must still
        // mutate. A wrapper that silently dropped writes would leave every
        // native ward reporting zero forever.
        let hb = FfiUsageHeartbeat::new();
        assert!(!hb.is_accounting(), "no policy, no accounting");

        hb.set_policy(Some(policy(None, None, Some(60))));
        assert!(hb.is_accounting(), "a daily budget turns accounting on");

        hb.seed_total(Some(15));
        assert_eq!(
            hb.used_today_minutes(1_000),
            Some(15),
            "the seeded cross-device total must be readable back"
        );

        // Clearing the budget drops all accounting state — a stale total would
        // keep locking a ward whose guardian just lifted the limit.
        hb.set_policy(None);
        assert!(!hb.is_accounting());
        assert_eq!(hb.used_today_minutes(1_000), None);
    }

    #[test]
    fn heartbeat_accrues_foreground_time_and_re_credits_a_failed_report() {
        let hb = FfiUsageHeartbeat::new();
        hb.set_policy(Some(policy(None, None, Some(60))));
        hb.seed_total(Some(0));

        // Foreground, ticked at the required cadence: a suspended device
        // credits at most MAX_ACCRUAL_STEP_SECS (120s) per step, so six real
        // minutes must be ticked as three ≤2-minute steps, not one 6-minute
        // jump. Getting this wrong is the documented caller trap, and it
        // reaches native apps through this face unchanged.
        hb.set_active(true, 0);
        for t in [120, 240, 360] {
            hb.set_active(true, t);
        }
        let due = hb.take_due(360).expect("a flush is due after the interval");
        assert_eq!(due, 6, "six properly-ticked foreground minutes");

        // A report that never landed puts its minutes back on the pile —
        // the delta is "since the last SUCCESSFUL report".
        hb.report_failed();
        let redue = hb
            .take_due(12 * 60)
            .expect("the re-credited minutes are still owed");
        assert!(
            redue >= due,
            "a failed report must not silently forgive time ({redue} < {due})"
        );

        hb.report_succeeded(20_000, 12, 12 * 60);
        assert_eq!(hb.nest_day(), Some(20_000));

        hb.reset();
        assert!(!hb.is_accounting(), "reset drops everything on sign-out");
    }

    #[test]
    fn a_slow_tick_is_clamped_not_billed() {
        // The suspend clamp, reaching native apps through this face: a
        // device that slept for an hour must not bill the child for an hour it
        // never spent. This is why the heartbeat's docs tell callers to tick at
        // least every 2 minutes — the missing minutes are dropped, never
        // invented. (Caught by this test being wrong first: a single 6-minute
        // jump credited 2 minutes, not 6.)
        let hb = FfiUsageHeartbeat::new();
        hb.set_policy(Some(policy(None, None, Some(60))));
        hb.seed_total(Some(0));

        hb.set_active(true, 0);
        let due = hb
            .take_due(3600)
            .expect("a flush is due after an hour-long gap");
        assert_eq!(
            due, 2,
            "one hour-long step credits only MAX_ACCRUAL_STEP_SECS (120s = 2 min)"
        );
    }
}

#[cfg(test)]
mod ward_ask_face_tests {
    use super::*;

    /// The faces carry every field the shared rules read across the boundary:
    /// the peer bytes for the contact ask, and the whole triple plus
    /// `approved_at` for the feed-source ask.
    #[test]
    fn the_ward_ask_faces_answer_through_the_shared_rules() {
        let contact = FfiFamilyContactRequest {
            peer_actor_id: vec![0xcd; 32],
            peer_handle: String::new(),
            created_at: 0,
        };
        assert!(ward_contact_ask_pending(
            vec![contact.clone()],
            "CD".repeat(32)
        ));
        assert!(!ward_contact_ask_pending(vec![contact], "ab".repeat(32)));

        let feed = |approved: Option<i64>| FfiFamilyFeedRequest {
            bridge_id: "activitypub".into(),
            operation: "follow".into(),
            target: "x".into(),
            label: String::new(),
            created_at: 1,
            approved_at: approved,
        };
        let state = |asks, target: &str| {
            ward_feed_request_state(asks, "activitypub".into(), "follow".into(), target.into())
        };
        assert_eq!(
            state(vec![feed(None)], "x"),
            Some(FfiFeedRequestState::Pending)
        );
        assert_eq!(
            state(vec![feed(Some(2))], "x"),
            Some(FfiFeedRequestState::Approved)
        );
        assert_eq!(state(vec![feed(Some(2))], "y"), None);
    }
}

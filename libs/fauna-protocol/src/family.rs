//! Family-safety WS-RPC payload types — the `fauna.family.*` domain
//! (`docs/goal/behavior/family-safety.md` § Wire & data shape). v1 slice 2
//! carries the relationship/lifecycle kinds (`status`, `policy.update`,
//! `graduate`, `transfer`); the reach-approval kinds (`approvals.*`,
//! `contact.add`) land with slice 3 and the guardian-device marker
//! (`device.mark`) with the guardian-device slice.
//!
//! Authorization is per-target, checked in the handler against the
//! `guardianships` link table — never the admin role (guardianship is an
//! account-to-account link; `graduate`/`transfer` additionally accept the
//! admin as caller because they only remove/re-point oversight, mirroring the
//! admission-time designation being an admin act).
//!
//! Wire convention: actor references ride as raw 32-byte `ByteBuf`s (the
//! `fauna.admin.*` convention); the dag-cbor wire forbids floats (none here)
//! and every optional field is a plain top-level `Option`.
//!
//! Kind registry: `kind.rs::register_family_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use fauna_core::format::PolicySummaryLine;
use fauna_core::obligation::ContentPolicy;
use fauna_core::screen_time::ScreenTimePolicy;

use crate::Value;

/// The per-ward reach-policy document (`family-safety.md` § Guardian policy
/// pillar 1) — nest-enforced routing-floor knobs. String knobs are closed
/// enums on the wire: `unknown_sender_mail` ∈ {`allow`, `hold`, `reject`},
/// `feed_sources` ∈ {`allow`, `block`}; the handler rejects anything else.
/// The `Default` is the unsupervised-equivalent policy (nothing enforced).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReachPolicy {
    /// New contact edges (both directions) require guardian approval.
    #[serde(default)]
    pub contact_approval: bool,
    /// Cold inbound mail from unknown senders: `allow` | `hold` | `reject`.
    #[serde(default = "default_allow")]
    pub unknown_sender_mail: String,
    /// Actors on other nests may initiate contact with the ward.
    #[serde(default = "default_true")]
    pub federation_contact: bool,
    /// Connecting new external feed sources / follows: `allow` | `block`.
    #[serde(default = "default_allow")]
    pub feed_sources: String,
    /// v1.x content pillar — the guardian's per-category render floor
    /// (`family-safety.md` § Content policy). Client-enforced post-decrypt, so
    /// it rides *inside* the reach document but is not a routing knob. `None`
    /// (absent on the wire) means **leave the content pillar unchanged** on a
    /// `policy.update`, so a caller saving only the reach knobs never clobbers a
    /// content policy it does not send (§ Policy-update compatibility). The nest
    /// validates its floors closed-enum at write; a client renders an unparseable
    /// floor fail-closed (`ContentFloor::Unknown`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_policy: Option<ContentPolicy>,
    /// v1.x screen-time pillar — the guardian's usage window + daily budget
    /// (`family-safety.md` § Screen time). Client-enforced. Same absent-means-
    /// unchanged `policy.update` semantics as [`Self::content_policy`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_time: Option<ScreenTimePolicy>,
    /// v1.x Guardian Notify knob (`family-safety.md` § Guardian Notify) — when
    /// on, the ward's conforming client reports coarse per-category enforcement
    /// counts (`fauna.family.notify_report`) and the guardian is notified with
    /// *category + count, never content*. A single boolean, kept a top-level
    /// field rather than folded into [`Self::content_policy`] so that shared
    /// render type ([`ContentPolicy`]) stays a pure floor set. `None` (absent on
    /// the wire) means **leave the knob unchanged** on a `policy.update` (same
    /// absent-means-unchanged semantics as the other v1.x pillars); the default
    /// is off (the unsupervised-equivalent). Client-reported, so a modified
    /// client under-reports — the stated trust bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_notify: Option<bool>,
    /// v1.x bridge-DM gate knob (`family-safety.md` § The bridge-DM gate) —
    /// `allow` | `hold`, what the nest does with an inbound bridge DM from an
    /// external peer the ward has never corresponded with. A **routing-floor
    /// knob** like the four v1 ones (the nest enforces it at every inbound DM
    /// write path), but a *later* field, so it takes the absent-means-unchanged
    /// semantics of the v1.x pillars rather than the v1 knobs' replace semantics
    /// (§ Policy-update compatibility; the [`Self::content_notify`] precedent):
    /// `None` on a `policy.update` leaves the knob as stored, so a caller saving
    /// the four reach knobs never silently relaxes a gate it does not send. The default is `allow` (the unsupervised-equivalent), and `None`
    /// on a `status` read means the knob is at that default.
    ///
    /// A `String` rather than a parsed enum for the same reason the two v1 string
    /// knobs are: the wire carries whatever a *newer* nest stored, and the parse
    /// (`fauna_core::data::UnknownPeerDm::from_wire`) is where the fail-closed
    /// rule lives — one implementation, never re-declared per consumer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_peer_dm: Option<String>,
    /// The guardian tier's controversial-class feature limits
    /// (`dynamic-features.md` § Wire & data shape — *"the guardian tier
    /// deliberately mints no new kind"*: it rides this document as an additive
    /// sub-document, keyed by stable feature key). Same absent-means-unchanged
    /// `policy.update` semantics as the other v1.x pillars, so a caller that
    /// saves only the reach knobs never clears a guardian's feature limits it
    /// does not send.
    ///
    /// Unlike the content and screen-time pillars this is **nest-enforced**, not
    /// client-enforced: § Evaluation points makes feature gates the nest floor,
    /// which is why `dynamic-features.md:99` says this tier carries *no*
    /// conforming-client caveat — a modified client cannot bypass a guardian's
    /// feature limit the way it can bypass a content floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<crate::features::GuardianFeaturePolicies>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl ReachPolicy {
    /// The guardian-policy **defaults dial** — the policy document a supervised
    /// admission carrying an age band starts from (`family-safety.md` § The
    /// account age band, D2: the band *"selects the guardian-policy defaults
    /// offered at admission … which the guardian then edits per-knob exactly as
    /// today; enforcement stays where the three pillars put it"*).
    ///
    /// One shared catalog for the nest (which materializes it into the fresh
    /// `guardian_policies` row at admission) and the apps (whose mint/approve
    /// UI offers the same values) — priority #2: the
    /// per-band values exist exactly once. [`crate::age::AgeBand::Adult`]
    /// returns [`ReachPolicy::default`] (the unsupervised-equivalent document,
    /// § Wire & data shape's schema-default rule — a banded `18+` admission
    /// changes nothing until the guardian tightens it). The per-band values
    /// are **advisory product defaults, not enforcement**: the guardian edits
    /// every knob afterwards, so the catalog errs toward protective-but-usable
    /// rather than maximal.
    pub fn age_band_defaults(band: crate::age::AgeBand) -> Self {
        use crate::age::AgeBand;
        use fauna_core::obligation::{ContentFloor, ContentPolicy};

        // The minor bands' feature-tier default: the payments plane denied
        // (`zaps` follows through its declared subset edge in the meet;
        // `p2p-share` stays at tier-1 constants — family file sharing is not
        // the controversial arm for a ward). The guardian relaxes by editing
        // their own tier's document (`fauna.family.policy.update`).
        fn minor_features() -> crate::features::GuardianFeaturePolicies {
            let mut features = crate::features::GuardianFeaturePolicies::new();
            features.insert(
                "payments".to_string(),
                fauna_core::feature_gate::FeaturePolicy::DENIED,
            );
            features
        }

        match band {
            AgeBand::U13 => Self {
                contact_approval: true,
                unknown_sender_mail: "hold".to_string(),
                federation_contact: false,
                feed_sources: "block".to_string(),
                content_policy: Some(ContentPolicy {
                    nsfw: ContentFloor::Block,
                    spam: ContentFloor::Block,
                    phishing: ContentFloor::Block,
                    commercial: ContentFloor::Collapse,
                }),
                screen_time: None,
                content_notify: Some(true),
                unknown_peer_dm: Some("hold".to_string()),
                features: Some(minor_features()),
                extra: BTreeMap::new(),
            },
            AgeBand::Teen13To15 => Self {
                contact_approval: true,
                unknown_sender_mail: "hold".to_string(),
                federation_contact: true,
                feed_sources: "allow".to_string(),
                content_policy: Some(ContentPolicy {
                    nsfw: ContentFloor::Block,
                    spam: ContentFloor::Collapse,
                    phishing: ContentFloor::Block,
                    commercial: ContentFloor::Inherit,
                }),
                screen_time: None,
                content_notify: None,
                unknown_peer_dm: Some("hold".to_string()),
                features: Some(minor_features()),
                extra: BTreeMap::new(),
            },
            AgeBand::Teen16To17 => Self {
                contact_approval: false,
                unknown_sender_mail: "allow".to_string(),
                federation_contact: true,
                feed_sources: "allow".to_string(),
                content_policy: Some(ContentPolicy {
                    nsfw: ContentFloor::Collapse,
                    spam: ContentFloor::Inherit,
                    phishing: ContentFloor::Collapse,
                    commercial: ContentFloor::Inherit,
                }),
                screen_time: None,
                content_notify: None,
                unknown_peer_dm: None,
                features: Some(minor_features()),
                extra: BTreeMap::new(),
            },
            AgeBand::Adult => Self::default(),
        }
    }

    /// The five-line read-only summary the *supervised* side renders
    /// (`family-policy-summary`), in the ratified knob order, with all three
    /// string knobs fail-closed (`family-safety.md` § Implementation status —
    /// *"an unrecognized value renders as the strictest option (`hold` /
    /// `block`), never the permissive one"*).
    ///
    /// A thin forward to [`fauna_core::format::reach_policy_summary`], which lives
    /// a crate below and so cannot name `ReachPolicy`. Callers holding a policy
    /// use this rather than the five-argument form — it is what keeps the bools
    /// from being transposed at a call site.
    pub fn summary_lines(&self) -> Vec<PolicySummaryLine> {
        let mut lines = fauna_core::format::reach_policy_summary(
            self.contact_approval,
            &self.unknown_sender_mail,
            self.federation_contact,
            &self.feed_sources,
            self.unknown_peer_dm.as_deref(),
        );
        // Content-policy pillar (§ Content policy): append one line per non-inherit
        // category floor, so the ward sees the guardian's content rules exactly as
        // the reach knobs. An absent `content_policy` is the all-inherit default →
        // no content lines (the reach-only summary).
        if let Some(policy) = &self.content_policy {
            lines.extend(fauna_core::format::content_policy_summary(policy));
        }
        // Guardian Notify (§ Guardian Notify) — shown only when on (transparency;
        // off is the unsupervised-equivalent default and adds no rule to display).
        if self.content_notify == Some(true) {
            lines.push(PolicySummaryLine {
                label: fauna_core::localized::LocalizedText::key(
                    "family.policy_content_notify_label",
                ),
                value: fauna_core::localized::LocalizedText::key("common.enable"),
            });
        }
        lines
    }
}

fn default_allow() -> String {
    "allow".to_string()
}
fn default_true() -> bool {
    true
}

impl Default for ReachPolicy {
    fn default() -> Self {
        Self {
            contact_approval: false,
            unknown_sender_mail: default_allow(),
            federation_contact: true,
            feed_sources: default_allow(),
            content_policy: None,
            screen_time: None,
            content_notify: None,
            unknown_peer_dm: None,
            features: None,
            extra: BTreeMap::new(),
        }
    }
}

/// fauna.family.status — caller-scoped, no parameters. One read answering
/// "what are my family relationships?" for both roles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// An established account age band + its provenance, as `fauna.family.status`
/// surfaces it (`family-safety.md` § The account age band). Present only where
/// a band **row** exists: a supervised account admitted without a band
/// carries `None` — band unknown, render
/// nothing — and an account with no guardianship link is `18+`/`none` **by
/// construction**, deliberately not echoed here (the absence *is* the value;
/// [`crate::age::AgeBand::Adult`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyAgeBandInfo {
    /// The band's wire token ([`crate::age::AgeBand::from_wire`]).
    pub band: String,
    /// How it was established ([`crate::age::AgeBandProvenance::from_wire`]).
    /// Audits and admission policy key on this; enforcement never does.
    pub provenance: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The caller's guardian (supervised side of `status`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyGuardianInfo {
    pub actor_id: ByteBuf,
    pub handle: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One supervised account the caller guards (guardian side of `status`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyWardInfo {
    pub actor_id: ByteBuf,
    pub handle: String,
    pub policy: ReachPolicy,
    /// The outstanding transfer proposal for this ward, if any — pending until
    /// the proposed guardian accepts (`family-safety.md` § Graduation &
    /// transfer). Additive: absent on the wire means no proposal.
    #[serde(default)]
    pub pending_transfer: Option<FamilyPendingTransferInfo>,
    /// v1.x Guardian Notify (`family-safety.md` § Guardian Notify) — the ward's
    /// coarse per-category enforcement counts *for the current day*: `category +
    /// count, never content, never a content id*. The notification is the
    /// doorbell; this status field is the truth the Family surface renders.
    /// Additive; empty when Notify is off or nothing was reported today.
    #[serde(default)]
    pub content_notices: Vec<FamilyContentNotice>,
    /// v1.x screen time (`family-safety.md` § Screen time) — the ward's
    /// cross-device foreground total for their current local day (derived from
    /// the link's last-reported UTC offset). `None` when the policy sets no
    /// daily budget (no accounting without a declared policy); `Some(0)` when
    /// a budget is set and nothing was reported yet. Additive.
    #[serde(default)]
    pub usage_today_minutes: Option<u32>,
    /// v1.x device marker (`family-safety.md` § Full visibility for young
    /// children / Slice F) — the ward's registered devices in a slim,
    /// guardian-facing projection, so the guardian's Family page renders the
    /// per-device mark toggle (`family-device-mark-toggle`) from this one
    /// `fauna.family.status` read (the ui.yaml "sections render from one status
    /// read" intent). Guardian-populated and guardianship-guarded — only a
    /// linked guardian ever sees a ward's devices; empty when the ward has no devices.
    /// Additive.
    #[serde(default)]
    pub devices: Vec<FamilyWardDeviceInfo>,
    /// The ward's established age band, when one was set at admission
    /// (`family-safety.md` § The account age band). `None` = admitted before
    /// the band existed (band unknown) — see [`FamilyAgeBandInfo`]. Additive.
    #[serde(default)]
    pub age_band: Option<FamilyAgeBandInfo>,
    /// v1.x — the bridge-DM peers this guardian has DENIED for the ward
    /// (`family-safety.md` § The bridge-DM gate → *The un-deny surface*): the
    /// `block`-verdict rows only, never the `allow` ones.
    ///
    /// Exists because a deny was a **one-way door in the UI**: the flip back is
    /// wire-supported and always was (`approvals_decide { kind: "dm_hold",
    /// approve: true }` — idempotent and not queue-scoped, so it works long
    /// after the queue row is gone), but no read exposed the denied set, so the
    /// guardian had no way to name the peer they were un-denying. This is that
    /// read.
    ///
    /// Guardian-populated and guardianship-guarded, exactly like [`Self::devices`]:
    /// only a linked guardian ever sees it. Additive; empty for a ward with
    /// nothing denied.
    #[serde(default)]
    pub blocked_dm_peers: Vec<FamilyBlockedPeerInfo>,
    /// True exactly when a stored `features` sub-document exists that this nest
    /// cannot decode (`family-safety.md` § Wire & data shape — the guardian's
    /// feature-limits editor seed).
    ///
    /// ⚠ `policy.features` then holds the **enforced deny** of every registry
    /// member, not anything the guardian authored — an editor must never seed
    /// from it, and absent and unreadable must not collapse (`nest/common.md`
    /// § Unreadable stored values). Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub features_unreadable: bool,
    /// The **ceiling** the guardian's feature editor notes against: one entry
    /// per registry member in the registry's order, the meet of the tiers
    /// outside the guardian's (tiers 1–3) composed nest-side for the ward.
    /// Filled on every entry the nest answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features_ceiling: Vec<crate::features::FeatureCeilingItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One denied bridge-DM peer on [`FamilyWardInfo::blocked_dm_peers`] — what
/// `family-blocked-peer-item` renders and what
/// `family-blocked-peer-allow-button` un-denies.
///
/// Carries the same `(bridge_id, peer_id)` pair `approvals_decide` takes, so the
/// un-deny needs no second lookup and cannot address a different peer than the
/// row the guardian read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyBlockedPeerInfo {
    /// The bridge the peer reached the ward through (`nostr` today; the gate's
    /// standing rule binds the next bridge's DM ingest the moment it goes live).
    pub bridge_id: String,
    /// The peer's bridge-native id — the spelling the verdict is stored under
    /// and the one `approvals_decide` matches on.
    pub peer_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One of a ward's registered devices in the slim guardian-facing projection on
/// [`FamilyWardInfo`] — just what the guardian's per-device mark control needs:
/// the id to pass to `fauna.family.device.mark`, a label to show, and the mark
/// state to render the toggle. Deliberately NOT the full [`crate::sync::SyncDevice`]
/// (no folder roles, `online`, or timestamps): a guardian *marks* a ward's
/// devices, it does not manage the ward's sync.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyWardDeviceInfo {
    /// Hex-encoded 32-byte device id — the key `fauna.family.device.mark` takes
    /// (the same spelling `fauna.sync.devices.list` renders to the ward).
    pub device_id: String,
    /// The device's **display identity**, never the ward's user-chosen label
    /// (ruled 2026-08-02 — `family-safety.md` § Full visibility for young
    /// children): a user-chosen label rests sealed under the ward's own root,
    /// which a guardian neither holds nor may be handed (no guardian key
    /// escrow), so the nest populates this with the device's **code** — the hex
    /// `device_id`, elided to the narrowest width that separates every device in
    /// that ward's list — with the machine-authored plaintext label beside it
    /// when one rests (`fauna_core::format::device_display_identities`, chosen
    /// over the whole list at once because `device_id` is client-chosen and a
    /// fixed-width prefix is therefore forgeable).
    /// Guaranteed non-empty and distinct across the ward's list; apps render it
    /// verbatim as the row's text.
    pub label: String,
    /// This device currently carries the guardian-enrolled marker; the guardian
    /// renders the toggle's on/off state from it. Additive.
    #[serde(default)]
    pub guardian_marked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One coarse per-category enforcement count on the guardian's Family surface
/// (`family-safety.md` § Guardian Notify — rides [`FamilyWardInfo`] and the
/// `fauna.family.notify_report` request). `category` is one of the four
/// negative canonical categories (`nsfw` | `spam` | `phishing` | `commercial`);
/// `count` carries **no content identifier** — the whole point of Notify.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FamilyContentNotice {
    pub category: String,
    pub count: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A pending transfer proposal as the *initiating* side sees it (rides
/// [`FamilyWardInfo`]): who was proposed, and when.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyPendingTransferInfo {
    pub proposed_guardian_actor_id: ByteBuf,
    pub proposed_guardian_handle: String,
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A pending transfer proposal as the *proposed guardian* sees it (rides
/// [`FamilyStatusReply::incoming_transfers`]): which ward would come under
/// their guardianship, handing off from whom.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyIncomingTransferInfo {
    /// The ward — what `fauna.family.transfer.accept`/`.decline` name.
    pub supervised_actor_id: ByteBuf,
    pub supervised_handle: String,
    /// The ward's *current* guardian (the initiator may have been the admin;
    /// the current guardian is the fact the prompt renders).
    pub guardian_handle: String,
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.family.status`. `supervised_by` + `policy` are set when
/// the caller is supervised (the supervised indicator renders from them);
/// `wards` is non-empty when the caller guards someone (the Family surface
/// renders from it). All empty/None for an ordinary account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyStatusReply {
    #[serde(default)]
    pub supervised_by: Option<FamilyGuardianInfo>,
    /// The caller's own active reach policy, when supervised (transparency
    /// by construction — the ward always sees the policy).
    #[serde(default)]
    pub policy: Option<ReachPolicy>,
    #[serde(default)]
    pub wards: Vec<FamilyWardInfo>,
    /// Transfer proposals awaiting the *caller's* consent as proposed guardian
    /// (`family-safety.md` § Graduation & transfer). Additive; empty for
    /// everyone who is not a proposed guardian. Clients widen the `family-tab`
    /// gate on it — a target not otherwise in a family relationship must still
    /// reach the prompt.
    #[serde(default)]
    pub incoming_transfers: Vec<FamilyIncomingTransferInfo>,
    /// v1.x screen time — the *caller's own* cross-device foreground total for
    /// their current local day, when the caller is supervised under a daily
    /// budget (`family-safety.md` § Screen time: the ward's summary shows the
    /// same number the guardian sees — transparency). `None` when the caller
    /// is unsupervised or no budget is set. Additive.
    #[serde(default)]
    pub usage_today_minutes: Option<u32>,
    /// v1.x — the supervised caller's own pending contact asks
    /// (`family-safety.md` § Child-initiated contact requests). Additive;
    /// empty for everyone unsupervised or with nothing pending.
    #[serde(default)]
    pub contact_requests: Vec<FamilyContactRequestInfo>,
    /// v1.x — the supervised caller's own feed-source asks, pending *and*
    /// approved-but-unredeemed (`family-safety.md` § Feed-source approvals).
    /// Additive; empty for everyone unsupervised or with nothing live.
    #[serde(default)]
    pub feed_requests: Vec<FamilyFeedRequestInfo>,
    /// The **caller's own** established age band, when a band row exists
    /// (`family-safety.md` § The account age band — transparency: the account
    /// always sees its own band and how it was established, exactly as it sees
    /// its policy). `None` for the common cases: no row (an unsupervised
    /// account is `18+`/`none` by construction) or a band-less admission.
    /// Additive.
    #[serde(default)]
    pub age_band: Option<FamilyAgeBandInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.policy.update — replace the ward's reach-policy document.
/// Caller must be the ward's guardian (never the admin, never the ward).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyPolicyUpdateRequest {
    pub supervised_actor_id: ByteBuf,
    pub policy: ReachPolicy,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.notify_report — the **supervised account** (its conforming
/// client) reports coarse per-category enforcement counts (`family-safety.md`
/// § Guardian Notify). Each entry's `count` is a **delta** — events since the
/// last report — which the nest accumulates into the day's stored total (the
/// reused [`FamilyContentNotice`] shape carries the accumulated total on the
/// `status` read; here it is the increment). Batched (at most hourly), carrying
/// **no content ids ever**. A no-op unless the caller is supervised with the
/// guardian's `content_notify` knob on; entries outside the four negative
/// canonical categories are dropped, counts clamped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FamilyNotifyReportRequest {
    #[serde(default)]
    pub entries: Vec<FamilyContentNotice>,
    /// The reporting device's UTC offset in minutes (`family-safety.md`
    /// § Screen time — the day-bucket rule, adopted by § Guardian Notify): the
    /// nest stamps the report's day bucket from **its own clock** plus this
    /// offset, clamped to `-720..=840`. Additive: an omitted offset means
    /// the bucket degrades to UTC.
    #[serde(default)]
    pub utc_offset_minutes: i32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.usage_report — the **supervised account** (its conforming
/// client) reports coarse foreground use for the daily screen-time budget
/// (`family-safety.md` § Screen time). `minutes` is the client-aggregated
/// foreground delta since its last successful report (clamped per report); a
/// **zero-minute report is a read** — it returns the day's total without
/// crediting (the unlock-screen check). Reporting runs only while the policy
/// sets a daily budget; the nest is a silent zero-reply no-op otherwise (like
/// `notify_report`, best-effort telemetry that discloses nothing by shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FamilyUsageReportRequest {
    /// Foreground minutes since the last successful report. Clamped by the
    /// nest; `0` = just read the day's total.
    #[serde(default)]
    pub minutes: u32,
    /// The reporting device's UTC offset in minutes, clamped to `-720..=840` —
    /// the day-bucket rule (`family-safety.md` § Screen time). Additive:
    /// absent degrades to UTC.
    #[serde(default)]
    pub utc_offset_minutes: i32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.family.usage_report`: the day bucket the nest stamped and
/// that day's **cross-device** running total, so the enforcing client learns
/// the number it locks on at each heartbeat without a second read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FamilyUsageReportReply {
    /// The local-day bucket the report landed in (`(now_epoch_secs +
    /// offset·60) / 86_400`, nest-stamped).
    pub day: i64,
    /// That day's cross-device foreground total after this report.
    pub day_total_minutes: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.device.mark — set or clear the guardian-enrolled-device marker
/// on one of the ward's devices (`family-safety.md` § Full visibility). Caller:
/// the **ward's guardian** only — never the admin (the trust shape attaches no
/// oversight power to that role) and never the ward, whose inability to
/// unilaterally unmark is the whole point.
///
/// The marker is what lets the nest tell the guardian's enrolled device from
/// the child's own — enrollment authenticates as the child's account, so
/// without it two promises are unenforceable: the child cannot remove the
/// guardian's device, and graduation auto-revokes it. Zero new cryptography:
/// the mark is one flag on the device row, rendered in the child's own device
/// list (transparency by construction).
///
/// `marked: false` is the guardian's ordinary un-enroll path — unmark first,
/// then delete the device the usual way.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyDeviceMarkRequest {
    pub supervised_actor_id: ByteBuf,
    /// Hex-encoded 32-byte device id, matching `fauna.sync.devices.*`.
    pub device_id: String,
    pub marked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.graduate — supervised → full account, in place
/// (`family-safety.md` § Graduation & transfer). Caller: the ward's guardian
/// or the admin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyGraduateRequest {
    pub supervised_actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.transfer — propose a new guardian for the ward: records a
/// pending transfer the proposed guardian must accept; the link is untouched
/// until then (`family-safety.md` § Graduation & transfer — the consent
/// handshake, ratified 2026-07-12). Caller: the current guardian or the
/// admin; the proposed guardian passes the same admissibility validation as
/// at admission. A caller proposing *themself* (an admin taking over) has
/// consented by construction, so that one case completes immediately. The
/// wire shape is unchanged from the pre-handshake kind — only the semantics
/// moved from "re-point now" to "propose".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyTransferRequest {
    pub supervised_actor_id: ByteBuf,
    pub new_guardian_actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.transfer.accept — the proposed guardian consents: re-points
/// the link and clears the pending proposal in one transaction. Caller: the
/// proposed guardian named by the ward's pending transfer; admissibility is
/// re-validated at accept time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyTransferAcceptRequest {
    pub supervised_actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.transfer.decline — the proposed guardian refuses; the pending
/// proposal is dropped and the link stands. Always available to the proposed
/// guardian (refusing a duty needs no admissibility).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyTransferDeclineRequest {
    pub supervised_actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.transfer.cancel — the current guardian or the admin withdraws
/// the ward's pending transfer proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyTransferCancelRequest {
    pub supervised_actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Generic success reply for the family mutations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyOkReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.approvals.list — the guardian's reach-approval queue
/// (`family-safety.md` § Reach approvals): pending items across every ward
/// whose policy routes them to the guardian. No parameters (caller-scoped).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyApprovalsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One pending reach approval. `kind` is a closed enum on the wire —
/// `"contact"` in v1 (the ward's pending knock, read guardian-side);
/// `"mail_hold"` joins it with the unknown-sender-mail slice. A mail hold
/// carries envelope metadata only (sender, time) — never content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyApprovalEntry {
    pub supervised_actor_id: ByteBuf,
    /// The ward's handle (so the queue renders without a second lookup).
    pub supervised_handle: String,
    /// `"contact"` | `"mail_hold"` | `"contact_request"` (v1.x — the ward's
    /// own ask, § Child-initiated contact requests) | `"feed_source"` (v1.x —
    /// the ward's ask to add an external source, § Feed-source approvals).
    pub kind: String,
    /// The party requesting reach: the knock sender's actor id (`contact`),
    /// or the peer the ward asked for (`contact_request`). Empty for a
    /// `mail_hold` — a mail sender has no actor on this nest. Kept honest
    /// rather than overloaded (`family-safety.md` § Reach approvals).
    pub peer_actor_id: ByteBuf,
    /// The `mail_hold` sender's envelope address; empty for a `contact`. The
    /// only sender fact the guardian sees — never subject-derived text, never
    /// content (`family-safety.md` § Don't do these).
    #[serde(default)]
    pub peer_address: String,
    /// Identifies a `mail_hold` item on the companion
    /// [`FamilyApprovalDecideRequest`]: the held message's 32-byte message id.
    /// Empty for a `contact`. Without it a guardian's client could render the
    /// queue but not decide on it.
    #[serde(default)]
    pub message_id: ByteBuf,
    /// The knock's summary line (already floor metadata on the knock row).
    /// Empty for a `mail_hold`: a subject line is content. For a
    /// `feed_source` this carries the ask's `label` — the ward's own naming of
    /// the *thing being approved* (a petname / feed name), not third-party
    /// content (`family-safety.md` § Feed-source approvals).
    pub summary: String,
    /// v1.x — the peer's handle, nest-joined for a local peer so a
    /// `contact_request` row renders without a second lookup
    /// (`family-safety.md` § Child-initiated contact requests). Empty for
    /// the v1 kinds and for a peer with no local handle. Additive.
    #[serde(default)]
    pub peer_handle: String,
    /// v1.x — the first third of a `feed_source` item's key: which bridge the
    /// ward is asking to draw from (`family-safety.md` § Feed-source
    /// approvals). Empty for every other kind. Additive.
    #[serde(default)]
    pub bridge_id: String,
    /// v1.x — the second third of a `feed_source` item's key: `"link"` |
    /// `"follow"` | `"feed"` (`fauna_core::data::FeedSourceOperation`). Empty
    /// for every other kind. Additive.
    #[serde(default)]
    pub operation: String,
    /// v1.x — the last third of a `feed_source` item's key: the follow id
    /// (`follow`) or feed URI (`feed`), and **empty for `link`**, which
    /// approves connecting the bridge as a whole. Empty for every other kind.
    /// Additive.
    ///
    /// The key is all three parts together, never `summary`/`label` — a label
    /// is display-only and never authorizing (§ Feed-source approvals).
    #[serde(default)]
    pub target: String,
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl FamilyApprovalEntry {
    /// What a `family-approval-item` row should display verbatim, or `None`
    /// when the caller should render its own localized no-sender placeholder.
    ///
    /// A thin forward to [`fauna_core::format::approval_display_text`], which
    /// lives a crate below and so cannot name `FamilyApprovalEntry`.
    pub fn display_text(&self) -> Option<&str> {
        fauna_core::format::approval_display_text(
            &self.kind,
            &self.peer_address,
            &self.peer_handle,
            &self.summary,
        )
    }
}

/// Reply to `fauna.family.approvals.list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyApprovalsListReply {
    pub approvals: Vec<FamilyApprovalEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.approvals.decide — approve or deny one pending reach item on
/// the ward's behalf. Guardian-only. For `kind = "contact"`: approve runs the
/// same accept path the ward's own knock review would (accepted edge + knock
/// dismissed); deny blocks the sender. For `kind = "mail_hold"`: approve moves
/// the held message to INBOX and allowlists `peer_address`; deny discards it.
/// For `kind = "contact_request"` (v1.x): approve mints the accepted edge
/// exactly as `contact.add` would; deny drops the ask and **deliberately does
/// not block the named peer** (`family-safety.md` § Child-initiated contact
/// requests — a request-deny refuses the child's question, and auto-blocking
/// would punish a third party for it).
///
/// For `kind = "feed_source"` (v1.x): approve **mints a single-use grant**
/// rather than performing the operation — a bridge link is interactive, so a
/// nest-side replay would run it as the ward hours later — and rings the ward
/// to retry; deny drops the ask (`family-safety.md` § Feed-source approvals).
///
/// For `kind = "dm_hold"` (v1.x): approve writes the peer verdict `allow` (the
/// conversation releases and future DMs deliver); deny writes `block` (new
/// arrivals are refused before storage, while already-stored messages stay the
/// ward's to read). Keyed on [`Self::bridge_id`] + [`Self::peer_address`]
/// (`family-safety.md` § The bridge-DM gate).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyApprovalDecideRequest {
    pub supervised_actor_id: ByteBuf,
    /// `"contact"` | `"mail_hold"` | `"contact_request"` | `"feed_source"` |
    /// `"dm_hold"`.
    pub kind: String,
    /// Identifies a `contact` or `contact_request` item. Empty for a `mail_hold`.
    pub peer_actor_id: ByteBuf,
    /// Identifies a `mail_hold` item: the held message's 32-byte message id.
    /// Empty for a `contact`. The message id — not the address — is the key, so
    /// deciding one held message never sweeps every message from that sender.
    #[serde(default)]
    pub message_id: ByteBuf,
    /// v1.x — with [`Self::bridge_id`], identifies a `dm_hold` item: the
    /// external peer's id (a nostr pubkey hex, …). Empty for every other kind.
    /// Additive.
    ///
    /// The same field the queue entry carries, and named `peer_address` for the
    /// same reason: an external DM peer is not an actor on this nest, so it
    /// cannot ride `peer_actor_id` without making that field dishonest
    /// (§ Reach approvals).
    #[serde(default)]
    pub peer_address: String,
    /// v1.x — with [`Self::operation`] and [`Self::target`], identifies a
    /// `feed_source` item; with [`Self::peer_address`], a `dm_hold` item. Empty
    /// for every other kind. Additive.
    #[serde(default)]
    pub bridge_id: String,
    /// v1.x — see [`Self::bridge_id`]. `"link"` | `"follow"` | `"feed"`.
    #[serde(default)]
    pub operation: String,
    /// v1.x — see [`Self::bridge_id`]. Empty for a `link` (and for every other
    /// kind). The triple is the key, so approving one feed source never
    /// unlocks another — the same "decide the item, not the sender" rule a
    /// `mail_hold`'s message id enforces.
    #[serde(default)]
    pub target: String,
    pub approve: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.contact.add — pre-approve a contact on the ward's behalf
/// (creates the accepted edge; dismisses a matching pending knock if one
/// exists). Guardian-only — the complement of contact-approval mode for the
/// outbound direction ("can I talk to X?" across the dinner table).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyContactAddRequest {
    pub supervised_actor_id: ByteBuf,
    pub peer_actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.contact.request — the ward's in-app ask to contact a peer
/// (`family-safety.md` § Child-initiated contact requests). Called by the
/// **supervised account**; pending in the guardian's queue as kind
/// `contact_request` until decided. Carries no message text, deliberately —
/// the ask is identified by *who*, never *why*.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyContactRequestRequest {
    pub peer_actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One of the supervised caller's own pending contact asks, surfaced
/// additively on [`FamilyStatusReply::contact_requests`] so the refused-send
/// surface renders "asked — waiting for your guardian" instead of a dead
/// refusal (`family-safety.md` § Child-initiated contact requests).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyContactRequestInfo {
    pub peer_actor_id: ByteBuf,
    /// The peer's handle, nest-joined for a local peer; empty when unknown.
    #[serde(default)]
    pub peer_handle: String,
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// fauna.family.feed_source.request — the ward's in-app ask to add an external
/// feed source the `feed_sources = "block"` knob just refused
/// (`family-safety.md` § Feed-source approvals). Called by the **supervised
/// account**; pending in the guardian's queue as kind `feed_source` until
/// decided. Approving mints a single-use grant the ward redeems by retrying —
/// the nest never replays the operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyFeedSourceRequestRequest {
    /// Which bridge the ward wants to draw from.
    pub bridge_id: String,
    /// `"link"` | `"follow"` | `"feed"` — the closed set
    /// `fauna_core::data::FeedSourceOperation` parses; anything else is refused.
    pub operation: String,
    /// The object being asked for: the follow id (`follow`) or feed URI
    /// (`feed`), **empty for `link`**. With `bridge_id` + `operation` this is
    /// the grant's exact match key.
    pub target: String,
    /// A display-only name for the thing being asked for (the petname / feed
    /// name the ward sees), length-capped. **Never authorizing** — it rides the
    /// queue row's `summary` for the guardian to read, and the grant matches on
    /// `(bridge_id, operation, target)` alone.
    #[serde(default)]
    pub label: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One of the supervised caller's own feed-source asks, surfaced additively on
/// [`FamilyStatusReply::feed_requests`] so the blocked bridges surface renders
/// the ask/approved state in place (`family-safety.md` § Feed-source
/// approvals).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FamilyFeedRequestInfo {
    pub bridge_id: String,
    /// `"link"` | `"follow"` | `"feed"`.
    pub operation: String,
    /// Empty for a `link`; the follow id / feed URI otherwise.
    pub target: String,
    /// The ward's own display label for the ask; may be empty.
    #[serde(default)]
    pub label: String,
    pub created_at: i64,
    /// When the guardian approved — `None` while the ask is still **pending**,
    /// `Some` once it is an **approved** grant. This is the `pending |
    /// approved` state the ward's surface renders (§ Feed-source approvals),
    /// carried as the approval *instant* rather than a state string so the
    /// surface can also say how long is left to redeem: a grant lapses 7 days
    /// after approval.
    ///
    /// Only live rows are listed — an expired pending ask or a lapsed grant is
    /// filtered out nest-side rather than surfaced in a dead state.
    #[serde(default)]
    pub approved_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};
    use fauna_core::obligation::{ContentFloor, ContentPolicy};
    use fauna_core::screen_time::ScreenTimePolicy;

    #[test]
    fn reach_policy_defaults_are_unsupervised_equivalent() {
        let d = ReachPolicy::default();
        assert!(!d.contact_approval);
        assert_eq!(d.unknown_sender_mail, "allow");
        assert!(d.federation_contact);
        assert_eq!(d.feed_sources, "allow");
        assert_round_trips(&d);
        // A legacy/partial policy document decodes to the defaults.
        #[derive(Serialize)]
        struct Empty {}
        let bytes = encode_canonical(&Empty {}).unwrap();
        assert_eq!(
            decode::<ReachPolicy>(&bytes).unwrap(),
            ReachPolicy::default()
        );
    }

    #[test]
    fn summary_lines_append_content_floors_and_notify() {
        // A v1-shaped policy (no content pillar) yields exactly the five reach
        // lines — the reach-only summary.
        let reach_only = ReachPolicy::default();
        assert_eq!(reach_only.summary_lines().len(), 5);

        // A guardian who set a spam floor + turned Notify on gets two extra lines,
        // in that order (content floors, then notify), only for the non-inherit
        // category and only because notify is on.
        let policy = ReachPolicy {
            content_policy: Some(ContentPolicy {
                spam: ContentFloor::Block,
                ..Default::default()
            }),
            content_notify: Some(true),
            ..Default::default()
        };
        let lines = policy.summary_lines();
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[5].label.key, "family.policy_content_spam_label");
        assert_eq!(lines[5].value.key, "family.value_block");
        assert_eq!(lines[6].label.key, "family.policy_content_notify_label");
        assert_eq!(lines[6].value.key, "common.enable");

        // Notify off (the default) adds no line; an all-inherit content_policy
        // adds no content lines.
        let quiet = ReachPolicy {
            content_policy: Some(ContentPolicy::default()),
            content_notify: Some(false),
            ..Default::default()
        };
        assert_eq!(quiet.summary_lines().len(), 5);
    }

    #[test]
    fn family_wire_round_trips() {
        assert_round_trips(&FamilyStatusRequest::default());
        assert_round_trips(&FamilyStatusReply::default());
        assert_round_trips(&FamilyStatusReply {
            supervised_by: Some(FamilyGuardianInfo {
                actor_id: ByteBuf::from(vec![1u8; 32]),
                handle: "parent".into(),
                extra: BTreeMap::new(),
            }),
            policy: Some(ReachPolicy {
                contact_approval: true,
                unknown_sender_mail: "hold".into(),
                ..Default::default()
            }),
            wards: vec![FamilyWardInfo {
                actor_id: ByteBuf::from(vec![2u8; 32]),
                handle: "kid".into(),
                policy: ReachPolicy::default(),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_round_trips(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            policy: ReachPolicy::default(),
            ..Default::default()
        });
        assert_round_trips(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            ..Default::default()
        });
        assert_round_trips(&FamilyTransferRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            new_guardian_actor_id: ByteBuf::from(vec![3u8; 32]),
            ..Default::default()
        });
        assert_round_trips(&FamilyOkReply {
            ok: true,
            ..Default::default()
        });
    }

    /// The guardian's feature-limits editor seed (`family-safety.md` § Wire &
    /// data shape): the ceiling rides every entry, and the unreadable flag is
    /// written only when set.
    #[test]
    fn a_ward_entry_round_trips_its_feature_limits_seed() {
        use fauna_core::feature_gate::{GatedFeature, effective_policy};

        let readable = FamilyWardInfo {
            actor_id: ByteBuf::from(vec![2u8; 32]),
            handle: "kid".into(),
            features_ceiling: GatedFeature::ALL
                .iter()
                .map(|feature| crate::features::FeatureCeilingItem {
                    feature: *feature,
                    ceiling: effective_policy(*feature, &[], &[]),
                    extra: BTreeMap::new(),
                })
                .collect(),
            ..Default::default()
        };
        assert_round_trips(&readable);
        let bytes = encode_canonical(&readable).unwrap();
        let has = |key: &[u8]| bytes.windows(key.len()).any(|w| w == key);
        assert!(has(b"features_ceiling") && !has(b"features_unreadable"));

        assert_round_trips(&FamilyWardInfo {
            features_unreadable: true,
            ..readable
        });
    }

    /// Screen-time budget accounting wire (`family-safety.md` § Screen time):
    /// the heartbeat + reply round-trip, the offset field defaults to `0`
    /// (UTC) when omitted, and the additive `usage_today_minutes`
    /// status fields decode absent when the reply omits them.
    #[test]
    fn usage_report_wire_round_trips_and_degrades_additively() {
        assert_round_trips(&FamilyUsageReportRequest::default());
        assert_round_trips(&FamilyUsageReportRequest {
            minutes: 5,
            utc_offset_minutes: -720,
            ..Default::default()
        });
        assert_round_trips(&FamilyUsageReportReply {
            day: 20_650,
            day_total_minutes: 117,
            ..Default::default()
        });
        assert_round_trips(&FamilyNotifyReportRequest {
            entries: vec![FamilyContentNotice {
                category: "spam".into(),
                count: 3,
                extra: BTreeMap::new(),
            }],
            utc_offset_minutes: 840,
            ..Default::default()
        });
        assert_round_trips(&FamilyWardInfo {
            actor_id: ByteBuf::from(vec![2u8; 32]),
            usage_today_minutes: Some(0),
            ..Default::default()
        });
        assert_round_trips(&FamilyStatusReply {
            usage_today_minutes: Some(42),
            ..Default::default()
        });

        // A v1.x-early notify report (no offset field) decodes with the UTC
        // degrade; an empty usage heartbeat is a pure read.
        #[derive(Serialize)]
        struct Empty {}
        let bytes = encode_canonical(&Empty {}).unwrap();
        assert_eq!(
            decode::<FamilyNotifyReportRequest>(&bytes)
                .unwrap()
                .utc_offset_minutes,
            0
        );
        let req = decode::<FamilyUsageReportRequest>(&bytes).unwrap();
        assert_eq!(req.minutes, 0);
        assert_eq!(req.utc_offset_minutes, 0);
        // A status reply without the usage fields → None, not an error.
        let ward = decode::<FamilyWardInfo>(&encode_canonical(&FamilyWardInfo::default()).unwrap())
            .unwrap();
        assert_eq!(ward.usage_today_minutes, None);

        // v1.x child-initiated contact requests (family-safety.md § Child-
        // initiated contact requests): the ask + the status-reply pending list
        // round-trip; a reply without the list decodes to an empty list,
        // not an error.
        assert_round_trips(&FamilyContactRequestRequest {
            peer_actor_id: ByteBuf::from(vec![7u8; 32]),
            ..Default::default()
        });
        assert_round_trips(&FamilyStatusReply {
            contact_requests: vec![FamilyContactRequestInfo {
                peer_actor_id: ByteBuf::from(vec![7u8; 32]),
                peer_handle: "penpal".into(),
                created_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_round_trips(&FamilyApprovalsListReply {
            approvals: vec![FamilyApprovalEntry {
                supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                supervised_handle: "kid".into(),
                kind: "contact_request".into(),
                peer_actor_id: ByteBuf::from(vec![7u8; 32]),
                peer_handle: "penpal".into(),
                created_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
        // A status reply encoded before the field existed (simulated by the
        // default-encode, which skips nothing — so use the Empty shape) still
        // decodes: absent list → empty, absent peer_handle → empty string.
        let status = decode::<FamilyStatusReply>(&bytes).unwrap();
        assert!(status.contact_requests.is_empty());

        // v1.x feed-source approvals (family-safety.md § Feed-source approvals):
        // the ask, the queue row's (bridge_id, operation, target) key, the decide
        // request's matching key, and the status-reply list in both states.
        assert_round_trips(&FamilyFeedSourceRequestRequest {
            bridge_id: "bluesky".into(),
            operation: "follow".into(),
            target: "did:plc:abc123".into(),
            label: "Science Museum".into(),
            ..Default::default()
        });
        // A `link` ask carries an empty target — the shape must survive the wire
        // unchanged, since the grant matches the triple exactly.
        assert_round_trips(&FamilyFeedSourceRequestRequest {
            bridge_id: "nostr".into(),
            operation: "link".into(),
            target: String::new(),
            ..Default::default()
        });
        assert_round_trips(&FamilyApprovalsListReply {
            approvals: vec![FamilyApprovalEntry {
                supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                supervised_handle: "kid".into(),
                kind: "feed_source".into(),
                bridge_id: "bluesky".into(),
                operation: "feed".into(),
                target: "at://feed/science".into(),
                // The label rides `summary` for a feed_source row.
                summary: "Science feed".into(),
                created_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_round_trips(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            kind: "feed_source".into(),
            bridge_id: "bluesky".into(),
            operation: "feed".into(),
            target: "at://feed/science".into(),
            approve: true,
            ..Default::default()
        });
        // Both states round-trip: pending (`approved_at: None`) and granted.
        assert_round_trips(&FamilyStatusReply {
            feed_requests: vec![
                FamilyFeedRequestInfo {
                    bridge_id: "bluesky".into(),
                    operation: "follow".into(),
                    target: "did:plc:abc123".into(),
                    label: "Science Museum".into(),
                    created_at: 1_700_000_000,
                    approved_at: None,
                    ..Default::default()
                },
                FamilyFeedRequestInfo {
                    bridge_id: "nostr".into(),
                    operation: "link".into(),
                    target: String::new(),
                    created_at: 1_700_000_000,
                    approved_at: Some(1_700_000_500),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        // Absent keys: a status reply without the list decodes to empty (not an
        // error), and a queue row / decide request without
        // the key triple decodes (→ empty strings, which match no grant rather than
        // wildcarding one).
        let status = decode::<FamilyStatusReply>(&bytes).unwrap();
        assert!(status.feed_requests.is_empty());
        let entry = decode::<FamilyApprovalEntry>(
            &encode_canonical(&FamilyApprovalEntry::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(entry.bridge_id, "");
        assert_eq!(entry.operation, "");
        assert_eq!(entry.target, "");
        let decide = decode::<FamilyApprovalDecideRequest>(
            &encode_canonical(&FamilyApprovalDecideRequest::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(decide.bridge_id, "");
        assert_eq!(decide.operation, "");
        assert_eq!(decide.target, "");
    }

    #[test]
    fn reach_policy_v1x_pillars_round_trip_on_the_wire() {
        // The v1.x content + screen-time pillars ride inside the reach document;
        // set → decode identically on the real dag-cbor wire, inside a policy
        // update (both roles carry the same doc).
        let p = ReachPolicy {
            content_policy: Some(ContentPolicy {
                nsfw: ContentFloor::Block,
                spam: ContentFloor::Collapse,
                phishing: ContentFloor::Inherit,
                commercial: ContentFloor::Inherit,
            }),
            screen_time: Some(ScreenTimePolicy {
                window_start: Some(1260),
                window_end: Some(420), // wrapping (bedtime) window
                daily_minutes: Some(120),
            }),
            ..Default::default()
        };
        assert_round_trips(&p);
        assert_round_trips(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            policy: p,
            ..Default::default()
        });
    }

    #[test]
    fn v1_shaped_policy_leaves_v1x_pillars_absent() {
        // A reach-knobs-only wire carries just the four reach knobs. It MUST decode
        // with the v1.x pillars absent (None) so the handler leaves them unchanged
        // (§ Policy-update compatibility) — such a caller can never clobber a
        // content/screen policy it does not send.
        #[derive(Serialize)]
        struct V1Policy {
            contact_approval: bool,
            unknown_sender_mail: String,
            federation_contact: bool,
            feed_sources: String,
        }
        let bytes = encode_canonical(&V1Policy {
            contact_approval: true,
            unknown_sender_mail: "hold".into(),
            federation_contact: false,
            feed_sources: "block".into(),
        })
        .unwrap();
        let decoded: ReachPolicy = decode(&bytes).unwrap();
        assert!(decoded.contact_approval);
        assert_eq!(decoded.unknown_sender_mail, "hold");
        assert_eq!(decoded.feed_sources, "block");
        assert_eq!(decoded.content_policy, None);
        assert_eq!(decoded.screen_time, None);

        // Conversely, a None-pillar policy serializes WITHOUT the v1.x fields
        // (`skip_serializing_if`), so a reach-only policy's reply carries
        // no pillar field.
        let map: BTreeMap<String, Value> = decode(
            &encode_canonical(&ReachPolicy {
                contact_approval: true,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(map.contains_key("contact_approval"));
        assert!(!map.contains_key("content_policy"));
        assert!(!map.contains_key("screen_time"));
    }

    #[test]
    fn transfer_handshake_wire_round_trips() {
        assert_round_trips(&FamilyTransferAcceptRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            ..Default::default()
        });
        assert_round_trips(&FamilyTransferDeclineRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            ..Default::default()
        });
        assert_round_trips(&FamilyTransferCancelRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            ..Default::default()
        });
        // Status carrying the pending state on both sides.
        assert_round_trips(&FamilyStatusReply {
            wards: vec![FamilyWardInfo {
                actor_id: ByteBuf::from(vec![2u8; 32]),
                handle: "kid".into(),
                policy: ReachPolicy::default(),
                pending_transfer: Some(FamilyPendingTransferInfo {
                    proposed_guardian_actor_id: ByteBuf::from(vec![3u8; 32]),
                    proposed_guardian_handle: "otherparent".into(),
                    created_at: 1_700_000_000,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            incoming_transfers: vec![FamilyIncomingTransferInfo {
                supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                supervised_handle: "kid".into(),
                guardian_handle: "parent".into(),
                created_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
    }

    /// A status reply / ward entry without the transfer fields (absent on the
    /// wire); they must decode to the empty defaults (additive-everywhere,
    /// `version-compatibility.md`).
    #[test]
    fn status_decodes_pre_handshake_shape_without_transfer_fields() {
        #[derive(Serialize)]
        struct LegacyWard {
            actor_id: ByteBuf,
            handle: String,
            policy: ReachPolicy,
        }
        #[derive(Serialize)]
        struct LegacyReply {
            supervised_by: Option<FamilyGuardianInfo>,
            policy: Option<ReachPolicy>,
            wards: Vec<LegacyWard>,
        }
        let bytes = encode_canonical(&LegacyReply {
            supervised_by: None,
            policy: None,
            wards: vec![LegacyWard {
                actor_id: ByteBuf::from(vec![2u8; 32]),
                handle: "kid".into(),
                policy: ReachPolicy::default(),
            }],
        })
        .unwrap();
        let decoded: FamilyStatusReply = decode(&bytes).unwrap();
        assert!(decoded.wards[0].pending_transfer.is_none());
        assert!(decoded.incoming_transfers.is_empty());
    }

    #[test]
    fn approvals_wire_round_trips() {
        assert_round_trips(&FamilyApprovalsListRequest::default());
        assert_round_trips(&FamilyApprovalsListReply {
            approvals: vec![FamilyApprovalEntry {
                supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                supervised_handle: "kid".into(),
                kind: "contact".into(),
                peer_actor_id: ByteBuf::from(vec![7u8; 32]),
                summary: "hi".into(),
                created_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
        // A `mail_hold` entry: the peer is an address, not an actor, and the
        // message id is what `decide` names. `summary` stays empty — a subject
        // line is content.
        assert_round_trips(&FamilyApprovalsListReply {
            approvals: vec![FamilyApprovalEntry {
                supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
                supervised_handle: "kid".into(),
                kind: "mail_hold".into(),
                peer_actor_id: ByteBuf::from(Vec::new()),
                peer_address: "stranger@example.com".into(),
                message_id: ByteBuf::from(vec![9u8; 32]),
                summary: String::new(),
                created_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_round_trips(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            kind: "contact".into(),
            peer_actor_id: ByteBuf::from(vec![7u8; 32]),
            approve: true,
            ..Default::default()
        });
        assert_round_trips(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            kind: "mail_hold".into(),
            message_id: ByteBuf::from(vec![9u8; 32]),
            approve: false,
            ..Default::default()
        });
        assert_round_trips(&FamilyContactAddRequest {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            peer_actor_id: ByteBuf::from(vec![7u8; 32]),
            ..Default::default()
        });
    }

    /// A pre-`message_id` peer omits the field on a list entry; it must decode
    /// to the empty default rather than failing (additive-everywhere,
    /// `version-compatibility.md`).
    #[test]
    fn approval_entry_decodes_legacy_shape_without_message_id() {
        #[derive(Serialize)]
        struct Legacy {
            supervised_actor_id: ByteBuf,
            supervised_handle: String,
            kind: String,
            peer_actor_id: ByteBuf,
            summary: String,
            created_at: i64,
        }
        let bytes = crate::encode_canonical(&Legacy {
            supervised_actor_id: ByteBuf::from(vec![2u8; 32]),
            supervised_handle: "kid".into(),
            kind: "contact".into(),
            peer_actor_id: ByteBuf::from(vec![7u8; 32]),
            summary: "hi".into(),
            created_at: 1,
        })
        .unwrap();
        let decoded: FamilyApprovalEntry = crate::decode_strict(&bytes).unwrap();
        assert!(decoded.message_id.is_empty());
        assert_eq!(decoded.peer_address, "");
    }
}

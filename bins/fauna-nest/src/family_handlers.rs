//! WS-RPC handlers for the family-safety surface —
//! `fauna.family.{status,policy.update,graduate,transfer}` (slice 2) +
//! the reach-approval surface `fauna.family.{approvals.list,approvals.decide,
//! contact.add}` (slice 3) — `docs/goal/behavior/family-safety.md`
//! § Wire & data shape + § Reach approvals
//! (tracked internally).
//!
//! Authorization model: the caller class (`bridge_method_allowlist`) admits
//! `User | Admin`; the real check is **per-target against the `guardianships`
//! link table** — `policy.update` requires the caller to BE the ward's
//! guardian (deliberately not an admin power: no oversight attaches to the
//! admin role), while `graduate`/`transfer` accept the guardian OR the admin
//! (they only remove/re-point oversight, mirroring the admission-time
//! designation being an admin act). Every guardian mutation writes a
//! `family:*` audit row.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_core::data::{DmPeerVerdict, FeedSourceOperation, FeedSources, UnknownPeerDm};
use fauna_core::day_bucket::clamp_utc_offset;
use fauna_core::obligation::{ContentFloor, GUARDIAN_FLOOR_CATEGORIES};
use fauna_protocol::family::{
    FamilyAgeBandInfo, FamilyApprovalDecideRequest, FamilyApprovalEntry, FamilyApprovalsListReply,
    FamilyApprovalsListRequest, FamilyBlockedPeerInfo, FamilyContactAddRequest,
    FamilyContactRequestInfo, FamilyContactRequestRequest, FamilyContentNotice,
    FamilyDeviceMarkRequest, FamilyFeedRequestInfo, FamilyFeedSourceRequestRequest,
    FamilyGraduateRequest, FamilyGuardianInfo, FamilyIncomingTransferInfo,
    FamilyNotifyReportRequest, FamilyOkReply, FamilyPendingTransferInfo, FamilyPolicyUpdateRequest,
    FamilyStatusReply, FamilyStatusRequest, FamilyTransferAcceptRequest,
    FamilyTransferCancelRequest, FamilyTransferDeclineRequest, FamilyTransferRequest,
    FamilyUsageReportReply, FamilyUsageReportRequest, FamilyWardDeviceInfo, FamilyWardInfo,
    ReachPolicy,
};
use fauna_protocol::{ByteBuf, RpcError, Value, decode_strict as decode};

use fauna_mail::segments::placement::MailPlacementRecord;
use fauna_protocol::bridge_routing::{MailboxStateEvent, MoveSide};

use crate::bridge_imap_handlers::emit_mailbox_state_event;
use crate::bridge_method_allowlist::CallerClass;
// The feed-source ask caps its fields at exactly what the bridge operations it
// asks for accept — a tighter cap here would make a legal operation unaskable.
use crate::bridges_ui_handlers::{MAX_BRIDGE_LEN, MAX_FEED_URI_LEN, MAX_NAME_LEN};
use crate::db::bridge_imap::{GUARDIAN_HELD_MAILBOX, StoreFlagsDbOp};
use crate::db::family::GuardianPolicyRow;
use crate::db::now_epoch_secs;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── helpers ────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("family", reason)
}

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("family", reason)
}

fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("family", reason)
}

fn guardian_suspended() -> RpcError {
    let mut e = RpcError::new(
        "fauna.family.guardian_suspended",
        "error.family.guardian_suspended",
    );
    e.details = Some(Box::new(Value::String(
        "a suspended guardian cannot exercise oversight — ask the admin to restore the account or transfer guardianship"
            .into(),
    )));
    e
}

/// Refuse a suspended *acting* guardian. Mirrors `check_guardian_admissible`'s
/// candidate-side rule so suspension strips family authority for as long as it
/// lasts (`family-safety.md` § Lifecycle gates). The admin arm of
/// `require_guardian_or_admin` bypasses this — the admin is how a suspended
/// guardian's links get resolved.
async fn deny_if_suspended(state: &Arc<AppState>, caller: &[u8; 32]) -> Result<(), RpcError> {
    // Fail-closed on a missing `users` row: a guardian must be a real user.
    let suspended = state
        .db
        .actor_suspended(caller.as_slice())
        .await
        .map_err(internal)?
        .unwrap_or(true);
    if suspended {
        return Err(guardian_suspended());
    }
    Ok(())
}

/// Validate the guardian tier's `features` sub-document and re-encode it for
/// storage (`dynamic-features.md` § Wire & data shape).
///
/// `None` in, `None` out — absent means unchanged, exactly like the v1.x
/// pillars, and here the stake is higher than for any of them: this tier is
/// nest-enforced, so silently clearing it would *lift* a restriction rather than
/// merely fail to render one.
///
/// Two things are checked, and deliberately only two:
///
/// 1. **Each policy is legal for the feature it names** — `FeaturePolicy::validate`
///    rejects a bound on a dimension the registry does not declare for that
///    feature (§ The quota grammar: such a bound is unrepresentable).
/// 2. **Nothing else.** In particular there is no "may a guardian relax past the
///    admin?" check, because the meet makes that **unrepresentable rather than
///    forbidden-by-rule** (`dynamic-features.md:111`): every tier can only
///    tighten, so a permissive guardian document simply loses the MIN. Adding a
///    validation rule here would be a second answer to a question the
///    composition already answers, free to drift from it.
///
/// An entry naming a feature this build does not know is **kept, not rejected**:
/// it is a newer nest's guardian document round-tripping through this one
/// (additive-everywhere), and it binds nothing here because the meet only ever
/// asks for features the registry declares.
fn validate_feature_sub_document(
    features: Option<&fauna_protocol::features::GuardianFeaturePolicies>,
) -> Result<Option<Vec<u8>>, RpcError> {
    let Some(documents) = features else {
        return Ok(None);
    };
    for (key, policy) in documents {
        let Some(feature) = fauna_core::feature_gate::GatedFeature::from_key(key) else {
            continue;
        };
        policy
            .validate(fauna_core::feature_gate::entry(feature))
            .map_err(|e| invalid_params(&e.to_string()))?;
    }
    let encoded = fauna_protocol::encode_canonical(documents)
        .map_err(|e| internal(anyhow::anyhow!("encode guardian feature sub-document: {e}")))?;
    Ok(Some(encoded.to_vec()))
}

/// The `{ ok: true }` reply shared by the family mutations (and the
/// notify-report no-op exits).
fn family_ok() -> Result<Bytes, RpcError> {
    encode_reply(&FamilyOkReply {
        ok: true,
        extra: Default::default(),
    })
}

async fn require_class(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<CallerClass, RpcError> {
    crate::bridge_method_allowlist::require_permission(&state.db, actor_id, kind, internal).await
}

fn parse_actor(b: &ByteBuf, field: &str) -> Result<[u8; 32], RpcError> {
    crate::rpc_errors::require_bytes32(field, b.as_slice()).map_err(|e| invalid_params(&e))
}

/// The ward's current *local*-day bucket (`family-safety.md` § Screen time —
/// the day-bucket rule, ratified 2026-07-16; adopted by § Guardian Notify):
/// this nest's own clock plus the client's clamped reported offset.
///
/// The rule itself — the clamp range and the bucket arithmetic — lives in
/// [`fauna_core::day_bucket`], because the controversial-class feature gate
/// accounts usage in the very same buckets (`dynamic-features.md` § Usage
/// accounting); the two planes must agree exactly on where a day starts, so
/// neither owns it. This wrapper only binds that rule to the one clock a
/// handler has.
fn local_day_bucket(utc_offset_minutes: i32) -> i64 {
    fauna_core::day_bucket::local_day_bucket(now_epoch_secs(), utc_offset_minutes)
}

/// What a family doorbell row says. The sentences name no one and nothing —
/// "the notification is the doorbell; the status read is the truth"
/// (`family-safety.md` § Guardian Notify) — so the only data is the content
/// notice's coarse category, carried for an app that wants it and unused by
/// the catalog sentence (`behavior/notifications.md` § Localized body).
fn doorbell_text(key: &str, category: Option<&str>) -> crate::db::notifications::NotificationText {
    let mut body = fauna_protocol::LocalizedText::new(key);
    if let Some(category) = category {
        body = body.with_arg("category", category);
    }
    crate::db::notifications::NotificationText::localized(body)
}

/// The doorbell notification type Guardian Notify writes to the guardian's
/// notification feed (`family-safety.md` § Guardian Notify — "the notification
/// is the doorbell; the status read is the truth").
const NOTIFY_NOTIF_TYPE: fauna_protocol::notifications::NotifType =
    fauna_protocol::notifications::NotifType::FamilyContentNotice;

/// The per-report clamp on a single category delta, applied before it reaches
/// the DB accumulate (`family-safety.md` § Guardian Notify — counts clamped). A
/// conforming client never reports a per-hour-batch category count near this;
/// the clamp only bounds a hostile or buggy client's single report.
const MAX_NOTIFY_DELTA_PER_REPORT: u32 = 100_000;

/// The per-report clamp on a usage heartbeat's minutes delta (`family-safety.md`
/// § Screen time — clamped per report). One day is the honest ceiling on a
/// single device's catch-up report; a conforming heartbeat cadence reports a
/// few minutes at a time.
///
/// Shared with the client heartbeat that produces the delta
/// ([`fauna_core::screen_time::UsageHeartbeat`]), so a conforming client never
/// sends a number this side silently truncates — the two halves of one rule
/// cannot drift apart.
use fauna_core::screen_time::MAX_USAGE_MINUTES_PER_REPORT as MAX_USAGE_DELTA_PER_REPORT;

/// The guardian's stored feature sub-document as **what it enforces** — itself
/// when it decodes, and an explicit deny of every registry member when it does
/// not (`dynamic-features.md` § Fail posture — the undecodable-document clause).
///
/// The substitute names every member rather than a subset because the failure is
/// document-wide: nothing can be recovered from bytes that do not parse, so
/// which features the guardian actually bounded is precisely what is unknown.
/// Denying all of them is the only reading that cannot under-state the
/// restriction, and it is what `feature_policies_for` enforces for each feature
/// it is asked about.
///
/// The second value is **whether the substitution happened** — the
/// `features_unreadable` flag a guardian-side ward entry carries, so an editor
/// never seeds from the deny (`family-safety.md` § Wire & data shape, the
/// guardian's feature-limits editor seed). One decode, two outputs: a second
/// decode beside this one would be a second place for the fold to disagree.
fn enforced_feature_view(
    bytes: &[u8],
) -> (fauna_protocol::features::GuardianFeaturePolicies, bool) {
    match fauna_protocol::decode_strict::<fauna_protocol::features::GuardianFeaturePolicies>(bytes)
    {
        Ok(documents) => (documents, false),
        Err(e) => {
            tracing::error!(error = %e, "undecodable guardian feature sub-document on status read — reporting the deny it is enforced as");
            let denied = fauna_core::feature_gate::registry()
                .iter()
                .map(|entry| {
                    (
                        entry.feature.as_str().to_string(),
                        fauna_core::feature_gate::FeaturePolicy::DENIED,
                    )
                })
                .collect();
            (denied, true)
        }
    }
}

fn policy_row_to_wire(row: &GuardianPolicyRow) -> ReachPolicy {
    policy_row_to_wire_flagged(row).0
}

/// [`policy_row_to_wire`] plus whether the stored feature sub-document was
/// unreadable — the guardian-side ward entry's `features_unreadable`.
fn policy_row_to_wire_flagged(row: &GuardianPolicyRow) -> (ReachPolicy, bool) {
    let (features, features_unreadable) = match row.features_document.as_ref() {
        Some(bytes) => {
            let (view, unreadable) = enforced_feature_view(bytes);
            (Some(view), unreadable)
        }
        None => (None, false),
    };
    let policy = ReachPolicy {
        contact_approval: row.contact_approval,
        unknown_sender_mail: row.unknown_sender_mail.clone(),
        federation_contact: row.federation_contact,
        feed_sources: row.feed_sources.clone(),
        // The v1.x pillars, folded from the additive columns — `None` when a
        // pillar is at its unsupervised-equivalent default, so `status` reports
        // to both roles exactly what is enforced (family-safety.md § Content
        // policy / § Screen time; § invariant 4 — supervision is transparent).
        content_policy: row.content_policy(),
        screen_time: row.screen_time(),
        // Guardian Notify knob: `Some(true)` when on, `None` when off — off is
        // the unsupervised-equivalent default, so an off policy round-trips to a
        // v1-shaped wire (`skip_serializing_if`), and reading it back never
        // clobbers the knob on a subsequent v1-client save (§ Guardian Notify).
        content_notify: row.content_notify.then_some(true),
        // The bridge-DM gate's knob: `None` when at its `allow` default — the
        // same unsupervised-equivalent-round-trips-to-a-v1-shape reasoning as
        // `content_notify` above. A value only a *newer* nest could have
        // written is reported verbatim rather than hidden, so an older client
        // fails it closed to `hold` on render (never `allow`, which would show
        // a policy weaker than the one actually enforced).
        unknown_peer_dm: (row.unknown_peer_dm != UnknownPeerDm::Allow.as_str())
            .then(|| row.unknown_peer_dm.clone()),
        // The guardian's feature limits, shown to BOTH roles
        // (`family-safety.md` § The trust shape invariant 4, restated at
        // `dynamic-features.md:168`: "a guardian-tier feature limit is visible
        // to the ward exactly as reach/content policy is"). `None` when the
        // guardian expressed no feature opinion, so such a policy round-trips
        // without a `features` field.
        //
        // An undecodable document reports as the **deny it is enforced as**
        // (`dynamic-features.md` § Fail posture — the undecodable-document
        // clause), never as an absence and never by failing the whole `status`
        // read. The read's job is to show what is enforced: the gate treats an
        // unreadable guardian document as a deny at the guardian tier
        // (`db::feature_gate::feature_policies_for`), so reporting "the guardian
        // set no feature limits" here would be the silent gate inverted — a read
        // promising a plane the gate refuses. One unreadable sub-document still
        // must not cost the ward the four reach knobs, which is why this is a
        // per-field substitution rather than a failed read.
        features,
        extra: Default::default(),
    };
    (policy, features_unreadable)
}

/// Closed-enum validation of the wire policy values (`family.rs` doc).
fn validate_policy(p: &ReachPolicy) -> Result<(), RpcError> {
    if !matches!(p.unknown_sender_mail.as_str(), "allow" | "hold" | "reject") {
        return Err(invalid_params(
            "unknown_sender_mail must be one of allow|hold|reject",
        ));
    }
    if !matches!(p.feed_sources.as_str(), "allow" | "block") {
        return Err(invalid_params("feed_sources must be one of allow|block"));
    }
    // A *present* unknown_peer_dm must name a value this gate can honor — the
    // nest refuses what it cannot name, exactly as it does for the two v1 string
    // knobs. An **absent** one means "leave the knob unchanged" (§ Policy-update
    // compatibility), so there is nothing to validate: that is what lets a
    // v1-era client save the four reach knobs without relaxing a gate it cannot
    // render. Note this refusal is what keeps an unnameable value out of the
    // store on the *write* path — `supervised_dm_verdict` still fails closed on
    // the read path, for a value a newer nest wrote before a rollback.
    if let Some(v) = &p.unknown_peer_dm
        && !matches!(v.as_str(), "allow" | "hold")
    {
        return Err(invalid_params("unknown_peer_dm must be one of allow|hold"));
    }
    // A *present* content sub-document must name only floors the nest can store
    // (family-safety.md § Policy-update compatibility:97 — "the nest refuses
    // what it cannot name"). A floor value the caller's build cannot parse
    // decodes to `ContentFloor::Unknown`; refusing it at write keeps an
    // unnameable value out of the store. An **absent** sub-document is left
    // unchanged and carries nothing to validate.
    if let Some(cp) = &p.content_policy {
        for (category, floor) in [
            ("nsfw", cp.nsfw),
            ("spam", cp.spam),
            ("phishing", cp.phishing),
            ("commercial", cp.commercial),
        ] {
            if floor == ContentFloor::Unknown {
                return Err(invalid_params(&format!(
                    "content_policy.{category} is not a recognized floor \
                     (expected inherit|collapse|block)"
                )));
            }
        }
    }
    // A *present* screen_time sub-document is range/shape-checked by the shared
    // rule on the policy type itself (family-safety.md § Screen time — window
    // semantics + write validation, ratified 2026-07-16: bounds in 0..1440,
    // both-or-neither, no ambiguous empty window, budget ≤ a day). The nest
    // refuses what it cannot honestly store; clients pre-validate with the
    // same fn. An absent sub-document is left unchanged, nothing to validate.
    if let Some(screen) = &p.screen_time
        && let Err(why) = screen.validate()
    {
        return Err(invalid_params(&format!("screen_time: {why}")));
    }
    Ok(())
}

/// The caller must be the ward's guardian; returns the link's guardian id.
/// `not_found` when the target is not supervised at all — except for the
/// ward themself probing, which stays `permission_denied` (they can see they
/// are supervised via `status`; they hold no lifecycle power either way).
async fn require_guardian_of(
    state: &Arc<AppState>,
    caller: &[u8; 32],
    ward: &[u8; 32],
) -> Result<(), RpcError> {
    let link = state
        .db
        .get_guardian_of(ward)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found("account is not supervised"))?;
    if link.guardian_actor_id != caller.as_slice() {
        return Err(permission_denied("caller is not this account's guardian"));
    }
    deny_if_suspended(state, caller).await?;
    Ok(())
}

// ── fauna.family.status ────────────────────────────────────────────────

fn status_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.status").await?;
            let _req: FamilyStatusRequest = decode(&payload).map_err(malformed)?;

            // Supervised side.
            let (supervised_by, policy, own_usage_today) = match state
                .db
                .get_guardian_of(&actor_id)
                .await
                .map_err(internal)?
            {
                Some(link) => {
                    let handle = match link.guardian_actor_id.as_slice().try_into() {
                        Ok(id) => state
                            .db
                            .get_handle(&id)
                            .await
                            .map_err(internal)?
                            .unwrap_or_default(),
                        Err(_) => String::new(),
                    };
                    let row = state
                        .db
                        .get_guardian_policy(&actor_id)
                        .await
                        .map_err(internal)?;
                    // The ward's own screen-time readout — the same number the
                    // guardian sees (family-safety.md § Screen time,
                    // transparency). Only when a daily budget is set; "today"
                    // derives from the link's last-reported offset.
                    let own_usage_today = match row.as_ref().and_then(|r| r.screen_daily_minutes) {
                        Some(_) => Some(
                            state
                                .db
                                .get_guardian_usage(
                                    &actor_id,
                                    local_day_bucket(link.ward_utc_offset_minutes),
                                )
                                .await
                                .map_err(internal)?,
                        ),
                        None => None,
                    };
                    let policy = row.as_ref().map(policy_row_to_wire);
                    (
                        Some(FamilyGuardianInfo {
                            actor_id: ByteBuf::from(link.guardian_actor_id),
                            handle,
                            extra: Default::default(),
                        }),
                        policy,
                        own_usage_today,
                    )
                }
                None => (None, None, None),
            };

            // Guardian side.
            let mut wards = Vec::new();
            for link in state.db.list_wards(&actor_id).await.map_err(internal)? {
                // A link row whose ward id is not an actor id names no ward:
                // it is skipped, as the other walk of `list_wards` in this
                // file skips it — never answered as an entry with no handle
                // and no ceiling, which every host would have to special-case.
                let id: [u8; 32] = match link.supervised_actor_id.as_slice().try_into() {
                    Ok(id) => id,
                    Err(_) => {
                        tracing::warn!(
                            len = link.supervised_actor_id.len(),
                            "guardian link row with a malformed supervised_actor_id — skipped on status read"
                        );
                        continue;
                    }
                };
                let handle = state
                    .db
                    .get_handle(&id)
                    .await
                    .map_err(internal)?
                    .unwrap_or_default();
                let (policy, features_unreadable) = state
                    .db
                    .get_guardian_policy(&id)
                    .await
                    .map_err(internal)?
                    .as_ref()
                    .map(policy_row_to_wire_flagged)
                    .unwrap_or_default();
                // The guardian's feature-limits editor seed
                // (family-safety.md § Wire & data shape): the meet of
                // the tiers outside the guardian's, composed FOR THE
                // WARD by the nest's one resolver — the same ceiling
                // the two authored-document reads carry for their
                // tiers, so the subset edge rides it here too.
                let mut features_ceiling =
                    Vec::with_capacity(fauna_core::feature_gate::registry().len());
                for entry in fauna_core::feature_gate::registry() {
                    let ceiling = crate::feature_gate::resolve_effective_policy(
                        &state,
                        &id,
                        entry.feature,
                        crate::feature_gate::TierScope::OutsideOf(
                            fauna_core::feature_gate::RuleTier::Guardian,
                        ),
                    )
                    .await
                    .map_err(internal)?;
                    features_ceiling.push(fauna_protocol::features::FeatureCeilingItem {
                        feature: entry.feature,
                        ceiling,
                        extra: Default::default(),
                    });
                }
                // The outstanding transfer proposal, if any — the initiating
                // side's pending view (family-safety.md § Graduation &
                // transfer).
                let pending_transfer = match state
                    .db
                    .get_pending_transfer(&link.supervised_actor_id)
                    .await
                    .map_err(internal)?
                {
                    Some(p) => {
                        let handle = match p.proposed_guardian_actor_id.as_slice().try_into() {
                            Ok(id) => {
                                let id: [u8; 32] = id;
                                state
                                    .db
                                    .get_handle(&id)
                                    .await
                                    .map_err(internal)?
                                    .unwrap_or_default()
                            }
                            Err(_) => String::new(),
                        };
                        Some(FamilyPendingTransferInfo {
                            proposed_guardian_actor_id: ByteBuf::from(p.proposed_guardian_actor_id),
                            proposed_guardian_handle: handle,
                            created_at: p.created_at,
                            extra: Default::default(),
                        })
                    }
                    None => None,
                };
                // "The ward's today", derived from the link's last-reported
                // clamped offset (family-safety.md § Screen time — the
                // day-bucket rule; 0 = UTC until a report lands).
                let ward_today = local_day_bucket(link.ward_utc_offset_minutes);
                // Guardian Notify (family-safety.md § Guardian Notify): the
                // ward's coarse per-category enforcement counts for the current
                // day. The doorbell notification is what pings the guardian; this
                // status field is the truth their Family surface renders. Carries
                // category + count only — never any content identifier.
                let content_notices = state
                    .db
                    .list_content_notices_for_day(&link.supervised_actor_id, ward_today)
                    .await
                    .map_err(internal)?
                    .into_iter()
                    .map(|(category, count)| FamilyContentNotice {
                        category,
                        count,
                        extra: Default::default(),
                    })
                    .collect();
                // Screen time (family-safety.md § Screen time): the ward's
                // cross-device foreground total for their current local day —
                // only when the policy sets a daily budget (no accounting
                // without a declared policy).
                let usage_today_minutes =
                    match policy.screen_time.as_ref().and_then(|s| s.daily_minutes) {
                        Some(_) => Some(
                            state
                                .db
                                .get_guardian_usage(&link.supervised_actor_id, ward_today)
                                .await
                                .map_err(internal)?,
                        ),
                        None => None,
                    };
                // Slice F device list (family-safety.md § Full visibility for
                // young children): the ward's registered devices in a slim
                // projection so the guardian's Family page renders the per-device
                // mark toggle from this one status read. Guardianship-guarded by
                // construction — we are inside the guardian-side `wards` loop,
                // each `link` a guardianship the caller holds, so a device list
                // is exposed only for a ward the caller genuinely guards (the
                // same gate `fauna.family.device.mark` re-checks per target). The
                // `device_id` is the same hex spelling the ward's own
                // `fauna.sync.devices.list` renders and `device.mark` consumes.
                //
                // `label` is the DISPLAY IDENTITY, not the ward's label (ruled
                // 2026-08-02, family-safety.md § Full visibility): the ward's
                // user-chosen label rests sealed under the ward's own root,
                // which the guardian neither holds nor may be handed (no
                // guardian key escrow), so the projection renders the device's
                // code — with the machine-authored plaintext label beside it
                // when one rests — never an empty row the guardian's mark
                // control cannot tell apart.
                //
                // ⚠ Chosen over the WHOLE list at once, never per row. The
                // doc guarantees these rows are *per-device distinct*, and
                // `device_id` is client-chosen: a ward sees the guardian's
                // enrolled device in their own `devices.list` and may register
                // a decoy sharing any fixed-width prefix of it, so only a
                // set-relative width is unforgeable. Marking the wrong row
                // leaves the real device removable while this page reads
                // correct, which is why this is the mark control's own
                // integrity property and not a cosmetic choice.
                let rows = state
                    .db
                    .list_devices_for_actor(&link.supervised_actor_id)
                    .await
                    .map_err(internal)?;
                let identities = fauna_core::format::device_display_identities(
                    rows.iter().map(|d| (d.label.as_str(), &d.device_id[..])),
                );
                let devices = rows
                    .iter()
                    .zip(identities)
                    .map(|(d, label)| FamilyWardDeviceInfo {
                        device_id: hex::encode(&d.device_id),
                        label,
                        guardian_marked: d.guardian_marked,
                        extra: Default::default(),
                    })
                    .collect();
                // The ward's established band (`family-safety.md` § The
                // account age band) — `None` when the admission named no band (a
                // guardian code with no band and no attested claim).
                let age_band = state
                    .db
                    .get_age_band(&link.supervised_actor_id)
                    .await
                    .map_err(internal)?
                    .map(|(band, provenance)| FamilyAgeBandInfo {
                        band,
                        provenance,
                        extra: Default::default(),
                    });
                // The peers this guardian has DENIED for the ward — the
                // `block` verdicts only (`family-safety.md` § The bridge-DM
                // gate → *The un-deny surface*). Without this read a deny is a
                // one-way door in the UI: `approvals_decide { kind: "dm_hold",
                // approve: true }` has always been able to flip it back
                // (idempotent, not queue-scoped), but nothing named the peer to
                // flip. `allow` rows are deliberately NOT carried — an allowed
                // peer is simply un-held, and listing them would invite a
                // surface that reads as a roster the guardian must curate.
                //
                // Inside the same guardianship-guarded loop as `devices`, so
                // the same gate covers it: only a linked guardian ever sees a
                // ward's denied set.
                let blocked_dm_peers = state
                    .db
                    .list_dm_peer_verdicts(&link.supervised_actor_id)
                    .await
                    .map_err(internal)?
                    .into_iter()
                    // Parsed through the shared `DmPeerVerdict` rather than
                    // compared to a literal: an unnameable stored verdict is
                    // then simply not `Block` and is left out, the same
                    // fail-quiet the gate itself takes (`from_wire` is
                    // deliberately not degraded — `data.rs`).
                    .filter(|(_, _, verdict)| {
                        fauna_core::data::DmPeerVerdict::from_wire(verdict)
                            == Some(fauna_core::data::DmPeerVerdict::Block)
                    })
                    .map(|(bridge_id, peer_id, _)| FamilyBlockedPeerInfo {
                        bridge_id,
                        peer_id,
                        extra: Default::default(),
                    })
                    .collect();
                wards.push(FamilyWardInfo {
                    actor_id: ByteBuf::from(link.supervised_actor_id),
                    handle,
                    policy,
                    pending_transfer,
                    content_notices,
                    usage_today_minutes,
                    devices,
                    age_band,
                    blocked_dm_peers,
                    features_unreadable,
                    features_ceiling,
                    extra: Default::default(),
                });
            }

            // Proposals awaiting the CALLER's consent as proposed guardian —
            // the incoming-transfer prompt's read. Renders the ward and their
            // *current* guardian (the initiator may have been the admin).
            let mut incoming_transfers = Vec::new();
            for p in state
                .db
                .list_incoming_transfers(&actor_id)
                .await
                .map_err(internal)?
            {
                let supervised_handle = match p.supervised_actor_id.as_slice().try_into() {
                    Ok(id) => {
                        let id: [u8; 32] = id;
                        state
                            .db
                            .get_handle(&id)
                            .await
                            .map_err(internal)?
                            .unwrap_or_default()
                    }
                    Err(_) => String::new(),
                };
                let guardian_handle = match state
                    .db
                    .get_guardian_of(&p.supervised_actor_id)
                    .await
                    .map_err(internal)?
                {
                    Some(link) => match link.guardian_actor_id.as_slice().try_into() {
                        Ok(id) => {
                            let id: [u8; 32] = id;
                            state
                                .db
                                .get_handle(&id)
                                .await
                                .map_err(internal)?
                                .unwrap_or_default()
                        }
                        Err(_) => String::new(),
                    },
                    None => String::new(),
                };
                incoming_transfers.push(FamilyIncomingTransferInfo {
                    supervised_actor_id: ByteBuf::from(p.supervised_actor_id),
                    supervised_handle,
                    guardian_handle,
                    created_at: p.created_at,
                    extra: Default::default(),
                });
            }

            // The supervised caller's own pending contact asks (v1.x,
            // family-safety.md § Child-initiated contact requests) — what lets
            // the refused-send surface render "asked — waiting" (transparency).
            let mut contact_requests = Vec::new();
            if supervised_by.is_some() {
                for ask in state
                    .db
                    .list_contact_requests(&actor_id)
                    .await
                    .map_err(internal)?
                {
                    let peer_handle = match ask.peer_actor_id.as_slice().try_into() {
                        Ok(id) => {
                            let id: [u8; 32] = id;
                            state
                                .db
                                .get_handle(&id)
                                .await
                                .map_err(internal)?
                                .unwrap_or_default()
                        }
                        Err(_) => String::new(),
                    };
                    contact_requests.push(FamilyContactRequestInfo {
                        peer_actor_id: ByteBuf::from(ask.peer_actor_id),
                        peer_handle,
                        created_at: ask.created_at,
                        extra: Default::default(),
                    });
                }
            }

            // The supervised caller's own feed-source asks — pending *and*
            // granted-but-unredeemed (v1.x, family-safety.md § Feed-source
            // approvals), so the blocked bridges surface renders the ask/approved
            // state in place. Unlike the guardian's queue this is not knob-gated:
            // a live grant minted before the guardian relaxed `feed_sources` is
            // simply inert, and the ward is better served seeing their own
            // outstanding asks than having them vanish on a knob flip.
            let mut feed_requests = Vec::new();
            if supervised_by.is_some() {
                for ask in state
                    .db
                    .list_feed_requests(&actor_id, false)
                    .await
                    .map_err(internal)?
                {
                    feed_requests.push(FamilyFeedRequestInfo {
                        bridge_id: ask.bridge_id,
                        operation: ask.operation,
                        target: ask.target,
                        label: ask.label,
                        created_at: ask.created_at,
                        approved_at: ask.approved_at,
                        extra: Default::default(),
                    });
                }
            }

            // The caller's OWN established band (`family-safety.md` § The
            // account age band — the account always sees its own band, the
            // same transparency rule as its policy). `None` for the common
            // cases: no row (an unsupervised account is `18+`/`none` by
            // construction) or an admission that named no band (a guardian
            // code with no band and no attested claim).
            let own_age_band = state
                .db
                .get_age_band(&actor_id)
                .await
                .map_err(internal)?
                .map(|(band, provenance)| FamilyAgeBandInfo {
                    band,
                    provenance,
                    extra: Default::default(),
                });

            encode_reply(&FamilyStatusReply {
                supervised_by,
                policy,
                wards,
                incoming_transfers,
                usage_today_minutes: own_usage_today,
                contact_requests,
                feed_requests,
                age_band: own_age_band,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.policy.update ─────────────────────────────────────────

fn policy_update_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.policy.update").await?;
            let req: FamilyPolicyUpdateRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            // Guardian-only — deliberately NOT an admin power (family-safety.md
            // § The trust shape invariant 1). not_found only for a genuinely
            // unsupervised target reached by its would-be guardian; anyone
            // else sees permission_denied first.
            require_guardian_of(&state, &actor_id, &ward).await?;
            validate_policy(&req.policy)?;
            let features = validate_feature_sub_document(req.policy.features.as_ref())?;

            let updated = state
                .db
                .update_guardian_policy(
                    &ward,
                    req.policy.contact_approval,
                    &req.policy.unknown_sender_mail,
                    req.policy.federation_contact,
                    &req.policy.feed_sources,
                    // Absent (`None`) pillars leave their columns unchanged
                    // (§ Policy-update compatibility); present ones replace.
                    req.policy.content_policy.as_ref(),
                    req.policy.screen_time.as_ref(),
                    req.policy.content_notify,
                    req.policy.unknown_peer_dm.as_deref(),
                    features.as_deref(),
                )
                .await
                .map_err(internal)?;
            if !updated {
                return Err(not_found("account is not supervised"));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:policy.update",
                    Some(&hex::encode(ward)),
                    Some(&format!(
                        "contact_approval={} unknown_sender_mail={} federation_contact={} feed_sources={} features={}",
                        req.policy.contact_approval,
                        req.policy.unknown_sender_mail,
                        req.policy.federation_contact,
                        req.policy.feed_sources,
                        // § Transparency & auditability: "every policy write
                        // (admin, guardian, self) writes an audit row". The
                        // guardian's feature write rides this row rather than
                        // minting its own, the same way its kind does. Names
                        // which features were bound, never their bounds — the
                        // row is a record that a write happened, and the bounds
                        // themselves are readable by the person they bind
                        // through `fauna.features.status`.
                        match req.policy.features.as_ref() {
                            None => "unchanged".to_string(),
                            Some(f) if f.is_empty() => "cleared".to_string(),
                            Some(f) => f.keys().cloned().collect::<Vec<_>>().join(","),
                        }
                    )),
                )
                .await;
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.notify_report ─────────────────────────────────────────

/// The `insert_notification` `content_id` discriminator for a Guardian-Notify
/// doorbell: the `(day, category)` bucket, so the row-dedup on
/// `(actor, notif_type, sender_id, content_id)` yields exactly **one** doorbell
/// per (guardian, ward, day, category). It is a dedup token, **not** a content
/// identifier — it names no post or message, only the coarse bucket, so nothing
/// the § forbids ("never a content id") crosses the wire.
fn notice_dedup_key(day: i64, category: &str) -> Vec<u8> {
    format!("{day}:{category}").into_bytes()
}

/// The supervised account's coarse per-category enforcement report
/// (`family-safety.md` § Guardian Notify). Verifies the caller is a supervised
/// account with the guardian's `content_notify` on, then for each recognized
/// category with a positive (clamped) delta accumulates the day's count (guarded
/// upsert, **no content ids**) and rings the guardian's doorbell once per
/// (ward, day, category). Every exit replies `ok` — a no-op is still a
/// successful call, and the reply never discloses whether the caller is
/// supervised or the knob is on (best-effort telemetry, § trust bound).
fn notify_report_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.notify_report").await?;
            let req: FamilyNotifyReportRequest = decode(&payload).map_err(malformed)?;

            // Supervised-caller gate + knob gate — each a silent `ok` no-op.
            let Some(link) = state
                .db
                .get_guardian_of(&actor_id)
                .await
                .map_err(internal)?
            else {
                return family_ok();
            };
            let notify_on = state
                .db
                .get_guardian_policy(&actor_id)
                .await
                .map_err(internal)?
                .map(|p| p.content_notify)
                .unwrap_or(false);
            if !notify_on {
                return family_ok();
            }
            let Ok(guardian) = <[u8; 32]>::try_from(link.guardian_actor_id.as_slice()) else {
                return family_ok();
            };

            // The reported local day (family-safety.md § Screen time — the
            // day-bucket rule, adopted by § Guardian Notify). Remember the
            // clamped offset on the link so the guardian's status read derives
            // the same "today".
            let offset = clamp_utc_offset(req.utc_offset_minutes);
            let day = local_day_bucket(offset);
            state
                .db
                .set_ward_utc_offset(&actor_id, offset)
                .await
                .map_err(internal)?;
            // Micros — the notifications column unit; seconds sort into 1970.
            let now = fauna_core::data::Timestamp::now().as_i64();
            // Fold the report to recognized categories only (an older nest must
            // degrade gracefully on a newer client's category — additive-safe),
            // summing any duplicate entries and clamping each per-report delta.
            let mut deltas: std::collections::BTreeMap<&'static str, u32> = Default::default();
            for entry in &req.entries {
                let Some(&canon) = GUARDIAN_FLOOR_CATEGORIES
                    .iter()
                    .find(|c| **c == entry.category)
                else {
                    continue;
                };
                let clamped = entry.count.min(MAX_NOTIFY_DELTA_PER_REPORT);
                if clamped == 0 {
                    continue;
                }
                let slot = deltas.entry(canon).or_default();
                *slot = slot
                    .saturating_add(clamped)
                    .min(MAX_NOTIFY_DELTA_PER_REPORT);
            }

            for (category, delta) in deltas {
                // Accumulate the coarse day count (guarded — a no-op for an
                // unsupervised account even though we already checked the link).
                state
                    .db
                    .upsert_content_notice(&actor_id, day, category, delta)
                    .await
                    .map_err(internal)?;
                // Doorbell: one per (guardian, ward, day, category). The
                // day+category bucket rides `content_id`, so
                // `insert_notification`'s built-in dedup returns `None` on a
                // repeat report the same day and never re-rings.
                let bucket = notice_dedup_key(day, category);
                let text = doorbell_text("notifications.row_family_content_notice", Some(category));
                if let Some(notif_id) = state
                    .db
                    .insert_notification(
                        &guardian,
                        &NOTIFY_NOTIF_TYPE,
                        "fauna",
                        Some(&actor_id[..]),
                        Some(&bucket),
                        None,
                        &text,
                        now,
                    )
                    .await
                    .map_err(internal)?
                {
                    state.ws.notify_push(
                        &guardian,
                        fauna_protocol::PushEvent::Notification(
                            fauna_protocol::push_events::NotificationPayload {
                                notification_id: notif_id,
                                notif_type: NOTIFY_NOTIF_TYPE,
                                source: "fauna".into(),
                                sender_id: Some(hex::encode(actor_id)),
                                content_id: Some(hex::encode(&bucket)),
                                summary: text.summary().to_string(),
                                body: text.body().cloned(),
                                timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                                extra: std::collections::BTreeMap::new(),
                            },
                        ),
                    );
                }
            }
            family_ok()
        })
    })
}

// ── fauna.family.usage_report ──────────────────────────────────────────

/// The supervised account's coarse foreground-use heartbeat for the daily
/// screen-time budget (`family-safety.md` § Screen time). Verifies the caller
/// is a supervised account whose policy sets a daily budget, then accumulates
/// the clamped minutes delta into the reported local day's cross-device total
/// (guarded upsert) and replies with `{ day, day_total_minutes }` — the number
/// the enforcing client locks on. A zero-minute report is a read (the
/// unlock-screen check). **Every exit replies the same shape** (a zero total)
/// — best-effort telemetry that discloses nothing by shape, like
/// `notify_report`; with no budget set there is no accounting at all.
fn usage_report_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.usage_report").await?;
            let req: FamilyUsageReportRequest = decode(&payload).map_err(malformed)?;

            let offset = clamp_utc_offset(req.utc_offset_minutes);
            let day = local_day_bucket(offset);
            let silent_zero = move || {
                encode_reply(&FamilyUsageReportReply {
                    day,
                    day_total_minutes: 0,
                    extra: Default::default(),
                })
            };

            // Supervised-caller gate + budget gate — each a silent zero reply
            // (no accounting without a declared budget).
            if state
                .db
                .get_guardian_of(&actor_id)
                .await
                .map_err(internal)?
                .is_none()
            {
                return silent_zero();
            }
            let budget_set = state
                .db
                .get_guardian_policy(&actor_id)
                .await
                .map_err(internal)?
                .is_some_and(|p| p.screen_daily_minutes.is_some());
            if !budget_set {
                return silent_zero();
            }

            // Remember the clamped offset on the link (the status reads derive
            // "the ward's today" from it), then accumulate and return the
            // day's cross-device total.
            state
                .db
                .set_ward_utc_offset(&actor_id, offset)
                .await
                .map_err(internal)?;
            let delta = req.minutes.min(MAX_USAGE_DELTA_PER_REPORT);
            let total = state
                .db
                .upsert_guardian_usage(&actor_id, day, delta)
                .await
                .map_err(internal)?;
            encode_reply(&FamilyUsageReportReply {
                day,
                day_total_minutes: total,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.device.mark ───────────────────────────────────────────

/// Set/clear the guardian-enrolled-device marker (`family-safety.md` § Full
/// visibility). The guardian enrolls one of their own devices into the child's
/// account by ordinary multi-device enrollment — it authenticates *as the
/// child*, so without this flag the nest cannot tell it from the child's own
/// device, and two promises are unenforceable: the child cannot unilaterally
/// remove it (§ The trust shape), and graduation auto-revokes it.
///
/// **Guardian-only, per target.** Not an admin power (the trust shape attaches
/// no oversight to that role — the same call `policy.update` makes), and
/// emphatically not the ward's: a child who could clear the mark could delete
/// the device, which is exactly what the marker prevents. Zero new
/// cryptography — one flag on the device row, rendered back in the child's own
/// device list.
fn device_mark_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.device.mark").await?;
            let req: FamilyDeviceMarkRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            require_guardian_of(&state, &actor_id, &ward).await?;

            let device_id = fauna_core::hex32::decode(&req.device_id)
                .map_err(|_| invalid_params("device_id is not 32-byte hex"))?;

            // Scoped to the ward's own devices by the UPDATE's `actor_id`
            // clause, so a guardian cannot reach across accounts.
            if !state
                .db
                .set_device_guardian_mark(&ward, &device_id, req.marked)
                .await
                .map_err(internal)?
            {
                return Err(not_found("device not found for this account"));
            }

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    if req.marked {
                        "family:device.mark"
                    } else {
                        "family:device.unmark"
                    },
                    Some(&hex::encode(ward)),
                    Some(&req.device_id),
                )
                .await;
            family_ok()
        })
    })
}

// ── mail-hold resolution (shared by approvals.decide and graduate) ─────

/// The RFC 9051 system flag `apply_expunge` requires before it will remove a
/// placement row.
const DELETED_FLAG: &str = r"\Deleted";

/// **Release** one held message into the ward's INBOX — the guardian's approve,
/// and the sweep graduation runs before dropping the link
/// (`family-safety.md` § Reach approvals → *"Graduation releases, never
/// drops"*).
///
/// A real IMAP move (`apply_move`), not an `UPDATE … SET mailbox`: UIDs are
/// per-mailbox, so the message needs a fresh INBOX UID, a source-side tombstone,
/// both modseq bumps, a `Move` placement-journal record and the IDLE/NOTIFY push
/// its MUA is waiting on — exactly what `fauna.bridges.move_messages` emits.
///
/// Idempotent by construction: a message no longer in the held mailbox (already
/// released, or expunged by the ward's own MUA) skips the move and still clears
/// the sidecar, so a retry — or a graduation replayed after a crash — never
/// errors and never double-places.
async fn release_mail_hold(
    state: &Arc<AppState>,
    ward: &[u8; 32],
    message_id: &[u8; 32],
) -> Result<(), RpcError> {
    if let Some(uid) = state
        .db
        .find_message_uid(ward, GUARDIAN_HELD_MAILBOX, message_id)
        .await
        .map_err(internal)?
    {
        let outcome = state
            .db
            .apply_move(ward, GUARDIAN_HELD_MAILBOX, &[uid], "INBOX")
            .await
            .map_err(internal)?;
        if !outcome.moved.is_empty() {
            let (src_uid_set, dst_uid_set): (Vec<u32>, Vec<u32>) =
                outcome.moved.iter().copied().unzip();
            let record = MailPlacementRecord::Move {
                src_mailbox: GUARDIAN_HELD_MAILBOX.to_string(),
                src_uid_set,
                dst_mailbox: "INBOX".to_string(),
                dst_uid_set,
                modseq_src: outcome.source_highestmodseq as u64,
                modseq_dst: outcome.dest_highestmodseq as u64,
                deleted_at: outcome.moved_at,
            };
            state
                .mail_placement
                .append_event(ward, &record)
                .await
                .map_err(internal)?;
            for (src_uid, dst_uid) in &outcome.moved {
                let event = |side| MailboxStateEvent::Move {
                    src_uid: *src_uid,
                    dst_uid: *dst_uid,
                    modseq_src: outcome.source_highestmodseq,
                    modseq_dst: outcome.dest_highestmodseq,
                    side,
                };
                emit_mailbox_state_event(
                    state,
                    ward,
                    GUARDIAN_HELD_MAILBOX,
                    event(MoveSide::Source),
                );
                emit_mailbox_state_event(state, ward, "INBOX", event(MoveSide::Destination));
            }
        }
    }
    state
        .db
        .delete_mail_hold(ward, message_id)
        .await
        .map_err(internal)?;
    Ok(())
}

/// **Discard** one held message — the guardian's deny (`family-safety.md`
/// § Reach approvals: *"approve = move to INBOX + allowlist the sender; deny =
/// discard"*).
///
/// Exactly what an MUA does to delete a message: set `\Deleted`, then EXPUNGE.
/// Both steps are required — `apply_expunge` removes only `\Deleted`-flagged
/// rows (RFC 9051 § 6.4.3), so expunging a held message straight off would
/// silently no-op and leave it in the mailbox with its sidecar gone. The ward's
/// MUA sees the same STORE + EXPUNGE wire events any other deletion produces,
/// and the `bridge_imap_expunged` tombstone keeps QRESYNC honest.
///
/// Idempotent, as [`release_mail_hold`].
///
/// This is not a no-user-data-loss violation: the message never entered the
/// ward's INBOX, and the discard is the guardian's deliberate act on their own
/// ward's queue — the mail equivalent of `approvals.decide{approve:false}`
/// blocking a knock.
async fn discard_mail_hold(
    state: &Arc<AppState>,
    ward: &[u8; 32],
    message_id: &[u8; 32],
) -> Result<(), RpcError> {
    if let Some(uid) = state
        .db
        .find_message_uid(ward, GUARDIAN_HELD_MAILBOX, message_id)
        .await
        .map_err(internal)?
    {
        let deleted = vec![DELETED_FLAG.to_string()];
        let stored = state
            .db
            .apply_store_flags(
                ward,
                GUARDIAN_HELD_MAILBOX,
                &[uid],
                StoreFlagsDbOp::Add,
                &deleted,
                None,
            )
            .await
            .map_err(internal)?;
        for (uid, before, after, modseq) in &stored.updated {
            let record = MailPlacementRecord::StoreFlags {
                mailbox: GUARDIAN_HELD_MAILBOX.to_string(),
                uid_set: vec![*uid],
                modseq: *modseq as u64,
                before_flags: before.split_whitespace().map(String::from).collect(),
                after_flags: after.split_whitespace().map(String::from).collect(),
            };
            state
                .mail_placement
                .append_event(ward, &record)
                .await
                .map_err(internal)?;
        }

        // No exclusions: the guardian's own discard outranks the handler-layer
        // hold gate (which never runs here — this is a direct CacheDb call).
        let now = now_epoch_secs();
        let outcome = state
            .db
            .apply_expunge(ward, GUARDIAN_HELD_MAILBOX, &[uid], &[], now)
            .await
            .map_err(internal)?;
        if !outcome.expunged_uids.is_empty() {
            let record = MailPlacementRecord::Expunge {
                mailbox: GUARDIAN_HELD_MAILBOX.to_string(),
                uid_set: outcome.expunged_uids.clone(),
                modseq: outcome.highestmodseq as u64,
                deleted_at: now,
            };
            state
                .mail_placement
                .append_event(ward, &record)
                .await
                .map_err(internal)?;
            for uid in &outcome.expunged_uids {
                emit_mailbox_state_event(
                    state,
                    ward,
                    GUARDIAN_HELD_MAILBOX,
                    MailboxStateEvent::Expunge {
                        uid: *uid,
                        modseq: outcome.highestmodseq,
                    },
                );
            }
        }
    }
    state
        .db
        .delete_mail_hold(ward, message_id)
        .await
        .map_err(internal)?;
    Ok(())
}

// ── fauna.family.graduate ──────────────────────────────────────────────

/// Guardian-or-admin gate shared by `graduate`/`transfer` — the two
/// lifecycle transitions that only remove/re-point oversight.
async fn require_guardian_or_admin(
    state: &Arc<AppState>,
    caller: &[u8; 32],
    class: CallerClass,
    ward: &[u8; 32],
) -> Result<(), RpcError> {
    let link = state
        .db
        .get_guardian_of(ward)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found("account is not supervised"))?;
    if matches!(class, CallerClass::Admin) {
        // The admin arm is how a suspended guardian's links get resolved
        // (`family-safety.md` § Lifecycle gates) — never suspension-gated.
        return Ok(());
    }
    if link.guardian_actor_id != caller.as_slice() {
        return Err(permission_denied(
            "caller is neither this account's guardian nor the admin",
        ));
    }
    deny_if_suspended(state, caller).await?;
    Ok(())
}

fn graduate_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.family.graduate").await?;
            let req: FamilyGraduateRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            require_guardian_or_admin(&state, &actor_id, class, &ward).await?;

            // "Graduation releases, never drops" (family-safety.md § Reach
            // approvals). Every still-held message goes to INBOX *before* the
            // link is dropped: the now-full account keeps its mail, and the
            // release/graduate order is the crash-safe one — a crash between
            // them leaves the mail delivered and the link intact, and the
            // replay is a no-op. `CacheDb::graduate` re-asserts `held == 0` as
            // a fail-closed tripwire against a caller that skips this loop.
            for hold in state.db.list_mail_holds(&ward).await.map_err(internal)? {
                let message_id: [u8; 32] = hold
                    .message_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| internal("held message_id is not 32 bytes"))?;
                release_mail_hold(&state, &ward, &message_id).await?;
            }

            // "Graduation revokes a marked guardian device" (family-safety.md
            // § Graduation; § Full visibility rule b). Same release-first shape
            // as the holds above, and for the same crash-safety reason: a crash
            // between revoke and drop leaves the device revoked and the link
            // intact, and the replay is a no-op. It must happen HERE rather than
            // inside the graduation transaction because revocation also drops
            // the device's live connection, which a DB transaction cannot reach;
            // `CacheDb::graduate` re-asserts `marked == 0` as the fail-closed
            // tripwire against a caller that skips this loop.
            //
            // Deliberately not sole-source-gated (unlike the ward's own
            // `devices.delete`): graduation must always be completable without
            // waiting on anyone (§ Lifecycle gates — it is what makes guardian
            // eviction resolvable), so a guardian device that happens to be a
            // folder's only source must not be able to block it.
            for device_id in state
                .db
                .list_marked_devices(&ward)
                .await
                .map_err(internal)?
            {
                let (_, _, revoked_key) = state
                    .db
                    .delete_device(&device_id, &ward)
                    .await
                    .map_err(internal)?;
                // A guardian device authenticates AS the ward, so graduation
                // severs its minted sessions and closes their sockets exactly
                // as the ward's own `devices.delete` would (the delete already
                // tombstoned the grant in the same transaction).
                if let Some(key) = revoked_key {
                    state.revoke_device_authority(&ward, &key).await;
                }
            }

            // One transaction: link + policy gone; account/keys/handle/data/
            // tier untouched (family-safety.md § Graduation & transfer).
            let graduated = state.db.graduate(&ward).await.map_err(internal)?;
            if !graduated {
                return Err(not_found("account is not supervised"));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:graduate",
                    Some(&hex::encode(ward)),
                    None,
                )
                .await;
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.transfer ──────────────────────────────────────────────

/// The proposed guardian must pass the same admissibility validation as at
/// admission (existing, non-supervised, non-suspended), plus ≠ the ward.
/// Shared by the proposal and the accept-time re-validation.
async fn require_transfer_admissible(
    state: &Arc<AppState>,
    ward: &[u8; 32],
    candidate: &[u8; 32],
) -> Result<(), RpcError> {
    if candidate == ward {
        return Err(invalid_params("the ward cannot be their own guardian"));
    }
    match state
        .db
        .check_guardian_admissible(candidate)
        .await
        .map_err(internal)?
    {
        Ok(()) => Ok(()),
        Err("not_found") => Err(not_found("new guardian actor not found")),
        Err(reason) => Err(invalid_params(&format!("new guardian actor is {reason}"))),
    }
}

fn transfer_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.family.transfer").await?;
            let req: FamilyTransferRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            let new_guardian = parse_actor(&req.new_guardian_actor_id, "new_guardian_actor_id")?;
            require_guardian_or_admin(&state, &actor_id, class, &ward).await?;
            require_transfer_admissible(&state, &ward, &new_guardian).await?;
            let link = state
                .db
                .get_guardian_of(&ward)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("account is not supervised"))?;
            if link.guardian_actor_id == new_guardian.as_slice() {
                return Err(invalid_params(
                    "the proposed guardian already guards this account",
                ));
            }

            // Guardianship is a duty, so the transfer is a consent handshake
            // (family-safety.md § Graduation & transfer): record a pending
            // proposal the target must accept — the link is untouched until
            // then. The one immediate case is the caller proposing THEMSELF
            // (an admin taking over): initiating a transfer to yourself is
            // consenting, so no pending state is needed.
            if new_guardian == actor_id {
                // Withdraw any other target's proposal FIRST, then re-point —
                // the crash-safe order: after either single write the state is
                // valid (no-pending + old link, or new link), whereas the
                // reverse order could strand a stale proposal that a third
                // party could still accept against the new link.
                let _ = state
                    .db
                    .cancel_pending_transfer(&ward)
                    .await
                    .map_err(internal)?;
                let repointed = state
                    .db
                    .transfer_guardian(&ward, &new_guardian)
                    .await
                    .map_err(internal)?;
                if !repointed {
                    return Err(not_found("account is not supervised"));
                }
                let _ = state
                    .db
                    .audit(
                        Some(&actor_id[..]),
                        "family:transfer.accept",
                        Some(&hex::encode(ward)),
                        Some("self-proposal — consent by construction"),
                    )
                    .await;
            } else {
                state
                    .db
                    .upsert_pending_transfer(&ward, &new_guardian, &actor_id)
                    .await
                    .map_err(internal)?;
                let _ = state
                    .db
                    .audit(
                        Some(&actor_id[..]),
                        "family:transfer",
                        Some(&hex::encode(ward)),
                        Some(&format!("proposed_guardian={}", hex::encode(new_guardian))),
                    )
                    .await;
            }
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.transfer.accept / .decline / .cancel ─────────────────

fn transfer_accept_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.transfer.accept").await?;
            let req: FamilyTransferAcceptRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            // Re-validate admissibility at accept time — the world may have
            // changed since the proposal (the target suspended, the ward
            // graduated). The db accept is keyed on (ward, caller), so only
            // the proposed guardian's consent can complete the re-point.
            require_transfer_admissible(&state, &ward, &actor_id).await?;
            let accepted = state
                .db
                .accept_pending_transfer(&ward, &actor_id)
                .await
                .map_err(internal)?;
            if !accepted {
                return Err(not_found(
                    "no pending transfer for this account awaits your consent",
                ));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:transfer.accept",
                    Some(&hex::encode(ward)),
                    None,
                )
                .await;
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn transfer_decline_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.transfer.decline").await?;
            let req: FamilyTransferDeclineRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            // Refusing a duty needs no admissibility — the delete is keyed on
            // (ward, caller), so only the named target can decline.
            let declined = state
                .db
                .decline_pending_transfer(&ward, &actor_id)
                .await
                .map_err(internal)?;
            if !declined {
                return Err(not_found(
                    "no pending transfer for this account awaits your consent",
                ));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:transfer.decline",
                    Some(&hex::encode(ward)),
                    None,
                )
                .await;
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

fn transfer_cancel_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_class(&state, &actor_id, "fauna.family.transfer.cancel").await?;
            let req: FamilyTransferCancelRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            require_guardian_or_admin(&state, &actor_id, class, &ward).await?;
            let cancelled = state
                .db
                .cancel_pending_transfer(&ward)
                .await
                .map_err(internal)?;
            if !cancelled {
                return Err(not_found("no pending transfer for this account"));
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:transfer.cancel",
                    Some(&hex::encode(ward)),
                    None,
                )
                .await;
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.approvals.list ────────────────────────────────────────

/// The ward's **held** DM conversations, as `(bridge_id, peer_id, created_at)`
/// (`family-safety.md` § The bridge-DM gate) — one queue entry per held
/// conversation peer.
///
/// **Computed, never read from a hold table**: a conversation is held iff the
/// knob says `hold` and its peer carries no verdict row. That is the whole
/// design — there is no stored hold state, so relaxing the knob releases every
/// held conversation by construction, with no drainage rule and nothing
/// strandable (§ Don't do these — *"don't store bridge-DM hold state"*).
///
/// Every inbound bridge-DM write path lands in the bridged-conversation family
/// — a third-party bridge's deposit and the in-process Nostr leg's alike — so
/// the queue reads every bridged room that holds a row, each far participant a
/// peer (§ Enforcement points' standing rule, met by construction).
/// `created_at` is the room's newest arrival, unix seconds.
async fn held_dm_conversations(
    state: &AppState,
    ward: &[u8; 32],
) -> Result<Vec<(String, String, i64)>, RpcError> {
    let verdicts: std::collections::HashSet<(String, String)> = state
        .db
        .list_dm_peer_verdicts(ward)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|(bridge, peer, _)| (bridge, peer))
        .collect();
    let rooms = state
        .db
        .summarize_bridged_rooms(ward, None)
        .await
        .map_err(internal)?;
    Ok(rooms
        .into_iter()
        .flat_map(|room| {
            let at = room.last_received_at / 1000;
            let bridge_id = room.bridge_id;
            room.participants
                .into_iter()
                .map(move |peer| (bridge_id.clone(), peer, at))
        })
        .filter(|(bridge, peer, _)| !verdicts.contains(&(bridge.clone(), peer.clone())))
        .collect())
}

/// The guardian's reach-approval queue (family-safety.md § Reach approvals):
/// contact approvals are the wards' pending knocks, read guardian-side; mail
/// holds are the messages sitting in a ward's held mailbox, read off their
/// envelope sidecar — no new hold store for either.
///
/// The two kinds gate differently on the policy, on purpose:
///
/// - **`contact`** entries appear only while `contact_approval` is on, because
///   the knob is what *moves* acceptance authority to the guardian. With it off
///   the ward reviews their own knocks and the guardian must not shadow them.
/// - **`mail_hold`** entries appear whenever a hold row exists, *whatever the
///   knob currently says*. The knob governs whether new mail is held; it must
///   never govern whether already-held mail can be drained. Flipping
///   `unknown_sender_mail` back to `allow` with messages still held would
///   otherwise strand them in a mailbox no queue entry could release.
fn approvals_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.approvals.list").await?;
            let _req: FamilyApprovalsListRequest = decode(&payload).map_err(malformed)?;

            let mut approvals = Vec::new();
            for link in state.db.list_wards(&actor_id).await.map_err(internal)? {
                let ward: [u8; 32] = match link.supervised_actor_id.as_slice().try_into() {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let handle = state
                    .db
                    .get_handle(&ward)
                    .await
                    .map_err(internal)?
                    .unwrap_or_default();

                // One policy read serves both knob gates below — the kinds gate
                // on different knobs (§ Reach approvals), not on different
                // policies.
                let policy = state
                    .db
                    .get_guardian_policy(&ward)
                    .await
                    .map_err(internal)?;
                let routed_to_guardian =
                    policy.as_ref().map(|p| p.contact_approval).unwrap_or(false);
                let feed_sources_blocked = policy
                    .as_ref()
                    .map(|p| FeedSources::from_wire(&p.feed_sources) == FeedSources::Block)
                    .unwrap_or(false);
                let dm_held = policy
                    .as_ref()
                    .map(|p| UnknownPeerDm::from_wire(&p.unknown_peer_dm) == UnknownPeerDm::Hold)
                    .unwrap_or(false);
                if routed_to_guardian {
                    for knock in state.db.poll_knocks(&ward).await.map_err(internal)? {
                        approvals.push(FamilyApprovalEntry {
                            supervised_actor_id: ByteBuf::from(ward.to_vec()),
                            supervised_handle: handle.clone(),
                            kind: "contact".into(),
                            peer_actor_id: ByteBuf::from(knock.sender_id.to_vec()),
                            // A contact knock's peer is an actor, not an address.
                            peer_address: String::new(),
                            message_id: ByteBuf::from(Vec::new()),
                            summary: knock.summary,
                            peer_handle: String::new(),
                            // Not a feed_source item — it names no bridge object.
                            bridge_id: String::new(),
                            operation: String::new(),
                            target: String::new(),
                            created_at: knock.created_at,
                            extra: Default::default(),
                        });
                    }
                    // The ward's own pending contact asks (v1.x,
                    // family-safety.md § Child-initiated contact requests) —
                    // same knob gate as `contact`: with approval off the ward
                    // contacts freely and a pending ask is moot.
                    for ask in state
                        .db
                        .list_contact_requests(&ward)
                        .await
                        .map_err(internal)?
                    {
                        let peer_handle = match ask.peer_actor_id.as_slice().try_into() {
                            Ok(id) => {
                                let id: [u8; 32] = id;
                                state
                                    .db
                                    .get_handle(&id)
                                    .await
                                    .map_err(internal)?
                                    .unwrap_or_default()
                            }
                            Err(_) => String::new(),
                        };
                        approvals.push(FamilyApprovalEntry {
                            supervised_actor_id: ByteBuf::from(ward.to_vec()),
                            supervised_handle: handle.clone(),
                            kind: "contact_request".into(),
                            peer_actor_id: ByteBuf::from(ask.peer_actor_id),
                            peer_address: String::new(),
                            message_id: ByteBuf::from(Vec::new()),
                            // An ask carries no message text, deliberately —
                            // identified by *who*, never *why*.
                            summary: String::new(),
                            peer_handle,
                            // Not a feed_source item — it names no bridge object.
                            bridge_id: String::new(),
                            operation: String::new(),
                            target: String::new(),
                            created_at: ask.created_at,
                            extra: Default::default(),
                        });
                    }
                }

                // The ward's pending feed-source asks (v1.x, family-safety.md
                // § Feed-source approvals). Gated on the knob that refused the
                // operation in the first place: with `feed_sources` back to
                // `allow` the ward adds sources freely, so a pending ask is
                // moot — the § Reach approvals rule, and the same shape the
                // `contact` kinds follow above.
                //
                // `pending_only`: an approved-but-unredeemed grant is waiting on
                // the *ward's* retry, not on any decision of the guardian's, so
                // it leaves the queue the moment it is granted.
                if feed_sources_blocked {
                    for ask in state
                        .db
                        .list_feed_requests(&ward, true)
                        .await
                        .map_err(internal)?
                    {
                        approvals.push(FamilyApprovalEntry {
                            supervised_actor_id: ByteBuf::from(ward.to_vec()),
                            supervised_handle: handle.clone(),
                            kind: "feed_source".into(),
                            // A bridge object has no actor and no address.
                            peer_actor_id: ByteBuf::from(Vec::new()),
                            peer_address: String::new(),
                            message_id: ByteBuf::from(Vec::new()),
                            // The ward's own label for the thing being approved
                            // — their naming of it, not third-party content.
                            summary: ask.label,
                            peer_handle: String::new(),
                            bridge_id: ask.bridge_id,
                            operation: ask.operation,
                            target: ask.target,
                            created_at: ask.created_at,
                            extra: Default::default(),
                        });
                    }
                }

                // The ward's held DM conversations (v1.x, family-safety.md § The
                // bridge-DM gate). Gated on `unknown_peer_dm = hold` — and,
                // unlike `mail_hold` below, this needs **no** drainage rule: a
                // DM hold is a computed placement, not stored state, so relaxing
                // the knob releases every held conversation by construction and
                // nothing is left strandable (§ Reach approvals).
                //
                // Envelope-class metadata only: the peer's external id, never the
                // message, which is sealed to the ward.
                if dm_held {
                    for (bridge_id, peer_id, created_at) in
                        held_dm_conversations(&state, &ward).await?
                    {
                        approvals.push(FamilyApprovalEntry {
                            supervised_actor_id: ByteBuf::from(ward.to_vec()),
                            supervised_handle: handle.clone(),
                            kind: "dm_hold".into(),
                            // An external bridge peer has no actor on this nest —
                            // which is exactly why `contact_approval` cannot see
                            // it and this gate exists. Its id rides the existing
                            // `peer_address` field (§ Reach approvals).
                            peer_actor_id: ByteBuf::from(Vec::new()),
                            peer_address: peer_id,
                            message_id: ByteBuf::from(Vec::new()),
                            // The message is sealed to the ward; a preview would
                            // be content the guardian must never see.
                            summary: String::new(),
                            peer_handle: String::new(),
                            bridge_id,
                            // Not a feed_source item — it names no bridge object.
                            operation: String::new(),
                            target: String::new(),
                            created_at,
                            extra: Default::default(),
                        });
                    }
                }

                // Envelope metadata only. `summary` stays empty because a
                // subject line is content, and the message is sealed to the ward
                // — the nest could not read it even if the design allowed
                // (family-safety.md § The mail gate → "What the nest may see").
                for hold in state.db.list_mail_holds(&ward).await.map_err(internal)? {
                    approvals.push(FamilyApprovalEntry {
                        supervised_actor_id: ByteBuf::from(ward.to_vec()),
                        supervised_handle: handle.clone(),
                        kind: "mail_hold".into(),
                        // A mail sender has no actor on this nest.
                        peer_actor_id: ByteBuf::from(Vec::new()),
                        peer_address: hold.sender_address,
                        message_id: ByteBuf::from(hold.message_id),
                        summary: String::new(),
                        peer_handle: String::new(),
                        // Not a feed_source item — it names no bridge object.
                        bridge_id: String::new(),
                        operation: String::new(),
                        target: String::new(),
                        created_at: hold.created_at,
                        extra: Default::default(),
                    });
                }
            }
            encode_reply(&FamilyApprovalsListReply {
                approvals,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.approvals.decide ──────────────────────────────────────

fn approvals_decide_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.approvals.decide").await?;
            let req: FamilyApprovalDecideRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            require_guardian_of(&state, &actor_id, &ward).await?;

            // Each kind names its item with a different key, so `peer_actor_id`
            // is parsed only on the arm that carries one.
            let audit_detail = match req.kind.as_str() {
                "contact" => {
                    let peer = parse_actor(&req.peer_actor_id, "peer_actor_id")?;
                    // The same accept/block paths the ward's own knock review
                    // would run (accepted edge + knock dismissed / blocked edge
                    // + knock dismissed; a block trains nothing).
                    if req.approve {
                        crate::contacts_handlers::accept_contact_core(&state, &ward, &peer)
                            .await
                            .map_err(|e| internal(e.message))?;
                    } else {
                        crate::contacts_handlers::block_contact_core(&state, &ward, &peer)
                            .await
                            .map_err(|e| internal(e.message))?;
                    }
                    format!(
                        "kind=contact peer={} approve={}",
                        hex::encode(peer),
                        req.approve
                    )
                }
                "mail_hold" => {
                    let message_id: [u8; 32] =
                        crate::rpc_errors::require_bytes32("message_id", req.message_id.as_ref())
                            .map_err(|e| invalid_params(&e))?;
                    // Ward-scoped lookup: a guardian may only decide on holds
                    // belonging to a ward they actually guard, and only on a
                    // message that is really held — never on a guessed id.
                    let hold = state
                        .db
                        .get_mail_hold(&ward, &message_id)
                        .await
                        .map_err(internal)?
                        .ok_or_else(|| not_found("no such held message for this ward"))?;
                    if req.approve {
                        // Release, then allowlist: the sender becomes known, so
                        // their *next* message flows straight to INBOX. Keyed on
                        // this message id alone — approving one held message
                        // never sweeps the sender's other held messages, but it
                        // does stop new ones being held.
                        release_mail_hold(&state, &ward, &message_id).await?;
                        state
                            .db
                            .add_mail_allowlist_entry(&ward, &hold.sender_address, "guardian")
                            .await
                            .map_err(internal)?;
                    } else {
                        discard_mail_hold(&state, &ward, &message_id).await?;
                    }
                    format!(
                        "kind=mail_hold message={} approve={}",
                        hex::encode(message_id),
                        req.approve
                    )
                }
                // v1.x (family-safety.md § Child-initiated contact requests):
                // approve mints the same accepted edge `contact.add` would;
                // deny drops the ask and deliberately does NOT block the named
                // peer — a request-deny refuses the child's question, never
                // punishes the third party it names.
                "contact_request" => {
                    let peer = parse_actor(&req.peer_actor_id, "peer_actor_id")?;
                    // Ward-scoped lookup, mirroring the mail_hold arm: only a
                    // live ask for a ward this guardian guards is decidable —
                    // never a guessed peer.
                    let live = state
                        .db
                        .list_contact_requests(&ward)
                        .await
                        .map_err(internal)?
                        .iter()
                        .any(|a| a.peer_actor_id.as_slice() == peer.as_slice());
                    if !live {
                        return Err(not_found("no such pending contact request for this ward"));
                    }
                    // Accept before dropping the row — the crash-safe order: a
                    // crash between the two leaves the ask pending and the
                    // replayed approve re-runs an idempotent accept upsert; the
                    // reverse order would lose the guardian's decision.
                    if req.approve {
                        crate::contacts_handlers::accept_contact_core(&state, &ward, &peer)
                            .await
                            .map_err(|e| internal(e.message))?;
                    }
                    state
                        .db
                        .delete_contact_request(&ward, &peer)
                        .await
                        .map_err(internal)?;
                    format!(
                        "kind=contact_request peer={} approve={}",
                        hex::encode(peer),
                        req.approve
                    )
                }
                // v1.x (family-safety.md § Feed-source approvals): approve mints
                // a single-use grant the ward redeems by retrying — the nest
                // never replays the operation, since a bridge link is
                // interactive and a staged replay would run it as the ward hours
                // later. Deny drops the ask.
                "feed_source" => {
                    let Some(operation) = FeedSourceOperation::from_wire(&req.operation) else {
                        return Err(invalid_params(
                            "operation must be one of: link, follow, feed",
                        ));
                    };
                    // Each of these is ward-scoped, live-scoped and
                    // pending-scoped in one statement, so the lookup and the act
                    // cannot drift apart: a guessed key, an expired ask, or one
                    // already decided simply matches nothing.
                    if req.approve {
                        let Some(row_id) = state
                            .db
                            .approve_feed_request(
                                &ward,
                                &req.bridge_id,
                                operation.as_str(),
                                &req.target,
                            )
                            .await
                            .map_err(internal)?
                        else {
                            return Err(not_found(
                                "no such pending feed-source request for this ward",
                            ));
                        };
                        // Ring the WARD, not the guardian: unlike a contact
                        // approve — whose accepted edge the ward's own client
                        // observes — a grant produces no organically visible
                        // state, so without this doorbell the ward could only
                        // poll to learn they may retry.
                        ring_feed_source_approved(&state, &ward, row_id).await?;
                    } else if !state
                        .db
                        .delete_feed_request(&ward, &req.bridge_id, operation.as_str(), &req.target)
                        .await
                        .map_err(internal)?
                    {
                        return Err(not_found(
                            "no such pending feed-source request for this ward",
                        ));
                    }
                    format!(
                        "kind=feed_source bridge={} op={} target={} approve={}",
                        req.bridge_id,
                        operation.as_str(),
                        req.target,
                        req.approve
                    )
                }
                // v1.x (family-safety.md § The bridge-DM gate): the decision IS
                // the verdict row — approve writes `allow` (the conversation
                // releases and future DMs deliver), deny writes `block` (new
                // arrivals are refused before storage; already-stored rows stay
                // the ward's to read, marked).
                //
                // Both arms are one guarded upsert, so there is no pending row to
                // race, drop, or leave half-decided: the hold was never state, so
                // deciding it does not *transition* anything — it records a fact
                // the computed placement then reads. A re-decide is idempotent,
                // and a guardian may reverse their own earlier call.
                "dm_hold" => {
                    if req.bridge_id.is_empty() || req.peer_address.is_empty() {
                        return Err(invalid_params(
                            "dm_hold requires bridge_id and peer_address",
                        ));
                    }
                    let verdict = if req.approve {
                        DmPeerVerdict::Allow
                    } else {
                        DmPeerVerdict::Block
                    };
                    // Guardianship-guarded at the write, so a ward this guardian
                    // does not guard (already refused above by
                    // `require_guardian_of`) or an unsupervised one writes
                    // nothing and reads as `not_found` — never a silent ok that
                    // would tell the guardian a decision landed when it did not.
                    if !state
                        .db
                        .set_dm_peer_verdict(&ward, &req.bridge_id, &req.peer_address, verdict)
                        .await
                        .map_err(internal)?
                    {
                        return Err(not_found("account is not supervised"));
                    }
                    format!(
                        "kind=dm_hold bridge={} peer={} approve={}",
                        req.bridge_id, req.peer_address, req.approve
                    )
                }
                other => {
                    return Err(invalid_params(&format!(
                        "kind must be \"contact\", \"mail_hold\", \"contact_request\", \"feed_source\", or \"dm_hold\", got {other:?}"
                    )));
                }
            };
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:approvals.decide",
                    Some(&hex::encode(ward)),
                    Some(&audit_detail),
                )
                .await;
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.contact.add ───────────────────────────────────────────

fn contact_add_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.contact.add").await?;
            let req: FamilyContactAddRequest = decode(&payload).map_err(malformed)?;
            let ward = parse_actor(&req.supervised_actor_id, "supervised_actor_id")?;
            let peer = parse_actor(&req.peer_actor_id, "peer_actor_id")?;
            require_guardian_of(&state, &actor_id, &ward).await?;
            if peer == ward {
                return Err(invalid_params("a contact must be another actor"));
            }

            // Pre-approve: the accepted edge lets the ward both receive from
            // and initiate to the peer under contact_approval; a matching
            // pending knock (if any) is dismissed by the same accept path.
            crate::contacts_handlers::accept_contact_core(&state, &ward, &peer)
                .await
                .map_err(|e| internal(e.message))?;
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:contact.add",
                    Some(&hex::encode(ward)),
                    Some(&format!("peer={}", hex::encode(peer))),
                )
                .await;
            encode_reply(&FamilyOkReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.family.contact.request ───────────────────────────────────────

/// The `insert_notification` `content_id` discriminator for a contact-ask
/// doorbell: the peer plus the created row's AUTOINCREMENT id, so each
/// *created* row rings exactly once (a re-ask while pending creates nothing
/// and never re-rings; a fresh ask after a deny is a new row — new id — and
/// rings again, even inside the same epoch second; AUTOINCREMENT is what
/// stops SQLite reusing a denied ask's id — `family-safety.md`
/// § Child-initiated contact requests). A dedup token, not a content
/// identifier.
fn contact_request_dedup_key(peer: &[u8; 32], row_id: i64) -> Vec<u8> {
    let mut key = peer.to_vec();
    key.extend_from_slice(&row_id.to_le_bytes());
    key
}

const CONTACT_REQUEST_NOTIF_TYPE: fauna_protocol::notifications::NotifType =
    fauna_protocol::notifications::NotifType::FamilyContactRequest;

/// The ward's in-app contact ask (`family-safety.md` § Child-initiated
/// contact requests): records a pending row the guardian's queue lists as
/// kind `contact_request` and rings the guardian once. Refusals are typed —
/// this is a user-initiated UI action, not best-effort telemetry, so unlike
/// `notify_report` it never swallows a precondition silently.
fn contact_request_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.contact.request").await?;
            let req: FamilyContactRequestRequest = decode(&payload).map_err(malformed)?;
            let peer = parse_actor(&req.peer_actor_id, "peer_actor_id")?;
            if peer == actor_id {
                return Err(invalid_params("a contact must be another actor"));
            }

            let Some(link) = state
                .db
                .get_guardian_of(&actor_id)
                .await
                .map_err(internal)?
            else {
                return Err(permission_denied(
                    "only a supervised account has a guardian to ask",
                ));
            };
            let approval_on = state
                .db
                .get_guardian_policy(&actor_id)
                .await
                .map_err(internal)?
                .map(|p| p.contact_approval)
                .unwrap_or(false);
            if !approval_on {
                return Err(invalid_params(
                    "contact_approval is not enabled — this account contacts freely",
                ));
            }

            // An existing decision short-circuits: an accepted/confirmed peer
            // needs no approval (quiet ok — nothing to ask), a guardian-blocked
            // peer is refused (the block is already visible on the ward's own
            // contact list — no new disclosure).
            match state
                .db
                .get_contact_status(&actor_id, &peer)
                .await
                .map_err(internal)?
                .as_deref()
            {
                Some("accepted") | Some("confirmed") => return family_ok(),
                Some("blocked") => {
                    return Err(permission_denied("this peer is blocked for this account"));
                }
                _ => {}
            }

            let row_id = match state
                .db
                .add_contact_request(&actor_id, &peer)
                .await
                .map_err(internal)?
            {
                crate::db::family::ContactRequestAdd::Created { row_id } => row_id,
                crate::db::family::ContactRequestAdd::AlreadyPending => return family_ok(),
                crate::db::family::ContactRequestAdd::CapExceeded => {
                    return Err(invalid_params(
                        "too many pending contact requests — wait for your guardian",
                    ));
                }
                crate::db::family::ContactRequestAdd::NotSupervised => {
                    return Err(permission_denied(
                        "only a supervised account has a guardian to ask",
                    ));
                }
            };

            // Doorbell: once per created ask, riding the ordinary notification
            // machinery exactly as Guardian Notify does.
            let Ok(guardian) = <[u8; 32]>::try_from(link.guardian_actor_id.as_slice()) else {
                return family_ok();
            };
            // Micros — the notifications column unit; seconds sort into 1970.
            let now = fauna_core::data::Timestamp::now().as_i64();
            let bucket = contact_request_dedup_key(&peer, row_id);
            let text = doorbell_text("notifications.row_family_contact_request", None);
            if let Some(notif_id) = state
                .db
                .insert_notification(
                    &guardian,
                    &CONTACT_REQUEST_NOTIF_TYPE,
                    "fauna",
                    Some(&actor_id[..]),
                    Some(&bucket),
                    None,
                    &text,
                    now,
                )
                .await
                .map_err(internal)?
            {
                state.ws.notify_push(
                    &guardian,
                    fauna_protocol::PushEvent::Notification(
                        fauna_protocol::push_events::NotificationPayload {
                            notification_id: notif_id,
                            notif_type: CONTACT_REQUEST_NOTIF_TYPE,
                            source: "fauna".into(),
                            sender_id: Some(hex::encode(actor_id)),
                            content_id: Some(hex::encode(&bucket)),
                            summary: text.summary().to_string(),
                            body: text.body().cloned(),
                            timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                            extra: std::collections::BTreeMap::new(),
                        },
                    ),
                );
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:contact.request",
                    Some(&hex::encode(actor_id)),
                    Some(&format!("peer={}", hex::encode(peer))),
                )
                .await;
            family_ok()
        })
    })
}

// ── fauna.family.feed_source.request ───────────────────────────────────

/// The `insert_notification` `content_id` discriminator for a feed-source ask
/// doorbell: the created row's AUTOINCREMENT id, unique per *created* row for
/// the life of the table. So each created ask rings exactly once — a re-ask
/// against an open row creates nothing and never re-rings, while a fresh ask
/// after a deny or a lapse is a new row with a new id and rings again, even
/// inside the same epoch second (`family-safety.md` § Feed-source approvals).
/// A dedup token, not a content identifier.
///
/// The row id alone suffices here, where [`contact_request_dedup_key`] also
/// mixes in the peer: AUTOINCREMENT never reuses an id, and `notif_type`
/// already separates this doorbell from every other kind's — so the two tables'
/// independently-numbered ids cannot collide into one dedup bucket.
fn feed_request_dedup_key(row_id: i64) -> Vec<u8> {
    row_id.to_le_bytes().to_vec()
}

const FEED_SOURCE_NOTIF_TYPE: fauna_protocol::notifications::NotifType =
    fauna_protocol::notifications::NotifType::FamilyFeedSourceRequest;

const FEED_SOURCE_APPROVED_NOTIF_TYPE: fauna_protocol::notifications::NotifType =
    fauna_protocol::notifications::NotifType::FamilyFeedSourceApproved;

/// Ring the **ward** that a grant is waiting to be redeemed ("approved — try
/// again"). `family-safety.md` § Feed-source approvals calls for this because a
/// grant, unlike a contact approve's accepted edge, produces no state the ward's
/// own client would observe: without the doorbell they could only poll.
///
/// Keyed on the approved row's AUTOINCREMENT id, which is the one thing unique
/// per *decision* — a key derived from the object would collide with a previous
/// approval of the same object and be swallowed as a duplicate (the v23 lesson;
/// pinned by `re_approving_the_same_object_yields_a_fresh_doorbell_id`).
async fn ring_feed_source_approved(
    state: &AppState,
    ward: &[u8; 32],
    row_id: i64,
) -> Result<(), RpcError> {
    // Micros — the notifications column unit; seconds sort into 1970.
    let now = fauna_core::data::Timestamp::now().as_i64();
    let bucket = feed_request_dedup_key(row_id);
    let text = doorbell_text("notifications.row_family_feed_source_approved", None);
    if let Some(notif_id) = state
        .db
        .insert_notification(
            ward,
            &FEED_SOURCE_APPROVED_NOTIF_TYPE,
            "fauna",
            None,
            Some(&bucket),
            None,
            &text,
            now,
        )
        .await
        .map_err(internal)?
    {
        state.ws.notify_push(
            ward,
            fauna_protocol::PushEvent::Notification(
                fauna_protocol::push_events::NotificationPayload {
                    notification_id: notif_id,
                    notif_type: FEED_SOURCE_APPROVED_NOTIF_TYPE,
                    source: "fauna".into(),
                    sender_id: None,
                    content_id: Some(hex::encode(&bucket)),
                    summary: text.summary().to_string(),
                    body: text.body().cloned(),
                    timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                    extra: std::collections::BTreeMap::new(),
                },
            ),
        );
    }
    Ok(())
}

/// The ward's in-app feed-source ask (`family-safety.md` § Feed-source
/// approvals): records a pending row the guardian's queue lists as kind
/// `feed_source` and rings the guardian once. Approving it mints a grant the
/// ward redeems by retrying — this handler only ever records the question.
///
/// Refusals are typed for the same reason [`contact_request_handler`]'s are:
/// this is a user-initiated UI action, not best-effort telemetry.
fn feed_source_request_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.family.feed_source.request").await?;
            let req: FamilyFeedSourceRequestRequest = decode(&payload).map_err(malformed)?;

            // The operation is a closed set (shared with every app through
            // `FeedSourceOperation`, so they pre-validate identically). An
            // unnameable one is refused outright rather than degraded: there is
            // no "strictest operation" to fall back to, and resolving it to one
            // of the three would mint a grant for an object nobody approved.
            let Some(operation) = FeedSourceOperation::from_wire(&req.operation) else {
                return Err(invalid_params(
                    "operation must be one of: link, follow, feed",
                ));
            };
            if req.bridge_id.is_empty() || req.bridge_id.len() > MAX_BRIDGE_LEN {
                return Err(invalid_params("bridge_id is required and must be short"));
            }
            if req.target.len() > MAX_FEED_URI_LEN {
                return Err(invalid_params("target too long"));
            }
            if req.label.len() > MAX_NAME_LEN {
                return Err(invalid_params("label too long"));
            }
            // The target's presence is part of the operation's shape, and both
            // directions matter: a `link` ask with a stray target would mint a
            // grant keyed on something the redeeming gate never passes (an
            // approval that silently changes nothing), and a `follow`/`feed` ask
            // with no target names no object for the guardian to approve.
            if operation.takes_target() && req.target.is_empty() {
                return Err(invalid_params("target is required for this operation"));
            }
            if !operation.takes_target() && !req.target.is_empty() {
                return Err(invalid_params(
                    "a link ask carries no target — it approves connecting the bridge",
                ));
            }

            let Some(link) = state
                .db
                .get_guardian_of(&actor_id)
                .await
                .map_err(internal)?
            else {
                return Err(permission_denied(
                    "only a supervised account has a guardian to ask",
                ));
            };
            // Gated on the knob that refused the operation in the first place:
            // with `feed_sources` on `allow` the ward adds sources freely, so
            // the refusal that carries this affordance can never have fired.
            let blocked = state
                .db
                .get_guardian_policy(&actor_id)
                .await
                .map_err(internal)?
                .map(|p| FeedSources::from_wire(&p.feed_sources) == FeedSources::Block)
                .unwrap_or(false);
            if !blocked {
                return Err(invalid_params(
                    "feed_sources is not blocked — this account adds sources freely",
                ));
            }

            let row_id = match state
                .db
                .add_feed_request(
                    &actor_id,
                    &req.bridge_id,
                    operation.as_str(),
                    &req.target,
                    &req.label,
                )
                .await
                .map_err(internal)?
            {
                crate::db::family::FeedRequestAdd::Created { row_id } => row_id,
                // Already asked (pending), or already granted and waiting on the
                // ward's own retry — either way there is nothing new to ring.
                crate::db::family::FeedRequestAdd::AlreadyOpen => return family_ok(),
                crate::db::family::FeedRequestAdd::CapExceeded => {
                    return Err(invalid_params(
                        "too many pending feed-source requests — wait for your guardian",
                    ));
                }
                crate::db::family::FeedRequestAdd::NotSupervised => {
                    return Err(permission_denied(
                        "only a supervised account has a guardian to ask",
                    ));
                }
            };

            // Doorbell: once per created ask, riding the ordinary notification
            // machinery exactly as the contact-ask twin does.
            let Ok(guardian) = <[u8; 32]>::try_from(link.guardian_actor_id.as_slice()) else {
                return family_ok();
            };
            // Micros — the notifications column unit; seconds sort into 1970.
            let now = fauna_core::data::Timestamp::now().as_i64();
            let bucket = feed_request_dedup_key(row_id);
            let text = doorbell_text("notifications.row_family_feed_source_request", None);
            if let Some(notif_id) = state
                .db
                .insert_notification(
                    &guardian,
                    &FEED_SOURCE_NOTIF_TYPE,
                    "fauna",
                    Some(&actor_id[..]),
                    Some(&bucket),
                    None,
                    &text,
                    now,
                )
                .await
                .map_err(internal)?
            {
                state.ws.notify_push(
                    &guardian,
                    fauna_protocol::PushEvent::Notification(
                        fauna_protocol::push_events::NotificationPayload {
                            notification_id: notif_id,
                            notif_type: FEED_SOURCE_NOTIF_TYPE,
                            source: "fauna".into(),
                            sender_id: Some(hex::encode(actor_id)),
                            content_id: Some(hex::encode(&bucket)),
                            summary: text.summary().to_string(),
                            body: text.body().cloned(),
                            timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                            extra: std::collections::BTreeMap::new(),
                        },
                    ),
                );
            }
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    "family:feed_source.request",
                    Some(&hex::encode(actor_id)),
                    Some(&format!(
                        "bridge={} op={} target={}",
                        req.bridge_id,
                        operation.as_str(),
                        req.target
                    )),
                )
                .await;
            family_ok()
        })
    })
}

// ── registration ───────────────────────────────────────────────────────

/// Register the family-safety handlers. Kind metadata mirrors
/// `KindRegistry::register_family_kinds` (all quick @5 s, replay-safe).
pub fn register_family_handlers(b: &mut RpcRouterBuilder) {
    let quick = || Duration::from_secs(5);
    b.add(
        "fauna.family.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: status_handler(),
        },
    );
    b.add(
        "fauna.family.policy.update",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: policy_update_handler(),
        },
    );
    b.add(
        "fauna.family.graduate",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: graduate_handler(),
        },
    );
    b.add(
        "fauna.family.device.mark",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: device_mark_handler(),
        },
    );
    b.add(
        "fauna.family.transfer",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: transfer_handler(),
        },
    );
    b.add(
        "fauna.family.transfer.accept",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: transfer_accept_handler(),
        },
    );
    b.add(
        "fauna.family.transfer.decline",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: transfer_decline_handler(),
        },
    );
    b.add(
        "fauna.family.transfer.cancel",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: transfer_cancel_handler(),
        },
    );
    b.add(
        "fauna.family.approvals.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: approvals_list_handler(),
        },
    );
    b.add(
        "fauna.family.approvals.decide",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: approvals_decide_handler(),
        },
    );
    b.add(
        "fauna.family.contact.add",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: contact_add_handler(),
        },
    );
    b.add(
        "fauna.family.contact.request",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: contact_request_handler(),
        },
    );
    b.add(
        "fauna.family.feed_source.request",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: feed_source_request_handler(),
        },
    );
    b.add(
        "fauna.family.notify_report",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: notify_report_handler(),
        },
    );
    b.add(
        "fauna.family.usage_report",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: quick(),
            handler: usage_report_handler(),
        },
    );
}

#[cfg(test)]
mod doorbell_text_tests {
    /// All four doorbells name a catalog sentence. The content notice's
    /// `category` rides as data the sentence deliberately does not show.
    #[test]
    fn every_family_doorbell_is_a_complete_catalog_sentence() {
        for key in [
            "notifications.row_family_content_notice",
            "notifications.row_family_contact_request",
            "notifications.row_family_feed_source_request",
            "notifications.row_family_feed_source_approved",
        ] {
            let text = super::doorbell_text(key, None);
            crate::db::notifications::assert_body_is_catalog_complete(text.body().unwrap());
        }
        let notice =
            super::doorbell_text("notifications.row_family_content_notice", Some("violence"));
        assert_eq!(
            notice
                .body()
                .unwrap()
                .args
                .get("category")
                .map(String::as_str),
            Some("violence")
        );
        assert!(!notice.summary().contains("violence"));
    }
}

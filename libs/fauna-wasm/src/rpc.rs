//! `#[wasm_bindgen]` exposure of the browser WS-RPC client to the Svelte SPA
//! (Phase 4 of the WS-RPC adoption migration; tracked internally).
//!
//! Wraps `fauna_rpc_wasm::WsRpcClient` (the gloo-net transport that impls
//! `fauna_protocol::RpcRequester`) and the shared typed feature wrappers
//! `AdminClient<R>` / `BridgesClient<R>` / `EmailClient<R>` / `FeedClient<R>` /
//! `PostsClient<R>` / `ContactsClient<R>` / `NotificationsClient<R>` /
//! `AccountClient<R>`, surfacing the typed `fauna.admin.*` / `fauna.bridges.*` /
//! `fauna.email.*` / `fauna.feed.*` / `fauna.posts.*` /
//! `fauna.{knocks,contacts,inbox.mode}.*` / `fauna.notifications.*` /
//! `fauna.{account,quota}.*` + `fauna.profile.handle.change` surface — plus the
//! `fauna.protocol.echo` probe — to JS.
//!
//! The CBOR composition stays Rust-side: request params that the wire types
//! carry as `Value` (`link.params`, `set_settings.settings`, follow
//! `extra`) and the `EmailFilterRule` / `EmailFilterAction` enums cross the JS
//! boundary as `JsValue` via `serde_wasm_bindgen` — *not* as stringly JSON
//! across the WS seam. Replies use the `json_compatible` serializer so CBOR
//! maps surface as plain JS objects and integers as JS numbers (matching the
//! `apps/fauna-web/src/lib/{bridges,api}.ts` TypeScript interfaces).
//!
//! The whole module is `#[cfg(target_arch = "wasm32")]` (gated at the `mod`
//! site in `lib.rs`) because `fauna-rpc-wasm` only exists on wasm; on native
//! the SPA bundle compiles without it.

use fauna_client_account::AccountClient;
use fauna_client_admin::AdminClient;
use fauna_client_bridges::{BridgesClient, MailAccountClient};
use fauna_client_caldav as caldav;
use fauna_client_capabilities::custody_hosting::AdminHostingClient;
use fauna_client_capabilities::view_model::{ReceiptState, admin_hosting_rows};
use fauna_client_carddav as carddav;
use fauna_client_config::{
    ResolvedDestination, attach_folder_to_destination, decide_filter_mark,
    deregister_backup_destination, detach_folder_from_destination, edit_backup_destination,
    enroll_backup_destination, keep_backup_destination_at_rest, list_folder_destinations,
    load_filter_marks, mutate_backup, read_backup_status,
};
use fauna_client_contacts::ContactsClient;
use fauna_client_conversations::ConversationsClient;
use fauna_client_dns::DnsAdminClient;
use fauna_client_dns::host_address::{HostAddressOutcome, HostAddressProbe};
use fauna_client_email::EmailClient;
use std::sync::Arc;

use fauna_account_plane::preference_surfaces;
use fauna_client_accounts::{AccountRegistry, LocalStorageSecretStore};
use fauna_client_core::nest_trust::NestIdentityPinStore;
use fauna_client_family::{FamilyClient, SupervisionSnapshot};
use fauna_client_feed::FeedClient;
use fauna_client_folders::FoldersClient;
use fauna_client_moderation::ModerationClient;
#[cfg(feature = "zaps")]
use fauna_client_nostr::NostrZapSignerClient;
use fauna_client_nostr::{NostrBunkerClient, NostrContentClient};
use fauna_client_notifications::NotificationsClient;
#[cfg(feature = "payments")]
use fauna_client_payments::PaymentsClient;
use fauna_client_personalization::publish::{PublishListError, PublishModelError};
use fauna_client_personalization::{TrainedTopicRow, TrainedTopics, TrainedTopicsError};
use fauna_client_posts::PostsClient;
use fauna_client_profile::ProfileClient;
use fauna_client_push::PushClient;
use fauna_client_push::push::SubscribeRequest;
use fauna_client_recovery::{PredecessorSeed, RecoveryClient, RecoveryKit, RecoveryKitStatus};
use fauna_client_search::SearchClient;
use fauna_client_snapshots::SnapshotsClient;
use fauna_client_spam::SpamClient;
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_subscriptions::orchestration::{
    ConnectPassLatch, ReconcilePass, SubscriptionsAuthor,
};
use fauna_client_subscriptions::subscriptions::{
    MineSubscription, PendingRequest, StatusGetReply, SubscribeReply, SubscriberEntry, TierItem,
};
use fauna_client_sync::SyncClient;
use fauna_client_web::WebClient;
use fauna_core::data::{BackupDestination, ProfileLink, Timestamp, UnattestedVerdict};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::scoring::{
    FilterRule, ListArtifactError, MAX_LABELER_LIST_NAME_LEN, TextModelArtifactError,
};
use fauna_protocol::discovery::{
    NestInfoReply, NestInfoRequest, SetupStatusReply, SetupStatusRequest,
};
use fauna_protocol::email::{EmailFilterAction, EmailFilterRule};
use fauna_protocol::search::{SearchQueryReply, SearchQueryRequest};
use fauna_protocol::spam::{
    SpamGetPreferencesRequest, SpamPreferences, SpamSetPreferencesRequest,
    per_mille_to_probability, probability_to_per_mille,
};
use fauna_protocol::{EchoReply, EchoRequest, RpcRequester, StaleSurfaces, Value};
use fauna_rpc_wasm::{
    AnonymousWsRpcClient, ConnectionState, TokenWsRpcClient, WsRpcClient as InnerClient,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

/// Serialize a reply to `JsValue` with maps-as-objects + numbers-as-numbers,
/// so the typed Rust wire shapes land as the plain JS objects the SPA's TS
/// interfaces expect (`Value::Integer` → JS number, not BigInt).
pub(crate) fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Deserialize a JS value into a typed Rust wire type (request params /
/// filter rules / actions). Errors surface as a rejected promise.
pub(crate) fn from_js<T: serde::de::DeserializeOwned>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// `Some(Value)` for a present JS value, `None` for `null`/`undefined` —
/// matches the `Option<Value>` slot of e.g. `add_follow`'s `extra`.
fn optional_cbor(value: JsValue) -> Result<Option<Value>, JsValue> {
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        Ok(Some(from_js(value)?))
    }
}

/// This crate's one error → `JsValue` conversion: the shared
/// [`fauna_wasm_panic_hook::err_to_js`] text, except that the nest's typed
/// guardian-approval refusal (`fauna_protocol::RpcError::is_guardian_approval_required`)
/// is prefixed `guardian_approval_required: ` — the SPA's twin of the FFI's
/// `FfiError::GuardianApprovalRequired`, in the `superseded:` / `outdated:`
/// prefix style. The ward's in-place ask is offered ONLY on this refusal
/// (`family-safety.md` § Child-initiated contact requests → *App affordance*),
/// and a prefix Rust writes is what spares the SPA matching the nest's message
/// text. Done here, once, so every call on this crate's transport carries it.
pub(crate) fn err_to_js<E: std::fmt::Display + 'static>(e: E) -> JsValue {
    if let Some(fauna_rpc_wasm::WsRpcError::Rpc(rpc)) =
        (&e as &dyn std::any::Any).downcast_ref::<fauna_rpc_wasm::WsRpcError>()
        && rpc.is_guardian_approval_required()
    {
        return JsValue::from_str(&guardian_refusal_text(&e.to_string()));
    }
    fauna_wasm_panic_hook::err_to_js(e)
}

/// The prefix [`err_to_js`] puts on a guardian-approval refusal — the one
/// spelling `apps/fauna-web/src/lib/guardian-refusal.ts` reads.
pub(crate) const GUARDIAN_APPROVAL_REQUIRED_PREFIX: &str = "guardian_approval_required: ";

fn guardian_refusal_text(detail: &str) -> String {
    format!("{GUARDIAN_APPROVAL_REQUIRED_PREFIX}{detail}")
}

/// `AdminInviteRequest` + the shared [`AdminInviteRequest::is_pending`] predicate
/// projected onto the JS reply — the wasm twin of native's projected
/// `FfiAdminInviteRequest.is_pending` field. The web admin-users hub reads
/// `r.is_pending` instead of re-coding `status === 'pending'` (`admin.md` —
/// adopting clients **call** `is_pending`, never re-code the wire string). Flatten
/// keeps every original field's serialization byte-identical; only the bool is
/// added.
#[derive(Serialize)]
struct InviteRequestJs {
    is_pending: bool,
    #[serde(flatten)]
    inner: fauna_protocol::admin::AdminInviteRequest,
}

impl From<fauna_protocol::admin::AdminInviteRequest> for InviteRequestJs {
    fn from(inner: fauna_protocol::admin::AdminInviteRequest) -> Self {
        Self {
            is_pending: inner.is_pending(),
            inner,
        }
    }
}

/// One `BackupState.backup.destinations` row on its way to the SPA, carrying the
/// post-succession review question the Backups page renders beside it
/// (`succession-aftermath.md` § Re-key scope → *Adjudicating what the aftermath
/// carries across*).
///
/// **`unattested` is computed in shared Rust, never re-derived in TS** — the
/// same rule, and deliberately the same field NAME, as the trust plane's
/// `TrustGrantRow.unattested` that row 355 leg (a) landed. The predicate is
/// [`fauna_core::data::DestinationUnattestedMark::row_is_raised`] over the
/// same read's `BackupState.marks`.
///
/// The row in `inner` carries no mark of its own: the verdict rides the
/// state's mark plane, which `row_is_raised` reads.
#[derive(Serialize)]
struct BackupDestinationJs {
    unattested: bool,
    #[serde(flatten)]
    inner: BackupDestination,
}

/// One row of the nest-wide custody-hosting registry (`admin-custody-hosting`,
/// `account-data-plane.md` § Two-sided bounds) — the JS-visible shape of
/// [`fauna_client_capabilities::view_model::AdminHostingRowView`], produced by
/// the shared `admin_hosting_rows` fold before crossing. `receipt_state`
/// degrades to a string (the three [`ReceiptState`] variant names in
/// snake_case — never collapsed, never re-derived in TS) since the enum
/// itself carries no `Serialize`.
#[derive(Serialize)]
struct JsAdminHostingRow {
    host_actor_id: String,
    owner_actor_id: String,
    owner_nest_url: String,
    grant_id: Vec<u8>,
    retained_bytes_cap: u64,
    held_bytes: u64,
    stopped: bool,
    receipt_state: String,
}

impl From<fauna_client_capabilities::view_model::AdminHostingRowView> for JsAdminHostingRow {
    fn from(r: fauna_client_capabilities::view_model::AdminHostingRowView) -> Self {
        Self {
            host_actor_id: r.host_actor_id,
            owner_actor_id: r.owner_actor_id,
            owner_nest_url: r.owner_nest_url,
            grant_id: r.grant_id,
            retained_bytes_cap: r.retained_bytes_cap,
            held_bytes: r.held_bytes,
            stopped: r.stopped,
            receipt_state: match r.receipt_state {
                ReceiptState::Fresh => "fresh",
                ReceiptState::Stale => "stale",
                ReceiptState::NoReceiptYet => "no_receipt_yet",
            }
            .to_string(),
        }
    }
}

/// Decode a hex id (the rpc.rs boundary convention — byte ids ride as hex
/// strings) into raw bytes, surfacing a malformed id as a rejected promise.
fn hex_bytes(hex_str: &str) -> Result<Vec<u8>, JsValue> {
    hex::decode(hex_str.trim()).map_err(|e| JsValue::from_str(&format!("invalid hex id: {e}")))
}

/// Decode a hex id into a fixed 32-byte array — the caldav byte-id convention
/// (`actor_id` / `calendar_id` / `uid_hash` are all 32 bytes).
fn hex_array_32(hex_str: &str) -> Result<[u8; 32], JsValue> {
    hex_bytes(hex_str)?
        .as_slice()
        .try_into()
        .map_err(|_| JsValue::from_str("expected a 32-byte hex id"))
}

// ── encrypted-DAV (caldav + carddav) store helpers ────────────────────────────

/// Build the wasm encrypted-DAV context: derive the actor id from `secret_hex` and
/// read the MSEK from this tab's mail custody (`fauna.state.mail`) **inside wasm**
/// (the msek never crosses into JS — see `apps/fauna-web/src/lib/conversations.ts`
/// § MSEK), the WASM twin of the linux `caldav_context` over the shared
/// [`fauna_client_config::dav_store_context`]. **Shared by CalDAV (Events) and
/// CardDAV (Address Book)** — both read the SAME MSEK gate (priority #2). Returns
/// `None` when mail is off (no msek minted) or the custody cannot be read, so a
/// caller degrades to an empty list rather than erroring (events.md /
/// carddav-server.md § Persistence — the encrypted path is msek-gated;
/// localhost/IP/mail-off ⇒ empty).
pub(crate) async fn dav_ctx(secret_hex: &str) -> Result<Option<([u8; 32], [u8; 32])>, JsValue> {
    let secret = hex_bytes(secret_hex)?;
    let arr: [u8; 32] = secret
        .as_slice()
        .try_into()
        .map_err(|_| JsValue::from_str("actor secret must be 32 bytes"))?;
    let actor_id = ActorKeypair::from_secret(arr).actor_id().0;
    Ok(fauna_client_config::dav_store_context(
        crate::account_runtime::mail_store().as_ref(),
        actor_id,
    )
    .await)
}

/// Find one stored event by `uid_hash` in `calendar_id` and return its unsealed
/// `.ics` + decoded Fauna sidecar — the read half of a read-mutate-rewrite (rsvp /
/// reminder / invite). Errors if the event is absent. Mirrors the linux mutate
/// path (query → locate by `uid_hash` → unseal body+sidecar → mutate → re-PUT).
async fn caldav_find_event(
    client: InnerClient,
    actor_id: &[u8; 32],
    calendar_id: &[u8; 32],
    uid_hash_target: &[u8],
    msek: &[u8; 32],
) -> Result<(String, Option<caldav::FaunaEventExt>), JsValue> {
    let reply = caldav::CalDavClient::new(client)
        .query_events(caldav::bridge_routing::QueryEventsRequest {
            actor_id: actor_id.to_vec(),
            calendar_id: calendar_id.to_vec(),
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        })
        .await
        .map_err(err_to_js)?;
    let events = match reply {
        caldav::bridge_routing::QueryEventsReply::Ok { events, .. } => events,
        caldav::bridge_routing::QueryEventsReply::CalendarNotFound => Vec::new(),
        caldav::bridge_routing::QueryEventsReply::Unknown => {
            return Err(JsValue::from_str(caldav::UNKNOWN_QUERY_OUTCOME));
        }
    };
    let entry = events
        .into_iter()
        .find(|e| e.uid_hash == uid_hash_target)
        .ok_or_else(|| JsValue::from_str("event not found"))?;
    // Derived once — the body and the sidecar below both ride the same
    // recipient keypair, so this saves the second re-derivation
    // `unseal_event_body` + `unseal_fauna_ext` would otherwise each pay
    // .
    let keys = caldav::DavRecipientKeys::derive(msek);
    let plaintext = keys.unseal(&entry.encrypted_body).map_err(err_to_js)?;
    let ics = String::from_utf8(plaintext)
        .map_err(|e| JsValue::from_str(&format!("VEVENT body not UTF-8: {e}")))?;
    let fauna_ext = match &entry.encrypted_fauna_ext {
        Some(sealed) => Some(caldav::unseal_fauna_ext(sealed, &keys).map_err(err_to_js)?),
        None => None,
    };
    Ok((ics, fauna_ext))
}

/// Project a decoded [`caldav::FlatEvent`] into the web Events-page row shape:
/// `{ id: hex(uid_hash), uid, summary, dtstart, dtend, location, description,
/// status, is_all_day, rrule, alarm, organized_by_me, attendees: [{ name, email,
/// rsvp }] }`. `id` is the hex `uid_hash` (the mutate key), matching the linux
/// `EventRow.id`. `organized_by_me` is the canonical author-gate predicate
/// (`caldav::organized_by_me` over the VEVENT `ORGANIZER` email vs `self_email`) —
/// computed in shared Rust, the same predicate native's `FfiCalEvent` exposes, so
/// the page gates author-only actions (delete / invite) on it rather than
/// re-deriving an email compare in TS (priority #2 — one rule, both faces).
fn flat_event_to_web_json(e: &caldav::FlatEvent, self_email: &str) -> serde_json::Value {
    let attendees: Vec<serde_json::Value> = e
        .attendees
        .iter()
        .map(|a| serde_json::json!({ "name": a.name, "email": a.email, "rsvp": a.rsvp }))
        .collect();
    let organizer = caldav::parse_ical_organizer(&e.ics).unwrap_or_default();
    serde_json::json!({
        "id": hex::encode(&e.uid_hash),
        "uid": e.fields.uid,
        "summary": e.fields.summary,
        "dtstart": e.fields.dtstart,
        "dtend": e.fields.dtend,
        "location": e.fields.location,
        "description": e.fields.description,
        "status": e.fields.status,
        "is_all_day": e.fields.is_all_day,
        "rrule": e.fields.rrule,
        "alarm": e.fields.alarm,
        "organized_by_me": caldav::organized_by_me(&organizer, self_email),
        "attendees": attendees,
    })
}

/// Project a decoded address book into the web Address-Book sidebar row shape:
/// `{ id: hex(addressbook_id), name, description, card_count }`. The wasm twin of
/// native's `FfiAddressbookRow` (priority #2 — one shape, both faces).
fn decoded_addressbook_to_web_json(book: &carddav::DecodedAddressbook) -> serde_json::Value {
    serde_json::json!({
        "id": hex::encode(&book.addressbook_id),
        "name": book.metadata.displayname,
        "description": book.metadata.description,
        "card_count": book.card_count,
    })
}

/// Project a decoded vCard into the web Address-Book row shape:
/// `{ id: hex(card_id), uid, formatted_name, emails, tels, addresses, urls, org,
/// title, note, bday, has_fauna_ext }`, where each `emails`/`tels`/`urls` entry is
/// `{ value, types, pref }` and each `addresses` entry carries the structured
/// components plus the shared one-line `formatted` render (`Address::one_line`, the
/// same text native's `FfiPostalAddress.formatted` shows). The wasm twin of native's
/// `FfiCardRow` (priority #2 — one shape, both faces).
fn decoded_card_to_web_json(card: &carddav::DecodedCard) -> serde_json::Value {
    let p = &card.parsed;
    let values = |vs: &[carddav::vcard::TypedValue]| -> Vec<serde_json::Value> {
        vs.iter()
            .map(|v| serde_json::json!({ "value": v.value, "types": v.types, "pref": v.pref }))
            .collect()
    };
    let addresses: Vec<serde_json::Value> = p
        .addresses
        .iter()
        .map(|a| {
            serde_json::json!({
                "types": a.types,
                "pref": a.pref,
                "po_box": a.po_box,
                "extended": a.extended,
                "street": a.street,
                "locality": a.locality,
                "region": a.region,
                "postal_code": a.postal_code,
                "country": a.country,
                "formatted": a.one_line(),
            })
        })
        .collect();
    serde_json::json!({
        "id": hex::encode(&card.card_id),
        "uid": p.uid,
        "formatted_name": p.formatted_name,
        "emails": values(&p.emails),
        "tels": values(&p.tels),
        "addresses": addresses,
        "urls": values(&p.urls),
        "org": p.org,
        "title": p.title,
        "note": p.note,
        "bday": p.bday,
        "has_fauna_ext": card.has_fauna_ext,
    })
}

/// Re-seal + PUT an [`caldav::EventRewrite`] produced by a read-mutate-rewrite
/// helper, the write half shared by the rsvp / reminder / invite seam methods.
async fn caldav_put_rewrite(
    client: InnerClient,
    actor_id: &[u8; 32],
    calendar_id: &[u8; 32],
    msek: &[u8; 32],
    rw: &caldav::EventRewrite,
    now_secs: i64,
) -> Result<(), JsValue> {
    let new_uid_hash = caldav::uid_hash(&rw.fields.uid);
    caldav::CalDavClient::new(client)
        .seal_and_put_event(
            actor_id,
            calendar_id,
            &new_uid_hash,
            msek,
            &rw.fields,
            &rw.attendees,
            &rw.organizer_email,
            rw.fauna_ext.as_ref(),
            now_secs,
            None,
        )
        .await
        .map_err(err_to_js)?;
    Ok(())
}

/// `SpamPreferences` (per-mille wire) → the flat web JSON the Settings page
/// consumes — byte-identical to the retired `GET /api/v1/spam/preferences`
/// shape's thresholds (`{ spam_threshold, phishing_threshold: 0.0–1.0 }`).
fn spam_prefs_to_web_json(p: &SpamPreferences) -> serde_json::Value {
    serde_json::json!({
        "spam_threshold": per_mille_to_probability(p.spam_threshold),
        "phishing_threshold": per_mille_to_probability(p.phishing_threshold),
    })
}

/// `SearchQueryReply` → the `{ results: [...] }` web JSON the search page
/// consumes — byte-identical to the retired `GET /api/v1/search` shape. The
/// wire `rank` is i64 micro-units (negated BM25 × 1e6 — dag-cbor forbids
/// floats); divide by 1e6 to restore the float `rank` the HTTP twin returned
/// (behavior-preserving; the page itself never reads `rank` — nest pre-sorts).
fn search_reply_to_web_json(reply: &SearchQueryReply) -> serde_json::Value {
    let results: Vec<serde_json::Value> = reply
        .results
        .iter()
        .map(|h| {
            serde_json::json!({
                "content_type": h.content_type,
                "content_id": h.content_id,
                "created_at": h.created_at,
                "rank": h.rank as f64 / 1_000_000.0,
                "snippet": h.snippet,
            })
        })
        .collect();
    serde_json::json!({ "results": results })
}

// ── backup destinations (docs/goal/ui/backups.md § Manage backup destinations) ──
//
// Free helpers behind the `WsRpcClient::backupDestination*` methods (below) —
// the wasm twin of the linux `views/backups/destinations.rs` glue + the native
// FFI `resolve_backup_destination`. The destination set is this box's
// `fauna.state.backup` row in the account store (`account_runtime::backup_seam`,
// keyed by `bound_nest_id`): **add** runs the one shared enroll sequence
// `fauna_client_config::enroll_backup_destination` (grant + writer-grant
// register + destination register, then the row write); **remove** runs
// `deregister_backup_destination` (destination-remove on the source nest, then
// the row drops); **edit** runs `edit_backup_destination` inside
// `mutate_backup` directly. Resolving the candidate (+, for add, opening an
// authed session to it) stays wasm-specific.

/// One row of the nest's `fauna.backup.status` projection, reshaped for JS —
/// the Backups page status rows (`backups.md` § Per-destination status read).
/// Field names match the native FFI projection so the JS shape is uniform.
#[derive(Serialize)]
struct WasmBackupDestinationStatus {
    destination_id: String,
    /// A real timestamp since the leg-(d) repoint: the nest's own coordinator
    /// uploads for web-only users, so this is no longer hard-coded `None`.
    last_upload_time: Option<u64>,
    backlog_count: u32,
    /// Client-device rows only: bytes the custodian reported holding at its last
    /// check-in (`backup-destination-usage`). `None` on a nest row **and** on a
    /// custodian that has never checked in — *nothing held yet*, not *0 held*.
    held_bytes: Option<u64>,
    /// Client-device rows only: `CAP_STATE_OK` / `CAP_STATE_REACHED`, passed
    /// straight to the shared `backupUsageLabel`.
    ///
    /// ⚠ Cap-reached is **read, never inferred**: a pull pass that stopped at its
    /// cap ends *below* the cap, so an SPA re-deriving the verdict from
    /// `held >= cap` renders a silently-stopped backup as healthy-with-room.
    /// Dropping this field would leave the SPA no choice but that inference.
    cap_state: Option<String>,
    /// Client-device rows only: the custodian's own last self-audit verdict
    /// (`AUDIT_STATE_OK` / `AUDIT_STATE_FAILED`). The owner-side audit loop can
    /// never produce this one — a custodian has no address to sample — so this
    /// field is the SPA's only answer to "does that device's copy still verify"
    /// (`backup-destinations.md` § Custodian contract, question 4).
    ///
    /// ⚠ `null` is *not yet audited*, never *passing*: every custodian shipped
    /// before the carrier landed reports nothing at all. The loud/quiet decision
    /// stays in shared Rust (`backupSelfAuditIsAlerting`), exactly as
    /// `alert_reason` keeps it for the owner-side verdicts below.
    audit_state: Option<String>,
    /// Client-device rows only: unix seconds of the custodian's last **passed**
    /// self-audit. Read **with** `audit_state`, never instead of it — a failure
    /// leaves this stamp at the previous pass, so the stamp alone renders a
    /// freshly-rotted custodian as just-verified.
    last_audit_passed_at: Option<u64>,
}

/// One destination's audit picture, reshaped for JS — the two
/// `backups.md` § Audit-alert surface elements and nothing else.
///
/// **Deliberately not the whole `DestinationAuditRecord`.** `verdict` is an
/// `AuditVerdict`, and handing it to JS would invite the SPA to decide which
/// verdicts are loud — the one decision the shared crate keeps
/// (`DestinationAuditRecord::alert_reason`, through which `AuditVerdict::
/// is_alerting` is itself defined, so a fourth alerting verdict cannot be added
/// without also giving it a banner). What crosses is the *already-resolved*
/// answer: `alert_reason` present ⇒ render a banner, absent ⇒ don't.
///
/// `alert_reason` is opaque to the SPA: it goes straight back into
/// `backupAuditAlertLabel` for its text.
#[derive(Serialize)]
struct WasmDestinationAuditRow {
    destination_id: String,
    /// Unix seconds of the last **passed** audit; `None` = never passed, which
    /// renders "never" and is deliberately not an alert. Narrowed from the
    /// record's `i64` exactly as linux's `last_audit_text` does — a negative
    /// stamp is not a time this client could have written.
    last_passed_at: Option<u64>,
    /// The standing banner reason, or `None` for a healthy destination. Kept for
    /// shells not yet moved to [`Self::alert_reasons`].
    alert_reason: Option<fauna_core::format::BackupAuditAlertReason>,
    /// Every banner reason standing at the pass's `now`
    /// (`DestinationAuditRecord::alert_reasons`): the standing verdict's reason,
    /// then at most one `SourceRegressed`. The SPA paints every entry, each
    /// opaque, through `backupAuditAlertLabel`.
    alert_reasons: Vec<fauna_core::format::BackupAuditAlertReason>,
}

/// Project one persisted record to its render row — the wasm twin of the ffi
/// `audit_row`, split out so the projection is unit-testable without a nest.
fn audit_row(
    r: &fauna_client_backup::audit::DestinationAuditRecord,
    now: i64,
) -> WasmDestinationAuditRow {
    WasmDestinationAuditRow {
        destination_id: r.state.destination_id.clone(),
        last_passed_at: r.state.last_passed_at.and_then(|s| u64::try_from(s).ok()),
        alert_reason: r.alert_reason(),
        alert_reasons: r.alert_reasons(now),
    }
}

#[cfg(test)]
mod audit_row_tests {
    use super::*;
    use fauna_client_backup::audit::{
        AcceptedRegression, AuditVerdict, DestinationAuditRecord, DestinationAuditState,
    };
    use fauna_core::format::BackupAuditAlertReason;

    /// An accepted regression inside its window projects as the fifth reason
    /// beside a `Passed` verdict (which leaves `alert_reason` empty); once the
    /// window closes the list is empty. Runs under `wasm-pack test` (the crate
    /// does not build natively); `run_in_browser` is configured once, in
    /// `critical_alerts`.
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn an_open_recovery_window_projects_as_a_reason() {
        const NOW: i64 = 1_800_000_000;
        let mut state = DestinationAuditState {
            destination_id: "id-a".to_string(),
            last_passed_at: Some(NOW),
            ..Default::default()
        };
        state.accepted_regressions.insert(
            "ledger".to_string(),
            AcceptedRegression {
                pinned: 40,
                served: 30,
                observed_at: NOW,
                floored_at: None,
                recoverable_until: Some(NOW + 1_000),
            },
        );
        let record = DestinationAuditRecord {
            state,
            verdict: Some(AuditVerdict::Passed),
        };

        let open = audit_row(&record, NOW);
        assert!(open.alert_reason.is_none());
        assert_eq!(
            open.alert_reasons,
            vec![BackupAuditAlertReason::SourceRegressed {
                left_secs: Some(1_000)
            }]
        );
        assert!(audit_row(&record, NOW + 1_000).alert_reasons.is_empty());
    }
}

/// The wasm twin of `fauna_core::DeploymentSeedEntry` for the total-box-loss
/// recovery box list (`box-recovery.md` § Restore, step 4) — one row per box the
/// admin custodies a deployment seed for (the account plane's
/// `fauna.state.deployment-seeds` fold). The raw seed is **never** handed to JS:
/// the seed is the nest's signing identity, a capability distinct from admin
/// authority, so it stays WASM-internal.
/// Only the box's `nest_actor_id` — which the id publicly *is*
/// (`ed25519(seed).public`) — crosses the boundary; the recovery re-provision
/// drive resolves the seed back in Rust by that id.
#[derive(Serialize)]
struct WasmDeploymentSeedEntry {
    /// 64-char hex of the box's `nest_actor_id`.
    nest_actor_id: String,
    /// The box's own handle domain (server_name / DNS zone), for the
    /// `recover-box-item` label + the cloud re-provision zone. `None` for a
    /// domainless box. Non-secret (unlike the seed), so it crosses to JS.
    domain: Option<String>,
}

/// Project a folded custody map onto the rows the recovery UI lists —
/// superseded boxes excluded, through the one shared projection
/// (`fauna_client_config::recoverable_boxes_in`, `box-recovery.md` § Custody
/// after rotation).
fn recovery_box_rows(
    seeds: &[fauna_core::data::DeploymentSeedEntry],
) -> Vec<WasmDeploymentSeedEntry> {
    fauna_client_config::recoverable_boxes_in(seeds)
        .into_iter()
        .map(|e| WasmDeploymentSeedEntry {
            nest_actor_id: hex::encode(e.nest_actor_id),
            domain: e.domain,
        })
        .collect()
}

/// The pre-login custody map for the identity behind `secret` — this device's
/// own account store (the IndexedDB root the account runtime opens) joined
/// with a cold read over `cold` when the caller holds a nest client
/// (`box-recovery.md` § The plane-era recovery floor, *(b) The reads*). A
/// source that fails leaves the other's answer; rejects only when every
/// source asked failed.
async fn resolve_recovery_seeds(
    secret: &[u8; 32],
    cold: Option<&InnerClient>,
) -> Result<Vec<fauna_core::data::DeploymentSeedEntry>, JsValue> {
    use fauna_account_plane::deployment_seed_recovery::{StoreRoot, resolve_deployment_seeds_over};
    resolve_deployment_seeds_over(&StoreRoot::platform(), secret, cold)
        .await
        .into_result()
        .map_err(|e| JsValue::from_str(&format!("{e:#}")))
}

/// Render the `recover-selfhosted-command` for `nest_actor_id_hex` out of a
/// folded custody map (the shared `selfhosted_recovery_command_in`, so every
/// app emits a byte-identical line); rejects when no source custodies it.
fn selfhosted_command_js(
    seeds: &[fauna_core::data::DeploymentSeedEntry],
    nest_actor_id_hex: &str,
) -> Result<JsValue, JsValue> {
    fauna_client_config::selfhosted_recovery_command_in(seeds, nest_actor_id_hex.trim())
        .map(|cmd| JsValue::from_str(&cmd))
        .ok_or_else(|| JsValue::from_str("no custodied deployment seed for that box"))
}

/// The `nest_actor_id` this connection is **bound** to, as an [`ActorId`] —
/// the one comparand the seed custody leg and the rotation drive key on. `LinkedNestsMachine::bound_nest_id` (the web twin
/// of linux `resolve_this_nest_id`): the origin's possession-verified pin,
/// else a possession proof over the connection, and a refusal when the
/// nest's own `fauna.nest.info` claim disagrees with it — never that claim on
/// its own, which any box can answer with a sibling's id.
pub(crate) async fn bound_nest_id(client: &InnerClient) -> Result<ActorId, JsValue> {
    let id = fauna_client_pair::build_linked_nests_machine(client.clone())
        .bound_nest_id()
        .await
        .map_err(err_to_js)?;
    let id: [u8; 32] = id
        .as_slice()
        .try_into()
        .map_err(|_| JsValue::from_str("this nest's id was not 32 bytes"))?;
    Ok(ActorId(id))
}

fn keypair_from_secret_hex(secret_hex: &str) -> Result<ActorKeypair, JsValue> {
    Ok(ActorKeypair::from_secret(hex_array_32(secret_hex)?))
}

// ── Recovery kit helpers (`settings.md` § Recovery kit) — shared behind the
// `recoveryKit*` methods below. The tui/linux twins are
// `apps/fauna-tui/src/settings/mod.rs`'s `recovery_identity` / `with_status` /
// `mirror_recovery_head` / `escrow_predecessor_seeds` and
// `apps/fauna-linux/src/client.rs`'s mirrors of the same — reproduced here
// rather than lifted, since each app supplies its own transport binding
// around the same shared `fauna_client_recovery` ceremonies (priority #2
// already covers the ceremony logic itself). ──

/// `recovery-kit-status`'s data: the status text (resolve via the SPA's
/// `resolveLocalized`) plus the three gesture-enablement flags this scope's
/// buttons read. Never re-derived client-side — this mirrors the shared
/// projection's own `allows_*`, so the SPA cannot drift from tui/linux.
#[derive(Serialize)]
struct RecoveryStatusJs {
    status_text: fauna_core::localized::LocalizedText,
    allows_create: bool,
    allows_replace: bool,
    allows_lost: bool,
    /// `identity-stolen-button`'s enablement — **true in every state**, per
    /// `ui/settings.md` § Recovery kit's "stolen (any)": theft does not wait for
    /// the account to be tidy, and an identity stolen while a replacement pends
    /// is the worst state of all. Projected rather than assumed constant on the
    /// SPA side so the enablement matrix stays the shared crate's to decide, and
    /// so the kit-in-hand phrase field can gate on the same predicate tui's does
    /// (it is the field the succession reads).
    allows_stolen: bool,
    /// `recovery-kit-escrow-reseal-button`'s render gate — the no-escrow state
    /// only (`RecoveryKitStatus::allows_escrow_reseal`).
    allows_escrow_reseal: bool,
    /// `recovery-pending-veto-button`'s render gate: a seed-alone replacement
    /// window is open. The FFI twin is `pending_lands_at.is_some()`.
    replacement_pending: bool,
}

fn recovery_status_view(status: &RecoveryKitStatus) -> RecoveryStatusJs {
    let now = Timestamp::now_secs_or_zero();
    RecoveryStatusJs {
        status_text: status.status_line(now),
        allows_create: status.allows_create(),
        allows_replace: status.allows_replace(),
        allows_lost: status.allows_lost(),
        allows_stolen: status.allows_stolen(),
        allows_escrow_reseal: status.allows_escrow_reseal(),
        replacement_pending: matches!(status, RecoveryKitStatus::ReplacementPending(_)),
    }
}

/// A create/replace/lost ceremony's result: the minted secret, its
/// `fauna://recovery` display URI (built here — the same actor/handle pairing
/// tui's fold and linux's `recovery_kit_uri` use), and a no-second-hop status
/// re-read taken inside the same async call (mirrors tui's `with_status`).
#[derive(Serialize)]
struct RecoveryMintedJs {
    secret_hex: String,
    uri: String,
    status: RecoveryStatusJs,
}

/// Whom `keypair`'s identity succeeded from, per this browser's account
/// registry — the same `AccountRegistry` [`recovery_predecessor_seeds`] reads —
/// as the list the profile writers admit an inherited base against
/// (`profile.md` § After an identity succession, the successor RE-PUBLISHES).
/// Resolved here rather than taken from JS, so the SPA's call sites stay as
/// they are and the registry walk stays the shared one.
fn profile_predecessors(keypair: &ActorKeypair) -> Vec<ActorId> {
    let registry = AccountRegistry::new(Arc::new(LocalStorageSecretStore));
    fauna_client_profile::predecessors_from_hex(&registry.predecessors_of(&keypair.actor_id_hex()))
}

/// The full-chain predecessor seeds an **escrow-writing** ceremony must carry
/// (`identity-succession.md` § Seed escrow: a kit *replacement* re-puts the
/// resting blob too, and the blob it replaces may be carrying predecessor
/// seed(s) inside the corpus re-seal window; writing it without them silently
/// reopens the device-loss race the escrow backstop exists to close). Reads
/// the SAME `AccountRegistry` the account switcher drives (`WasmAccountRegistry`
/// wraps this identical type) — constructed fresh here since both halves are
/// stateless.
fn recovery_predecessor_seeds(secret_hex: &str) -> Vec<PredecessorSeed> {
    let Ok(keypair) = ActorKeypair::from_secret_hex(secret_hex) else {
        return Vec::new();
    };
    let registry = AccountRegistry::new(Arc::new(LocalStorageSecretStore));
    registry
        .predecessor_seeds(&keypair.actor_id_hex())
        .into_iter()
        .filter_map(|(hex, seed)| {
            let actor_id = fauna_core::hex32::decode(&hex).ok()?;
            Some(PredecessorSeed { actor_id, seed })
        })
        .collect()
}

/// The dialed host, stripped of scheme/path/userinfo — the browser TOFU pin
/// store's key. `fauna_anon_client::trust::authority_of` (native) delegates to
/// the same `fauna_core::web::authority_of` — that crate itself pulls in
/// tokio/rustls/tokio-tungstenite and does not build for wasm32, but the
/// pure-string primitive both need lives in the wasm-safe `fauna-core`.
fn wasm_authority_of(nest_url: &str) -> String {
    fauna_core::web::authority_of(nest_url)
}

/// The home-nest facts a profile mirror publishes as the primary `nests`
/// entry: the dialed URL and the browser's TOFU pin for it, when one rests
/// (`fauna_client_recovery::ceremony`'s native `home_nest` twin).
pub(crate) fn wasm_home_nest(client: &InnerClient) -> fauna_client_profile::HomeNest {
    let url = client.nest_url();
    let nest_id = fauna_client_core::nest_trust::LocalStoragePinStore.get(&wasm_authority_of(&url));
    fauna_client_profile::HomeNest { url, nest_id }
}

/// Mirror a **landed** registration's chain head into the signed profile — the
/// one step `fauna-client-recovery` deliberately leaves to its caller
/// (`identity-succession.md` § The RecoveryKey: peers cache the binding with
/// the profile they already hold, so a succession verifies with no chain
/// fetch). Best-effort by design: the field is a cache — the registration
/// chain is authoritative — so a failure here costs peers one chain fetch,
/// never the ceremony's own success (the kit is minted, registered, and its
/// secret is already on its way to the screen). The home-nest pin read is
/// equally best-effort: an unpinned/unprovable origin publishes `nest_id:
/// None` rather than failing the mirror.
async fn mirror_recovery_head_wasm(
    client: InnerClient,
    identity: &ActorKeypair,
    kit: &RecoveryKit,
) {
    let home = wasm_home_nest(&client);
    if let Err(e) = fauna_client_profile::publish_recovery_head(
        client,
        identity,
        &profile_predecessors(identity),
        kit.chain_head(),
        Some(home),
    )
    .await
    {
        tracing::warn!("[wasm/recovery] recovery-head profile mirror: {e}");
    }
}

/// Pair a ceremony's returned secret with a fresh status read, inside the
/// same call — the ceremony moved the registration chain, so the section's
/// line is stale the instant it returns. A failed re-read must not discard
/// the minted secret (the only copy in existence): it falls back to the state
/// the ceremony is known to have produced.
async fn with_fresh_status_wasm(
    recovery: &RecoveryClient<InnerClient>,
    identity: &ActorKeypair,
    secret_hex: String,
    handle: Option<&str>,
    nest_url: &str,
) -> RecoveryMintedJs {
    // The one URI builder every app shares; the SPA's handle is already
    // `handle@domain`, which it passes through unchanged.
    let uri = fauna_client_recovery::kit_display_uri(
        &secret_hex,
        &identity.actor_id().to_hex(),
        handle.unwrap_or_default(),
        nest_url,
    );
    let status = fauna_client_recovery::kit_status(recovery, &identity.actor_id())
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("[wasm/recovery] status re-read after a ceremony: {e}");
            RecoveryKitStatus::Registered
        });
    RecoveryMintedJs {
        secret_hex,
        uri,
        status: recovery_status_view(&status),
    }
}

/// The `create_kit` ceremony, shared between the create and replace gestures
/// (they differ only in `prior`) — mints/replaces the kit, mirrors the landed
/// head into the profile, then re-reads status in the SAME call so the
/// section repaints from one round trip with no follow-up fetch.
async fn recovery_create_kit(
    client: InnerClient,
    secret_hex: &str,
    handle: Option<&str>,
    prior: Option<&fauna_core::recovery::RecoveryKey>,
) -> Result<RecoveryMintedJs, JsValue> {
    let identity = keypair_from_secret_hex(secret_hex)?;
    let nest_url = client.nest_url();
    let recovery = RecoveryClient::new(client.clone());
    let predecessors = recovery_predecessor_seeds(secret_hex);
    let minted = fauna_client_recovery::create_kit(&recovery, &identity, prior, &predecessors)
        .await
        .map_err(err_to_js)?;
    let minted_secret_hex = minted.secret_hex().to_string();
    // The chain moved: carry it to every linked nest now, not at the next
    // full pass (`identity-succession.md` § Enforcement on the home nest).
    if let Some(store) = crate::account_runtime::handle() {
        store.registration_chain_moved();
    }
    mirror_recovery_head_wasm(client, &identity, &minted).await;
    Ok(with_fresh_status_wasm(&recovery, &identity, minted_secret_hex, handle, &nest_url).await)
}

/// Open a seed-alone replacement window (`recovery-kit-lost-button`) at the
/// bound nest and every linked nest — `request_seed_alone_replacement_everywhere`.
/// Unlike create/replace this does not move
/// the chain head (the request only lands after the 30-day window), so no
/// profile mirror runs here.
async fn recovery_lost_kit(
    client: InnerClient,
    secret_hex: &str,
    handle: Option<&str>,
) -> Result<RecoveryMintedJs, JsValue> {
    let identity = keypair_from_secret_hex(secret_hex)?;
    let nest_url = client.nest_url();
    let recovery = RecoveryClient::new(client);
    // At the bound nest, then at every linked nest (`identity-succession.md`
    // § Enforcement on the home nest, clause (c)); a linked nest it cannot
    // reach is logged and owed by the runtime's secondary leg.
    let dial = crate::account_runtime::linked_nest_dial(&identity.actor_id_hex());
    let (pending, _linked) = fauna_client_recovery::request_seed_alone_replacement_everywhere(
        &recovery, &identity, &dial,
    )
    .await
    .map_err(err_to_js)?;
    let minted_secret_hex = pending.secret_hex().to_string();
    Ok(with_fresh_status_wasm(&recovery, &identity, minted_secret_hex, handle, &nest_url).await)
}

/// Replace the registered kit using the one the user holds
/// (`recovery-kit-replace-button`) — parses the held phrase, then runs the
/// SAME `create_kit` arm as [`recovery_create_kit`] with `prior: Some(..)`,
/// which re-puts the escrow blob in the same ceremony (the nest deletes the
/// resting row the moment a registration changes the pubkey, so skipping the
/// re-put would leave the account with no escrow at all).
async fn recovery_replace_kit(
    client: InnerClient,
    secret_hex: &str,
    handle: Option<&str>,
    phrase: &str,
) -> Result<RecoveryMintedJs, JsValue> {
    let prior = fauna_client_recovery::parse_kit(phrase).map_err(err_to_js)?;
    recovery_create_kit(client, secret_hex, handle, Some(&prior.recovery)).await
}

// ── trained-topic lifecycle helpers (topic-factors.md § Authoring surface) ──

/// A factor's 16-byte registry id, from the hex the SPA round-trips.
fn decode_factor_id(id_hex: &str) -> Result<Vec<u8>, JsValue> {
    let id = hex_bytes(id_hex)?;
    if id.len() != 16 {
        return Err(JsValue::from_str("a trained-topic id is 16 bytes"));
    }
    Ok(id)
}

/// The JS shape of a Trained-topics row. `id` is **hex** (not a byte array):
/// the SPA writes it straight into the row's `data-factor` attribute, and it
/// round-trips back through `renameTrainedTopic` / `deleteTrainedTopic` as a plain string.
#[derive(Serialize)]
struct JsTrainedTopicRow {
    id: String,
    name: String,
    factor_key: Option<String>,
    example_count: u32,
    learn_from_engagement: bool,
}

fn rows_to_js(rows: Vec<TrainedTopicRow>) -> Vec<JsTrainedTopicRow> {
    rows.into_iter()
        .map(|r| JsTrainedTopicRow {
            id: hex::encode(&r.id),
            name: r.name,
            factor_key: r.factor_key,
            example_count: r.example_count,
            learn_from_engagement: r.learn_from_engagement,
        })
        .collect()
}

/// The rejection shape of a trained-topic gesture.
///
/// **Structured, not a pre-formatted sentence.** `TrainedTopicsError::Display`
/// is hard-coded English; the cap and blank-name cases are user-facing product
/// messages that already have i18n strings on every app (linux renders the
/// cap through `personalization.trained_factor_cap`). Handing the SPA the Rust
/// sentence would ship untranslatable English into a localized UI — so the
/// boundary carries the `code` (+ the cap's `max`) and the SPA localizes.
/// `message` stays as the fallback for the transport-ish variants.
#[derive(Serialize)]
struct JsTopicsError {
    code: &'static str,
    max: Option<usize>,
    message: String,
}

fn topics_err_to_js(e: TrainedTopicsError) -> JsValue {
    let (code, max) = match &e {
        TrainedTopicsError::Cap(m) => ("cap", Some(*m)),
        TrainedTopicsError::BlankName => ("blank_name", None),
        TrainedTopicsError::Config(_) => ("config", None),
        TrainedTopicsError::Model(_) => ("model", None),
    };
    let message = e.to_string();
    to_js(&JsTopicsError {
        code,
        max,
        message: message.clone(),
    })
    .unwrap_or_else(|_| JsValue::from_str(&message))
}

/// The rejection shape of a **publish** (`topic-factors.md` § Publishing a
/// trained factor). Same discipline as [`JsTopicsError`], and a separate
/// mapping for the same reason: `PublishListError` has no `code()` of its own —
/// each boundary decides which cases are user-facing product messages worth a
/// stable key, and which collapse to a fallback sentence. Here only the two a
/// user can cause by typing get one; `max` carries the name bound the SPA
/// cannot know.
#[derive(Serialize)]
struct JsPublishError {
    code: &'static str,
    max: Option<usize>,
    message: String,
}

fn publish_err_to_js(e: PublishListError) -> JsValue {
    let (code, max) = match &e {
        PublishListError::Artifact(ListArtifactError::BlankName) => ("blank_name", None),
        PublishListError::Artifact(ListArtifactError::NameTooLong(_)) => {
            ("name_too_long", Some(MAX_LABELER_LIST_NAME_LEN))
        }
        PublishListError::Transport(_) => ("transport", None),
        _ => ("artifact", None),
    };
    let message = e.to_string();
    to_js(&JsPublishError {
        code,
        max,
        message: message.clone(),
    })
    .unwrap_or_else(|_| JsValue::from_str(&message))
}

/// The JS shape of a landed publish (mirrors
/// `fauna_client_personalization::publish::PublishedList`). `labeler_id` is hex
/// — the id a subscriber inspects, and the **derived** verifying key rather
/// than the publishing actor (§ Publishing: publishing is pseudonymous).
#[derive(Serialize)]
struct JsPublishedList {
    labeler_id: String,
    version: u64,
    entry_count: u32,
}

/// One kept exemplar, as the SPA hands it back from the review sheet.
///
/// Deliberately **not** the sheet's own row shape: an exemplar row carries the
/// post's `preview` text, and this type is where it becomes unmissable that the
/// preview does not cross — a published List is `content_id → score` and
/// nothing else.
#[derive(Deserialize)]
struct JsPublishEntry {
    post_id: String,
    score: i64,
}

/// The JS spelling of a **model** publish refusal. Same contract as
/// [`publish_err_to_js`]: a stable `code` the SPA branches on plus the bound the
/// crate knows and the SPA does not, never a pre-formatted sentence.
///
/// `empty_vocabulary` is the one code the List has no analogue for — it is the
/// refusal a user fixes by marking more posts rather than by editing the sheet,
/// so it must not arrive as the generic `artifact`.
fn publish_model_err_to_js(e: PublishModelError) -> JsValue {
    let (code, max) = match &e {
        PublishModelError::EmptyVocabulary => ("empty_vocabulary", None),
        PublishModelError::Artifact(TextModelArtifactError::BlankName) => ("blank_name", None),
        PublishModelError::Artifact(TextModelArtifactError::NameTooLong(_)) => {
            ("name_too_long", Some(MAX_LABELER_LIST_NAME_LEN))
        }
        PublishModelError::Transport(_) => ("transport", None),
        _ => ("artifact", None),
    };
    let message = e.to_string();
    to_js(&JsPublishError {
        code,
        max,
        message: message.clone(),
    })
    .unwrap_or_else(|_| JsValue::from_str(&message))
}

/// The JS shape of a landed model publish (mirrors
/// `fauna_client_personalization::publish::PublishedModel`). `labeler_id` is hex
/// — the **derived** verifying key, and the same id a List publish of this
/// factor would produce, since the kind is per-version under one publisher
/// identity.
#[derive(Serialize)]
struct JsPublishedModel {
    labeler_id: String,
    version: u64,
    ngram_count: u32,
    document_count: u32,
}

/// One kept n-gram, as the SPA hands it back from the Model review sheet.
///
/// Deliberately **not** the sheet's own row shape, for the same reason
/// [`JsPublishEntry`] is not the exemplar row: what a sheet renders and what it
/// publishes are two sets that happen to coincide today.
#[derive(Deserialize)]
struct JsPublishNgram {
    ngram: String,
    more: u32,
    less: u32,
}

/// A stable client-assigned destination id (the key in the `sync_db` state
/// tables). No `uuid` dep on wasm, so derive a 16-byte random hex id — the
/// field is opaque; only stability + uniqueness matter.
fn new_destination_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS RNG available for destination id");
    hex::encode(bytes)
}

/// Resolve a candidate destination nest: connect anonymously, prove
/// reachability and authorization by minting a `fauna.auth.handshake` as the
/// owner (a stranger is rejected — `require_registration`), whose signature
/// binds the identity the connection **proved** (a possession proof,
/// pin-checked for the origin — [`crate::bound_login_identity`]); that proven
/// id is the nest's `actor_pubkey` (which detects URL-change vs nest-change),
/// refused when `fauna.nest.info`'s claim disagrees
/// (`fauna_client_backup::trust::proven_destination_id`), and the claim
/// supplies only the domain. The wasm-safe twin of
/// `segment_backup::resolve_destination` (which builds a native
/// `NestClient`) — one anonymous WS carries both pre-identity kinds. Returns
/// `(actor_pubkey, domain)`.
async fn resolve_destination_inner(
    secret_hex: &str,
    url: &str,
) -> Result<([u8; 32], String), JsValue> {
    use fauna_protocol::auth::HandshakeReply;

    let keypair = keypair_from_secret_hex(secret_hex)?;
    let client = AnonymousWsRpcClient::connect(url)
        .map_err(|e| JsValue::from_str(&format!("connect to backup destination {url}: {e}")))?;
    // Handshake = reachability + authorization proof (the bearer is discarded).
    let req = crate::build_handshake_request(&client, url, &keypair).await?;
    let bound = hex_array_32(&req.nest_id)?;
    let _: HandshakeReply = client
        .request("fauna.auth.handshake", req)
        .await
        .map_err(err_to_js)?;
    // nest.info → the domain, and a claim that must agree with the proof (the
    // wasm `nestInfo` export drops `nest_id`, so this reads `NestInfoReply`
    // directly).
    let info: NestInfoReply = client
        .request("fauna.nest.info", NestInfoRequest::default())
        .await
        .map_err(err_to_js)?;
    let actor_pubkey = fauna_client_backup::trust::proven_destination_id(url, bound, &info.nest_id)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok((actor_pubkey, info.domain))
}

/// [`resolve_destination_inner`], but keeps the minted bearer and opens an
/// **authenticated** one-shot connection to the destination — for the add
/// flow, which reuses it to register the nest-writer grant
/// (`fauna.backup.writer_grant.register`, a USER-class kind the anonymous
/// pre-identity connection cannot call). Mirrors the box-recovery reader's
/// mint-then-reconnect shape (`fauna-onboarding-machine`'s
/// `WasmRecoveryConfigReader`): the anonymous connection's handshake reply
/// already carries a bearer (discarded by `resolve_destination_inner`), so
/// this reuses that single round trip rather than a second handshake.
pub(crate) async fn resolve_and_authorize_destination_inner(
    secret_hex: &str,
    url: &str,
) -> Result<([u8; 32], String, TokenWsRpcClient), JsValue> {
    use fauna_protocol::auth::HandshakeReply;

    let keypair = keypair_from_secret_hex(secret_hex)?;
    let actor_id_hex = hex::encode(keypair.actor_id().0);
    let anon = AnonymousWsRpcClient::connect(url)
        .map_err(|e| JsValue::from_str(&format!("connect to backup destination {url}: {e}")))?;
    let req = crate::build_handshake_request(&anon, url, &keypair).await?;
    // The identity the handshake signature binds — proven over this
    // connection, pin-checked for the origin — is the destination's id, both
    // at enroll and on every later connection (`make_backup_connect`).
    let bound = hex_array_32(&req.nest_id)?;
    let handshake: HandshakeReply = anon
        .request("fauna.auth.handshake", req)
        .await
        .map_err(err_to_js)?;
    let info: NestInfoReply = anon
        .request("fauna.nest.info", NestInfoRequest::default())
        .await
        .map_err(err_to_js)?;
    let actor_pubkey = fauna_client_backup::trust::proven_destination_id(url, bound, &info.nest_id)
        .map_err(|e| JsValue::from_str(&e))?;
    drop(anon); // release the anonymous connection; the authed session follows.

    let authed = TokenWsRpcClient::connect(url, &actor_id_hex, &handshake.token).map_err(|e| {
        JsValue::from_str(&format!("authed connect to backup destination {url}: {e}"))
    })?;
    Ok((actor_pubkey, info.domain, authed))
}

// ── fauna.profile.* edit-form free functions ────────────────────────────
//
// The read-modify-write + decode logic for the text-only profile edit form
// lives once in shared `fauna_client_profile` (priority #2), the wasm twin of
// the `#[uniffi::export]` `decode_profile_display` / `build_edited_profile` in
// libs/fauna-ffi/src/profile.rs and the linux native
// apps/fauna-linux/src/views/profile/edit.rs. These thin wrappers only marshal
// hex/JSON ↔ the shared core types; the `WsRpcClient::{profileGet,profileSet}`
// methods above carry the bytes over the wire.

/// Decode a stored profile `body` (from `WsRpcClient::profileGet`) to the
/// editable fields as JSON — `{"display_name":..,"bio":..,"links":
/// [{"label":..,"uri":..}],"avatar_hash_hex":..,"banner_hash_hex":..}` — which
/// the edit form populates. The two image hashes let the form render the
/// current picture and offer a remove affordance; fetch their bytes over the
/// ordinary blob download path. Over the shared
/// `fauna_client_profile::decode_profile_display`.
#[wasm_bindgen]
pub fn decode_profile_display(body: &[u8]) -> Result<String, JsValue> {
    let d = fauna_client_profile::decode_profile_display(body).map_err(err_to_js)?;
    let json = serde_json::json!({
        "display_name": d.display_name,
        "bio": d.bio,
        "links": d.links,
        "avatar_hash_hex": d.avatar.map(|h| hex::encode(h.digest())),
        "banner_hash_hex": d.banner.map(|h| hex::encode(h.digest())),
    });
    serde_json::to_string(&json).map_err(err_to_js)
}

/// Where a knock sent from the profile page of `actor_id_hex` goes —
/// `inboxSend`'s `recipient_nest_url` (`undefined` = this nest). Over the
/// shared `fauna_client_profile::knock_recipient_nest_url`, whose rule
/// `profile.md` § Where logic lives → *Request contact routing* owns;
/// `profile_body` is the bytes the page's open already fetched.
#[wasm_bindgen(js_name = knockRecipientNestUrl)]
pub fn knock_recipient_nest_url(
    actor_id_hex: &str,
    profile_body: &[u8],
    own_nest_url: &str,
) -> Option<String> {
    let actor = fauna_core::identity::ActorId::from_hex(actor_id_hex).ok()?;
    fauna_client_profile::knock_recipient_nest_url(&actor, profile_body, own_nest_url)
}

/// Whether the ward's own `contact_requests` (the `familyStatus` reply's list,
/// passed back as received) hold an outstanding ask for `peer_actor_id_hex` —
/// what `contact-request-pending` renders from. Over the shared
/// `fauna_client_family::ward_asks::contact_ask_pending` (case-insensitive; a
/// non-hex id matches nothing).
#[wasm_bindgen(js_name = wardContactAskPending)]
pub fn ward_contact_ask_pending(asks: JsValue, peer_actor_id_hex: &str) -> Result<bool, JsValue> {
    let asks: Vec<fauna_protocol::family::FamilyContactRequestInfo> = from_js(asks)?;
    Ok(fauna_client_family::ward_asks::contact_ask_pending(
        &asks,
        peer_actor_id_hex,
    ))
}

/// The live state of the ward's feed-source ask for one
/// `(bridge_id, operation, target)` triple — `"pending"`, `"approved"` (the
/// try-again prompt, never an auto-retry), or `undefined` when no live ask
/// covers it. `asks` is the `familyStatus` reply's `feed_requests` as
/// received. Over the shared `fauna_client_family::ward_asks::feed_request_state`.
#[wasm_bindgen(js_name = wardFeedRequestState)]
pub fn ward_feed_request_state(
    asks: JsValue,
    bridge_id: &str,
    operation: &str,
    target: &str,
) -> Result<Option<String>, JsValue> {
    use fauna_client_family::ward_asks::{FeedRequestState, feed_request_state};
    let asks: Vec<fauna_protocol::family::FamilyFeedRequestInfo> = from_js(asks)?;
    Ok(
        feed_request_state(&asks, bridge_id, operation, target).map(|s| match s {
            FeedRequestState::Pending => "pending".to_string(),
            FeedRequestState::Approved => "approved".to_string(),
        }),
    )
}

/// A feed-source operation, as the ward's ask names it — the wasm twin of the
/// FFI's `FfiFeedSourceOperation`, so the SPA passes a variant rather than
/// spelling the wire string itself.
#[wasm_bindgen(js_name = FeedSourceOperation)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmFeedSourceOperation {
    Link,
    Follow,
    Feed,
}

/// The canonical wire string for a feed-source operation — what a refused
/// triple's `operation` carries and `familyFeedSourceRequest` takes. Shared
/// `FeedSourceOperation::as_str`, the FFI's `feed_source_operation_wire` twin.
#[wasm_bindgen(js_name = feedSourceOperationWire)]
pub fn feed_source_operation_wire(operation: WasmFeedSourceOperation) -> String {
    use fauna_core::data::FeedSourceOperation as Op;
    match operation {
        WasmFeedSourceOperation::Link => Op::Link,
        WasmFeedSourceOperation::Follow => Op::Follow,
        WasmFeedSourceOperation::Feed => Op::Feed,
    }
    .as_str()
    .to_string()
}

/// The read-modify-write sign step for the profile edit form. `secret_hex` is
/// the 64-char owner secret; `base_body` is the current stored bytes (re-edit)
/// or `None`/`undefined` (first publish); `links_json` is
/// `[{"label":..,"uri":..}]`. Overwrites only `display_name` / `bio` / `links`,
/// preserving every other identity field (or minimal defaults on first
/// publish). A stored base that is unsigned, or signed for anyone but this
/// identity or a predecessor this browser's registry records, is refused rather
/// than re-signed ([`profile_predecessors`]). Returns the signed `EmbedAsBytes`
/// wire ready for `WsRpcClient::profileSet`. Over the shared
/// `fauna_client_profile::build_edited_profile`.
#[wasm_bindgen]
pub fn build_edited_profile(
    secret_hex: &str,
    base_body: Option<Vec<u8>>,
    display_name: Option<String>,
    bio: Option<String>,
    links_json: &str,
) -> Result<Vec<u8>, JsValue> {
    let keypair = keypair_from_secret_hex(secret_hex)?;
    let links: Vec<ProfileLink> = serde_json::from_str(links_json)
        .map_err(|e| JsValue::from_str(&format!("bad links json: {e}")))?;
    fauna_client_profile::build_edited_profile(
        &keypair,
        base_body.as_deref(),
        &profile_predecessors(&keypair),
        display_name,
        bio,
        links,
    )
    .map_err(err_to_js)
}

/// `build_edited_profile` plus the two image fields — the full edit-form write
/// once web can set or remove a profile picture / banner. Over the shared
/// `fauna_client_profile::build_edited_profile_with_images`.
///
/// wasm-bindgen enums cannot carry a payload, so each field's three-state edit
/// arrives as two arguments (the `ProfileImageEdit::from_parts` adapter):
/// `*_clear = true` removes the picture; otherwise `Some(hex)` sets it and
/// `None`/`undefined` leaves it untouched. `clear` wins over a supplied hash.
///
/// The picture bytes are uploaded separately, through the ordinary public-post
/// blob path (`media.md` § Encryption at rest — avatar / banner blobs are
/// signed plaintext, the same shape as public-post attachments); this records
/// the resulting hash on the signed profile.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn build_edited_profile_with_images(
    secret_hex: &str,
    base_body: Option<Vec<u8>>,
    display_name: Option<String>,
    bio: Option<String>,
    links_json: &str,
    avatar_clear: bool,
    avatar_hash_hex: Option<String>,
    banner_clear: bool,
    banner_hash_hex: Option<String>,
) -> Result<Vec<u8>, JsValue> {
    let keypair = keypair_from_secret_hex(secret_hex)?;
    let links: Vec<ProfileLink> = serde_json::from_str(links_json)
        .map_err(|e| JsValue::from_str(&format!("bad links json: {e}")))?;
    let avatar = fauna_client_profile::ProfileImageEdit::from_parts(
        avatar_clear,
        avatar_hash_hex.as_deref(),
    )
    .map_err(err_to_js)?;
    let banner = fauna_client_profile::ProfileImageEdit::from_parts(
        banner_clear,
        banner_hash_hex.as_deref(),
    )
    .map_err(err_to_js)?;
    fauna_client_profile::build_edited_profile_with_images(
        &keypair,
        base_body.as_deref(),
        &profile_predecessors(&keypair),
        display_name,
        bio,
        links,
        avatar,
        banner,
    )
    .map_err(err_to_js)
}

/// The custodied deployment-seed box list read **offline** from this device's
/// own account store — the nest-less twin of [`WsRpcClient::deploymentSeeds`],
/// for the case recovery exists for: the saved nest is the dead box, or no nest
/// URL is stored at all (`box-recovery.md` § The plane-era recovery floor, *(b)
/// The reads* — the local read, which replaced the device-local
/// replica for this kind). A **free function** (not a `WsRpcClient` method)
/// precisely because the offline recovery flow has no live client. Reads the
/// IndexedDB store the account runtime opens (`StoreRoot::platform()`) without a
/// runtime, a nest or a generation key; a store that does not exist answers `[]`
/// and is never created. Resolves to the same `[{nest_actor_id, domain}]` array
/// as the reachable-nest getter; the raw seed never crosses into JS.
///
/// Rejects on a malformed secret, a store that fails to open, or a stored row
/// that does not decode strictly (never a silently skipped box).
#[wasm_bindgen(js_name = recoveryBoxesLocal, unchecked_return_type = "Promise<Array<{ nest_actor_id: string; domain: string | null }>>")]
pub fn recovery_boxes_local(secret_hex: String) -> js_sys::Promise {
    future_to_promise(async move {
        let secret = hex_array_32(&secret_hex)?;
        let seeds = resolve_recovery_seeds(&secret, None).await?;
        to_js(&recovery_box_rows(&seeds))
    })
}

/// The `recover-selfhosted-command` read **offline** — the nest-less twin of
/// [`WsRpcClient::recoverSelfhostedCommand`] over this device's own account
/// store (the local read of `box-recovery.md` § The plane-era recovery floor,
/// *(b)*), for the recovery flow that has no reachable nest. Rejects when the
/// store custodies no seed for that box.
#[wasm_bindgen(js_name = recoverSelfhostedCommandLocal, unchecked_return_type = "Promise<string>")]
pub fn recover_selfhosted_command_local(
    secret_hex: String,
    nest_actor_id_hex: String,
) -> js_sys::Promise {
    future_to_promise(async move {
        let secret = hex_array_32(&secret_hex)?;
        let seeds = resolve_recovery_seeds(&secret, None).await?;
        selfhosted_command_js(&seeds, &nest_actor_id_hex)
    })
}

/// Cheaply-cloneable handle to the SPA's singleton browser WS-RPC client.
/// Construction opens one WebSocket per actor session (the subprotocol-bearer
/// handshake) and starts the reconnect loop; the inner `Rc` is shared by every
/// typed call.
/// The web [`HostAddressProbe`] — the browser has no DNS resolver and no UDP
/// sockets, so both capabilities are stubs (`resolve_host` → empty,
/// `stun_public_ipv4` → `None`). The web app therefore reports only a
/// public-IP-literal dial-address and skips a name/LAN dial (the native apps,
/// with a real resolver, additionally report a public name they can resolve). A
/// few-absent-on-web capability, consistent with the client family (priority #1).
struct WebHostAddressProbe;

impl HostAddressProbe for WebHostAddressProbe {
    async fn resolve_host(&self, _host: &str) -> Vec<std::net::IpAddr> {
        Vec::new()
    }
    async fn stun_public_ipv4(&self) -> Option<std::net::Ipv4Addr> {
        None
    }
}

/// JS-friendly projection of [`HostAddressOutcome`] for `reportHostAddress` — the
/// SPA logs it fire-and-forget (`{ kind, nest_ipv4?, error? }`).
#[derive(Serialize)]
struct HostAddressReport {
    /// `"reported" | "skipped_no_public_ip" | "failed"`.
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    nest_ipv4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl From<HostAddressOutcome> for HostAddressReport {
    fn from(o: HostAddressOutcome) -> Self {
        match o {
            HostAddressOutcome::Reported(req) => Self {
                kind: "reported",
                nest_ipv4: Some(req.nest_ipv4),
                error: None,
            },
            HostAddressOutcome::SkippedNoPublicIp => Self {
                kind: "skipped_no_public_ip",
                nest_ipv4: None,
                error: None,
            },
            HostAddressOutcome::Failed(error) => Self {
                kind: "failed",
                nest_ipv4: None,
                error: Some(error),
            },
        }
    }
}

#[wasm_bindgen]
pub struct WsRpcClient {
    inner: InnerClient,
    /// Lowercase-hex actor id this session authenticates as — the key of the
    /// per-actor supervision-snapshot slot `family_status` rewrites on every
    /// successful read (family-safety.md § Content policy, clause 2).
    actor_id_hex: String,
    /// This session's author-pump once-per-connect latch. The pump tick
    /// (`subscriptionsReconcileOnce`) rebuilds its author per call, so the
    /// latch lives here, with the session, or the once-per-connect pass would
    /// run on every 30 s tick.
    subscriptions_connect_pass: ConnectPassLatch,
}

impl WsRpcClient {
    /// The shared transport client, for a sibling module's face that rides
    /// this session (`region::WasmRegionPlane::refresh`).
    pub(crate) fn inner_client(&self) -> InnerClient {
        self.inner.clone()
    }
}

#[wasm_bindgen]
impl WsRpcClient {
    /// Open the WS-RPC connection and start the reconnect loop. Returns
    /// immediately. `token_provider` is a JS `(forceRefresh: boolean) =>
    /// Promise<string>` yielding the bearer (called on connect, and again with
    /// `force = true` after a 4401 close).
    #[wasm_bindgen(constructor)]
    pub fn new(
        node_url: String,
        actor_id_hex: String,
        token_provider: js_sys::Function,
    ) -> WsRpcClient {
        WsRpcClient {
            inner: InnerClient::connect(node_url, actor_id_hex.clone(), token_provider),
            actor_id_hex,
            subscriptions_connect_pass: ConnectPassLatch::default(),
        }
    }

    /// Permanently tear the client down: stop the reconnect loop, close any
    /// live socket, fail pending requests fast. One-way — build a new client to
    /// talk again. The SPA's singleton owner (`rpc.ts` `getClient`) calls this
    /// when the identity **or the target nest URL** changes, so a superseded
    /// client can't keep dialing the stale nest forever (the mid-claim
    /// crash-recovery wrong-nest loop). Idempotent.
    pub fn close(&self) {
        self.inner.close();
    }

    /// `"connecting" | "connected" | "disconnected" | "unreachable"` — for the
    /// SPA's status surface (mirrors `fauna_client::types::ConnectionState`).
    /// `unreachable` is the settled-failure state; feed it to
    /// [`crate::connection_state_label`] rather than switching on it in TS.
    /// Pace this client's reconnect retries, or restore them — the web leg of
    /// `fauna_e2e_agent::RECONNECT_BACKOFF`, taking its payload JSON verbatim
    /// (`{"initial_ms": N, "max_ms": M}`, or `{}` to restore) and parsing it with
    /// the natives' own `reconnect_backoff_bounds`, so every app refuses the same
    /// malformed payloads. Rejects on one (convention 11: a pace that did not
    /// land leaves the production one in force). `test-helpers` only.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setReconnectBackoffForTest)]
    pub fn set_reconnect_backoff_for_test(&self, payload_json: &str) -> Result<(), JsValue> {
        let payload: serde_json::Value = serde_json::from_str(payload_json)
            .map_err(|e| JsValue::from_str(&format!("reconnect_backoff payload: {e}")))?;
        let bounds = fauna_e2e_contract::reconnect_backoff_bounds(&payload)
            .map_err(|e| JsValue::from_str(&format!("reconnect_backoff: {e}")))?;
        self.inner.set_reconnect_backoff_for_test(bounds);
        Ok(())
    }

    #[wasm_bindgen(js_name = connectionState)]
    pub fn connection_state(&self) -> String {
        match self.inner.connection_state() {
            ConnectionState::Connecting => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Disconnected => "disconnected",
            ConnectionState::Unreachable => "unreachable",
        }
        .to_string()
    }

    /// Why this client's reconnect loop stopped for good — the localized error
    /// every request on it now fails with — or `undefined` while it can still
    /// connect. `rpc.ts`'s `ensureConnected` reads it, so a call fails at once
    /// with why instead of polling out its connect wait. Mirrors native
    /// `NestClient::supervisor_stop`.
    #[wasm_bindgen(js_name = supervisorStop)]
    pub fn supervisor_stop(&self) -> Option<String> {
        self.inner.supervisor_stop().map(|e| e.to_string())
    }

    /// The session-ending verdict the stop is, typed — `"sign_in_refused"`
    /// when the reconnect loop stopped because the nest refused this
    /// identity's re-mint (`fauna.auth.not_registered`: suspended or removed
    /// while signed in), else `undefined`. The wasm twin of native
    /// `NestClient::supervisor_stop` read through
    /// `NestClientError::session_ending_verdict`: `rpc.ts` routes it to the
    /// launch surface (`security.md` § Post-auth surfacing), never by matching
    /// [`Self::supervisor_stop`]'s localized text.
    #[wasm_bindgen(js_name = sessionEndingVerdict)]
    pub fn session_ending_verdict(&self) -> Option<String> {
        self.inner
            .stopped_by_sign_in_refusal()
            .then(|| "sign_in_refused".to_string())
    }

    /// Register a JS `() => void` fired on every reconnect (a `Connected` after
    /// the first connect — never the initial one), so the SPA re-hydrates
    /// surfaces with no poll backstop (the feed). The wasm twin of native
    /// `subscribe_reconnects` (`transport.md` § Push events). Replaces any
    /// previously-registered callback.
    #[wasm_bindgen(js_name = setOnReconnected)]
    pub fn set_on_reconnected(&self, cb: js_sys::Function) {
        self.inner.set_on_reconnected(cb);
    }

    /// Register a JS `(state: string) => void` fired on every connection-state
    /// *transition* (`"connecting" | "connected" | "disconnected"`), driving the
    /// SPA's global `connection-status` indicator. Distinct from
    /// `setOnReconnected` (which fires only on a reconnect): this also fires on
    /// Connecting/Disconnected, so a Watchtower-swap gap shows live without
    /// surfacing as an error. The wasm twin of a native app observing
    /// `NestClient::connection_state()`. Replaces any previously-registered callback.
    #[wasm_bindgen(js_name = setOnConnectionStateChanged)]
    pub fn set_on_connection_state_changed(&self, cb: js_sys::Function) {
        self.inner.set_on_connection_state_changed(cb);
    }

    /// Register a JS `(kind: string, payload: object) => void` fired for every
    /// server push on this connection. The wasm twin of native
    /// `NestClient::subscribe_pushes` (`transport.md` § Push events): the SPA
    /// matches on the same `fauna_protocol::PushEvent` kind strings a native
    /// app does (`"fauna.calendar.changed"`, `"fauna.notification"`, `"fauna.knock"`,
    /// …), and `payload` is that variant's payload object.
    ///
    /// This is the SPA's **only** inbound push path — it rides the one
    /// authenticated WS-RPC socket. (It replaced `$lib/ws.ts`, a second raw
    /// WebSocket that still spoke the retired `?token=` query-auth form, which
    /// the nest answers with 401.) Registration survives reconnects. Replaces any
    /// previously-registered callback.
    #[wasm_bindgen(js_name = setOnPushEvent)]
    pub fn set_on_push_event(&self, cb: js_sys::Function) {
        self.inner.set_on_push_event(cb);
    }

    /// Which client surfaces a push `kind` has made stale — the wasm twin of
    /// native `PushEvent::invalidates()` (`transport.md` § Which surfaces a
    /// push invalidates). The SPA's `setOnPushEvent` callback already receives
    /// this exact `kind` string; call this with it instead of hand-deriving a
    /// per-kind refresh table in TypeScript. Returns `{ feed, notifications,
    /// knocks, contacts, account, atproto, events, media }`; an unrecognized
    /// kind answers all-`false`, matching how a native app ignores an unmodeled
    /// kind. Static (no connection needed) — a plain function of the kind
    /// string, called as `WsRpcClient.staleSurfacesForPushKind(kind)`.
    #[wasm_bindgen(js_name = staleSurfacesForPushKind)]
    pub fn stale_surfaces_for_push_kind(kind: &str) -> Result<JsValue, JsValue> {
        to_js(&StaleSurfaces::for_kind(kind))
    }

    /// The full reconnect sweep (`StaleSurfaces::on_reconnect()`) — every
    /// surface a dropped push might have staled, since a reconnect resets the
    /// push `seq` to 0 and the gap may have swallowed anything. Call once from
    /// `setOnReconnected` instead of hand-listing every surface to re-pull; the
    /// SPA still owns folding these logical flags onto whatever it actually
    /// re-reads. Static, same shape as `staleSurfacesForPushKind`.
    #[wasm_bindgen(js_name = staleSurfacesOnReconnect)]
    pub fn stale_surfaces_on_reconnect() -> Result<JsValue, JsValue> {
        to_js(&StaleSurfaces::on_reconnect())
    }

    // ── fauna.protocol.echo (e2e probe) ─────────────────────────────

    /// `fauna.protocol.echo` — round-trips `data` through the dispatcher and
    /// resolves the echoed bytes. The web twin of the Linux `rpc_echo` probe.
    #[wasm_bindgen(js_name = rpcEcho)]
    pub fn rpc_echo(&self, data: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply: EchoReply = client
                .request(
                    "fauna.protocol.echo",
                    EchoRequest {
                        data,
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(err_to_js)?;
            Ok(js_sys::Uint8Array::from(reply.data.as_slice()).into())
        })
    }

    // ── The shared rpc port: this socket, lent to the other wasm chunks ──

    /// Run one request on this client's socket for ANOTHER wasm chunk —
    /// the owner's half of the shared rpc port (`fauna_rpc_wasm::shared_port`;
    /// `docs/goal/architecture/apps/web.md` § Transport). `payload` is the
    /// request's canonical DAG-CBOR bytes, `idempotencyKey` the 16-byte
    /// envelope key the chunk chose, and the promise resolves to the reply's
    /// canonical bytes — or rejects with the tagged refusal object
    /// (`WsRpcError::to_js`) the chunk decodes back into the same error this
    /// client would have returned. Same registry deadline, same wait for a
    /// mid-reconnect gap as every request of the core's own. `rpc.ts`'s
    /// `sharedRpcPort` wraps it; page-machine chunks (folders, media, backups,
    /// labeler catalog, atproto settings) build their clients over that port
    /// instead of dialling sockets of their own — one WebSocket per actor.
    #[wasm_bindgen(js_name = requestRaw)]
    pub fn request_raw(
        &self,
        kind: String,
        idempotency_key: Vec<u8>,
        payload: Vec<u8>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let idem: [u8; 16] = idempotency_key.as_slice().try_into().map_err(|_| {
                fauna_rpc_wasm::WsRpcError::Codec(format!(
                    "requestRaw: idempotency key must be 16 bytes, got {}",
                    idempotency_key.len()
                ))
                .to_js()
            })?;
            let reply = client
                .request_raw_bytes(&kind, idem, &payload)
                .await
                .map_err(|e| e.to_js())?;
            Ok(js_sys::Uint8Array::from(reply.as_slice()).into())
        })
    }

    /// The nest base URL this socket is connected on — the CURRENT value (the
    /// reconnect loop SRV-swaps it on a serving-port change), which is why
    /// the shared rpc port reads it here rather than from the SPA's stored
    /// URL. Mirrors native `NestClient::nest_url`.
    #[wasm_bindgen(js_name = nestUrl)]
    pub fn nest_url(&self) -> String {
        self.inner.nest_url()
    }

    /// The lowercase-hex actor id this socket authenticates as.
    #[wasm_bindgen(js_name = actorIdHex)]
    pub fn actor_id_hex_js(&self) -> String {
        self.actor_id_hex.clone()
    }

    // ── fauna.setup.status ──────────────────────────────────────────

    /// `fauna.setup.status` → `SetupStatusReply` (`{domain, dns_configured,
    /// tls_active, email_enabled, admin_exists, claimed, version, …}`). The
    /// authed twin of the onboarding machine's anonymous `probeSetupStatus`.
    /// Pure read.
    #[wasm_bindgen(js_name = setupStatus)]
    pub fn setup_status(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply: SetupStatusReply = client
                .request(
                    "fauna.setup.status",
                    SetupStatusRequest {
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.dns.set_host_address (client host-address reporting) ───

    /// Report the nest's **public** IP so it gates ACME HTTP-01 on the *strong*
    /// resolve-check and assembles the apex/`mail.` records — the web twin of the
    /// native `report_host_address` FFI and linux's direct drive
    /// (`domains-and-tls-bootstrap.md` § Host-address acquisition). Call at
    /// onboarding/claim success and at the SPA's universal post-auth hook,
    /// **admin-gated** by the caller; fire-and-forget (the SPA logs the returned
    /// `{ kind, nest_ipv4?, error? }`). Never publishes a private/LAN address (the
    /// safety invariant lives in the shared decision fn); the browser has no
    /// resolver / UDP, so web reports only a public-IP-literal dial and skips
    /// otherwise. Idempotent last-writer-wins nest-side.
    #[wasm_bindgen(js_name = reportHostAddress)]
    pub fn report_host_address(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            // The dial-address the client used to reach the nest, read before the
            // client is moved into the typed DNS caller.
            let dial_url = client.nest_url();
            let dns = DnsAdminClient::new(client);
            let outcome = fauna_client_dns::host_address::report_host_address(
                &dns,
                &dial_url,
                &WebHostAddressProbe,
            )
            .await;
            to_js(&HostAddressReport::from(outcome))
        })
    }

    // ── fauna.conversations.keypackage.* ────────────────────────────

    /// `fauna.conversations.keypackage.count` → non-destructive count of the
    /// actor's remaining non-expired MLS key packages. `actor_id_hex` is the
    /// hex actor id (self, for the settings-page top-up gauge). Pure read; the
    /// WS-RPC twin of the deleted `GET /api/v1/keypackage/{actor}/count`.
    #[wasm_bindgen(js_name = keypackageCount)]
    pub fn keypackage_count(&self, actor_id_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ConversationsClient::new(client)
                .keypackage_count(actor_id_hex)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_f64(reply.count as f64))
        })
    }

    /// `fauna.conversations.keypackage.upload` → publish one or more MLS key
    /// packages for the calling actor (implicit from the connection).
    /// `packages_hex` are the hex-encoded blobs the wasm MLS engine generated;
    /// `last_resort = false` for the consumable login top-up pool. Resolves the
    /// stored count. The WS-RPC twin of the deleted `POST /api/v1/keypackage/{actor}`.
    #[wasm_bindgen(js_name = keypackageUpload)]
    pub fn keypackage_upload(
        &self,
        packages_hex: Vec<String>,
        last_resort: bool,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let packages: Vec<Vec<u8>> = packages_hex
                .iter()
                .map(hex::decode)
                .collect::<Result<_, _>>()
                .map_err(err_to_js)?;
            let reply = ConversationsClient::new(client)
                .keypackage_upload(packages, last_resort)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_f64(reply.stored as f64))
        })
    }

    // ── fauna.web.* (web-content authoring) ─────────────────────────
    //
    // The thin `WebClient<R>` (`fauna-client-web`) over the singleton WS-RPC
    // client — the web twin of the linux native `WebClient<Arc<NestClient>>` +
    // the native FFI `FfiWebClient`. Direct-call thin client (like
    // `bridgesList` / `keypackageCount`), not a stateful machine: the
    // user web-settings subdomain toggle + admin-web apex picker render off
    // these four calls + the pure `webSubdomainView` / `webApexUrl` projections
    // (free fns in `lib.rs`). See `docs/goal/behavior/web-content-hosting.md`
    // § Admin apex hosting / § Published-post management.

    /// `fauna.web.get_subdomain_enabled` → the calling actor's per-user
    /// subdomain opt-in (default `false`). The web-settings toggle's state.
    /// Caller-scoped, pure read.
    /// `fauna.nest.info` → **the domain this nest actually routes web content
    /// on** — the only legitimate `domain` input to `webSubdomainView` /
    /// `webSiteLinkView`. Resolves a string; an EMPTY string is a real answer
    /// ("this nest serves no web content") and must be passed through, not
    /// replaced.
    #[wasm_bindgen(js_name = webServingDomain)]
    pub fn web_serving_domain(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let domain = WebClient::new(client)
                .serving_domain()
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(&domain))
        })
    }

    #[wasm_bindgen(js_name = webGetSubdomainEnabled)]
    pub fn web_get_subdomain_enabled(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let enabled = WebClient::new(client)
                .get_subdomain_enabled()
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_bool(enabled))
        })
    }

    /// `fauna.web.set_subdomain_enabled` → flip the calling actor's own
    /// subdomain hosting; resolves the nest-confirmed state. Non-optimistic —
    /// the toggle's `data-state` renders off this echo, so reading it back
    /// proves the round-trip.
    #[wasm_bindgen(js_name = webSetSubdomainEnabled)]
    pub fn web_set_subdomain_enabled(&self, enabled: bool) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let enabled = WebClient::new(client)
                .set_subdomain_enabled(enabled)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_bool(enabled))
        })
    }

    /// `fauna.web.get_apex_actor` → the nest-wide apex designation as a
    /// `Uint8Array` actor id, or `null` (the built-in info page). Admin-class,
    /// pure read.
    #[wasm_bindgen(js_name = webGetApexActor)]
    pub fn web_get_apex_actor(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let actor = WebClient::new(client)
                .get_apex_actor()
                .await
                .map_err(err_to_js)?;
            Ok(match actor {
                Some(id) => js_sys::Uint8Array::from(id.as_slice()).into(),
                None => JsValue::NULL,
            })
        })
    }

    /// `fauna.web.set_apex_actor` → designate (a `number[]`/`Uint8Array` actor
    /// id) or clear (`null`) the deployment apex; resolves the nest-confirmed
    /// designation. Admin-class.
    #[wasm_bindgen(js_name = webSetApexActor)]
    pub fn web_set_apex_actor(&self, actor_id: JsValue) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let actor: Option<Vec<u8>> = if actor_id.is_null() || actor_id.is_undefined() {
                None
            } else {
                Some(from_js(actor_id)?)
            };
            let actor = WebClient::new(client)
                .set_apex_actor(actor)
                .await
                .map_err(err_to_js)?;
            Ok(match actor {
                Some(id) => js_sys::Uint8Array::from(id.as_slice()).into(),
                None => JsValue::NULL,
            })
        })
    }

    // ── fauna.web.publish.* / fauna.web.paywall.* ───────────────────
    //
    // The published-post management surface (web-content-hosting.md
    // § Published-post management): the feed ⋯-overflow verbs + the
    // `web-settings` published-posts section. All caller-scoped. The link
    // strings themselves come from the pure `webSiteLinkView` /
    // `webPostPageUrl` / `webTokenedUrl` projections in `lib.rs`, never from
    // SPA-side string building.

    /// `fauna.web.publish.set` → the **effective** slug (`null` slug ⇒ the
    /// nest's post-id-hex default). Build the copy-link URL from this echo,
    /// never from the requested slug.
    #[wasm_bindgen(js_name = webPublishSet)]
    pub fn web_publish_set(&self, post_id: JsValue, slug: Option<String>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let post_id: Vec<u8> = from_js(post_id)?;
            let slug = WebClient::new(client)
                .publish_set(post_id, slug)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(&slug))
        })
    }

    /// `fauna.web.publish.unset` → take a published post down. Idempotent and
    /// reversible, so the UI offers it as a one-tap verb with no confirm step.
    #[wasm_bindgen(js_name = webPublishUnset)]
    pub fn web_publish_unset(&self, post_id: JsValue) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let post_id: Vec<u8> = from_js(post_id)?;
            let ok = WebClient::new(client)
                .publish_unset(post_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_bool(ok))
        })
    }

    /// `fauna.web.domain.get` → `[{ domain, status }, …]`, already narrowed to
    /// what `webSiteLinkView` takes — pass it straight through as that call's
    /// `domains` argument. Only an `active` row resolves to an origin.
    #[wasm_bindgen(js_name = webDomainGet)]
    pub fn web_domain_get(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let rows = WebClient::new(client)
                .domain_get()
                .await
                .map_err(err_to_js)?;
            to_js(&rows)
        })
    }

    /// `fauna.web.publish.list` → `{ posts, rendered_pages_down }` — the
    /// `web-published-posts-list` rows (`{ post_id, slug, gated_tier? }`; a row
    /// without `gated_tier` is ungated: no paywall-link affordance) plus
    /// whether the caller's rendered pages are down (the `web-settings` status
    /// line).
    #[wasm_bindgen(js_name = webPublishedSite)]
    pub fn web_published_site(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let site = WebClient::new(client)
                .publish_list()
                .await
                .map_err(err_to_js)?;
            let posts = to_js(&site.posts)?;
            let out = js_sys::Object::new();
            js_sys::Reflect::set(&out, &"posts".into(), &posts)?;
            js_sys::Reflect::set(
                &out,
                &"rendered_pages_down".into(),
                &site.rendered_pages_down.into(),
            )?;
            Ok(out.into())
        })
    }

    /// `fauna.web.paywall.mint_token` → `{ token, expires, path }` for one of
    /// the caller's own paywalled resources. `slug` targets a published+gated
    /// post; pass `path` instead for a paywalled `web` file. Short-lived by
    /// ratified design and freely re-mintable — there is no TTL knob.
    #[wasm_bindgen(js_name = webPaywallMintToken)]
    pub fn web_paywall_mint_token(
        &self,
        slug: Option<String>,
        path: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let target = match (slug, path) {
                (_, Some(p)) => fauna_client_web::PaywallTarget::FilePath { path: p },
                (Some(s), None) => fauna_client_web::PaywallTarget::PostSlug { slug: s },
                (None, None) => {
                    return Err(JsValue::from_str(
                        "webPaywallMintToken needs either a slug or a path",
                    ));
                }
            };
            let minted = WebClient::new(client)
                .paywall_mint_token(target)
                .await
                .map_err(err_to_js)?;
            to_js(&minted)
        })
    }

    // ── fauna.bridges.* ─────────────────────────────────────────────

    /// `fauna.bridges.list` → `BridgeStatus[]`.
    #[wasm_bindgen(js_name = bridgesList)]
    pub fn bridges_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = BridgesClient::new(client).list().await.map_err(err_to_js)?;
            to_js(&reply.bridges)
        })
    }

    /// `fauna.bridges.link` (`forbid_replay`) → `LinkReply { linked, identity,
    /// redirect_url }`. `params` is a plain JS object → `Value::Map`.
    #[wasm_bindgen(js_name = bridgesLink)]
    pub fn bridges_link(
        &self,
        bridge_id: String,
        mode: String,
        params: JsValue,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let params: Value = from_js(params)?;
            let reply = BridgesClient::new(client)
                .link(bridge_id, mode, params)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.bridges.link_challenge` → `LinkChallengeReply { challenge,
    /// expires_at, payload }` — the proof-of-possession challenge an external
    /// signer signs before `bridgesLink` in that mode (Nostr `nip07`: `payload`
    /// is the unsigned event `window.nostr.signEvent` takes).
    #[wasm_bindgen(js_name = bridgesLinkChallenge)]
    pub fn bridges_link_challenge(&self, bridge_id: String, mode: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = BridgesClient::new(client)
                .link_challenge(bridge_id, mode)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.bridges.unlink` — idempotent disconnect.
    #[wasm_bindgen(js_name = bridgesUnlink)]
    pub fn bridges_unlink(&self, bridge_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            BridgesClient::new(client)
                .unlink(bridge_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.bridges.set_settings` — `settings` is a plain JS object →
    /// `Value::Map`.
    #[wasm_bindgen(js_name = bridgesSetSettings)]
    pub fn bridges_set_settings(&self, bridge_id: String, settings: JsValue) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let settings: Value = from_js(settings)?;
            BridgesClient::new(client)
                .set_settings(bridge_id, settings)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.bridges.list_follows` → `BridgeFollow[]`.
    #[wasm_bindgen(js_name = bridgesListFollows)]
    pub fn bridges_list_follows(&self, bridge_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = BridgesClient::new(client)
                .list_follows(bridge_id)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.follows)
        })
    }

    /// `fauna.bridges.add_follow` (`forbid_replay`). `petname` / `extra` are
    /// optional (`null`/`undefined` → absent).
    #[wasm_bindgen(js_name = bridgesAddFollow)]
    pub fn bridges_add_follow(
        &self,
        bridge_id: String,
        id: String,
        petname: Option<String>,
        extra: JsValue,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let extra = optional_cbor(extra)?;
            BridgesClient::new(client)
                .add_follow(bridge_id, id, petname, extra)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.bridges.remove_follow` — idempotent.
    #[wasm_bindgen(js_name = bridgesRemoveFollow)]
    pub fn bridges_remove_follow(&self, bridge_id: String, follow_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            BridgesClient::new(client)
                .remove_follow(bridge_id, follow_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.bridges.feeds.list` → `FeedSubscription[]`.
    #[wasm_bindgen(js_name = bridgesFeedsList)]
    pub fn bridges_feeds_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = BridgesClient::new(client)
                .feeds_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.subscriptions)
        })
    }

    /// `fauna.bridges.feeds.create` → the (new or existing) row id, as a JS
    /// number.
    #[wasm_bindgen(js_name = bridgesFeedsCreate)]
    pub fn bridges_feeds_create(
        &self,
        bridge: String,
        feed_uri: String,
        name: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let id = BridgesClient::new(client)
                .feeds_create(bridge, feed_uri, name)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_f64(id as f64))
        })
    }

    /// `fauna.bridges.feeds.delete` by row id (JS number).
    #[wasm_bindgen(js_name = bridgesFeedsDelete)]
    pub fn bridges_feeds_delete(&self, id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            BridgesClient::new(client)
                .feeds_delete(id as i64)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── fauna.admin.* ───────────────────────────────────────────────
    //
    // The admin surface the consolidated `admin-users` hub (Pending requests
    // / Invite / Users) drives, via the shared `AdminClient`. All kinds are
    // Admin-gated nest-side — a non-admin caller's call rejects with the
    // permission-denied `RpcError` string. Replies ride through `to_js` so the
    // wire shapes land as the plain JS objects the SPA's TS interfaces expect
    // (`actor_id` byte strings → `Uint8Array`, `i64` slots → JS numbers).

    /// `fauna.admin.users.list` → `AdminUsersListReply { users, total }`.
    /// `limit` `null` ⇒ the nest default (50, clamped `1..=500`); `offset` `>=0`
    /// (both `f64`-on-the-wire to dodge BigInt — see the feed section note).
    #[wasm_bindgen(js_name = adminUsersList)]
    pub fn admin_users_list(&self, limit: Option<f64>, offset: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .users_list(limit.map(|l| l as i64), offset as i64)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// Every account on the nest (`fauna_client_admin::users_list_all`) →
    /// `AdminUser[]`, newest first — what an admin actor picker offers
    /// (`admin.md` § 2 → *Which accounts a picker offers*), never one
    /// `adminUsersList` page.
    #[wasm_bindgen(js_name = adminUsersListAll)]
    pub fn admin_users_list_all(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let users = fauna_client_admin::users_list_all(&AdminClient::new(client))
                .await
                .map_err(err_to_js)?;
            to_js(&users)
        })
    }

    /// `fauna.admin.logs` → the nest's in-memory `fauna-log` ring snapshot,
    /// mapped to the same `{ timestamp_ms, level, target, message }` shape the
    /// client's own Settings → Logs page gets from `logSnapshot`, so the admin
    /// Logs view reuses one TS component. The nest ring has no Clear (no RPC
    /// wipes it). See `docs/goal/architecture/apps/observability.md`.
    #[wasm_bindgen(js_name = adminLogs)]
    pub fn admin_logs(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client).logs().await.map_err(err_to_js)?;
            let entries: Vec<fauna_log::LogEntry> = reply
                .entries
                .iter()
                .map(fauna_client_admin::log_entry_from_wire)
                .collect();
            to_js(&crate::logs::js_entries(&entries))
        })
    }

    /// `fauna.admin.users.update` — set a user's `tier` + `label` (the change-
    /// tier control). `actor_id` is the raw 32-byte id.
    #[wasm_bindgen(js_name = adminUsersUpdate)]
    pub fn admin_users_update(
        &self,
        actor_id: Vec<u8>,
        tier: String,
        label: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .users_update(actor_id, tier, label)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.users.evict` — schedule an eviction (warn → suspend → delete)
    /// with a `reason` + `category`; the row's `eviction` then renders the
    /// timeline (`admin-users-evict-button`). `actor_id` is the raw 32-byte id.
    #[wasm_bindgen(js_name = adminUsersEvict)]
    pub fn admin_users_evict(
        &self,
        actor_id: Vec<u8>,
        reason: String,
        category: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .users_evict(actor_id, reason, category)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.users.suspend` — cut a user off **now**, with no delete
    /// timeline (`admin-users-suspend-button`); reversible from the same
    /// `admin-users-cancel-eviction-button` an eviction uses. Accepted from a
    /// mid-eviction `warning` row too, where it *clears* the pending delete.
    /// Empty `reason`/`category` let the nest fill its canonical defaults
    /// (`"suspended by admin"` / `"other"`), so no client hard-codes them.
    /// `actor_id` is the raw 32-byte id.
    #[wasm_bindgen(js_name = adminUsersSuspend)]
    pub fn admin_users_suspend(
        &self,
        actor_id: Vec<u8>,
        reason: String,
        category: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .users_suspend(actor_id, reason, category)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.users.cancel_eviction` — cancel an in-flight eviction
    /// (`admin-users-cancel-eviction-button`). `actor_id` is the raw 32-byte id.
    #[wasm_bindgen(js_name = adminUsersCancelEviction)]
    pub fn admin_users_cancel_eviction(&self, actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .users_cancel_eviction(actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.users.create` — direct admission, the third account-
    /// creation path (`public-mode.md` § Registration & Identity; the Admit
    /// section, `admin-users-admit-*`). `handle` blank/`undefined` admits the
    /// deliberate handle-less state (§ A handle-less account); `label` is
    /// always empty, matching tui/linux — there is no free-text label on
    /// direct admission, only a tier. `actor_id` is the raw 32-byte id.
    #[wasm_bindgen(js_name = adminUsersCreate)]
    pub fn admin_users_create(
        &self,
        actor_id: Vec<u8>,
        tier: String,
        handle: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .users_create(actor_id, tier, String::new(), handle)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.admins.add` — grant the admin role
    /// (`admin-users-make-admin-button`). Schedules an `AdminAdd` pending action
    /// (24h delay) — the row does not flip to an admin row right away; a
    /// scheduled reply (no error) is success. `actor_id` is the raw 32-byte id.
    #[wasm_bindgen(js_name = adminAdminsAdd)]
    pub fn admin_admins_add(&self, actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .admins_add(actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.admins.remove` — revoke the admin role
    /// (`admin-users-remove-admin-button`). Refuses (`fauna.admin.conflict`) when
    /// it would leave zero superadmins. `actor_id` is the raw 32-byte id.
    #[wasm_bindgen(js_name = adminAdminsRemove)]
    pub fn admin_admins_remove(&self, actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .admins_remove(actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.admins.list` + a per-admin `fauna.admin.users.get` join,
    /// folded by the shared `fauna_client_admin::seed_rotation_confirm_view`
    /// (`box-recovery.md` § Deployment-seed rotation → Ordering rule) — the web
    /// twin of tui's `load_seed_rotate_roster` / linux's
    /// `FaunaClient::load_seed_rotate_roster`. Fired on the arm click
    /// (`admin-nest-seed-rotate-button`); read-only, arms nothing rotated.
    /// Resolves the `SeedRotationConfirmView` JSON directly (`{ inheritors,
    /// can_confirm, blocked_reason }`) — the SAME shared fold every app
    /// renders, never re-derived in TypeScript (priority #2).
    #[wasm_bindgen(js_name = adminSeedRotateRoster)]
    pub fn admin_seed_rotate_roster(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let view = AdminClient::new(client)
                .seed_rotate_roster_view()
                .await
                .map_err(err_to_js)?;
            to_js(&view)
        })
    }

    /// Drive the deployment-seed rotation ceremony and report it as one
    /// sentence (`box-recovery.md` § Deployment-seed rotation → § The
    /// ceremony) — the web twin of tui's `rotate_deployment_seed` / linux's
    /// `FaunaClient::rotate_deployment_seed`. Fired on the confirm click
    /// (`admin-nest-seed-rotate-confirm-button`) once the caller has disarmed
    /// the roster (disarm-before-dispatch — a double click must not chain a
    /// second rotation onto the first).
    ///
    /// The ordering discipline lives entirely in the shared plane drive
    /// (`fauna_client_config::rotate_deployment_seed_on_plane`: mint → custody
    /// merged → published to the bound nest → dispatch → mark), over this
    /// account's store handle — no handle refuses before dispatch
    /// (`box-recovery.md` § The plane-era recovery floor, (c)). There is no
    /// fan-out step: the plane carries the successor's row. Every outcome
    /// resolves to ONE `LocalizedText` — `seed_rotation_verdict` on a dispatch,
    /// the `admin.nest_page.rotate_seed_failed` key otherwise — rendered by the
    /// caller via `resolveLocalized`. Rejects only on a malformed secret.
    #[wasm_bindgen(js_name = rotateDeploymentSeed)]
    pub fn rotate_deployment_seed(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let keypair = keypair_from_secret_hex(&secret_hex)?;
            let failed = |cause: String| {
                fauna_core::localized::LocalizedText::key_arg(
                    "admin.nest_page.rotate_seed_failed",
                    "cause",
                    cause,
                )
            };
            // The identity this connection is BOUND to — the custody entry
            // this rotation supersedes (web twin of linux
            // `resolve_this_nest_id`; a box claiming a sibling's id must not
            // get that sibling's entry marked). Resolved before the ceremony,
            // while the box is still reachable on the predecessor: after the
            // flip it serves the successor.
            let id = match bound_nest_id(&client).await {
                Ok(id) => id,
                Err(e) => {
                    return to_js(&failed(e.as_string().unwrap_or_else(|| format!("{e:?}"))));
                }
            };
            // This account's store — never another account's that outlived a
            // switch by a tick — resolved per call, so a confirm clicked while
            // the runtime is still assembling waits out the assembly (bounded
            // by `LEDGER_READY_WAIT`) and only then refuses plainly, before
            // any dispatch (measured 2026-09-30: the web journey's confirm
            // beat the assembly and refused on the first look).
            let actor = keypair.actor_id_hex();
            let store = fauna_client_config::ResolvingLedgerStore::new(move || {
                crate::account_runtime::handle_for(&actor)
            });
            let verdict = match fauna_client_config::rotate_deployment_seed_on_plane(
                &client,
                id,
                Some(&store),
            )
            .await
            {
                Ok(outcome) => fauna_client_config::seed_rotation_verdict(&outcome),
                Err(e) => failed(e.to_string()),
            };
            to_js(&verdict)
        })
    }

    /// `fauna.admin.evictions.list` → `AdminUser[]` — every user with an in-flight
    /// eviction (each row's `eviction` is always set). Backs the Users section's
    /// eviction view.
    #[wasm_bindgen(js_name = adminEvictionsList)]
    pub fn admin_evictions_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .evictions_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.evictions)
        })
    }

    /// `fauna.admin.tiers.list` → `AdminTier[]` (the tier pickers' options).
    #[wasm_bindgen(js_name = adminTiersList)]
    pub fn admin_tiers_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .tiers_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.tiers)
        })
    }

    /// `fauna.admin.membership_tiers.list` → `AdminMembershipTier[]` — the
    /// membership designations this admin owns (monetization.md § Pillar 4).
    /// An empty array is the out-of-the-box state (nothing monetized).
    #[wasm_bindgen(js_name = adminMembershipTiersList)]
    pub fn admin_membership_tiers_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .membership_tiers_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.membership_tiers)
        })
    }

    /// `fauna.admin.membership_tiers.set` — designate one of the admin's own
    /// subscription tiers as a membership tier, or re-point an existing
    /// designation (an upsert). Pass an empty `lapseTier` for the documented
    /// default (`free`). A `tierName` the admin does not own is
    /// `fauna.admin.not_found`; an unknown quota tier is
    /// `fauna.admin.invalid_params`.
    #[wasm_bindgen(js_name = adminMembershipTiersSet)]
    pub fn admin_membership_tiers_set(
        &self,
        tier_name: String,
        admin_tier: String,
        lapse_tier: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .membership_tiers_set(fauna_client_admin::admin::AdminMembershipTierSetRequest {
                    tier_name,
                    admin_tier,
                    // Empty ⇒ omitted on the wire ⇒ the nest applies the default.
                    lapse_tier: (!lapse_tier.is_empty()).then_some(lapse_tier),
                    ..Default::default()
                })
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.membership_tiers.clear` — drop a designation, leaving the
    /// subscription tier itself untouched. A tier carrying no designation is
    /// `fauna.admin.not_found`.
    #[wasm_bindgen(js_name = adminMembershipTiersClear)]
    pub fn admin_membership_tiers_clear(&self, tier_name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .membership_tiers_clear(tier_name)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.tiers.create` — define a new tier (the `admin-settings`
    /// tier-definition UI). An empty `name` is `fauna.admin.invalid_params`; a
    /// duplicate is `fauna.admin.conflict`. Caps cross as `f64` (BigInt dodge).
    #[wasm_bindgen(js_name = adminTiersCreate)]
    pub fn admin_tiers_create(
        &self,
        name: String,
        max_inbox_bytes: f64,
        max_storage_bytes: f64,
        max_devices: f64,
        max_blob_size: f64,
        max_feeds: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .tiers_create(fauna_client_admin::admin::AdminTierCreateRequest {
                    name,
                    max_inbox_bytes: max_inbox_bytes as i64,
                    max_storage_bytes: max_storage_bytes as i64,
                    max_devices: max_devices as i64,
                    max_blob_size: max_blob_size as i64,
                    max_feeds: max_feeds as i64,
                    ..Default::default()
                })
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.tiers.update` — overwrite a tier's caps. A missing tier is
    /// `fauna.admin.not_found`. Caps cross as `f64` (BigInt dodge).
    #[wasm_bindgen(js_name = adminTiersUpdate)]
    pub fn admin_tiers_update(
        &self,
        name: String,
        max_inbox_bytes: f64,
        max_storage_bytes: f64,
        max_devices: f64,
        max_blob_size: f64,
        max_feeds: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .tiers_update(fauna_client_admin::admin::AdminTierUpdateRequest {
                    name,
                    max_inbox_bytes: max_inbox_bytes as i64,
                    max_storage_bytes: max_storage_bytes as i64,
                    max_devices: max_devices as i64,
                    max_blob_size: max_blob_size as i64,
                    max_feeds: max_feeds as i64,
                    ..Default::default()
                })
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.services.list` → the `AdminServiceFlags`
    /// (`{ bridge, pairing }`). Replay-safe pure read. After the
    /// per-page-services redesign (admin.md § Admin IA redesign, 2026-06-04) the
    /// only flag the UI still exposes is `pairing` (on `admin-nest`), but the
    /// read returns the whole flag struct (the page picks `.pairing`). Web reads
    /// it here over WS-RPC instead of the deleted `/admin/api/services` twin.
    #[wasm_bindgen(js_name = adminServicesList)]
    pub fn admin_services_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .services_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.services)
        })
    }

    /// `fauna.admin.services.update` — flip one service flag (`pairing` is the
    /// only one the UI still drives). The caller re-reads `adminServicesList` to
    /// reflect the applied state.
    #[wasm_bindgen(js_name = adminServicesUpdate)]
    pub fn admin_services_update(&self, name: String, enabled: bool) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .services_update(name, enabled)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.set_serving_port` — set the deployment-wide client-facing API
    /// serving port (the admin-set `admin-nest-serving-port-input`, nest/common.md
    /// § Serving ports). `port` is a u16 in `[1, 65535]` (the caller validates the
    /// range first). The caller re-reads `setupStatus().serving_port` to reflect
    /// the applied value; the new port binds on the next nest (re)start.
    #[wasm_bindgen(js_name = adminSetServingPort)]
    pub fn admin_set_serving_port(&self, port: u16) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .set_serving_port(port)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.set_registration_mode` — set the deployment's registration
    /// posture and, orthogonally, the free-tier ceiling: the single Save on the
    /// `admin-users` registration section (`admin.md` § 2 Users → *Section 2 —
    /// Registration*). One call carries both; the nest swaps the live posture with
    /// no restart. The caller re-reads `setupStatus().registration_mode` /
    /// `.max_free_users` to reflect the applied values.
    ///
    /// `mode` is the wire string the `admin-users-registration-mode-select` option
    /// carries — `"open"` / `"invite_required"` / `"closed"`. It is **validated
    /// here** (shared Rust owns the vocabulary): anything else rejects at this
    /// boundary rather than travelling to the nest, so the SPA can never invent a
    /// posture.
    ///
    /// `max_free_users` `undefined` **clears** the cap (the blank input = no cap)
    /// rather than leaving it unchanged — mode + ceiling are one decision, saved
    /// together. The cap counts every free-tier account *including the admin's
    /// own*, so "room for one more" is a cap of 2.
    #[wasm_bindgen(js_name = adminSetRegistrationMode)]
    pub fn admin_set_registration_mode(
        &self,
        mode: String,
        max_free_users: Option<u64>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let parsed = fauna_client_admin::RegistrationMode::from_wire_str(&mode)
                .ok_or_else(|| {
                    err_to_js(format!(
                        "unknown registration mode {mode:?}; expected open / invite_required / closed"
                    ))
                })?;
            AdminClient::new(client)
                .set_registration_mode(parsed, max_free_users)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.request_host_restart` — the admin's "restart now" affordance
    /// for the host Ubuntu box of an onboarded VPS (`installers/vps.md` § Host OS
    /// Maintenance § 4). Writes a flag the host reboot-coordinator picks up and
    /// reboots gracefully. The `nest-os-restart-now-button` calls this; it rejects
    /// on a nest with no maintenance mount (surfaced as a namespaced RpcError).
    #[wasm_bindgen(js_name = adminRequestHostRestart)]
    pub fn admin_request_host_restart(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .request_host_restart()
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.region.get` folded through the shared `admin_region_view`
    /// (region-blocking.md § Region determination) — the one call
    /// `admin-nest-region-section` needs to paint all five fields. Every
    /// rendering decision lives in the shared fold; this file decides
    /// nothing about the plane (priority #2 — six apps lift the same fold).
    #[wasm_bindgen(js_name = adminRegionStatus)]
    pub fn admin_region_status(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .region_status()
                .await
                .map_err(err_to_js)?;
            to_js(&fauna_client_admin::admin_region_view(&reply))
        })
    }

    /// `fauna.admin.region.set` — `admin-nest-region-save-button` (`region`
    /// already validated by `adminParseRegionCode`) and
    /// `admin-nest-region-withdraw-button` (`region` `undefined`) both call
    /// through here; re-validates regardless of whether the caller already
    /// did — the only path that can reach the wire. Re-declaring also
    /// retires the previous region's feature-policy document nest-side.
    #[wasm_bindgen(js_name = adminSetRegion)]
    pub fn admin_set_region(&self, region: Option<String>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let region = region
                .map(|r| fauna_client_admin::parse_region_code(&r))
                .transpose()
                .map_err(err_to_js)?;
            AdminClient::new(client)
                .set_region(region)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.web_app_origin.get` folded through the shared
    /// `admin_web_app_origin_view` (`admin.md` § N Nest → *Web-app origin*) —
    /// resolves `{ selected: "bundled" | "central" | null, status, scope,
    /// can_set, central_label }`. An older nest's unknown-kind answer resolves
    /// the "predates the choice" view, never a rejection.
    #[wasm_bindgen(js_name = adminWebAppOriginStatus)]
    pub fn admin_web_app_origin_status(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .web_app_origin()
                .await
                .map_err(err_to_js)?;
            to_js(&fauna_client_admin::admin_web_app_origin_view(
                reply.as_ref(),
            ))
        })
    }

    /// `fauna.admin.web_app_origin.set` — `admin-nest-web-app-origin-save-button`
    /// (`mode` `"bundled"` | `"central"`; anything else rejects before the
    /// wire). Resolves the fold of the nest's reply, so the save repaints the
    /// status with no second read.
    #[wasm_bindgen(js_name = adminSetWebAppOrigin)]
    pub fn admin_set_web_app_origin(&self, mode: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let mode = fauna_client_admin::WebAppOrigin::parse(&mode)
                .ok_or_else(|| JsValue::from_str("unknown web-app origin mode"))?;
            let reply = AdminClient::new(client)
                .set_web_app_origin(mode)
                .await
                .map_err(err_to_js)?;
            to_js(&fauna_client_admin::admin_web_app_origin_view(Some(&reply)))
        })
    }

    // ── Outside-app sign-in keys (`admin-nest-oauth-*`) — the web face of the
    //    shared issuer doors (`authorization-server.md` § The issuer → *Two
    //    rotation arms*); the pure folds are free fns in `lib.rs`
    //    (`issuerKeyRowLabel`, `issuerKeyRotateCost`, `issuerForcedConfirmView`).

    /// `fauna.oauth.issuer_key_status` folded through the shared
    /// `issuer_key_view` — resolves the `IssuerKeyView` JSON (`{ active_kid,
    /// keys: [{ kid, signing, retired_at, served_until }],
    /// retirement_horizon_secs, rotation_in_flight }`). Rejects on failure,
    /// which the page words through `oauth_keys_error` on
    /// `admin-nest-oauth-key-reason` — never the page's error line, since the
    /// status read can fail on its own and the rest must still paint.
    #[wasm_bindgen(js_name = adminIssuerKeyStatus)]
    pub fn admin_issuer_key_status(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let view = AdminClient::new(client)
                .issuer_key_status_view()
                .await
                .map_err(err_to_js)?;
            to_js(&view)
        })
    }

    /// `admin-nest-oauth-rotate-button` — the ordinary rotation, dispatched
    /// and worded: resolves the `LocalizedText` that IS
    /// `admin-nest-oauth-status`, success or failure alike (it never rejects —
    /// a reply lost to a timeout can follow a committed rotation, and only
    /// the shared fold may say so). Re-read the key set after.
    #[wasm_bindgen(js_name = adminRotateIssuerKey)]
    pub fn admin_rotate_issuer_key(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            to_js(&AdminClient::new(client).rotate_issuer_key_verdict().await)
        })
    }

    /// `admin-nest-oauth-confirm-button` — exactly the armed arm's kind
    /// (`arm` is `"IssuerKey"` or `"SessionSecret"`, the shared enum's own
    /// names), dispatched and worded like [`Self::admin_rotate_issuer_key`].
    /// `format_instant(secs) → string` is the SPA's clock face for the
    /// session-secret verdict's instant: `format_unix_local` needs the OS
    /// timezone database, which wasm lacks, so web is the one app that renders
    /// the instant in JS — the sentence around it is still the shared fold's.
    /// Disarm before calling.
    #[wasm_bindgen(js_name = adminForceRotateIssuer)]
    pub fn admin_force_rotate_issuer(
        &self,
        arm: JsValue,
        format_instant: js_sys::Function,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let arm: fauna_client_admin::IssuerForcedArm = from_js(arm)?;
            let verdict = AdminClient::new(client)
                .force_rotate_verdict(arm, |secs| {
                    format_instant
                        .call1(&JsValue::NULL, &JsValue::from_f64(secs as f64))
                        .ok()
                        .and_then(|v| v.as_string())
                        // A clock face that threw still owes the admin an
                        // instant: the raw seconds, never an empty gap.
                        .unwrap_or_else(|| secs.to_string())
                })
                .await;
            to_js(&verdict)
        })
    }

    /// `fauna.admin.stats` → the `AdminStatsReply` (`{ total_users,
    /// users_by_tier, suspended_users, total_inbox_bytes, total_storage_bytes,
    /// ws_connections }`). Pure read; the admin dashboard renders the user
    /// count + total storage from it (email/TLS status come from
    /// `setupStatus`). Web reads it here over WS-RPC instead of the deleted
    /// `/admin/api/stats` twin.
    #[wasm_bindgen(js_name = adminStats)]
    pub fn admin_stats(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client).stats().await.map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.admin.invite_codes.list` → `AdminInviteCode[]`.
    #[wasm_bindgen(js_name = adminInviteCodesList)]
    pub fn admin_invite_codes_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .invite_codes_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.invite_codes)
        })
    }

    /// `fauna.admin.invite_codes.create` → the resulting `code` string. **An
    /// empty `code` mints a random token** (the admit-a-user flow); a non-empty
    /// one is used verbatim. `guardian_actor` links the redeemed account to a
    /// guardian for supervised admission (`family-safety.md` § Wire & data
    /// shape); `None`/`undefined` mints an ordinary code. `age_band` is the
    /// `admin-users-invite-age-band-select` option value (a wire token from
    /// `ageBandOptions`, or `undefined`/`""` = no band); anything the
    /// vocabulary cannot name rejects **here**, never travelling to the nest
    /// (`family-safety.md` § The account age band, D2).
    #[wasm_bindgen(js_name = adminInviteCodesCreate)]
    pub fn admin_invite_codes_create(
        &self,
        code: String,
        tier: String,
        uses: f64,
        guardian_actor: Option<Vec<u8>>,
        age_band: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let age_band = parse_age_band(age_band)?;
            let reply = AdminClient::new(client)
                .invite_codes_create(code, tier, uses as i64, guardian_actor, age_band)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(&reply.code))
        })
    }

    /// `fauna.admin.invite_codes.delete` by `code`.
    #[wasm_bindgen(js_name = adminInviteCodesDelete)]
    pub fn admin_invite_codes_delete(&self, code: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .invite_codes_delete(code)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.invite_requests.list` → `AdminInviteRequest[]` (pending +
    /// decided).
    #[wasm_bindgen(js_name = adminInviteRequestsList)]
    pub fn admin_invite_requests_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .invite_requests_list()
                .await
                .map_err(err_to_js)?;
            // Project the shared `is_pending` predicate onto each row (see
            // `InviteRequestJs`) so web reads `r.is_pending`.
            let projected: Vec<InviteRequestJs> =
                reply.invite_requests.into_iter().map(Into::into).collect();
            to_js(&projected)
        })
    }

    /// `fauna.admin.invite_requests.approve` → `AdminInviteRequestApproveReply
    /// { actor_id, handle, tier }`. `tier` `null` ⇒ the nest default `"free"`;
    /// `label` optional. `guardian_actor` links the admitted account to a
    /// guardian for supervised admission (`family-safety.md` § Wire & data
    /// shape); `None`/`undefined` admits an ordinary account. `age_band` is the
    /// `invite-request-row-age-band-select` option value (a wire token or
    /// `undefined`/`""`), rejected here when the vocabulary cannot name it.
    #[wasm_bindgen(js_name = adminInviteRequestsApprove)]
    pub fn admin_invite_requests_approve(
        &self,
        id: f64,
        tier: Option<String>,
        label: Option<String>,
        guardian_actor: Option<Vec<u8>>,
        age_band: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let age_band = parse_age_band(age_band)?;
            let reply = AdminClient::new(client)
                .invite_requests_approve(id as i64, tier, label, guardian_actor, age_band)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.admin.set_age_verification_required` — the Registration
    /// section's "accept only signups carrying app age verification" knob
    /// (`admin-users-registration-age-verification-toggle`; default off; read
    /// back on `setupStatus().age_verification_required`). The section's save
    /// dispatches it beside `adminSetRegistrationMode` only when the toggle's
    /// value changed (`family-safety.md` § The account age band, D5+D6).
    #[wasm_bindgen(js_name = adminSetAgeVerificationRequired)]
    pub fn admin_set_age_verification_required(&self, required: bool) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .set_age_verification_required(required)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.invite_requests.deny` — mark request `id` denied with an
    /// optional `reason`.
    #[wasm_bindgen(js_name = adminInviteRequestsDeny)]
    pub fn admin_invite_requests_deny(&self, id: f64, reason: Option<String>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            AdminClient::new(client)
                .invite_requests_deny(id as i64, reason)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.admin.factory_reset` → `FactoryResetReply { claim_code }`. Returns
    /// the nest to fresh/unclaimed via a restart-wipe; the handler replies with
    /// the next claim code and then exits + restarts, so the human **never sees**
    /// the code — the client re-seeds onboarding at claim-code with it pre-filled.
    /// `new_claim_code` `null` ⇒ the nest regenerates a random code. Per
    /// `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset.
    #[wasm_bindgen(js_name = adminFactoryReset)]
    pub fn admin_factory_reset(&self, new_claim_code: Option<String>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .factory_reset(new_claim_code)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.admin.custody_hosting.list` → `AdminHostingRow[]` — every
    /// hosting row on this nest (`account-data-plane.md` § Two-sided bounds), folded by the shared
    /// `admin_hosting_rows` projection (heaviest hold first, tie-broken on
    /// `(host, owner, grant)`) so every lift app renders the same rows in the
    /// same order — do NOT re-sort or re-derive receipt freshness in TS.
    #[wasm_bindgen(js_name = adminHostingList)]
    pub fn admin_hosting_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminHostingClient::new(client)
                .list()
                .await
                .map_err(err_to_js)?;
            let now = Timestamp::now().0;
            let rows: Vec<JsAdminHostingRow> = admin_hosting_rows(&reply, now)
                .into_iter()
                .map(JsAdminHostingRow::from)
                .collect();
            to_js(&rows)
        })
    }

    /// `fauna.admin.custody_hosting.remove` — drop one row, keyed by the
    /// `(host, grant)` pair an `adminHostingList` row carries (never a painted
    /// index — a re-read can reorder rows). `removed: false` is an honest
    /// no-op (the row was already gone), not a failure.
    #[wasm_bindgen(js_name = adminHostingRemove)]
    pub fn admin_hosting_remove(
        &self,
        host_actor_id: String,
        grant_id: Vec<u8>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminHostingClient::new(client)
                .remove(&host_actor_id, &grant_id)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.family.* ──────────────────────────────────────────────
    //
    // The Family surface + supervised indicator (family-safety.md § Wire &
    // data shape) over the shared FamilyClient — the wasm twins of the
    // UniFFI FfiFamilyClient. Policies ride as the wire ReachPolicy shape
    // (serde-serialized to JS); actor ids as Uint8Array.

    /// `fauna.family.status` → `FamilyStatusReply` (supervised_by + policy +
    /// wards — both roles in one read).
    ///
    /// Every **successful** read also rewrites the caller's persisted
    /// last-known supervision snapshot (family-safety.md § Content policy,
    /// clause 2 — "written on every successful status read"). This method is
    /// web's one choke point for the read — the layout gate, the two social
    /// surfaces' hydrates and the Family page all come through here — so
    /// persisting here is what keeps the clause true for every present and
    /// future caller. The fold (and its graduation gate) is the shared
    /// `SupervisionSnapshot::from_status`; a failed read returns before the
    /// write, which is clause 1 by construction.
    ///
    /// The reply object also carries that same fold as `supervision` — an
    /// app-side field, never on the wire. The client-enforced inputs (the
    /// content floor, `content_notify`, the screen-time policy) move from it
    /// and never from the raw `policy`, so no TypeScript reader re-derives the
    /// graduation gate: a reply that still carries a policy document but names
    /// no guardian enforces nothing, exactly as it persists nothing.
    ///
    /// It carries `kidsAppEligible` too — app-side, never on the wire: the
    /// shared `fauna_client_family::kids_app_eligible` verdict over this same
    /// reply (`family-safety.md` § The account age band → the kids-app bullet,
    /// item (3)), the web twin of the UniFFI `kids_app_eligible` face.
    #[wasm_bindgen(js_name = familyStatus)]
    pub fn family_status(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        let actor_id_hex = self.actor_id_hex.clone();
        future_to_promise(async move {
            let reply = FamilyClient::new(client)
                .status()
                .await
                .map_err(err_to_js)?;
            let supervision = SupervisionSnapshot::from_status(&reply);
            let kids_app_eligible = fauna_client_family::kids_app_eligible(&reply);
            // A registry mutator, so it runs inside the cross-tab mutation
            // lock like every other one (`accounts.rs`'s module note).
            let snapshot_json = supervision.to_json();
            fauna_client_accounts::with_web_mutation_lock(move || {
                AccountRegistry::new(Arc::new(LocalStorageSecretStore))
                    .set_supervision_snapshot_json(&actor_id_hex, &snapshot_json);
            })
            .await;
            let obj = to_js(&reply)?;
            js_sys::Reflect::set(&obj, &"supervision".into(), &to_js(&supervision)?)?;
            js_sys::Reflect::set(
                &obj,
                &"kidsAppEligible".into(),
                &JsValue::from_bool(kids_app_eligible),
            )?;
            Ok(obj)
        })
    }

    /// `fauna.family.policy.update` — replace a ward's reach policy. `policy`
    /// is the JS shape of the wire `ReachPolicy` (contact_approval,
    /// unknown_sender_mail, federation_contact, feed_sources).
    #[wasm_bindgen(js_name = familyPolicyUpdate)]
    pub fn family_policy_update(
        &self,
        supervised_actor_id: Vec<u8>,
        policy: JsValue,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let policy: fauna_client_family::family::ReachPolicy =
                serde_wasm_bindgen::from_value(policy).map_err(err_to_js)?;
            FamilyClient::new(client)
                .policy_update(supervised_actor_id, policy)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.notify_report` — the supervised caller reports coarse
    /// per-category enforcement counts (`family-safety.md` § Guardian Notify).
    /// `entries` is the JS shape of `FamilyContentNotice[]` (`{category, count}`);
    /// `utc_offset_minutes` is the device's UTC offset (nest-clamped, the
    /// § Screen time day-bucket rule — pass `-new Date().getTimezoneOffset()`);
    /// a no-op nest-side unless the caller is supervised with `content_notify` on.
    #[wasm_bindgen(js_name = familyNotifyReport)]
    pub fn family_notify_report(
        &self,
        entries: JsValue,
        utc_offset_minutes: i32,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let entries: Vec<fauna_client_family::family::FamilyContentNotice> =
                serde_wasm_bindgen::from_value(entries).map_err(err_to_js)?;
            FamilyClient::new(client)
                .notify_report(entries, utc_offset_minutes)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.usage_report` — the supervised caller heartbeats coarse
    /// foreground minutes for the daily screen-time budget (`family-safety.md`
    /// § Screen time). `minutes` is the foreground delta since the last
    /// successful report (`0` = a pure read); `utc_offset_minutes` as on
    /// `familyNotifyReport`. Resolves to the JS shape of
    /// `FamilyUsageReportReply` (`{day, day_total_minutes}`) — the day's
    /// cross-device total the lock surface locks on. A silent zero-reply
    /// no-op nest-side unless the caller is supervised with a daily budget set.
    #[wasm_bindgen(js_name = familyUsageReport)]
    pub fn family_usage_report(&self, minutes: u32, utc_offset_minutes: i32) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FamilyClient::new(client)
                .usage_report(minutes, utc_offset_minutes)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.features.* ────────────────────────────────────────────
    //
    // The gated-feature plane's transparency read (dynamic-features.md
    // § Transparency & auditability) over the shared FeaturesClient — the
    // wasm twin of the UniFFI FfiFeaturesClient. Both hand their UI the same
    // `FeatureRow` shape from `fauna_client_features::row`, so web and the
    // native apps render the same words for the same state (priority #1).

    /// `fauna.features.status` → `FeatureRow[]` — every registry member's
    /// effective policy and remaining quota for the calling account, already
    /// folded into rows (the join, `remaining = limit − observed`, the binding
    /// tier per cell, and the Dim-3 `available | disabled | hidden` decision).
    ///
    /// `nestCapabilities` is the nest's advertised capability set
    /// (`NestInfoReply.capabilities`), which decides the `hidden` affordance: a
    /// member whose token the nest does not advertise is not carried by that
    /// build, and rendering its row would advertise a plane the artifact does
    /// not have. Pass `[]` only when the set is genuinely unknown — every
    /// member that *has* a token then reads `hidden`.
    ///
    /// Rows are complete, including members the caller is not limited on:
    /// "unrestricted" is an answer, and an absence could not be told apart from
    /// a nest that never heard of the feature.
    #[wasm_bindgen(js_name = featuresStatus)]
    pub fn features_status(&self, nest_capabilities: Vec<String>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = fauna_client_features::FeaturesClient::new(client)
                .status()
                .await
                .map_err(err_to_js)?;
            to_js(&fauna_client_features::feature_rows(
                &reply.features,
                &nest_capabilities,
            ))
        })
    }

    /// **The call an app makes** — the whole feature-limits surface, ready to
    /// render: [`fauna_client_features::FeaturesClient::rows`] joined with
    /// `fauna.nest.info`'s capability set and folded via `feature_rows`,
    /// exactly mirroring [`Self::features_status`] plus the capability read
    /// tui/linux already do through the shared crate directly. wasm needed its
    /// own mirror rather than exposing `nestCapabilities` as a separate call:
    /// the SPA had no wrapper carrying `NestInfoReply.capabilities` either
    /// (`$lib/api.ts`'s `fetchNestInfo` returns the narrower pre-identity
    /// `{domain, version, registration}` shape) — the same "no export, not
    /// just unconsumed" gap the earlier trickle-down found in
    /// `resolve_post`/`locate_card_by_uid_hash`.
    #[wasm_bindgen(js_name = featuresRows)]
    pub fn features_rows(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = fauna_client_features::FeaturesClient::new(client)
                .rows()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.family.approvals.list` → `FamilyApprovalEntry[]`.
    #[wasm_bindgen(js_name = familyApprovalsList)]
    pub fn family_approvals_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FamilyClient::new(client)
                .approvals_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.approvals)
        })
    }

    /// `fauna.family.approvals.decide` — approve/deny one pending item.
    ///
    /// Each kind names its item with a different key — pass that kind's key and
    /// leave the others empty; a `familyApprovalsList` entry carries all of
    /// them. `contact`/`contact_request` → `peer_actor_id`; `mail_hold` →
    /// `message_id`; `feed_source` → the whole `(bridge_id, operation, target)`
    /// triple (`target` empty for a `link`; the label is never part of the key);
    /// `dm_hold` → `(bridge_id, peer_address)`.
    #[wasm_bindgen(js_name = familyApprovalsDecide)]
    #[allow(clippy::too_many_arguments)]
    pub fn family_approvals_decide(
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
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
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
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The guardian's un-deny of one denied bridge-DM peer (`family-safety.md`
    /// § The bridge-DM gate → *The un-deny surface*) — over the shared
    /// `FamilyClient::allow_blocked_dm_peer`, which owns the approving
    /// `dm_hold` decide's wire shape. Pass the denied row's own
    /// `(bridge_id, peer_id)`.
    #[wasm_bindgen(js_name = familyAllowBlockedDmPeer)]
    pub fn family_allow_blocked_dm_peer(
        &self,
        supervised_actor_id: Vec<u8>,
        bridge_id: String,
        peer_id: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .allow_blocked_dm_peer(supervised_actor_id, bridge_id, peer_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.contact.add` — pre-approve a contact on the ward's behalf.
    #[wasm_bindgen(js_name = familyContactAdd)]
    pub fn family_contact_add(
        &self,
        supervised_actor_id: Vec<u8>,
        peer_actor_id: Vec<u8>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .contact_add(supervised_actor_id, peer_actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.contact.request` — the **supervised** caller's in-app
    /// ask to contact a peer (family-safety.md § Child-initiated contact
    /// requests); pending in the guardian's queue until decided.
    #[wasm_bindgen(js_name = familyContactRequest)]
    pub fn family_contact_request(&self, peer_actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .contact_request(peer_actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.feed_source.request` — the **supervised** caller's in-app
    /// ask to add an external source their `feed_sources = "block"` policy just
    /// refused (family-safety.md § Feed-source approvals); pending in the
    /// guardian's queue until decided. Approving mints a single-use grant the
    /// caller redeems by retrying the original call.
    ///
    /// `operation` is `"link" | "follow" | "feed"`; `target` is the follow id /
    /// feed URI and is empty for `link`; `label` is display-only.
    #[wasm_bindgen(js_name = familyFeedSourceRequest)]
    pub fn family_feed_source_request(
        &self,
        bridge_id: String,
        operation: String,
        target: String,
        label: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .feed_source_request(bridge_id, operation, target, label)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.graduate` — supervised → full account, in place.
    #[wasm_bindgen(js_name = familyGraduate)]
    pub fn family_graduate(&self, supervised_actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .graduate(supervised_actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.device.mark` — set/clear the guardian-enrolled-device
    /// marker on one of the ward's devices (family-safety.md § Full
    /// visibility). Guardian-only nest-side; a marked device is un-removable by
    /// the ward and auto-revoked at graduation.
    #[wasm_bindgen(js_name = familyDeviceMark)]
    pub fn family_device_mark(
        &self,
        supervised_actor_id: Vec<u8>,
        device_id: String,
        marked: bool,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .device_mark(supervised_actor_id, device_id, marked)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.transfer` — propose a new guardian for a ward; pending
    /// until the proposed guardian accepts (family-safety.md § Graduation &
    /// transfer; a self-proposal completes immediately).
    #[wasm_bindgen(js_name = familyTransfer)]
    pub fn family_transfer(
        &self,
        supervised_actor_id: Vec<u8>,
        new_guardian_actor_id: Vec<u8>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .transfer(supervised_actor_id, new_guardian_actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.transfer.accept` — consent to a proposal naming the
    /// caller as new guardian; completes the re-point.
    #[wasm_bindgen(js_name = familyTransferAccept)]
    pub fn family_transfer_accept(&self, supervised_actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .transfer_accept(supervised_actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.transfer.decline` — refuse a proposal naming the caller.
    #[wasm_bindgen(js_name = familyTransferDecline)]
    pub fn family_transfer_decline(&self, supervised_actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .transfer_decline(supervised_actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.family.transfer.cancel` — withdraw the ward's pending proposal.
    #[wasm_bindgen(js_name = familyTransferCancel)]
    pub fn family_transfer_cancel(&self, supervised_actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FamilyClient::new(client)
                .transfer_cancel(supervised_actor_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── fauna.email.* ───────────────────────────────────────────────

    /// `fauna.email.filters.list` → `EmailFilter[]`.
    #[wasm_bindgen(js_name = emailFiltersList)]
    pub fn email_filters_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let filters = EmailClient::new(client)
                .filters_list()
                .await
                .map_err(err_to_js)?;
            to_js(&filters)
        })
    }

    /// `fauna.email.filters.create` → the new row id (JS number). `rules` is a
    /// JS array of externally-tagged `EmailFilterRule` objects, `action` an
    /// `EmailFilterAction` (a string for the unit variants, an object
    /// otherwise).
    #[wasm_bindgen(js_name = emailFiltersCreate)]
    pub fn email_filters_create(
        &self,
        name: String,
        rules: JsValue,
        combination: String,
        action: JsValue,
        priority: i32,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let rules: Vec<EmailFilterRule> = from_js(rules)?;
            let action: EmailFilterAction = from_js(action)?;
            let id = EmailClient::new(client)
                .filters_create(name, rules, combination, action, priority)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_f64(id as f64))
        })
    }

    /// `fauna.email.filters.get` by row id (JS number) → `EmailFilter`.
    #[wasm_bindgen(js_name = emailFiltersGet)]
    pub fn email_filters_get(&self, id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let filter = EmailClient::new(client)
                .filters_get(id as i64)
                .await
                .map_err(err_to_js)?;
            to_js(&filter)
        })
    }

    /// `fauna.email.filters.update` — overwrite an existing rule. `rules` is a
    /// JS array of externally-tagged `EmailFilterRule` objects, `action` an
    /// `EmailFilterAction` (a string for the unit variants, an object
    /// otherwise) — the same shapes `emailFiltersCreate` takes.
    #[wasm_bindgen(js_name = emailFiltersUpdate)]
    pub fn email_filters_update(
        &self,
        id: f64,
        name: String,
        rules: JsValue,
        combination: String,
        action: JsValue,
        priority: i32,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let rules: Vec<EmailFilterRule> = from_js(rules)?;
            let action: EmailFilterAction = from_js(action)?;
            EmailClient::new(client)
                .filters_update(id as i64, name, rules, combination, action, priority)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.email.filters.delete` by row id (JS number).
    #[wasm_bindgen(js_name = emailFiltersDelete)]
    pub fn email_filters_delete(&self, id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            EmailClient::new(client)
                .filters_delete(id as i64)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── The post-succession filter-mark review (`succession-aftermath.md`
    // § Adjudicating what the aftermath carries across, the fourth plane) — the wasm twin of fauna-ffi's
    // `filter_marks.rs` (zero logic owed, priority #2). `filterMarkRemoved`
    // never deletes: this plane has no second removal mechanism, so the
    // caller's own `emailFiltersDelete` owns the deletion, called FIRST —
    // `filterMarkRemoved` only records the verdict afterward (this app's
    // `+page.svelte` trap note: recording before deleting would leave an
    // armed rule under a list that now reads clean). ──

    /// Read the ids of every filter rule still awaiting the owner's verdict
    /// (`load_filter_marks`) → row ids as JS numbers. The SPA caches what
    /// this returns and answers per-row questions against it: the filter
    /// list paints far more often than the ledger changes. The marks rest on
    /// the succession ledger, read through this tab's account-store handle.
    #[wasm_bindgen(js_name = filterMarksList)]
    pub fn filter_marks_list(&self) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::ledger_store()?;
            let ids = load_filter_marks(&store).await.map_err(err_to_js)?;
            to_js(&ids.into_iter().map(|id| id as f64).collect::<Vec<_>>())
        })
    }

    /// Record **Keep** — the owner recognises this rule; it stays, and the
    /// mark clears. Resolves to whether anything was actually open (a
    /// concurrent device may have already answered — a success no-op, never
    /// a rejection).
    #[wasm_bindgen(js_name = filterMarkKeep)]
    pub fn filter_mark_keep(&self, filter_id: f64) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::ledger_store()?;
            let changed = decide_filter_mark(&store, filter_id as i64, UnattestedVerdict::Kept)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_bool(changed))
        })
    }

    /// Record **Removed** — call ONLY after `emailFiltersDelete` has already
    /// deleted the rule. Resolves to whether anything was actually open.
    #[wasm_bindgen(js_name = filterMarkRemoved)]
    pub fn filter_mark_removed(&self, filter_id: f64) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::ledger_store()?;
            let changed = decide_filter_mark(&store, filter_id as i64, UnattestedVerdict::Removed)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_bool(changed))
        })
    }

    /// `fauna.email.inbox.fetch` → `InboxFetchReply { messages, more }`, the
    /// inbound twin of `emailSend`. Each `messages[i]` carries `{ uid,
    /// message_id (bytes), internal_date (epoch secs), sealed_envelope (bytes) }`
    /// — the still-sealed `MailRecordEnvelope` the conversations receive poll
    /// opens client-side with `WasmConversationsManager.ingestSealedInbound`
    /// (`docs/goal/behavior/smtp-server.md` § Inbound client receive). `after_uid`
    /// is the paging cursor (return only `uid > after_uid`); `limit` caps records
    /// per page (`0` = source default).
    #[wasm_bindgen(js_name = emailInboxFetch)]
    pub fn email_inbox_fetch(&self, after_uid: u32, limit: u32) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = EmailClient::new(client)
                .inbox_fetch(after_uid, limit)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.email.sent.fetch` → `InboxFetchReply` — the **Sent** sibling of
    /// `emailInboxFetch` (same wire shape, paging and client-side decrypt; the
    /// mailbox is chosen server-side). Surfaces mail the user sent from an
    /// external SMTP-submission MUA (its server-side `Sent` copy is sealed to the
    /// sender's own MSEK-derived key, opened with the same recipient secret as
    /// INBOX) so the conversations view shows both halves of a thread
    /// (`docs/goal/behavior/smtp-server.md` § Inbound client receive). The web
    /// conversations receive poll drains its own Sent cursor in parallel with
    /// INBOX (`$lib/conversations.ts`, the twin of linux `mail_sink.rs`).
    #[wasm_bindgen(js_name = emailSentFetch)]
    pub fn email_sent_fetch(&self, after_uid: u32, limit: u32) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = EmailClient::new(client)
                .sent_fetch(after_uid, limit)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.email.send` (`forbid_replay`, 30 s) → `SendEmailReply {
    /// local_delivered, remote_queued, remote_errors }`. `raw_rfc5322` is the
    /// raw message bytes (no base64 — WS-RPC is binary-clean).
    #[wasm_bindgen(js_name = emailSend)]
    pub fn email_send(&self, recipients: Vec<String>, raw_rfc5322: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = EmailClient::new(client)
                .send(recipients, raw_rfc5322)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── On-device mail spam scorer (mail-spam.md § Scoring placement) ───
    //
    // The three RPCs the web receive loop drives around the INBOX drain to
    // score mail on-device (the Fauna-app scoring position): fetch the
    // sealed per-user model + the effective policy, then apply the scorer's
    // watermark+move outcome. The scoring itself runs in
    // `WasmConversationsManager` (the decrypt/model never crosses into JS).

    /// `fauna.bridges.fetch_spam_model` → `{ blob, baseline }` where `blob` is
    /// the caller's per-user spam model **sealed to them** (a bare inner
    /// `wrapped_blob`) and `baseline` is the published deployment baseline
    /// (plaintext aggregate) that rides the reply **only for a client-sealed
    /// stored model** (else `null` — the nest already folded it server-side;
    /// the no-double-fold rule, `mail-spam.md` § Encrypted-mode interaction).
    /// The whole result is `null` when untrained (cold start). `actor_id` is
    /// the caller's OWN 32-byte id (the RPC is caller-scoped; the shared
    /// request schema carries it explicitly). The SPA hands both to
    /// `WasmConversationsManager.enableSpamScoring`, which unwraps the model
    /// under the held recipient secret and applies the local fold.
    #[wasm_bindgen(js_name = fetchSpamModel)]
    pub fn fetch_spam_model(&self, actor_id: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = MailAccountClient::new(client)
                .fetch_spam_model(actor_id)
                .await
                .map_err(err_to_js)?;
            let Some(blob) = reply.blob else {
                return Ok(JsValue::NULL);
            };
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(
                &obj,
                &"blob".into(),
                &js_sys::Uint8Array::from(blob.as_slice()).into(),
            )?;
            let baseline: JsValue = match reply.baseline {
                Some(b) => js_sys::Uint8Array::from(b.as_slice()).into(),
                None => JsValue::NULL,
            };
            js_sys::Reflect::set(&obj, &"baseline".into(), &baseline)?;
            Ok(obj.into())
        })
    }

    /// `fauna.bridges.get_spam_scoring_policy` → the admin-effective spam-scoring
    /// policy `{ spamFolderThreshold, bayesianWeightMilli, bayesianMinSamples,
    /// bayesianFullConfidenceSamples }` the on-device scorer needs so its
    /// INBOX→Junk line matches the MDA/nest (`mail-spam.md` § Architectural
    /// rules). Server-wide, non-secret; readable by any `User`.
    #[wasm_bindgen(js_name = getSpamScoringPolicy)]
    pub fn get_spam_scoring_policy(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = MailAccountClient::new(client)
                .get_spam_scoring_policy()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.bridges.get_spam_threshold_override` → the caller's per-account
    /// spam-folder threshold override, in whole points, or `null` when the
    /// account follows the admin default. The `mail-spam` page's threshold input.
    #[wasm_bindgen(js_name = spamThresholdOverrideGet)]
    pub fn spam_threshold_override_get(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let value = MailAccountClient::new(client)
                .get_spam_threshold_override()
                .await
                .map_err(err_to_js)?;
            to_js(&value)
        })
    }

    /// `fauna.bridges.set_spam_threshold_override` — set (or clear, with
    /// `null`) the override; the nest confirms the write, then this re-reads
    /// so the caller reflects the **persisted** value, never the local edit.
    /// `0` is a real setting — it turns automatic Junk filing off for this
    /// account, distinct from `null` (follow the admin default).
    #[wasm_bindgen(js_name = spamThresholdOverrideSet)]
    pub fn spam_threshold_override_set(&self, value: Option<u32>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let confirmed = MailAccountClient::new(client)
                .set_spam_threshold_override_and_reload(value)
                .await
                .map_err(err_to_js)?;
            to_js(&confirmed)
        })
    }

    /// `fauna.email.apply_spam_disposition` → `{ watermarked, movedToJunk }`.
    /// Applies the on-device scorer's outcome to the caller's own INBOX:
    /// watermark every `scored_uids` message `$FaunaSpamScored`, then move the
    /// `junk_uids` subset INBOX→Junk (`junk_uids ⊆ scored_uids`). Driven from
    /// `WasmConversationsManager.takeSpamDisposition` after the INBOX drain.
    #[wasm_bindgen(js_name = applySpamDisposition)]
    pub fn apply_spam_disposition(
        &self,
        scored_uids: Vec<u32>,
        junk_uids: Vec<u32>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = EmailClient::new(client)
                .apply_spam_disposition(scored_uids, junk_uids)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.nostr.bunker.* ────────────────────────────────────────
    //
    // The NIP-46 bunker control plane (the *Connected apps* roster,
    // `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer) over the
    // shared `fauna_client_nostr::NostrBunkerClient<R>` (priority #2). Consumed
    // by the web Nostr page's *Connected apps* section (`$lib/nostr.ts`). All
    // four kinds are User-class, caller-scoped server-side. `connection_id` is
    // taken as `f64` (JS numbers) and narrowed to the wire `i64` — an `i64`
    // param would demand a JS `BigInt` at the call site (the `email_filters_delete`
    // discipline).

    /// `fauna.nostr.bunker.create_invite` → `CreateBunkerInviteReply` — mint a
    /// pending connection; the reply carries the one-time `bunker://…` connect
    /// string, signer pubkey, row id, and invite TTL (the single reveal).
    #[wasm_bindgen(js_name = nostrBunkerCreateInvite)]
    pub fn nostr_bunker_create_invite(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = NostrBunkerClient::new(client)
                .create_invite()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.nostr.bunker.list` → `BunkerAppEntry[]` — the caller's connection
    /// roster (pending + active; revoked tombstones are not served).
    #[wasm_bindgen(js_name = nostrBunkerList)]
    pub fn nostr_bunker_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let apps = NostrBunkerClient::new(client)
                .list()
                .await
                .map_err(err_to_js)?;
            to_js(&apps)
        })
    }

    /// `fauna.nostr.bunker.revoke` → `bool` — immediate disconnect of one
    /// connection; `false` when no live caller-owned row matched.
    #[wasm_bindgen(js_name = nostrBunkerRevoke)]
    pub fn nostr_bunker_revoke(&self, connection_id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let revoked = NostrBunkerClient::new(client)
                .revoke(connection_id as i64)
                .await
                .map_err(err_to_js)?;
            to_js(&revoked)
        })
    }

    /// `fauna.nostr.bunker.set_label` → `bool` — name a connection row; `false`
    /// when no live caller-owned row matched.
    #[wasm_bindgen(js_name = nostrBunkerSetLabel)]
    pub fn nostr_bunker_set_label(&self, connection_id: f64, label: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let updated = NostrBunkerClient::new(client)
                .set_label(connection_id as i64, label)
                .await
                .map_err(err_to_js)?;
            to_js(&updated)
        })
    }

    // ── nostr.* (protocol-native content) ───────────────────────────
    //
    // The prefix-less protocol-native content kinds
    // (`nostr.{zaps.total,badges.list,events.publish_signed}` — the
    // `bluesky.feed.thread` precedent), WS-RPC successors to the deleted
    // `/api/v1/nostr/{zaps,badges,publish-signed}` HTTP routes (the
    // native-content HTTP→WS-RPC rip, `docs/goal/ui/nostr.md` § WS-RPC
    // migration contract), over the shared
    // `fauna_client_nostr::NostrContentClient<R>` (priority #2). Consumed by
    // `$lib/nostr.ts`.

    /// `nostr.badges.list` → `NostrBadgeItem[]` — badge awards (NIP-58) for
    /// one pubkey, newest first.
    #[wasm_bindgen(js_name = nostrBadges)]
    pub fn nostr_badges(&self, pubkey: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let badges = NostrContentClient::new(client)
                .badges(pubkey)
                .await
                .map_err(err_to_js)?;
            to_js(&badges)
        })
    }

    /// `nostr.events.publish_signed` — relay-enqueue an event the browser's
    /// NIP-07 extension signed. `event_json` is the signed event's NIP-01 wire
    /// JSON exactly as the extension returned it; the nest verifies the
    /// signature and that the pubkey is the caller's linked account. Resolves
    /// to `undefined` on success.
    #[wasm_bindgen(js_name = nostrPublishSigned)]
    pub fn nostr_publish_signed(&self, event_json: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            NostrContentClient::new(client)
                .publish_signed(event_json)
                .await
                .map_err(err_to_js)?;
            Ok(wasm_bindgen::JsValue::UNDEFINED)
        })
    }

    // ── fauna.bridges.* (encrypted CalDAV store) ────────────────────────────
    //
    // The web Events page's WS-RPC seam over the *encrypted* `bridge_caldav_*`
    // store via the shared `fauna_client_caldav::CalDavClient` — the events.md
    // Decision-B store flip, lifting the proven linux lead (`apps/fauna-linux`
    // `caldav_backend.rs` + `client.rs`), retiring the legacy plaintext
    // `fauna.events.*` / `/api/calendars` path. Sealing/unsealing + the WASM-safe
    // flat decode are shared Rust (priority #2). Each method derives the actor id
    // from `secret_hex` and reads the mail custody's MSEK *inside wasm* (`dav_ctx`; the
    // msek never crosses into JS); reads degrade to an empty list when mail is off.
    // Timestamps + `self_email` ride in from JS (the wasm-time discipline — no
    // `Date::now()` in wasm). See events.md.

    /// `fauna.bridges.list_calendars` → `{ calendars: [{ id: hex, name, color,
    /// visibility, event_count }] }`. Lazy-provisions the deterministic Personal
    /// calendar on an empty list (mirrors the linux `fetch_calendars`), so a fresh
    /// actor lands a usable calendar. Empty list when mail/CalDAV is off.
    #[wasm_bindgen(js_name = caldavListCalendars)]
    pub fn caldav_list_calendars(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let Some((actor_id, msek)) = dav_ctx(&secret_hex).await? else {
                return to_js(&serde_json::json!({ "calendars": [] }));
            };
            let dav = caldav::CalDavClient::new(client);
            let mut reply = dav
                .list_calendars(caldav::bridge_routing::ListCalendarsRequest {
                    actor_id: actor_id.to_vec(),
                })
                .await
                .map_err(err_to_js)?;
            if reply.calendars.is_empty() {
                // Best-effort lazy Personal: a provision failure still returns the
                // (empty) list rather than erroring the page load.
                let meta = caldav::CalendarMetadata {
                    displayname: "Personal".into(),
                    color: "#3273dc".into(),
                    description: String::new(),
                    ..Default::default()
                };
                if let Ok(sealed) = caldav::seal_calendar_metadata(&meta, &msek) {
                    let _ = dav
                        .provision_calendar(caldav::bridge_routing::ProvisionCalendarRequest {
                            actor_id: actor_id.to_vec(),
                            calendar_id: caldav::personal_calendar_id().to_vec(),
                            encrypted_metadata: sealed,
                            update_metadata: false,
                        })
                        .await;
                    reply = dav
                        .list_calendars(caldav::bridge_routing::ListCalendarsRequest {
                            actor_id: actor_id.to_vec(),
                        })
                        .await
                        .map_err(err_to_js)?;
                }
            }
            // Derived once for the whole list — every row's metadata reuses it
            // instead of paying its own X-Wing keygen .
            let keys = caldav::DavRecipientKeys::derive(&msek);
            let calendars: Vec<serde_json::Value> = reply
                .calendars
                .iter()
                .map(|c| {
                    let meta = caldav::unseal_calendar_metadata(&c.encrypted_metadata, &keys)
                        .unwrap_or_default();
                    serde_json::json!({
                        "id": hex::encode(&c.calendar_id),
                        "name": meta.displayname,
                        "color": meta.color,
                        "visibility": "private",
                        "event_count": c.event_count,
                    })
                })
                .collect();
            to_js(&serde_json::json!({ "calendars": calendars }))
        })
    }

    /// `fauna.bridges.provision_calendar` (MKCOL) → `{ id: hex }`. `calendar_id_hex`
    /// is a client-assigned 32-byte id (the web generates it via
    /// `crypto.getRandomValues`, mirroring the linux client-side id). Seals the
    /// `{ name, color }` metadata to the actor's msek before sending.
    #[wasm_bindgen(js_name = caldavProvisionCalendar)]
    pub fn caldav_provision_calendar(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        name: String,
        color: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let meta = caldav::CalendarMetadata {
                displayname: name,
                color,
                description: String::new(),
                ..Default::default()
            };
            let sealed = caldav::seal_calendar_metadata(&meta, &msek).map_err(err_to_js)?;
            caldav::CalDavClient::new(client)
                .provision_calendar(caldav::bridge_routing::ProvisionCalendarRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: calendar_id.to_vec(),
                    encrypted_metadata: sealed,
                    update_metadata: false,
                })
                .await
                .map_err(err_to_js)?;
            to_js(&serde_json::json!({ "id": hex::encode(calendar_id) }))
        })
    }

    /// `fauna.bridges.query_events` over a calendar → `{ events: [{ ...row }] }`
    /// (see `flat_event_to_web_json`). Each sealed body is unsealed + flat-parsed
    /// via the WASM-safe `decode_event_entry_flat`. Empty list when mail is off or
    /// the calendar has no row yet.
    #[wasm_bindgen(js_name = caldavQueryEvents)]
    pub fn caldav_query_events(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        self_email: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let Some((actor_id, msek)) = dav_ctx(&secret_hex).await? else {
                return to_js(&serde_json::json!({ "events": [] }));
            };
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let reply = caldav::CalDavClient::new(client)
                .query_events(caldav::bridge_routing::QueryEventsRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: calendar_id.to_vec(),
                    since_modseq: None,
                    after_event_id: None,
                    limit: 0,
                })
                .await
                .map_err(err_to_js)?;
            let entries = match reply {
                caldav::bridge_routing::QueryEventsReply::Ok { events, .. } => events,
                caldav::bridge_routing::QueryEventsReply::CalendarNotFound => Vec::new(),
                caldav::bridge_routing::QueryEventsReply::Unknown => {
                    return Err(JsValue::from_str(caldav::UNKNOWN_QUERY_OUTCOME));
                }
            };
            // Derived once for the whole page — every row reuses it instead of
            // paying its own X-Wing keygen .
            let keys = caldav::DavRecipientKeys::derive(&msek);
            let events: Vec<serde_json::Value> = entries
                .iter()
                .filter_map(|e| caldav::decode_event_entry_flat(e, &keys).ok())
                .map(|f| flat_event_to_web_json(&f, &self_email))
                .collect();
            to_js(&serde_json::json!({ "events": events }))
        })
    }

    // ── CardDAV Address Book (read-only, slice 4b) ─────────────────────────────
    //
    // The web Contacts page's "Address Book" segment over the encrypted
    // `bridge_carddav_*` store via the shared `fauna_client_carddav::CardDavClient`
    // — the read analogue of the caldav Events seam above. Unseal + parse are shared
    // Rust (priority #2); the whole read path is WASM-safe (the vCard reader is
    // hand-rolled RFC 6350, unlike caldav's native-only iCalendar parser). Each
    // method reads the mail custody's MSEK *inside wasm* (`dav_ctx`; the msek never crosses
    // into JS) and degrades to an empty list when mail/CardDAV is off. There is NO
    // lazy provisioning (unlike caldav's Personal calendar) — the read seam never
    // writes; a book is minted by the write slice (4c) or a CardDAV MUA.
    // See carddav-server.md § Independent enablement + contacts.md § Layout & flow.

    /// `fauna.bridges.list_addressbooks` → `{ addressbooks: [{ id: hex, name,
    /// description, card_count }] }`. Empty list when mail/CardDAV is off or the
    /// actor has no address book yet.
    #[wasm_bindgen(js_name = carddavListAddressbooks)]
    pub fn carddav_list_addressbooks(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let Some((actor_id, msek)) = dav_ctx(&secret_hex).await? else {
                return to_js(&serde_json::json!({ "addressbooks": [] }));
            };
            let keys = carddav::DavRecipientKeys::derive(&msek);
            let books = carddav::CardDavClient::new(client)
                .list_addressbooks_decoded(
                    carddav::bridge_routing::ListAddressbooksRequest {
                        actor_id: actor_id.to_vec(),
                    },
                    &keys,
                )
                .await
                .map_err(err_to_js)?;
            let addressbooks: Vec<serde_json::Value> =
                books.iter().map(decoded_addressbook_to_web_json).collect();
            to_js(&serde_json::json!({ "addressbooks": addressbooks }))
        })
    }

    /// `fauna.bridges.query_cards` over an address book → `{ cards: [{ ...row }] }`
    /// (see `decoded_card_to_web_json`). Each sealed body is unsealed + parsed via
    /// the WASM-safe `decode_card_entry`. Empty list when mail is off or the book
    /// has no row yet (`AddressbookNotFound`).
    #[wasm_bindgen(js_name = carddavQueryCardsDecoded)]
    pub fn carddav_query_cards_decoded(
        &self,
        secret_hex: String,
        addressbook_id_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let Some((actor_id, msek)) = dav_ctx(&secret_hex).await? else {
                return to_js(&serde_json::json!({ "cards": [] }));
            };
            let addressbook_id = hex_array_32(&addressbook_id_hex)?;
            let keys = carddav::DavRecipientKeys::derive(&msek);
            let page = carddav::CardDavClient::new(client)
                .query_cards_decoded(
                    carddav::bridge_routing::QueryCardsRequest {
                        actor_id: actor_id.to_vec(),
                        addressbook_id: addressbook_id.to_vec(),
                        since_modseq: None,
                        after_card_id: None,
                        limit: 0,
                    },
                    &keys,
                )
                .await
                .map_err(err_to_js)?;
            let cards: Vec<serde_json::Value> = match page {
                carddav::DecodedCardsPage::Ok { cards, .. } => {
                    cards.iter().map(decoded_card_to_web_json).collect()
                }
                carddav::DecodedCardsPage::AddressbookNotFound => Vec::new(),
            };
            to_js(&serde_json::json!({ "cards": cards }))
        })
    }

    /// Locate a card by its `uid_hash` (hex-encoded `blake3(uid)`) across every
    /// address book the actor holds — the contact-navigation deep-link door a
    /// search hit needs, since the holding book need not be the one currently
    /// open, or loaded at all (`ui/search.md` § Where logic lives → *Result
    /// navigation (deep link)*). ⚠ `uid_hash` and `card_id` are different id
    /// spaces of the same width — never cast one for the other; this is the
    /// only sanctioned lookup. Returns `{ addressbooks: [...], addressbook_id:
    /// hex | null, card_id: hex | null, cards: [...] }` — every address book
    /// (so the destination page can render its picker + card list off one
    /// round of reads) plus the found card's location, `null` when no book
    /// holds it (deleted since it was indexed).
    #[wasm_bindgen(js_name = carddavLocateCardByUidHash)]
    pub fn carddav_locate_card_by_uid_hash(
        &self,
        secret_hex: String,
        uid_hash_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let Some((actor_id, msek)) = dav_ctx(&secret_hex).await? else {
                return to_js(&serde_json::json!({
                    "addressbooks": [], "addressbook_id": null, "card_id": null, "cards": [],
                }));
            };
            let uid_hash = hex_array_32(&uid_hash_hex)?;
            let located = carddav::CardDavClient::new(client)
                .locate_card_by_uid_hash(actor_id.to_vec(), &msek, &uid_hash)
                .await
                .map_err(err_to_js)?;
            let addressbooks: Vec<serde_json::Value> = located
                .books
                .iter()
                .map(decoded_addressbook_to_web_json)
                .collect();
            let (addressbook_id, card_id, cards) = match located.card {
                Some(found) => (
                    serde_json::Value::String(hex::encode(&found.addressbook_id)),
                    serde_json::Value::String(hex::encode(&found.card_id)),
                    found.cards.iter().map(decoded_card_to_web_json).collect(),
                ),
                None => (serde_json::Value::Null, serde_json::Value::Null, Vec::new()),
            };
            to_js(&serde_json::json!({
                "addressbooks": addressbooks,
                "addressbook_id": addressbook_id,
                "card_id": card_id,
                "cards": cards,
            }))
        })
    }

    /// `fauna.bridges.put_event_ciphertext` (create) → `{ id: hex(uid_hash), uid }`.
    /// Builds the canonical VEVENT from the web event form, seals it, and PUTs it
    /// to `calendar_id`. The caller is the organizer (`self_email`); `now_secs`
    /// stamps DTSTAMP (the wasm-time discipline — passed in from JS).
    #[wasm_bindgen(js_name = caldavCreateEvent)]
    #[allow(clippy::too_many_arguments)]
    pub fn caldav_create_event(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        uid: String,
        summary: String,
        dtstart: String,
        dtend: String,
        description: Option<String>,
        location: Option<String>,
        self_email: String,
        now_secs: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let fields = caldav::EventFields {
                summary,
                dtstart,
                dtend,
                location: location.unwrap_or_default(),
                description: description.unwrap_or_default(),
                uid: uid.clone(),
                status: "confirmed".into(),
                ..Default::default()
            };
            let uid_hash = caldav::uid_hash(&uid);
            caldav::CalDavClient::new(client)
                .seal_and_put_event(
                    &actor_id,
                    &calendar_id,
                    &uid_hash,
                    &msek,
                    &fields,
                    &[],
                    &self_email,
                    None,
                    now_secs as i64,
                    None,
                )
                .await
                .map_err(err_to_js)?;
            to_js(&serde_json::json!({ "id": hex::encode(uid_hash), "uid": uid }))
        })
    }

    /// `fauna.bridges.delete_event` — tombstone an event by `uid_hash`. Resolves
    /// `undefined`.
    #[wasm_bindgen(js_name = caldavDeleteEvent)]
    pub fn caldav_delete_event(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        uid_hash_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, _msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let uid_hash = hex_bytes(&uid_hash_hex)?;
            caldav::CalDavClient::new(client)
                .delete_event(caldav::bridge_routing::DeleteEventRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: calendar_id.to_vec(),
                    uid_hash,
                    if_match: None,
                })
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// RSVP to a stored event (read-mutate-rewrite): locate by `uid_hash`, apply
    /// `response` for `self_email` via the shared `apply_rsvp`
    /// (sidecar-authoritative `interested`), and re-PUT.
    ///
    /// `response` is one of `going | interested | declined`
    /// (`fauna_core::rsvp::RsvpResponse`) — **not** `tentative`, which is an
    /// inbound-only state a stock CalDAV client sets and no Fauna app offers
    /// (caldav-server.md § RSVP semantics). An unrecognized value is refused by the
    /// shared `apply_rsvp`, not folded to `NEEDS-ACTION`.
    #[wasm_bindgen(js_name = caldavRsvpEvent)]
    #[allow(clippy::too_many_arguments)]
    pub fn caldav_rsvp_event(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        uid_hash_hex: String,
        response: String,
        self_email: String,
        now_secs: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let uid_hash = hex_bytes(&uid_hash_hex)?;
            let (ics, ext) =
                caldav_find_event(client.clone(), &actor_id, &calendar_id, &uid_hash, &msek)
                    .await?;
            let rw = caldav::apply_rsvp(&ics, ext.as_ref(), &self_email, &response)
                .map_err(|e| JsValue::from_str(&e))?;
            caldav_put_rewrite(
                client.clone(),
                &actor_id,
                &calendar_id,
                &msek,
                &rw,
                now_secs as i64,
            )
            .await?;
            // Notify a *different* organizer with an iMIP REPLY — the shared
            // `imip_reply_for_rsvp` (the "Responding" half of caldav-server.md
            // § Server-side auto-schedule), matching every native app. This
            // was previously dropped on web: a web user's accept/decline never
            // reached an external (Apple/Gmail) organizer. Best-effort — the
            // local RSVP already persisted.
            if let Some(reply) = caldav::imip_reply_for_rsvp(&rw, &self_email, now_secs as i64) {
                let _ = EmailClient::new(client)
                    .send(reply.recipients, reply.raw_rfc5322)
                    .await;
            }
            to_js(&serde_json::json!({ "status": response }))
        })
    }

    /// Read a stored event's reminder offset (the single `VALARM`, e.g. `-PT15M`)
    /// → the offset string, or `null` when none is set.
    #[wasm_bindgen(js_name = caldavReminderGet)]
    pub fn caldav_reminder_get(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        uid_hash_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let uid_hash = hex_bytes(&uid_hash_hex)?;
            let (ics, _ext) =
                caldav_find_event(client, &actor_id, &calendar_id, &uid_hash, &msek).await?;
            let fields = caldav::parse_ical(&ics).map_err(err_to_js)?;
            if fields.alarm.is_empty() {
                Ok(JsValue::NULL)
            } else {
                to_js(&fields.alarm)
            }
        })
    }

    /// Set a stored event's reminder offset (read-mutate-rewrite via the shared
    /// `set_reminder`). Resolves the offset string.
    #[wasm_bindgen(js_name = caldavReminderSet)]
    #[allow(clippy::too_many_arguments)]
    pub fn caldav_reminder_set(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        uid_hash_hex: String,
        offset: String,
        now_secs: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let uid_hash = hex_bytes(&uid_hash_hex)?;
            let (ics, ext) =
                caldav_find_event(client.clone(), &actor_id, &calendar_id, &uid_hash, &msek)
                    .await?;
            let rw = caldav::set_reminder(&ics, ext.as_ref(), &offset)
                .map_err(|e| JsValue::from_str(&e))?;
            caldav_put_rewrite(client, &actor_id, &calendar_id, &msek, &rw, now_secs as i64)
                .await?;
            to_js(&offset)
        })
    }

    /// Clear a stored event's reminder (read-mutate-rewrite, empty offset).
    /// Resolves `undefined`.
    #[wasm_bindgen(js_name = caldavReminderRemove)]
    pub fn caldav_reminder_remove(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        uid_hash_hex: String,
        now_secs: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let uid_hash = hex_bytes(&uid_hash_hex)?;
            let (ics, ext) =
                caldav_find_event(client.clone(), &actor_id, &calendar_id, &uid_hash, &msek)
                    .await?;
            let rw =
                caldav::set_reminder(&ics, ext.as_ref(), "").map_err(|e| JsValue::from_str(&e))?;
            caldav_put_rewrite(client, &actor_id, &calendar_id, &msek, &rw, now_secs as i64)
                .await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Invite an attendee by email (read-mutate-rewrite via the shared
    /// `add_attendee`): append a `mailto:` ATTENDEE to the roster, re-PUT, then fan
    /// the iMIP `REQUEST` out to the event's email-reachable roster — the shared
    /// `imip_request_for_invite` (the "Organizer fan-out" half of caldav-server.md
    /// § Server-side auto-schedule), matching linux. Best-effort send — the roster
    /// change already persisted. Resolves `undefined`.
    #[wasm_bindgen(js_name = caldavInviteAttendee)]
    #[allow(clippy::too_many_arguments)]
    pub fn caldav_invite_attendee(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        uid_hash_hex: String,
        attendee_email: String,
        self_email: String,
        now_secs: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let uid_hash = hex_bytes(&uid_hash_hex)?;
            let (ics, ext) =
                caldav_find_event(client.clone(), &actor_id, &calendar_id, &uid_hash, &msek)
                    .await?;
            let rw = caldav::add_attendee(&ics, ext.as_ref(), &self_email, &attendee_email)
                .map_err(|e| JsValue::from_str(&e))?;
            caldav_put_rewrite(
                client.clone(),
                &actor_id,
                &calendar_id,
                &msek,
                &rw,
                now_secs as i64,
            )
            .await?;
            // Fan the iMIP REQUEST to the email-reachable roster (organizer
            // excluded; `None` when nobody is reachable). This was previously
            // dropped on web: a web user's invite never reached the attendee.
            if let Some(message) = caldav::imip_request_for_invite(
                &rw.fields,
                &rw.attendees,
                &rw.organizer_email,
                now_secs as i64,
            ) {
                let _ = EmailClient::new(client)
                    .send(message.recipients, message.raw_rfc5322)
                    .await;
            }
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Export a calendar as a single RFC 5545 `.ics` string via the shared
    /// `CalDavClient::export_calendar_ics` — the one export every app calls.
    /// Errors when mail/CalDAV is off.
    #[wasm_bindgen(js_name = caldavExportCalendar)]
    pub fn caldav_export_calendar(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            // Derived once for the whole export — every entry's body reuses it
            // instead of paying its own X-Wing keygen .
            let ics = caldav::CalDavClient::new(client)
                .export_calendar_ics(
                    &actor_id,
                    &calendar_id,
                    &caldav::DavRecipientKeys::derive(&msek),
                )
                .await
                .map_err(|e| match e {
                    caldav::ReadEventsError::Transport(e) => err_to_js(e),
                    other => JsValue::from_str(&other.to_string()),
                })?;
            to_js(&ics)
        })
    }

    /// Import a `.ics` file into a calendar via the shared
    /// `CalDavClient::import_ical_events` (parse + seal + PUT each VEVENT) — the
    /// linux `import_calendar_ics` twin (v1 imports event fields only; an
    /// imported event's attendee roster is a follow-up). A UID-less entry gets a
    /// synthesized stable UID. Resolves `{ imported, skipped, total }`. Errors
    /// when mail/CalDAV is off.
    #[wasm_bindgen(js_name = caldavImportCalendar)]
    pub fn caldav_import_calendar(
        &self,
        secret_hex: String,
        calendar_id_hex: String,
        ics_text: String,
        self_email: String,
        now_secs: f64,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_id, msek) = dav_ctx(&secret_hex)
                .await?
                .ok_or_else(|| JsValue::from_str("calendar requires mail to be enabled"))?;
            let calendar_id = hex_array_32(&calendar_id_hex)?;
            let dav = caldav::CalDavClient::new(client);
            let outcome = dav
                .import_ical_events(
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &self_email,
                    &ics_text,
                    now_secs as i64,
                    |idx| format!("import-{}-{}@fauna-web", now_secs as i64, idx),
                )
                .await;
            to_js(&serde_json::json!({
                "imported": outcome.imported,
                "skipped": outcome.skipped,
                "total": outcome.total,
            }))
        })
    }

    /// Build the unified `WasmConversationsManager` for the logged-in actor over
    /// this client — registering **both** rails: the `Rail::Smtp` backend (send +
    /// receive over `fauna.email.send` / `inbox.fetch`) and the `Rail::FaunaMls`
    /// backend (E2E MLS DMs over the `fauna.conversations.*` seam, driving an
    /// in-memory `MlsEngine`). `self_address` is the logged-in `<handle>@<domain>`
    /// (the outbound From + FaunaMls self-handle); `self_secret` is the 32-byte
    /// Ed25519 actor secret the MLS engine derives its credential from. The web
    /// conversations page + receive poll share this one instance
    /// (`docs/goal/ui/conversations.md` § Goal — one page, every rail).
    /// Start this tab's **account runtime** for the signed-in account over
    /// this session, and register the account-plane conversations seams on
    /// `manager` at the store-ready edge (`crate::account_runtime`;
    /// `account-client-lifecycle.md` § The client-side lifecycle → *The
    /// trigger fired*, ruling (4)). Call it once `manager` is built; `nest_url`
    /// is the session's nest URL as the SPA pins it (the origin-pin trust
    /// read). `peer_token_provider_factory` is the Nests page's own
    /// `(peerUrl) => (forceRefresh) => Promise<string>` — what the runtime's
    /// secondary leg connects to each linked replica nest with
    /// (`account-sync-plane.md` § The bind leg, ruling 4). Rejects — leaving
    /// the tab with no account store — when the store cannot assemble.
    #[wasm_bindgen(js_name = startAccountRuntime, unchecked_return_type = "Promise<void>")]
    pub fn start_account_runtime(
        &self,
        manager: &crate::conversations::WasmConversationsManager,
        self_secret: Vec<u8>,
        nest_url: String,
        peer_token_provider_factory: js_sys::Function,
    ) -> Result<js_sys::Promise, JsValue> {
        let secret: [u8; 32] = self_secret
            .try_into()
            .map_err(|_| JsValue::from_str("self secret must be 32 bytes"))?;
        let parts = manager.account_seam_parts().ok_or_else(|| {
            JsValue::from_str("account runtime: no conversations engine built yet")
        })?;
        let client = self.inner.clone();
        let peer_connect = crate::pairing::make_peer_connect(
            client.actor_id_hex().to_string(),
            peer_token_provider_factory,
        );
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            crate::account_runtime::start(client, parts, secret, &nest_url, peer_connect).await?;
            Ok(JsValue::UNDEFINED)
        }))
    }

    #[wasm_bindgen(js_name = conversationsManager)]
    pub fn conversations_manager(
        &self,
        self_address: String,
        self_secret: Vec<u8>,
    ) -> Result<crate::conversations::WasmConversationsManager, JsValue> {
        crate::conversations::WasmConversationsManager::with_conversations(
            self.inner.clone(),
            self_address,
            self_secret,
        )
    }

    /// Build the shared `WasmFeedManager` for the logged-in actor over this
    /// client — the web twin of the native `FfiNestClient::feed_manager` (and the
    /// Rust-native Linux app's `FeedManager<Arc<NestClient>>`). The Svelte
    /// Feed page renders entirely from its `snapshot()` and forwards gestures to
    /// the async manager methods (`docs/goal/ui/feed.md` § State & data shape).
    /// `secret` is the 32-byte Ed25519 actor secret (needed to build + sign posts
    /// on `submitPost`).
    #[wasm_bindgen(js_name = feedManager)]
    pub fn feed_manager(&self, secret: Vec<u8>) -> Result<crate::feed::WasmFeedManager, JsValue> {
        crate::feed::WasmFeedManager::with_client(self.inner.clone(), secret)
    }

    /// Build the owner's **events-rail** draft persistence over this client —
    /// the third and last constant of `fauna_protocol::drafts::DRAFT_RAILS`
    /// (`reserved-folders.md` § Drafts Sync). Unlike `feedManager` this is not a
    /// page manager: the Events page has no shared manager on any app, so the
    /// face carries the five `event-form` inputs across the boundary while the
    /// encoding, the seal, the WS calls and the launch gate stay in Rust.
    ///
    /// `secret` is the 32-byte Ed25519 actor secret — the at-rest `BackupKey` is
    /// derived from it internally; drafts are owner-only and sign nothing.
    #[wasm_bindgen(js_name = eventDrafts)]
    pub fn event_drafts(
        &self,
        secret: Vec<u8>,
    ) -> Result<crate::event_drafts::WasmEventDrafts, JsValue> {
        crate::event_drafts::WasmEventDrafts::with_client(self.inner.clone(), secret)
    }

    /// Build the shared `WasmSearchManager` for the logged-in actor over this
    /// client — the web twin of the native `FfiNestClient::search_manager`
    /// (and the Rust-native Linux/tui apps' `SearchManager<Arc<NestClient>>`).
    /// The Svelte Search page renders entirely from its `snapshot()` and
    /// forwards gestures to the async manager methods (`docs/goal/ui/search.md`
    /// § State & data shape). Unlike `feedManager` this needs no actor
    /// secret — searching signs nothing.
    #[wasm_bindgen(js_name = searchManager)]
    pub fn search_manager(&self) -> crate::search::WasmSearchManager {
        crate::search::WasmSearchManager::with_client(self.inner.clone())
    }

    /// Build the shared Task-delegation surface for the logged-in actor over this
    /// client — the web twin of the native
    /// `FfiNestClient::task_delegation_view_for_device`. The Settings → Task
    /// delegation sub-page renders `load()`'s rows verbatim
    /// (`docs/goal/behavior/participants.md` § Task delegation).
    ///
    /// `device_id_hex` is this browser's device id; the pins rest sealed on
    /// `fauna.state.delegation` in this tab's account store. The capability is
    /// not a parameter: a browser tab is always `ViewerOnly` (see
    /// `task_delegation::WEB_CAPABILITY`).
    #[wasm_bindgen(js_name = taskDelegationViewForDevice)]
    pub fn task_delegation_view_for_device(
        &self,
        device_id_hex: String,
    ) -> crate::task_delegation::WasmTaskDelegationView {
        crate::task_delegation::WasmTaskDelegationView::with_client(
            self.inner.clone(),
            device_id_hex,
        )
    }

    // ── fauna.posts.* ───────────────────────────────────────────────

    /// `fauna.posts.create` — store a signed post. `body` is the raw
    /// signed-post bytes the SPA built (embed-as-bytes wire); the reply is
    /// `{ post_id }` (hex of `blake3(body)`).
    #[wasm_bindgen(js_name = postsCreate)]
    pub fn posts_create(&self, body: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PostsClient::new(client)
                .posts_create(body)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.posts.get` — resolve the raw post bytes for `post_id` (hex).
    /// Resolves a `Uint8Array` (the `application/octet-stream` body the HTTP
    /// twin returned); a missing/quarantine-gated post rejects with the
    /// `fauna.posts.not_found` error string.
    #[wasm_bindgen(js_name = postsGet)]
    pub fn posts_get(&self, post_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PostsClient::new(client)
                .posts_get(post_id)
                .await
                .map_err(err_to_js)?;
            Ok(js_sys::Uint8Array::from(reply.body.as_slice()).into())
        })
    }

    /// `fauna.posts.delete` — destroy the caller's own post. `tombstone` is
    /// the raw signed-tombstone bytes the SPA built (embed-as-bytes wire;
    /// build + sign with `sign_and_pack`, exactly as `postsCreate` takes
    /// already-signed post bytes — signing lives with the caller). Resolves
    /// `PostDeleteReply { post_id, deleted }`; `deleted: false` is the
    /// idempotent already-gone success, never a rejection.
    #[wasm_bindgen(js_name = postsDelete)]
    pub fn posts_delete(&self, tombstone: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PostsClient::new(client)
                .posts_delete(tombstone)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.posts.interact` (`forbid_replay`) — like / unlike / reply /
    /// repost / unrepost / quote on `post_id`. `body` (optional) is the
    /// reply/quote text. Resolves `PostInteractReply { action, source,
    /// result }`.
    #[wasm_bindgen(js_name = postsInteract)]
    pub fn posts_interact(
        &self,
        post_id: String,
        action: String,
        body: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PostsClient::new(client)
                .posts_interact(post_id, action, body)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.feed.* ────────────────────────────────────────────────
    //
    // Cursor / limit / score-cursor cross the JS boundary as `f64` (cast to
    // `i64` here) — wasm-bindgen would otherwise surface `i64` as a JS BigInt;
    // the values (epoch micros, micro-unit scores) sit well within 2^53. The
    // `json_compatible` `to_js` serializer surfaces the reply's `i64` slots as
    // plain JS numbers, matching the SPA's `number`-typed interfaces.

    /// `fauna.feed.list` → `FeedSummary[]` (summary view, no rules).
    #[wasm_bindgen(js_name = feedList)]
    pub fn feed_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FeedClient::new(client)
                .feed_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.feeds)
        })
    }

    /// `fauna.feed.create` → `{ feed_id }`. `rules` is a JS array of
    /// externally-tagged `FilterRule` objects (the typed wire list);
    /// `scope`/`contributor_seeds` are left nest-default (local feed),
    /// matching the linux create path.
    #[wasm_bindgen(js_name = feedCreate)]
    pub fn feed_create(
        &self,
        name: String,
        rules: JsValue,
        combination: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let rules: Vec<FilterRule> = from_js(rules)?;
            let reply = FeedClient::new(client)
                .feed_create(name, rules, combination, None, None, None)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.feed.get` → `FeedGetReply` including its typed `rules` array.
    #[wasm_bindgen(js_name = feedGet)]
    pub fn feed_get(&self, feed_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FeedClient::new(client)
                .feed_get(feed_id)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.feed.update` — overwrite `name` / `rules` / `combination`
    /// (`rules` as in [`Self::feed_create`]).
    #[wasm_bindgen(js_name = feedUpdate)]
    pub fn feed_update(
        &self,
        feed_id: String,
        name: String,
        rules: JsValue,
        combination: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let rules: Vec<FilterRule> = from_js(rules)?;
            FeedClient::new(client)
                .feed_update(feed_id, name, rules, combination, None)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.feed.delete` — delete an owned feed.
    #[wasm_bindgen(js_name = feedDelete)]
    pub fn feed_delete(&self, feed_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            FeedClient::new(client)
                .feed_delete(feed_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.feed.posts` → `FeedPostsReply`. `cursor` / `limit` /
    /// `score_cursor` are `f64`-on-the-wire (see the section note); `order` is
    /// `"score"` for ranked paging, else chronological.
    #[wasm_bindgen(js_name = feedPosts)]
    #[allow(clippy::too_many_arguments)]
    pub fn feed_posts(
        &self,
        feed_id: String,
        cursor: Option<f64>,
        limit: Option<f64>,
        order: Option<String>,
        score_cursor: Option<f64>,
        score_cursor_created_at: Option<f64>,
        search: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FeedClient::new(client)
                .feed_posts(
                    feed_id,
                    cursor.map(|c| c as i64),
                    limit.map(|l| l as i64),
                    order,
                    score_cursor.map(|s| s as i64),
                    // The keyset cursor's tiebreak half — echo it back beside
                    // the key (both or neither).
                    score_cursor_created_at.map(|t| t as i64),
                    search,
                )
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.feed.local.posts` → `FeedLocalPostsReply` (always chronological).
    #[wasm_bindgen(js_name = feedLocalPosts)]
    pub fn feed_local_posts(
        &self,
        cursor: Option<f64>,
        limit: Option<f64>,
        search: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FeedClient::new(client)
                .feed_local_posts(cursor.map(|c| c as i64), limit.map(|l| l as i64), search)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.knocks.* / fauna.contacts.* / fauna.inbox.mode.* ──────
    //
    // Connection-management surface (knocks inbox + contact roster + inbox-
    // acceptance policy). The connection actor is the calling actor, so no
    // `{actor_id}` param rides — it replaced the deleted HTTP routes'
    // `{actor_id}` path segment. The accept/block/dismiss/confirm/set kinds
    // reply with an empty ack (`Ok(UNDEFINED)`); failure is a rejected promise
    // carrying the `RpcError` string.

    /// `fauna.knocks.list` → `KnockItem[]` (`{id, sender, sender_node, summary,
    /// created_at}`, matching the SPA's `Knock` type).
    #[wasm_bindgen(js_name = knocksList)]
    pub fn knocks_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ContactsClient::new(client)
                .knocks_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.knocks)
        })
    }

    /// `fauna.knocks.accept` — accept the knock from `peer_id` (hex).
    #[wasm_bindgen(js_name = knocksAccept)]
    pub fn knocks_accept(&self, peer_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ContactsClient::new(client)
                .knocks_accept(peer_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.knocks.block` — block the knock sender `peer_id` (hex).
    #[wasm_bindgen(js_name = knocksBlock)]
    pub fn knocks_block(&self, peer_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ContactsClient::new(client)
                .knocks_block(peer_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.knocks.unblock` — clear a `blocked` edge for `peer_id` (hex), the
    /// guarded clear-the-edge (`ContactStatus` → `None`). The unblock half of the
    /// profile-block-button toggle (`contacts.md` § Where logic lives → Unblock).
    #[wasm_bindgen(js_name = knocksUnblock)]
    pub fn knocks_unblock(&self, peer_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ContactsClient::new(client)
                .knocks_unblock(peer_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.knocks.dismiss` — dismiss the knock from `peer_id` (hex).
    #[wasm_bindgen(js_name = knocksDismiss)]
    pub fn knocks_dismiss(&self, peer_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ContactsClient::new(client)
                .knocks_dismiss(peer_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.contacts.list` → `ContactItem[]` (`{peer_id, status, accepted_at?,
    /// created_at}`, matching the SPA's `Contact` type).
    #[wasm_bindgen(js_name = contactsList)]
    pub fn contacts_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ContactsClient::new(client)
                .contacts_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.contacts)
        })
    }

    /// `fauna.contacts.confirm` — promote the `accepted` contact `peer_id`
    /// (hex) to `confirmed`.
    #[wasm_bindgen(js_name = contactsConfirm)]
    pub fn contacts_confirm(&self, peer_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ContactsClient::new(client)
                .contacts_confirm(peer_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.inbox.mode.get` → the inbox-acceptance mode string (`open` /
    /// `allow_knock` / `contacts_only` / `closed`).
    #[wasm_bindgen(js_name = inboxModeGet)]
    pub fn inbox_mode_get(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ContactsClient::new(client)
                .inbox_mode_get()
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(&reply.mode))
        })
    }

    /// `fauna.inbox.mode.set` — set the inbox-acceptance `mode` (an
    /// unrecognized value rejects with the invalid-params `RpcError`).
    #[wasm_bindgen(js_name = inboxModeSet)]
    pub fn inbox_mode_set(&self, mode: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ContactsClient::new(client)
                .inbox_mode_set(mode)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.inbox.send` — hand the home nest a canonical signed payload for
    /// `recipient_actor_id` (hex). `recipient_nest_url` is `null`/`undefined`
    /// for a same-nest recipient (the nest local-delivers) and a peer URL for a
    /// cross-nest send (the nest originates `fauna.federation.inbox.deliver`).
    /// The browser twin of the native `InboxClient::send` seam — the carrier for
    /// an outbound knock built with `buildKnockPayload`
    /// (`api-layers.md` § Contacts & Knocks). The reply's `inbox_id` is the
    /// created row id on delivery, or `null` when the peer's `allow_knock` mode
    /// stored it as a pending knock.
    #[wasm_bindgen(js_name = inboxSend)]
    pub fn inbox_send(
        &self,
        recipient_actor_id: String,
        recipient_nest_url: Option<String>,
        payload_bytes: Vec<u8>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = fauna_client_inbox::InboxClient::new(client)
                .send(recipient_actor_id, recipient_nest_url, payload_bytes)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.notifications.* ───────────────────────────────────────
    //
    // `cursor` / `limit` / `up_to` cross the JS boundary as `f64` (cast to
    // `i64`) — wasm-bindgen would otherwise surface `i64` as a JS BigInt; the
    // values (notification ids, epoch micros) sit well within 2^53. Replies
    // ride through `to_js` so the `i64` slots surface as plain JS numbers. The
    // `notif_type` wire key is renamed to `type` SPA-side (see
    // `apps/fauna-web/src/lib/notifications.ts`). `fauna.notifications.count`
    // has no web consumer (the page derives the unread count from the list), so
    // it isn't surfaced here.

    /// `fauna.notifications.list` → `NotifListReply` (`{notifications:[…],
    /// cursor?}`). `cursor` is the last-seen id (`null` ⇒ newest page);
    /// `limit` is the page size (`null` ⇒ the nest's default 25).
    #[wasm_bindgen(js_name = notificationsList)]
    pub fn notifications_list(&self, cursor: Option<f64>, limit: Option<f64>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = NotificationsClient::new(client)
                .notifications_list(cursor.map(|c| c as i64), limit.map(|l| l as i64))
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.notifications.mark_read` → `{ marked_read }`. `up_to` (micros,
    /// `null` ⇒ now) marks everything created at or before it as read.
    #[wasm_bindgen(js_name = notificationsMarkRead)]
    pub fn notifications_mark_read(&self, up_to: Option<f64>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = NotificationsClient::new(client)
                .notifications_mark_read(up_to.map(|u| u as i64))
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.account.* / fauna.quota.get / fauna.profile.handle.change ──
    //
    // The personal account-management surface the Settings page hits (quota
    // usage, handle change, account deletion) + the admin-UI gate. The
    // connection actor is the calling actor — no bearer/path rides (the deleted
    // HTTP twins took it from the bearer). `fauna.account.get` and
    // `fauna.account.upgrade` have no web consumer (the SPA renders the quota
    // block, not the full account-state transparency view, and has no tier-
    // upgrade UI), so they aren't surfaced here — mirroring the
    // `fauna.notifications.count` omission above.

    /// `fauna.quota.get` → `QuotaGetReply` (`{tier, inbox, storage, devices,
    /// features}`, matching the SPA's `QuotaInfo`). The `i64` byte/count slots
    /// ride through `to_js` as plain JS numbers.
    #[wasm_bindgen(js_name = quotaGet)]
    pub fn quota_get(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AccountClient::new(client)
                .quota_get()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.account.am_i_admin` → `bool` — whether the calling actor is a nest
    /// admin (so the SPA shows/hides admin UI without a separate round-trip).
    #[wasm_bindgen(js_name = accountAmIAdmin)]
    pub fn account_am_i_admin(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AccountClient::new(client)
                .am_i_admin()
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_bool(reply.admin))
        })
    }

    /// `fauna.profile.handle.change` → `ChangeHandleReply` (the queued pending
    /// action; `new_handle` is the requested handle the SPA shows
    /// optimistically). Delayed + cancellable — api-layers.md § Destructive
    /// operations are delayed.
    #[wasm_bindgen(js_name = profileHandleChange)]
    pub fn profile_handle_change(&self, handle: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AccountClient::new(client)
                .change_handle(handle)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.profile.get` — fetch a user's stored profile bytes by hex
    /// `actor_id` (the viewer's own or anyone else's). Resolves the raw stored
    /// `body` (the signed `EmbedAsBytes` wire) as a `Uint8Array`; decode it with
    /// the `decodeProfileDisplay` free function. A missing profile rejects with
    /// the `fauna.profile.not_found` error string (the SELF shell falls back to
    /// handle/actor_id). profile.md § Where logic lives → *Profile publish/edit*.
    #[wasm_bindgen(js_name = profileGet)]
    pub fn profile_get(&self, actor_id_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ProfileClient::new(client)
                .profile_get(actor_id_hex)
                .await
                .map_err(err_to_js)?;
            Ok(js_sys::Uint8Array::from(reply.body.as_ref()).into())
        })
    }

    /// The profile edit form's base load — the caller's OWN stored profile
    /// bytes, read through the shared read-prove-record so a succession link
    /// the base needs is proven and recorded in this browser's registry
    /// **before** the form can save (`profile.md` § After an identity
    /// succession → the linkless bullet; tui's
    /// `fauna_client_recovery::ceremony::load_profile_edit_base`). Resolves a
    /// `Uint8Array`, or `null` for a never-published profile (a first
    /// publish); rejects only when the read itself fails.
    #[wasm_bindgen(js_name = loadProfileEditBase)]
    pub fn load_profile_edit_base(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let keypair = keypair_from_secret_hex(&secret_hex)?;
            let body = fauna_client_recovery::ceremony::load_profile_edit_base(
                client,
                &crate::succession::account_registry(),
                &keypair,
            )
            .await
            .map_err(err_to_js)?;
            Ok(match body {
                Some(body) => js_sys::Uint8Array::from(body.as_slice()).into(),
                None => JsValue::NULL,
            })
        })
    }

    /// `fauna.profile.set` — publish/replace the caller's own profile. `body` is
    /// the signed `EmbedAsBytes` wire from the `buildEditedProfile` free
    /// function (read-modify-write + sign). Immediate; the nest verifies the
    /// signature, asserts the inner `actor_id` is the caller, then stores it
    /// (append + keep-latest prune). The SPA ignores the (empty) reply.
    #[wasm_bindgen(js_name = profileSet)]
    pub fn profile_set(&self, body: Vec<u8>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ProfileClient::new(client)
                .profile_set(body)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.account.delete` → `AccountDeleteReply` (the queued pending
    /// action with its cancellation window). No sign-out, no navigation: the
    /// account stays active until the pending action executes, and the
    /// pending-actions section (below) is the receipt + the way back
    /// (`settings.md` § Pending actions; ruled 2026-08-26 — `api-layers.md`
    /// § Where logic lives → *Account deletion*).
    #[wasm_bindgen(js_name = accountDelete)]
    pub fn account_delete(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AccountClient::new(client)
                .delete()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.pending_actions.* (`settings.md` § Pending actions) ──
    //
    // The cancellation window the three delayed verbs (handle change, account
    // delete, snapshot delete) open — read/manage complement to the creators
    // above. tui/linux twins: `apps/fauna-tui/src/settings/account.rs`,
    // `apps/fauna-linux/src/{client,settings/pending_actions}.rs`.

    /// `fauna.pending_actions.list` → this actor's queued destructive
    /// operations (ALL statuses, newest first) as `{ actions: [...] }`,
    /// unfiltered — the caller narrows to still-`pending` rows (the SPA's
    /// `pendingActionsList` in `$lib/rpc` does, mirroring tui's/linux's own
    /// `list_pending_actions` helper). Fired on Settings/Account page build,
    /// on every page-visible refresh, and after either delayed verb this
    /// page hosts completes — never trust a stale read.
    #[wasm_bindgen(js_name = pendingActionsList)]
    pub fn pending_actions_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AccountClient::new(client)
                .pending_actions_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.pending_actions.cancel` — cancel a scheduled action (one click,
    /// no confirm — cancelling is the safe direction). `id` rides as `f64`
    /// (wasm-bindgen has no native `i64`; small pending-action row ids never
    /// approach `f64`'s 53-bit exact-integer ceiling, the same pattern
    /// `emailFiltersDelete`/`adminInviteRequestsApprove` use).
    #[wasm_bindgen(js_name = pendingActionCancel)]
    pub fn pending_action_cancel(&self, id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AccountClient::new(client)
                .pending_action_cancel(id as i64)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.recovery.* (`settings.md` § Recovery kit) ──
    //
    // The RecoveryKey's Settings home — read/create/replace/lost. Calls
    // fauna_client_recovery IN-PROCESS over wasm (no FFI hop; the crate is
    // transport-generic over `RpcRequester` precisely so native and wasm
    // share one implementation). The tui/linux twins are
    // `apps/fauna-tui/src/settings/mod.rs` and
    // `apps/fauna-linux/src/{client,settings/recovery_kit}.rs`.

    /// A fresh read of `recovery-kit-status` off the registration chain
    /// (never a local flag, so a kit created on another device is reflected
    /// here) → [`RecoveryStatusJs`].
    #[wasm_bindgen(js_name = recoveryKitStatus)]
    pub fn recovery_kit_status(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let identity = keypair_from_secret_hex(&secret_hex)?;
            let recovery = RecoveryClient::new(client);
            let status = fauna_client_recovery::kit_status(&recovery, &identity.actor_id())
                .await
                .map_err(err_to_js)?;
            to_js(&recovery_status_view(&status))
        })
    }

    /// Mint the first RecoveryKey registration (`recovery-kit-create-button`,
    /// live only in `NeverCreated`) → [`RecoveryMintedJs`]. `handle` is the
    /// SPA's already-qualified `handle@domain`, or `None` pre-registration.
    #[wasm_bindgen(js_name = recoveryCreateKit)]
    pub fn recovery_create_kit_js(
        &self,
        secret_hex: String,
        handle: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            to_js(&recovery_create_kit(client, &secret_hex, handle.as_deref(), None).await?)
        })
    }

    /// Replace the registered kit using the one the user holds
    /// (`recovery-kit-replace-button`) → [`RecoveryMintedJs`]. `phrase` is the
    /// `recovery-entry-phrase-field` contents.
    #[wasm_bindgen(js_name = recoveryReplaceKit)]
    pub fn recovery_replace_kit_js(
        &self,
        secret_hex: String,
        handle: Option<String>,
        phrase: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            to_js(&recovery_replace_kit(client, &secret_hex, handle.as_deref(), &phrase).await?)
        })
    }

    /// Open a seed-alone replacement window (`recovery-kit-lost-button`) →
    /// [`RecoveryMintedJs`].
    #[wasm_bindgen(js_name = recoveryLostKit)]
    pub fn recovery_lost_kit_js(
        &self,
        secret_hex: String,
        handle: Option<String>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            to_js(&recovery_lost_kit(client, &secret_hex, handle.as_deref()).await?)
        })
    }

    /// Register the kit the onboarding `recovery_kit` screen minted and the
    /// user confirmed, at the wizard's signed-in handoff — the one point
    /// custody permits it (`identity-succession.md` § The RecoveryKey →
    /// *Creation UX*). `kit_hex` is the wizard's `takePendingRecoverySecret()`.
    /// The browser twin of `fauna_client_recovery::ceremony::register_deferred_kit`
    /// (tui, linux): a first registration with no predecessors, then the
    /// profile-head mirror. Never rejects — a failure leaves Settings'
    /// `recovery-kit-status` telling the truth, so every arm is logged.
    #[wasm_bindgen(js_name = recoveryRegisterDeferredKit)]
    pub fn recovery_register_deferred_kit_js(
        &self,
        secret_hex: String,
        kit_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let kit_hex = fauna_core::secret::SecretString::from(kit_hex);
            let identity = keypair_from_secret_hex(&secret_hex)?;
            let root = match fauna_core::recovery::RecoveryKey::from_hex(kit_hex.as_str()) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("[wasm/recovery] deferred kit: minted root unreadable: {e}");
                    return Ok(JsValue::UNDEFINED);
                }
            };
            let recovery = RecoveryClient::new(client.clone());
            match fauna_client_recovery::create_kit_with_root(&recovery, &identity, None, root, &[])
                .await
            {
                Ok(kit) => {
                    mirror_recovery_head_wasm(client, &identity, &kit).await;
                    match &kit.escrow {
                        fauna_client_recovery::EscrowOutcome::Stored { .. } => {
                            tracing::info!(
                                seq = kit.seq,
                                "[wasm/recovery] deferred kit registered"
                            );
                        }
                        fauna_client_recovery::EscrowOutcome::Failed { reason } => {
                            tracing::warn!(
                                seq = kit.seq,
                                %reason,
                                "[wasm/recovery] deferred kit registered but the escrow put failed"
                            );
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    "[wasm/recovery] deferred kit registration failed ({e}); \
                     Settings will show the never-created warning"
                ),
            }
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Contest the pending seed-alone replacement with the kit the user holds
    /// (`recovery-pending-veto-button`) at the bound nest and every linked nest
    /// → the fresh [`RecoveryStatusJs`], read
    /// inside the same call — it is the gesture's only receipt (the shared
    /// `fauna_client_recovery::veto_with_status`, tui's and linux's too).
    #[wasm_bindgen(js_name = recoveryVeto)]
    pub fn recovery_veto_js(&self, secret_hex: String, phrase: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let identity = keypair_from_secret_hex(&secret_hex)?;
            let recovery = RecoveryClient::new(client);
            let dial = crate::account_runtime::linked_nest_dial(&identity.actor_id_hex());
            let (_cancelled, status) = fauna_client_recovery::veto_with_status(
                &recovery,
                identity.actor_id(),
                &phrase,
                &dial,
            )
            .await
            .map_err(|e| JsValue::from_str(&e))?;
            to_js(&recovery_status_view(&status))
        })
    }

    /// The no-escrow repair (`recovery-kit-escrow-reseal-button`): re-put the
    /// sealed seed under the kit already in hand, WITHOUT retiring it → the
    /// fresh [`RecoveryStatusJs`] (the shared `reseal_escrow_with_status`, with
    /// this browser's registry-resolved predecessor seeds).
    #[wasm_bindgen(js_name = recoveryResealEscrow)]
    pub fn recovery_reseal_escrow_js(&self, secret_hex: String, phrase: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let identity = keypair_from_secret_hex(&secret_hex)?;
            let recovery = RecoveryClient::new(client);
            let predecessors = recovery_predecessor_seeds(&secret_hex);
            let status = fauna_client_recovery::reseal_escrow_with_status(
                &recovery,
                &identity,
                &phrase,
                &predecessors,
            )
            .await
            .map_err(|e| JsValue::from_str(&e))?;
            to_js(&recovery_status_view(&status))
        })
    }

    /// Take the account back from a stolen secret using the kit in hand
    /// (`identity-stolen-button`) → `LandedSuccessionJs`. The whole orchestration
    /// lives in [`crate::succession`], web's twin of the native
    /// `fauna_client_recovery::ceremony` module.
    ///
    /// `conversations` is the live manager when the tab has one: the sweep needs
    /// the retired identity's own engine, and this is the only moment in the
    /// page's life where it and the successor's exist at once (a beat later the
    /// SPA reloads as the successor and the old one is gone). Passing `None` is
    /// the honest `no-engine` outcome, not an error.
    ///
    /// ⚠ **This ends the session it is called from.** The nest revokes the old
    /// identity's bearers inside the succession transaction, so `self` is dead
    /// when this resolves — the SPA's next move is to sign in as the returned
    /// identity, never another call on this client.
    /// Run the whole post-succession **aftermath** for this identity — the
    /// ordered pass (legs 2, 4, 7, 6) that repairs what a succession moves
    /// ownership of but not the seal on: the `NestBackupKey` grant the nest's backup sweep enumerates owners by, the
    /// capability-grant ledger, the `__drafts` rails, and the mail plane the
    /// retired seed's holder otherwise keeps reading.
    ///
    /// ⚠ **One pass with real barriers, not four calls the SPA can sequence
    /// itself.** Which leg waits on which is the safety property, and it lives
    /// in `fauna_client_recovery::aftermath` — the same statement tui drives.
    /// This export exists so the browser can *start* it, not so JS can
    /// re-order it.
    ///
    /// `onProgress` is a JS `(leg: string, line: LocalizedText | null) => void`,
    /// called with `leg` naming one of the SPA's own `recoveryAftermath` fields
    /// (`"backupRegrant"`, `"grantRemint"`, `"draftsReseal"`, `"mailBurn"`) —
    /// once when a leg starts, once when it settles. `null` means the leg
    /// finished owing the user nothing to read, which is a value, not an
    /// omission.
    ///
    /// Resolves to `"not-a-successor"` (no pass ran) or `"ran"`, for the
    /// console ring.
    ///
    /// **Safe and free to call on every actor settle** — an identity that never
    /// succeeded has no predecessors and returns before any round trip, which is
    /// why the SPA does not try to detect a succession first. Idempotent: a
    /// completed pass reports `already-current` and writes nothing.
    ///
    /// `"no-key-opens-it"` is **not a failure** — it is the ordinary answer on a
    /// browser that never held the predecessor's seed, where the pass is still
    /// owed by a device that did.
    #[wasm_bindgen(js_name = runSuccessionAftermath)]
    pub fn run_succession_aftermath(
        &self,
        secret_hex: String,
        nest_url: String,
        on_progress: js_sys::Function,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let outcome =
                crate::succession::run_aftermath_web(client, &secret_hex, &nest_url, on_progress)
                    .await
                    .map_err(|e| JsValue::from_str(&e))?;
            Ok(JsValue::from_str(outcome))
        })
    }

    /// Whether the ephemeral kit-side member-review pass should render this
    /// session (`succession-aftermath.md` § Propagation, item (ii)) —
    /// `runSuccessionAftermath` witnessed a real sweep the instant it ran, so
    /// this is a synchronous local read, not a round trip. `false` on every
    /// ordinary sign-in and on a successor's *second* one, same as tui's
    /// `App::succession_sweep.is_none()`. The roster itself is the caller's
    /// own `memberReviewList` / `$lib/member-reviews` read — this answers only
    /// "was a sweep the reason it's non-empty right now".
    #[wasm_bindgen(js_name = ephemeralReviewPassActive)]
    pub fn ephemeral_review_pass_active(&self, secret_hex: String) -> Result<bool, JsValue> {
        let keypair = keypair_from_secret_hex(&secret_hex)?;
        Ok(crate::succession::ephemeral_review_pass_active(
            &keypair.actor_id_hex(),
        ))
    }

    /// *Review The Rest Later* — hides the pass and decides nothing: the open
    /// items stay exactly where they are on the account plane
    /// (`fauna.state.succession-ledger`), inherited by the
    /// permanent review page. Clears the witness above; never touches the
    /// roster.
    #[wasm_bindgen(js_name = deferEphemeralReviewPass)]
    pub fn defer_ephemeral_review_pass(&self, secret_hex: String) -> Result<(), JsValue> {
        let keypair = keypair_from_secret_hex(&secret_hex)?;
        crate::succession::clear_ephemeral_review_pass_witness(&keypair.actor_id_hex());
        Ok(())
    }

    /// The post-succession sweep's own lines for this identity — `{ outcome,
    /// unattested, owesWork }`, each line a `LocalizedText | null` selected by
    /// the shared `SweepView::copy` — or `null` when no succession ran in this
    /// tab (`settings.md` § Recovery kit → *The sweep's own lines*). A
    /// synchronous local read like `ephemeralReviewPassActive`, and for the
    /// same reason: the ceremony parked its view across the document swap.
    #[wasm_bindgen(js_name = successionSweepCopy)]
    pub fn succession_sweep_copy(&self, secret_hex: String) -> Result<JsValue, JsValue> {
        let keypair = keypair_from_secret_hex(&secret_hex)?;
        match crate::succession::succession_sweep_copy(&keypair.actor_id_hex()) {
            // `to_js`, never a bare `serde_wasm_bindgen::to_value`:
            // `LocalizedText::args` is a map, and the default serializer would
            // hand `resolveLocalized` a JS `Map` with no own properties.
            Some(copy) => to_js(&copy),
            None => Ok(JsValue::NULL),
        }
    }

    /// `recovery-kit-sweep-retry-button`'s press — the answer, as a
    /// `LocalizedText` the SPA resolves and paints on `error-message`.
    ///
    /// Never `null`: the button "must answer in words on every press"
    /// (`settings.md` § Recovery kit → *Finishing an unfinished group sweep*),
    /// so a silent arm would be a dropped command (testing.md point 11). On web
    /// the arm is always the same one and the reasoning is
    /// [`crate::succession::succession_sweep_retry`]'s; the sentence itself is
    /// the shared projection's, identical to the one a native device without
    /// the retired identity's conversation history gives.
    #[wasm_bindgen(js_name = successionSweepRetry)]
    pub fn succession_sweep_retry(&self) -> Result<JsValue, JsValue> {
        // `to_js` for `successionSweepCopy`'s reason: `LocalizedText::args` is a
        // map, and the default serializer hands `resolveLocalized` a JS `Map`
        // with no own properties.
        to_js(&crate::succession::succession_sweep_retry())
    }

    /// `nestUrl` is the nest the successor's account row is BOUND to;
    /// `dialUrl` is where the ceremony's sockets go. Two strings on web (see
    /// [`crate::succession::succeed_with_held_kit_web`]) — same in production,
    /// different under e2e automation.
    ///
    /// Resolves to the ceremony's typed outcome (`StolenOutcomeJs` — `kind`,
    /// `message`, `landed`, `carriesTheOnlySeed`) on every arm of a ceremony
    /// that ran; rejects only when it could not start (an unreadable secret).
    #[wasm_bindgen(js_name = succeedIdentityWithHeldKit)]
    pub fn succeed_identity_with_held_kit(
        &self,
        secret_hex: String,
        phrase: String,
        nest_url: String,
        dial_url: String,
        conversations: Option<crate::conversations::WasmConversationsManager>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let old = conversations.as_ref().and_then(|c| c.succession_engine());
            let outcome = crate::succession::succeed_with_held_kit_web(
                client,
                &secret_hex,
                &phrase,
                &nest_url,
                &dial_url,
                old.as_ref(),
            )
            .await
            .map_err(|e| JsValue::from_str(&e))?;
            // `to_js` for `successionSweepCopy`'s reason: `message.args` is a map.
            to_js(&outcome)
        })
    }

    // ── admin mail / DNS machines ───────────────────────────────────
    //
    // Each factory builds a fresh stateful machine (`libs/fauna-client-{dns,
    // mail-settings}`) over this client — the wasm twin of the native
    // `build_*_machine(Arc<NestClient>)` `fauna-ffi`/linux call. The SPA holds one
    // per mounted admin page; `hydrate()` loads the initial snapshot. See
    // `crate::mail_admin`.

    /// `WasmDnsManagementMachine` for the web `admin-dns` page — read/verify-only
    /// (record matrix + live red/green). For the managed-mode credential surface,
    /// see `dnsManagementMachineWithCredentials`.
    #[wasm_bindgen(js_name = dnsManagementMachine)]
    pub fn dns_management_machine(&self) -> crate::mail_admin::WasmDnsManagementMachine {
        crate::mail_admin::WasmDnsManagementMachine::build(self.inner.clone())
    }

    /// The **managed-mode** `WasmDnsManagementMachine` for the web `admin-dns`
    /// page — the read/verify surface plus the client-held DNS-provider
    /// credential store (the account's `fauna.state.dns` row, through this tab's
    /// account runtime — the nest never sees the provider key) + publish/verify
    /// provider seam. Takes the actor's 32-byte ed25519 `secret` (the issuance
    /// seal signs with it), unlike the read-only `dnsManagementMachine` which
    /// needs only the connection.
    #[wasm_bindgen(js_name = dnsManagementMachineWithCredentials)]
    pub fn dns_management_machine_with_credentials(
        &self,
        secret: Vec<u8>,
    ) -> Result<crate::mail_admin::WasmDnsManagementMachine, JsValue> {
        crate::mail_admin::WasmDnsManagementMachine::build_with_credentials(
            self.inner.clone(),
            secret,
        )
    }

    /// `WasmLocalDomainMachine` for the web `admin-dns` page.
    #[wasm_bindgen(js_name = localDomainMachine)]
    pub fn local_domain_machine(&self) -> crate::mail_admin::WasmLocalDomainMachine {
        crate::mail_admin::WasmLocalDomainMachine::build(self.inner.clone())
    }

    /// `WasmBridgeApprovalMachine` for the web `admin-bridges-pending` page.
    #[wasm_bindgen(js_name = bridgeApprovalMachine)]
    pub fn bridge_approval_machine(&self) -> crate::mail_admin::WasmBridgeApprovalMachine {
        crate::mail_admin::WasmBridgeApprovalMachine::build(self.inner.clone())
    }

    /// `WasmMailPolicyMachine` for the web flat `admin-mail` policy page
    /// (`admin.md` § Mail / `mail-policy-config.md` Tier 2/3). Admin-class,
    /// connection-only — like `forwarderMachine`.
    #[wasm_bindgen(js_name = mailPolicyMachine)]
    pub fn mail_policy_machine(&self) -> crate::mail_admin::WasmMailPolicyMachine {
        crate::mail_admin::WasmMailPolicyMachine::build(self.inner.clone())
    }

    /// `WasmCaldavPolicyMachine` for the web flat `admin-calendar` page
    /// (`admin.md` § 8 Calendar / `caldav-server.md` § Independent enablement) —
    /// the deployment-wide CalDAV-enable toggle. Admin-class, connection-only,
    /// the sibling of `mailPolicyMachine`.
    #[wasm_bindgen(js_name = caldavPolicyMachine)]
    pub fn caldav_policy_machine(&self) -> crate::mail_admin::WasmCaldavPolicyMachine {
        crate::mail_admin::WasmCaldavPolicyMachine::build(self.inner.clone())
    }

    /// `WasmCarddavPolicyMachine` for the web flat `admin-contacts` page
    /// (`admin.md` § Contacts / `carddav-server.md` § Independent enablement) —
    /// the deployment-wide CardDAV-enable toggle. Admin-class, connection-only,
    /// the contacts sibling of `caldavPolicyMachine`.
    #[wasm_bindgen(js_name = carddavPolicyMachine)]
    pub fn carddav_policy_machine(&self) -> crate::mail_admin::WasmCarddavPolicyMachine {
        crate::mail_admin::WasmCarddavPolicyMachine::build(self.inner.clone())
    }

    /// `WasmWebdavPolicyMachine` for the web flat `admin-files` page
    /// (`admin.md` § Files / `webdav-server.md` § Independent enablement) —
    /// the deployment-wide WebDAV-enable toggle. Admin-class, connection-only,
    /// the files sibling of `carddavPolicyMachine`.
    #[wasm_bindgen(js_name = webdavPolicyMachine)]
    pub fn webdav_policy_machine(&self) -> crate::mail_admin::WasmWebdavPolicyMachine {
        crate::mail_admin::WasmWebdavPolicyMachine::build(self.inner.clone())
    }

    /// `WasmForwarderMachine` for the web `admin-aliases` page (admin external
    /// forwarders — `admin.md` § 4 / `mail-aliases.md` § Kind 7).
    #[wasm_bindgen(js_name = forwarderMachine)]
    pub fn forwarder_machine(&self) -> crate::mail_admin::WasmForwarderMachine {
        crate::mail_admin::WasmForwarderMachine::build(self.inner.clone())
    }

    /// `WasmMailSettingsMachine` for the web user-facing `mail-settings` page.
    /// Takes the actor's 32-byte ed25519 `secret` (signs submission tokens +
    /// derives the account's mail custody) and the deployment `node_url` (MUA connection
    /// details), unlike the admin factories which need only the connection.
    #[wasm_bindgen(js_name = mailSettingsMachine)]
    pub fn mail_settings_machine(
        &self,
        secret: Vec<u8>,
        node_url: String,
    ) -> Result<crate::mail_admin::WasmMailSettingsMachine, JsValue> {
        crate::mail_admin::WasmMailSettingsMachine::build(self.inner.clone(), secret, node_url)
    }

    /// The post-claim serving enablement (`onboarding.md` § 3b *Mechanism*) —
    /// the web `LoggedIn` handoff's ONE call, running the shared
    /// `fauna_client_mail_settings::serving_enablement::apply_serving_enablement`
    /// the native apps run, over this browser connection. The four flags are
    /// the onboarding machine's `*EnableRequested()` intents. Resolves
    /// `undefined` once every step has answered (each logs its own failure);
    /// progress is `servingEnablementJson()`.
    #[wasm_bindgen(js_name = applyPostClaimServingEnablement)]
    pub fn apply_post_claim_serving_enablement(
        &self,
        secret: Vec<u8>,
        node_url: String,
        email: bool,
        caldav: bool,
        carddav: bool,
        webdav: bool,
    ) -> Result<js_sys::Promise, JsValue> {
        crate::mail_admin::apply_post_claim_serving_enablement(
            self.inner.clone(),
            secret,
            node_url,
            fauna_client_mail_settings::serving_enablement::ServingEnablementIntents {
                email,
                caldav,
                carddav,
                webdav,
            },
        )
    }

    /// `WasmMailAliasesMachine` for the web user-facing `mail-aliases` page (a
    /// sub-section of the mail-settings family). User-class, owner-scoped — needs
    /// only the connection (the nest derives the owning actor from the
    /// authenticated caller), so it takes no secret unlike `mailSettingsMachine`.
    #[wasm_bindgen(js_name = mailAliasesMachine)]
    pub fn mail_aliases_machine(&self) -> crate::mail_admin::WasmMailAliasesMachine {
        crate::mail_admin::WasmMailAliasesMachine::build(self.inner.clone())
    }

    /// `WasmMailSpamMachine` for the web user-facing `mail-spam` page (another
    /// sub-section of the mail-settings family). Owner-scoped, but undo of a
    /// client-written (sealed) training row runs the reseal loop — so like
    /// `mailSettingsMachine` it takes the actor `secret` + `node_url` to build the
    /// sealed-model writer (`mail-spam.md` § Encrypted-mode interaction).
    #[wasm_bindgen(js_name = mailSpamMachine)]
    pub fn mail_spam_machine(
        &self,
        secret: Vec<u8>,
        node_url: String,
    ) -> Result<crate::mail_admin::WasmMailSpamMachine, JsValue> {
        crate::mail_admin::WasmMailSpamMachine::build(self.inner.clone(), secret, node_url)
    }

    /// `WasmMailExportMachine` for the web user-facing `mail-export` page (another
    /// sub-section of the mail-settings family). Owner-scoped, and the machine
    /// has key custody — so like `mailSpamMachine` it takes the actor `secret` +
    /// `node_url`, plus the user's `handle` (the archive's root directory) and
    /// the page's save port, the browser half of `mail-export.md` § Download
    /// flow (`{ openDownload, createArchive }`, `$lib/mail-export-save`).
    #[wasm_bindgen(js_name = mailExportMachine)]
    pub fn mail_export_machine(
        &self,
        secret: Vec<u8>,
        node_url: String,
        handle: String,
        save_port: crate::mail_export_delivery::MailExportSavePort,
    ) -> Result<crate::mail_admin::WasmMailExportMachine, JsValue> {
        crate::mail_admin::WasmMailExportMachine::build(
            self.inner.clone(),
            secret,
            node_url,
            handle,
            save_port,
        )
    }

    /// `WasmMailImportMachine` for the web user-facing `mail-import` page — the
    /// export twin's mirror image. User-class, owner-scoped, so it needs only
    /// the connection. Its source-side seam is the one stub left in the family
    /// (see the wrapper's own docs): the nest half is real, so everything but
    /// `Connect` works on web today.
    #[wasm_bindgen(js_name = mailImportMachine)]
    pub fn mail_import_machine(&self) -> crate::mail_admin::WasmMailImportMachine {
        crate::mail_admin::WasmMailImportMachine::build(self.inner.clone())
    }

    /// `WasmMailListsMachine` for the web user-facing `mail-lists` page (another
    /// sub-section of the mail-settings family — a list is a sixth alias kind).
    /// User-class, owner-scoped — needs only the connection, like `mailSpamMachine`.
    #[wasm_bindgen(js_name = mailListsMachine)]
    pub fn mail_lists_machine(&self) -> crate::mail_admin::WasmMailListsMachine {
        crate::mail_admin::WasmMailListsMachine::build(self.inner.clone())
    }

    /// `WasmMailListMembersMachine` for the web user-facing `mail-list-members`
    /// drill-down off a `mail-lists` row. Scoped to one list, so it takes the
    /// list's hex id + friendly name (the page heading) and returns `Result`
    /// (a malformed hex id rejects), unlike the owner-scoped connection-only
    /// factories. Built lazily when the user opens a list's members view.
    #[wasm_bindgen(js_name = mailListMembersMachine)]
    pub fn mail_list_members_machine(
        &self,
        list_id_hex: String,
        list_name: String,
    ) -> Result<crate::mail_admin::WasmMailListMembersMachine, JsValue> {
        crate::mail_admin::WasmMailListMembersMachine::build(
            self.inner.clone(),
            list_id_hex,
            list_name,
        )
    }

    /// `WasmLinkedNestsMachine` for the web user-settings `linked-nests` page.
    /// `peer_token_provider_factory(peerUrl) => (forceRefresh) => Promise<string>`
    /// lets the both-ends `LinkBoth` path open a second authenticated client to a
    /// peer nest (the SPA's `getAuthToken(secret, peerUrl, …)` over `challengeVerify`,
    /// minting the peer bearer on the CORS-exempt anonymous WS). The single-end
    /// `Link` path ignores it.
    #[wasm_bindgen(js_name = linkedNestsMachine)]
    pub fn linked_nests_machine(
        &self,
        peer_token_provider_factory: js_sys::Function,
    ) -> crate::pairing::WasmLinkedNestsMachine {
        crate::pairing::WasmLinkedNestsMachine::build(
            self.inner.clone(),
            peer_token_provider_factory,
        )
    }

    /// `WasmLinkedNestsMachine` for the web **Nests** page — both-ends linking
    /// (as [`Self::linked_nests_machine`]) *plus* the trust facet (content-
    /// processor holder discovery + the signed grant-event log's Now/History
    /// folds + Mint/Renew/Revoke), the wasm twin of native's
    /// `build_linked_nests_machine_with_trust`. `secret` is the actor's 32-byte
    /// ed25519 identity — the trust seams sign the grant-event log
    /// (`fauna.state.succession-ledger`) with it (the raw key never returns to JS).
    #[wasm_bindgen(js_name = linkedNestsMachineWithTrust)]
    pub fn linked_nests_machine_with_trust(
        &self,
        peer_token_provider_factory: js_sys::Function,
        secret: Vec<u8>,
    ) -> Result<crate::pairing::WasmLinkedNestsMachine, JsValue> {
        crate::pairing::WasmLinkedNestsMachine::build_with_trust(
            self.inner.clone(),
            peer_token_provider_factory,
            secret,
        )
    }

    // ── The Backups page's snapshot half is NOT here ────────────────
    //
    // Its eight `fauna.filesync.snapshot.*` / `fauna.sync.backup_status`
    // bindings were deleted on 2026-08-05 when the web page adopted the shared
    // `BackupsMachine` (`libs/fauna-backups-machine`, chunk
    // `libs/fauna-wasm-backups`): the machine composes those kinds in Rust over
    // the same `SnapshotsClient<R>` and hands the page one renderable snapshot,
    // so a per-call binding here is exactly the boundary drift § Snapshot-list
    // shape retires. Notably `snapshotGet`'s custody wiring lives on the
    // machine's seam now — see the chunk constructor's own note on why keyless
    // custody there would reintroduce the same bug. The message-kind RESTORE surfaces
    // further down are a different page half and stay. The snapshot byte
    // downloads (ZIP restore + single-file) remain HTTP residue.

    /// `fauna.sync.devices.list` → `SyncDevice[]` (`{ device_id, label, … }`) —
    /// the actor's device roster, over this client's existing connection.
    ///
    /// The Task-delegation sub-page joins it by hex `device_id` to name the
    /// participant running a task kind ("Running on ‹device›"), the same roster
    /// join linux and windows do; the shared row deliberately carries the
    /// `ParticipantRef`, not a name, because device names are client-side state
    /// (`fauna_core::delegation::RunnerStatus`). A thin kind rather than a second
    /// `DevicesMachine` — the full roster machine (`fauna_wasm_folders`) opens its
    /// own connection, which a read-only label join does not need.
    #[wasm_bindgen(js_name = syncDevicesList)]
    pub fn sync_devices_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SyncClient::new(client)
                .devices_list()
                .await
                .map_err(err_to_js)?;
            to_js(&reply.devices)
        })
    }

    /// `fauna.sync.status` → `{ folder, source_online, destinations }` — the media page's sync-state indicator (reads
    /// `source_online`). Twin of `GET /api/v1/sync/status`.
    #[wasm_bindgen(js_name = syncStatus)]
    pub fn sync_status(&self, folder: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SyncClient::new(client)
                .status(folder)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.sync.files` → `SyncFile[]` (`{ path, manifest_hash, size_bytes,
    /// updated_at }`) — the media page's file list. Twin of
    /// `GET /api/v1/sync/files`.
    #[wasm_bindgen(js_name = syncFiles)]
    pub fn sync_files(&self, folder: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SyncClient::new(client)
                .files(folder)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.files)
        })
    }

    /// `fauna.folders.members.list_actors` — the *actor* (user) roster of a
    /// shared folder: who the set is shared with (the owner-side "Shared with"
    /// list). Resolves to an array of `{ actor_id, handle, role }` — the hex
    /// actor id, its nest-resolved handle (empty when unknown), and `role`
    /// (`"owner"` | `"member"`). A pure transport read (no MLS engine), unlike
    /// the `foldersShareSet` author methods on the conversations manager.
    #[wasm_bindgen(js_name = foldersActorMembers)]
    pub fn folders_actor_members(&self, name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FoldersClient::new(client)
                .actor_members_list(name)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.members)
        })
    }

    /// `fauna.folders.members.list` — the enrolled **device** roster for one
    /// folder (`{ device_id, label, role, flags }`), as the nest gave it.
    ///
    /// Distinct from `foldersActorMembers`, which is the cross-user *actor*
    /// roster ("Shared with"); this is the device roster the places editor
    /// paints. A pure transport read, no MLS engine — the SPA resolves each
    /// seat's checkboxes by putting this reply through `placeRows` on the
    /// folders face, never by reading `role` or `flags` itself.
    #[wasm_bindgen(js_name = foldersMembers)]
    pub fn folders_members(&self, name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FoldersClient::new(client)
                .members_list(name)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.members)
        })
    }

    /// `fauna.folders.devices` → `FolderDevice[]` (`{ device_id, label,
    /// last_change_at, change_count }`) — the per-device recorded sync activity
    /// for one folder (`folder-device-activity-item`/`-label`/`-count`,
    /// lazy-loaded on row expand, same as `foldersActorMembers`). Ordinary
    /// sync-type activity signal, distinct from `cached_snapshot_count`/
    /// `cached_total_bytes` (snapshot-only) — file-sync.md § Implementation
    /// status today.
    #[wasm_bindgen(js_name = foldersDevices)]
    pub fn folders_devices(&self, name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FoldersClient::new(client)
                .devices(name)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.devices)
        })
    }

    /// `fauna.folders.members.set_access` — the **owner** grants or edits a
    /// member's `"reader"`/`"writer"` access on their shared set (multi-writer
    /// Phase 1; the write behind `folder-member-role-select` /
    /// `folder-member-cap-input`). `byte_cap` is the writer byte cap in bytes
    /// (`undefined` = uncapped — the ratified blank-cap-means-uncapped; passed
    /// as `f64` per the wasm no-`i64`-param rule, integral values only). Role
    /// transitions never rotate the content key — removal is a member evict.
    #[wasm_bindgen(js_name = foldersMemberSetAccess)]
    pub fn folders_member_set_access(
        &self,
        name: String,
        actor_id: String,
        access: String,
        byte_cap: Option<f64>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = FoldersClient::new(client)
                .members_set_access(fauna_protocol::folders::MemberSetAccessRequest {
                    name,
                    actor_id,
                    access,
                    byte_cap: byte_cap.map(|c| c as i64),
                    ..Default::default()
                })
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── Destination places (backup-destinations.md § Ordinary-folder coverage) ──
    //
    // The folders page's per-folder *Destination places* section: attach/detach an
    // enrolled backup destination to ONE ordinary folder. web is the first
    // non-linking consumer of `fauna_client_config::{list_folder_destinations,
    // attach_folder_to_destination, detach_folder_from_destination}` (linux/tui
    // link the crate directly — the uncalled-export rule). `folder_id` crosses as
    // `f64` per the wasm no-`i64`-param rule; each mutation re-reads the folder's
    // places from the nest afterwards and returns THAT, never an optimistic flip
    // — the same non-optimistic contract linux's `attach_folder_destination` /
    // `detach_folder_destination` follow.

    /// `fauna.backup.destination.list` joined with this box's
    /// `fauna.state.backup` display names, for ONE folder — the section's
    /// lazy-on-first-expand read, mirroring `foldersDevices`. `secret_hex` is
    /// kept for the JS signature only; none of the trio reads it any more (the
    /// list rides this tab's account store, keyed by `bound_nest_id`).
    #[wasm_bindgen(js_name = folderDestinationsList)]
    pub fn folder_destinations_list(&self, secret_hex: String, folder_id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let _ = secret_hex;
            let source_nest = bound_nest_id(&client).await?.0;
            let store = crate::account_runtime::backup_seam();
            let places = list_folder_destinations(client, &*store, source_nest, folder_id as i64)
                .await
                .map_err(err_to_js)?;
            to_js(&places)
        })
    }

    /// Attach `folder_id` to `destination_id`
    /// (`fauna_client_config::attach_folder_to_destination`), then re-read this
    /// folder's places so the caller repaints from the nest's own answer. Mirrors
    /// `folderDestinationDetach`.
    #[wasm_bindgen(js_name = folderDestinationAttach)]
    pub fn folder_destination_attach(
        &self,
        secret_hex: String,
        folder_id: f64,
        destination_id: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let _ = secret_hex;
            let source_nest = bound_nest_id(&client).await?.0;
            let store = crate::account_runtime::backup_seam();
            attach_folder_to_destination(
                client.clone(),
                &*store,
                source_nest,
                &destination_id,
                folder_id as i64,
            )
            .await
            .map_err(err_to_js)?;
            // The attach itself succeeded — a re-read failure here is not the
            // caller's error to see (mirrors linux's `attach_folder_destination`).
            let places = list_folder_destinations(client, &*store, source_nest, folder_id as i64)
                .await
                .unwrap_or_default();
            to_js(&places)
        })
    }

    /// Detach `folder_id` from `destination_id`
    /// (`fauna_client_config::detach_folder_from_destination`), then re-read this
    /// folder's places. `folder_set` is the attached row's own
    /// `__folder/<hex>/<id>` name, carried by the `FolderDestinationPlace` the
    /// detach button's row was built from — never re-derived here.
    #[wasm_bindgen(js_name = folderDestinationDetach)]
    pub fn folder_destination_detach(
        &self,
        secret_hex: String,
        folder_id: f64,
        destination_id: String,
        folder_set: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let _ = secret_hex;
            let source_nest = bound_nest_id(&client).await?.0;
            let store = crate::account_runtime::backup_seam();
            detach_folder_from_destination(
                client.clone(),
                &*store,
                source_nest,
                &destination_id,
                folder_id as i64,
                &folder_set,
            )
            .await
            .map_err(err_to_js)?;
            // Same posture as folderDestinationAttach: the detach already landed.
            let places = list_folder_destinations(client, &*store, source_nest, folder_id as i64)
                .await
                .unwrap_or_default();
            to_js(&places)
        })
    }

    // ── Backups restore surfaces (message-kind path) ────────────────
    //
    // The restore picker / history / divergence reads + the local restore
    // action, mirroring the Linux app's `views/backups/restore.rs`
    // (`docs/goal/ui/backups.md` §§ Restore history / Restore divergence /
    // Restore from backup destination). All four ride the same shared
    // `SnapshotsClient<R>` (priority #2) — these are the thin wasm seam.

    /// `fauna.filesync.snapshot.list` (message-kind mode) →
    /// `SnapshotSummaryRow[]`. Owner-implicit list of the bearer's
    /// message-kind snapshots (`mail` / `calendar`), newest first — the
    /// `restore-snapshot-select` local-snapshot picker's data source.
    #[wasm_bindgen(js_name = snapshotListMessageKind)]
    pub fn snapshot_list_message_kind(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SnapshotsClient::new(client)
                .list(None, None, 0)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.rows)
        })
    }

    /// `fauna.filesync.snapshot.list_restore_history` →
    /// `RestoreHistoryRow[]`. Backs the Backups restore-history section.
    #[wasm_bindgen(js_name = snapshotListRestoreHistory)]
    pub fn snapshot_list_restore_history(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SnapshotsClient::new(client)
                .list_restore_history(0)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.rows)
        })
    }

    /// `fauna.filesync.snapshot.list_restore_divergence` →
    /// `RestoreDivergenceRow[]` for one snapshot. Backs the per-row
    /// divergence banner + the forensic details modal.
    #[wasm_bindgen(js_name = snapshotListRestoreDivergence)]
    pub fn snapshot_list_restore_divergence(&self, snapshot_id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SnapshotsClient::new(client)
                .list_restore_divergence(snapshot_id as i64)
                .await
                .map_err(err_to_js)?;
            to_js(&reply.rows)
        })
    }

    /// `fauna.filesync.snapshot.restore_message_kind` — owner-only
    /// message-kind restore. `confirm_id` must equal `snapshot_id`
    /// stringified (the re-type friction bar). Returns the reply
    /// (`config_present` / `note` warn if the wrapped-MLS-blob bundle isn't restored yet).
    #[wasm_bindgen(js_name = snapshotRestoreMessageKind)]
    pub fn snapshot_restore_message_kind(
        &self,
        snapshot_id: f64,
        confirm_id: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SnapshotsClient::new(client)
                .restore_message_kind(snapshot_id as i64, confirm_id)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── backup destinations (backups.md § Manage backup destinations) ──
    //
    // Add / edit / remove the cross-location backup destinations on the Backups
    // page — the wasm twin of the linux `views/backups/destinations.rs` glue.
    // Each method sequences the shared calls (resolve + the
    // `add_/edit_/remove_backup_destination` mutate helpers inside
    // `mutate_backup`, the one write door onto this box's `fauna.state.backup`
    // row via `crate::account_runtime::backup_seam`) and returns the new
    // `BackupDestination[]` so the SPA re-renders its status rows. The row is
    // keyed by the id this connection is bound to (`bound_nest_id`); an id that
    // cannot be proven rejects the call — never another box's list. The owner
    // secret rides as hex (the page already holds it for the bearer mint /
    // caldav), consumed by the enroll grant and the edit's re-resolve.

    /// This box's `fauna.state.backup` destination list — the status-row list,
    /// each row carrying its post-succession review answer
    /// ([`BackupDestinationJs`]).
    ///
    /// One row per enrolled destination: coverage rows (per-folder mirror-set
    /// rows sharing a destination_id) are folded away before crossing the
    /// wasm boundary, so the page never renders one destination N+1 times for
    /// its N covered folders (`fauna_core::data::
    /// distinct_destinations`'s own doc).
    ///
    /// The mark rides on the rows rather than arriving as a second list because
    /// both halves must come from **one** `BackupState` read (`state.backup`
    /// and `state.marks`): tui reads them together for the same reason
    /// (`apps/fauna-tui/src/backups.rs`), and a second load would let a row
    /// and its mark disagree about which read they came from. The read is
    /// `load_backup_state_refiled`, the Backups page's list read.
    ///
    /// ⚠ The fold runs FIRST and the mark is resolved on what survives it: the
    /// verdict is keyed by `destination_id`, which every coverage row of a
    /// destination shares, so marking before folding would answer the same
    /// question N+1 times and marking a folded-away row would answer it for a
    /// row nothing renders.
    #[wasm_bindgen(js_name = backupDestinationList)]
    pub fn backup_destination_list(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            // The owner is this socket's actor; the secret is no longer needed
            // to read the (account-store) list.
            let _ = secret_hex;
            let source_nest = bound_nest_id(&client).await?.0;
            let state = fauna_client_config::load_backup_state_refiled(
                &*crate::account_runtime::backup_seam(),
                &client,
                source_nest,
            )
            .await
            .map_err(err_to_js)?;
            let rows: Vec<BackupDestinationJs> =
                fauna_core::data::distinct_destinations(&state.backup.destinations)
                    .into_iter()
                    .map(|d| BackupDestinationJs {
                        unattested: fauna_core::data::DestinationUnattestedMark::row_is_raised(
                            &state.marks,
                            &d,
                        ),
                        inner: d,
                    })
                    .collect();
            to_js(&rows)
        })
    }

    /// **Keep** one raised destination — the owner answering "I recognise this".
    ///
    /// Clears that row's review mark and leaves the destination enrolled; Keep
    /// is not Remove (the row stays, and stays removable forever after). The
    /// write is the shared `keep_backup_destination_at_rest` rather than an
    /// open-coded load/put precisely so it rides `mutate_backup`'s mark merge —
    /// a blind put here would clobber a concurrent device's adjudication, which
    /// is how tui's own copy once drifted onto the blind path.
    ///
    /// Answers whether anything actually changed: a Keep on a row already
    /// answered (or already gone) writes nothing and reports `false`, which is
    /// a legitimate outcome and not an error.
    #[wasm_bindgen(js_name = backupDestinationKeep)]
    pub fn backup_destination_keep(
        &self,
        secret_hex: String,
        destination_id: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            // The owner is this socket's actor; the write needs no secret.
            let _ = secret_hex;
            let source_nest = bound_nest_id(&client).await?.0;
            let changed = keep_backup_destination_at_rest(
                &*crate::account_runtime::backup_seam(),
                source_nest,
                &destination_id,
            )
            .await
            .map_err(err_to_js)?;
            Ok(JsValue::from_bool(changed))
        })
    }

    /// Add a destination: resolve identity + verify reachability/authorization
    /// (keeping an authed session to the destination open), then run the
    /// shared enroll sequence (grant + writer-grant register + destination
    /// register, then the atomic `fauna.state.backup` write). A blank name defaults to
    /// the resolved domain. A resolve/save failure rejects the promise and
    /// records nothing. Returns the new destination list.
    #[wasm_bindgen(js_name = backupDestinationAdd)]
    pub fn backup_destination_add(
        &self,
        secret_hex: String,
        url: String,
        name: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let (actor_pubkey, domain, destination) =
                resolve_and_authorize_destination_inner(&secret_hex, &url).await?;
            let secret = hex_array_32(&secret_hex)?;
            // The writer the destination authorizes is the id this connection
            // proved, never the nest's own claim.
            let source_nest_id = bound_nest_id(&client).await?.0;
            // The shared enroll sequence — grant the source nest this owner's
            // `NestBackupKey`, register the nest-writer grant at the
            // destination, register the destination with the source nest,
            // then record it (the atomic decision point). Only the resolve
            // above is wasm-specific.
            let destinations = enroll_backup_destination(
                client,
                destination,
                &*crate::account_runtime::backup_seam(),
                secret,
                ResolvedDestination {
                    destination_id: new_destination_id(),
                    destination_nest_url: url,
                    destination_actor_pubkey: actor_pubkey,
                    domain,
                    requested_name: name,
                },
                source_nest_id,
            )
            .await
            .map_err(err_to_js)?;
            to_js(&fauna_core::data::distinct_destinations(&destinations))
        })
    }

    /// Stand in for ANOTHER device of this owner enrolling itself as a
    /// client-device custodian, so an e2e test can witness what web shows for
    /// one: the kind badge, the usage line and the sole-client warning
    /// (`ui/backups.md` § Third destination kind). Enrolment is declared absent
    /// on web — it runs on the device being enrolled — so no web gesture can
    /// produce the row, and this seam is the other device's act, never web's.
    /// It runs the same shared `enroll_client_custodian` every enrolling app
    /// calls, with `device_id` naming the stand-in device. `capacity` is typed
    /// text read by the shared `parse_byte_size`; blank is uncapped. Returns
    /// the new destination list.
    ///
    /// Compiled out of every release artifact (testing.md convention 15); the
    /// `ForTest` suffix is what `scripts/check-wasm-seam-exclusion.py` keys on.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = backupDestinationEnrollCustodianForTest)]
    pub fn backup_destination_enroll_custodian_for_test(
        &self,
        secret_hex: String,
        device_id: String,
        name: String,
        capacity: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let _ = secret_hex;
            let source_nest = bound_nest_id(&client).await?.0;
            let typed = capacity.trim();
            let capacity_cap_bytes =
                if typed.is_empty() {
                    None
                } else {
                    Some(fauna_core::format::parse_byte_size(typed).ok_or_else(|| {
                        JsValue::from_str(&format!("unreadable capacity {typed:?}"))
                    })?)
                };
            let destinations = fauna_client_config::enroll_client_custodian(
                client,
                &*crate::account_runtime::backup_seam(),
                source_nest,
                fauna_client_config::CustodianEnrollment {
                    destination_id: new_destination_id(),
                    custodian_device_id: device_id,
                    display_name: name,
                    capacity_cap_bytes,
                },
            )
            .await
            .map_err(err_to_js)?;
            to_js(&fauna_core::data::distinct_destinations(&destinations))
        })
    }

    /// Edit a destination's display name and/or URL. A URL change must point at
    /// the SAME nest (re-resolve + pubkey compare; a different nest is remove +
    /// re-add, rejected inline — backups.md § Edit). Re-resolve only when the URL
    /// changed (renaming an offline destination still works). A blank name clears
    /// the display name (the row falls back to the domain). Returns the new list.
    #[wasm_bindgen(js_name = backupDestinationEdit)]
    pub fn backup_destination_edit(
        &self,
        secret_hex: String,
        id: String,
        url: String,
        name: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let source_nest = bound_nest_id(&client).await?.0;
            let store = crate::account_runtime::backup_seam();
            let state = fauna_client_config::load_backup_state(&*store, source_nest)
                .await
                .map_err(err_to_js)?;

            if let Some(stored) = state
                .backup
                .destinations
                .iter()
                .find(|d| d.destination_id == id)
                .cloned()
                && stored.destination_nest_url != url
            {
                let (actor_pubkey, _) = resolve_destination_inner(&secret_hex, &url).await?;
                if actor_pubkey != stored.destination_actor_pubkey {
                    return Err(JsValue::from_str(
                        "That URL points to a different nest. Remove this destination and add the new one.",
                    ));
                }
            }

            let display_name = if name.trim().is_empty() {
                None
            } else {
                Some(name)
            };
            // Through the one write door, never a blind put: `mutate_backup`
            // re-reads the row and applies the edit to what it finds, so a
            // concurrent device's write in the load→put window survives (the
            // same-nest check above is validated against the earlier read — see
            // tui's twin for why that is safe).
            let (state, _) = mutate_backup(&*store, source_nest, |state| {
                edit_backup_destination(state, &id, display_name, url)
            })
            .await
            .map_err(err_to_js)?;
            to_js(&fauna_core::data::distinct_destinations(
                &state.backup.destinations,
            ))
        })
    }

    /// Remove a destination: deregister it from the source nest's own
    /// registry, then drop the `fauna.state.backup` row (the atomic decision
    /// point). No *destination*-side call — the coordinator reconciles the
    /// offsite deregistration on its next pass (backups.md § Remove), and the
    /// destination-side nest-writer grant is not revoked (a distinct
    /// trust-facet action). Returns the new list.
    #[wasm_bindgen(js_name = backupDestinationRemove)]
    pub fn backup_destination_remove(&self, secret_hex: String, id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let _ = secret_hex;
            let source_nest = bound_nest_id(&client).await?.0;
            let destinations = deregister_backup_destination(
                client,
                &*crate::account_runtime::backup_seam(),
                source_nest,
                &id,
            )
            .await
            .map_err(err_to_js)?;
            to_js(&fauna_core::data::distinct_destinations(&destinations))
        })
    }

    // ── muted keywords (content-moderation-and-ranking.md § Q3) ──
    //
    // Read / set the tier-1 muted-keywords word-list on the `muted-words`
    // Settings sub-page — the wasm twin of the FFI `load_muted_words`/`save_muted_words`.
    // Each face goes through the shared `preference_surfaces` the native apps
    // call, over this tab's account store
    // (`crate::account_runtime::handle_source()`): a call made before the
    // runtime is up waits for it, and fails if none comes — as in a second tab
    // of the same account, which hosts no runtime (`config-dissolution.md`
    // § The `__config` dissolution schedule → *The closure order*, steps (2)
    // and (5)). Each set returns the stored list so the
    // SPA re-renders exactly what was saved. The conversation apply matches with
    // the free `matchesMutedKeywords` (lib.rs) over the same shared decision.

    /// The owner's muted-keywords page record — `{ keywords: [{ keyword,
    /// weight }], loaded }`, the rows plus the bit that gates `muted-word-empty`
    /// (`docs/goal/ui/README.md` § *List pages: loading is not empty*). A caller
    /// that only wants the list (the conversation collapse) reads `.keywords`;
    /// an empty `keywords` here means "no terms", never "not read yet".
    #[wasm_bindgen(js_name = loadMutedWords)]
    pub fn load_muted_words(&self) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let snapshot = preference_surfaces::load_muted_words(&store)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&snapshot)
        })
    }

    /// Replace the owner's muted-keywords list and persist. `keywords` is an
    /// array of `{ keyword, weight }`, normalized (trim, drop blanks,
    /// case-insensitive dedupe keeping the first-seen entry, weights clamped) by
    /// the shared seam; the stored page record is returned.
    ///
    /// ⚠ Whole-list intents only. The page's add/remove buttons go through
    /// `addMutedWord` / `removeMutedWord` below — sending the page's
    /// list wholesale clobbers a term another device stored since the page
    /// loaded.
    #[wasm_bindgen(js_name = saveMutedWords)]
    pub fn save_muted_words(&self, keywords: JsValue) -> js_sys::Promise {
        future_to_promise(async move {
            let keywords: Vec<fauna_core::data::MutedKeyword> = from_js(keywords)?;
            let store = crate::account_runtime::handle_source();
            let snapshot = preference_surfaces::save_muted_words(&store, keywords)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&snapshot)
        })
    }

    /// Add one term to the owner's muted-keywords list — the production shape
    /// of the page's add gesture (`fauna_client_config::add_muted_word`, apps
    /// row 214). A **delta**: the seam re-reads the stored list inside its own
    /// CAS update, so a term another device stored since this page loaded
    /// survives this click. Re-adding an existing term is a no-op; the stored
    /// normalization is returned.
    #[wasm_bindgen(js_name = addMutedWord)]
    pub fn add_muted_word(&self, word: String) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let snapshot = preference_surfaces::add_muted_word(&store, &word)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&snapshot)
        })
    }

    /// Remove one term — `addMutedWord`'s inverse: exact stored spelling,
    /// and removing a term already gone is a success no-op (another device
    /// deleting it first is convergence, not an error).
    #[wasm_bindgen(js_name = removeMutedWord)]
    pub fn remove_muted_word(&self, word: String) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let snapshot = preference_surfaces::remove_muted_word(&store, &word)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&snapshot)
        })
    }

    /// Set one term's level — the muted-words page's level picker
    /// (`preference_surfaces::set_muted_word_level`). `level` is `"hide"` |
    /// `"show-less"` (`fauna_core::scoring::MutedKeywordLevel`; the weights
    /// behind the two live there, never in the SPA). A delta like
    /// `addMutedWord`: exact stored spelling, and a term the list does not
    /// hold is a success no-op; the stored page record is returned, each row's
    /// level readable through the free `mutedKeywordLevel` (lib.rs).
    #[wasm_bindgen(js_name = setMutedWordLevel)]
    pub fn set_muted_word_level(&self, word: String, level: JsValue) -> js_sys::Promise {
        future_to_promise(async move {
            let level: fauna_core::scoring::MutedKeywordLevel = from_js(level)?;
            let store = crate::account_runtime::handle_source();
            let snapshot = preference_surfaces::set_muted_word_level(&store, &word, level)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&snapshot)
        })
    }

    // ── the reporter-side hide (`moderation.md` § Corollary — block also
    // hides): the same sealed record as the muted words, so the same store
    // and the same deltas; the list feeds `contentRenderForItem`. Twins of the UniFFI
    // `reported_content_{list,hide,unhide}`.

    /// The ids the owner hid by reporting them.
    #[wasm_bindgen(js_name = loadHiddenContent)]
    pub fn load_hidden_content(&self) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let ids = preference_surfaces::load_hidden_content(&store)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&ids)
        })
    }

    /// Hide a reported subject for the owner; resolves to the stored list.
    #[wasm_bindgen(js_name = hideReported)]
    pub fn hide_reported(&self, id: String) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let ids = preference_surfaces::hide_reported(&store, &id)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&ids)
        })
    }

    /// Show a hidden subject again; resolves to the stored list.
    #[wasm_bindgen(js_name = unhideReported)]
    pub fn unhide_reported(&self, id: String) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let ids = preference_surfaces::unhide_reported(&store, &id)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&ids)
        })
    }

    // ── trained topic factors (topic-factors.md § Authoring surface & picker) ──
    //
    // The Trained-topics facet's four gestures. Each is a thin wrapper over the
    // SHARED `fauna_client_personalization::TrainedTopics` lifecycle — the
    // registry↔model-plane sequencing (advisory example-count read, create cap,
    // and the delete's registry-removal-then-`model.delete` pairing) lives there
    // once, so this wasm surface and the FFI twin the natives get are the same
    // four calls over the same logic (priority #2).
    //
    // Ids cross the boundary as **hex** rather than byte arrays: it is what the
    // SPA renders into the row's `data-factor` attribute (the e2e's only way to
    // learn a sealed, freshly-minted key), and it round-trips through a JS
    // string without a Uint8Array copy on every call.

    /// The owner's trained topics — one row per registry entry, each carrying
    /// the advisory example count off the model plane. `{ id (hex), name,
    /// factor_key ("topic:<hex>" | null), example_count }`.
    #[wasm_bindgen(js_name = listTrainedTopics)]
    pub fn list_trained_topics(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let t = TrainedTopics::new(client);
            let store = crate::account_runtime::handle_source();
            to_js(&rows_to_js(
                preference_surfaces::list_trained_topics(&store, &t)
                    .await
                    .map_err(topics_err_to_js)?,
            ))
        })
    }

    /// Mint a trained topic. Rejects a blank name, and the registry cap
    /// (`TRAINED_FACTORS_MAX`) — the client-side twin of the nest's create-cap.
    /// Returns the fresh row list.
    #[wasm_bindgen(js_name = createTrainedTopic)]
    pub fn create_trained_topic(&self, name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let t = TrainedTopics::new(client);
            let store = crate::account_runtime::handle_source();
            to_js(&rows_to_js(
                preference_surfaces::create_trained_topic(&store, &t, &name)
                    .await
                    .map_err(topics_err_to_js)?,
            ))
        })
    }

    /// Rename a trained topic in place. The id — and so the derived composition
    /// key — is untouched, so every feed composing this factor keeps working.
    #[wasm_bindgen(js_name = renameTrainedTopic)]
    pub fn rename_trained_topic(&self, id_hex: String, name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let t = TrainedTopics::new(client);
            let id = decode_factor_id(&id_hex)?;
            let store = crate::account_runtime::handle_source();
            to_js(&rows_to_js(
                preference_surfaces::rename_trained_topic(&store, &t, &id, &name)
                    .await
                    .map_err(topics_err_to_js)?,
            ))
        })
    }

    /// Delete a trained topic: the registry entry AND its paired nest-side model
    /// row. Compositions still naming the key stay valid (the zero-term seam
    /// makes an orphan key inert).
    #[wasm_bindgen(js_name = deleteTrainedTopic)]
    pub fn delete_trained_topic(&self, id_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let t = TrainedTopics::new(client);
            let id = decode_factor_id(&id_hex)?;
            let store = crate::account_runtime::handle_source();
            to_js(&rows_to_js(
                preference_surfaces::delete_trained_topic(&store, &t, &id)
                    .await
                    .map_err(topics_err_to_js)?,
            ))
        })
    }

    /// Flip a trained topic's Layer-A opt-in (`learn_from_engagement` — the
    /// row's "Learn from my activity" toggle). Registry-only: the model row is
    /// untouched, so turning it off stops future weak training without
    /// rewriting what engagement already taught. Unknown id is a no-op; the
    /// returned rows show the current truth.
    #[wasm_bindgen(js_name = setTrainedTopicEngagement)]
    pub fn set_trained_topic_engagement(&self, id_hex: String, on: bool) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let t = TrainedTopics::new(client);
            let id = decode_factor_id(&id_hex)?;
            let store = crate::account_runtime::handle_source();
            to_js(&rows_to_js(
                preference_surfaces::set_trained_topic_engagement(&store, &t, &id, on)
                    .await
                    .map_err(topics_err_to_js)?,
            ))
        })
    }

    /// Publish a trained factor's kept exemplars as a tier-3 List labeler
    /// (`topic-factors.md` § Publishing a trained factor; the frame's D8).
    ///
    /// A wrapper over the SHARED
    /// `fauna_client_personalization::publish::publish_trained_factor_list`,
    /// which owns the whole lifecycle — deriving the per-factor keypair,
    /// resolving the next version off the catalog, building + signing the
    /// artifact, and the `fauna.labelers.publish` call. Nothing is re-coded on
    /// this side of the boundary.
    ///
    /// `entries` is the **pruned** set the review sheet hands back
    /// (`[{post_id, score}]`, the ids off `scoreCorpusForFactor`), in whatever
    /// order it rendered — the shared builder owns the sort/dedup/validate into
    /// the canonical form the nest gate demands. `name` is the publisher-chosen
    /// **public** display name; the sealed registry name stays private, so the
    /// SPA must never default this to it. The signed `updated_at` is stamped by
    /// the shared lifecycle (the SPA supplies no clock, or its unit). Resolves
    /// `{labeler_id, version, entry_count}`.
    #[wasm_bindgen(js_name = publishTrainedFactorList)]
    pub fn publish_trained_factor_list(
        &self,
        secret_hex: String,
        id_hex: String,
        name: String,
        entries: JsValue,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let secret = hex_array_32(&secret_hex)?;
            let id = decode_factor_id(&id_hex)?;
            let id16: [u8; 16] = id
                .as_slice()
                .try_into()
                .map_err(|_| JsValue::from_str("a trained-topic id is 16 bytes"))?;
            let entries: Vec<JsPublishEntry> = from_js(entries)?;
            let published = fauna_client_personalization::publish::publish_trained_factor_list(
                client,
                &secret,
                &id16,
                &name,
                entries.into_iter().map(|e| (e.post_id, e.score)).collect(),
            )
            .await
            .map_err(publish_err_to_js)?;
            to_js(&JsPublishedList {
                labeler_id: hex::encode(published.labeler_id),
                version: published.version,
                entry_count: published.entry_count as u32,
            })
        })
    }

    /// Publish a trained factor's **scrubbed vocabulary** as a tier-3
    /// `text-model` labeler (`topic-factors.md` § Publishing a trained factor,
    /// v2).
    ///
    /// A wrapper over the SHARED
    /// `fauna_client_personalization::publish::publish_trained_factor_model`,
    /// which owns the whole lifecycle — deriving the per-factor keypair,
    /// resolving the next version off the catalog, building + signing the
    /// artifact, and the `fauna.labelers.publish` call. Nothing is re-coded on
    /// this side of the boundary, and the publishing identity is the List's
    /// unchanged, which is what makes upgrading a factor from List to Model an
    /// ordinary version bump rather than a new, orphaned artifact.
    ///
    /// `ngrams` is the **pruned** set the review sheet hands back
    /// (`[{ngram, more, less}]`, the rows off `scrubCorpusForFactor`), in
    /// whatever order it rendered — the shared builder owns the
    /// sort/dedup/validate into the canonical form the nest gate demands.
    /// `moreDocs`/`lessDocs` are that scrub's **corpus** counters and are passed
    /// through **unshrunk**: they say how many public examples the vocabulary
    /// was built from, which stays true however much of it the user withheld.
    /// An SPA that "corrects" them down to match the pruned rows would silently
    /// make the published model look more confident than it is.
    ///
    /// `name` is the publisher-chosen **public** display name; the sealed
    /// registry name stays private, so the SPA must never default this to it.
    /// The signed `updated_at` is stamped by the shared lifecycle (the SPA
    /// supplies no clock, or its unit). Resolves
    /// `{labeler_id, version, ngram_count, document_count}`.
    #[wasm_bindgen(js_name = publishTrainedFactorModel)]
    pub fn publish_trained_factor_model(
        &self,
        secret_hex: String,
        id_hex: String,
        name: String,
        more_docs: u32,
        less_docs: u32,
        ngrams: JsValue,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let secret = hex_array_32(&secret_hex)?;
            let id = decode_factor_id(&id_hex)?;
            let id16: [u8; 16] = id
                .as_slice()
                .try_into()
                .map_err(|_| JsValue::from_str("a trained-topic id is 16 bytes"))?;
            let ngrams: Vec<JsPublishNgram> = from_js(ngrams)?;
            let published = fauna_client_personalization::publish::publish_trained_factor_model(
                client,
                &secret,
                &id16,
                &name,
                more_docs,
                less_docs,
                ngrams
                    .into_iter()
                    .map(|n| (n.ngram, n.more, n.less))
                    .collect(),
            )
            .await
            .map_err(publish_model_err_to_js)?;
            to_js(&JsPublishedModel {
                labeler_id: hex::encode(published.labeler_id),
                version: published.version,
                ngram_count: published.ngram_count as u32,
                document_count: published.document_count,
            })
        })
    }

    // ── sync preferences (file-sync.md § Conflicts, policy) ──
    //
    // Read / set the user-global default conflict policy for new folders on
    // the Folders page's "Sync defaults" section — the wasm twin of the FFI
    // `default_conflict_policy_{get,set}`. Sealed under the owner's BackupKey
    // in `fauna.state.sync-prefs` (nest-opaque); whole-record latest-wins on
    // `updated_at` so clearing the preference propagates across the fleet.
    //
    // Delegates to the shared `preference_surfaces::{load,save}_sync_prefs` the
    // native apps call, over this tab's account store (`config-dissolution.md`
    // § The `__config` dissolution schedule → *The closure order*, steps (2)
    // and (5)).

    /// The default conflict policy for new sets (`"auto"` |
    /// `"latest_wins_always"`), or `null` = no preference (nest column default).
    #[wasm_bindgen(js_name = loadSyncPrefs)]
    pub fn load_sync_prefs(&self) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let policy = preference_surfaces::load_sync_prefs(&store)
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&policy)
        })
    }

    /// Set (or clear, with `null`) the default conflict policy for new sets and
    /// persist. Normalized to a canonical wire string (unknown → `"auto"`); the
    /// stored value is returned. Existing sets are untouched — each set's row
    /// (the per-set `folder-conflict-policy-select`) stays authoritative.
    #[wasm_bindgen(js_name = saveSyncPrefs)]
    pub fn save_sync_prefs(&self, policy: Option<String>) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let stored = preference_surfaces::save_sync_prefs(&store, policy.as_deref())
                .await
                .map_err(preference_surfaces::plane_failure)
                .map_err(err_to_js)?;
            to_js(&stored)
        })
    }

    /// **The deployment-seed custody leg's post-auth edge** — the web twin of
    /// the native hosts' `run_custody_leg` (`box-recovery.md` § The plane-era
    /// recovery floor, *(c) The writes*). The SPA's universal post-auth hook
    /// fires it on every connect. It leaves this edge's half —
    /// `onWarning`, the SPA's warning sink for this account — for the
    /// store-ready edge (`startAccountRuntime`), and runs the leg now when
    /// this account's store is already up: the leg runs at whichever of the
    /// two edges lands second, and at every later post-auth edge
    /// (`crate::deployment_seed_custody`).
    ///
    /// The leg is the ONLY capture: if the plane already holds a live entry
    /// for the bound nest it is done with no round trip; otherwise it asks
    /// whether this identity is an admin (fail-closed), fetches the seed over
    /// `fauna.admin.deployment_seed.get`, refuses one that does not derive to
    /// the bound id, and merges the entry — the seed never crosses into JS.
    ///
    /// `onWarning` is a JS `(warning: 'mismatch' | 'failed') => void`, called
    /// when a run (on either edge) ends with custody unconfirmed for an admin
    /// of the bound nest. Resolves a token for the console ring:
    /// `"already_custodied"`, `"captured"`, `"not_admin"`,
    /// `"handoff_unavailable"`, `"nest_holds_no_seed"`, `"refused_mismatch"`,
    /// `"store_refused"`, `"bound_unresolved"`, or `"store_not_ready"` (the
    /// store-ready edge will run it). Rejects only on a malformed secret.
    #[wasm_bindgen(js_name = selfHealDeploymentSeedCustody)]
    pub fn self_heal_deployment_seed_custody(
        &self,
        secret_hex: String,
        on_warning: js_sys::Function,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let actor_id_hex = keypair_from_secret_hex(&secret_hex)?.actor_id_hex();
            crate::deployment_seed_custody::note_post_auth(
                actor_id_hex.clone(),
                on_warning.clone(),
            );
            let Some(handle) = crate::account_runtime::handle_for(&actor_id_hex) else {
                return Ok(JsValue::from_str("store_not_ready"));
            };
            let run = crate::deployment_seed_custody::run_leg(&client, &handle).await;
            if let Some(warning) = run.warning {
                crate::deployment_seed_custody::emit_warning(&on_warning, warning);
            }
            Ok(JsValue::from_str(run.token))
        })
    }

    /// Run the feeders that have no page of their own, at the same universal
    /// post-auth point as [`Self::self_heal_deployment_seed_custody`] /
    /// `refreshMailEpochSchedule` (`critical-alerts.md` § Mechanism → *Who
    /// runs the detector*). tui's `critical_alerts::spawn_session_start_sweep`
    /// (`session::establish`) is the reference this mirrors; `secret_hex`
    /// derives the actor id the sweep checks conditions against, and feeder
    /// #1 reads the rotation keyring from that account's runtime in this tab
    /// (the account plane's `fauna.state.atproto-identity`, through the
    /// shared `RuntimeAtprotoIdentity` — a loop that outlives an account
    /// switch reads nothing of the next account's).
    ///
    /// Runs [`fauna_client_alert_sweep::run_alert_sweep_loop`] — sweeps
    /// immediately, then every `RE_SWEEP_INTERVAL_SECS` for as long as the
    /// identity lives, logging each sweep itself. **The returned promise does
    /// not resolve under normal operation** (only on sign-out, when
    /// `CriticalAlerts::clear_all` bumps the teardown epoch the loop watches)
    /// — the caller must stay fire-and-forget, exactly as it is today
    /// (`+layout.svelte`'s `void runCriticalAlertSweep(...).catch(...)`, never
    /// awaited). Only rejects on a malformed `secret_hex`.
    ///
    /// **One loop per identity.** A call for an identity whose loop is already
    /// running (no teardown since it started) runs ONE pass and resolves — the
    /// SPA fires this on every session establishment, a same-actor re-establish
    /// included, and a second loop there would only stack a concurrent sweeper
    /// (`crate::critical_alerts::loop_is_live_for`).
    #[wasm_bindgen(js_name = runCriticalAlertSweep)]
    pub fn run_critical_alert_sweep(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let keypair = keypair_from_secret_hex(&secret_hex)?;
            let actor_id = keypair.actor_id();
            let actor_id_hex = keypair.actor_id_hex();
            let custody: std::sync::Arc<
                dyn fauna_account_seams::atproto_identity::AtprotoIdentityStore,
            > = std::sync::Arc::new(
                fauna_account_seams::atproto_identity::RuntimeAtprotoIdentity::new(move || {
                    crate::account_runtime::handle_for(&actor_id_hex)
                }),
            );
            let alerts = crate::critical_alerts::alerts_registry();
            if crate::critical_alerts::loop_is_live_for(&actor_id) {
                fauna_client_alert_sweep::run_session_start_sweep(
                    client, &custody, &alerts, &actor_id,
                )
                .await;
                return Ok(JsValue::UNDEFINED);
            }
            crate::critical_alerts::mark_loop_live(&actor_id);
            // The e2e bundle runs the SAME loop with its wait raceable by
            // `alertSweepWakeForTest` (convention 14); a production bundle has
            // no wake to race and runs the clock alone.
            #[cfg(feature = "test-helpers")]
            {
                let wake = crate::critical_alerts::mint_sweep_wake();
                fauna_client_alert_sweep::run_alert_sweep_loop_wakeable(
                    client,
                    &custody,
                    &alerts,
                    &actor_id,
                    move || {
                        let notify = std::rc::Rc::clone(&wake);
                        async move { notify.notified().await }
                    },
                )
                .await;
            }
            #[cfg(not(feature = "test-helpers"))]
            fauna_client_alert_sweep::run_alert_sweep_loop(client, &custody, &alerts, &actor_id)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The S8 **seal-backfill sweep** — D1 over the folder plane, then D3 over
    /// each owned set's snapshot tags — at the same universal post-auth point as
    /// [`Self::run_critical_alert_sweep`] (`path-sealing.md` § Implementation
    /// status today; the other six apps wire the same sweep at their own
    /// session-start hook).
    ///
    /// **This is web's D1/D3 leg, and it was the last one unwired** — deliberately
    /// so until `NestFolderKeyResolver` became transport-generic, because the
    /// only custody web could build before that was owner-only, and an
    /// owner-only custody seals a *bound* set's fields under a root no roster
    /// member can open (the failure mode that looks like a
    /// graceful degrade). It now builds the same resolver-backed custody every
    /// native app does, through the one shared constructor
    /// (`seal_backfill::resolver_backed_custody`), so a bound-but-unresolvable
    /// set fails **closed** here exactly as it does on native.
    ///
    /// Best-effort by contract: never rejects except on a malformed
    /// `secret_hex`, resolves as soon as the pass is done (unlike the
    /// alert-sweep loop above, this one is a single pass — not a loop — so the
    /// caller may `void` it or await it). The counts go to the shared log ring
    /// (the Logs view's `fauna_web::seal_backfill` target), never a set name
    /// (S7).
    #[wasm_bindgen(js_name = runSealBackfillSweep)]
    pub fn run_seal_backfill_sweep(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let keypair = keypair_from_secret_hex(&secret_hex)?;
            let report = fauna_client_folders::seal_backfill::run_sweep(
                client,
                &keypair,
                crate::account_runtime::folder_key_store(keypair.actor_id_hex()),
            )
            .await;
            if report.is_noteworthy() {
                let fields = report.fields.unwrap_or_default();
                fauna_log::log_message(
                    fauna_log::LogLevel::Info,
                    "fauna_web::seal_backfill",
                    &format!(
                        "seal backfill: names={} selective_sync={} retention={} \
                         bound_skipped={} identity_mismatch={} update_failures={} \
                         tags_stamped={} tags_unsealable={} tags_stamp_failures={} \
                         sets_swept={} member_sets_skipped={} set_failures={} \
                         fields_error={} roster_error={}",
                        fields.names,
                        fields.selective_sync,
                        fields.retention,
                        fields.bound_skipped,
                        fields.identity_mismatch,
                        fields.update_failures,
                        report.tags.stamped,
                        report.tags.unsealable,
                        report.tags.stamp_failures,
                        report.sets_swept,
                        report.member_sets_skipped,
                        report.set_failures,
                        report.fields_error.as_deref().unwrap_or("-"),
                        report.roster_error.as_deref().unwrap_or("-"),
                    ),
                );
            }
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Per-destination backup status for the Backups page status rows
    /// (`backups.md` § Per-destination status read) — the **wasm twin** of the
    /// native FFI `backup_destination_status`, both now thin wrappers over the
    /// one shared `fauna_client_config::read_backup_status`.
    ///
    /// **Repointed 2026-07-24 (slice-4 leg (d)).** This used to be a
    /// *degenerate-but-live* carve-out: web has no `rusqlite` `SyncDb` and no
    /// wasm upload coordinator, so it hard-coded `last_upload_time: None`
    /// ("never") and summed `fauna.segments.list` for a whole-source
    /// `backlog_count`. It now reads the **nest's** `fauna.backup.status`
    /// projection like every other app — which is strictly better on web than
    /// anywhere else, because the nest's in-process coordinator is the only
    /// entity that ever uploads for a web-only user, so `last_upload_time` is a
    /// real timestamp here for the first time. The degenerate carve-out and the
    /// `BACKUP_STATUS_KINDS` mirror-list it needed are both deleted.
    ///
    /// Returns rows keyed by `destination_id` for the SPA to map onto the
    /// per-row IDs; the JS shape is unchanged by the repoint. Zero destinations
    /// ⇒ an empty array. The shared read also heals a pre-enrollment owner in
    /// passing — see `read_backup_status`.
    #[wasm_bindgen(js_name = backupDestinationStatus)]
    pub fn backup_destination_status(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let secret = hex_array_32(&secret_hex)?;
            let source_nest = bound_nest_id(&client).await?.0;
            let reply = read_backup_status(
                client,
                &*crate::account_runtime::backup_seam(),
                secret,
                source_nest,
            )
            .await
            .map_err(err_to_js)?;
            let statuses: Vec<WasmBackupDestinationStatus> = reply
                .destinations
                .into_iter()
                .map(|d| WasmBackupDestinationStatus {
                    destination_id: d.destination_id,
                    last_upload_time: d.last_upload_time,
                    backlog_count: d.backlog_count,
                    held_bytes: d.held_bytes,
                    cap_state: d.cap_state,
                    audit_state: d.audit_state,
                    last_audit_passed_at: d.last_audit_passed_at,
                })
                .collect();
            to_js(&statuses)
        })
    }

    /// Run one client-side backup **audit** pass and hand back the full
    /// per-destination picture (`backups.md` § Audit-alert surface).
    ///
    /// This implements **no** audit logic. `run_audit_pass` loads this device's
    /// state, audits everything due, merges the outcomes over what was already
    /// known, persists, and returns one record per configured destination —
    /// merge, debounce and verdict all shared. Web supplies exactly three seams,
    /// each the browser twin of the native one it mirrors:
    ///
    /// * the **connector** — `fauna_client_pair::wasm_backup_destination_connector`
    ///   over `make_backup_connect`, so the client opens its **own** authenticated
    ///   session to each *destination*, never a read through the source nest;
    /// * the **inclusion source** — `fauna_client_pair::wasm_backup_inclusion_source`
    ///   over `WasmPublicChunkFetcher`, so sampled records are fetched from the
    ///   party being audited and opened under the owner's derived `NestBackupKey`;
    /// * the **store** — `crate::backup_audit::LocalStorageAuditStateStore`,
    ///   actor-scoped.
    ///
    /// The destination set audited is the client's **own pinned** list — this
    /// box's `fauna.state.backup` row in the account store
    /// ([`crate::account_runtime::backup_seam`]), keyed by the id this
    /// connection is bound to — not a nest-side list: a destination the
    /// source nest has "forgotten" must still be audited, and must still alert.
    ///
    /// `now` comes from `fauna_client_backup::audit_clock::now_secs` — real time
    /// plus the e2e offset, which is zero in every real run and compiled out of
    /// release artifacts entirely.
    #[wasm_bindgen(js_name = backupAuditRunPass)]
    pub fn backup_audit_run_pass(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let secret = hex_array_32(&secret_hex)?;
            let keypair = keypair_from_secret_hex(&secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            // The owner's own nest — this socket's — dialled by the pass only to
            // settle a ledger regression a destination served (the generation
            // pin, `fauna_client_backup::audit::SourceLedgerVouch`).
            let source_nest_url = client.nest_url();
            let source_nest = bound_nest_id(&client).await?.0;
            let state = fauna_client_config::load_backup_state(
                &*crate::account_runtime::backup_seam(),
                source_nest,
            )
            .await
            .map_err(err_to_js)?;

            let connector = fauna_client_pair::wasm_backup_destination_connector(
                crate::pairing::make_backup_connect(secret_hex.clone()),
            );
            let inclusion = fauna_client_pair::wasm_backup_inclusion_source(secret);
            let store = crate::backup_audit::LocalStorageAuditStateStore::for_actor(actor_hex);

            let now = fauna_client_backup::audit_clock::now_secs();
            let pass = fauna_client_backup::audit::run_audit_pass(
                connector.as_ref(),
                inclusion.as_ref(),
                &store,
                &state.backup.destinations,
                &source_nest_url,
                &source_nest,
                // A browser hosts no custodian store — nothing to fold.
                None,
                now,
            )
            .await;
            for degradation in &pass.degradations {
                tracing::warn!("backup audit: {degradation}");
            }

            let rows: Vec<WasmDestinationAuditRow> =
                pass.records.iter().map(|r| audit_row(r, now)).collect();
            to_js(&rows)
        })
    }

    /// The custodied deployment-seed box list for the total-box-loss recovery UI
    /// (`box-recovery.md` § Recovery UI (step 4)) — the **wasm twin** of the
    /// native `deployment_seeds()` getter. Reads the resolver of
    /// `box-recovery.md` § The plane-era recovery floor, *(b) The reads*: this
    /// device's own account store **joined with** a cold read from the nest
    /// this client reaches (a throwaway replica that writes nothing), through
    /// the kind's own lattice — never either-or, because a surviving device's
    /// stored nest may be the dead box. A source that fails leaves the other's
    /// answer; rejects only when both failed. One row per recoverable box
    /// (superseded boxes excluded), each carrying only the box's
    /// `nest_actor_id` (hex) and domain — the raw seed stays WASM-internal.
    #[wasm_bindgen(js_name = deploymentSeeds)]
    pub fn deployment_seeds(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let secret = hex_array_32(&secret_hex)?;
            let seeds = resolve_recovery_seeds(&secret, Some(&client)).await?;
            to_js(&recovery_box_rows(&seeds))
        })
    }

    /// The `recover-selfhosted-command` for the box-recovery step-4 self-hosted
    /// install page (`box-recovery.md` § Recovery UI (step 4)) — the **wasm
    /// twin** of the native `recover_selfhosted_command()` getter. Resolves the
    /// selected box's custodied seed **in Rust** from the same joined read as
    /// [`Self::deployment_seeds`] (this device's store + the reachable nest's
    /// cold read), then renders via the single shared
    /// `selfhosted_recovery_command_in` so web + native emit a byte-identical
    /// `FAUNA_DEPLOYMENT_SEED=<64-hex>` command (priorities #1/#3). A
    /// superseded box yields no command.
    ///
    /// Unlike the box list (where the raw seed never crosses into JS), the seed **is** surfaced here by design: it is exactly
    /// the installer input the admin pastes into the rebuilt box's deploy `.env`
    /// so it re-presents the **same** `nest_actor_id` and every TOFU-pinned
    /// client reconnects without a trust break (`box-recovery.md` § Trust &
    /// audience).
    ///
    /// Rejects if `secret_hex` is malformed, if every custody source failed, or
    /// if no source custodies a seed for that box.
    #[wasm_bindgen(js_name = recoverSelfhostedCommand)]
    pub fn recover_selfhosted_command(
        &self,
        secret_hex: String,
        nest_actor_id_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let secret = hex_array_32(&secret_hex)?;
            let seeds = resolve_recovery_seeds(&secret, Some(&client)).await?;
            selfhosted_command_js(&seeds, &nest_actor_id_hex)
        })
    }

    // ── fauna.spam.* ────────────────────────────────────────────────
    //
    // The Settings → Privacy spam-classifier preferences seam over the shared
    // `fauna_client_spam::SpamClient`, retiring `GET|PUT /api/v1/spam/preferences`
    // (`apps/fauna-web/src/lib/api.ts`). The wire is per-mille `[0,1000]` (dag-cbor
    // forbids floats); the web UI works in probability `[0.0,1.0]`, so the helpers
    // convert at the boundary, keeping the page shape byte-identical to the retired
    // twin. Mirrors the linux native binding.

    /// `fauna.spam.get_preferences` → `{ spam_threshold, phishing_threshold:
    /// 0.0–1.0 }` (the twin's `getSpamPreferences` shape).
    #[wasm_bindgen(js_name = spamGetPreferences)]
    pub fn spam_get_preferences(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let prefs = SpamClient::new(client)
                .get_preferences(SpamGetPreferencesRequest {
                    extra: Default::default(),
                })
                .await
                .map_err(err_to_js)?;
            to_js(&spam_prefs_to_web_json(&prefs))
        })
    }

    /// `fauna.spam.set_preferences` — partial update; only the provided fields
    /// change (mirrors the twin's per-field PUT). Echoes the resulting prefs in
    /// the same shape as `spamGetPreferences`.
    #[wasm_bindgen(js_name = spamSetPreferences)]
    pub fn spam_set_preferences(
        &self,
        spam_threshold: Option<f64>,
        phishing_threshold: Option<f64>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let prefs = SpamClient::new(client)
                .set_preferences(SpamSetPreferencesRequest {
                    spam_threshold: spam_threshold.map(probability_to_per_mille),
                    phishing_threshold: phishing_threshold.map(probability_to_per_mille),
                    extra: Default::default(),
                })
                .await
                .map_err(err_to_js)?;
            to_js(&spam_prefs_to_web_json(&prefs))
        })
    }

    // ── fauna.search.* ──────────────────────────────────────────────
    //
    // The search page's full-text-search seam over the shared
    // `fauna_client_search::SearchClient`, retiring `GET /api/v1/search`
    // (`apps/fauna-web/src/lib/api.ts`). The wire `rank` is i64 micro-units
    // (negated BM25 × 1e6); `search_reply_to_web_json` restores the float the
    // twin returned, keeping the page shape byte-identical.

    /// `fauna.search.query` → `{ results: [{ content_type, content_id,
    /// created_at, rank, snippet }] }` (the twin's `searchContent` shape).
    /// `content_type`/`limit` are optional; `offset`/`before`/`after` ride the
    /// handler defaults (the web page paginates by growing `limit`, matching
    /// the retired HTTP twin).
    #[wasm_bindgen(js_name = searchQuery)]
    pub fn search_query(
        &self,
        query: String,
        content_type: Option<String>,
        // `f64` (→ JS `number`), converted to the `i64` wire type here — matches
        // the pagination convention of `adminUsersList`/`notificationsList`.
        limit: Option<f64>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = SearchClient::new(client)
                .query(SearchQueryRequest {
                    query,
                    content_type,
                    before: None,
                    after: None,
                    limit: limit.map(|l| l as i64),
                    offset: None,
                    extra: Default::default(),
                })
                .await
                .map_err(err_to_js)?;
            to_js(&search_reply_to_web_json(&reply))
        })
    }

    // ── fauna.moderation.* ──────────────────────────────────────────
    //
    // The user-facing moderation kinds over the shared
    // `fauna_client_moderation::ModerationClient`.

    /// `fauna.moderation.train` — report a spam/ham correction for `content_id`
    /// (hex post id): the nest half only (read gate + report capture; it trains
    /// nothing). A moderation-queue correction runs the whole shared flow via
    /// `WasmMailSettingsMachine.trainModerationCorrection` instead, which also
    /// trains the sealed model client-side. `verdict` is `"spam"` or `"ham"`.
    #[wasm_bindgen(js_name = moderationTrain)]
    pub fn moderation_train(&self, content_id: String, verdict: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ModerationClient::new(client)
                .train(content_id, verdict)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.moderation.legal_takedown` — the Admin-only legal-compulsion
    /// takedown / overturn (`moderation.md` § Legal takedown → *Invocation
    /// surface*). `conversation` selects the MLS relay-withhold kind
    /// (`content_type = "conversation"`), else `"post"`; `restore` overturns
    /// (the reference becomes the optional note). Gate the dispatch through
    /// `takedownFormView` so a citation-less takedown is refused before it
    /// bounces off the nest's `invalid_params`. Resolves to the reply
    /// (`{ status: "taken_down" | "restored", content_id }`); errors reject for
    /// `takedownVerdict` to word.
    #[wasm_bindgen(js_name = moderationLegalTakedown)]
    pub fn moderation_legal_takedown(
        &self,
        content_id: String,
        conversation: bool,
        legal_reference: String,
        restore: bool,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        let content_type = if conversation { "conversation" } else { "post" };
        future_to_promise(async move {
            let reply = ModerationClient::new(client)
                .legal_takedown(content_id, content_type, legal_reference, restore)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── user-initiated reporting (`moderation.md` § User-initiated reporting)
    // — the wasm twins of the UniFFI `abuse_report_*` client methods; rows come
    // back worded by the shared `fauna_client_moderation::report`.

    /// `fauna.moderation.abuse_report.submit`, built from the sheet by the
    /// shared `report::report_request` — a sheet `reportSheetView` would not
    /// let send is rejected before anything leaves. Resolves to `{ report_id,
    /// routed_to, acknowledgement }`. `block_author` is recorded only: the SPA
    /// chains `fauna.knocks.block` and `hideReported` itself.
    #[wasm_bindgen(js_name = moderationAbuseReportSubmit)]
    pub fn moderation_abuse_report_submit(
        &self,
        target: JsValue,
        form: JsValue,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let target: fauna_client_moderation::report::ReportTarget = from_js(target)?;
            let form: fauna_client_moderation::report::ReportForm = from_js(form)?;
            let request = fauna_client_moderation::report::report_request(&target, &form)
                .ok_or_else(|| JsValue::from_str("the report cannot be sent yet"))?;
            let reply = ModerationClient::new(client)
                .abuse_report_submit(request)
                .await
                .map_err(err_to_js)?;
            to_js(&serde_json::json!({
                "report_id": reply.report_id,
                "acknowledgement":
                    fauna_client_moderation::report::report_acknowledgement(&reply.routed_to),
                "routed_to": reply.routed_to,
            }))
        })
    }

    /// `fauna.moderation.abuse_report.mine` — the ledger, newest first: each
    /// entry's `report_id`, `subject`, `created_at` beside its shared
    /// `ledger_row_view` wording (`reason`, `status`, `outcome`, `routed_to`,
    /// `can_withdraw`).
    #[wasm_bindgen(js_name = moderationAbuseReportMine)]
    pub fn moderation_abuse_report_mine(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ModerationClient::new(client)
                .abuse_report_mine()
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> = reply
                .reports
                .iter()
                .map(|entry| {
                    let view = fauna_client_moderation::report::ledger_row_view(entry);
                    serde_json::json!({
                        "report_id": entry.report_id,
                        "subject": entry.subject,
                        "created_at": entry.created_at,
                        "reason": view.reason,
                        "status": view.status,
                        "outcome": view.outcome,
                        "routed_to": view.routed_to,
                        "can_withdraw": view.can_withdraw,
                    })
                })
                .collect();
            to_js(&rows)
        })
    }

    /// `fauna.moderation.abuse_report.withdraw` — errors reject for
    /// `reportWithdrawVerdict` to word.
    #[wasm_bindgen(js_name = moderationAbuseReportWithdraw)]
    pub fn moderation_abuse_report_withdraw(&self, report_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            ModerationClient::new(client)
                .abuse_report_withdraw(report_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::NULL)
        })
    }

    /// `fauna.moderation.abuse_report.queue` — the open reports, oldest first:
    /// `report_id`, `subject`, `subject_actor`, `note`, `excerpt`, `created_at`
    /// beside the shared `reason` label and `origin` line (a local handle or "a
    /// user of <nest>" — never a forwarded reporter). Admin-class.
    #[wasm_bindgen(js_name = adminAbuseReportQueue)]
    pub fn admin_abuse_report_queue(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = AdminClient::new(client)
                .abuse_report_queue()
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> = reply
                .reports
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "report_id": entry.report_id,
                        "subject": entry.subject,
                        "subject_actor": entry.subject_actor,
                        "reason": fauna_client_moderation::report::reason_label(entry.reason),
                        "note": entry.note,
                        "excerpt": entry.excerpt,
                        "origin": fauna_client_moderation::report::queue_origin(entry),
                        "created_at": entry.created_at,
                    })
                })
                .collect();
            to_js(&rows)
        })
    }

    /// `fauna.moderation.abuse_report.resolve` — a record, not an action;
    /// `acted` false dismisses. Errors reject for `reportResolveVerdict`.
    #[wasm_bindgen(js_name = adminAbuseReportResolve)]
    pub fn admin_abuse_report_resolve(&self, report_id: String, acted: bool) -> js_sys::Promise {
        use fauna_protocol::moderation::AbuseReportOutcome;
        let client = self.inner.clone();
        let outcome = if acted {
            AbuseReportOutcome::Acted
        } else {
            AbuseReportOutcome::Dismissed
        };
        future_to_promise(async move {
            AdminClient::new(client)
                .abuse_report_resolve(report_id, outcome)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::NULL)
        })
    }

    // `moderationScanReport` was removed 2026-07-19 (the client-side
    // compliance-beacon producer was retired without replacement —
    // `moderation.md` § State & data shape owns the verdict); the kind itself
    // left the wire 2026-09-24 with the compat-remnant sweep.

    /// `fauna.moderation.actions` → `ModerationActionsReply { actions:
    /// Vec<ObligationAction> }` — read the connection actor's **own** moderation
    /// queue: the server-issued obligation-action records (why each piece of the
    /// caller's content was labeled / quarantined / rejected), one row per
    /// `{ id, content_type, content_id, category, confidence_per_mille (u16),
    /// action (u8), timestamp }`. Empty request — the connection scopes to its
    /// caller. This is the same queue the 5 native apps read; web unions these
    /// server rows (which carry the enforcement `action` + appeal path, and in
    /// encrypted mode are the mail-ingest/admin actions) with its local
    /// post-decrypt `flaggedItems` (the only social-content signal in encrypted
    /// mode) so web's moderation-queue is the superset — `moderation.md`
    /// § State & data shape.
    #[wasm_bindgen(js_name = moderationActions)]
    pub fn moderation_actions(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = ModerationClient::new(client)
                .actions()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    // ── fauna.push.* ─────────────────────────────────────────────────────────
    //
    // Push-subscription management over the shared `fauna_client_push::
    // PushClient`, retiring the three `/api/v1/push/*` HTTP routes
    // (`apps/fauna-web/src/lib/push.ts`). The browser `serviceWorker`/
    // `PushManager.subscribe` work stays in `push.ts`; only these three nest
    // hops migrate. All ride the authenticated connection (push subscription is
    // inherently post-login).

    /// `fauna.push.vapid_key` → `VapidKeyReply { public_key }` — the base64url
    /// VAPID public key for `pushManager.subscribe`'s `applicationServerKey`.
    #[wasm_bindgen(js_name = pushVapidKey)]
    pub fn push_vapid_key(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PushClient::new(client)
                .vapid_key()
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.push.subscribe` → `SubscribeReply { ok }` — register/update the
    /// connection actor's push subscription for one device (idempotent upsert).
    /// `req` is `{ device_id, endpoint, key_p256dh?, key_auth?, transport? }`.
    #[wasm_bindgen(js_name = pushSubscribe)]
    pub fn push_subscribe(&self, req: JsValue) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let req: SubscribeRequest = from_js(req)?;
            let reply = PushClient::new(client)
                .subscribe(req)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }

    /// `fauna.push.unsubscribe` → `UnsubscribeReply { ok }` — remove the
    /// connection actor's push subscription for `device_id`.
    #[wasm_bindgen(js_name = pushUnsubscribe)]
    pub fn push_unsubscribe(&self, device_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PushClient::new(client)
                .unsubscribe(device_id)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }
}

// The `zaps` registry feature's web half (`dynamic-features.md` § Charter
// members — a subset member of `payments`, so this block exists only in builds
// that also carry the money plane). Its own `impl` block rather than
// method-level `#[cfg]`s, for the same reason the payments block later in this
// file is one: a wasm export keyed on the FEATURE keeps the generated JS/.d.ts
// face a pure function of the feature set (§ The cargo feature spine, rule (c)).
//
// `nostrBunker*` and `nostrBadges` deliberately stay in the ungated block
// above — NIP-46 signing and NIP-58 badges are not registry members, and only
// the zap surfaces excise. That is why this is a carve, not a family gate.
#[cfg(feature = "zaps")]
#[wasm_bindgen]
impl WsRpcClient {
    // ── fauna.nostr.zap_signers.* ───────────────────────────────────
    //
    // The NIP-57 zap trust root (`docs/goal/behavior/monetization.md` § Zap
    // receipts — the trust model) over the shared
    // `fauna_client_nostr::NostrZapSignerClient<R>` (priority #2). A kind-9735
    // receipt is signed by the payee's LNURL/wallet server and is plain signed
    // JSON anyone may mint, so its own signature proves nothing — this roster
    // is what makes one believable. All three kinds are User-class and
    // caller-scoped server-side. Same plane split as `fauna.nostr.bunker.*`
    // above: deliberately not a row on the generic `fauna.bridges.*` surface.

    /// `fauna.nostr.zap_signers.list` → `ZapSignerEntry[]` — the caller's
    /// designated signers, newest first. **An empty array is the meaningful
    /// out-of-the-box default, not a failed load**: a payee who has designated
    /// nobody believes nobody, so every zap stays inert. Render that state as
    /// such rather than as an error.
    #[wasm_bindgen(js_name = nostrZapSignersList)]
    pub fn nostr_zap_signers_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let signers = NostrZapSignerClient::new(client)
                .list()
                .await
                .map_err(err_to_js)?;
            to_js(&signers)
        })
    }

    /// `fauna.nostr.zap_signers.add` → `ZapSignerEntry` — designate a signer
    /// (64-hex pubkey, normalized lowercase nest-side) with an optional
    /// provider label; idempotent, so re-adding refreshes the label. Render
    /// the *returned* row's `signer_pubkey` rather than the input: a key
    /// pasted in uppercase comes back lowercased, and only the stored form
    /// will ever match a real receipt.
    #[wasm_bindgen(js_name = nostrZapSignersAdd)]
    pub fn nostr_zap_signers_add(&self, signer_pubkey: String, label: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let entry = NostrZapSignerClient::new(client)
                .add(signer_pubkey, label)
                .await
                .map_err(err_to_js)?;
            to_js(&entry)
        })
    }

    /// `fauna.nostr.zap_signers.remove` → `bool` — stop trusting a signer,
    /// keyed by the pubkey itself (no roster round-trip first); `false` when
    /// the caller had not designated it. Takes effect at the next receipt —
    /// the gate runs at ingest, so this stops future belief and does not
    /// retract past acceptances.
    #[wasm_bindgen(js_name = nostrZapSignersRemove)]
    pub fn nostr_zap_signers_remove(&self, signer_pubkey: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let removed = NostrZapSignerClient::new(client)
                .remove(signer_pubkey)
                .await
                .map_err(err_to_js)?;
            to_js(&removed)
        })
    }

    /// `nostr.zaps.total` → `NostrZapTotalReply { total_msats, zap_count }` —
    /// aggregate zap receipts (NIP-57) for one event. Unknown ids are zeros.
    #[wasm_bindgen(js_name = nostrZapTotal)]
    pub fn nostr_zap_total(&self, event_id: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = NostrContentClient::new(client)
                .zap_total(event_id)
                .await
                .map_err(err_to_js)?;
            to_js(&reply)
        })
    }
}

// ── calendar-selection resolution (events.md § Where logic lives) ──────────────
//
// A standalone, non-async function — no RPC, no `&self` — mirroring the
// android UniFFI face (`libs/fauna-ffi/src/resolve.rs::resolve_calendar_selection`,
// the shape template) rather than `WsRpcClient`'s async methods above.

/// `fauna_client_caldav::resolve_calendar_selection` (events.md § Where logic
/// lives → *"Which calendars the page is scoped to"*): resolve the Events
/// page's `calendar-item` selection against the calendars that actually exist
/// right now. Returns the selected id back unchanged when it is still live,
/// or `None` (the no-selection union) when it has vanished — deleted here, or
/// by a CalDAV MUA against the same `bridge_caldav_*` store — rather than
/// stranding the page on a permanently blank list with no way back. Resolve
/// at READ time on every query, never by mutating the stored selection on a
/// calendar-list refresh (the poll would otherwise drop a live selection out
/// from under the user every cadence interval).
#[wasm_bindgen(js_name = resolveCalendarSelection)]
pub fn resolve_calendar_selection(
    selected: Option<String>,
    existing_ids: Vec<String>,
) -> Option<String> {
    caldav::resolve_calendar_selection(selected.as_deref(), &existing_ids).map(str::to_string)
}

// ── subscriptions (profile Tiers-tab SELF author management) ───────────────────
//
// The web twin of the linux Slice-A lead (`apps/fauna-linux/src/views/profile`):
// the §1 My tiers / §2 Pending requests / §3 Subscribers author surface over the
// shared `fauna-client-subscriptions`. Pure reads (tiers/requests/roster) +
// thin mutations (tier update/delete, reject) drive the thin `SubscriptionsClient`
// directly; the three crypto-bearing actions (create-tier, approve, remove) go
// through `SubscriptionsAuthor`, which composes the `EncryptedKeyBlobUpload`
// envelope Rust-side (custody → mint_key_blob → upload → bounded retry) — the SPA
// never touches the crypto (priority #2; monetization.md § Pillar 1 — *Where the
// logic lives*; memory `subscription-encrypted-upload-not-shareable-into-wasm`).
// `SubscriptionsAuthor` mints as the owner, so the author-path methods take the
// owner `secret_hex` (the author's keypair); the thin reads/mutations ride the
// already-authenticated singleton client and need none.

/// One of the author's own tiers → the §1 row JSON the profile Tiers tab renders
/// (name / rank / price_hint shown; the rest round-trip the edit form). Drops the
/// `created_at` + `extra` wire fields the UI never reads.
///
/// `unlocks_post` IS carried: the §1 My-tiers list excludes per-post
/// pay-to-unlock tiers client-side off exactly this field
/// (`monetization.md` § Per-post pay-to-unlock — the author's own `tiers.list`
/// read is the one surface that carries them). Dropping it here would leave
/// web structurally unable to apply that exclusion — the same shape as the
/// `provider_item_to_web_json` gap that silently broke the Pillar-3 status
/// render.
fn tier_item_to_web_json(t: &TierItem) -> serde_json::Value {
    serde_json::json!({
        "name": t.name,
        "rank": t.rank,
        "description": t.description,
        "price_hint": t.price_hint,
        // The reverse of `TierAskingPrice::from_sats` — so the edit form can
        // pre-fill the tier's current price in sats (monetization.md § The
        // asking price). `None` for an unpriced tier or a unit this build
        // cannot interpret (fail-closed) — never derived from `price_hint`.
        "asking_price_sats": t.asking_price.as_ref().and_then(|p| p.to_sats()),
        "payment_url": t.payment_url,
        "auto_approve": t.auto_approve,
        "unlocks_post": t.unlocks_post,
        "hidden": t.hidden,
    })
}

/// One pending subscribe/unsubscribe request as it crosses into the SPA — and
/// back, unchanged, into `subscriptionsApprove`.
///
/// A **total** field map of [`PendingRequest`] in both directions — the wasm twin
/// of `fauna-ffi`'s `From<PendingRequest> for FfiPendingRequest` +
/// `pending_from_ffi`, and total for the same stated reason: so it can't silently
/// drift. Totality is load-bearing, not tidiness. The orchestration reads `kind`
/// (an `unsubscribe` row must never be approve-minted — it commits via the
/// removal rotation) and `mlkem_encaps_key` (the hybrid wrap for a subscriber who
/// isn't on the roster yet), and a *partial* reconstruction drops whatever the
/// orchestration grows next: web previously rebuilt this row from three
/// primitives with `kind: String::new()`, so every web approve was refused with
/// `request kind "" cannot be approve-minted` the moment that guard landed.
///
/// Byte fields are hex (the rpc.rs byte-id convention) so they round-trip through
/// JS as strings, never `Array`/`BigInt`.
#[derive(Serialize, Deserialize)]
struct WebPendingRequest {
    request_id: i64,
    /// 32-byte `ActorId`, hex-encoded.
    subscriber_id: String,
    tier_name: String,
    /// `"subscribe"` | `"unsubscribe"`.
    kind: String,
    /// Request creation time, microseconds since the Unix epoch.
    created_at: u64,
    /// The pending subscriber's 1184-byte ML-KEM-768 encapsulation key
    /// (post-quantum surface B, slice S4b), hex-encoded, or `null` if they
    /// published none ⇒ the author seals classical for them.
    #[serde(default)]
    mlkem_encaps_key: Option<String>,
    /// Drives the §2 `subscription-request-paid-badge`: a request the payment
    /// engine entitled (a verified provider webhook, or a redeemed claim code)
    /// rather than a plain manual subscribe. Without it the SPA cannot tell the
    /// two apart — monetization.md § Pillar 3.
    payment_entitled: bool,
}

impl From<&PendingRequest> for WebPendingRequest {
    fn from(r: &PendingRequest) -> Self {
        WebPendingRequest {
            request_id: r.request_id,
            subscriber_id: hex::encode(r.subscriber_id.0),
            tier_name: r.tier_name.clone(),
            kind: r.kind.clone(),
            created_at: r.created_at.0,
            mlkem_encaps_key: r.mlkem_encaps_key.as_ref().map(hex::encode),
            payment_entitled: r.payment_entitled,
        }
    }
}

impl WebPendingRequest {
    /// The reverse map, for the row the SPA hands back to `subscriptionsApprove`.
    /// `extra` (the forward-compat catch-all) is not carried across the JS
    /// boundary — same as the native `pending_from_ffi`.
    fn into_wire(self) -> Result<PendingRequest, JsValue> {
        Ok(PendingRequest {
            request_id: self.request_id,
            subscriber_id: ActorId(hex_array_32(&self.subscriber_id)?),
            tier_name: self.tier_name,
            kind: self.kind,
            created_at: Timestamp(self.created_at),
            mlkem_encaps_key: self
                .mlkem_encaps_key
                .as_deref()
                .map(hex_bytes)
                .transpose()?
                .map(fauna_protocol::ByteBuf::from),
            payment_entitled: self.payment_entitled,
            extra: Default::default(),
        })
    }
}

/// One confirmed subscriber → the §3 roster row JSON; `subscriber_id` hex-encoded
/// for the per-row remove (`subscriptionsRemoveSubscriber`).
fn subscriber_entry_to_web_json(e: &SubscriberEntry) -> serde_json::Value {
    serde_json::json!({ "subscriber_id": hex::encode(e.subscriber_id.0) })
}

/// One of the caller's own subscriptions → the consumer-page row JSON
/// (`subscription-settings` page, Slice B). `author_id` hex-encoded for the
/// per-row unsubscribe (`subscriptionsUnsubscribe`); `handle` is the nest's
/// best-effort local resolution. `author_display` is the `subscription-mine-author`
/// label, pre-computed here through the shared chooser (the wasm twin of
/// `FfiMineSubscription.author_display`) so the page renders it verbatim instead
/// of re-deriving the handle-else-hex fallback — `value-formatting.md`
/// § Subscription author label.
fn mine_subscription_to_web_json(m: &MineSubscription) -> serde_json::Value {
    serde_json::json!({
        "author_id": hex::encode(m.author_id.0),
        "tier": m.tier,
        "status": m.status,
        "handle": m.handle,
        "author_display": fauna_core::format::author_display_label(
            m.handle.as_deref(),
            &m.author_id.0,
        ),
    })
}

/// `fauna.subscriptions.subscribe` reply → the OTHER-profile offer-row JSON.
/// `Approved` (granted inline — auto-approve tier or plaintext mode) vs `Queued`
/// (encrypted-mode pending) drive the row's status flip (`subscription-offer-status`)
/// and the header follow button. The `expires_at` on `Approved` is dropped —
/// the browse UI only needs the active/pending distinction (re-reads via
/// `status.get`). An outcome a newer nest added is an error — sent, state
/// unknown, re-read — the same answer the UniFFI mirror gives, never a third
/// shape the page must render (`transport.md` § Schema and forward-compat
/// discipline, rule 3).
fn subscribe_reply_to_web_json(r: &SubscribeReply) -> Result<serde_json::Value, JsValue> {
    Ok(match r {
        SubscribeReply::Approved { tier, .. } => {
            serde_json::json!({ "outcome": "approved", "tier": tier })
        }
        SubscribeReply::Queued { request_id } => {
            serde_json::json!({ "outcome": "queued", "request_id": request_id })
        }
        SubscribeReply::Unknown => {
            return Err(JsValue::from_str(
                "the nest answered with an outcome this app does not know; re-read the subscription status",
            ));
        }
    })
}

/// `fauna.subscriptions.status.get` reply → JSON for the OTHER-profile browse:
/// the caller's active `tier` for the author (`null` when none) feeds the
/// per-offer-row status badge. `expires_at` is omitted (the browse UI shows only
/// active/pending/none).
fn status_get_reply_to_web_json(r: &StatusGetReply) -> serde_json::Value {
    serde_json::json!({ "tier": r.tier, "auto_approve": r.auto_approve })
}

/// A [`ReconcilePass`] (one author-pump tick) → JSON for the web pump's log
/// lines — mirrors what the native shells (tui/linux) already log per field.
fn reconcile_pass_to_web_json(p: &ReconcilePass) -> serde_json::Value {
    serde_json::json!({
        "resumed": p.resumed,
        "approved": p.approved,
        "resume_error": p.resume_error,
        "drain_error": p.drain_error,
    })
}

/// Build the encrypted-mode author orchestration over the singleton client, the
/// owner keypair (the mint signs with it) and this tab's period-key custody.
fn subscriptions_author(
    client: InnerClient,
    secret_hex: &str,
) -> Result<SubscriptionsAuthor<InnerClient>, JsValue> {
    let keypair = keypair_from_secret_hex(secret_hex)?;
    Ok(SubscriptionsAuthor::over(
        client,
        keypair,
        crate::account_runtime::period_key_store(),
    ))
}

/// The JS-boundary sats amount → the wire `TierAskingPrice`, shared by the tier
/// create/update faces (`monetization.md` § The asking price).
///
/// `f64` at this boundary, not `u64`: a 64-bit integer PARAMETER maps to a JS
/// `bigint` the SPA cannot satisfy from its serde snapshots (which deserialize
/// numbers as plain `number`), so the house convention is `f64` in and a
/// checked cast inside. A negative, non-integral, or msat-overflowing amount
/// is an input error, never a silent truncation — and the conversion itself is
/// the shared `TierAskingPrice::from_sats` constructor, so no app writes the
/// arithmetic.
fn asking_price_from_js_sats(
    sats: Option<f64>,
) -> Result<Option<fauna_protocol::subscriptions::TierAskingPrice>, JsValue> {
    let Some(sats) = sats else { return Ok(None) };
    Some(sats)
        .filter(|s| s.is_finite() && *s >= 0.0 && s.fract() == 0.0)
        .map(|s| s as u64)
        .and_then(fauna_protocol::subscriptions::TierAskingPrice::from_sats)
        .map(Some)
        .ok_or_else(|| {
            JsValue::from_str(
                "asking price must be a whole, non-negative number of sats \
                 small enough to express in msats",
            )
        })
}

#[wasm_bindgen]
impl WsRpcClient {
    // ── §1 My tiers ──────────────────────────────────────────────────────────

    /// `fauna.subscriptions.tiers.list` — the calling author's own tier
    /// definitions, the §1 "My tiers" source. Returns `[{ name, rank, description,
    /// price_hint, payment_url, auto_approve, unlocks_post }]`.
    #[wasm_bindgen(js_name = subscriptionsTiersList)]
    pub fn subscriptions_tiers_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let tiers = SubscriptionsClient::new(client)
                .tiers_list()
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> = tiers.iter().map(tier_item_to_web_json).collect();
            to_js(&rows)
        })
    }

    /// Create a tier (`SubscriptionsAuthor::create_tier`): record a fresh period
    /// key in the period-key custody (`fauna.state.subscriptions`), then
    /// `fauna.subscriptions.tiers.create`.
    /// Encrypted-mode author path — takes the owner `secret_hex` for custody.
    ///
    /// `askingPriceSats` is the machine-comparable purchase threshold in
    /// **sats** (`monetization.md` § The asking price), converted once by the
    /// shared pair. `null` leaves the tier unbuyable by an *inferring*
    /// mechanism — the permanently-correct default, not a gap: a zap on such a
    /// tier stays a tip.
    #[wasm_bindgen(js_name = subscriptionsCreateTier)]
    #[allow(clippy::too_many_arguments)]
    pub fn subscriptions_create_tier(
        &self,
        secret_hex: String,
        name: String,
        rank: u32,
        description: Option<String>,
        price_hint: Option<String>,
        payment_url: Option<String>,
        auto_approve: bool,
        asking_price_sats: Option<f64>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let asking_price = asking_price_from_js_sats(asking_price_sats)?;
            let author = subscriptions_author(client, &secret_hex)?;
            author
                .create_tier(
                    &name,
                    rank,
                    description,
                    price_hint,
                    payment_url,
                    auto_approve,
                    // Ordinary tier-management form — never a per-post
                    // pay-to-unlock tier: that designation is set only by the
                    // "sell this post" orchestration (monetization.md gap 2).
                    None,
                    asking_price,
                    // The tier-management form mints OFFERED tiers; the reserved
                    // hidden tier is provisioned by the archive-import machine,
                    // not by hand.
                    false,
                )
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.subscriptions.tiers.update` — overwrite a tier's mutable fields
    /// (metadata only; no custody/crypto change, so a thin call). The tier name is
    /// the server key and is not editable.
    ///
    /// `askingPriceSats` is the machine-comparable asking price in **sats**
    /// (`monetization.md` § The asking price), converted once by the shared
    /// pair. `null` keeps the tier's current price — this method has no clear
    /// verb, like every other optional field on it.
    #[wasm_bindgen(js_name = subscriptionsUpdateTier)]
    #[allow(clippy::too_many_arguments)]
    pub fn subscriptions_update_tier(
        &self,
        name: String,
        rank: u32,
        description: Option<String>,
        price_hint: Option<String>,
        payment_url: Option<String>,
        auto_approve: bool,
        asking_price_sats: Option<f64>,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let asking_price = asking_price_from_js_sats(asking_price_sats)?;
            SubscriptionsClient::new(client)
                .tiers_update(
                    name,
                    Some(rank),
                    description,
                    price_hint,
                    payment_url,
                    Some(auto_approve),
                    asking_price,
                )
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.subscriptions.tiers.delete` — remove a tier by name (thin,
    /// idempotent).
    #[wasm_bindgen(js_name = subscriptionsDeleteTier)]
    pub fn subscriptions_delete_tier(&self, name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            SubscriptionsClient::new(client)
                .tiers_delete(name)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── §2 Pending requests ────────────────────────────────────────────────────

    /// `fauna.subscriptions.requests.list` — the author's pending subscribe/
    /// unsubscribe requests, as whole [`WebPendingRequest`] rows. The SPA hands
    /// one back verbatim to `subscriptionsApprove`.
    #[wasm_bindgen(js_name = subscriptionsRequestsList)]
    pub fn subscriptions_requests_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let requests = SubscriptionsClient::new(client)
                .requests_list()
                .await
                .map_err(err_to_js)?;
            let rows: Vec<WebPendingRequest> =
                requests.iter().map(WebPendingRequest::from).collect();
            to_js(&rows)
        })
    }

    /// Approve a pending request (`SubscriptionsAuthor::approve_subscriber`): mint a
    /// broadcast `KeyBlob` over the post-approval roster and upload it via
    /// `fauna.subscriptions.requests.approve`.
    ///
    /// `request` is the **whole** §2 row the SPA got from
    /// `subscriptionsRequestsList`, handed back verbatim — the same contract as
    /// the native `subscriptions_approve_subscriber(…, FfiPendingRequest)`. It is
    /// the row, not a few of its fields, because the orchestration refuses a row
    /// whose `kind` isn't `subscribe` and wraps hybrid off `mlkem_encaps_key`;
    /// see [`WebPendingRequest`].
    #[wasm_bindgen(js_name = subscriptionsApprove)]
    pub fn subscriptions_approve(&self, secret_hex: String, request: JsValue) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let request = from_js::<WebPendingRequest>(request)?.into_wire()?;
            let author = subscriptions_author(client, &secret_hex)?;
            author
                .approve_subscriber(&request)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.subscriptions.requests.reject` — decline a pending request by id
    /// (thin, idempotent).
    #[wasm_bindgen(js_name = subscriptionsReject)]
    pub fn subscriptions_reject(&self, request_id: f64) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            SubscriptionsClient::new(client)
                .requests_reject(request_id as i64)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── §3 Subscribers roster ───────────────────────────────────────────────────

    /// `fauna.subscriptions.subscribers.list` — the confirmed roster of
    /// `tier_name`. Returns `[{ subscriber_id (hex) }]`.
    #[wasm_bindgen(js_name = subscriptionsSubscribersList)]
    pub fn subscriptions_subscribers_list(&self, tier_name: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let roster = SubscriptionsClient::new(client)
                .subscribers_list(tier_name)
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> =
                roster.iter().map(subscriber_entry_to_web_json).collect();
            to_js(&rows)
        })
    }

    /// Remove a subscriber (`SubscriptionsAuthor::remove_subscriber`): rotate to a
    /// fresh period key, mint over the post-removal roster, upload via
    /// `fauna.subscriptions.subscribers.remove`, then commit the rotation
    /// (crash-staged). Encrypted-mode author path — takes the owner `secret_hex`.
    #[wasm_bindgen(js_name = subscriptionsRemoveSubscriber)]
    pub fn subscriptions_remove_subscriber(
        &self,
        secret_hex: String,
        tier_name: String,
        subscriber_id_hex: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let subscriber_id = ActorId(hex_array_32(&subscriber_id_hex)?);
            let author = subscriptions_author(client, &secret_hex)?;
            author
                .remove_subscriber(&tier_name, subscriber_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Auto-approve every pending **subscribe** request whose tier is
    /// `auto_approve` (`SubscriptionsAuthor::drain_auto_approvals`): mint the
    /// covering `KeyBlob` for each and upload via `requests.approve`. This is
    /// what makes an encrypted-mode **follow** frictionless — the nest cannot
    /// mint, so a follow *enqueues* (`Queued`) and the author's web app drains
    /// it here. Call from the app-wide connect path (next to the conversations
    /// `startReceivePoll`), so a queued follow transitions `Queued` → granted
    /// once the author is online. Only `auto_approve` tiers + `subscribe` rows;
    /// an `unsubscribe` row is left for `subscriptionsRemoveSubscriber`. Resolves
    /// to the number approved this pass (a poisoned request is skipped so it
    /// can't starve the rest). Encrypted-mode author path — takes `secret_hex`.
    #[wasm_bindgen(js_name = subscriptionsDrainAutoApprovals)]
    pub fn subscriptions_drain_auto_approvals(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let author = subscriptions_author(client, &secret_hex)?;
            let n = author.drain_auto_approvals().await.map_err(err_to_js)?;
            Ok(JsValue::from_f64(n as f64))
        })
    }

    /// Re-drive every staged subscriber-removal whose upload was interrupted by a
    /// crash (`SubscriptionsAuthor::resume_pending_removals`). Call on the same
    /// app-wide connect path as `subscriptionsDrainAutoApprovals` (the web author
    /// pump), so the rotate-on-removal forward-secrecy guarantee completes once
    /// the author is online — the web twin of `subscriptionsResumePendingRemovals`
    /// over UniFFI (`libs/fauna-ffi`) and `foldersResumePendingRemovals`.
    /// Resolves to the count resumed. Encrypted-mode author path — takes
    /// `secret_hex`.
    #[wasm_bindgen(js_name = subscriptionsResumePendingRemovals)]
    pub fn subscriptions_resume_pending_removals(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let author = subscriptions_author(client, &secret_hex)?;
            let n = author.resume_pending_removals().await.map_err(err_to_js)?;
            Ok(JsValue::from_f64(n as f64))
        })
    }

    /// **One author-pump tick** (`SubscriptionsAuthor::reconcile_once`) — the
    /// shared resume-then-drain sequencing tui/linux/android/windows already
    /// call, so web's own pump (`subscriptionsAuthor.ts`) schedules this single
    /// call per tick instead of re-deriving the two-step order itself
    /// (`monetization.md` § Pillar 1 → *Where the logic lives*: "An app MUST NOT
    /// re-derive either"). Best-effort by construction — never rejects; both
    /// halves' failures (if any) come back as strings for the shell to log.
    /// Once per session it also runs the stale-keyed-blob pass
    /// (`SubscriptionsAuthor::republish_stale_keyed_blobs`), keyed on this
    /// client's latch, which logs its own outcome.
    /// Encrypted-mode author path — takes `secret_hex`.
    #[wasm_bindgen(js_name = subscriptionsReconcileOnce)]
    pub fn subscriptions_reconcile_once(&self, secret_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        let latch = self.subscriptions_connect_pass.clone();
        future_to_promise(async move {
            let author = subscriptions_author(client, &secret_hex)?.with_connect_pass(latch);
            let pass = author.reconcile_once().await;
            to_js(&reconcile_pass_to_web_json(&pass))
        })
    }

    // ── consumer side (subscription-settings page, Slice B) ─────────────────────

    /// `fauna.subscriptions.mine.list` — the calling actor's own subscriptions
    /// across every creator (the caller-scoped enumeration; thin read). Returns
    /// `[{ author_id (hex), tier, status, handle }]`.
    #[wasm_bindgen(js_name = subscriptionsMineList)]
    pub fn subscriptions_mine_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let mine = SubscriptionsClient::new(client)
                .mine_list()
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> =
                mine.iter().map(mine_subscription_to_web_json).collect();
            to_js(&rows)
        })
    }

    /// `fauna.subscriptions.unsubscribe` — the calling actor drops its
    /// subscription to `author_id` (thin). Encrypted mode returns `Queued` (the
    /// subscriber stays until the author commits the removal), so the row does not
    /// vanish immediately — the page re-reads to reflect nest state.
    #[wasm_bindgen(js_name = subscriptionsUnsubscribe)]
    pub fn subscriptions_unsubscribe(&self, author_id_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let author_id = ActorId(hex_array_32(&author_id_hex)?);
            SubscriptionsClient::new(client)
                .unsubscribe(author_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── OTHER profile (subscriber browse): offers + subscribe + status ──────────
    //
    // The Tiers tab when viewing *another* actor's profile (`profile.md` § Layout
    // & flow → Another's profile; `monetization.md` § Pillar 1 — surface 2): the
    // creator's offered tiers (`offers.list`), the viewer's current status per
    // creator (`status.get`), and the subscribe action (`subscribe`, also the
    // header follow button via `tier = "followers"`). All thin reads/writes over
    // the landed `SubscriptionsClient` kinds; no author-side mint orchestration.

    /// `fauna.subscriptions.offers.list` — **another** author's offered tiers
    /// (request-supplied `author_id_hex`), the subscriber-browse source for the
    /// profile Tiers tab on someone else's profile. Returns the same
    /// `[{ name, rank, description, price_hint, payment_url, auto_approve }]`
    /// shape as `subscriptionsTiersList` (reuses `tier_item_to_web_json`); the
    /// SPA filters out the free "followers" tier (it is the header
    /// `profile-follow-button`, not a per-row offer).
    #[wasm_bindgen(js_name = subscriptionsOffersList)]
    pub fn subscriptions_offers_list(&self, author_id_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let author_id = ActorId(hex_array_32(&author_id_hex)?);
            let tiers = SubscriptionsClient::new(client)
                .offers_list(author_id)
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> = tiers.iter().map(tier_item_to_web_json).collect();
            to_js(&rows)
        })
    }

    /// `fauna.subscriptions.subscribe` — the calling actor requests `tier` from
    /// `author_id_hex`. Powers the OTHER-profile per-row Subscribe button and the
    /// header follow button (`tier = "followers"`). Returns
    /// `{ outcome: "approved", tier }` (granted inline — auto-approve tier or
    /// plaintext mode) or `{ outcome: "queued", request_id }` (encrypted-mode
    /// pending); the SPA flips `subscription-offer-status` on the outcome.
    #[wasm_bindgen(js_name = subscriptionsSubscribe)]
    pub fn subscriptions_subscribe(&self, author_id_hex: String, tier: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let author_id = ActorId(hex_array_32(&author_id_hex)?);
            let reply = SubscriptionsClient::new(client)
                .subscribe(author_id, tier)
                .await
                .map_err(err_to_js)?;
            to_js(&subscribe_reply_to_web_json(&reply)?)
        })
    }

    /// `fauna.subscriptions.subscribe`, **publishing** the caller's
    /// identity-seed ML-KEM ek (surface B, S4b) unconditionally (no capability
    /// token), so an author can later wrap hybrid `KeyBlob`s to this
    /// subscriber. `secret_hex` is the caller's own 32-byte identity seed. The
    /// web twin of `fauna-ffi`'s `subscriptions_subscribe_publishing_ek`.
    #[wasm_bindgen(js_name = subscriptionsSubscribePublishingEk)]
    pub fn subscriptions_subscribe_publishing_ek(
        &self,
        secret_hex: String,
        author_id_hex: String,
        tier: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let subscriber = keypair_from_secret_hex(&secret_hex)?;
            let author_id = ActorId(hex_array_32(&author_id_hex)?);
            let reply = SubscriptionsClient::new(client)
                .subscribe_publishing_ek(author_id, tier, &subscriber)
                .await
                .map_err(err_to_js)?;
            to_js(&subscribe_reply_to_web_json(&reply)?)
        })
    }

    /// `fauna.subscriptions.status.get` — the caller's current subscription
    /// status for `author_id_hex` (the active `tier`, or `null` when none).
    /// Drives the per-offer-row status badge (Subscribed / Pending / Not
    /// subscribed) on the OTHER-profile Tiers tab. Replay-safe pure read.
    #[wasm_bindgen(js_name = subscriptionsStatusGet)]
    pub fn subscriptions_status_get(&self, author_id_hex: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let author_id = ActorId(hex_array_32(&author_id_hex)?);
            let reply = SubscriptionsClient::new(client)
                .status_get(author_id)
                .await
                .map_err(err_to_js)?;
            to_js(&status_get_reply_to_web_json(&reply))
        })
    }
}

// ── payments (Pillar 3 client legs — monetization.md § Pillars 2+3 client UX) ──
//
// The web twin of the linux lead: the profile Tiers-tab §4 provider section
// (`subscription-provider-*` — kind + webhook-verification secret + entitled
// tier, over `fauna.payments.providers.{set,list,remove}`) and the
// `subscription-settings` claim redemption (`subscription-claim-redeem-*`,
// over `fauna.payments.claims.redeem`). All thin calls over the shared
// `fauna-client-payments` (priority #2); the nest owns validation (unknown
// kind / dangling tier / empty secret are typed `fauna.payments.*` errors).

/// One configured provider → the §4 row JSON. Deliberately no webhook secret
/// (the list reply omits it — changing it means re-entering it in the form).
/// `last_verified_at`/`last_rejected_at` (epoch seconds) feed the evidence-based
/// status badge (`providerStatusLabel`; monetization.md § Pillar 3).
#[cfg(feature = "payments")]
fn provider_item_to_web_json(p: &fauna_protocol::payments::ProviderItem) -> serde_json::Value {
    serde_json::json!({
        "kind": p.kind,
        "tier": p.tier,
        "last_verified_at": p.last_verified_at,
        "last_rejected_at": p.last_rejected_at,
    })
}

/// One claim code → the §5 row JSON — the audit surface for BOTH
/// manually-minted and webhook-minted codes.
#[cfg(feature = "payments")]
fn claim_item_to_web_json(c: &fauna_protocol::payments::ClaimItem) -> serde_json::Value {
    serde_json::json!({
        "code": c.code,
        "tier": c.tier,
        "provider": c.provider,
        "valid_until": c.valid_until.map(|t| t.0),
        "created_at": c.created_at.0,
        "redeemed_by": c.redeemed_by.map(|a| hex::encode(a.0)),
        "redeemed_at": c.redeemed_at.map(|t| t.0),
        "voided_at": c.voided_at.map(|t| t.0),
    })
}

/// Every registered payment-provider kind — the §4 provider form's kind
/// select enumerates these (the same `fauna-payments` registry the nest's
/// `providers.set` validates against).
#[cfg(feature = "payments")]
#[wasm_bindgen(js_name = paymentsKnownKinds)]
pub fn payments_known_kinds() -> Vec<String> {
    fauna_client_payments::known_kinds()
        .iter()
        .map(|k| k.to_string())
        .collect()
}

/// The exact URL a creator registers at their provider's dashboard for
/// `kind` — the §4 provider form previews it live as the kind select
/// changes. Derived from the same constant the nest registers its ingress
/// route from, so the SPA can never hand-assemble a stale path.
#[cfg(feature = "payments")]
#[wasm_bindgen(js_name = paymentsWebhookUrl)]
pub fn payments_webhook_url(base_url: String, author_id_hex: String, kind: String) -> String {
    fauna_client_payments::webhook_url(&base_url, &author_id_hex, &kind)
}

// Gated on its OWN `impl` block: the whole money plane is the `payments`
// registry feature, and a wasm export keyed on the feature keeps the generated
// JS/.d.ts face a pure function of the feature set (§ The cargo feature spine,
// rule (c)).
#[cfg(feature = "payments")]
#[wasm_bindgen]
impl WsRpcClient {
    /// `fauna.payments.providers.set` — upsert the calling author's config
    /// for one provider kind (kind + webhook-verification secret + entitled
    /// tier). Typed errors: `fauna.payments.{unknown_provider,tier_not_found,
    /// malformed}`.
    #[wasm_bindgen(js_name = paymentsProvidersSet)]
    pub fn payments_providers_set(
        &self,
        kind: String,
        webhook_secret: String,
        tier: String,
    ) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            PaymentsClient::new(client)
                .providers_set(kind, webhook_secret, tier)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.payments.providers.list` — the calling author's own configured
    /// providers, ascending by kind. Returns `[{ kind, tier }]`; the webhook
    /// secret never rides the reply. Replay-safe pure read.
    #[wasm_bindgen(js_name = paymentsProvidersList)]
    pub fn payments_providers_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let rows = PaymentsClient::new(client)
                .providers_list()
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> = rows.iter().map(provider_item_to_web_json).collect();
            to_js(&rows)
        })
    }

    /// `fauna.payments.providers.remove` — delete the calling author's
    /// config for one provider kind (idempotent).
    #[wasm_bindgen(js_name = paymentsProvidersRemove)]
    pub fn payments_providers_remove(&self, kind: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            PaymentsClient::new(client)
                .providers_remove(kind)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `fauna.payments.claims.redeem` — bind a post-payment claim code to
    /// the calling actor; the entitlement lands through Pillar 1's grant
    /// queue. Returns `{ author (hex), tier, queued }` — `queued: true`
    /// renders exactly like a queued subscribe (the creator's client mints
    /// on its next drain pass). Typed errors:
    /// `fauna.payments.claim_{not_found,already_redeemed,voided}`.
    #[wasm_bindgen(js_name = paymentsClaimsRedeem)]
    pub fn payments_claims_redeem(&self, code: String) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PaymentsClient::new(client)
                .claims_redeem(code)
                .await
                .map_err(err_to_js)?;
            to_js(&serde_json::json!({
                "author": hex::encode(reply.author.0),
                "tier": reply.tier,
                "queued": reply.queued,
            }))
        })
    }

    /// `fauna.payments.claims.mint` — the author mints a claim code manually,
    /// for a no-API provider (bank transfer, cash, …) already paid
    /// out-of-band. Always `provider = "manual"`; the nest rejects a `tier`
    /// that isn't one of the author's own tiers.
    #[wasm_bindgen(js_name = paymentsClaimsMint)]
    pub fn payments_claims_mint(&self, tier: String, valid_until: Option<u64>) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let reply = PaymentsClient::new(client)
                .claims_mint(tier, valid_until.map(fauna_core::data::Timestamp))
                .await
                .map_err(err_to_js)?;
            to_js(&serde_json::json!({
                "code": reply.code,
                "tier": reply.tier,
                "valid_until": reply.valid_until.map(|t| t.0),
            }))
        })
    }

    /// `fauna.payments.claims.list` — the calling author's own claim codes,
    /// newest first; the audit surface for BOTH manually-minted and
    /// webhook-minted codes. Replay-safe pure read.
    #[wasm_bindgen(js_name = paymentsClaimsList)]
    pub fn payments_claims_list(&self) -> js_sys::Promise {
        let client = self.inner.clone();
        future_to_promise(async move {
            let rows = PaymentsClient::new(client)
                .claims_list()
                .await
                .map_err(err_to_js)?;
            let rows: Vec<serde_json::Value> = rows.iter().map(claim_item_to_web_json).collect();
            to_js(&rows)
        })
    }
}

/// The two admission age-band selects' option value → the typed band the
/// shared writer takes (`undefined`/`""` = no band). A token the vocabulary
/// cannot name rejects at this boundary, so the SPA can never send a band the
/// nest would refuse (`fauna_client_admin::age_band_from_option_value`).
fn parse_age_band(value: Option<String>) -> Result<Option<fauna_protocol::age::AgeBand>, JsValue> {
    match value.as_deref() {
        None | Some(fauna_client_admin::AGE_BAND_NOT_SET_VALUE) => Ok(None),
        Some(token) => fauna_client_admin::age_band_from_option_value(token)
            .map(Some)
            .ok_or_else(|| {
                err_to_js(format!(
                    "unknown age band {token:?}; expected one of u13 / 13-15 / 16-17 / 18+"
                ))
            }),
    }
}

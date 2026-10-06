//! User-facing WS-RPC payload types for the unified notifications surface —
//! the list / mark-read / count plane end-user clients call from the
//! notifications inbox + unread badge. T1 of
//! the WS-RPC-everywhere migration (tracked internally) ships the notifications
//! cluster (`fauna.notifications.{list,mark_read,count}`); the contacts
//! cluster (`fauna.{knocks,contacts,inbox.mode}.*`) lands in T2.
//!
//! This is a **behavior-preserving** transport migration of the existing
//! HTTP routes (`GET /api/v1/notifications/{actor_id}`,
//! `POST /api/v1/notifications/{actor_id}/read`,
//! `GET /api/v1/notifications/{actor_id}/count`); the request/reply shapes
//! mirror those routes exactly. The handlers reuse the same `CacheDb`
//! methods the HTTP twins call (`list_notifications` /
//! `mark_notifications_read` / `count_unread_notifications`) — no logic is
//! duplicated. The connection actor replaces the HTTP `{actor_id}` path
//! param + bearer-match.
//!
//! - `list` carries an optional `cursor` (last-seen `id`) + `limit`; the
//!   reply mirrors the HTTP twin's `{notifications:[…], cursor}`. Each
//!   `NotifItem` mirrors the HTTP json keys, with the `type` json key named
//!   `notif_type` on the typed wire (a fresh CBOR wire format, not the
//!   json). `sender_id` / `content_id` ride as hex strings (the HTTP twin
//!   `hex::encode`d the raw `[u8; 32]`).
//! - `mark_read` carries an optional `up_to` micros timestamp (the handler
//!   defaults to now when omitted, matching the HTTP twin); the reply
//!   echoes `{marked_read}`.
//! - `count` takes no params (the actor is the connection); the reply is
//!   `{count}` — the HTTP twin's `{count}`.
//!
//! Kind registry entries live in `kind.rs::register_notifications_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{LocalizedText, Value};

/// The notification type — a string on the wire, an enum in every build that
/// reads it. Defined in `fauna-core` so the icon classification can match it
/// exhaustively; this is its wire home.
pub use fauna_core::notification_type::NotifType;

// ── fauna.notifications.list ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifListRequest {
    /// Cursor for pagination — the last-seen notification `id`. `None`
    /// fetches the newest page. The HTTP twin took it as the `?cursor=`
    /// query param.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    /// Page size. `None` lets the handler apply the HTTP twin's default
    /// (25) + clamp (1..=100). The HTTP twin took it as `?limit=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A single notification — mirrors the HTTP twin's per-row json object
/// (`notif_routes.rs` `list_notifications`). `sender_id` / `content_id`
/// are hex-encoded `[u8; 32]` (the twin `hex::encode`d the raw bytes; the
/// underlying `NotificationRow` carries `Option<Vec<u8>>`). `summary` is a
/// plain `String` (the `NotificationRow.summary` field is non-optional and
/// the twin emitted it directly).
///
/// `Default` exists for fixtures only (`..Default::default()`), so two
/// branches growing this struct merge cleanly; no producer mints a default row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotifItem {
    pub id: i64,
    /// The notification type (`like` / `reply` / …): a string on the wire,
    /// read as [`NotifType`]. A type this build does not name decodes into
    /// its carrying arm and re-encodes unchanged, so a newer nest's type
    /// never fails the list (`behavior/notifications.md` § The notification
    /// type).
    pub notif_type: NotifType,
    /// Origin protocol (`fauna` / `bluesky` / `nostr` / `activitypub`).
    pub source: String,
    /// Hex-encoded `[u8; 32]` sender actor id, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    /// Hex-encoded `[u8; 32]` content id (e.g. the liked/replied post),
    /// when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// Subject URI for non-fauna-native notifications, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_uri: Option<String>,
    /// The English rendering of the row's sentence — the compat fallback an
    /// app paints when it has no usable [`body`](Self::body). Always populated
    /// by the nest (`behavior/notifications.md` § Localized body).
    pub summary: String,
    /// What happened, as an `i18n/strings/en.yaml` key
    /// (`notifications.<name>`) plus data args; the app says it in the
    /// reader's language. Absent on rows minted without one (test-hook rows, rows whose args
    /// were malformed). Render through
    /// `fauna_client_notifications::notification_text`, never directly: an
    /// unknown key must fall back to `summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<LocalizedText>,
    pub is_read: bool,
    /// Creation timestamp (micros since epoch).
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifListReply {
    pub notifications: Vec<NotifItem>,
    /// Next-page cursor — the `id` of the last row in this page (the HTTP
    /// twin's `next_cursor`). `None` when the page is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.notifications.mark_read ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifMarkReadRequest {
    /// Mark all notifications created at or before this micros timestamp as
    /// read. `None` ⇒ mark everything up to now (the handler substitutes
    /// the current time, matching the HTTP twin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifMarkReadReply {
    /// Number of rows flipped to read — the HTTP twin's `{marked_read}`.
    pub marked_read: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.notifications.count ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifCountRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifCountReply {
    /// Unread count for the connection actor — the HTTP twin's `{count}`.
    pub count: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.notifications.dismiss ────────────────────────────────────────
//
// The two user-initiated deletes `behavior/notifications.md` § Retention
// rules (rule 2): a notification row is the user's own record and nothing on
// the nest sweeps it by age or count, so the only way a row leaves the table
// short of the account going, or a knock doorbell going with its knock, is
// the user removing it — one row, or everything up to a moment.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifDismissRequest {
    /// The `NotifItem.id` to delete. Scoped to the connection actor: an id
    /// that is not the caller's row is not found, never someone else's row.
    pub id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifDismissReply {
    /// `true` when a row was deleted; `false` when no row of that id was the
    /// caller's. Idempotent, never an error — a replay or a double-tap
    /// deletes nothing new.
    pub dismissed: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.notifications.clear ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifClearRequest {
    /// Delete every row of the caller's created at or before this micros
    /// timestamp. `None` ⇒ everything up to now (the handler substitutes the
    /// current time — `mark_read`'s shape, so a page that lists then clears
    /// cannot delete a row that arrived after it looked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotifClearReply {
    /// Number of rows deleted. A retained security notice (see
    /// [`is_security_notice_retained`]) is skipped, not counted.
    pub cleared: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── The security-notice window ─────────────────────────────────────────
//
// `behavior/notifications.md` § Retention, rule 2's one carve-out: a
// `security.notice` row is the account's record of what a *session* did, and
// the session it exposes can call dismiss/clear the second it lands — so for
// a fixed window neither grain removes it. Shared here so an app can grey the
// dismiss affordance with no wire change; the nest's refusal stays the
// authority.

/// How long after its `created_at` a security notice is deletable by neither
/// `fauna.notifications.dismiss` nor `.clear`: 14 days, the same delay (and
/// for the same reason) as the account-deletion wait. A hard-coded constant —
/// no user or admin would tune it.
pub const SECURITY_NOTICE_RETENTION_SECS: i64 = 14 * 24 * 60 * 60;

/// The refusal `fauna.notifications.dismiss` answers on a retained row —
/// distinct from `{ dismissed: false }`, which means "no row of that id is
/// yours".
pub const CODE_NOTIFICATION_RETAINED: &str = "fauna.notifications.retained";

/// The oldest `created_at` (micros) still inside the window at `now_micros`:
/// a security notice created strictly after it is retained.
pub fn security_notice_window_start(now_micros: i64) -> i64 {
    now_micros.saturating_sub(SECURITY_NOTICE_RETENTION_SECS.saturating_mul(1_000_000))
}

/// Whether a row of `notif_type` created at `created_at_micros` is inside the
/// security-notice window at `now_micros` — the nest's delete predicate and
/// an app's "grey the dismiss" test, one definition.
pub fn is_security_notice_retained(
    notif_type: &NotifType,
    created_at_micros: i64,
    now_micros: i64,
) -> bool {
    *notif_type == NotifType::SecurityNotice
        && created_at_micros > security_notice_window_start(now_micros)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn security_notice_window_edges() {
        let now = 100 * SECURITY_NOTICE_RETENTION_SECS * 1_000_000;
        let start = security_notice_window_start(now);
        assert_eq!(start, now - 14 * 86_400 * 1_000_000);
        // Inside the window (strictly after its start) → retained.
        let notice = NotifType::SecurityNotice;
        assert!(is_security_notice_retained(&notice, now, now));
        assert!(is_security_notice_retained(&notice, start + 1, now));
        // At or before the start → the user's to remove again.
        assert!(!is_security_notice_retained(&notice, start, now));
        // Only security notices take the window.
        assert!(!is_security_notice_retained(&NotifType::Like, now, now));
        // A clock at zero never underflows.
        assert!(security_notice_window_start(0) < 0);
    }

    fn sample_list_request() -> NotifListRequest {
        NotifListRequest {
            cursor: Some(42),
            limit: Some(25),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn notif_list_request_round_trips() {
        let req = sample_list_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifListRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn notif_list_request_canonical_re_encodes_identically() {
        let req = sample_list_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: NotifListRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn notif_list_request_omits_optional_fields() {
        let req = NotifListRequest {
            cursor: None,
            limit: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifListRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert!(decoded.cursor.is_none());
        assert!(decoded.limit.is_none());
    }

    fn sample_list_reply() -> NotifListReply {
        NotifListReply {
            notifications: vec![
                NotifItem {
                    id: 7,
                    notif_type: NotifType::Like,
                    source: "fauna".into(),
                    sender_id: Some("ab".repeat(32)),
                    content_id: Some("cd".repeat(32)),
                    subject_uri: None,
                    summary: "alice liked your post".into(),
                    body: Some(
                        LocalizedText::new("notifications.row_like").with_arg("sender", "alice"),
                    ),
                    is_read: false,
                    created_at: 1000,
                    extra: BTreeMap::new(),
                },
                NotifItem {
                    id: 3,
                    notif_type: NotifType::Mention,
                    source: "bluesky".into(),
                    sender_id: None,
                    content_id: None,
                    subject_uri: Some("at://did:plc:xyz/app.bsky.feed.post/1".into()),
                    summary: "you were mentioned".into(),
                    body: None,
                    is_read: true,
                    created_at: 900,
                    extra: BTreeMap::new(),
                },
            ],
            cursor: Some(3),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn notif_list_reply_round_trips() {
        let reply = sample_list_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NotifListReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn notif_list_reply_canonical_re_encodes_identically() {
        let reply = sample_list_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: NotifListReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    /// The older-app half of `behavior/notifications.md` § Localized body: a
    /// reader that predates `body` keeps it in `extra` (so a relay re-emits
    /// it) and still has the `summary` it renders.
    #[test]
    fn a_reader_that_predates_body_keeps_summary_and_preserves_body_in_extra() {
        #[derive(Deserialize)]
        struct OldNotifItem {
            summary: String,
            #[serde(flatten, default)]
            extra: BTreeMap<String, Value>,
        }
        let item = sample_list_reply().notifications.remove(0);
        let bytes = encode_canonical(&item).unwrap();
        let old: OldNotifItem = decode(&bytes).unwrap();
        assert_eq!(old.summary, "alice liked your post");
        assert!(old.extra.contains_key("body"));
    }

    /// A row with no `body` (the field simply absent on the wire) decodes with
    /// `body: None`.
    #[test]
    fn a_row_minted_without_body_decodes_as_none() {
        #[derive(Serialize)]
        struct OldWireItem {
            id: i64,
            notif_type: String,
            source: String,
            summary: String,
            is_read: bool,
            created_at: i64,
        }
        let bytes = encode_canonical(&OldWireItem {
            id: 1,
            notif_type: "like".into(),
            source: "fauna".into(),
            summary: "alice liked your post".into(),
            is_read: false,
            created_at: 5,
        })
        .unwrap();
        let item: NotifItem = decode(&bytes).unwrap();
        assert!(item.body.is_none());
        assert_eq!(item.summary, "alice liked your post");
    }

    /// The retype changed how a build reads the field, not what is on the
    /// wire: the typed row is the bytes the plain-string row was.
    #[test]
    fn the_typed_row_is_byte_identical_to_the_string_row() {
        #[derive(Serialize)]
        struct StringItem {
            id: i64,
            notif_type: String,
            source: String,
            summary: String,
            is_read: bool,
            created_at: i64,
        }
        for (typed, wire) in [
            (NotifType::Like, "like"),
            (NotifType::Interaction, "other"),
            (NotifType::SecurityNotice, "security.notice"),
            (NotifType::FamilyContactRequest, "family.contact_request"),
        ] {
            let string_bytes = encode_canonical(&StringItem {
                id: 1,
                notif_type: wire.into(),
                source: "fauna".into(),
                summary: "s".into(),
                is_read: false,
                created_at: 5,
            })
            .unwrap();
            let typed_bytes = encode_canonical(&NotifItem {
                id: 1,
                notif_type: typed.clone(),
                source: "fauna".into(),
                summary: "s".into(),
                is_read: false,
                created_at: 5,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(typed_bytes, string_bytes, "{wire}");
            let back: NotifItem = decode(&string_bytes).unwrap();
            assert_eq!(back.notif_type, typed, "{wire}");
        }
    }

    /// `transport.md` § Rule 3, the carrying arm: a type a newer nest minted
    /// decodes without failing the row, and a re-encode emits it unchanged.
    #[test]
    fn a_type_this_build_does_not_name_decodes_and_re_encodes_unchanged() {
        #[derive(Serialize)]
        struct NewerItem {
            id: i64,
            notif_type: String,
            source: String,
            summary: String,
            is_read: bool,
            created_at: i64,
        }
        let bytes = encode_canonical(&NewerItem {
            id: 9,
            notif_type: "calendar.rsvp".into(),
            source: "fauna".into(),
            summary: "bob answered your invitation".into(),
            is_read: false,
            created_at: 5,
        })
        .unwrap();
        let item: NotifItem = decode(&bytes).unwrap();
        assert_eq!(item.notif_type, NotifType::Other("calendar.rsvp".into()));
        assert_eq!(encode_canonical(&item).unwrap(), bytes);
    }

    #[test]
    fn notif_list_reply_empty_omits_cursor() {
        let reply = NotifListReply {
            notifications: vec![],
            cursor: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NotifListReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert!(decoded.cursor.is_none());
        assert!(decoded.notifications.is_empty());
    }

    #[test]
    fn notif_mark_read_request_round_trips() {
        let req = NotifMarkReadRequest {
            up_to: Some(1_700_000_000_000_000),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifMarkReadRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn notif_mark_read_request_omits_up_to() {
        let req = NotifMarkReadRequest {
            up_to: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifMarkReadRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert!(decoded.up_to.is_none());
    }

    #[test]
    fn notif_mark_read_reply_round_trips() {
        let reply = NotifMarkReadReply {
            marked_read: 5,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NotifMarkReadReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn notif_mark_read_reply_canonical_re_encodes_identically() {
        let reply = NotifMarkReadReply {
            marked_read: 5,
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: NotifMarkReadReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn notif_count_request_round_trips() {
        let req = NotifCountRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifCountRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn notif_count_reply_round_trips() {
        let reply = NotifCountReply {
            count: 12,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NotifCountReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn notif_count_reply_canonical_re_encodes_identically() {
        let reply = NotifCountReply {
            count: 12,
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: NotifCountReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn notif_dismiss_request_and_reply_round_trip() {
        let req = NotifDismissRequest {
            id: 42,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifDismissRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert_eq!(encode_canonical(&decoded).unwrap(), bytes);

        let reply = NotifDismissReply {
            dismissed: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NotifDismissReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert_eq!(encode_canonical(&decoded).unwrap(), bytes);
    }

    #[test]
    fn notif_clear_request_omits_up_to_and_reply_round_trips() {
        let req = NotifClearRequest {
            up_to: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifClearRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert!(decoded.up_to.is_none());

        let req = NotifClearRequest {
            up_to: Some(1_700_000_000_000_000),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NotifClearRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);

        let reply = NotifClearReply {
            cleared: 5,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NotifClearReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert_eq!(encode_canonical(&decoded).unwrap(), bytes);
    }
}

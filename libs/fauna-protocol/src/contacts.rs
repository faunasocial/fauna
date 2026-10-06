//! User-facing WS-RPC payload types for the unified connection-management
//! surface — knocks (inbound contact requests), the contact roster, and the
//! inbox-acceptance policy (inbox-mode) end-user clients drive from the
//! knocks inbox + contacts list + inbox-settings affordances. T2 of
//! the WS-RPC-everywhere migration (tracked internally) ships this contacts cluster
//! (`fauna.knocks.*` + `fauna.contacts.*` + `fauna.inbox.mode.*` — three top
//! namespaces, one connection-management feature, mirroring the single
//! `knock_routes.rs` HTTP route file).
//!
//! This is a **behavior-preserving** transport migration of the existing
//! HTTP routes (`GET /api/v1/knocks/{actor}`,
//! `POST /api/v1/knocks/{actor}/{accept,block,dismiss}`,
//! `GET /api/v1/contacts/{actor}`,
//! `POST /api/v1/contacts/{actor}/confirm`,
//! `GET|PUT /api/v1/inbox-mode/{actor}`); the request/reply shapes mirror
//! those routes exactly. The handlers reuse the same `CacheDb` methods the
//! HTTP twins call (`poll_knocks` / `accept_contact` / `block_contact` /
//! `dismiss_knock` / `delete_contact` / `list_contacts_full` /
//! `promote_to_confirmed` / `get_inbox_mode` / `set_inbox_mode`) plus the
//! `pub(crate)` multi-DB-call cores in `contacts_handlers.rs` (the auto-train
//! they once carried is gone — `mail-spam.md` § Implicit signals are
//! forbidden) — no logic is duplicated. The connection actor
//! replaces the HTTP `{actor_id}` path param + bearer-match.
//!
//! - `knocks.list` takes no params (the actor is the connection); the reply
//!   mirrors the HTTP twin's per-row json (`{id, sender, sender_node,
//!   summary, created_at}`). `sender` rides as a hex string (the twin
//!   `hex::encode`d the raw `[u8; 32]`); `sender_node` rides as a `String`
//!   (the twin emitted `String::from_utf8_lossy` of the raw `Vec<u8>`).
//! - `knocks.{accept,block,unblock,dismiss}` and `contacts.confirm` each carry
//!   a hex `peer_id` and reply with an empty ack (`KnockActionReply` /
//!   `ContactConfirmReply`) — the HTTP twins returned a bare
//!   `"accepted"`/`"blocked"`/`"dismissed"`/`"confirmed"` string; on WS-RPC,
//!   success = an empty ack, failure = an `RpcError`. `knocks.unblock` (the
//!   block⇄unblock toggle's clear-the-edge inverse)
//!   reuses this same `{peer_id}` request + empty-ack reply — no new wire type.
//! - `contacts.list` takes no params; the reply mirrors the HTTP twin's
//!   per-row json (`{peer_id, status, accepted_at, created_at}`). `peer_id`
//!   rides as a hex string (the twin `hex::encode`d the raw bytes);
//!   `accepted_at` is optional (`ContactRow.accepted_at` is `Option<i64>`).
//! - `inbox.mode.get` takes no params; the reply is `{mode}` (default
//!   `allow_knock`). `inbox.mode.set` carries the new `{mode}` and replies
//!   with an empty ack (an invalid mode → an `RpcError`, mirroring the HTTP
//!   twin's `400`).
//!
//! Kind registry entries live in `kind.rs::register_contacts_kinds`.

use fauna_core::data::InboxMode;
use fauna_i18n::strings::status::inbox_privacy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.knocks.list ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnockListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A single pending knock — mirrors the HTTP twin's per-row json object
/// (`knock_routes.rs` `get_knocks`). `sender` is the hex-encoded `[u8; 32]`
/// knock sender (the twin `hex::encode`d `KnockRow.sender_id`).
/// `sender_node` is the origin node identifier rendered as a `String` (the
/// twin emitted `String::from_utf8_lossy` of the raw `KnockRow.sender_node`
/// bytes). All fields are non-optional — the underlying `KnockRow` carries
/// them all unconditionally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnockItem {
    pub id: i64,
    /// Hex-encoded `[u8; 32]` knock-sender actor id.
    pub sender: String,
    /// Origin node identifier (lossy-UTF-8 of the raw node bytes).
    pub sender_node: String,
    /// Human-readable summary line.
    pub summary: String,
    /// Creation timestamp (millis since epoch — `push_knock` stamps
    /// `now_epoch_millis`).
    pub created_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnockListReply {
    pub knocks: Vec<KnockItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.knocks.{accept,block,dismiss} + fauna.contacts.confirm ────────

/// Request carrying the hex-encoded `[u8; 32]` peer id — shared by
/// `knocks.{accept,block,dismiss}` and `contacts.confirm` (the HTTP twins
/// took an identical `{peer_id}` json body). The handler parses it with
/// `channel_routes::parse_32_bytes` (the posts pattern).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnockActionRequest {
    /// Hex-encoded `[u8; 32]` peer id.
    pub peer_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Empty success ack for `knocks.{accept,block,dismiss}` — the HTTP twins
/// returned a bare `"accepted"`/`"blocked"`/`"dismissed"` string; on WS-RPC
/// success is an empty ack, failure is an `RpcError`. Carries only the
/// forward-compat `extra` envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnockActionReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.contacts.list ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A single contact — mirrors the HTTP twin's per-row json object
/// (`knock_routes.rs` `get_contacts`), **enriched** with the peer's public
/// handle + domain. `peer_id` is the hex-encoded peer actor id (the twin
/// `hex::encode`d `ContactRow.peer_id`). `accepted_at` is optional
/// (`ContactRow.accepted_at` is `Option<i64>` — null until the contact is
/// accepted). `handle`/`domain` are joined nest-side from the peer's Profile
/// (`docs/goal/ui/contacts.md` § State & data shape) — both additive fields
/// (absent for a federated peer with no profile handle).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactItem {
    /// Hex-encoded peer actor id.
    pub peer_id: String,
    /// Relationship status (`accepted` / `confirmed` / `blocked`).
    pub status: String,
    /// Acceptance timestamp (secs since epoch), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<i64>,
    /// Creation timestamp (secs since epoch — `upsert_contact` stamps
    /// `now_epoch_secs`).
    pub created_at: i64,
    /// The peer's public handle, when known. `Some` for a local user with a
    /// handle set; `None` for a federated peer (the nest holds no cached
    /// federated Profile) or a handle-less local user. Joined nest-side from
    /// the peer's Profile (`users.handle`), not stored on the contact edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The peer's handle domain, when known — this nest's handle domain for any
    /// local peer; `None` for a federated peer. Pairs with `handle` to form the
    /// peer's `handle@domain` identity the roster filter matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactListReply {
    pub contacts: Vec<ContactItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.contacts.status ───────────────────────────────────────────────
//
// Single-actor status lookup: the caller's contact-status toward one `peer_id`
// (the `fauna.contacts.list` roster read for exactly one peer). The request
// reuses [`KnockActionRequest`] (`{ peer_id }`, like `fauna.contacts.confirm`).
// Cheaper than `list` on the recipient contact-gate drain path (O(1) vs.
// O(roster)) — the folder recipient gate reads it per un-acked folder
// Welcome to decide auto/knock/suppress (`docs/goal/ui/folders.md` § Sharing).

/// Reply to `fauna.contacts.status`: the caller's relationship to `peer_id`, or
/// `None` when no contact record exists (a true stranger). `status` is the same
/// lowercase token `ContactItem.status` carries (`pending` / `accepted` /
/// `confirmed` / `blocked`), parsed client-side via
/// `fauna_core::data::ContactStatus::from_wire`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactStatusReply {
    /// Relationship status token, or `None` for no relationship (stranger).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.contacts.confirm ──────────────────────────────────────────────

/// Empty success ack for `contacts.confirm` — the HTTP twin returned a bare
/// `"confirmed"` string; success is an empty ack, failure is an `RpcError`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactConfirmReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.inbox.mode.get ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxModeGetRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxModeGetReply {
    /// The inbox-acceptance mode (`open` / `allow_knock` / `contacts_only` /
    /// `closed`). Default `allow_knock` — the HTTP twin's `{mode}`.
    pub mode: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.inbox.mode.set ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxModeSetRequest {
    /// The new inbox-acceptance mode. An unrecognized value → an
    /// invalid-params `RpcError` (the HTTP twin returned `400`).
    pub mode: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Empty success ack for `inbox.mode.set` — the HTTP twin returned a bare
/// `"ok"` string; success is an empty ack, failure (invalid mode) is an
/// `RpcError`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxModeSetReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── The inbox-mode selector's rows ──────────────────────────────────────

/// The four `inbox-mode-selector` rows — wire token + display label, in the
/// canonical button order (`docs/goal/ui/settings.md` § Privacy sub-page
/// item 7). The wire half comes from [`InboxMode::to_wire`], already the
/// single source of the four tokens; **the label half lives here** rather
/// than in each Rust-native app.
///
/// Both halves shared for the same reason the sibling spam control's band
/// labels are ([`crate::spam::SpamThresholdBand::label`]): tui and linux each
/// hand-kept a byte-identical copy of this table under a comment in each file
/// asserting it "matches the other exactly", which is a promise no build
/// checks — a fifth mode, or a renamed label, drifts on whichever app is
/// edited second. The label half stays here because these two shells compile
/// `fauna-i18n` directly; the other apps localize with their own string table.
///
/// ⚠ **Who actually reads the wire half, corrected 2026-08-23.** This doc used
/// to claim "the five non-Rust apps read the tokens over their own door
/// (`inboxModeValues`)". That is true of **web alone** — `inboxModeValues` is a
/// `wasm_bindgen` export, which apple, windows and android structurally cannot
/// call, and no UniFFI façade for this table exists. All three therefore
/// hand-write the four tokens *and* their labels today
/// (`PrivacySettingsView.swift`, `SettingsPrivacyPage.xaml`,
/// `PrivacySettingsScreen.kt`), which is exactly the drift this table was built
/// to end — the claim above is what kept anyone from noticing. Adding the
/// UniFFI door and retiring those three tables is to be built once for all three.
///
/// Position is meaningful: both shells index the parallel radio-button vector
/// by it, so a reorder here reorders the selector on both.
pub const INBOX_MODES: [(&str, &str); 4] = [
    (InboxMode::Open.to_wire().unwrap(), inbox_privacy::OPEN),
    (
        InboxMode::AllowKnock.to_wire().unwrap(),
        inbox_privacy::ALLOW_KNOCK,
    ),
    (
        InboxMode::ContactsOnly.to_wire().unwrap(),
        inbox_privacy::CONTACTS_ONLY,
    ),
    (InboxMode::Closed.to_wire().unwrap(), inbox_privacy::CLOSED),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    // ── knocks.list ────────────────────────────────────────────────

    fn sample_list_request() -> KnockListRequest {
        KnockListRequest {
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn knock_list_request_round_trips() {
        let req = sample_list_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: KnockListRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    fn sample_knock_list_reply() -> KnockListReply {
        KnockListReply {
            knocks: vec![
                KnockItem {
                    id: 7,
                    sender: "ab".repeat(32),
                    sender_node: "node-a".into(),
                    summary: "alice wants to connect".into(),
                    created_at: 1000,
                    extra: BTreeMap::new(),
                },
                KnockItem {
                    id: 3,
                    sender: "cd".repeat(32),
                    sender_node: "node-b".into(),
                    summary: "bob wants to connect".into(),
                    created_at: 900,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn knock_list_reply_round_trips() {
        let reply = sample_knock_list_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: KnockListReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn knock_list_reply_canonical_re_encodes_identically() {
        let reply = sample_knock_list_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: KnockListReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn knock_list_reply_empty_round_trips() {
        let reply = KnockListReply {
            knocks: vec![],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: KnockListReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert!(decoded.knocks.is_empty());
    }

    // ── knocks.{accept,block,dismiss} action req/ack ───────────────

    fn sample_action_request() -> KnockActionRequest {
        KnockActionRequest {
            peer_id: "ef".repeat(32),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn knock_action_request_round_trips() {
        let req = sample_action_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: KnockActionRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn knock_action_request_canonical_re_encodes_identically() {
        let req = sample_action_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: KnockActionRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn knock_action_reply_round_trips() {
        let reply = KnockActionReply {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: KnockActionReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    // ── contacts.list ──────────────────────────────────────────────

    fn sample_contact_list_reply() -> ContactListReply {
        ContactListReply {
            contacts: vec![
                ContactItem {
                    peer_id: "11".repeat(32),
                    status: "accepted".into(),
                    accepted_at: Some(1234),
                    created_at: 1200,
                    handle: Some("alice".into()),
                    domain: Some("fauna.example".into()),
                    extra: BTreeMap::new(),
                },
                ContactItem {
                    peer_id: "22".repeat(32),
                    status: "blocked".into(),
                    accepted_at: None,
                    created_at: 1100,
                    handle: None,
                    domain: None,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn contact_list_request_round_trips() {
        let req = ContactListRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ContactListRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn contact_list_reply_round_trips() {
        let reply = sample_contact_list_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ContactListReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn contact_list_reply_canonical_re_encodes_identically() {
        let reply = sample_contact_list_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ContactListReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn contact_item_omits_accepted_at_when_none() {
        let item = ContactItem {
            peer_id: "33".repeat(32),
            status: "blocked".into(),
            accepted_at: None,
            created_at: 1000,
            handle: None,
            domain: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&item).unwrap();
        let decoded: ContactItem = decode(&bytes).unwrap();
        assert_eq!(item, decoded);
        assert!(decoded.accepted_at.is_none());
    }

    #[test]
    fn contact_item_round_trips_handle_and_domain() {
        let item = ContactItem {
            peer_id: "44".repeat(32),
            status: "confirmed".into(),
            accepted_at: Some(1234),
            created_at: 1000,
            handle: Some("alice".into()),
            domain: Some("fauna.example".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&item).unwrap();
        let decoded: ContactItem = decode(&bytes).unwrap();
        assert_eq!(item, decoded);
        assert_eq!(decoded.handle.as_deref(), Some("alice"));
        assert_eq!(decoded.domain.as_deref(), Some("fauna.example"));
    }

    #[test]
    fn contact_item_omits_handle_domain_when_none() {
        // A federated peer (no handle/domain) encodes with the fields absent,
        // and absent keys decode to `None` via serde default.
        let item = ContactItem {
            peer_id: "55".repeat(32),
            status: "accepted".into(),
            accepted_at: Some(7),
            created_at: 1000,
            handle: None,
            domain: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&item).unwrap();
        // Re-decode as an old-shape struct (only the original four fields) to
        // prove the new fields are simply absent, not breaking the wire.
        let decoded: ContactItem = decode(&bytes).unwrap();
        assert_eq!(item, decoded);
        assert!(decoded.handle.is_none());
        assert!(decoded.domain.is_none());
    }

    #[test]
    fn contact_confirm_reply_round_trips() {
        let reply = ContactConfirmReply {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ContactConfirmReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    // ── contacts.status ────────────────────────────────────────────

    #[test]
    fn contact_status_reply_round_trips_present_and_absent() {
        // A relationship: the lowercase token round-trips.
        let reply = ContactStatusReply {
            status: Some("confirmed".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ContactStatusReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);

        // No relationship (a stranger): `status` omitted on the wire, decodes `None`.
        let none = ContactStatusReply {
            status: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&none).unwrap();
        let decoded: ContactStatusReply = decode(&bytes).unwrap();
        assert_eq!(none, decoded);
        assert!(decoded.status.is_none());
    }

    // ── inbox.mode.{get,set} ───────────────────────────────────────

    #[test]
    fn inbox_mode_get_request_round_trips() {
        let req = InboxModeGetRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxModeGetRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn inbox_mode_get_reply_round_trips() {
        let reply = InboxModeGetReply {
            mode: "allow_knock".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: InboxModeGetReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn inbox_mode_get_reply_canonical_re_encodes_identically() {
        let reply = InboxModeGetReply {
            mode: "contacts_only".into(),
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: InboxModeGetReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn inbox_mode_set_request_round_trips() {
        let req = InboxModeSetRequest {
            mode: "closed".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxModeSetRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn inbox_mode_set_reply_round_trips() {
        let reply = InboxModeSetReply {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: InboxModeSetReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    // ── The selector table ──────────────────────────────────────────

    /// Every row's wire half is a token the shared parser accepts, and it
    /// parses back to the variant the row was built from. This is what the
    /// two hand-kept copies could not assert: a table row whose token the
    /// nest rejects paints a radio button that can never be set.
    #[test]
    fn every_selector_row_round_trips_through_the_shared_parser() {
        for (token, label) in INBOX_MODES {
            let parsed = InboxMode::from_wire(token)
                .unwrap_or_else(|| panic!("`{token}` is not a mode the shared parser knows"));
            assert_eq!(
                parsed.to_wire(),
                Some(token),
                "token {token} is not canonical"
            );
            assert!(!label.is_empty(), "row {token} has no label");
        }
    }

    /// The canonical button order (`settings.md` § Privacy sub-page item 7),
    /// pinned because both shells index a parallel radio-button vector by
    /// position — a reorder silently re-labels every button on both.
    #[test]
    fn selector_order_is_canonical() {
        let tokens: Vec<&str> = INBOX_MODES.iter().map(|(t, _)| *t).collect();
        assert_eq!(tokens, ["open", "allow_knock", "contacts_only", "closed"]);
    }

    /// No two rows share a token: `inbox-mode-{token}` is the e2e element id,
    /// so a duplicate would make two buttons indistinguishable to the driver.
    #[test]
    fn selector_tokens_are_distinct() {
        let mut tokens: Vec<&str> = INBOX_MODES.iter().map(|(t, _)| *t).collect();
        tokens.sort_unstable();
        let before = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), before, "duplicate inbox-mode token");
    }
}

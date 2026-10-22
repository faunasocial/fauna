//! Web-push / APNs subscription-management WS-RPC payload types. A
//! behavior-preserving transport migration of the three push-management HTTP
//! routes (`bins/fauna-nest/src/push_routes.rs`) onto the per-actor WS-RPC
//! connection — Track B22 of the WS-RPC-everywhere migration
//! (tracked internally). The handlers reuse the same
//! `CacheDb` push-subscription methods + `PushService::vapid_public_key` the
//! twins call (no shared core — one call plus reply shaping, like
//! `stats_handlers`).
//!
//! Three kinds (api-layers.md § Push Notifications, "Migrating to WS-RPC:
//! `push.{vapidKey,subscribe,unsubscribe}`"):
//!
//! - `fauna.push.vapid_key` ≡ GET `/api/v1/push/vapid-key` — the server's VAPID
//!   public key (for `pushManager.subscribe`'s `applicationServerKey`).
//! - `fauna.push.subscribe` ≡ POST `/api/v1/push/subscribe` — register/update a
//!   device's push subscription (idempotent upsert).
//! - `fauna.push.unsubscribe` ≡ DELETE `/api/v1/push/subscribe/{device_id}` —
//!   remove a device's subscription.
//! - `fauna.push.presence` (ruled 2026-09-26, no HTTP twin) — announce which
//!   device the calling connection serves, so the nest's dispatch decision can
//!   be per device (`apps/common.md` § Registration → *Every connection
//!   announces*, § Dispatch Logic).
//!
//! The actual *push delivery* to Web Push / APNs endpoints is server-to-third-
//! party HTTP and stays as is (api-layers.md § Push Notifications); these kinds
//! cover only subscription management. Inbound notification delivery to the
//! client uses the typed `fauna.notification` push event (transport.md § Push
//! events), not this surface.
//!
//! These ride the **bearer** connection. The HTTP `vapid-key` route was
//! no-auth, but push subscription is inherently post-login (a client subscribes
//! its own device for its own actor's notifications), so there is no
//! pre-identity caller — `vapid_key` rides the authenticated connection like the
//! other two. `subscribe` / `unsubscribe` are actor-scoped on the connection
//! actor. Gate `User | Admin` (the human running the client manages their own
//! devices; an admin actor — `CallerClass::Admin`, never `User` — has devices
//! too, and must be able to fetch the VAPID key) — enforced in
//! `bridge_method_allowlist::is_permitted`.
//!
//! Wire convention (matching `web.rs` / `stats.rs`): every field is
//! `String`/`Option<String>`/`bool` — no floats; optionals are plain `Option`.
//! Every struct carries a `#[serde(flatten, default)] extra` forward-compat map.
//! Write replies are `{ ok: true }` (the twins' `204 No Content`), the
//! `WebPublishUnsetReply` precedent.
//!
//! Kind registry: `kind.rs::register_push_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.push.vapid_key (≡ GET /api/v1/push/vapid-key) ──────────────────────

/// Fetch the server's VAPID public key. No parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VapidKeyRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The base64url-encoded VAPID public key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VapidKeyReply {
    pub public_key: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.push.subscribe (≡ POST /api/v1/push/subscribe) ─────────────────────

/// Register or update a push subscription for the connection actor's device.
/// Idempotent: upserting the same `device_id` is safe. `transport` is
/// `"web-push"` (default if omitted), `"apns"` or `"ws-device"`; for
/// `web-push`, `key_p256dh` and `key_auth` are required (the handler
/// validates). A `ws-device` row (linux, windows, tui — delivered over the
/// device's own live connection as `fauna.push.notification`) carries its own
/// `device_id` as the `endpoint` and no keys: nothing leaves the nest's
/// authenticated channel, so there is nothing to encrypt against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SubscribeRequest {
    pub device_id: String,
    pub endpoint: String,
    pub key_p256dh: Option<String>,
    pub key_auth: Option<String>,
    pub transport: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `{ ok: true }` on success (the twin returned 204 No Content).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SubscribeReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.push.unsubscribe (≡ DELETE /api/v1/push/subscribe/{device_id}) ─────

/// Remove the connection actor's push subscription for `device_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UnsubscribeRequest {
    pub device_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `{ ok: true }` on success (the twin returned 204 No Content).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UnsubscribeReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.push.presence (ruled 2026-09-26) ───────────────────────────────────

/// Announce which device the calling connection serves: the same `device_id`
/// the device's subscription row uses (the install's derived id for this
/// actor). Tags the connection itself — a later announce replaces an earlier
/// one — and lasts as long as the connection. Sent once per (re)connect by the
/// shared client. Client-asserted on purpose: all a connection can do with it
/// is suppress or receive its own actor's push. A connection that never
/// announces (before the announce, or a non-app connection) keeps the relay
/// skip: its presence suppresses every relay push.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PresenceRequest {
    pub device_id: String,
    /// `true` when the announcing connection is the device's wake stand-in
    /// (the sync agent), not an app (ruled 2026-10-03). A stand-in's announce
    /// is a delivery point for the device's `ws-device` row and nothing else:
    /// it never makes the device present for `web-push`/`apns` and never
    /// triggers the compatibility rider (`apps/common.md` § Push Notifications
    /// → *Dispatch Logic*). Absent means an app, so a plain announce encodes
    /// byte-for-byte as before the field existed, and an older nest reads a
    /// stand-in's announce as a plain one (`version-compatibility.md` § I4).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stand_in: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `{ ok: true }` on success.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PresenceReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── The web-push payload (the plaintext inside RFC 8291 aes128gcm) ───────────

/// The title every `web-push` payload and `ws-device` frame carries — generic
/// on purpose: a relay must not read the content (`apps/common.md` § Content
/// Encryption). One owner for the nest that writes it and the app that falls
/// back to it.
pub const GENERIC_TITLE: &str = "Fauna";
/// The body twin of [`GENERIC_TITLE`].
pub const GENERIC_BODY: &str = "New message";

/// The JSON object a `web-push` delivery encrypts — what the nest writes and
/// what an app that decrypts its own Web Push messages (android through its
/// UnifiedPush connector; a browser's service worker) reads back. JSON, not
/// CBOR: it is the Web Push convention a service worker's `event.data.json()`
/// reads. `url` is the deep link a tap opens, absent today on `web-push`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebPushPayload {
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A newer sender's keys, kept rather than dropped (transport.md § Schema
    /// and forward-compat discipline, rule 4). JSON, like the payload.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl WebPushPayload {
    /// The generic payload every `web-push` delivery carries.
    pub fn generic() -> Self {
        Self {
            title: GENERIC_TITLE.to_string(),
            body: GENERIC_BODY.to_string(),
            url: None,
            extra: BTreeMap::new(),
        }
    }

    /// The JSON bytes the nest encrypts.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a struct of strings always serializes")
    }

    /// Read a decrypted payload. Anything that is not the object above — a
    /// sender newer than this reader, a relay's own test message — reads as
    /// [`Self::generic`]: a notification still shows, and nothing a relay
    /// wrote is shown as Fauna's words.
    pub fn parse_or_generic(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_else(|_| Self::generic())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn the_generic_web_push_payload_is_the_bytes_the_nest_has_always_sent() {
        assert_eq!(
            WebPushPayload::generic().to_json(),
            br#"{"title":"Fauna","body":"New message"}"#.to_vec()
        );
    }

    #[test]
    fn a_web_push_payload_reads_back_with_its_url() {
        let sent = WebPushPayload {
            url: Some("fauna://inbox".into()),
            ..WebPushPayload::generic()
        };
        assert_eq!(WebPushPayload::parse_or_generic(&sent.to_json()), sent);
        // A newer sender's extra field is kept, not a parse failure.
        let newer = br#"{"title":"T","body":"B","tag":"x"}"#;
        let read = WebPushPayload::parse_or_generic(newer);
        assert_eq!(read.title, "T");
        assert_eq!(read.extra.get("tag"), Some(&serde_json::json!("x")));
    }

    #[test]
    fn an_unreadable_web_push_payload_reads_as_the_generic_one() {
        for bytes in [&b""[..], b"not json", br#"{"title":1}"#] {
            assert_eq!(
                WebPushPayload::parse_or_generic(bytes),
                WebPushPayload::generic()
            );
        }
    }

    #[test]
    fn presence_round_trips() {
        assert_round_trips(&PresenceRequest {
            device_id: "dev-1".into(),
            ..Default::default()
        });
        assert_round_trips(&PresenceRequest {
            device_id: "dev-1".into(),
            stand_in: true,
            ..Default::default()
        });
        assert_round_trips(&PresenceReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }

    /// The stand-in flag is additive (`version-compatibility.md` § I4): a plain
    /// announce encodes exactly as the field-less shape did, and that shape
    /// decodes as an app announce.
    #[test]
    fn a_plain_presence_announce_encodes_without_the_stand_in_field() {
        #[derive(Serialize)]
        struct Legacy {
            device_id: String,
        }
        let legacy = encode_canonical(&Legacy {
            device_id: "dev-1".into(),
        })
        .unwrap();
        let plain = PresenceRequest {
            device_id: "dev-1".into(),
            ..Default::default()
        };
        assert_eq!(encode_canonical(&plain).unwrap(), legacy);
        let decoded: PresenceRequest = decode(&legacy).unwrap();
        assert!(!decoded.stand_in);
        assert!(decoded.extra.is_empty());
    }

    #[test]
    fn vapid_key_round_trips() {
        assert_round_trips(&VapidKeyRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&VapidKeyReply {
            public_key: "BPaX...base64url".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn subscribe_round_trips_web_push_and_apns() {
        // Web-push: keys present.
        assert_round_trips(&SubscribeRequest {
            device_id: "dev-1".into(),
            endpoint: "https://push.example.com/abc".into(),
            key_p256dh: Some("p256dh".into()),
            key_auth: Some("auth".into()),
            transport: Some("web-push".into()),
            extra: BTreeMap::new(),
        });
        // APNs: keys absent (Option → null → round-trips to None), transport
        // omitted (defaulted server-side).
        let apns = SubscribeRequest {
            device_id: "dev-2".into(),
            endpoint: "deadbeef".into(),
            key_p256dh: None,
            key_auth: None,
            transport: Some("apns".into()),
            extra: BTreeMap::new(),
        };
        let decoded: SubscribeRequest = decode(&encode_canonical(&apns).unwrap()).unwrap();
        assert_eq!(apns, decoded);
        assert!(decoded.key_p256dh.is_none());
        assert_round_trips(&SubscribeReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn unsubscribe_round_trips() {
        assert_round_trips(&UnsubscribeRequest {
            device_id: "dev-1".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&UnsubscribeReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn presence_round_trips() {
        assert_round_trips(&PresenceRequest {
            device_id: "dev-1".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&PresenceReply {
            ok: true,
            extra: BTreeMap::new(),
        });
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

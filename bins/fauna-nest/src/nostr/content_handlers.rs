//! WS-RPC handlers for the protocol-native Nostr content surfaces —
//! `nostr.{zaps.total,badges.list,events.publish_signed}`, the successors to
//! the deleted `/api/v1/nostr/{zaps,badges,publish-signed}` HTTP routes (the
//! native-content HTTP→WS-RPC rip, user-directed 2026-07-22;
//! `docs/goal/ui/nostr.md` § WS-RPC migration contract). Prefix-less
//! `nostr.*` kinds per the `bluesky.feed.thread` precedent
//! (`fauna_protocol::nostr`'s module doc owns the naming split).
//!
//! All three are **User-class** (`bridge_method_allowlist`); the reads are
//! caller-authenticated but not caller-scoped — zap totals and badge lists
//! are public-by-construction Nostr facts (signed events any relay serves),
//! exactly as the HTTP twins served them to any bearer. `publish_signed` is
//! the NIP-07 leg: the **client's browser extension** signed the event, so
//! the nest verifies the signature, checks the pubkey is the caller's linked
//! account, and relay-enqueues — it never holds a key for this account.
//!
//! One deliberate behavior change vs the HTTP twin: the twin swallowed a
//! failed relay-queue enqueue (`let _ = sync_tx.send(..)` under a 202), so a
//! publish could silently vanish. Here a failed enqueue is a loud `internal`
//! error (testing.md convention 11 — never silently drop a command).

use std::time::Duration;

use fauna_bridge_nostr::signing::verify_event;

use fauna_protocol::nostr::{
    ListNostrBadgesReply, ListNostrBadgesRequest, NostrBadgeItem, PublishSignedNostrEventReply,
    PublishSignedNostrEventRequest,
};
// The zap-total wire pair rides the `zaps` member with its handler. (The types
// themselves are still unconditional in `fauna-protocol` — the shared-crate half
// of the `zaps` retrofit.)
#[cfg(feature = "zaps")]
use fauna_protocol::nostr::{NostrZapTotalReply, NostrZapTotalRequest};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::nostr::db;
use crate::nostr::relays::resolve_relay_urls;
use crate::nostr::sync_worker::OutboundEvent;
use crate::rpc_errors::internal;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers (the per-module convention) ─────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("nostr", reason)
}

/// The WS-RPC analogue of the HTTP twin's `400 bad_request` surface
/// (unparseable event JSON, invalid signature).
fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("nostr", reason)
}

/// The account's relay list resolved to an explicit empty set — the user
/// removed every relay, so there is nowhere to publish. Never silently
/// substitutes [`crate::nostr::relays::DEFAULT_RELAYS`] (the explicit-empty
/// semantic flip, `relays.rs` module doc): the caller must retry after adding
/// a relay, matching `publish_signed`'s "never silently drop a command"
/// discipline for a failed enqueue.
fn no_relays_configured() -> RpcError {
    crate::rpc_errors::no_relays_configured_ns("nostr")
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── nostr.zaps.total ────────────────────────────────────────────────

// The zap-total display read — a `zaps` member surface (`dynamic-features.md`
// § Charter members, receive side: "zap-total display (`nostr.zaps.total`)").
#[cfg(feature = "zaps")]
fn zap_total_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "nostr.zaps.total").await?;
            let req: NostrZapTotalRequest = decode(&payload).map_err(malformed)?;

            let conn = state.db.conn().await;
            let (total_msats, zap_count) =
                db::get_zap_total(&conn, &req.event_id).map_err(internal)?;
            drop(conn);

            encode_reply(&NostrZapTotalReply {
                total_msats,
                zap_count,
                extra: Default::default(),
            })
        })
    })
}

// ── nostr.badges.list ───────────────────────────────────────────────

fn badges_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "nostr.badges.list").await?;
            let req: ListNostrBadgesRequest = decode(&payload).map_err(malformed)?;

            let conn = state.db.conn().await;
            let badges = db::list_badges_for_pubkey(&conn, &req.pubkey).map_err(internal)?;
            drop(conn);

            let badges = badges
                .into_iter()
                .map(|b| NostrBadgeItem {
                    badge_id: b.badge_id,
                    badge_name: b.badge_name,
                    badge_image: b.badge_image,
                    created_at: b.created_at,
                    extra: Default::default(),
                })
                .collect();
            encode_reply(&ListNostrBadgesReply {
                badges,
                extra: Default::default(),
            })
        })
    })
}

// ── nostr.events.publish_signed ─────────────────────────────────────
//
// The full flow of the deleted HTTP `publish_signed`, verbatim except for the
// loud-enqueue change (module doc): 1) parse the NIP-01 wire JSON, 2) verify
// the event signature, 3) require the event pubkey to be the caller's linked
// account (the caller may only publish as themself), 4) resolve the account's
// relay list, 5) enqueue on the outbound sync channel.

fn publish_signed_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "nostr.events.publish_signed").await?;
            let req: PublishSignedNostrEventRequest = decode(&payload).map_err(malformed)?;

            let event: fauna_bridge_nostr::types::Event = serde_json::from_str(&req.event_json)
                .map_err(|e| invalid_params(&format!("malformed event JSON: {e}")))?;

            if !verify_event(&event) {
                return Err(invalid_params("invalid event signature"));
            }

            let actor_hex = hex::encode(actor_id);
            let conn = state.db.conn().await;
            let acct = db::get_account(&conn, &actor_hex).map_err(internal)?;
            drop(conn);

            let relay_list = match &acct {
                Some(a) if a.nostr_pubkey == event.pubkey => a.relay_list.clone(),
                _ => {
                    return Err(permission_denied(
                        "event pubkey doesn't match linked account",
                    ));
                }
            };

            let relay_urls = resolve_relay_urls(relay_list.as_deref());
            if relay_urls.is_empty() {
                return Err(no_relays_configured());
            }
            state
                .nostr
                .sync_tx
                .send(OutboundEvent { event, relay_urls })
                .await
                .map_err(|e| internal(format!("relay enqueue: {e}")))?;

            encode_reply(&PublishSignedNostrEventReply {
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────

pub fn register_nostr_content_handlers(b: &mut RpcRouterBuilder) {
    let read = || Duration::from_secs(5);
    #[cfg(feature = "zaps")]
    b.add(
        "nostr.zaps.total",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: zap_total_handler(),
        },
    );
    b.add(
        "nostr.badges.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: read(),
            handler: badges_list_handler(),
        },
    );
    // `publish_signed` forbids replay at 30 s — it relay-enqueues a one-shot
    // outbound event (the `fauna.email.send` shape; a replayed envelope
    // must not re-broadcast).
    b.add(
        "nostr.events.publish_signed",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: Duration::from_secs(30),
            handler: publish_signed_handler(),
        },
    );
}

// ── Tests (port of the deleted account_routes HTTP tests) ───────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;
    use crate::state::NostrState;
    use bytes::Bytes;
    use fauna_bridge_nostr::signing::Keypair;
    use fauna_bridge_nostr::types::UnsignedEvent;
    use fauna_protocol::encode_canonical;
    use std::sync::Arc;

    async fn build_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::nostr::init_db(&db).await.expect("init_db");
        Arc::new(AppState::for_test(db))
    }

    /// Seed the `users` row that resolves the actor to `CallerClass::User`
    /// (the WS-RPC twin of the HTTP tests' bearer token).
    async fn seed_user(state: &Arc<AppState>, actor: [u8; 32], handle: &str) {
        state
            .db
            .create_user_with_handle(&actor, "personal", handle, None)
            .await
            .expect("seed user");
    }

    fn encode_req<T: serde::Serialize>(req: &T) -> Bytes {
        Bytes::from(encode_canonical(req).expect("encode req").to_vec())
    }

    fn decode_reply<T: serde::de::DeserializeOwned>(b: &Bytes) -> T {
        decode(b).expect("decode reply")
    }

    fn signed_event_json(kp: &Keypair, content: &str) -> String {
        let event = kp.sign_event(UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at: 1000,
            kind: 1,
            tags: vec![],
            content: content.to_string(),
        });
        serde_json::to_string(&event).expect("event json")
    }

    #[tokio::test]
    async fn zap_total_requires_a_known_actor() {
        let state = build_state().await;
        // No users row → the central gate's UNKNOWN-ACTOR arm (the HTTP twin's
        // 401 class). That arm answers `fauna.bridges.permission_denied` on
        // EVERY kind, never the kind's own family — the every-kind shape the Go
        // bridges' revocation probe keys on (`api-layers.md` § Caller-class
        // authorization → Refusal codes at the gate, ruled 2026-08-17). The
        // nostr family code below is for the finer WITHIN-class refusals
        // (`publish_signed_rejects_pubkey_not_linked_to_actor`), which this
        // caller never reaches — it is refused before any class is resolved.
        let err = zap_total_handler()(
            state,
            [0x10u8; 32],
            encode_req(&NostrZapTotalRequest {
                event_id: "evt1".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("unknown actor must be rejected");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    #[tokio::test]
    async fn zap_total_sums_seeded_zaps() {
        let state = build_state().await;
        let actor = [0x11u8; 32];
        seed_user(&state, actor, "zapreader").await;
        {
            let conn = state.db.conn().await;
            db::insert_zap(
                &conn,
                "zap1",
                Some("evt1"),
                "target_pk",
                Some("sender_pk"),
                Some(1000),
                1,
                None,
            )
            .unwrap();
            db::insert_zap(
                &conn,
                "zap2",
                Some("evt1"),
                "target_pk",
                Some("sender_pk"),
                Some(2000),
                2,
                None,
            )
            .unwrap();
        }

        let bytes = zap_total_handler()(
            state,
            actor,
            encode_req(&NostrZapTotalRequest {
                event_id: "evt1".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("zap total");
        let reply: NostrZapTotalReply = decode_reply(&bytes);
        assert_eq!(reply.total_msats, 3000);
        assert_eq!(reply.zap_count, 2);
    }

    #[tokio::test]
    async fn zap_total_unknown_event_returns_zero() {
        let state = build_state().await;
        let actor = [0x12u8; 32];
        seed_user(&state, actor, "zapzero").await;

        let bytes = zap_total_handler()(
            state,
            actor,
            encode_req(&NostrZapTotalRequest {
                event_id: "never-seen".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("zap total");
        let reply: NostrZapTotalReply = decode_reply(&bytes);
        assert_eq!(reply.total_msats, 0);
        assert_eq!(reply.zap_count, 0);
    }

    #[tokio::test]
    async fn badges_list_returns_seeded_badges_newest_first() {
        let state = build_state().await;
        let actor = [0x13u8; 32];
        seed_user(&state, actor, "badgereader").await;
        let pubkey = "awardee_pk";
        {
            let conn = state.db.conn().await;
            db::insert_badge(
                &conn,
                "badge1",
                Some("Early Adopter"),
                Some("https://x/1.png"),
                pubkey,
                1,
            )
            .unwrap();
            db::insert_badge(
                &conn,
                "badge2",
                Some("Verified"),
                Some("https://x/2.png"),
                pubkey,
                2,
            )
            .unwrap();
        }

        let bytes = badges_list_handler()(
            state,
            actor,
            encode_req(&ListNostrBadgesRequest {
                pubkey: pubkey.into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("badges list");
        let reply: ListNostrBadgesReply = decode_reply(&bytes);
        assert_eq!(reply.badges.len(), 2);
        assert_eq!(reply.badges[0].badge_id, "badge2", "newest first");
        assert_eq!(reply.badges[0].badge_name.as_deref(), Some("Verified"));
    }

    #[tokio::test]
    async fn publish_signed_rejects_tampered_signature() {
        let state = build_state().await;
        let actor = [0x14u8; 32];
        seed_user(&state, actor, "tamperer").await;
        let kp = Keypair::generate();
        let mut event: fauna_bridge_nostr::types::Event =
            serde_json::from_str(&signed_event_json(&kp, "hello")).unwrap();
        event.content = "tampered".to_string();

        let err = publish_signed_handler()(
            state,
            actor,
            encode_req(&PublishSignedNostrEventRequest {
                event_json: serde_json::to_string(&event).unwrap(),
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("tampered signature must be rejected");
        assert_eq!(err.code, "fauna.nostr.invalid_params");
    }

    #[tokio::test]
    async fn publish_signed_rejects_pubkey_not_linked_to_actor() {
        let state = build_state().await;
        let actor = [0x15u8; 32];
        seed_user(&state, actor, "unlinked").await;
        // Actor has no linked nostr account at all.
        let kp = Keypair::generate();

        let err = publish_signed_handler()(
            state,
            actor,
            encode_req(&PublishSignedNostrEventRequest {
                event_json: signed_event_json(&kp, "hello"),
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("unlinked pubkey must be rejected");
        assert_eq!(err.code, "fauna.nostr.permission_denied");
    }

    #[tokio::test]
    async fn publish_signed_accepts_and_enqueues_for_linked_account() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::nostr::init_db(&db).await.expect("init_db");
        // Keep a live outbound receiver so the enqueue can be observed (the
        // deleted HTTP twin swallowed enqueue failures; this handler errors
        // loudly, so the test state needs a real channel).
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let nostr = NostrState {
            sync_tx: tx,
            ..NostrState::default()
        };
        let state = Arc::new(AppState {
            nostr,
            ..AppState::for_test(db)
        });
        let actor = [0x16u8; 32];
        seed_user(&state, actor, "publisher").await;
        let actor_hex = hex::encode(actor);
        let kp = Keypair::generate();
        {
            let conn = state.db.conn().await;
            db::link_account(
                &conn,
                &actor_hex,
                &kp.public_key_hex(),
                "generated",
                None,
                None,
                None,
            )
            .unwrap();
        }

        let bytes = publish_signed_handler()(
            state,
            actor,
            encode_req(&PublishSignedNostrEventRequest {
                event_json: signed_event_json(&kp, "hello"),
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish accepted");
        let _reply: PublishSignedNostrEventReply = decode_reply(&bytes);

        let outbound = rx.try_recv().expect("event enqueued to the sync worker");
        assert_eq!(outbound.event.pubkey, kp.public_key_hex());
        assert_eq!(outbound.event.content, "hello");
    }

    /// P2.7 regression: a NULL `relay_list` used to resolve to an empty
    /// `Vec<String>` (`relay_list.as_deref().and_then(serde_json::from_str)
    /// .unwrap_or_default()`), so the event enqueued loudly then
    /// `sync_worker::handle_outbound` iterated zero relays — the publish
    /// silently vanished. `resolve_relay_urls` never returns empty: it must
    /// fall back to `DEFAULT_RELAYS`.
    #[tokio::test]
    async fn publish_signed_null_relay_list_falls_back_to_default_relays() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::nostr::init_db(&db).await.expect("init_db");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let nostr = NostrState {
            sync_tx: tx,
            ..NostrState::default()
        };
        let state = Arc::new(AppState {
            nostr,
            ..AppState::for_test(db)
        });
        let actor = [0x17u8; 32];
        seed_user(&state, actor, "defaultrelaypublisher").await;
        let actor_hex = hex::encode(actor);
        let kp = Keypair::generate();
        {
            let conn = state.db.conn().await;
            // `relay_list: None` — the account was linked without ever
            // configuring a relay list (the NULL-column case).
            db::link_account(
                &conn,
                &actor_hex,
                &kp.public_key_hex(),
                "generated",
                None,
                None,
                None,
            )
            .unwrap();
        }

        let bytes = publish_signed_handler()(
            state,
            actor,
            encode_req(&PublishSignedNostrEventRequest {
                event_json: signed_event_json(&kp, "hello"),
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish accepted");
        let _reply: PublishSignedNostrEventReply = decode_reply(&bytes);

        let outbound = rx.try_recv().expect("event enqueued to the sync worker");
        assert_eq!(
            outbound.relay_urls,
            crate::nostr::relays::DEFAULT_RELAYS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            "NULL relay_list must resolve to DEFAULT_RELAYS, never an empty vec"
        );
    }

    /// Row 15 — the explicit-empty semantic flip: a user who removed every
    /// relay (`relay_list: Some("[]")`) must be refused loudly, never
    /// silently fall back to `DEFAULT_RELAYS` and publish to relays they
    /// removed. Twin of `publish_signed_null_relay_list_falls_back_to_default_relays`
    /// above, opposite assertion.
    #[tokio::test]
    async fn publish_signed_explicit_empty_relay_list_is_refused() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::nostr::init_db(&db).await.expect("init_db");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let nostr = NostrState {
            sync_tx: tx,
            ..NostrState::default()
        };
        let state = Arc::new(AppState {
            nostr,
            ..AppState::for_test(db)
        });
        let actor = [0x18u8; 32];
        seed_user(&state, actor, "norelayspublisher").await;
        let actor_hex = hex::encode(actor);
        let kp = Keypair::generate();
        {
            let conn = state.db.conn().await;
            // The user removed every relay — an explicit `[]`, not NULL.
            db::link_account(
                &conn,
                &actor_hex,
                &kp.public_key_hex(),
                "generated",
                None,
                None,
                Some("[]"),
            )
            .unwrap();
        }

        let err = publish_signed_handler()(
            state,
            actor,
            encode_req(&PublishSignedNostrEventRequest {
                event_json: signed_event_json(&kp, "hello"),
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("an explicitly empty relay list must be refused, not silently defaulted");
        assert_eq!(err.code, "fauna.nostr.no_relays_configured");

        assert!(
            rx.try_recv().is_err(),
            "nothing must be enqueued to the sync worker on refusal"
        );
    }
}

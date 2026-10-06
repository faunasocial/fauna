//! WS-RPC handlers for the task-delegation heartbeat lease —
//! `fauna.delegation.{heartbeat,observe}` (+ the `fauna.delegation.lease_changed`
//! push). `docs/goal/behavior/participants.md` § Coordination primitive
//! (design tracked internally).
//!
//! The nest is a dumb per-actor last-writer-wins blackboard
//! ([`crate::delegation_registry::LeaseRegistry`]): `heartbeat` records the
//! caller's **self-reported** `holder` / `holder_class` unconditionally and,
//! when the holder *changes*, fans a best-effort `lease_changed` "re-observe"
//! nudge to the actor's connections; `observe` returns the current snapshot.
//! No CAS, no epoch — all convergence logic is client-side
//! (`fauna_core::delegation::decide`).
//!
//! **Only the map KEY is authenticated.** The owning actor is the connection's
//! actor (below), but `req.holder` and `req.holder_class` are taken verbatim —
//! a caller can name any device and any [`ParticipantClass`]. That adds no
//! capability (a lying holder can only mis-describe leases in its *own* actor's
//! blackboard, which it may already write freely), but it does mean a consumer
//! must never treat `holder_class` as a nest-attested fact. The first consumer,
//! `LeaseCoordinator`'s co-tier/`AlwaysOnNest` arms, is safe precisely because
//! the equivalent forgery was already reachable; the next one should re-derive
//! that argument rather than inherit it.
//!
//! Self-scoped: the owning actor is the authenticated connection actor (neither
//! kind carries an actor_id), so a caller reads/writes only their own leases —
//! and all a user's participants share that one actor, which is exactly the
//! slices-2-3 scope. Caller-class gate (`User | Admin`) lives in
//! `bridge_method_allowlist::is_permitted`.

use std::time::Duration;

use fauna_protocol::delegation::{
    HeartbeatReply, HeartbeatRequest, KIND_HEARTBEAT, KIND_OBSERVE, ObserveReply, ObserveRequest,
};
use fauna_protocol::push_events::LeaseChangedPayload;
use fauna_protocol::{PushEvent, RpcError, decode_strict as decode};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

#[cfg(test)]
use crate::routes::AppState;
#[cfg(test)]
use std::sync::Arc;

// ── Helpers (mirror mls_replica_handlers.rs) ────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// Only a kind on the shared list may hold a slot — any
/// other string would grow the in-memory blackboard without bound. A newer
/// client naming a kind this nest predates gets the ordinary `malformed`
/// (Rejected-class) answer (`version-compatibility.md`).
fn validate_task_kind(task_kind: &str) -> Result<&'static str, RpcError> {
    if task_kind.is_empty() {
        return Err(malformed("task_kind must not be empty"));
    }
    fauna_core::delegation::live_task_kind(task_kind)
        .ok_or_else(|| malformed("task_kind is not a known delegation task kind"))
}

// ── fauna.delegation.heartbeat ──────────────────────────────────────────────

fn heartbeat_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_HEARTBEAT).await?;
            let req: HeartbeatRequest = decode(&payload).map_err(malformed)?;
            let task_kind = validate_task_kind(&req.task_kind)?;
            let (lease, holder_changed) = state
                .delegation_leases
                .heartbeat(actor_id, task_kind, req.holder, req.holder_class)
                .ok_or_else(|| malformed("task_kind is not a known delegation task kind"))?;
            // A holder change (first claim / takeover) nudges the actor's other
            // clients to re-observe promptly. Best-effort — fanned to all the
            // actor's connections (including the caller, which ignores its own
            // echo); the observe poll is the backstop. Renews are silent.
            if holder_changed {
                state.ws.notify_push(
                    &actor_id,
                    PushEvent::LeaseChanged(LeaseChangedPayload {
                        task_kind: task_kind.to_string(),
                        extra: Default::default(),
                    }),
                );
            }
            encode_reply(&HeartbeatReply {
                lease,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.delegation.observe ────────────────────────────────────────────────

fn observe_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_OBSERVE).await?;
            let req: ObserveRequest = decode(&payload).map_err(malformed)?;
            let leases = state.delegation_leases.observe(actor_id, &req.task_kinds);
            encode_reply(&ObserveReply {
                leases,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

pub fn register_delegation_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_HEARTBEAT,
        RpcKindMeta {
            // Last-writer-wins write of the caller's own lease — replaying it
            // just re-records the same holder (idempotent).
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: heartbeat_handler(),
        },
    );
    b.add(
        KIND_OBSERVE,
        RpcKindMeta {
            // Pure read of the calling actor's lease snapshot.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: observe_handler(),
        },
    );
}

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
use fauna_protocol::encode_canonical;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_core::data::ParticipantRef;
    use fauna_core::delegation::ParticipantClass;
    use fauna_protocol::delegation::LeaseState;

    async fn fixture() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        Arc::new(AppState::for_test(db))
    }

    fn dev(id: &str) -> ParticipantRef {
        ParticipantRef::Device {
            device_id: id.to_string(),
        }
    }

    fn heartbeat_req(kind: &str, holder: ParticipantRef, class: ParticipantClass) -> Bytes {
        Bytes::from(
            encode_canonical(&HeartbeatRequest {
                task_kind: kind.to_string(),
                holder,
                holder_class: class,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    fn observe_req(kinds: &[&str]) -> Bytes {
        Bytes::from(
            encode_canonical(&ObserveRequest {
                task_kinds: kinds.iter().map(|s| s.to_string()).collect(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    async fn observe(state: &Arc<AppState>, actor: [u8; 32], kinds: &[&str]) -> Vec<LeaseState> {
        let bytes = observe_handler()(state.clone(), actor, observe_req(kinds))
            .await
            .expect("observe ok");
        decode::<ObserveReply>(&bytes).unwrap().leases
    }

    #[tokio::test]
    async fn heartbeat_then_observe_reflects_holder() {
        let state = fixture().await;
        let actor = [5u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        let reply = heartbeat_handler()(
            state.clone(),
            actor,
            heartbeat_req(
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
            ),
        )
        .await
        .expect("heartbeat ok");
        let reply: HeartbeatReply = decode(&reply).unwrap();
        assert_eq!(reply.lease.holder, dev("dev-a"));
        assert_eq!(reply.lease.age_ms, 0);

        let leases = observe(&state, actor, &[]).await;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].holder, dev("dev-a"));
        assert_eq!(leases[0].task_kind, "backup-upload");
    }

    #[tokio::test]
    async fn second_device_takes_over_via_lww() {
        let state = fixture().await;
        let actor = [5u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        heartbeat_handler()(
            state.clone(),
            actor,
            heartbeat_req(
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
            ),
        )
        .await
        .unwrap();
        heartbeat_handler()(
            state.clone(),
            actor,
            heartbeat_req(
                "backup-upload",
                dev("dev-b"),
                ParticipantClass::PluggedInDesktop,
            ),
        )
        .await
        .unwrap();
        let leases = observe(&state, actor, &["backup-upload"]).await;
        assert_eq!(leases[0].holder, dev("dev-b"), "last writer wins");
    }

    #[tokio::test]
    async fn observe_is_actor_scoped() {
        let state = fixture().await;
        state
            .db
            .create_user(&[1u8; 32], "free", "test")
            .await
            .unwrap();
        state
            .db
            .create_user(&[2u8; 32], "free", "test")
            .await
            .unwrap();
        heartbeat_handler()(
            state.clone(),
            [1u8; 32],
            heartbeat_req(
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
            ),
        )
        .await
        .unwrap();
        // A different actor sees none of actor-1's leases.
        assert!(observe(&state, [2u8; 32], &[]).await.is_empty());
    }

    #[tokio::test]
    async fn empty_task_kind_is_rejected() {
        let state = fixture().await;
        state
            .db
            .create_user(&[5u8; 32], "free", "test")
            .await
            .unwrap();
        let err = heartbeat_handler()(
            state.clone(),
            [5u8; 32],
            heartbeat_req("", dev("dev-a"), ParticipantClass::PluggedInDesktop),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    /// The blackboard holds the shared kind list and nothing else: a
    /// loop of fresh `task_kind` strings is refused one by one and never grows
    /// the lease map, so the map stays at most `LIVE_TASK_KINDS.len()` slots per
    /// actor however long the loop runs.
    #[tokio::test]
    async fn unknown_task_kind_is_rejected_and_never_grows_the_map() {
        let state = fixture().await;
        let actor = [5u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        for i in 0..64 {
            let kind = format!("junk-{i}-{}", "x".repeat(i * 64));
            let err = heartbeat_handler()(
                state.clone(),
                actor,
                heartbeat_req(&kind, dev("dev-a"), ParticipantClass::PluggedInDesktop),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "fauna.protocol.malformed");
        }
        assert_eq!(state.delegation_leases.lease_count(), 0);

        // Every known kind is still accepted, one slot each.
        for spec in fauna_core::delegation::LIVE_TASK_KINDS {
            heartbeat_handler()(
                state.clone(),
                actor,
                heartbeat_req(spec.kind, dev("dev-a"), ParticipantClass::PluggedInDesktop),
            )
            .await
            .expect("a live kind heartbeats");
        }
        assert_eq!(
            state.delegation_leases.lease_count(),
            fauna_core::delegation::LIVE_TASK_KINDS.len()
        );

        // An observe naming unknown kinds (however many) answers only the
        // known ones it also names.
        let mut kinds: Vec<String> = (0..10_000).map(|i| format!("junk-{i}")).collect();
        kinds.push(fauna_core::delegation::KIND_INDEX.to_string());
        let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
        let leases = observe(&state, actor, &kinds).await;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].task_kind, fauna_core::delegation::KIND_INDEX);
    }

    #[tokio::test]
    async fn heartbeat_holder_change_emits_lease_changed_push() {
        let state = fixture().await;
        let actor = [5u8; 32];
        state.db.create_user(&actor, "free", "test").await.unwrap();
        // Subscribe a second connection for the actor to receive the push.
        let (_conn, mut rx) = state.ws.subscribe(actor);

        // First claim → holder change → push.
        heartbeat_handler()(
            state.clone(),
            actor,
            heartbeat_req(
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
            ),
        )
        .await
        .unwrap();
        let bytes = rx.try_recv().expect("first claim pushes lease_changed");
        let frame = fauna_protocol::decode_frame(&bytes).unwrap();
        match frame {
            fauna_protocol::Frame::Push(p) => {
                assert_eq!(p.kind, "fauna.delegation.lease_changed");
                let payload_bytes = encode_canonical(&p.payload).unwrap();
                let payload: LeaseChangedPayload =
                    fauna_cbor::decode_strict(&payload_bytes).unwrap();
                assert_eq!(payload.task_kind, "backup-upload");
            }
            _ => panic!("expected Push"),
        }

        // A renew by the same holder → no change → no push.
        heartbeat_handler()(
            state.clone(),
            actor,
            heartbeat_req(
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
            ),
        )
        .await
        .unwrap();
        assert!(rx.try_recv().is_err(), "a renew is silent");

        // A takeover by dev-b → change → push.
        heartbeat_handler()(
            state.clone(),
            actor,
            heartbeat_req(
                "backup-upload",
                dev("dev-b"),
                ParticipantClass::PluggedInDesktop,
            ),
        )
        .await
        .unwrap();
        assert!(rx.try_recv().is_ok(), "a takeover pushes lease_changed");
    }
}

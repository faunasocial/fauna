//! Typed-call wrapper for the task-delegation heartbeat-lease WS-RPC kinds —
//! `fauna.delegation.{heartbeat,observe}`. The transport half of the advisory
//! lease that makes `fauna_core::delegation::current_candidates`
//! exactly-one-runner at runtime (`docs/goal/behavior/participants.md`
//! § Coordination primitive; design tracked internally).
//!
//! Pattern: same shape as the sibling per-feature client wrappers
//! (`fauna-client-spam`, `-search`, `-conversations`) — a thin
//! `pub struct DelegationClient { nest: R }`, one async method per kind, no
//! state machine. Generic over the WS-RPC transport (`R: RpcRequester`): native
//! call sites pass `Arc<NestClient>`, the wasm SPA passes its `WsRpcClient`.
//!
//! The **decision** logic — whether to `heartbeat` (as holder) or stand by, and
//! whether the observed lease is stale — is the pure
//! `fauna_core::delegation::decide` (re-exported here for the loop's
//! convenience). The lease loop itself is [`LeaseCoordinator`] (this crate's
//! `coordinator` module): build the candidate set (`current_candidates`) →
//! [`DelegationClient::observe`] → `decide` → on `Acquire`/`Renew` call
//! [`DelegationClient::heartbeat`] and open a runner **gate**, else stand by;
//! repeated every `HEARTBEAT_PERIOD_MS`, re-observing promptly on a
//! `lease_changed` push. The pure per-cycle `LeaseCoordinator::step` is always
//! available; its async timer/push-wake driver `LeaseCoordinator::run` is behind
//! the native-only `driver` feature (the three desktops enable it; the wasm SPA
//! runs no heavy task kind, so it observes but never drives). Slice 3 brings the
//! first task kind — backup uploads — under this loop.

use fauna_protocol::RpcRequester;
use fauna_protocol::delegation::{
    HeartbeatReply, HeartbeatRequest, KIND_HEARTBEAT, KIND_OBSERVE, ObserveReply, ObserveRequest,
};

mod coordinator;
pub use coordinator::LeaseCoordinator;

mod view;
pub use view::{TaskDelegationError, TaskDelegationView};

pub use fauna_core::delegation::{
    self, HEARTBEAT_PERIOD_MS, HeavyTaskCapability, LEASE_STALE_MS, LeaseAction, NotPinnable,
    OBSERVE_POLL_MS, ObservedLease, PinOption, RunnerStatus, TaskDelegationRow, decide,
    delegation_rows, resolve_pin,
};
pub use fauna_protocol::delegation as wire;

/// Typed `fauna.delegation.*` call surface. Errors propagate as the transport's
/// `R::Error` (native `NestClientError`, wasm rpc-wasm error); the namespaced
/// `RpcError`s the handlers emit (`fauna.delegation.permission_denied`,
/// `fauna.protocol.malformed`) surface through that error channel.
pub struct DelegationClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> DelegationClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.delegation.heartbeat` — claim or renew the lease for a task kind.
    /// The nest records `holder = caller` last-writer-wins and returns the
    /// post-write [`wire::LeaseState`]. Call this only when
    /// [`decide`] returned [`LeaseAction::Acquire`] or
    /// [`LeaseAction::Renew`]. Replay-safe (`forbid_replay=false`, 5 s).
    pub async fn heartbeat(&self, req: HeartbeatRequest) -> Result<HeartbeatReply, R::Error> {
        self.nest.request(KIND_HEARTBEAT, req).await
    }

    /// `fauna.delegation.observe` — read the current per-kind lease snapshot for
    /// the calling actor. `req.task_kinds` empty ⇒ every lease the actor holds.
    /// Pure read (`forbid_replay=false`, 5 s). This is the correctness backstop
    /// to the best-effort `fauna.delegation.lease_changed` push.
    pub async fn observe(&self, req: ObserveRequest) -> Result<ObserveReply, R::Error> {
        self.nest.request(KIND_OBSERVE, req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};
    use fauna_core::data::ParticipantRef;
    use fauna_core::delegation::ParticipantClass;
    use fauna_protocol::delegation::LeaseState;

    fn dev(id: &str) -> ParticipantRef {
        ParticipantRef::Device {
            device_id: id.to_string(),
        }
    }

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = DelegationClient::new(MockRequester);
    }

    // ── Wire-contract tests (mirror fauna-client-spam) ──────────────────────
    //
    // Pin each method's exact kind string + that the payload round-trips to the
    // typed request. Transport-free (runs on wasm too); real end-to-end
    // round-trip lives in tests/e2e-unified/tests/api/test_delegation.py.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            KIND_HEARTBEAT => fauna_protocol::encode_canonical(&HeartbeatReply {
                lease: LeaseState {
                    task_kind: "backup-upload".into(),
                    holder: dev("dev-a"),
                    holder_class: ParticipantClass::PluggedInDesktop,
                    age_ms: 0,
                    extra: Default::default(),
                },
                extra: Default::default(),
            }),
            KIND_OBSERVE => fauna_protocol::encode_canonical(&ObserveReply {
                leases: vec![],
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn heartbeat_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = DelegationClient::new(rec.clone());
        block_on(client.heartbeat(HeartbeatRequest {
            task_kind: "backup-upload".into(),
            holder: dev("dev-a"),
            holder_class: ParticipantClass::PluggedInDesktop,
            extra: Default::default(),
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.delegation.heartbeat");
        let req: HeartbeatRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.task_kind, "backup-upload");
        assert_eq!(req.holder, dev("dev-a"));
    }

    #[test]
    fn observe_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = DelegationClient::new(rec.clone());
        block_on(client.observe(ObserveRequest {
            task_kinds: vec!["backup-upload".into()],
            extra: Default::default(),
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.delegation.observe");
        let req: ObserveRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.task_kinds, vec!["backup-upload".to_string()]);
    }

    /// The re-exported `decide` is usable straight off this crate (the loop's
    /// entry point) — a smoke check that the re-export wires through.
    #[test]
    fn reexported_decide_is_callable() {
        let cands = vec![dev("dev-a")];
        assert_eq!(
            decide(&dev("dev-a"), &cands, None, LEASE_STALE_MS),
            LeaseAction::Acquire
        );
    }
}

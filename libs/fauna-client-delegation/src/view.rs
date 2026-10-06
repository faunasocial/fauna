//! The Task-delegation surface's async orchestration (slice 4) — the one shared
//! layer all seven apps render (`docs/goal/behavior/participants.md`
//! § Task delegation; ui.yaml page `task-delegation`).
//!
//! [`TaskDelegationView`] is the async twin of the pure
//! `fauna_core::delegation::{delegation_rows, resolve_pin}` pair: over the
//! user's pins (the `delegation` record, read off the account plane by
//! `fauna_account_plane::preference_surfaces`) and the live advisory leases
//! (`fauna.delegation.observe`, via [`DelegationClient`]) it composes
//! [`TaskDelegationRow`]s, and it decides what a picker's choice resolves to
//! before the plane writes it.
//!
//! **All the judgment lives in the pure layer.** This module holds no policy: it
//! is transport + composition, so the rules that decide what a picker may offer
//! (and what a pin may target) are stated once, in `fauna-core`, for every
//! app — rather than six times, differently. See
//! `fauna_core::delegation::PinOption` for why the option set is a correctness
//! surface and not a cosmetic one.

use fauna_core::data::{DelegationConfig, ParticipantRef};
use fauna_core::delegation::{
    HeavyTaskCapability, LEASE_STALE_MS, NotPinnable, ObservedLease, PinOption, TaskDelegationRow,
    delegation_rows, resolve_pin,
};
use fauna_protocol::delegation::ObserveRequest;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::DelegationClient;

/// What can go wrong loading or mutating the Task-delegation surface.
#[derive(Debug)]
pub enum TaskDelegationError<E> {
    /// A `fauna.delegation.observe` call failed.
    Transport(E),
    /// The account store's read or write of the `delegation` record failed,
    /// carried as text because the store's own error type lives above this
    /// wasm-clean crate (`fauna_account_plane::preference_surfaces`).
    Store(String),
    /// This client can never run heavy task kinds, so it refused to pin one to
    /// itself (`fauna_core::delegation::resolve_pin`). Unreachable through a
    /// picker built from [`TaskDelegationRow::pin_options`].
    NotPinnable(NotPinnable),
}

impl<E: core::fmt::Display> core::fmt::Display for TaskDelegationError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "delegation transport error: {e}"),
            Self::Store(e) => write!(f, "delegation store error: {e}"),
            Self::NotPinnable(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> core::error::Error for TaskDelegationError<E> {}

/// The Task-delegation surface for one actor on one device.
///
/// Generic over the WS-RPC transport (`R: RpcRequester`), like every sibling
/// client crate: native call sites pass `Arc<NestClient>`, the wasm SPA passes
/// its `WsRpcClient`.
pub struct TaskDelegationView<R: RpcRequester> {
    delegation: DelegationClient<R>,
    self_ref: ParticipantRef,
    self_capability: HeavyTaskCapability,
}

impl<R: RpcRequester> TaskDelegationView<R> {
    /// `self_ref` is this device's participant ref
    /// (`ParticipantRef::Device { device_id: hex::encode(device_id) }` — the
    /// encoding the lease loop already heartbeats with); `self_capability` says
    /// **which kinds this build ships a runner for**, and so which of them it
    /// may be pinned for (`HeavyTaskCapability::runner_for` — e.g. linux and
    /// tui declare `index`, the FFI desktops declare `backup-upload`; web +
    /// mobile pass `HeavyTaskCapability::viewer_only()`).
    pub fn new(
        delegation: DelegationClient<R>,
        self_ref: ParticipantRef,
        self_capability: HeavyTaskCapability,
    ) -> Self {
        Self {
            delegation,
            self_ref,
            self_capability,
        }
    }

    /// [`Self::new`] over a transport handle — the construction every native
    /// call site (tui, linux, the FFI desktops via
    /// `FfiNestClient::task_delegation_view_for_device`) shares.
    pub fn for_nest(
        nest: R,
        self_ref: ParticipantRef,
        self_capability: HeavyTaskCapability,
    ) -> Self {
        Self::new(DelegationClient::new(nest), self_ref, self_capability)
    }

    /// This device's participant ref — the client joins it against its devices
    /// roster to render the "this device" row.
    pub fn self_ref(&self) -> &ParticipantRef {
        &self.self_ref
    }

    /// Which kinds this client may be pinned as a runner for.
    pub fn capability(&self) -> &HeavyTaskCapability {
        &self.self_capability
    }

    /// Compose the rows over pins the caller already holds: the account plane
    /// (`fauna_sync_engine::preference_surfaces::load_delegation`) reads the
    /// `delegation` sub-record (`fauna.state.delegation`) off the replica's own
    /// store and hands it here, so the lease observation, the staleness rule,
    /// and the runner column are one implementation.
    pub async fn rows_from(
        &self,
        delegation: &DelegationConfig,
    ) -> Result<Vec<TaskDelegationRow>, TaskDelegationError<R::Error>> {
        let reply = self
            .delegation
            .observe(ObserveRequest::default())
            .await
            .map_err(TaskDelegationError::Transport)?;

        let observed: Vec<(String, ObservedLease)> = reply
            .leases
            .into_iter()
            .map(|l| {
                (
                    l.task_kind,
                    ObservedLease {
                        holder: l.holder,
                        holder_class: l.holder_class,
                        age_ms: l.age_ms,
                    },
                )
            })
            .collect();

        Ok(delegation_rows(
            &self.self_ref,
            &self.self_capability,
            delegation,
            &observed,
            LEASE_STALE_MS,
        ))
    }
}

impl<R: RpcRequester> TaskDelegationView<R>
where
    R::Error: RpcErrorClass,
{
    /// What the picker's `option` resolves to as a stored pin, or the refusal —
    /// the gate the account-plane write
    /// (`fauna_account_plane::preference_surfaces::update_delegation`) passes
    /// through first: a self-pin the client can never run — a nest-run kind, or
    /// one this build ships no runner for — would leave the kind waiting
    /// forever, so nothing reaches the write in that case.
    pub fn resolve(
        &self,
        task_kind: &str,
        option: &PinOption,
    ) -> Result<Option<ParticipantRef>, TaskDelegationError<R::Error>> {
        resolve_pin(task_kind, &self.self_ref, &self.self_capability, option)
            .map_err(TaskDelegationError::NotPinnable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_core::delegation::{ParticipantClass, RunnerStatus};
    use fauna_protocol::delegation::{KIND_OBSERVE, LeaseState, ObserveReply};
    use std::sync::{Arc, Mutex};

    fn dev(id: &str) -> ParticipantRef {
        ParticipantRef::Device {
            device_id: id.to_string(),
        }
    }

    /// Serves `fauna.delegation.observe` from a scripted lease list. Records every kind it saw, so a test can assert that
    /// a rejected write never reached the wire.
    struct MockNest {
        leases: Vec<LeaseState>,
        seen: Mutex<Vec<&'static str>>,
    }

    impl MockNest {
        fn new(leases: Vec<LeaseState>) -> Arc<Self> {
            Arc::new(Self {
                leases,
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    // `fauna-protocol` blanket-impls `RpcRequester for Arc<T>`, so the tests can
    // hand `Arc<MockNest>` straight to both clients.
    impl RpcRequester for MockNest {
        type Error = std::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.seen.lock().unwrap().push(kind);
            let bytes = match kind {
                KIND_OBSERVE => fauna_protocol::encode_canonical(&ObserveReply {
                    leases: self.leases.clone(),
                    extra: Default::default(),
                }),
                other => panic!("MockNest: unhandled kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
        }
    }

    fn lease(kind: &str, holder: ParticipantRef, age_ms: u64) -> LeaseState {
        LeaseState {
            task_kind: kind.to_string(),
            holder,
            holder_class: ParticipantClass::PluggedInDesktop,
            age_ms,
            extra: Default::default(),
        }
    }

    fn view(nest: Arc<MockNest>, cap: HeavyTaskCapability) -> TaskDelegationView<Arc<MockNest>> {
        TaskDelegationView::new(DelegationClient::new(nest), dev("me"), cap)
    }

    #[test]
    fn load_with_no_lease_and_no_pin_is_waiting_automatic() {
        let rows = block_on(
            view(
                MockNest::new(vec![]),
                HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_BACKUP_UPLOAD]),
            )
            .rows_from(&DelegationConfig::default()),
        )
        .expect("infallible mock");
        assert_eq!(rows.len(), fauna_core::delegation::LIVE_TASK_KINDS.len());
        assert_eq!(rows[0].task_kind, "backup-upload");
        assert_eq!(rows[0].runner, RunnerStatus::Waiting);
        assert_eq!(rows[0].assignment, PinOption::Automatic);
    }

    /// The composition the whole slice turns on: an `ObserveReply.leases` entry
    /// must land on the row for its own `task_kind`, carrying `age_ms` through
    /// so staleness is judged against the nest's clock.
    #[test]
    fn load_maps_a_fresh_foreign_lease_onto_its_kinds_row() {
        let nest = MockNest::new(vec![lease("backup-upload", dev("dev-b"), 0)]);
        let rows = block_on(
            view(
                nest,
                HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_BACKUP_UPLOAD]),
            )
            .rows_from(&DelegationConfig::default()),
        )
        .expect("infallible");
        assert_eq!(rows[0].runner, RunnerStatus::Other { who: dev("dev-b") });
    }

    #[test]
    fn load_maps_a_fresh_self_lease_to_this_device() {
        let nest = MockNest::new(vec![lease("backup-upload", dev("me"), 0)]);
        let rows = block_on(
            view(
                nest,
                HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_BACKUP_UPLOAD]),
            )
            .rows_from(&DelegationConfig::default()),
        )
        .expect("infallible");
        assert_eq!(rows[0].runner, RunnerStatus::ThisDevice);
    }

    #[test]
    fn load_treats_a_stale_lease_as_waiting() {
        let nest = MockNest::new(vec![lease("backup-upload", dev("dev-b"), LEASE_STALE_MS)]);
        let rows = block_on(
            view(
                nest,
                HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_BACKUP_UPLOAD]),
            )
            .rows_from(&DelegationConfig::default()),
        )
        .expect("infallible");
        assert_eq!(rows[0].runner, RunnerStatus::Waiting);
    }

    /// A lease for a kind this build doesn't list must not bleed onto a row —
    /// the forward-compat case when a newer sibling client heartbeats a kind
    /// this client has never heard of.
    #[test]
    fn load_ignores_a_lease_for_an_unknown_kind() {
        let nest = MockNest::new(vec![lease("some-future-kind", dev("dev-b"), 0)]);
        let rows = block_on(
            view(
                nest,
                HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_BACKUP_UPLOAD]),
            )
            .rows_from(&DelegationConfig::default()),
        )
        .expect("infallible");
        assert_eq!(rows.len(), fauna_core::delegation::LIVE_TASK_KINDS.len());
        for row in &rows {
            assert_eq!(row.runner, RunnerStatus::Waiting);
        }
    }

    /// The slice-6 display leg: a nest heartbeating `content-rescore` (it holds
    /// a sufficient grant) shows up as that row's runner on every app.
    #[test]
    fn load_maps_a_nest_lease_onto_the_content_rescore_row() {
        let nest_ref = ParticipantRef::Nest {
            actor_pubkey: [9u8; 32],
        };
        let nest = MockNest::new(vec![LeaseState {
            task_kind: "content-rescore".to_string(),
            holder: nest_ref.clone(),
            holder_class: ParticipantClass::AlwaysOnNest,
            age_ms: 0,
            extra: Default::default(),
        }]);
        let rows = block_on(
            view(
                nest,
                HeavyTaskCapability::runner_for([fauna_core::delegation::KIND_BACKUP_UPLOAD]),
            )
            .rows_from(&DelegationConfig::default()),
        )
        .expect("infallible");
        let rescore = rows
            .iter()
            .find(|r| r.task_kind == "content-rescore")
            .expect("content-rescore row");
        assert_eq!(rescore.runner, RunnerStatus::Other { who: nest_ref });
        assert_eq!(rows[0].runner, RunnerStatus::Waiting, "no bleed");
    }

    #[test]
    fn load_on_a_viewer_only_client_offers_no_this_device_pin() {
        let rows = block_on(
            view(MockNest::new(vec![]), HeavyTaskCapability::viewer_only())
                .rows_from(&DelegationConfig::default()),
        )
        .expect("infallible");
        assert_eq!(rows[0].pin_options, vec![PinOption::Automatic]);
    }

    #[test]
    fn resolve_refuses_a_viewer_only_self_pin_without_touching_the_wire() {
        let nest = MockNest::new(vec![]);
        let v = view(nest.clone(), HeavyTaskCapability::viewer_only());
        let err = v
            .resolve("backup-upload", &PinOption::ThisDevice)
            .expect_err("a viewer-only client must not pin itself");
        assert!(matches!(err, TaskDelegationError::NotPinnable(_)));
        assert!(
            nest.seen.lock().unwrap().is_empty(),
            "the rejection must happen before any request is sent",
        );
    }
}

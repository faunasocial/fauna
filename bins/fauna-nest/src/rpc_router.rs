//! RPC kind-to-handler dispatch table. Built once at app startup; immutable
//! during runtime. Per spec § 5.1.
//!
//! Each per-area module exposes a `register_<area>_handlers(builder)` function
//! that adds its kinds. `lib.rs::build_app` composes these into a single
//! `RpcRouter` carried in `AppState`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;

use fauna_protocol::RpcError;

/// Boxed handler. Takes app state, the authenticated actor, and the raw
/// request payload bytes (canonical-CBOR-encoded by the dispatcher).
/// Returns Reply payload bytes (canonical-CBOR-encoded) or an `RpcError`.
pub type RpcHandler = Box<
    dyn Fn(
            Arc<crate::routes::AppState>,
            [u8; 32],
            Bytes,
        ) -> BoxFuture<'static, Result<Bytes, RpcError>>
        + Send
        + Sync,
>;

/// Per-kind metadata + the handler itself.
pub struct RpcKindMeta {
    pub forbid_replay: bool,
    pub default_deadline: Duration,
    pub handler: RpcHandler,
}

/// Immutable kind-to-meta map. Construct via `builder()`.
pub struct RpcRouter {
    kinds: BTreeMap<&'static str, RpcKindMeta>,
}

impl RpcRouter {
    pub fn builder() -> RpcRouterBuilder {
        RpcRouterBuilder {
            kinds: BTreeMap::new(),
        }
    }

    pub fn kind_meta(&self, kind: &str) -> Option<&RpcKindMeta> {
        self.kinds.get(kind)
    }

    pub fn contains(&self, kind: &str) -> bool {
        self.kinds.contains_key(kind)
    }

    pub fn iter_kinds(&self) -> impl Iterator<Item = &str> + '_ {
        self.kinds.keys().copied()
    }
}

pub struct RpcRouterBuilder {
    kinds: BTreeMap<&'static str, RpcKindMeta>,
}

impl RpcRouterBuilder {
    pub fn add(&mut self, kind: &'static str, meta: RpcKindMeta) {
        if self.kinds.insert(kind, meta).is_some() {
            panic!("duplicate RPC kind registration: {kind}");
        }
    }

    pub fn build(self) -> RpcRouter {
        RpcRouter { kinds: self.kinds }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dummy_handler() -> RpcHandler {
        Box::new(|_state, _actor, _payload| Box::pin(async { Ok(Bytes::new()) }))
    }

    #[test]
    fn builder_adds_and_router_looks_up() {
        let mut b = RpcRouter::builder();
        b.add(
            "fauna.test.one",
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler: dummy_handler(),
            },
        );
        let r = b.build();
        let m = r.kind_meta("fauna.test.one").expect("kind registered");
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(5));
        assert!(r.contains("fauna.test.one"));
        assert!(!r.contains("fauna.test.unknown"));
    }

    #[test]
    fn iter_kinds_returns_all_registered() {
        let mut b = RpcRouter::builder();
        b.add(
            "fauna.b.one",
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(1),
                handler: dummy_handler(),
            },
        );
        b.add(
            "fauna.a.one",
            RpcKindMeta {
                forbid_replay: true,
                default_deadline: Duration::from_secs(1),
                handler: dummy_handler(),
            },
        );
        let r = b.build();
        let kinds: Vec<&str> = r.iter_kinds().collect();
        // BTreeMap → alphabetical
        assert_eq!(kinds, vec!["fauna.a.one", "fauna.b.one"]);
    }

    #[test]
    #[should_panic(expected = "duplicate RPC kind registration")]
    fn duplicate_kind_panics() {
        let mut b = RpcRouter::builder();
        b.add(
            "fauna.test.dup",
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(1),
                handler: dummy_handler(),
            },
        );
        b.add(
            "fauna.test.dup",
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(1),
                handler: dummy_handler(),
            },
        );
    }

    // Compile-only: verify the handler type is `Send + Sync` so AppState
    // can hold the router in an Arc.
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    #[test]
    fn router_is_send_sync() {
        let r = RpcRouter::builder().build();
        assert_send_sync(&Arc::new(r));
    }

    /// Per-kind wire metadata is declared **twice** — nest-side in each
    /// `register_<area>_handlers` (this router) and client-side in
    /// `fauna_protocol::KindRegistry::register_<area>_kinds`. Both describe the
    /// same wire fact, so a disagreement is a client/nest split-brain:
    /// `transport.md` § Idempotency and reconnect-with-resume makes
    /// `forbid_replay` the client's auto-retry gate and `default_deadline` the
    /// deadline both sides fall back to, so drift means a client that
    /// auto-retries a mutation the nest considers replay-forbidden, or gives up
    /// before the nest's own deadline.
    ///
    /// This test is the anti-drift gate: every kind the live router dispatches
    /// must be declared in the registry with identical metadata.
    ///
    /// **Scope — deliberately the per-actor router only.** The nest has one
    /// other kind table, and it does not belong here: `FederationRouter`
    /// (`fauna.federation.*`, nest↔nest). `KindRegistry` is the
    /// *client's* metadata table — it is consulted by `fauna-client` /
    /// `fauna-rpc-wasm` / `fauna-anon-client` when an app issues a request — and
    /// no app ever issues a federation kind. So their absence from
    /// the registry is correct, not a gap: do **not** "complete" this test by
    /// iterating that router, and do not add its kinds to `KindRegistry`.
    /// (The federation channel's own originating-side metadata is instead
    /// *derived* from the `FederationRouter` — `federation_router.rs::
    /// hint_registry` — so it needs no parity gate at all: one table serves,
    /// hints, and decides the §4.D retry policy.)
    #[test]
    fn router_and_kind_registry_agree_on_every_kind() {
        let router = crate::build_rpc_router();
        let registry = fauna_protocol::KindRegistry::full();

        let mut missing: Vec<&str> = Vec::new();
        let mut replay_drift: Vec<String> = Vec::new();
        let mut deadline_drift: Vec<String> = Vec::new();

        for kind in router.iter_kinds() {
            let nest = router
                .kind_meta(kind)
                .expect("iter_kinds yields registered");
            match registry.meta(kind) {
                None => missing.push(kind),
                Some(client) => {
                    if client.forbid_replay != nest.forbid_replay {
                        replay_drift.push(format!(
                            "{kind}: nest={} registry={}",
                            nest.forbid_replay, client.forbid_replay
                        ));
                    }
                    if client.default_deadline != nest.default_deadline {
                        deadline_drift.push(format!(
                            "{kind}: nest={:?} registry={:?}",
                            nest.default_deadline, client.default_deadline
                        ));
                    }
                }
            }
        }
        missing.sort_unstable();
        replay_drift.sort();
        deadline_drift.sort();

        assert!(
            missing.is_empty() && replay_drift.is_empty() && deadline_drift.is_empty(),
            "router/KindRegistry drift\n\
             ── {} kind(s) dispatched by the nest but absent from KindRegistry:\n{}\n\
             ── {} forbid_replay disagreement(s):\n{}\n\
             ── {} default_deadline disagreement(s):\n{}",
            missing.len(),
            missing.join("\n"),
            replay_drift.len(),
            replay_drift.join("\n"),
            deadline_drift.len(),
            deadline_drift.join("\n"),
        );
    }
}

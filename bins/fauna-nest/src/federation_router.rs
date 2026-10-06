//! Federation kind-to-handler dispatch table (Spec Y2 slice 4 §4.C / hub Track D).
//!
//! The peer-symmetric serving counterpart of [`crate::rpc_router::RpcRouter`]: it
//! routes the `fauna.federation.*` kinds a verified peer nest may invoke over the
//! long-lived federation channel ([`crate::federation_channel`]). It **reuses**
//! the per-actor router's [`RpcKindMeta`] / [`RpcHandler`] verbatim — the handler
//! signature is identical (`Fn(Arc<AppState>, [u8; 32], Bytes) -> …`), the only
//! difference being that the `[u8; 32]` carries the **originating nest_id** rather
//! than an authenticated actor (the connection authenticated the peer at handshake
//! time, §4.B). Each handler maps onto the SAME DB work its HTTP twin does today,
//! minus the per-request signature/skew/nonce (now the connection's job).
//!
//! **The kind allowlist is a construction invariant.** §4.C makes the federation
//! kind allowlist *the* structural authorization boundary: a peer may invoke ONLY
//! `fauna.federation.*` kinds, never a client-actor kind. We enforce that here, at
//! the source, rather than as a separate runtime list to keep in sync: the builder
//! panics if a registered kind does not start with [`FEDERATION_KIND_PREFIX`]. So
//! "is this kind allowed over the channel?" reduces to [`FederationRouter::contains`]
//! — the allowlist *is* the set of registered kinds, single-source-of-truth.
//!
//! Handlers register via `register_<area>_federation_handlers(&mut
//! FederationRouterBuilder)`, assembled in `lib.rs::build_app` alongside the
//! `RpcRouter` (§5).

use std::collections::BTreeMap;

pub use crate::rpc_router::{RpcHandler, RpcKindMeta};

/// Every federation kind shares this prefix. Registering a kind without it is a
/// programming error (the builder panics) — this is what makes "registered in the
/// `FederationRouter`" equivalent to "on the federation kind allowlist" (§4.C).
pub const FEDERATION_KIND_PREFIX: &str = "fauna.federation.";

/// Immutable federation-kind-to-meta map. Construct via [`FederationRouter::builder`].
pub struct FederationRouter {
    kinds: BTreeMap<&'static str, RpcKindMeta>,
}

impl FederationRouter {
    pub fn builder() -> FederationRouterBuilder {
        FederationRouterBuilder {
            kinds: BTreeMap::new(),
        }
    }

    /// Look up a kind's metadata + handler. `None` ⇒ the kind is not on the
    /// federation allowlist (a peer invoking it gets `unauthenticated`, §4.C).
    pub fn kind_meta(&self, kind: &str) -> Option<&RpcKindMeta> {
        self.kinds.get(kind)
    }

    /// True iff `kind` is on the federation allowlist (i.e. a registered
    /// `fauna.federation.*` kind). This is the structural authz check the
    /// federation serving loop applies before dispatch (§4.C).
    pub fn contains(&self, kind: &str) -> bool {
        self.kinds.contains_key(kind)
    }

    pub fn iter_kinds(&self) -> impl Iterator<Item = &str> + '_ {
        self.kinds.keys().copied()
    }

    /// The originating-side mirror of this serving table: a metadata-only
    /// [`fauna_protocol::KindRegistry`] holding every registered federation
    /// kind's `forbid_replay` + `default_deadline`, for
    /// `RpcDispatcher::set_kind_registry` on a federation channel
    /// (`federation_channel::{dial, serve_listener}`).
    ///
    /// Derived, never declared twice: the serving table is the single source of
    /// truth, so the wire's `replay_forbidden` hint, the §4.D retry policy
    /// ([`Self::retry_safe`]), and the served metadata cannot drift from each
    /// other. This is deliberately **not** an extension of the client's
    /// `KindRegistry::full()` — that table is the *client's* (the
    /// `rpc_router.rs` parity test's scope note), and no app ever issues a
    /// federation kind.
    pub fn hint_registry(&self) -> fauna_protocol::KindRegistry {
        let mut r = fauna_protocol::KindRegistry::new();
        for (kind, meta) in &self.kinds {
            r.add(
                *kind,
                fauna_protocol::RpcKindMeta::new(meta.forbid_replay, meta.default_deadline),
            );
        }
        r
    }

    /// Whether the stale-channel dead-link policy (§4.D) may re-dial + re-send
    /// `kind` once with the same `idempotency_key` after an in-flight
    /// disconnect: exactly the kinds whose handler is **naturally idempotent**
    /// under that repeated call (`forbid_replay = false`). The peer's
    /// idempotency cache is per-connection and never survives the redial, so
    /// semantic idempotence — recorded at each kind's declaration site in
    /// `federation_handlers.rs` — is the only thing this can rest on.
    ///
    /// An unregistered kind is never retry-safe: originating a kind this nest
    /// does not itself serve is a programming error (the tables are
    /// peer-symmetric), and "don't re-run what we can't vouch for" is the
    /// fail-safe reading of it.
    pub fn retry_safe(&self, kind: &str) -> bool {
        debug_assert!(
            self.kinds.contains_key(kind),
            "originating unregistered federation kind: {kind}"
        );
        self.kinds.get(kind).is_some_and(|m| !m.forbid_replay)
    }
}

pub struct FederationRouterBuilder {
    kinds: BTreeMap<&'static str, RpcKindMeta>,
}

impl FederationRouterBuilder {
    /// Register a federation kind. Panics if `kind` is already registered or does
    /// not start with [`FEDERATION_KIND_PREFIX`] (the allowlist invariant, §4.C).
    pub fn add(&mut self, kind: &'static str, meta: RpcKindMeta) {
        assert!(
            kind.starts_with(FEDERATION_KIND_PREFIX),
            "federation kind must start with `{FEDERATION_KIND_PREFIX}` (the channel \
             allowlist is the structural authz boundary, §4.C): got `{kind}`"
        );
        if self.kinds.insert(kind, meta).is_some() {
            panic!("duplicate federation kind registration: {kind}");
        }
    }

    pub fn build(self) -> FederationRouter {
        FederationRouter { kinds: self.kinds }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::time::Duration;

    fn dummy_handler() -> RpcHandler {
        Box::new(|_state, _nest_id, _payload| Box::pin(async { Ok(Bytes::new()) }))
    }

    fn meta() -> RpcKindMeta {
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: dummy_handler(),
        }
    }

    #[test]
    fn builder_adds_and_router_looks_up() {
        let mut b = FederationRouter::builder();
        b.add("fauna.federation.keypackage.fetch", meta());
        let r = b.build();
        assert!(r.kind_meta("fauna.federation.keypackage.fetch").is_some());
        assert!(r.contains("fauna.federation.keypackage.fetch"));
        assert!(!r.contains("fauna.federation.unknown"));
    }

    #[test]
    fn contains_is_the_allowlist() {
        // A registered kind is allowed; a client-actor kind is not — `contains`
        // is the structural authz boundary the serving loop applies (§4.C).
        let mut b = FederationRouter::builder();
        b.add("fauna.federation.sync.pull", meta());
        let r = b.build();
        assert!(r.contains("fauna.federation.sync.pull"));
        assert!(!r.contains("fauna.conversations.send"));
        assert!(!r.contains("fauna.bridges.set_catch_all_actor"));
    }

    #[test]
    #[should_panic(expected = "must start with")]
    fn non_federation_prefix_panics() {
        // The allowlist invariant: a non-`fauna.federation.*` kind can never be
        // registered here, so it can never be served over the channel.
        let mut b = FederationRouter::builder();
        b.add("fauna.conversations.send", meta());
    }

    #[test]
    #[should_panic(expected = "duplicate federation kind registration")]
    fn duplicate_kind_panics() {
        let mut b = FederationRouter::builder();
        b.add("fauna.federation.post.get", meta());
        b.add("fauna.federation.post.get", meta());
    }

    #[test]
    fn iter_kinds_returns_all_registered_sorted() {
        let mut b = FederationRouter::builder();
        b.add("fauna.federation.welcome.deliver", meta());
        b.add("fauna.federation.keypackage.fetch", meta());
        let r = b.build();
        let kinds: Vec<&str> = r.iter_kinds().collect();
        // BTreeMap → alphabetical.
        assert_eq!(
            kinds,
            vec![
                "fauna.federation.keypackage.fetch",
                "fauna.federation.welcome.deliver",
            ]
        );
    }

    #[allow(dead_code)]
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    #[test]
    fn router_is_send_sync() {
        let r = FederationRouter::builder().build();
        assert_send_sync(&Arc::new(r));
    }

    /// Build the PRODUCTION federation table (the same registration call
    /// `lib.rs::build_app` makes), not a dummy fixture.
    fn production_router() -> FederationRouter {
        let mut b = FederationRouter::builder();
        crate::federation_handlers::register_federation_handlers(&mut b);
        b.build()
    }

    /// The federation twin of `kind::tests::the_replay_forbidden_set_is_exactly_
    /// these_kinds`, and load-bearing for the same reason: since the §4.D retry
    /// policy and the wire hint both derive from this one table
    /// ([`FederationRouter::retry_safe`] / [`FederationRouter::hint_registry`]),
    /// a kind's `forbid_replay` value is the *only* thing standing between a
    /// mid-call channel drop and an automatic re-dial + re-send of a
    /// non-idempotent operation on the peer.
    ///
    /// **Changing this list is the point, not a nuisance.** Adding a kind is
    /// free. *Removing* one asserts the handler behind it is idempotent under a
    /// repeated same-key call **arriving on a fresh connection** (the peer's
    /// idempotency cache does not survive the redial) — read the handler and
    /// say so in the commit body, and update the rationale at the declaration
    /// site in `federation_handlers.rs`.
    #[test]
    fn the_federation_replay_forbidden_set_is_exactly_these_kinds() {
        let r = production_router();
        let actual: Vec<&str> = r
            .iter_kinds()
            .filter(|k| {
                r.kind_meta(k)
                    .expect("registered kind has meta")
                    .forbid_replay
            })
            .collect();

        // Sorted, because the router walks a BTreeMap.
        let expected = [
            // A delivery appends an inbox row and charges the invitee's quota
            // per call — see the declaration site.
            "fauna.federation.conversation.room.invite",
            // The foreign-inviter leg: records a row and delivers a knock per
            // call, so the same verdict as `room.invite` — see the declaration
            // site.
            "fauna.federation.conversation.room.invite_issue",
            // Appends a fresh inbox row + charges `inbox_bytes_used` per call
            // (shared core with `fauna.inbox.send`) — see the declaration site.
            "fauna.federation.inbox.deliver",
            // Destructive consume of a one-time key package.
            "fauna.federation.keypackage.fetch",
            // Same inbox append + quota charge as `inbox.deliver` (F10
            // leg-parity) — see the declaration site.
            "fauna.federation.welcome.deliver",
        ];

        assert_eq!(
            actual, expected,
            "the federation replay-forbidden set changed — see this test's doc comment"
        );
    }

    /// The derived hint registry is the serving table, kind for kind and value
    /// for value — the contract that keeps the wire hint and the served
    /// metadata from ever disagreeing. Near-tautological against today's
    /// implementation (a direct iteration), and that is the point: it pins the
    /// contract against a future refactor that declares the originating-side
    /// table separately.
    #[test]
    fn hint_registry_mirrors_the_serving_table() {
        let r = production_router();
        let hints = r.hint_registry();

        for kind in r.iter_kinds() {
            let serving = r.kind_meta(kind).expect("registered kind has meta");
            let hint = hints
                .meta(kind)
                .unwrap_or_else(|| panic!("{kind} missing from the hint registry"));
            assert_eq!(hint.forbid_replay, serving.forbid_replay, "{kind}");
            assert_eq!(hint.default_deadline, serving.default_deadline, "{kind}");
        }
        assert_eq!(
            hints.iter().count(),
            r.iter_kinds().count(),
            "the hint registry declares kinds the serving table does not"
        );
    }

    /// §4.D's retry decision comes from the table, and fails safe on a kind the
    /// table has never heard of.
    #[test]
    fn retry_safe_derives_from_forbid_replay() {
        let mut b = FederationRouter::builder();
        b.add("fauna.federation.keypackage.fetch", {
            let mut m = meta();
            m.forbid_replay = true;
            m
        });
        b.add("fauna.federation.sync.pull", meta());
        let r = b.build();

        assert!(!r.retry_safe("fauna.federation.keypackage.fetch"));
        assert!(r.retry_safe("fauna.federation.sync.pull"));
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "unregistered federation kind")
    )]
    fn an_unregistered_kind_is_never_retry_safe() {
        let r = FederationRouter::builder().build();
        // debug builds: the debug_assert names the programming error loudly;
        // release builds: the lookup falls through to the fail-safe `false`.
        assert!(!r.retry_safe("fauna.federation.unknown"));
    }
}

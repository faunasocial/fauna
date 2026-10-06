//! The nest's arm of the permission-set request call — how the nest-hosted
//! `/oauth/par` resolves an `include:<NSID>` scope **through the PDS bridge**
//! rather than porting the resolution chain or refusing the scope.
//!
//! `docs/goal/behavior/atproto-oauth-provider.md` § Implementation status
//! today (the 2026-09-25 bullet) rules the shape: the nest pushes
//! `fauna.bridges.atproto.permission_set_requested` (a random `request_id` and
//! the NSID) to every connected approved `atproto.pds` bridge, and the bridge
//! answers over the BRIDGE-class kind
//! `fauna.bridges.atproto.deliver_permission_set` with the verified dag-cbor
//! bytes — or with no record at all when its chain refused. The two legs are
//! correlated here, by `request_id`, in [`PermissionSetRequests`].
//!
//! # Why a push out and an RPC back, not a nest-initiated `Request` frame
//!
//! `transport.md` § Request lifecycle rejects a `Reply` from any client
//! connection, so a request the nest starts would change the substrate every
//! app rides for the sake of one bridge call — and would need a correlation-id
//! space of its own beside the client's. Every nest→bridge handoff already
//! takes this shape (`projection_ready` → the fetch kinds, `issuer_key_rotated`
//! → `fetch_issuer_jwks`); this one merely carries its correlation id as data.
//!
//! # Every failure is the one closed-world refusal
//!
//! No approved PDS bridge connected, an older bridge that ignores a push kind
//! it does not know, a shed push frame, a bridge-side refusal, or the deadline:
//! each answers the caller `invalid_scope` naming the set, and never an
//! expansion of bytes this nest did not receive verified. The bridge-absent arm
//! is decided **before** any push goes out, so a deployment with no PDS bridge
//! refuses at once rather than after [`PERMISSION_SET_RESOLVE_TIMEOUT`].
//!
//! The nest adds no cache of its own: the bridge's `CachingSetResolver` already
//! serves repeats under the spec's TTLs, and a second cache here would be a
//! second freshness rule for one document.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_bridge_atproto::permission_set::ParsedInclude;
use fauna_protocol::PushEvent;
use fauna_protocol::atproto_pds::BridgeAtprotoPermissionSetRequestedPush;
use tokio::sync::oneshot;

use crate::db::bridge_service_users::BridgeRole;
use crate::routes::AppState;

/// How long one PAR waits for the bridge to deliver a set.
///
/// The bridge's chain is three legs — the `_lexicon` TXT lookup, the DID
/// document, the `sync.getRecord` proof — each bounded at ten seconds on the
/// bridge (`permset_wiring.go`), so this is the chain's own worst case rather
/// than a number invented here. A PAR request is held for this long only when
/// a bridge is connected and has not yet answered; the bridge-absent arm never
/// waits.
pub const PERMISSION_SET_RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

/// How many resolutions may be in flight at once before the oldest is shed.
///
/// Bounded like every other store the ceremony keeps (`ParStore`,
/// `ConsentWakes`): `/oauth/par` is anonymous, so an unbounded map keyed by
/// caller-driven requests is a memory lever. Sized far above what the per-IP
/// PAR budget lets through in one deadline window, so an honest population
/// never sheds anything; a flood sheds its own oldest, and a shed waiter is
/// **refused**, never guessed — the same closed-world answer as a timeout.
pub const PERMISSION_SET_REQUEST_CAPACITY: usize = 1024;

/// Bytes of entropy in a `request_id`. The id names an in-memory waiter for at
/// most one deadline; 128 bits is what stops another party's answer landing
/// on it by guess, and no more is needed.
pub const REQUEST_ID_LEN: usize = 16;

/// What the bridge delivered for one request: the verified dag-cbor bytes, or
/// `None` when its chain refused the set.
pub type Delivery = Option<Vec<u8>>;

struct Waiter {
    sender: oneshot::Sender<Delivery>,
    /// Monotonic open order, for oldest-first shedding.
    opened: u64,
}

#[derive(Default)]
struct Inner {
    waiters: HashMap<[u8; REQUEST_ID_LEN], Waiter>,
    next_open: u64,
}

/// The in-flight permission-set requests: `request_id` → the PAR waiting on
/// it. One entry per include, opened before the push goes out and removed by
/// the delivery, the deadline, or the shed.
#[derive(Default)]
pub struct PermissionSetRequests {
    inner: Mutex<Inner>,
}

impl PermissionSetRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a request id and register the waiter that will receive its answer.
    ///
    /// Registered **before** the push is sent, so an answer that arrives
    /// faster than the caller can await it is not lost — the `oneshot` holds
    /// it. At capacity the oldest waiter is shed: its sender drops, its
    /// receiver sees `RecvError`, and [`resolve_permission_sets`] refuses.
    pub fn open(&self) -> ([u8; REQUEST_ID_LEN], oneshot::Receiver<Delivery>) {
        use rand::RngCore as _;

        let mut inner = self.inner.lock().expect("permission-set requests poisoned");
        if inner.waiters.len() >= PERMISSION_SET_REQUEST_CAPACITY {
            let oldest = inner
                .waiters
                .iter()
                .min_by_key(|(_, w)| w.opened)
                .map(|(id, _)| *id);
            if let Some(id) = oldest {
                inner.waiters.remove(&id);
                tracing::warn!(
                    target: "oauth_as",
                    "permission-set requests at capacity; shed the oldest waiter (it is refused, never guessed)"
                );
            }
        }
        let mut id = [0u8; REQUEST_ID_LEN];
        loop {
            rand::rngs::OsRng.fill_bytes(&mut id);
            if !inner.waiters.contains_key(&id) {
                break;
            }
        }
        let (sender, receiver) = oneshot::channel();
        let opened = inner.next_open;
        inner.next_open += 1;
        inner.waiters.insert(id, Waiter { sender, opened });
        (id, receiver)
    }

    /// Hand the bridge's answer to the waiting PAR. `false` when nothing waits
    /// under that id — the deadline already answered, the id was never minted,
    /// or the waiter was shed — which is a log line for the bridge and never an
    /// error: a late answer is ordinary.
    pub fn deliver(&self, request_id: &[u8], record: Delivery) -> bool {
        let Ok(id) = <[u8; REQUEST_ID_LEN]>::try_from(request_id) else {
            return false;
        };
        let waiter = self
            .inner
            .lock()
            .expect("permission-set requests poisoned")
            .waiters
            .remove(&id);
        match waiter {
            // A receiver that has already gone away (the caller's future was
            // dropped) makes the send fail; that is the same "nobody waits"
            // answer, not a fault.
            Some(w) => w.sender.send(record).is_ok(),
            None => false,
        }
    }

    /// Drop a waiter the caller has stopped waiting on.
    pub fn forget(&self, request_id: &[u8; REQUEST_ID_LEN]) {
        self.inner
            .lock()
            .expect("permission-set requests poisoned")
            .waiters
            .remove(request_id);
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.inner
            .lock()
            .expect("permission-set requests poisoned")
            .waiters
            .len()
    }
}

/// The set that could not be resolved — the first one to fail, named because
/// the NSID is the client's own string and therefore safe to echo. Why it
/// failed is not carried: that describes this deployment's bridge and its
/// outbound network to whoever chose the NSID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSet {
    pub nsid: String,
}

/// Resolve every set a planned PAR named, through the bridge, and hand back
/// the verified bytes in the order the includes were named — the order
/// `finish_par_request` expects them in.
///
/// Every include is in flight at once; any one failing refuses the whole
/// request (`atproto-pds-full.md:331`, fail-at-session-start — a partial
/// grant would mean the user consents to a card missing a set the client
/// believes it got).
pub async fn resolve_permission_sets(
    state: &Arc<AppState>,
    includes: &[ParsedInclude],
) -> Result<Vec<Vec<u8>>, UnresolvedSet> {
    resolve_permission_sets_within(state, includes, PERMISSION_SET_RESOLVE_TIMEOUT).await
}

/// [`resolve_permission_sets`] with the deadline as an argument, so a test can
/// witness the timeout arm without waiting the production thirty seconds.
pub async fn resolve_permission_sets_within(
    state: &Arc<AppState>,
    includes: &[ParsedInclude],
    deadline: Duration,
) -> Result<Vec<Vec<u8>>, UnresolvedSet> {
    let Some(first) = includes.first() else {
        return Ok(Vec::new());
    };
    let refuse = |nsid: &str| UnresolvedSet {
        nsid: nsid.to_string(),
    };

    // The bridge-absent arm, decided before any push goes out: a deployment
    // with no PDS bridge enrolled, or none connected right now, refuses at
    // once rather than holding the request for the whole deadline.
    let bridges = match state.db.list_approved_bridge_service_users().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "oauth_as",
                error = %format!("{e:#}"),
                "permission set: failed to list approved bridges; refusing"
            );
            return Err(refuse(&first.nsid));
        }
    };
    let connected: Vec<[u8; 32]> = bridges
        .into_iter()
        .filter(|b| b.role == BridgeRole::AtprotoPds && state.ws.has_connections(&b.ed25519_pubkey))
        .map(|b| b.ed25519_pubkey)
        .collect();
    if connected.is_empty() {
        tracing::info!(
            target: "oauth_as",
            nsid = %first.nsid,
            "permission set: no connected atproto.pds bridge to resolve through; refusing"
        );
        return Err(refuse(&first.nsid));
    }

    let registry = &state.oauth_as.permission_sets;
    let mut ids = Vec::with_capacity(includes.len());
    let mut waits = Vec::with_capacity(includes.len());
    for include in includes {
        let (id, receiver) = registry.open();
        ids.push(id);
        for bridge in &connected {
            state.ws.notify_push(
                bridge,
                PushEvent::BridgeAtprotoPermissionSetRequested(
                    BridgeAtprotoPermissionSetRequestedPush {
                        request_id: id.to_vec(),
                        nsid: include.nsid.clone(),
                        extra: Default::default(),
                    },
                ),
            );
        }
        let nsid = include.nsid.clone();
        waits.push(async move {
            match tokio::time::timeout(deadline, receiver).await {
                Ok(Ok(Some(record))) => Ok(record),
                Ok(Ok(None)) => {
                    tracing::info!(target: "oauth_as", nsid = %nsid, "permission set: the bridge could not resolve it; refusing");
                    Err(refuse(&nsid))
                }
                Ok(Err(_shed)) => {
                    tracing::warn!(target: "oauth_as", nsid = %nsid, "permission set: waiter shed under load; refusing");
                    Err(refuse(&nsid))
                }
                Err(_elapsed) => {
                    tracing::info!(target: "oauth_as", nsid = %nsid, "permission set: no bridge answered within the deadline; refusing");
                    Err(refuse(&nsid))
                }
            }
        });
    }

    let outcome = futures_util::future::try_join_all(waits).await;
    // Whatever happened, nothing stays registered: a delivered or shed waiter
    // is already gone, a timed-out or abandoned one is dropped here.
    for id in &ids {
        registry.forget(id);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn include(nsid: &str) -> ParsedInclude {
        ParsedInclude {
            nsid: nsid.to_string(),
            aud: None,
        }
    }

    #[tokio::test]
    async fn a_delivery_before_the_await_is_not_lost() {
        let registry = PermissionSetRequests::new();
        let (id, receiver) = registry.open();
        assert!(registry.deliver(&id, Some(b"doc".to_vec())));
        assert_eq!(receiver.await.unwrap(), Some(b"doc".to_vec()));
        assert_eq!(registry.pending(), 0);
    }

    #[test]
    fn an_unknown_or_malformed_id_is_not_accepted() {
        let registry = PermissionSetRequests::new();
        let (id, _receiver) = registry.open();
        assert!(!registry.deliver(&[0u8; REQUEST_ID_LEN], Some(vec![])));
        assert!(!registry.deliver(&id[..5], Some(vec![])));
        assert!(registry.deliver(&id, None), "a refusal is a delivery too");
        assert!(
            !registry.deliver(&id, None),
            "a second answer matches nothing"
        );
    }

    #[tokio::test]
    async fn at_capacity_the_oldest_waiter_is_shed_and_refused() {
        let registry = PermissionSetRequests::new();
        let (_, oldest) = registry.open();
        let mut later = Vec::new();
        for _ in 1..PERMISSION_SET_REQUEST_CAPACITY {
            later.push(registry.open());
        }
        assert_eq!(registry.pending(), PERMISSION_SET_REQUEST_CAPACITY);
        let (_, newest) = registry.open();
        assert_eq!(registry.pending(), PERMISSION_SET_REQUEST_CAPACITY);
        assert!(
            oldest.await.is_err(),
            "the shed waiter must be refused, not left hanging"
        );
        drop(newest);
        drop(later);
    }

    #[tokio::test]
    async fn with_no_connected_bridge_the_refusal_is_immediate_and_nothing_is_pushed() {
        let state = crate::test_support::fixture_state();
        let started = std::time::Instant::now();
        let outcome =
            resolve_permission_sets(&state, &[include("com.example.calendar.appPerms")]).await;
        assert_eq!(
            outcome,
            Err(UnresolvedSet {
                nsid: "com.example.calendar.appPerms".to_string()
            })
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the bridge-absent arm must not wait out the deadline"
        );
        assert_eq!(state.oauth_as.permission_sets.pending(), 0);
    }

    #[tokio::test]
    async fn no_includes_resolve_to_nothing_without_a_bridge() {
        let state = crate::test_support::fixture_state();
        assert_eq!(resolve_permission_sets(&state, &[]).await, Ok(Vec::new()));
    }
}

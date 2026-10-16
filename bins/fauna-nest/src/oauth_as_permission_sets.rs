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
//! correlated by `request_id` in [`PermissionSetRequests`], the shared
//! nest→bridge waiter registry ([`crate::bridge_round_trip`]).
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

use std::sync::Arc;
use std::time::Duration;

use fauna_bridge_atproto::permission_set::ParsedInclude;
use fauna_protocol::PushEvent;
use fauna_protocol::atproto_pds::BridgeAtprotoPermissionSetRequestedPush;

use crate::bridge_round_trip::RoundTrips;
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

/// What the bridge delivered for one request: the verified dag-cbor bytes, or
/// `None` when its chain refused the set.
pub type Delivery = Option<Vec<u8>>;

/// The in-flight permission-set requests: `request_id` → the PAR waiting on
/// it. One entry per include, opened before the push goes out and removed by
/// the delivery, the deadline, or the shed — the shared nest→bridge registry
/// ([`crate::bridge_round_trip`]), sized by its
/// [`crate::bridge_round_trip::ROUND_TRIP_CAPACITY`]: `/oauth/par` is
/// anonymous, and that bound sits far above what the per-IP PAR budget lets
/// through in one deadline window.
pub type PermissionSetRequests = RoundTrips<Delivery>;

/// A fresh, empty registry for [`crate::oauth_as_state::OAuthAsRuntime`].
#[must_use]
pub fn new_requests() -> PermissionSetRequests {
    RoundTrips::new("permission-set")
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

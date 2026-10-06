//! An `AuthClient` assembled from **two** identity sources proves they agree.
//!
//! `AuthClient::with_bearer_source` (and `bearer_only`) take a path identity —
//! the keypair/actor id that becomes the authenticated WS URL's actor — and,
//! separately, a `BearerSource` that mints the token presented on that same
//! connection. Nothing structurally ties the two together, so a bug in either
//! lookup makes them name different actors, and the nest then refuses every WS
//! upgrade with `403` while the app shows nothing at all.
//!
//! That failure was diagnosable only from a *nest* log until this check existed:
//! in 2026-08 a credential-store read-modify-write race restored a signed-out
//! actor's stored index, so the launch machine and the connection keypair
//! resolved different identities.
//! These tests pin the three verdicts: **agree**, **disagree**, and the
//! **cannot-say** case that must not be mistaken for either.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_client::AuthClient;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{ApiError, BearerSource, StaticBearer};

/// A `BearerSource` that names an actor of the caller's choosing — the shape
/// `LaunchMachineBearer` has in production, without needing a launch machine.
struct ActorNamingBearer {
    token: String,
    actor_id: [u8; 32],
}

#[async_trait]
impl BearerSource for ActorNamingBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        Ok(self.token.clone())
    }
    fn bearer_actor_id(&self) -> Option<[u8; 32]> {
        Some(self.actor_id)
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

/// The failure this whole seam exists for: the bearer mints for one actor while
/// the connection path names another. Both ids are reported, so a reader of the
/// app log (or of this fact) can tell *which* source drifted.
#[test]
fn disagreeing_sources_are_reported_with_both_actors() {
    let path_kp = ActorKeypair::from_secret([1u8; 32]);
    let path_actor = path_kp.actor_id().0;
    let stale_kp = ActorKeypair::from_secret([2u8; 32]);
    let bearer: Arc<dyn BearerSource> = Arc::new(ActorNamingBearer {
        token: "stale-actors-token".into(),
        actor_id: stale_kp.actor_id().0,
    });

    let client =
        AuthClient::with_bearer_source("http://127.0.0.1:1".into(), path_kp, bearer, http());

    let mismatch = client
        .identity_mismatch()
        .expect("two disagreeing sources must be reported");
    assert_eq!(mismatch.path_actor_id, path_actor);
    assert_eq!(mismatch.bearer_actor_id, stale_kp.actor_id().0);
}

/// The healthy case — same actor on both sides — reports nothing.
#[test]
fn agreeing_sources_report_no_mismatch() {
    let kp = ActorKeypair::from_secret([3u8; 32]);
    let bearer: Arc<dyn BearerSource> = Arc::new(ActorNamingBearer {
        token: "our-token".into(),
        actor_id: kp.actor_id().0,
    });

    let client = AuthClient::with_bearer_source("http://127.0.0.1:1".into(), kp, bearer, http());

    assert_eq!(client.identity_mismatch(), None);
}

/// **"Cannot say" is not "mismatched."** A `StaticBearer` holds an opaque token
/// it cannot attribute to any actor, so it opts out of the check rather than
/// guessing — otherwise every test fixture in the tree would report a false
/// mismatch and the signal would be worthless.
#[test]
fn a_source_that_cannot_name_its_actor_is_not_a_mismatch() {
    let kp = ActorKeypair::from_secret([4u8; 32]);
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("opaque".into()));

    let client = AuthClient::with_bearer_source("http://127.0.0.1:1".into(), kp, bearer, http());

    assert_eq!(client.identity_mismatch(), None);
}

/// `AuthClient::new` builds its own `WsChallengeBearer` from the very keypair it
/// was handed, so it is single-source and cannot disagree. Pinned because that
/// is a property of the constructor's wiring, not of the type — a future rewrite
/// that mints from somewhere else would break it here rather than in the field.
#[test]
fn the_single_source_constructor_cannot_disagree() {
    let kp = ActorKeypair::from_secret([5u8; 32]);
    let client = AuthClient::new("http://127.0.0.1:1".into(), kp);
    assert_eq!(client.identity_mismatch(), None);
}

/// The bearer-only helper (the Windows on-demand hydration host) is two-source
/// as well — a supplied actor id plus a pre-minted bearer — so it is checked on
/// the same terms.
#[test]
fn the_bearer_only_helper_is_checked_too() {
    let owner = ActorKeypair::from_secret([6u8; 32]);
    let other = ActorKeypair::from_secret([7u8; 32]);
    let bearer: Arc<dyn BearerSource> = Arc::new(ActorNamingBearer {
        token: "someone-elses".into(),
        actor_id: other.actor_id().0,
    });

    let client = AuthClient::bearer_only(
        "http://127.0.0.1:1".into(),
        owner.actor_id().0,
        bearer,
        http(),
    );

    let mismatch = client.identity_mismatch().expect("must be reported");
    assert_eq!(mismatch.path_actor_id, owner.actor_id().0);
    assert_eq!(mismatch.bearer_actor_id, other.actor_id().0);
}

/// The `Arc<dyn BearerSource>` blanket impl must *forward* the new method, not
/// fall back to the trait default. Without this the production wiring — which
/// always hands an `Arc` in — would silently report "cannot say" for every
/// client, and the check would be dead code that always passes.
#[test]
fn the_arc_forward_does_not_swallow_the_actor() {
    let kp = ActorKeypair::from_secret([8u8; 32]);
    let inner = Arc::new(ActorNamingBearer {
        token: "t".into(),
        actor_id: kp.actor_id().0,
    });
    // Exercise the `impl BearerSource for Arc<T>` path explicitly.
    assert_eq!(
        BearerSource::bearer_actor_id(&inner),
        Some(kp.actor_id().0),
        "Arc<T> must forward bearer_actor_id to T"
    );
}

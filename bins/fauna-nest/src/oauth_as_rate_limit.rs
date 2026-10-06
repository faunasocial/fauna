//! The authorization server's per-endpoint request budget.
//!
//! # The re-decision this module IS
//!
//! On the bridge, every AS endpoint spent one shared `ClassAuth` bucket —
//! `createSession`'s bucket — and that sharing had a reason: `createSession` is
//! a **guessing** surface, so a caller hammering it is presumed to be guessing,
//! and the AS endpoints rode along. On the nest that premise is gone. There is
//! no `createSession` here, nothing on these routes is guessable, and the
//! endpoints have genuinely different shapes: `/oauth/par` makes an outbound
//! fetch on a caller-named host, `/oauth/authorize` renders a page, and the
//! token endpoint's refresh grant is a *legitimately repeated* call every live
//! session makes on a schedule. Sharing one bucket across them would let a
//! flood of one starve the others, and would meter a client's honest refresh
//! against a budget sized for a stranger's first contact.
//!
//! So: **one bucket per route**, at `ClassAuth`'s own limit — the number is
//! ported by value (`bins/fauna-bridges/internal/xrpc/ratelimit.go`), only its
//! *key* is re-decided. The refresh grant gets its own bucket when the token endpoint lands,
//! for the same reason.
//!
//! # Why a layer and not a call inside the handler
//!
//! The ordering ruling is that an endpoint's budget is spent **before the
//! body** — the endpoint is anonymous and `/oauth/par` dials a host the caller
//! names, so an unbounded rate is both a resource drain here and an
//! amplification primitive pointed at third parties. A `tower::Layer` runs
//! before the handler is entered at all, which makes "before the body"
//! structural rather than a comment on line ordering.
//!
//! A refusal here therefore carries **no** `DPoP-Nonce`, which is correct and
//! matches the bridge: the nonce is minted by the handler, on responses to
//! requests this server has agreed to consider.
//!
//! # Loopback is bounded, not exempt
//!
//! The same posture `rate_limit.rs` ratified for the blanket governor
//! (`transport-connection.md` § Abuse posture): a same-host caller is metered against a
//! separate aggregate ceiling rather than skipping the check. Without it the
//! deployment's own co-resident callers — the e2e harness above all, which
//! drives whole consent ceremonies from `127.0.0.1` — would exhaust a budget
//! sized for one stranger's first contact within a single test module. With an
//! exemption instead, a co-resident runaway would be invisible, which is the
//! 2026-08-22 signature that ruling exists to prevent.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use axum::extract::ConnectInfo;
use axum::response::{IntoResponse, Response};
use tower_layer::Layer;
use tower_service::Service;

use crate::oauth_as_error::{OAuthDeny, oauth_error_response};

/// One route's budget: how many requests, from one source, in how long.
///
/// **A budget per route is the re-decision this module exists for**, so a route
/// names its own rather than inheriting a class's. Two exist today and they are
/// genuinely different shapes: starting an authorization is a rare act by a
/// stranger, while polling one is a *frequent* act by a browser this server
/// itself sent to the page. Metering them alike would either strangle the poll
/// or hand a stranger the poll's budget.
#[derive(Debug, Clone, Copy)]
pub struct EndpointBudget {
    /// Requests per window, per remote source.
    pub limit: u32,
    /// The aggregate ceiling every **loopback** source shares — bounded, not
    /// exempt (module docs).
    pub loopback_limit: u32,
    pub window_secs: i64,
}

/// The budget for a request-taking endpoint — `/oauth/par`, `/oauth/authorize`,
/// `/oauth/revoke` and the two polled consent starts. `/oauth/token` is the
/// one exception ([`TOKEN_ENDPOINT_BUDGET`]).
///
/// `ClassAuth`'s numbers, ported by value from the bridge: ten per five minutes.
pub const AUTHORIZATION_BUDGET: EndpointBudget = EndpointBudget {
    limit: OAUTH_LIMIT,
    loopback_limit: OAUTH_LOOPBACK_LIMIT,
    window_secs: OAUTH_WINDOW_SECS,
};

/// The **refresh grant's own** bucket, spent inside `/oauth/token` after
/// `grant_type` is parsed and before any lookup.
///
/// `ClassAuth`'s numbers again — the ported ones, not a number invented here
/// (`atproto-oauth-provider.md` § F4 detail owns the `ClassAuth` re-decision).
/// The route key differs, so this is a second bucket rather than more of the
/// endpoint's.
///
/// # It is not binding today, and that is the point
///
/// A refresh request spends the endpoint's bucket first (the layer) and then
/// this one, so with identical numbers the endpoint's is always the one that
/// refuses. This bucket exists for what the two are protecting, which is not
/// the same thing:
///
/// * The **endpoint's** bucket bounds an anonymous flood making this nest parse
///   bodies and resolve caller-named clients. Ten per five minutes is *tight*
///   for a real fleet — a client refreshes every access-token lifetime, so a
///   handful of accounts on one address reaches it — and widening it is a
///   deployment question a later slice will very likely answer yes to.
/// * **This** bucket bounds *guessing a refresh token*. A refresh token is a
///   secret presented in a body and stays valid for up to 180 days, so
///   attempts against it are a sustained surface in a way the code grant's
///   never is: an authorization code is consumed on lookup and dead in 60
///   seconds, which bounds guessing all by itself.
///
/// So the two must not share a number even while they hold the same one:
/// widening the endpoint's budget for legitimate traffic would otherwise widen
/// refresh-token guessing with it, silently. This is the floor that survives
/// that change.
pub const REFRESH_GRANT_BUDGET: EndpointBudget = EndpointBudget {
    limit: OAUTH_LIMIT,
    loopback_limit: OAUTH_LOOPBACK_LIMIT,
    window_secs: OAUTH_WINDOW_SECS,
};

/// The **authorization-code grant's own** bucket, spent inside `/oauth/token`
/// after `grant_type` is parsed — [`REFRESH_GRANT_BUDGET`]'s twin, and
/// `ClassAuth`'s numbers again.
///
/// It exists because the token endpoint's own layer is no longer `ClassAuth`
/// ([`TOKEN_ENDPOINT_BUDGET`]): a device polling a typed-code or quiet-push
/// start hits that endpoint every few seconds by design. The two grants that
/// are NOT polled keep the floor they had through their own buckets, so
/// widening the endpoint for polling made neither of them cheaper.
pub const AUTHORIZATION_CODE_GRANT_BUDGET: EndpointBudget = EndpointBudget {
    limit: OAUTH_LIMIT,
    loopback_limit: OAUTH_LOOPBACK_LIMIT,
    window_secs: OAUTH_WINDOW_SECS,
};

/// The budget for `/oauth/token` itself — the layer, spent before the body.
///
/// **Poll-class, because a legitimate client now polls this endpoint.** The
/// typed-code and quiet-push starts (`authorization-server.md` § Consent) end
/// in a client polling the token endpoint at the interval this server hands it
/// ([`crate::oauth_as_ceremony::BACKCHANNEL_POLL_INTERVAL_SECS`], five seconds),
/// which is twelve requests a minute from one well-behaved device — the whole
/// of `ClassAuth`'s five-minute allowance in under a minute. At `ClassAuth`
/// numbers the grant RFC 8628 defines would refuse its own clients.
///
/// Sized as [`POLL_BUDGET`] is and for the same reason: the caller holds a
/// handle this server minted and a key it must prove. What the layer still
/// bounds — an anonymous flood making this nest parse bodies — needs a DPoP
/// proof over a live server nonce before any body is read, and the two grants
/// that are guessing-adjacent keep `ClassAuth`'s floor in their own buckets
/// ([`AUTHORIZATION_CODE_GRANT_BUDGET`], [`REFRESH_GRANT_BUDGET`]). A polled
/// grant needs no bucket of its own beyond this one: its handle is 256 bits,
/// and the per-flow `slow_down` pacing bounds what one client may ask.
pub const TOKEN_ENDPOINT_BUDGET: EndpointBudget = POLL_BUDGET;

/// The budget for the consent page's long-poll.
///
/// `ClassPublicRead`'s numbers, ported by value — 60 per minute, the class the
/// bridge metered this endpoint under, and the right one: the caller is a
/// browser this server sent to its own page, holding a flow token this server
/// minted, and its only credential is that token. An invalid one is answered
/// immediately with no hold and no read, so guessing costs a map lookup inside
/// this budget.
///
/// It is far above the real cadence rather than tight against it: a page polls
/// serially and each poll is HELD for [`POLL_HOLD_SECS`], so a waiting browser
/// issues about two a minute, not sixty. The headroom is for the retry paths —
/// a dropped response, a backgrounded tab resuming — which must not be metered
/// into failure while the user is still looking at the page.
pub const POLL_BUDGET: EndpointBudget = EndpointBudget {
    limit: 60,
    loopback_limit: 6000,
    window_secs: 60,
};

/// The budget for `/oauth/userinfo` (TP6).
///
/// `ClassPublicRead`'s numbers, the same as [`POLL_BUDGET`]'s — not
/// [`AUTHORIZATION_BUDGET`]'s, because the caller is not a stranger starting
/// something: it presents an access token this issuer signed and a DPoP proof
/// over it, and a relying party reads UserInfo once per sign-in of each of its
/// users, so ten per five minutes from one server address would throttle any
/// site with more than a handful of sign-ins. What the request costs before it
/// is refused is one signature verification — no body, no fetch, no write.
pub const USERINFO_BUDGET: EndpointBudget = EndpointBudget {
    limit: 60,
    loopback_limit: 6000,
    window_secs: 60,
};

/// `ClassAuth`'s window, ported by value from the bridge.
pub const OAUTH_WINDOW_SECS: i64 = 5 * 60;

/// `ClassAuth`'s limit, ported by value: ten requests per window, per remote
/// source, per route.
pub const OAUTH_LIMIT: u32 = 10;

/// The aggregate ceiling every **loopback** source shares, per route.
///
/// A hundred times the per-IP bucket. Sized from both ends, in the shape
/// [`crate::rate_limit::LOOPBACK_MAX_RPS`] is:
///
/// * **Above** the legitimate peak. The most aggressive legitimate loopback
///   client this project has is the tier_3 e2e harness, and a consent ceremony
///   costs one PAR, one authorize and a handful of polls; a thousand per five
///   minutes clears a full suite run of that surface with room to spare.
/// * **Below** the shape it exists to catch. Three requests a second, sustained,
///   from co-resident processes on one route is not a test suite — and past this
///   point the nest answers a cheap refusal instead of dialling a caller-named
///   host, keeping its outbound capacity for real flows.
///
/// A starting point, refutable the moment the suite's real peak is measured —
/// the same honesty [`crate::bridge_rate_limit::CHANNEL_COMMIT_LIMITER_CONFIG`]
/// states about its own number.
pub const OAUTH_LOOPBACK_LIMIT: u32 = 1000;

/// The hard ceiling on distinct (route, source) buckets.
///
/// ⚠ **This used to be a SWEEP threshold, and that is the bug that the finding
/// named.** Past it, an admission called `retain` to drop *expired* windows —
/// but a bucket is created by a source's first request and is not removed when
/// that source is refused (the bucket is what remembers the refusal), so under
/// a live flood `retain` frees **nothing**, the condition persists, and every
/// subsequent admission walks the whole map while holding the one `Mutex` all
/// three AS routes spend. Cost per request grew with the flood, at the cheapest
/// possible position for an attacker: a layer, spent before DPoP and before any
/// authentication.
///
/// Now it bounds membership instead. Reclamation moved off the request path
/// entirely, to [`spawn_endpoint_sweeper`] — the shape
/// [`crate::rate_limit::spawn_retain_sweeper`] already uses for the blanket
/// governor, whose own doc says the keyed limiter "never sheds entries on its
/// own".
///
/// The true ceiling is this **plus one overflow bucket per route**, because
/// the redirect below inserts that bucket the first time a route needs it.
/// Four routes, so five constants' worth of slack on a map of 8192 — stated
/// rather than rounded away, since a bound nobody can state exactly is one
/// nobody can test.
const MAX_BUCKETS: usize = 8192;

/// Which population a bucket meters.
///
/// Named rather than left as an `Option<IpAddr>` because there are **three**
/// populations, not two, and the third arrived with the ceiling: collapsing any
/// of them together lets one party spend another's budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BucketSource {
    /// Every loopback spelling, and every caller whose address is unknown.
    ///
    /// One bucket on purpose: loopback is one party — the same set of
    /// co-resident processes — so per-address buckets would hand a runaway a
    /// full budget for every fresh address it picked; and an unknown address
    /// must not be a way around the budget at all.
    Shared,
    /// A remote source with a bucket of its own.
    Remote(IpAddr),
    /// Every source that arrived while the map was already full.
    ///
    /// The bucket a source metered while the map is FULL shares with every other
    /// such source, per route.
    ///
    /// ⚠ It is its own [`BucketSource`] variant and **not** `None`. `None` is
    /// already the loopback-and-unknown bucket, so reusing it would have put a
    /// remote flood and the co-resident e2e harness in one window — the flood
    /// eating loopback's budget, and loopback's traffic counting against the
    /// flood's. Two populations with different limits must not share a key.
    ///
    /// Shedding to one aggregate rather than growing is what keeps memory bounded
    /// **without letting fresh keys flush live windows** — the property the old
    /// threshold's comment correctly insisted on and which a plain "evict something
    /// to make room" would have broken. It is the shape
    /// `anonymous_rate_limit::check_global` already uses against distributed
    /// brute-force, where the per-source limiter is evaded because each source gets
    /// its own full budget.
    ///
    /// Its ceiling is the per-source limit: once the map is full the nest has
    /// already met more distinct sources on one route than any real deployment
    /// does, so what remains is to keep answering cheaply rather than to meter
    /// newcomers generously. A legitimate client caught by this retries into a
    /// bucket of its own as soon as the sweeper drains the flood.
    Overflow,
}

/// One route's budget for one population. The key is the pair, which is the
/// whole re-decision: on the bridge it was (source, *class*).
type BucketKey = (&'static str, BucketSource);

#[derive(Debug, Clone, Copy)]
struct Window {
    started_at: i64,
    count: u32,
}

/// Fixed-window per-(route, source) counters.
///
/// Fixed-window rather than sliding because that is what is being ported, and
/// the difference does not matter at this limit: the worst a window boundary
/// buys a caller is two windows' worth back to back, which at ten per five
/// minutes is twenty requests in a burst — still four orders of magnitude below
/// anything worth calling a flood.
#[derive(Debug, Default)]
pub struct EndpointLimiter {
    buckets: Mutex<HashMap<BucketKey, Window>>,
}

impl EndpointLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Report whether one more request from `source` on `route` fits its
    /// window, counting it if so.
    ///
    /// `source` is `None` when the peer address is unknown — which must not be
    /// a way around the budget, so every such caller shares one bucket.
    pub fn allow(
        &self,
        route: &'static str,
        source: Option<IpAddr>,
        budget: EndpointBudget,
        now: i64,
    ) -> bool {
        let limit = match source {
            Some(ip) if ip.is_loopback() => budget.loopback_limit,
            _ => budget.limit,
        };
        let key: BucketKey = match source {
            Some(ip) if !ip.is_loopback() => (route, BucketSource::Remote(ip)),
            _ => (route, BucketSource::Shared),
        };

        let mut buckets = self.buckets.lock().expect("oauth limiter poisoned");
        // ⚠ NO sweep here. Reclamation is [`spawn_endpoint_sweeper`]'s, off the
        // request path — see [`MAX_BUCKETS`] for the rescan-under-the-shared-lock
        // this ordering removes. What happens here instead is O(1): a source
        // arriving when the map is already full is metered against the shared
        // overflow bucket rather than being given one of its own.
        // ⚠ Only a REMOTE source can be shed. `Shared` is one key per route —
        // loopback and unknown-address callers — so it costs O(routes) memory,
        // not O(sources), and it is not what the ceiling exists to bound.
        // Redirecting it was measured starving the local e2e harness: with the
        // map full of a remote flood's keys, loopback's own key was absent, so
        // it landed in the overflow bucket and spent a budget the flood had
        // already been drawing down.
        let key = match key.1 {
            BucketSource::Remote(_)
                if buckets.len() >= MAX_BUCKETS && !buckets.contains_key(&key) =>
            {
                (route, BucketSource::Overflow)
            }
            _ => key,
        };
        match buckets.get_mut(&key) {
            Some(window) if now.saturating_sub(window.started_at) < budget.window_secs => {
                if window.count >= limit {
                    return false;
                }
                window.count += 1;
                true
            }
            _ => {
                buckets.insert(
                    key,
                    Window {
                        started_at: now,
                        count: 1,
                    },
                );
                true
            }
        }
    }

    /// Drop every window that can no longer refuse anything.
    ///
    /// The widest window any route uses is the cut-off, so a sweep can never
    /// drop a bucket still live for a route with a longer one. Called on a
    /// timer, never from `allow` — that is the whole of the finding's second
    /// consequence.
    pub fn retain_recent(&self, now: i64) {
        let mut buckets = self.buckets.lock().expect("oauth limiter poisoned");
        buckets.retain(|_, w| now.saturating_sub(w.started_at) < OAUTH_WINDOW_SECS);
    }

    #[cfg(test)]
    fn bucket_count(&self) -> usize {
        self.buckets.lock().expect("oauth limiter poisoned").len()
    }
}

/// Reclaim the AS limiter's expired windows on a timer.
///
/// The direct fix for the finding's second consequence: bounded work, off the
/// request path, so a flood can never make each admission pay for the size of
/// the flood. Modelled on [`crate::rate_limit::spawn_retain_sweeper`], which
/// exists for exactly this reason on the blanket governor.
///
/// The interval wants to be no longer than [`OAUTH_WINDOW_SECS`]: past that,
/// expired buckets linger a whole extra window and the map's steady-state size
/// is larger than it needs to be. It is not a correctness bound — [`MAX_BUCKETS`]
/// is — so a missed tick costs nothing but memory, which is why the ticker
/// skips rather than bursts.
pub fn spawn_endpoint_sweeper(
    limiter: Arc<EndpointLimiter>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            limiter.retain_recent(fauna_core::data::Timestamp::now_secs_or_zero());
        }
    })
}

// ── The layer ────────────────────────────────────────────────────────────────

/// Spends one request of `route`'s budget before the inner service is entered.
#[derive(Clone)]
pub struct OAuthRateLimitLayer {
    limiter: Arc<EndpointLimiter>,
    route: &'static str,
    budget: EndpointBudget,
}

impl OAuthRateLimitLayer {
    pub fn new(limiter: Arc<EndpointLimiter>, route: &'static str, budget: EndpointBudget) -> Self {
        Self {
            limiter,
            route,
            budget,
        }
    }
}

impl<S> Layer<S> for OAuthRateLimitLayer {
    type Service = OAuthRateLimitService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        OAuthRateLimitService {
            inner,
            limiter: self.limiter.clone(),
            route: self.route,
            budget: self.budget,
        }
    }
}

#[derive(Clone)]
pub struct OAuthRateLimitService<S> {
    inner: S,
    limiter: Arc<EndpointLimiter>,
    route: &'static str,
    budget: EndpointBudget,
}

/// The refusal a spent budget answers with: `temporarily_unavailable`, in the
/// same JSON shape as every other refusal on these endpoints, with `Retry-After`
/// so a client backs off deliberately rather than by guessing.
fn too_many(route: &str) -> Response {
    let deny = OAuthDeny::unavailable(format!(
        "too many requests to {route} from this address — retry after the interval in Retry-After"
    ));
    let mut response = oauth_error_response(&deny);
    *response.status_mut() = axum::http::StatusCode::TOO_MANY_REQUESTS;
    response.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        axum::http::HeaderValue::from_static("60"),
    );
    response
}

impl<S, B> Service<axum::http::Request<B>> for OAuthRateLimitService<S>
where
    S: Service<axum::http::Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send,
    B: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: axum::http::Request<B>) -> Self::Future {
        let limiter = self.limiter.clone();
        let route = self.route;
        let budget = self.budget;
        let mut inner = self.inner.clone();
        Box::pin(async move {
            // Behind the SNI router the PROXY-v2 header has already resolved the
            // real internet source into `ConnectInfo`, so a remote client is
            // keyed on its own address and only a genuine same-host peer reaches
            // the loopback arm.
            let source = req
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ci| ci.0.ip());
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            if !limiter.allow(route, source, budget, now) {
                return Ok(too_many(route).into_response());
            }
            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAR: &str = "/oauth/par";
    const TOKEN: &str = "/oauth/token";

    fn remote(last: u8) -> Option<IpAddr> {
        Some(IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, last)))
    }

    /// The ported number, exactly: ten in a window, the eleventh refused.
    #[test]
    fn a_remote_source_gets_ten_per_window() {
        let l = EndpointLimiter::new();
        for i in 0..OAUTH_LIMIT {
            assert!(
                l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000),
                "request {i} refused"
            );
        }
        assert!(!l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
    }

    /// **The re-decision under test.** A source that has spent `/oauth/par`'s
    /// budget can still reach `/oauth/token` — on the bridge's shared bucket it
    /// could not, and a flood of one endpoint starved the others.
    #[test]
    fn one_endpoints_budget_does_not_starve_another() {
        let l = EndpointLimiter::new();
        for _ in 0..OAUTH_LIMIT {
            assert!(l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
        }
        assert!(!l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
        assert!(
            l.allow(TOKEN, remote(1), AUTHORIZATION_BUDGET, 1_000),
            "a spent PAR budget closed the token endpoint too"
        );
    }

    /// One source's flood never refuses another's request.
    #[test]
    fn one_source_cannot_spend_anothers_budget() {
        let l = EndpointLimiter::new();
        for _ in 0..OAUTH_LIMIT {
            assert!(l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
        }
        assert!(!l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
        assert!(l.allow(PAR, remote(2), AUTHORIZATION_BUDGET, 1_000));
    }

    /// The window is fixed: past it, the budget is whole again.
    #[test]
    fn the_window_resets() {
        let l = EndpointLimiter::new();
        for _ in 0..OAUTH_LIMIT {
            assert!(l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
        }
        assert!(!l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
        assert!(l.allow(
            PAR,
            remote(1),
            AUTHORIZATION_BUDGET,
            1_000 + OAUTH_WINDOW_SECS
        ));
    }

    /// Loopback is bounded, not exempt — and every loopback spelling shares one
    /// bucket, so a runaway cannot reset its budget by picking a fresh
    /// same-host address.
    #[test]
    fn loopback_is_bounded_and_aggregated() {
        let l = EndpointLimiter::new();
        let v4 = Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        let v6 = Some(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        let other_v4 = Some(IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 2)));

        // Far past the remote budget, still admitted.
        for _ in 0..OAUTH_LIMIT * 2 {
            assert!(l.allow(PAR, v4, AUTHORIZATION_BUDGET, 1_000));
        }
        // Three spellings, one bucket.
        assert_eq!(l.bucket_count(), 1);
        for _ in 0..(OAUTH_LOOPBACK_LIMIT - OAUTH_LIMIT * 2) {
            assert!(l.allow(PAR, v6, AUTHORIZATION_BUDGET, 1_000));
        }
        assert!(
            !l.allow(PAR, other_v4, AUTHORIZATION_BUDGET, 1_000),
            "a fresh loopback address reset the aggregate"
        );
    }

    /// An unknown peer address must not be a way around the budget: every such
    /// caller shares one bucket at the remote limit.
    #[test]
    fn an_unknown_source_is_still_bounded() {
        let l = EndpointLimiter::new();
        for _ in 0..OAUTH_LIMIT {
            assert!(l.allow(PAR, None, AUTHORIZATION_BUDGET, 1_000));
        }
        assert!(!l.allow(PAR, None, AUTHORIZATION_BUDGET, 1_000));
    }

    /// Reclamation happens on the timer's call, and drops only what can no
    /// longer refuse anything.
    ///
    /// The successor to a test that asserted the same words about a sweep
    /// inside `allow` — the arrangement that finding removed.
    #[test]
    fn retain_recent_reclaims_expired_windows_only() {
        let l = EndpointLimiter::new();
        assert!(l.allow(PAR, remote(1), AUTHORIZATION_BUDGET, 1_000));
        assert!(l.allow(
            TOKEN,
            remote(2),
            AUTHORIZATION_BUDGET,
            1_000 + OAUTH_WINDOW_SECS
        ));
        assert_eq!(l.bucket_count(), 2);

        l.retain_recent(1_000 + OAUTH_WINDOW_SECS);
        assert_eq!(
            l.bucket_count(),
            1,
            "the live window was swept, or the stale one kept"
        );
        // And the survivor is still counting where it left off, not reset.
        for _ in 1..OAUTH_LIMIT {
            assert!(l.allow(
                TOKEN,
                remote(2),
                AUTHORIZATION_BUDGET,
                1_000 + OAUTH_WINDOW_SECS
            ));
        }
        assert!(!l.allow(
            TOKEN,
            remote(2),
            AUTHORIZATION_BUDGET,
            1_000 + OAUTH_WINDOW_SECS
        ));
    }

    /// **The finding's first consequence, asserted with the clock STANDING
    /// STILL** — which is the only case a flood actually presents, and the case
    /// the predecessor test never reached because it advanced time a full
    /// window before its one over-threshold admission.
    ///
    /// Nothing here is ever expired, so a sweep of any kind would free zero.
    /// The map must stay bounded anyway.
    #[test]
    fn a_flood_of_fresh_sources_cannot_grow_the_map_past_its_ceiling() {
        let l = EndpointLimiter::new();
        for i in 0..(MAX_BUCKETS as u32 + 512) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(i.to_be_bytes()));
            l.allow(PAR, Some(ip), AUTHORIZATION_BUDGET, 1_000);
        }
        // The exact bound: the ceiling, plus the one overflow bucket this
        // single route needed. Asserted precisely rather than as "roughly
        // bounded", because a bound nobody states exactly is one nobody can
        // tell from a slow leak.
        assert_eq!(
            l.bucket_count(),
            MAX_BUCKETS + 1,
            "the map did not settle at its ceiling plus one overflow bucket"
        );
    }

    /// A flood must not flush the windows of sources already being refused —
    /// the property the old threshold's comment insisted on and which "evict
    /// something to make room" would have broken. An established source keeps
    /// its own bucket, and its count, across the ceiling being reached.
    #[test]
    fn reaching_the_ceiling_never_flushes_an_established_source() {
        let l = EndpointLimiter::new();
        // An honest client spends its whole budget and is now refused.
        for _ in 0..OAUTH_LIMIT {
            assert!(l.allow(PAR, remote(7), AUTHORIZATION_BUDGET, 1_000));
        }
        assert!(!l.allow(PAR, remote(7), AUTHORIZATION_BUDGET, 1_000));

        for i in 0..(MAX_BUCKETS as u32 + 512) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(i.to_be_bytes()));
            l.allow(TOKEN, Some(ip), AUTHORIZATION_BUDGET, 1_000);
        }

        // Still refused: had the flood evicted its window, this would be a
        // fresh bucket and the refusal would have been laundered into an
        // allowance — a limiter a flood can RESET is worse than none.
        assert!(
            !l.allow(PAR, remote(7), AUTHORIZATION_BUDGET, 1_000),
            "a flood of fresh keys flushed an established source's window"
        );
    }

    /// Sources arriving at a full map share one bucket rather than each
    /// getting their own — bounded memory without a reset, and it is still a
    /// real limit rather than a free pass.
    #[test]
    fn sources_that_overflow_the_ceiling_share_one_bucket() {
        let l = EndpointLimiter::new();
        for i in 0..(MAX_BUCKETS as u32) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(i.to_be_bytes()));
            l.allow(TOKEN, Some(ip), AUTHORIZATION_BUDGET, 1_000);
        }
        let at_ceiling = l.bucket_count();

        // Every one of these is a source the map has no room for. They meter
        // together, so the budget runs out across them rather than per-source.
        let mut allowed = 0;
        for i in 0..(OAUTH_LIMIT * 4) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from((900_000 + i).to_be_bytes()));
            if l.allow(TOKEN, Some(ip), AUTHORIZATION_BUDGET, 1_000) {
                allowed += 1;
            }
        }
        assert!(
            allowed <= OAUTH_LIMIT,
            "overflowing sources were metered separately, not against one bucket: {allowed}"
        );
        assert_eq!(
            l.bucket_count(),
            at_ceiling + 1,
            "the overflow allocated a bucket per source instead of sharing ONE"
        );
    }

    /// **The overflow bucket is not the loopback bucket**, and this is the
    /// assertion that would have caught the first shape of this fix.
    ///
    /// Both were `(route, None)` for one commit. Sharing them puts a remote
    /// flood and the co-resident e2e harness in one window with two different
    /// limits — the flood spending loopback's thousand, loopback's traffic
    /// counting against the flood's ten. The populations must not mix.
    #[test]
    fn an_overflowing_remote_source_never_lands_in_the_loopback_bucket() {
        let l = EndpointLimiter::new();
        for i in 0..(MAX_BUCKETS as u32) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(i.to_be_bytes()));
            l.allow(TOKEN, Some(ip), AUTHORIZATION_BUDGET, 1_000);
        }
        // Spend the overflow bucket dry with remote sources the map has no
        // room for.
        for i in 0..(OAUTH_LIMIT * 4) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from((900_000 + i).to_be_bytes()));
            l.allow(TOKEN, Some(ip), AUTHORIZATION_BUDGET, 1_000);
        }
        // Loopback is a different party and must still have its own budget in
        // full — the local harness cannot be starved by a remote flood.
        let loopback = Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        for i in 0..OAUTH_LOOPBACK_LIMIT {
            assert!(
                l.allow(TOKEN, loopback, AUTHORIZATION_BUDGET, 1_000),
                "a remote flood consumed loopback's budget at request {i}"
            );
        }
    }
}

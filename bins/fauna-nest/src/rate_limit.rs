//! Global per-IP rate limiting middleware.
//!
//! Authority: `docs/goal/architecture/transport-connection.md` § Abuse posture — the
//! blanket per-source request-rate governor. Loopback is **bounded, not
//! exempt** here since 2026-08-30 (the shape ratified for the connection caps
//! 2026-08-25): a same-host caller is metered against [`LOOPBACK_MAX_RPS`],
//! aggregated over every loopback address, instead of skipping the check.
//!
//! Not to be confused with the **anonymous sliding-window throttles**
//! (`anonymous_rate_limit.rs`, owned by `federation.md`): those are per-kind,
//! per-source budgets on the account-creating writes and answer with the
//! `fauna.protocol.rate_limited` RPC error. This layer runs earlier and lower —
//! it answers a bare HTTP `429` before any RPC frame is parsed, so it cannot be
//! the source of that error code.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fauna_conn_limit::ShedCounter;
use governor::clock::DefaultClock;
use governor::state::keyed::DashMapStateStore;
use governor::{Quota, RateLimiter};
use std::num::NonZeroU32;
use tower_layer::Layer;
use tower_service::Service;

pub type IpRateLimiter = RateLimiter<IpAddr, DashMapStateStore<IpAddr>, DefaultClock>;

/// The loopback ceiling's limiter: **unkeyed**, unlike the per-IP one above.
///
/// "Same host" is one party, not several — `127.0.0.1`, `::1` and the rest of
/// `127.0.0.0/8` are the same set of co-resident processes sharing the same
/// CPU, so the thing worth bounding is their **sum**. Keying them would hand
/// each spelling its own full budget and let a runaway pick a fresh loopback
/// address to reset it. It also keeps the loopback path out of the keyed
/// `DashMap` entirely, so it cannot contribute to the growth
/// [`spawn_retain_sweeper`] exists to bound.
pub type AggregateRateLimiter =
    RateLimiter<governor::state::NotKeyed, governor::state::InMemoryState, DefaultClock>;

/// The ceiling a **loopback** source is metered against instead of the admin's
/// per-IP quota — a hard-coded safety bound, not a knob (`principles.md`
/// § One configuration surface, bucket 1: nobody chooses it, because the
/// loopback peers are the deployment artifact's own co-resident processes).
///
/// Sized from both ends, from a measurement taken 2026-08-30 on an ARM64 Linux
/// development VM against a **debug** nest on a **loaded** box — so every figure
/// below is a floor on what production hardware reaches, and the headroom is
/// stated against the floor.
///
/// **Above** — legitimate loopback traffic never approaches it. Every byteplane
/// fan-out loop in `bins/fauna-bridges` is serial and one-request-per-chunk (the
/// WebDAV download loop, `mailstage`'s chunk upload), and chunks are 512 KiB to
/// 8 MiB, so a stream's request rate is its bandwidth divided by its chunk size.
/// Measured, serially, on one kept-alive connection: **1 637 req/s** at the
/// 512 KiB minimum chunk, 513 at the 2 MiB average, 60 at the 8 MiB maximum.
/// Concurrency does **not** multiply it — the box is bandwidth-bound, so 2/4/8/16
/// parallel 2 MiB streams measured 922 / 1 651 / 1 395 / 1 173 req/s aggregate,
/// i.e. it *plateaus* near 1 650. That plateau is the number to clear, and 65 536
/// is ~40× it. It also has to clear the tier_3 e2e harness, which drives the
/// whole suite from `127.0.0.1` and is the most aggressive legitimate loopback
/// client this project has.
///
/// **Below** — it still catches the shape that motivated it. Paired with
/// [`fauna_conn_limit::LOOPBACK_MAX_CONNS`] (1024), 65 536 req/s is **64 requests
/// per second per permitted loopback connection**. A handful of real bulk streams
/// sit far under that in aggregate; a fleet of a thousand leaked co-resident
/// sockets each issuing 64 requests a second is unambiguously the 2026-08-22
/// runaway signature, and past this point the nest answers a cheap `429` instead
/// of doing the work, keeping its CPU for external clients.
///
/// ⚠ **This bound is a backstop, not a tight one, and the measurement says why.**
/// On loopback the *legitimate* peak (1 637 req/s) and the fastest a runaway can
/// go at all (a trivial 404 in a tight loop: 2 547 req/s median 2 439) are within
/// a factor of two of each other, because both are bounded by the same machine
/// rather than by the network. So a request-rate ceiling on loopback cannot
/// separate "busy" from "pathological" the way the *connection* ceiling can —
/// what separates them is connection count, which `LOOPBACK_MAX_CONNS` already
/// bounds. Sizing this tighter to make it bite would throttle the deployment's
/// own bridges, which is the one outcome worse than not having it.
///
/// Independent of the admin's per-IP cap in both directions, exactly as
/// [`fauna_conn_limit::LOOPBACK_MAX_CONNS`] is: an admin lowering the abuse cap
/// to a handful must not starve the co-resident bridge, and raising it must not
/// widen this safety bound.
pub const LOOPBACK_MAX_RPS: u32 = 65_536;

pub fn new_ip_limiter(per_second: u32) -> Arc<IpRateLimiter> {
    let quota = Quota::per_second(NonZeroU32::new(per_second).unwrap());
    Arc::new(RateLimiter::dashmap(quota))
}

/// The unkeyed limiter the loopback aggregate is metered against.
pub fn new_loopback_limiter(per_second: u32) -> Arc<AggregateRateLimiter> {
    let quota = Quota::per_second(NonZeroU32::new(per_second).unwrap());
    Arc::new(RateLimiter::direct(quota))
}

/// Spawn a periodic GC task that shrinks the limiter's keyed `DashMap`.
///
/// governor's keyed limiter never sheds entries on its own, so without this the
/// map grows by one entry per distinct client IP ever seen — an unbounded leak
/// the moment the SNI router restores real (and rotating) client IPs
/// (a security-review finding, tracked internally; moot
/// while every client collapsed to `127.0.0.1`, live now that PROXY protocol
/// resolves the real source). `retain_recent` keeps only the buckets that could
/// still be rate-limiting (those that haven't fully replenished) and drops the
/// rest; a returning IP simply re-seeds a fresh full bucket, so the GC never
/// weakens the limit. Mirrors the sweeper the sliding-window
/// `bridge_rate_limit` limiters already run (`bridge_rate_limit::spawn_sweeper`).
pub fn spawn_retain_sweeper(
    limiter: Arc<IpRateLimiter>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            limiter.retain_recent();
        }
    })
}

#[derive(Clone)]
pub struct RateLimitLayer {
    limiter: Arc<IpRateLimiter>,
    loopback: Arc<AggregateRateLimiter>,
    shed: Arc<ShedCounter>,
}

impl RateLimitLayer {
    pub fn new(limiter: Arc<IpRateLimiter>) -> Self {
        Self {
            limiter,
            loopback: new_loopback_limiter(LOOPBACK_MAX_RPS),
            shed: Arc::new(ShedCounter::new()),
        }
    }

    /// Same, with a caller-supplied shed counter — so a test can hold the
    /// counter this layer reports through and assert the reporting decision
    /// itself, without a wall-clock wait or a log-capture harness.
    pub fn with_shed(limiter: Arc<IpRateLimiter>, shed: Arc<ShedCounter>) -> Self {
        Self {
            limiter,
            loopback: new_loopback_limiter(LOOPBACK_MAX_RPS),
            shed,
        }
    }

    /// Same, with an explicit loopback ceiling. A test seam — production always
    /// takes [`LOOPBACK_MAX_RPS`], and the seam exists for the same reason
    /// [`fauna_conn_limit::PerIpConnLimit::with_loopback_ceiling`] does: driving
    /// the real bound would take 65 537 requests, so a test that had to do that
    /// would be measuring the machine instead of the decision.
    pub fn with_loopback_ceiling(
        limiter: Arc<IpRateLimiter>,
        shed: Arc<ShedCounter>,
        loopback_per_second: u32,
    ) -> Self {
        Self {
            limiter,
            loopback: new_loopback_limiter(loopback_per_second),
            shed,
        }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: self.limiter.clone(),
            loopback: self.loopback.clone(),
            shed: self.shed.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    limiter: Arc<IpRateLimiter>,
    /// The unkeyed ceiling every **loopback** source is metered against instead
    /// of `limiter` — [`LOOPBACK_MAX_RPS`] in production. Loopback is bounded,
    /// not exempt (`transport-connection.md` § Abuse posture, the shape ratified for the
    /// connection caps 2026-08-25).
    loopback: Arc<AggregateRateLimiter>,
    /// Rate-limited reporter for "this governor is shedding". A cap that sheds
    /// silently is indistinguishable from a quiet night — the invisibility that
    /// kept the 2026-07-31 router outage off the log entirely — but a `warn!`
    /// per rejection would flood at request rate. Shared with
    /// `fauna_conn_limit`'s connection caps rather than reinvented, so every
    /// ceiling in the nest reports in one shape.
    shed: Arc<ShedCounter>,
}

impl<S, B> Service<axum::http::Request<B>> for RateLimitService<S>
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
        let loopback = self.loopback.clone();
        let shed = self.shed.clone();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let ip = req
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ci| ci.0.ip());
            // Loopback (same-host) callers are **bounded, not exempt**
            // (`transport-connection.md` § Abuse posture — the shape ratified for the
            // connection caps 2026-08-25, applied here 2026-08-30). A process
            // reaching nest over loopback is inside the deployment trust
            // boundary (the same principle that gates
            // `fauna.bridges.request_enrollment` — see
            // `pre_identity_allowlist::requires_loopback_peer`) and must never be
            // throttled at the admin's per-IP rate: the in-container mail bridge
            // legitimately streams the byteplane far past it. But trusted is not
            // unmetered — the 2026-08-22 incident was a co-resident e2e client
            // leaking 16 311 sockets while the nest reported a quiet night — so
            // it is metered against the hard-coded [`LOOPBACK_MAX_RPS`] instead,
            // aggregated over every same-host caller rather than keyed per
            // loopback address.
            //
            // A *remote* client is unaffected in either direction: behind the SNI
            // router the PROXY-v2 header resolves the real internet source into
            // `ConnectInfo` (`lib.rs` `read_optional_proxy_header`), so an
            // external client is keyed on its real IP and only a genuine
            // same-host peer reaches the loopback arm here.
            if let Some(ip) = ip {
                let over = if ip.is_loopback() {
                    loopback.check().is_err()
                } else {
                    limiter.check_key(&ip).is_err()
                };
                if over {
                    // Say so, at most one line per `SHED_LOG_INTERVAL`, carrying
                    // the batch count. Before this the governor's sheds were
                    // *completely* invisible: this module emitted no line of any
                    // level, so a nest throttling a client at 100 rps and a nest
                    // with no traffic at all produced identical logs.
                    if let Some(batch) = shed.record() {
                        // Name WHICH ceiling was reached: the two have different
                        // remedies (an admin can raise the per-IP quota; the
                        // loopback ceiling is a constant and a hit means a
                        // co-resident process is misbehaving).
                        let which = if ip.is_loopback() {
                            "loopback aggregate"
                        } else {
                            "per-IP"
                        };
                        // `(sample: …)` either way: `shed` is ONE counter
                        // shared by BOTH branches, so on the per-IP branch it
                        // sums sheds across every source IP, not just `ip`;
                        // on the loopback branch the ceiling itself is a
                        // same-host aggregate (deliberately not keyed per
                        // loopback address), so `ip` there is one example of
                        // a many-caller total either way.
                        tracing::warn!(
                            "{which} request-rate governor reached; shedding \
                             (sample: {ip}) ({batch} shed since last line)"
                        );
                    }
                    return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
                }
            }
            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// Minimal always-ready inner service that answers `200 OK`, so a 429 in a
    /// test can only have come from the `RateLimitService` layer.
    #[derive(Clone)]
    struct OkService;

    impl Service<Request<Body>> for OkService {
        type Response = Response;
        type Error = std::convert::Infallible;
        type Future = std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Response, Self::Error>> + Send>,
        >;
        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            Box::pin(async { Ok(StatusCode::OK.into_response()) })
        }
    }

    fn req_from(ip: IpAddr) -> Request<Body> {
        let mut req = Request::builder().uri("/").body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 12345)));
        req
    }

    async fn call_status(svc: &mut RateLimitService<OkService>, ip: IpAddr) -> StatusCode {
        svc.call(req_from(ip)).await.unwrap().status()
    }

    #[tokio::test]
    async fn remote_ip_is_throttled_once_quota_is_spent() {
        // 1 req/s, burst 1: the second back-to-back call from the same remote IP
        // exceeds the quota and gets 429 — the limiter is wired and live.
        let mut svc = RateLimitLayer::new(new_ip_limiter(1)).layer(OkService);
        let remote = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        assert_eq!(call_status(&mut svc, remote).await, StatusCode::OK);
        assert_eq!(
            call_status(&mut svc, remote).await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    /// Property 1 of the *Loopback is bounded, not exempt* shape: **the
    /// loopback ceiling is independent of the admin's per-IP quota.**
    ///
    /// The same tiny 1 req/s quota that 429s a remote IP on its 2nd call must
    /// not touch a loopback source — the in-container bridge, local tooling and
    /// the tier_3 harness all stream far past any abuse quota an admin would set
    /// for the internet, and an admin tightening that quota must never starve
    /// the deployment's own co-resident processes.
    #[tokio::test]
    async fn the_loopback_ceiling_is_independent_of_the_admin_per_ip_quota() {
        let mut svc = RateLimitLayer::new(new_ip_limiter(1)).layer(OkService);
        for _ in 0..50 {
            assert_eq!(
                call_status(&mut svc, IpAddr::V4(Ipv4Addr::LOCALHOST)).await,
                StatusCode::OK
            );
            assert_eq!(
                call_status(&mut svc, IpAddr::V6(Ipv6Addr::LOCALHOST)).await,
                StatusCode::OK
            );
        }
    }

    /// Property 2, and the one this row exists for: **loopback is METERED, not
    /// exempt.** Before 2026-08-30 the layer short-circuited on
    /// `!ip.is_loopback()`, so no number of same-host requests could ever be
    /// shed; now they are counted against [`LOOPBACK_MAX_RPS`].
    ///
    /// Driven through the ceiling seam rather than the real 65 536: sending
    /// 65 537 requests would measure the machine, not the decision (e2e
    /// convention 14's discipline, applied to a unit test).
    #[tokio::test]
    async fn a_loopback_source_is_metered_against_the_loopback_ceiling() {
        let shed = Arc::new(ShedCounter::with_interval(std::time::Duration::from_secs(
            3600,
        )));
        // A generous per-IP quota, so a 429 here can only be the loopback
        // ceiling — never the admin cap leaking into the loopback arm.
        let mut svc =
            RateLimitLayer::with_loopback_ceiling(new_ip_limiter(10_000), Arc::clone(&shed), 2)
                .layer(OkService);
        let local = IpAddr::V4(Ipv4Addr::LOCALHOST);

        assert_eq!(call_status(&mut svc, local).await, StatusCode::OK);
        assert_eq!(call_status(&mut svc, local).await, StatusCode::OK);
        assert_eq!(
            call_status(&mut svc, local).await,
            StatusCode::TOO_MANY_REQUESTS,
            "the third same-host request in one second exceeded a ceiling of 2 \
             and must be shed — loopback is bounded, not exempt"
        );
    }

    /// Property 2b: the ceiling is an **aggregate over same-host callers**, not
    /// a per-loopback-address budget.
    ///
    /// `127.0.0.1`, `::1` and the rest of `127.0.0.0/8` are the same set of
    /// co-resident processes on the same CPU, so they share one bucket. Keying
    /// them would hand each spelling a full budget and let a runaway reset its
    /// own limit by picking a fresh loopback address — which is why the loopback
    /// limiter is unkeyed while the remote one is keyed.
    #[tokio::test]
    async fn the_loopback_ceiling_aggregates_over_every_same_host_address() {
        let shed = Arc::new(ShedCounter::with_interval(std::time::Duration::from_secs(
            3600,
        )));
        let mut svc =
            RateLimitLayer::with_loopback_ceiling(new_ip_limiter(10_000), Arc::clone(&shed), 2)
                .layer(OkService);

        // Two different loopback spellings spend the SAME budget…
        assert_eq!(
            call_status(&mut svc, IpAddr::V4(Ipv4Addr::LOCALHOST)).await,
            StatusCode::OK
        );
        assert_eq!(
            call_status(&mut svc, IpAddr::V6(Ipv6Addr::LOCALHOST)).await,
            StatusCode::OK
        );
        // …so a THIRD, from yet another loopback address, is shed.
        assert_eq!(
            call_status(&mut svc, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))).await,
            StatusCode::TOO_MANY_REQUESTS,
            "a fresh loopback address must not reset the ceiling — same host, \
             same bucket"
        );
    }

    /// The production constant is the one wired, not a placeholder — and it is
    /// the value the doc comment's measurement justifies. A silent drift here
    /// (someone "tuning" it to make a test bite) is exactly what the doc comment
    /// argues against, so it is pinned.
    #[test]
    fn the_production_loopback_ceiling_is_the_measured_constant() {
        assert_eq!(LOOPBACK_MAX_RPS, 65_536);
        assert_eq!(
            LOOPBACK_MAX_RPS as u64,
            64 * fauna_conn_limit::LOOPBACK_MAX_CONNS as u64,
            "the doc comment's below-end argument is that the ceiling is 64 \
             requests/second per permitted loopback connection; if either \
             constant moves, that sentence has to be re-derived, not silently \
             falsified"
        );
    }

    /// A shed must reach the log. Until this landed the governor was **totally
    /// silent** — `rate_limit.rs` emitted no line at any level — so a nest
    /// throttling a client and a nest with no traffic at all produced identical
    /// logs. That is the same invisibility that kept the 2026-07-31 router
    /// outage off the record, and the reason `fauna_conn_limit` grew
    /// [`ShedCounter`] in the first place.
    ///
    /// Asserted through the counter the layer reports *through*, rather than by
    /// capturing log output: the counter's contract is that the first shed after
    /// a quiet period always reports and the rest are suppressed until the
    /// interval elapses. So if the service recorded its shed, the test's own
    /// follow-up `record()` is suppressed; if the service recorded nothing, that
    /// same call would be the first shed and would report. The 1-hour interval
    /// never elapses during the test, so the assertion is latency-independent
    /// (e2e convention 14) — no sleep, no wall-clock dependence.
    ///
    /// What this does not pin is the `warn!` macro call itself, only the
    /// rate-limited decision that gates it; the two are adjacent lines.
    #[tokio::test]
    async fn a_shed_is_reported_through_the_rate_limited_counter() {
        let shed = Arc::new(ShedCounter::with_interval(std::time::Duration::from_secs(
            3600,
        )));
        let mut svc =
            RateLimitLayer::with_shed(new_ip_limiter(1), Arc::clone(&shed)).layer(OkService);
        let remote = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));

        assert_eq!(call_status(&mut svc, remote).await, StatusCode::OK);
        assert_eq!(
            call_status(&mut svc, remote).await,
            StatusCode::TOO_MANY_REQUESTS
        );

        assert!(
            shed.record().is_none(),
            "the throttled request did not report a shed — this counter's \
             first-shed-reports-immediately slot was still unspent, so the \
             governor shed silently"
        );
    }

    /// Property 3: **a loopback shed reaches the log.**
    ///
    /// ⚠ This test is the INVERTED form of `an_exempt_loopback_source_reports_no_shed`,
    /// which stood here until 2026-08-30 asserting that loopback consumed no
    /// shed slot *because it was exempt*. It was left as the declared seam for
    /// this change, so that flipping it would have to be deliberate. It is.
    ///
    /// A loopback source under the ceiling still reports nothing (an unthrottled
    /// path must not put lines in the log for traffic it served); one that
    /// crosses the ceiling reports, through the same rate-limited counter the
    /// remote arm uses. Asserted through the counter rather than by capturing
    /// log output, for the reason
    /// `a_shed_is_reported_through_the_rate_limited_counter` states: the
    /// counter's first-shed-reports-immediately contract makes the assertion
    /// latency-independent — no sleep, no wall-clock dependence.
    #[tokio::test]
    async fn a_loopback_shed_is_reported_through_the_rate_limited_counter() {
        let shed = Arc::new(ShedCounter::with_interval(std::time::Duration::from_secs(
            3600,
        )));
        let mut svc =
            RateLimitLayer::with_loopback_ceiling(new_ip_limiter(10_000), Arc::clone(&shed), 2)
                .layer(OkService);
        let local = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // Under the ceiling: served, and nothing reported.
        assert_eq!(call_status(&mut svc, local).await, StatusCode::OK);
        assert_eq!(call_status(&mut svc, local).await, StatusCode::OK);

        // Over it: shed, and the shed is reported — so the service consumed the
        // counter's first-shed slot and our own `record()` is suppressed.
        assert_eq!(
            call_status(&mut svc, local).await,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert!(
            shed.record().is_none(),
            "the loopback shed did not reach the log — this counter's \
             first-shed slot was still unspent, so a co-resident process \
             hammering the nest would shed in complete silence, which is the \
             exact invisibility of the 2026-08-22 socket leak"
        );
    }

    /// The other half of property 3, kept honest: a loopback source **under**
    /// the ceiling consumes no shed slot. Without this, a layer that reported on
    /// every loopback request would pass the test above for the wrong reason.
    #[tokio::test]
    async fn a_loopback_source_under_the_ceiling_reports_no_shed() {
        let shed = Arc::new(ShedCounter::with_interval(std::time::Duration::from_secs(
            3600,
        )));
        let mut svc = RateLimitLayer::with_loopback_ceiling(
            new_ip_limiter(10_000),
            Arc::clone(&shed),
            10_000,
        )
        .layer(OkService);

        for _ in 0..50 {
            assert_eq!(
                call_status(&mut svc, IpAddr::V4(Ipv4Addr::LOCALHOST)).await,
                StatusCode::OK
            );
        }

        assert_eq!(
            shed.record(),
            Some(1),
            "nothing was shed, so this counter's first-shed slot must still be \
             unspent"
        );
    }
}

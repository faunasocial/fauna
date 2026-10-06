//! Accept-loop connection defence: a per-source-IP concurrent-connection cap,
//! the TCP dead-peer detection that bounds each permit's lifetime, and
//! rate-limited shed reporting.
//!
//! # Why dead-peer detection lives in this crate
//!
//! A per-IP cap is only as good as the *release* half of its RAII permit. On
//! 2026-07-31 example.com's `:443` went fully dark for ~7 days' worth of
//! accumulated traffic: ~261 `ESTABLISHED` sockets from one source IP sat in
//! `fauna-sni-router` forever, exhausting that IP's 256-slot budget, so every
//! new connection was shed at accept time — TCP connected, the server sent zero
//! bytes and no certificate. The router had not crashed and the cap was working
//! exactly as written. The permits had simply never come back, because
//! [`PerIpPermit`] lives as long as its connection task, the task lives as long
//! as the splice, and **nothing bounded the splice**: a peer that disappears
//! without a clean TCP close (a suspended VM, a dropped link, a killed test, a
//! NAT rebind) leaves a socket that is `ESTABLISHED` forever, since SO_KEEPALIVE
//! is off by default. One vanished peer = one permanently burned slot.
//!
//! So the cap and the thing that bounds it are one concern and are kept in one
//! crate on purpose. Adding a new caller of [`PerIpConnLimit`] without also
//! calling [`arm_dead_peer_detection`] re-introduces the same outage; keeping
//! them adjacent is what makes that visible to whoever reads this next.
//!
//! **Why TCP keepalive rather than an idle timer on the connection.** The
//! router is an L4 splicer: it cannot tell a peer that is *gone* from one that
//! is merely *quiet*, and both look identical to a byte-counting timer. Quiet
//! is legitimate here — a CalDAV client parked on a keep-alive connection, an
//! idle nostr relay subscription, a browser WS-RPC socket (browsers cannot send
//! WS Ping frames from JS at all). Keepalive asks the peer's *kernel*, which
//! answers whether or not the peer's application has anything to say, so it
//! separates the two cases that an idle ceiling conflates. It is also
//! protocol-agnostic, which matters because the router carries nest, CalDAV,
//! relay and PDS traffic through the same splice.
//!
//! This is a backstop, not the only line of defence: fauna's own clients run a
//! 30 s WS Ping / 60 s dead-link heartbeat above it
//! (`fauna_ws_substrate::adapter::KEEPALIVE_INTERVAL`). That heartbeat is
//! faster, but it only covers connections speaking fauna's WS protocol with a
//! fauna client on the far end — which is why the fix belongs *below* it, at
//! the layer that actually owns the permit.
//!
//! # The cap
//!
//! A global connection `Semaphore` (an OOM/FD backstop bounding *total*
//! concurrent connections) lets a single source fill the whole pool. This adds
//! the finer per-source layer the exposed-ports security review's
//! D-items call for: one abusive source can hold at most `max_per_ip`
//! concurrent connections instead of the entire global cap.
//!
//! Two callers share this crate (priority #2 — one shape, not a copy per
//! binary):
//! - **`fauna-nest`**'s one shared accept loop (`serve_tls` and `serve_plain`
//!   — every listener the nest binds), keyed on the **real client IP** the
//!   PROXY-v2 fix resolves (`source_addr`), not the fronting router's loopback.
//! - **`fauna-sni-router`**, keyed on the TCP peer it accepts directly (the
//!   router is the front-most hop, so the peer *is* the real client).
//!
//! **Loopback is bounded by its own ceiling, not by the admin's cap.** The
//! in-container mail bridge dials nest's loopback directly (headerless — its
//! self-enrollment rides the loopback trust gate), the router itself reaches
//! nest over loopback, and on a same-box deploy the apps do too. Those are the
//! deployment artifact's own processes, so they are not subject to the
//! admin-tunable abuse cap (an admin tightening it to a handful must never
//! starve the bridge) — but they are not *unbounded* either. Until 2026-08-22
//! loopback was exempt outright, and one leaking same-box client accumulated
//! **16,311** accepted-and-never-closed loopback sockets on a nest that logged
//! nothing, exhausting the machine's whole network state (every new outbound
//! TCP connection box-wide failed for hours). So loopback is counted like any
//! other source, against [`LOOPBACK_MAX_CONNS`] instead of `max_per_ip`: far
//! above any legitimate co-resident fleet, far below the global pool, and a
//! shed at that ceiling is the warn line that makes the leak visible.
//! External clients arrive with a non-loopback (PROXY-conveyed, for nest;
//! directly-observed, for the router) IP and are capped by the admin's value.
//!
//! The count map is GC'd as connections close (an entry is removed when its
//! count returns to zero), so it can't grow unbounded from rotating source IPs
//! — the same leak class the governor sweeper (review § D11) closed for the
//! rate limiter.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::TcpStream;

/// How long an accepted connection may sit completely silent before the kernel
/// starts probing whether the peer is still reachable.
///
/// Comfortably above fauna's own 30 s client WS Ping cadence, so a healthy
/// fauna client never triggers a probe at all — probes fire only on links that
/// are genuinely quiet (idle CalDAV/relay sockets, or peers that are gone).
pub const KEEPALIVE_IDLE: Duration = Duration::from_secs(120);

/// Gap between probes once probing has started.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Unanswered probes before the kernel declares the connection dead and fails
/// the next read/write with `ETIMEDOUT` — which ends the connection task and
/// drops its [`PerIpPermit`].
///
/// With the values above, a vanished peer is reaped within
/// `KEEPALIVE_IDLE + KEEPALIVE_RETRIES * KEEPALIVE_INTERVAL` ≈ 4 minutes. The
/// leak that motivated this accrued over days, so minutes is ample; the budget
/// is deliberately unhurried so a brief network blip never costs a live client
/// its connection.
pub const KEEPALIVE_RETRIES: u32 = 4;

/// Arm TCP keepalive on an accepted connection so a peer that vanishes without
/// a clean close is detected and its [`PerIpPermit`] released.
///
/// Call this on every socket an accept loop admits, next to
/// [`PerIpConnLimit::try_acquire`] — see the crate docs for why the two belong
/// together. Failure is not fatal to the connection (the caller should log and
/// carry on); it only means this one connection falls back to the OS default
/// idle of ~2 h.
///
/// Only the *accepted* socket needs this. A backend socket the process dials
/// over loopback has no peer that can vanish, and when the accepted side errors
/// out the whole connection task ends anyway, closing the backend socket with
/// it.
pub fn arm_dead_peer_detection(stream: &TcpStream) -> std::io::Result<()> {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL);
    // Probe count is not settable on every platform socket2 supports; the
    // deployment target (Linux, in the nest image) is one of the ones where it
    // is. Elsewhere we still get SO_KEEPALIVE with the idle + interval above and
    // the OS default probe count, which is enough to bound the permit.
    #[cfg(not(any(windows, target_os = "openbsd", target_os = "solaris")))]
    let keepalive = keepalive.with_retries(KEEPALIVE_RETRIES);
    socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive)
}

/// Minimum gap between shed log lines, per [`ShedCounter`].
pub const SHED_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Rate limiter for "I am shedding connections" logging.
///
/// A cap that sheds silently is indistinguishable from a quiet night, which is
/// precisely how the 2026-07-31 outage stayed invisible: both shed paths logged
/// at `debug!` while the router's default filter is `info`, so a *fully*
/// shedding router emitted **zero** lines. But a shed is also exactly the
/// condition under which a naive `warn!` per rejection would flood the log at
/// connection-attempt rate. So: count every shed, surface at most one line per
/// [`SHED_LOG_INTERVAL`], and carry the batch count on that line so the reader
/// sees the true rate rather than a single sample.
///
/// The first shed after a quiet period always reports immediately — the edge
/// into shedding is the interesting event.
pub struct ShedCounter {
    interval: Duration,
    state: Mutex<ShedState>,
}

struct ShedState {
    /// Sheds recorded since the last reported line (including unreported ones).
    since_report: u64,
    /// When the last line was reported; `None` until the first shed.
    last_report: Option<Instant>,
}

impl ShedCounter {
    pub fn new() -> Self {
        Self::with_interval(SHED_LOG_INTERVAL)
    }

    /// Same, with an explicit interval — lets tests assert the rate-limiting
    /// decision itself without a wall-clock wait (convention 14: assert
    /// latency-independent state).
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            interval,
            state: Mutex::new(ShedState {
                since_report: 0,
                last_report: None,
            }),
        }
    }

    /// Record one shed. Returns `Some(n)` when the caller should log, where `n`
    /// is how many sheds that line stands for (this one included); `None` when
    /// the line is being suppressed by the rate limit.
    pub fn record(&self) -> Option<u64> {
        let mut state = self.state.lock().unwrap();
        state.since_report += 1;
        let now = Instant::now();
        let due = match state.last_report {
            None => true,
            Some(last) => now.duration_since(last) >= self.interval,
        };
        if !due {
            return None;
        }
        state.last_report = Some(now);
        Some(std::mem::replace(&mut state.since_report, 0))
    }
}

impl Default for ShedCounter {
    fn default() -> Self {
        Self::new()
    }
}

/// Default per-source concurrent-connection cap. Generous enough for a real
/// client (several actors × a WS each) or a household behind one NAT IP, while
/// still bounding one source to a fraction of the global pool (the nest accept
/// loop and the router both default their global cap to 4096).
pub const DEFAULT_MAX_CONNS_PER_IP: usize = 256;

/// The ceiling on concurrent connections from a **loopback** source — a
/// hard-coded safety bound, not a knob (`principles.md` § One configuration
/// surface, bucket 1: nobody chooses it, because the loopback peers are the
/// deployment artifact's own co-resident processes, whose legitimate count is
/// a property of the artifact, not a preference).
///
/// Sized from both ends. Above: the bridges dial nest once per *process* (one
/// WS-RPC client per bridge binary plus its anonymous bearer refreshes — single
/// digits), the router's headerless loopback traffic is a handful, and a
/// same-box household of apps is tens; 1024 is two orders of magnitude clear of
/// all of it together. Below: it is a quarter of the 4096 global default, so a
/// leaking co-resident process can never take more than that fraction of the
/// pool from external clients, and its `ESTABLISHED` sockets stay a small
/// fraction of any machine's ephemeral-port range (the 2026-08-22 leak reached
/// 16 k and killed the box's networking at ~32 k sockets).
///
/// Independent of the admin's per-IP cap on purpose: an admin lowering the
/// abuse cap to a handful must not starve the bridge, and raising it must not
/// widen the safety bound.
///
/// **This ceiling only binds a connection AFTER its source resolves.** Between
/// accept and resolution — the PROXY-v2 header read `bins/fauna-nest/src/lib.rs`
/// does over the loopback socket — a stalling peer holds a global permit but
/// is not yet charged against this ceiling at all. [`HEADER_PARSE_MAX_CONCURRENT`]
/// closes that gap: it bounds how many loopback connections may be mid-parse
/// at once, so a leaking co-resident process can hold at most
/// `HEADER_PARSE_MAX_CONCURRENT` global permits pre-resolution *plus*
/// `LOOPBACK_MAX_CONNS` post-resolution — both fixed, hard-coded fractions of
/// the global pool, never the whole of it.
pub const LOOPBACK_MAX_CONNS: usize = 1024;

/// Bounds how many loopback connections may be concurrently mid-PROXY-header-
/// parse — the window between the nest accept loop taking its **global**
/// permit and resolving the connection's real source (`read_optional_proxy_header`
/// in `bins/fauna-nest/src/lib.rs`), during which [`LOOPBACK_MAX_CONNS`] above
/// does not yet bind the connection at all. A hard-coded safety bound, not a
/// knob — same bucket-1 class as `LOOPBACK_MAX_CONNS` (`principles.md` § One
/// configuration surface).
///
/// Acquired with a non-blocking `try_acquire` before the header read starts;
/// a full gate sheds the connection immediately rather than let it join a
/// stall it can't yet be distinguished from.
///
/// Sized from both ends. Above: a legitimate router-forwarded connection has
/// its header already buffered by the router before nest even accepts the
/// socket, so it clears the gate in microseconds — real concurrent holders sit
/// far below this number even under heavy legitimate load, so 512 costs
/// nothing in practice. Below: it is half of `LOOPBACK_MAX_CONNS` and an
/// eighth of the 4096 global default, so a leaking co-resident process
/// sending nothing (or one stalling byte) can occupy at most this many global
/// permits before resolution — a small, fixed fraction, not the pool.
pub const HEADER_PARSE_MAX_CONCURRENT: usize = 512;

/// Tracks live connection counts per source IP and enforces a per-IP ceiling.
///
/// The ceiling is an [`AtomicUsize`] so it can be **hot-reloaded** without
/// rebuilding the limiter (or tearing down the accept loop holding the `Arc`):
/// an admin's `fauna.transport.put_policy` calls [`set_max`](Self::set_max) and
/// the live `serve_tls` accept hot-path observes the new cap on its next
/// `try_acquire`. Relaxed ordering is sufficient — this is an advisory abuse
/// cap, not a synchronization primitive, so a connection arriving exactly as the
/// cap changes may use either the old or new value (both acceptable).
pub struct PerIpConnLimit {
    max_per_ip: AtomicUsize,
    /// The ceiling a loopback source is counted against instead of
    /// `max_per_ip` — [`LOOPBACK_MAX_CONNS`] in production; settable only at
    /// construction ([`with_loopback_ceiling`](Self::with_loopback_ceiling))
    /// so a test can drive the shed path without 1024 real sockets.
    loopback_max: usize,
    counts: Mutex<HashMap<IpAddr, usize>>,
}

impl PerIpConnLimit {
    /// A limiter with the production loopback ceiling ([`LOOPBACK_MAX_CONNS`]).
    pub fn new(max_per_ip: usize) -> Arc<Self> {
        Self::with_loopback_ceiling(max_per_ip, LOOPBACK_MAX_CONNS)
    }

    /// Same, with an explicit loopback ceiling. A test seam — production
    /// callers use [`new`](Self::new); the ceiling is a constant, not config.
    pub fn with_loopback_ceiling(max_per_ip: usize, loopback_max: usize) -> Arc<Self> {
        Arc::new(Self {
            max_per_ip: AtomicUsize::new(max_per_ip),
            loopback_max,
            counts: Mutex::new(HashMap::new()),
        })
    }

    /// Replace the per-source ceiling live (hot-reload). The change binds on the
    /// next [`try_acquire`](Self::try_acquire); connections already admitted keep
    /// their slots. Used by the `fauna.transport.put_policy` Admin handler so a
    /// cap change takes effect without a nest restart.
    pub fn set_max(&self, max_per_ip: usize) {
        self.max_per_ip.store(max_per_ip, Ordering::Relaxed);
    }

    /// Try to admit one connection from `ip`. Returns a [`PerIpPermit`] (which
    /// decrements on drop) on success, or `None` if `ip` is already at its
    /// ceiling — the admin's cap for an external source, the hard-coded
    /// [`LOOPBACK_MAX_CONNS`] for a loopback one (see
    /// [`ceiling_for`](Self::ceiling_for)).
    pub fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Option<PerIpPermit> {
        let ceiling = self.ceiling_for(ip);
        let mut counts = self.counts.lock().unwrap();
        let n = counts.entry(ip).or_insert(0);
        if *n >= ceiling {
            // Drop the borrow before returning; leave the (possibly-zero-only-if
            // -newly-inserted) entry — but a freshly-inserted 0 that we reject
            // would leak, so clean it up.
            if *n == 0 {
                counts.remove(&ip);
            }
            return None;
        }
        *n += 1;
        Some(PerIpPermit {
            limit: Arc::clone(self),
            ip,
        })
    }

    fn release(&self, ip: IpAddr) {
        let mut counts = self.counts.lock().unwrap();
        if let Some(n) = counts.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                counts.remove(&ip); // GC: no unbounded growth from rotating IPs
            }
        }
    }

    /// The configured per-source ceiling (for shed-log lines / diagnostics).
    /// Reads the live value, so it reflects any [`set_max`](Self::set_max).
    /// This is the *admin's* cap; a loopback source is bounded by
    /// [`ceiling_for`](Self::ceiling_for) instead.
    pub fn max(&self) -> usize {
        self.max_per_ip.load(Ordering::Relaxed)
    }

    /// The ceiling `ip` is counted against: [`max`](Self::max) for an external
    /// source, the loopback ceiling for a loopback one. A shed line should
    /// report this, not [`max`](Self::max), so a loopback shed is never
    /// misreported as the admin's cap biting.
    pub fn ceiling_for(&self, ip: IpAddr) -> usize {
        if ip.is_loopback() {
            self.loopback_max
        } else {
            self.max_per_ip.load(Ordering::Relaxed)
        }
    }

    /// Current number of tracked source IPs (test/diagnostic).
    #[cfg(test)]
    fn tracked_ips(&self) -> usize {
        self.counts.lock().unwrap().len()
    }
}

/// RAII permit: holds one per-IP connection slot for its lifetime, releasing it
/// on drop (when the connection's task ends). Loopback permits hold a slot
/// too — counted against the loopback ceiling.
pub struct PerIpPermit {
    limit: Arc<PerIpConnLimit>,
    ip: IpAddr,
}

impl Drop for PerIpPermit {
    fn drop(&mut self) {
        self.limit.release(self.ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn admits_up_to_cap_then_rejects() {
        let limit = PerIpConnLimit::new(2);
        let a = ip("203.0.113.7");
        let p1 = limit.try_acquire(a);
        let p2 = limit.try_acquire(a);
        assert!(p1.is_some() && p2.is_some(), "first two must be admitted");
        assert!(
            limit.try_acquire(a).is_none(),
            "third must be rejected at cap"
        );
    }

    #[test]
    fn release_frees_a_slot_and_gcs_the_entry() {
        let limit = PerIpConnLimit::new(1);
        let a = ip("203.0.113.7");
        let p1 = limit.try_acquire(a);
        assert!(p1.is_some());
        assert!(limit.try_acquire(a).is_none(), "at cap");
        drop(p1); // releases the slot
        assert_eq!(limit.tracked_ips(), 0, "entry GC'd when count hit zero");
        assert!(limit.try_acquire(a).is_some(), "slot freed, re-admittable");
    }

    #[test]
    fn distinct_ips_are_independent() {
        let limit = PerIpConnLimit::new(1);
        let a = limit.try_acquire(ip("203.0.113.7"));
        let b = limit.try_acquire(ip("203.0.113.8"));
        assert!(
            a.is_some() && b.is_some(),
            "different sources don't share the cap"
        );
    }

    /// The 2026-08-22 regression lock: loopback used to be exempt outright,
    /// and one leaking same-box client held 16 k accepted sockets on a nest
    /// that shed nothing. Loopback is now counted against its own ceiling.
    #[test]
    fn loopback_is_bounded_by_its_own_ceiling() {
        let limit = PerIpConnLimit::with_loopback_ceiling(1, 3);
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let held: Vec<_> = (0..3).map(|_| limit.try_acquire(lo)).collect();
        assert!(
            held.iter().all(|p| p.is_some()),
            "admitted up to the loopback ceiling (past the admin cap of 1)"
        );
        assert!(
            limit.try_acquire(lo).is_none(),
            "the connection past the loopback ceiling is shed"
        );
        assert_eq!(limit.tracked_ips(), 1, "loopback IS tracked in the map");
        drop(held);
        assert_eq!(limit.tracked_ips(), 0, "loopback entry GC'd on release");
        assert!(
            limit.try_acquire(lo).is_some(),
            "released slots are re-admittable"
        );
        let lo6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(
            limit.ceiling_for(lo6),
            3,
            "IPv6 loopback is bounded by the same ceiling"
        );
    }

    #[test]
    fn loopback_ceiling_is_independent_of_the_admin_cap() {
        let limit = PerIpConnLimit::with_loopback_ceiling(256, 2);
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let ext = ip("203.0.113.7");
        // Tightening the abuse cap to nothing must not starve loopback...
        limit.set_max(0);
        assert!(
            limit.try_acquire(ext).is_none(),
            "external source capped at 0"
        );
        let _a = limit
            .try_acquire(lo)
            .expect("loopback unaffected by set_max(0)");
        let _b = limit
            .try_acquire(lo)
            .expect("loopback unaffected by set_max(0)");
        // ...and widening it must not widen the safety bound.
        limit.set_max(10_000);
        assert!(
            limit.try_acquire(lo).is_none(),
            "loopback still shed at its own ceiling after set_max(10_000)"
        );
        assert_eq!(limit.ceiling_for(lo), 2);
        assert_eq!(limit.ceiling_for(ext), 10_000);
    }

    #[test]
    fn production_constructor_uses_the_loopback_constant() {
        let limit = PerIpConnLimit::new(DEFAULT_MAX_CONNS_PER_IP);
        assert_eq!(
            limit.ceiling_for(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            LOOPBACK_MAX_CONNS
        );
        assert_eq!(
            limit.ceiling_for(ip("203.0.113.7")),
            DEFAULT_MAX_CONNS_PER_IP
        );
        const {
            assert!(
                LOOPBACK_MAX_CONNS > DEFAULT_MAX_CONNS_PER_IP,
                "the loopback ceiling is a safety bound above the abuse default"
            )
        };
    }

    #[test]
    fn set_max_hot_reloads_the_live_cap() {
        let limit = PerIpConnLimit::new(1);
        let a = ip("203.0.113.7");
        let p1 = limit.try_acquire(a);
        assert!(p1.is_some(), "first admitted under cap 1");
        assert!(limit.try_acquire(a).is_none(), "second rejected at cap 1");

        // Raise the cap live → the same source can now hold more, with no
        // rebuild of the limiter and without dropping the existing permit.
        limit.set_max(3);
        assert_eq!(limit.max(), 3, "max() reflects the live value");
        let p2 = limit.try_acquire(a);
        let p3 = limit.try_acquire(a);
        assert!(
            p2.is_some() && p3.is_some(),
            "raised cap admits up to the new ceiling"
        );
        assert!(
            limit.try_acquire(a).is_none(),
            "rejected again at the new cap 3"
        );

        // Lower the cap live → already-admitted slots persist, but no *new*
        // connection is admitted while the source is over the lowered ceiling.
        limit.set_max(1);
        assert_eq!(limit.max(), 1);
        assert!(
            limit.try_acquire(a).is_none(),
            "over the lowered cap → shed new connections"
        );
        drop(p1);
        drop(p2);
        drop(p3);
        // Back to zero held → admittable again under the lowered cap.
        assert!(
            limit.try_acquire(a).is_some(),
            "freed below the lowered cap"
        );
    }

    #[test]
    fn rejected_first_connection_does_not_leak_a_zero_entry() {
        let limit = PerIpConnLimit::new(0); // pathological: cap 0 rejects everything
        assert!(limit.try_acquire(ip("203.0.113.9")).is_none());
        assert_eq!(limit.tracked_ips(), 0, "no zero-count entry left behind");
    }

    /// The regression lock for the 2026-07-31 example.com `:443` outage.
    ///
    /// The leak was: an accepted socket whose peer vanished without a clean
    /// close stayed `ESTABLISHED` forever, so its permit was never dropped.
    /// Our half of the fix is arming the kernel's dead-peer detection on every
    /// accepted socket — so that is what this asserts, by reading the options
    /// back off a real accepted `TcpStream`. (Whether the kernel then actually
    /// times the peer out is the kernel's contract, not ours to re-test; what
    /// regressed here, and what can regress again, is a caller forgetting to
    /// arm it — hence the readback.)
    #[tokio::test]
    async fn arming_dead_peer_detection_sets_the_keepalive_options() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();

        let sock = socket2::SockRef::from(&accepted);
        assert!(
            !sock.keepalive().unwrap(),
            "precondition: SO_KEEPALIVE is off by default — that default IS the leak"
        );

        arm_dead_peer_detection(&accepted).expect("arming keepalive");

        let sock = socket2::SockRef::from(&accepted);
        assert!(sock.keepalive().unwrap(), "SO_KEEPALIVE must be on");
        // `tcp_keepalive_time`/`_interval`/`_retries` readback is not available on
        // every platform `set_tcp_keepalive` supports (windows, openbsd, solaris
        // lack a getter) — same platform set `arm_dead_peer_detection` above
        // already special-cases for `with_retries`.
        #[cfg(not(any(windows, target_os = "openbsd", target_os = "solaris")))]
        {
            assert_eq!(
                sock.tcp_keepalive_time().unwrap(),
                KEEPALIVE_IDLE,
                "idle period before probing starts"
            );
            assert_eq!(
                sock.tcp_keepalive_interval().unwrap(),
                KEEPALIVE_INTERVAL,
                "gap between probes"
            );
            assert_eq!(
                sock.tcp_keepalive_retries().unwrap(),
                KEEPALIVE_RETRIES,
                "unanswered probes before the connection is declared dead"
            );
        }
        drop(client);
    }

    #[test]
    fn shed_counter_reports_the_first_shed_immediately() {
        // The edge into shedding is the interesting event; an hour-long
        // interval must not suppress it.
        let shed = ShedCounter::with_interval(Duration::from_secs(3600));
        assert_eq!(shed.record(), Some(1), "first shed always surfaces");
    }

    #[test]
    fn shed_counter_suppresses_within_the_interval_and_carries_the_batch() {
        let shed = ShedCounter::with_interval(Duration::from_secs(3600));
        assert_eq!(shed.record(), Some(1));
        for _ in 0..49 {
            assert_eq!(shed.record(), None, "suppressed inside the interval");
        }
        // A zero interval makes the next record due without any wall-clock
        // wait — the batch it carries must account for every suppressed shed.
        let shed = ShedCounter::with_interval(Duration::ZERO);
        assert_eq!(shed.record(), Some(1));
        assert_eq!(
            shed.record(),
            Some(1),
            "each record is due at zero interval"
        );
    }

    #[test]
    fn shed_counter_batch_count_covers_every_suppressed_shed() {
        let shed = ShedCounter::with_interval(Duration::from_secs(3600));
        assert_eq!(shed.record(), Some(1), "first line stands for itself");
        for _ in 0..9 {
            assert_eq!(shed.record(), None);
        }
        // Force the next record to be due, then confirm the line it emits
        // stands for all 10 sheds since the previous line, not just itself —
        // otherwise the log understates the shed rate exactly when it matters.
        {
            let mut state = shed.state.lock().unwrap();
            state.last_report = Some(Instant::now() - Duration::from_secs(7200));
        }
        assert_eq!(shed.record(), Some(10), "9 suppressed + this one");
    }
}

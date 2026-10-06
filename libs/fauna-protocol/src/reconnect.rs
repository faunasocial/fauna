//! Runtime-agnostic reconnect policy — the backoff curve and the
//! "is this connection actually established?" decision.
//!
//! Both native (`fauna-ws-substrate`'s `run_supervisor`) and the browser
//! (`fauna-rpc-wasm`'s `run_reconnect_loop`) run the same shape: dial, drive a
//! [`crate::RpcDispatcher`] over the connection, react to the close signal, back
//! off, redial. The **substrate** genuinely cannot be shared — tungstenite is
//! `Send` + has a Ping primitive, the browser `WebSocket` is neither
//! (`transport.md` § Connection lifecycle sanctions that divergence, and it is
//! **keepalive-only**: reconnect recovery must reach parity). The **policy**
//! below is not substrate, so it lives here, in the one runtime-agnostic crate
//! both loops already depend on (priority #2), and is unit-tested on native
//! where neither loop's own crate can be.
//!
//! ## Why "established" needs a decision at all
//!
//! Native's `channel.connect().await` performs the real TCP/TLS/upgrade, so its
//! `Ok` *proves* the connection is up — the supervisor can reset its backoff
//! and announce `Connected` the moment it returns. The browser's
//! `WebSocket` constructor is **synchronous handle creation**: it throws only on
//! a malformed URL/protocol, and a nest that is down, refusing, or unreachable
//! still yields a perfectly good handle whose failure surfaces asynchronously,
//! later, on the stream. A loop that treats those two `Ok`s alike — as
//! `fauna-rpc-wasm` did, having copied native's *structure* — resets its backoff
//! and fires its "reconnected" callback on **every** dial against a down nest.
//! That is not a slower reconnect; it is an unbounded-growth backoff that never
//! grows, i.e. a hot redial loop.
//!
//! [`probe_established`] supplies the missing proof in the one currency a
//! browser always has: **survival**. Race the connection's own driver future
//! against a probe of length `N`. If the driver ends first, the connection never
//! came up. If the probe elapses with the driver still running, the connection
//! carried a live socket for `N` — good enough to call it established, and
//! (unlike a post-mortem "did it last ≥ N?" check) it is observed *during* the
//! connection, so it can gate the `Connected` announcement too, not just the
//! backoff reset.
//!
//! The probe is deliberately **dep-agnostic**: the caller supplies the timer
//! future (`gloo_timers::future::TimeoutFuture` in the browser,
//! `tokio::time::sleep` natively, a plain `ready`/`pending` in tests), so this
//! module needs no clock, no runtime, and no randomness of its own.

use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use futures_util::future::{Either, select};

/// Capped exponential-backoff **ceiling** with full jitter.
///
/// The ceiling doubles on each failed attempt toward `max` and resets to
/// `initial` only when an attempt is *proven established* (see
/// [`probe_established`]); the actual sleep is a uniform draw in `[0, ceiling]`
/// (see [`Backoff::jittered`]), not the ceiling itself. Full jitter is what
/// decorrelates the **thundering herd** a nest redeploy creates: every connected
/// device drops at the same instant carrying the same reset ceiling, so without
/// it they would all redial in lockstep against the just-booted, cold-cache nest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    ceiling: Duration,
    initial: Duration,
    max: Duration,
}

impl Backoff {
    /// Start at `initial`, growing toward `max`.
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            ceiling: initial,
            initial,
            max,
        }
    }

    /// The current ceiling — the upper bound of the next [`jittered`] draw.
    ///
    /// [`jittered`]: Backoff::jittered
    pub fn ceiling(&self) -> Duration {
        self.ceiling
    }

    /// Reset to `initial`. Call this **only** on a connection proven established
    /// — resetting on a mere dial-`Ok` is what makes the ceiling unreachable.
    pub fn reset(&mut self) {
        self.ceiling = self.initial;
    }

    /// Double the ceiling toward `max`. Call after every attempt that did not
    /// establish.
    pub fn grow(&mut self) {
        self.ceiling = (self.ceiling * 2).min(self.max);
    }

    /// Full-jitter the current ceiling: a uniform draw in `[0, ceiling]`.
    ///
    /// `unit` is the caller's `[0, 1)` random draw — the browser's
    /// `Math::random()`, native's [`JitterRng::next_unit`], or a fixed
    /// value in tests. Keeping the RNG on the caller's side is what lets this
    /// module stay runtime-agnostic (and lets a test assert an exact duration).
    /// Values outside `[0, 1)` are clamped, so a misbehaving RNG can never
    /// produce a sleep longer than the ceiling or a negative one.
    pub fn jittered(&self, unit: f64) -> Duration {
        let unit = if unit.is_finite() {
            unit.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Duration::from_secs_f64(self.ceiling.as_secs_f64() * unit)
    }
}

/// The nap after a refused dial while a request waits on the connection
/// (`transport-connection.md` § Connection lifecycle, the dial-on-demand rule):
/// a jittered draw within `initial_ceiling`, or `None` when that draw would not
/// end before the curve's own next dial, `remaining` from now — then the caller
/// sleeps to the curve's dial, which is the curve's, not an extra one.
///
/// `unit` is the caller's `[0, 1)` draw, as for [`Backoff::jittered`]. Both
/// reconnect loops take their demand naps from here, so the waiter's pace has
/// one definition; only the "a waiter arrived" wake-up is per runtime.
pub fn demand_nap(remaining: Duration, initial_ceiling: Duration, unit: f64) -> Option<Duration> {
    let short = Backoff::new(initial_ceiling, initial_ceiling).jittered(unit);
    (short < remaining).then_some(short)
}

/// How long a fresh connection must survive before [`probe_established`]
/// calls it established.
///
/// Every reconnect loop whose dial does not itself prove the nest accepted the
/// connection races this one window: the browser's `WebSocket` constructor
/// (handle creation, not a connection). 1 s is long enough that a refused or
/// immediately closing socket reliably dies first, and short enough not to
/// perceptibly delay a genuine reconnect.
pub const ESTABLISH_PROBE: Duration = Duration::from_secs(1);

/// The `[0, 1)` source for [`Backoff::jittered`] where no platform RNG call is
/// at hand: a SplitMix64 stream.
///
/// Backoff jitter needs *spread*, not cryptographic quality, so one
/// multiply-shift chain per draw is the right tool, and it keeps
/// `rand`/`getrandom` out of this crate. The browser draws from
/// `Math::random()` instead; only the `[0, 1)` source differs per runtime, so
/// "uniform draw in `[0, ceiling]`" keeps one definition.
#[derive(Debug, Clone)]
pub struct JitterRng {
    state: u64,
}

impl JitterRng {
    /// A stream from a fixed seed: for tests, and for callers bringing their
    /// own entropy.
    pub fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// A stream seeded from std's per-process random hasher keys, which the
    /// OS randomizes once per thread and std advances on every construction.
    /// Two devices, and two loops in one process, therefore start different
    /// streams. The seed is entropy, not a time read: no wall clock involved.
    /// Native only; the browser draws from `Math::random()`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_entropy() -> Self {
        use std::hash::{BuildHasher, Hasher};

        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(0x9E37_79B9_7F4A_7C15);
        Self::from_seed(hasher.finish())
    }

    /// The next uniform `[0, 1)` draw: one SplitMix64 step, its top 53 bits
    /// divided by 2^53 (the standard 53-bit-mantissa construction).
    pub fn next_unit(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// The verdict of [`probe_established`] on one connection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Establish {
    /// The probe elapsed with the driver still running: the connection carried a
    /// live socket for the whole probe window. Reset the backoff and announce
    /// `Connected`.
    Established,
    /// The driver ended before the probe elapsed: the socket never came up (a
    /// down/refusing/unreachable nest), or it came up and died immediately.
    /// Grow the backoff and do **not** announce `Connected`.
    Died,
    /// The caller's close signal fired first — shut down; the verdict is moot.
    Closed,
}

/// Race a live connection's `driver` against a `probe` timer to decide whether
/// the connection is really established. See the module docs for why a browser
/// needs this and native does not.
///
/// `driver` is taken as `Pin<&mut _>` rather than by value **on purpose**: an
/// [`Establish::Established`] verdict means the connection is still running, and
/// the caller must go on driving that same future. Borrowing leaves it with the
/// caller; taking it by value would end the connection the instant we proved it
/// was good.
///
/// Tie-break: if the driver and the probe are both ready in the same poll, the
/// verdict is [`Establish::Died`] — a connection that ends exactly at the probe
/// boundary is not one we want to reset the backoff on.
pub async fn probe_established<D, P, C>(
    driver: Pin<&mut D>,
    probe: P,
    close: Pin<&mut C>,
) -> Establish
where
    D: Future<Output = ()> + ?Sized,
    P: Future<Output = ()>,
    C: Future + ?Sized,
{
    futures_util::pin_mut!(probe);
    match select(select(driver, probe), close).await {
        // Driver finished first — the connection never came up (or died at once).
        Either::Left((Either::Left(((), _probe)), _close)) => Establish::Died,
        // Probe elapsed with the driver still pending — established.
        Either::Left((Either::Right(((), _driver)), _close)) => Establish::Established,
        // close() fired during the probe window.
        Either::Right((_closed, _race)) => Establish::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::{pending, ready};

    fn backoff() -> Backoff {
        Backoff::new(Duration::from_secs(1), Duration::from_secs(60))
    }

    #[test]
    fn grow_doubles_toward_max_then_saturates() {
        let mut b = backoff();
        assert_eq!(b.ceiling(), Duration::from_secs(1));
        for expected in [2, 4, 8, 16, 32, 60, 60] {
            b.grow();
            assert_eq!(b.ceiling(), Duration::from_secs(expected));
        }
    }

    /// A waiter's nap is a draw within the initial ceiling while the curve's
    /// next dial is further off, and gives way to that dial when the draw would
    /// reach it — so a demand nap never delays the curve's own dial.
    #[test]
    fn a_demand_nap_draws_within_the_initial_ceiling_and_yields_to_the_curve() {
        let initial = Duration::from_secs(1);
        assert_eq!(
            demand_nap(Duration::from_secs(8), initial, 0.5),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            demand_nap(Duration::from_secs(8), initial, 1.0),
            Some(initial)
        );
        assert_eq!(demand_nap(Duration::from_millis(300), initial, 0.5), None);
        assert_eq!(demand_nap(Duration::from_millis(500), initial, 0.5), None);
        assert_eq!(demand_nap(Duration::ZERO, initial, 0.0), None);
    }

    /// The regression that matters: the ceiling is only reachable if `reset` is
    /// NOT called on every attempt. This pins the *policy* half of the
    /// `fauna-rpc-wasm` bug — a loop that reset per-dial could never grow.
    #[test]
    fn reset_only_on_established_is_what_makes_the_ceiling_reachable() {
        let mut b = backoff();
        for _ in 0..10 {
            b.grow();
        }
        assert_eq!(b.ceiling(), Duration::from_secs(60), "grows to the cap");

        b.reset();
        assert_eq!(
            b.ceiling(),
            Duration::from_secs(1),
            "established → back to 1s"
        );

        // ...whereas resetting after every grow (the bug) pins it at `initial`
        // forever, so the cap is unreachable no matter how many attempts fail.
        let mut buggy = backoff();
        for _ in 0..10 {
            buggy.grow();
            buggy.reset();
        }
        assert_eq!(buggy.ceiling(), Duration::from_secs(1));
    }

    #[test]
    fn jittered_draws_within_the_ceiling_and_clamps_a_bad_rng() {
        let mut b = backoff();
        b.grow(); // ceiling = 2s
        assert_eq!(b.jittered(0.0), Duration::ZERO);
        assert_eq!(b.jittered(0.5), Duration::from_secs(1));
        // A draw of 1.0 is the ceiling itself; anything beyond clamps to it, and
        // a negative / NaN draw floors at zero — never a longer-than-ceiling nap.
        assert_eq!(b.jittered(1.0), Duration::from_secs(2));
        assert_eq!(b.jittered(9.9), Duration::from_secs(2));
        assert_eq!(b.jittered(-1.0), Duration::ZERO);
        assert_eq!(b.jittered(f64::NAN), Duration::ZERO);
    }

    // The probe tests below pass `ready`/`pending` rather than real timers, so
    // they are deterministic and load-immune by construction — no clock, no
    // sleeping, nothing for a busy machine to make flaky.

    #[tokio::test]
    async fn driver_outliving_the_probe_is_established() {
        let mut driver = pending::<()>();
        let mut close = pending::<()>();
        let verdict = probe_established(
            Pin::new(&mut driver),
            ready(()), // probe elapses immediately
            Pin::new(&mut close),
        )
        .await;
        assert_eq!(verdict, Establish::Established);
    }

    /// The wasm bug in miniature: the browser hands back a live handle for a
    /// **down** nest, the stream dies straight away, and the loop must NOT count
    /// that as a connection.
    #[tokio::test]
    async fn driver_ending_before_the_probe_never_established() {
        let mut driver = ready(());
        let mut close = pending::<()>();
        let verdict = probe_established(
            Pin::new(&mut driver),
            pending::<()>(), // probe never elapses
            Pin::new(&mut close),
        )
        .await;
        assert_eq!(verdict, Establish::Died);
    }

    #[tokio::test]
    async fn close_during_the_probe_window_wins() {
        let mut driver = pending::<()>();
        let mut close = ready(());
        let verdict =
            probe_established(Pin::new(&mut driver), pending::<()>(), Pin::new(&mut close)).await;
        assert_eq!(verdict, Establish::Closed);
    }

    /// An `Established` verdict must leave the connection running — the caller
    /// goes on driving the very same future. If the probe consumed it, the loop
    /// would tear down every connection at the moment it proved it was good.
    #[tokio::test]
    async fn established_leaves_the_driver_drivable() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let driver = async {
            let _ = rx.await;
        };
        futures_util::pin_mut!(driver);
        let mut close = pending::<()>();

        let verdict = probe_established(driver.as_mut(), ready(()), Pin::new(&mut close)).await;
        assert_eq!(verdict, Establish::Established);

        // Still alive and still ours to drive: end it and confirm it completes.
        tx.send(()).unwrap();
        driver.await;
    }

    /// Full jitter must (a) keep every sleep within `[0, ceiling]` and (b)
    /// actually spread across the window — so N devices dropped together by a
    /// nest redeploy don't reconnect in lockstep. Asserts the spread over many
    /// samples rather than any single draw.
    #[test]
    fn jitter_spreads_backoff_across_the_window() {
        let ceiling = Duration::from_secs(10);
        let backoff = Backoff::new(ceiling, ceiling);
        let mut rng = JitterRng::from_seed(0x1234_5678_9abc_def0);
        let mut min = Duration::MAX;
        let mut max = Duration::ZERO;
        let mut samples = Vec::with_capacity(1000);
        for _ in 0..1000 {
            let d = backoff.jittered(rng.next_unit());
            assert!(
                d <= ceiling,
                "jittered sleep {d:?} exceeded the ceiling {ceiling:?}"
            );
            min = min.min(d);
            max = max.max(d);
            samples.push(d);
        }
        // Not a constant (no-jitter regression): many distinct values.
        let distinct: std::collections::HashSet<u128> =
            samples.iter().map(|d| d.as_millis()).collect();
        assert!(
            distinct.len() > 100,
            "jitter produced only {} distinct values",
            distinct.len()
        );
        // Good coverage: the draws reach into both the low and high quartiles.
        assert!(min < ceiling / 4, "jitter never went low: min={min:?}");
        assert!(max > ceiling * 3 / 4, "jitter never went high: max={max:?}");
    }

    /// Distinct seeds (different devices) yield different first-sleep draws —
    /// the property that actually decorrelates the herd.
    #[test]
    fn distinct_seeds_yield_distinct_draws() {
        let ceiling = Duration::from_secs(60);
        let backoff = Backoff::new(ceiling, ceiling);
        let draws: std::collections::HashSet<u128> = (0..50u64)
            .map(|seed| {
                let mut rng =
                    JitterRng::from_seed(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xDEAD_BEEF);
                backoff.jittered(rng.next_unit()).as_millis()
            })
            .collect();
        assert!(
            draws.len() > 40,
            "seeds barely decorrelated: {} distinct of 50",
            draws.len()
        );
    }

    /// Two entropy-seeded streams in one process start apart — two reconnect
    /// loops of one app (the bearer and federation channels, say) must not
    /// share a jitter sequence.
    #[test]
    fn two_entropy_seeded_streams_in_one_process_differ() {
        let first = JitterRng::from_entropy().next_unit();
        let second = JitterRng::from_entropy().next_unit();
        assert_ne!(first, second, "both streams drew {first}");
    }

    /// Every draw is a `[0, 1)` fraction, so `jittered` never has to clamp a
    /// native draw.
    #[test]
    fn every_jitter_draw_is_a_unit_fraction() {
        let mut rng = JitterRng::from_seed(0);
        for _ in 0..10_000 {
            let unit = rng.next_unit();
            assert!((0.0..1.0).contains(&unit), "draw {unit} left [0, 1)");
        }
    }
}

//! The process-wide **dial budget** — every native client WS dial to a nest
//! passes through [`acquire`] first, so no bug above this layer can dial one
//! nest faster than a fixed rate (`transport-connection.md` § Connection
//! lifecycle → *The dial budget*).
//!
//! Each reconnect loop already paces itself (the supervisor's full-jittered
//! 1 s → 60 s curve), but a pace is a property of *one* loop. Nothing bounds
//! how many loops a process runs, and every flood so far has been a count
//! problem, not a pace problem: a supervisor stranded per re-assembly, a retry
//! task per failed mount — each correctly paced, together dialling a nest
//! several times a second for days. This bucket is the backstop that holds
//! whatever the count: it is keyed on the nest (the dial's `host:port`), not on
//! the caller, so a thousand dialers share one allowance.
//!
//! **Constants, not configuration** (`principles.md` § One configuration
//! surface): no user or admin would ever choose these. The burst is sized for
//! the two legitimate bursts a process makes — a redeploy reconnect (every
//! connection the process holds redials within a second) **and a run of
//! session starts** (sign out, sign in as another account, again: each start
//! is several mints and dials, and a process switching accounts back to back
//! is as correct as one reconnecting) — see [`DIAL_BUDGET_BURST`] for the
//! arithmetic; and the refill sits well below any loop's steady state. So the
//! budget never paces a correct process and caps a broken one at a few dials a
//! minute. A burst sized for the reconnect alone (64, until 2026-09-28) paced
//! one e2e app process three sign-ins into a succession journey, and would
//! have paced a person after a handful of account switches.
//!
//! **A nest's `Retry-After` holds the same gate** ([`hold`]): a `429` on one
//! dial defers every dial to that nest in this process until the nest's time is
//! up, not merely the one loop that happened to read it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::time::Instant;

/// The widest legitimate one-shot reconnect: a redeploy drops every connection
/// an app process holds and they all redial within a second — the session's
/// client, the store principal's client, and a terminal app's per-feature
/// clients (tui opens one per visited tab today, `transport.md`
/// § Implementation status), each a bearer re-mint plus the authenticated
/// dial. Measured under 24 on the widest app; sized at the budget's original
/// whole burst, which no redeploy ever spent.
pub const REDEPLOY_RECONNECT_DIALS: u32 = 64;

/// Dials one session start costs at its widest today (windows and tui,
/// measured 2026-09-28): the launch's silent challenge, the session client's
/// bearer mint and authenticated dial, the store principal's device-handshake
/// mint and authenticated dial, and the post-auth silent challenge tui, linux
/// and apple still run beside the session's own mint. Rounded up, and kept at
/// the 2026-09-28 measurement: the device principal's one to five mints
/// refused `not_registered` it also counted are gone (its first connect now
/// waits for the grant), and the post-auth challenge — a dial the process owes
/// to nothing — is being removed; a ceiling that outlives what it covered only
/// widens the margin.
pub const DIALS_PER_SESSION_START: u32 = 12;

/// Session starts back to back — within one refill interval, so the refill
/// contributes nothing — that the burst absorbs on top of a redeploy
/// reconnect. A person switching accounts needs a few; the e2e harness, one
/// app process signing in afresh per test and several times per succession
/// journey, spent 146 dials over one nine-test module.
pub const SESSION_STARTS_COVERED: u32 = 16;

/// Dials to one nest a process may make back to back before the refill paces
/// it: a redeploy reconnect plus a run of session starts (the module doc; the
/// arithmetic is the three constants above, pinned by
/// `tests::a_run_of_account_switches_is_not_paced`). 256 as of 2026-09-28; the
/// original 64 covered the reconnect alone, and one e2e process signing in
/// three times per journey spent it in about 21 s.
pub const DIAL_BUDGET_BURST: u32 =
    REDEPLOY_RECONNECT_DIALS + SESSION_STARTS_COVERED * DIALS_PER_SESSION_START;

/// Once the burst is spent, one further dial to that nest per this interval —
/// six a minute. The supervisor's own curve dials at most once a minute at its
/// ceiling, so a correct process only ever meets this pace in a burst.
pub const DIAL_BUDGET_REFILL: Duration = Duration::from_secs(10);

/// The longest a nest's `Retry-After` may hold the gate. A nest names its own
/// hold, and a bogus one (a proxy's, a misconfiguration) must not park a client
/// for a day; past this the ordinary budget and backoff take over again.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(15 * 60);

/// One nest's allowance, as a GCRA reservation line: `tat` is the theoretical
/// arrival time of the next dial, advanced by [`DIAL_BUDGET_REFILL`] per dial
/// (waiting ones included, so concurrent waiters queue rather than stampede).
#[derive(Debug, Clone, Copy)]
struct Slot {
    tat: Instant,
    /// Latest `Retry-After` deadline the nest asked for, if still in force.
    hold_until: Option<Instant>,
    /// Whether the "throttled" line was already logged for this run of
    /// throttling — once per run, so a broken process logs its state, not a
    /// line per dial.
    warned: bool,
}

/// A budget over any number of nests. Production uses the one process-wide
/// instance behind [`acquire`] / [`hold`]; a test builds its own so its
/// accounting is independent of every other test in the binary.
#[derive(Debug)]
pub struct DialBudget {
    burst: u32,
    refill: Duration,
    slots: Mutex<HashMap<String, Slot>>,
}

impl DialBudget {
    pub fn new(burst: u32, refill: Duration) -> Self {
        Self {
            burst: burst.max(1),
            refill,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Reserve one dial to `nest` and wait until it is allowed.
    pub async fn acquire(&self, nest: &str) {
        let at = self.reserve(nest, Instant::now());
        if at > Instant::now() {
            tokio::time::sleep_until(at).await;
        }
    }

    /// Take the next place in `nest`'s line and return when it comes up. Split
    /// from [`Self::acquire`] so the arithmetic is testable without a clock.
    fn reserve(&self, nest: &str, now: Instant) -> Instant {
        let tolerance = self.refill * (self.burst - 1);
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        let slot = slots.entry(nest.to_string()).or_insert(Slot {
            tat: now,
            hold_until: None,
            warned: false,
        });
        let tat = slot.tat.max(now);
        let mut at = tat.checked_sub(tolerance).unwrap_or(now).max(now);
        slot.tat = tat + self.refill;
        if let Some(until) = slot.hold_until {
            if until > now {
                at = at.max(until);
            } else {
                slot.hold_until = None;
            }
        }
        // One line per dial, so a paced run can be attributed: the dial count
        // and each wait are read off a debug log, not inferred from the one
        // warning below.
        tracing::debug!(
            nest,
            wait_ms = at.saturating_duration_since(now).as_millis() as u64,
            "dial budget: dial reserved"
        );
        if at > now {
            if !slot.warned {
                slot.warned = true;
                tracing::warn!(
                    nest,
                    wait_ms = (at - now).as_millis() as u64,
                    "dial budget: dials to this nest are being paced (burst of \
                     {} spent, or the nest asked for Retry-After) — something in \
                     this process is dialling far more than one reconnect loop would",
                    self.burst
                );
            }
        } else {
            slot.warned = false;
        }
        at
    }

    /// Hold every dial to `nest` for `retry_after` (capped at
    /// [`MAX_RETRY_AFTER`]) — the nest answered a dial `429 Retry-After`.
    pub fn hold(&self, nest: &str, retry_after: Duration) {
        let until = Instant::now() + retry_after.min(MAX_RETRY_AFTER);
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let slot = slots.entry(nest.to_string()).or_insert(Slot {
            tat: now,
            hold_until: None,
            warned: false,
        });
        slot.hold_until = Some(slot.hold_until.map_or(until, |u| u.max(until)));
    }

    /// Forget every nest's spent burst and standing hold, as a fresh process
    /// would. Test builds only — see [`clear_for_test`].
    #[cfg(any(test, feature = "test-helpers"))]
    fn clear(&self) {
        self.slots.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

fn process_budget() -> &'static DialBudget {
    static BUDGET: OnceLock<DialBudget> = OnceLock::new();
    BUDGET.get_or_init(|| DialBudget::new(DIAL_BUDGET_BURST, DIAL_BUDGET_REFILL))
}

/// Wait for this process's allowance to dial `nest` (its `host:port`). Every
/// native client WS dial calls this immediately before opening the socket.
pub async fn acquire(nest: &str) {
    process_budget().acquire(nest).await;
}

/// Give this process a fresh budget — the e2e per-test factory reset's half of
/// "a reset app is a new device". A native e2e driver keeps one app process
/// across tests and factory-resets it in place, so without this a journey's
/// dials spend the NEXT test's burst too, a coupling no real device has: a
/// device lost and restored is a new process. `test-helpers` only; a shipped
/// build has no way to clear the backstop.
#[cfg(any(test, feature = "test-helpers"))]
pub fn clear_for_test() {
    process_budget().clear();
}

/// Record a nest's `Retry-After` against this process's dials to it.
pub fn hold(nest: &str, retry_after: Duration) {
    process_budget().hold(nest, retry_after);
}

/// How long a `429` with no readable `Retry-After` holds the gate: the
/// supervisor's own ceiling, the longest any one loop would wait unasked.
pub const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Read a failed dial for the one refusal the budget acts on — a `429 Too Many
/// Requests` on the upgrade — and hold the gate for as long as the nest asked.
/// Every other outcome is the caller's to classify; this only listens.
pub fn note_dial_error(nest: &str, err: &tokio_tungstenite::tungstenite::Error) {
    if let tokio_tungstenite::tungstenite::Error::Http(resp) = err
        && resp.status().as_u16() == 429
    {
        let wait = retry_after_of(resp.headers()).unwrap_or(DEFAULT_RETRY_AFTER);
        tracing::warn!(
            nest,
            wait_secs = wait.as_secs(),
            "nest answered 429 to a dial; holding every dial to it"
        );
        hold(nest, wait);
    }
}

/// The `Retry-After` a refused upgrade carried, in its delay-seconds form. The
/// HTTP-date form is not read: the nest emits seconds, and a date would need a
/// clock this client does not trust against the nest's.
pub fn retry_after_of(
    headers: &tokio_tungstenite::tungstenite::http::HeaderMap,
) -> Option<Duration> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// The budget key for a dial: the request's `host:port`, lowercased, with the
/// scheme's default port filled in so `wss://nest` and `wss://nest:443` share
/// one allowance.
pub fn nest_key(uri: &tokio_tungstenite::tungstenite::http::Uri) -> String {
    let host = uri.host().unwrap_or_default().to_ascii_lowercase();
    let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
        Some("ws") | Some("http") => 80,
        _ => 443,
    });
    format!("{host}:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The backstop's whole claim: however many dialers a process runs against
    /// one nest, over any window it makes at most the burst plus one dial per
    /// refill interval. A thousand tight loops — the shape a stranded-dialer
    /// leak reaches after enough days — are paced to that, not to their count.
    #[tokio::test(start_paused = true)]
    async fn a_thousand_dialers_share_one_allowance() {
        let budget = Arc::new(DialBudget::new(DIAL_BUDGET_BURST, DIAL_BUDGET_REFILL));
        let dials = Arc::new(AtomicUsize::new(0));
        for _ in 0..1000 {
            let budget = Arc::clone(&budget);
            let dials = Arc::clone(&dials);
            tokio::spawn(async move {
                loop {
                    budget.acquire("nest.example:443").await;
                    dials.fetch_add(1, Ordering::SeqCst);
                    // A loop with no pace of its own beyond a token second.
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
        }
        let window = Duration::from_secs(10 * 60);
        tokio::time::sleep(window).await;
        let bound = DIAL_BUDGET_BURST as usize
            + (window.as_secs() / DIAL_BUDGET_REFILL.as_secs()) as usize
            + 1;
        let made = dials.load(Ordering::SeqCst);
        assert!(
            made <= bound,
            "{made} dials in ten minutes; the budget allows at most {bound}"
        );
        assert!(
            made >= bound - 2,
            "the budget must still let dials through: {made}"
        );
    }

    /// A correct reconnect burst — every connection a desktop holds, redialling
    /// together after a redeploy — goes straight through: the budget must
    /// never be the thing that slows a healthy process down.
    #[tokio::test(start_paused = true)]
    async fn a_redeploy_burst_is_not_paced() {
        let budget = DialBudget::new(DIAL_BUDGET_BURST, DIAL_BUDGET_REFILL);
        let start = Instant::now();
        for _ in 0..DIAL_BUDGET_BURST {
            budget.acquire("nest.example:443").await;
        }
        assert_eq!(
            Instant::now(),
            start,
            "a burst within the allowance waits for nothing"
        );
    }

    /// A run of account switches in one process — sign out, sign in as
    /// another account, again and again, faster than the refill could help —
    /// is a correct process too, and the doc's promise covers it: the burst
    /// holds [`SESSION_STARTS_COVERED`] session starts at the widest measured
    /// per-start cost on top of a redeploy reconnect burst, all within one
    /// refill interval. The e2e harness does exactly this by design (one app
    /// process, a fresh sign-in per test, several per succession journey),
    /// and a user flipping between accounts does it more rarely.
    #[tokio::test(start_paused = true)]
    async fn a_run_of_account_switches_is_not_paced() {
        let budget = DialBudget::new(DIAL_BUDGET_BURST, DIAL_BUDGET_REFILL);
        let start = Instant::now();
        for _ in 0..REDEPLOY_RECONNECT_DIALS {
            budget.acquire("nest.example:443").await;
        }
        for _ in 0..SESSION_STARTS_COVERED {
            for _ in 0..DIALS_PER_SESSION_START {
                budget.acquire("nest.example:443").await;
            }
        }
        assert_eq!(
            Instant::now(),
            start,
            "{SESSION_STARTS_COVERED} session starts after a redeploy reconnect must wait \
             for nothing"
        );
    }

    /// Nests are budgeted apart: one nest's flood never paces a dial to
    /// another.
    #[tokio::test(start_paused = true)]
    async fn each_nest_has_its_own_allowance() {
        let budget = DialBudget::new(2, DIAL_BUDGET_REFILL);
        for _ in 0..5 {
            let at = budget.reserve("a:443", Instant::now());
            let _ = at;
        }
        let start = Instant::now();
        budget.acquire("b:443").await;
        assert_eq!(Instant::now(), start);
    }

    /// A `Retry-After` holds every dial to that nest until it is up, even with
    /// allowance to spare, and then lets them through again.
    #[tokio::test(start_paused = true)]
    async fn a_retry_after_holds_the_whole_gate() {
        let budget = DialBudget::new(DIAL_BUDGET_BURST, DIAL_BUDGET_REFILL);
        let start = Instant::now();
        budget.hold("nest.example:443", Duration::from_secs(120));
        budget.acquire("nest.example:443").await;
        assert_eq!(Instant::now() - start, Duration::from_secs(120));
        let after = Instant::now();
        budget.acquire("nest.example:443").await;
        assert_eq!(Instant::now(), after, "the hold ends when its time is up");
    }

    /// A cleared budget is a fresh process's: a spent burst and a standing
    /// `Retry-After` are both forgotten, so the next dial waits for nothing.
    #[tokio::test(start_paused = true)]
    async fn a_cleared_budget_is_a_fresh_process() {
        let budget = DialBudget::new(2, DIAL_BUDGET_REFILL);
        for _ in 0..5 {
            let _ = budget.reserve("nest.example:443", Instant::now());
        }
        budget.hold("nest.example:443", Duration::from_secs(120));
        budget.clear();
        let start = Instant::now();
        budget.acquire("nest.example:443").await;
        budget.acquire("nest.example:443").await;
        assert_eq!(
            Instant::now(),
            start,
            "a cleared budget has its whole burst"
        );
    }

    /// The e2e seam reaches the process-wide budget, not a copy: a burst spent
    /// through [`acquire`] is whole again after [`clear_for_test`]. The key is
    /// unique to this test, so the shared instance's other nests are untouched.
    #[tokio::test(start_paused = true)]
    async fn clear_for_test_restores_the_process_burst() {
        const NEST: &str = "clear-for-test.example:443";
        for _ in 0..DIAL_BUDGET_BURST + 3 {
            let _ = process_budget().reserve(NEST, Instant::now());
        }
        clear_for_test();
        let start = Instant::now();
        acquire(NEST).await;
        assert_eq!(Instant::now(), start, "the seam cleared the process budget");
    }

    /// A nest cannot park a client for longer than [`MAX_RETRY_AFTER`].
    #[tokio::test(start_paused = true)]
    async fn a_retry_after_is_capped() {
        let budget = DialBudget::new(DIAL_BUDGET_BURST, DIAL_BUDGET_REFILL);
        let start = Instant::now();
        budget.hold("nest.example:443", Duration::from_secs(86_400));
        budget.acquire("nest.example:443").await;
        assert_eq!(Instant::now() - start, MAX_RETRY_AFTER);
    }

    #[test]
    fn the_key_fills_the_default_port_and_folds_case() {
        let k = |s: &str| nest_key(&s.parse().unwrap());
        assert_eq!(k("wss://Nest.Example/api/v1/ws"), "nest.example:443");
        assert_eq!(k("wss://nest.example:443/x"), "nest.example:443");
        assert_eq!(k("ws://127.0.0.1:8080/x"), "127.0.0.1:8080");
        assert_eq!(k("ws://localhost/x"), "localhost:80");
    }

    #[test]
    fn retry_after_reads_delay_seconds_only() {
        use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        assert_eq!(retry_after_of(&h), None);
        h.insert("retry-after", HeaderValue::from_static(" 30 "));
        assert_eq!(retry_after_of(&h), Some(Duration::from_secs(30)));
        h.insert(
            "retry-after",
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after_of(&h), None);
    }
}

//! Screen-time policy — family-safety pillar 3 (`family-safety.md` § Screen time).
//!
//! Client-enforced by construction: the nest cannot see (and must not gate) when
//! a child's device is in use, so enforcement is a render rule on the ward's
//! clients. This module owns the shared value type plus everything a client
//! needs to act on it identically to every other app: the write-validation
//! rule ([`ScreenTimePolicy::validate`], the same rule the nest enforces) and
//! the pure window/budget decision ([`ScreenTimePolicy::lock_state`]) —
//! mirroring how `obligation.rs` co-locates `ContentPolicy` with the
//! `render_verdict` engine that consumes it.

use serde::{Deserialize, Serialize};

/// A guardian's screen-time policy (`family-safety.md` § Screen time). Two
/// independent, both-optional controls:
///
/// - a **usage window** [`window_start`, `window_end`) in **minutes from local
///   midnight** (`0..1440`), **wrapping allowed** — `window_start > window_end`
///   is the bedtime case ("21:00–07:00" == `1260..420`). Outside the window a
///   conforming ward client renders a full-screen lock (window enforcement is
///   pure client-local clock — no wire).
/// - a **daily budget** [`daily_minutes`] of foreground use, enforced via
///   cross-device accounting (the nest sums heartbeats; Slice E).
///
/// Every field absent is the unsupervised-equivalent (no screen-time limit) —
/// the [`Default`]. `None` on any field means "that control is unset".
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenTimePolicy {
    /// Usage-window start, minutes from local midnight (`0..1440`). `None` = no
    /// window. Mirrors the `guardian_policies.screen_window_start` column.
    #[serde(default)]
    pub window_start: Option<u16>,
    /// Usage-window end, minutes from local midnight (`0..1440`), **exclusive**.
    /// `None` = no window. `window_start > window_end` wraps midnight. Mirrors
    /// the `guardian_policies.screen_window_end` column.
    #[serde(default)]
    pub window_end: Option<u16>,
    /// Daily foreground-use budget in minutes. `None` = no budget. Mirrors the
    /// `guardian_policies.screen_daily_minutes` column.
    #[serde(default)]
    pub daily_minutes: Option<u16>,
}

/// Minutes in a day — the exclusive upper bound on a window bound, and the
/// inclusive cap on [`ScreenTimePolicy::daily_minutes`].
pub const MINUTES_PER_DAY: u16 = 1440;

/// The ward-client lock verdict (`family-safety.md` § Screen time) — what
/// [`ScreenTimePolicy::lock_state`] folds a policy + the local clock + the
/// nest-accounted usage total into. The two locked variants are distinct
/// because the lock screen names *why* ("bedtime" vs. "time's up"); both keep
/// the Family page reachable read-only (the ward must always be able to see
/// who supervises them and what the policy is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenLockState {
    /// No control locks right now.
    Allowed,
    /// The local clock is outside the usage window.
    LockedWindow,
    /// The day's cross-device foreground total has reached the daily budget.
    LockedBudget,
}

impl ScreenTimePolicy {
    /// Whether the policy sets any control at all (an all-`None` policy is the
    /// unsupervised-equivalent and enforces nothing).
    pub fn is_unset(&self) -> bool {
        self.window_start.is_none() && self.window_end.is_none() && self.daily_minutes.is_none()
    }

    /// Whether a usage window is configured (both bounds present).
    pub fn has_window(&self) -> bool {
        self.window_start.is_some() && self.window_end.is_some()
    }

    /// The write-validation rule (`family-safety.md` § Screen time — window
    /// semantics + write validation, ratified 2026-07-16). Shared so every
    /// app's policy editor pre-validates **identically** to the nest, which
    /// calls this from `policy.update` and refuses what it cannot honestly
    /// store:
    ///
    /// - a window bound outside `0..1440` (minutes from local midnight);
    /// - a half-set window — bounds come in pairs;
    /// - `start == end` — an empty window has two contradictory readings
    ///   (always-locked vs. no-window), so the guardian must say which they
    ///   mean: clear the window for "no window", or set `daily_minutes = 0`
    ///   for a deliberate full lock;
    /// - `daily_minutes > 1440` (`0` is allowed — it has exactly one reading).
    ///
    /// The wrap case (`start > end`, the "21:00–07:00" bedtime window) is
    /// valid by construction, and `end = 0` with a wrapping start expresses
    /// "until midnight" — every real window is representable.
    pub fn validate(&self) -> Result<(), &'static str> {
        for bound in [self.window_start, self.window_end].into_iter().flatten() {
            if bound >= MINUTES_PER_DAY {
                return Err("a window bound is minutes from local midnight (0..1440)");
            }
        }
        match (self.window_start, self.window_end) {
            (Some(start), Some(end)) if start == end => {
                return Err(
                    "an empty window (start == end) is ambiguous — clear the window \
                     for no window, or set daily_minutes = 0 for a full lock",
                );
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err("window bounds come in pairs (set both or neither)");
            }
            _ => {}
        }
        if let Some(budget) = self.daily_minutes
            && budget > MINUTES_PER_DAY
        {
            return Err("daily_minutes is at most 1440 (one day)");
        }
        Ok(())
    }

    /// Whether the local clock is inside the usage window (`family-safety.md`
    /// § Screen time — window enforcement is pure client-local clock). The
    /// window is half-open `[start, end)`; `start > end` wraps midnight (the
    /// "21:00–07:00" bedtime case). No window configured → always inside.
    ///
    /// A stored `start == end` is refused at write ([`Self::validate`]), so it
    /// can only mean corrupt state — it reads **fail-closed** (outside, i.e.
    /// locked), the same rule as an unparseable content floor: silently
    /// enforcing nothing when the guardian believes a window exists is the
    /// worse failure, and the lock is recoverable (the guardian edits the
    /// policy; the Family page stays reachable).
    pub fn in_window(&self, now_local_minutes: u16) -> bool {
        let (Some(start), Some(end)) = (self.window_start, self.window_end) else {
            return true;
        };
        let now = now_local_minutes % MINUTES_PER_DAY;
        match start.cmp(&end) {
            core::cmp::Ordering::Equal => false, // corrupt (write-refused) — fail closed
            core::cmp::Ordering::Less => start <= now && now < end,
            core::cmp::Ordering::Greater => now >= start || now < end,
        }
    }

    /// The pure window/budget decision (`family-safety.md` § Screen time) —
    /// the one fn the ward-client lock surface consumes. `now_local_minutes`
    /// is the device's local clock as minutes from local midnight;
    /// `used_today_minutes` is the day's cross-device foreground total from
    /// the last `fauna.family.usage_report` reply (`None` = not yet known —
    /// the budget is not evaluated until the client has heard a total, so a
    /// fresh launch renders, heartbeats, and locks on the reply if exhausted).
    ///
    /// The window is checked first: outside it, *which* budget state holds is
    /// moot and the lock screen names the bedtime rule.
    pub fn lock_state(
        &self,
        now_local_minutes: u16,
        used_today_minutes: Option<u32>,
    ) -> ScreenLockState {
        if !self.in_window(now_local_minutes) {
            return ScreenLockState::LockedWindow;
        }
        if let (Some(budget), Some(used)) = (self.daily_minutes, used_today_minutes)
            && used >= u32::from(budget)
        {
            return ScreenLockState::LockedBudget;
        }
        ScreenLockState::Allowed
    }
}

/// The ward-client lock surface in ONE call (`family-safety.md` § Screen time):
/// `None` = render nothing, `Some(text)` = render the full-screen
/// `screen-time-lock` with `text` as its `screen-time-lock-message`.
///
/// Deliberately takes the raw inputs (policy + local clock + the day's
/// nest-accounted total + the guardian's handle) and calls
/// [`ScreenTimePolicy::lock_state`] itself, rather than taking an already-
/// computed [`ScreenLockState`]. Two reasons, both about the 7 apps agreeing:
///
/// 1. **Gating and text cannot drift.** Every render surface gets its
///    visibility decision and its wording from the same call, so no app can
///    decide to lock on one rule and explain it with another — the same
///    lesson `obligation::render_verdict_composed` learned after three apps
///    hand-wrote the same three steps and were free to order them differently.
/// 2. **The inconsistent pairs become unrepresentable.** `LockedWindow` is
///    only reachable when both window bounds are set and `LockedBudget` only
///    when a budget is, so "locked, but nothing to name" cannot occur.
///
/// The message names *the policy and the guardian*, per the goal doc — when
/// use resumes (window) or the budget that ran out — and never anything about
/// what the ward was doing.
pub fn screen_lock_message(
    policy: &ScreenTimePolicy,
    now_local_minutes: u16,
    used_today_minutes: Option<u32>,
    guardian_handle: &str,
) -> Option<crate::localized::LocalizedText> {
    use crate::localized::LocalizedText;
    match policy.lock_state(now_local_minutes, used_today_minutes) {
        ScreenLockState::Allowed => None,
        ScreenLockState::LockedWindow => {
            // `lock_state` returns this only when `in_window` was false, which
            // requires both bounds present (no window → always inside).
            let resumes = policy.window_start.map(format_time_of_day)?;
            Some(LocalizedText::key_args(
                "family.screen_lock_window",
                [
                    ("resumes", resumes),
                    ("guardian", guardian_handle.to_string()),
                ],
            ))
        }
        ScreenLockState::LockedBudget => {
            // Likewise only reachable with a budget set.
            let budget = policy.daily_minutes?;
            Some(LocalizedText::key_args(
                "family.screen_lock_budget",
                [
                    ("minutes", budget.to_string()),
                    ("guardian", guardian_handle.to_string()),
                ],
            ))
        }
    }
}

/// Parse a guardian-typed **time of day** into the policy's storage unit,
/// minutes from local midnight (`family-safety.md` § Screen time). Accepts
/// `"H:MM"` / `"HH:MM"` (and tolerates surrounding whitespace); an empty or
/// whitespace-only string is `Ok(None)` — "this bound is unset", which is how
/// a guardian clears the window.
///
/// This lives in shared Rust rather than in each app's editor because the
/// *storage* unit is minutes but no guardian would ever type `1260` for 9pm:
/// every one of the 7 apps must therefore do the same conversion, and a
/// per-app parser is a per-app divergence waiting to disagree about `"9:5"`,
/// `"24:00"`, or `"08:60"`. Out-of-range components are rejected here so a
/// client can never assemble a policy [`ScreenTimePolicy::validate`] would
/// refuse for a reason the guardian can't see.
///
/// `Err` carries the same shape as `validate` — a static reason string the
/// caller surfaces on its `error-message`.
pub fn parse_time_of_day(input: &str) -> Result<Option<u16>, &'static str> {
    let text = input.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let (hours, minutes) = text
        .split_once(':')
        .ok_or("a time of day looks like HH:MM (for example 21:00)")?;
    let hours: u16 = hours
        .trim()
        .parse()
        .map_err(|_| "a time of day looks like HH:MM (for example 21:00)")?;
    let minutes: u16 = minutes
        .trim()
        .parse()
        .map_err(|_| "a time of day looks like HH:MM (for example 21:00)")?;
    if hours > 23 || minutes > 59 {
        return Err("a time of day runs from 00:00 to 23:59");
    }
    Ok(Some(hours * 60 + minutes))
}

/// Render minutes-from-local-midnight back as `"HH:MM"` — the inverse of
/// [`parse_time_of_day`], used to fill a guardian's editor from the stored
/// policy. Values at or beyond a day wrap, so this is total.
pub fn format_time_of_day(minutes_from_midnight: u16) -> String {
    let m = minutes_from_midnight % MINUTES_PER_DAY;
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// Parse a guardian-typed **daily budget** in whole minutes. Empty is
/// `Ok(None)` (no budget — the unsupervised-equivalent default); anything
/// else must be a plain non-negative integer inside `0..=1440`, the range
/// [`ScreenTimePolicy::validate`] accepts (`0` is the deliberate full lock).
pub fn parse_daily_minutes(input: &str) -> Result<Option<u16>, &'static str> {
    let text = input.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let minutes: u16 = text
        .parse()
        .map_err(|_| "a daily budget is a whole number of minutes")?;
    if minutes > MINUTES_PER_DAY {
        return Err("daily_minutes is at most 1440 (one day)");
    }
    Ok(Some(minutes))
}

/// The heartbeat cadence (`family-safety.md` § Screen time — *"coarse
/// heartbeats … at a fixed cadence"*). Five minutes: the budget's own
/// resolution is one minute, but the *lock* does not wait for a heartbeat to
/// fire (see [`UsageHeartbeat::used_today_minutes`], which adds the client's
/// own unreported minutes to the nest total), so the cadence only bounds how
/// fast **cross-device** totals converge — and every report is a write the
/// nest must serve. One shared constant so all 7 apps heartbeat identically,
/// exactly as [`crate::obligation::NOTIFY_REPORT_MIN_INTERVAL_SECS`] does for
/// Guardian Notify.
pub const USAGE_REPORT_INTERVAL_SECS: i64 = 300;

/// The most minutes one report may carry — mirrors the nest's own per-report
/// clamp, so a client can never send a delta the nest will silently truncate.
/// A day's worth: reaching it means the device was foregrounded far longer
/// than any accrual step permits, i.e. the state is already nonsense.
pub const MAX_USAGE_MINUTES_PER_REPORT: u32 = MINUTES_PER_DAY as u32;

/// The most wall-clock seconds one accrual step may credit.
///
/// The engine accrues from the *gap* between calls, so a device that suspends
/// (a closed laptop, a VM paused, a debugger held) with the app foregrounded
/// would otherwise credit the whole outage as screen time. Callers tick on the
/// order of a minute, so anything past two minutes is a gap that did not
/// happen in front of the ward — credit two minutes and drop the rest. Erring
/// this way under-reports, which is the honest direction: over-reporting would
/// lock a child out for time they never spent.
pub const MAX_ACCRUAL_STEP_SECS: i64 = 120;

/// The ward-client heartbeat that turns foreground time into
/// `fauna.family.usage_report` calls (`family-safety.md` § Screen time —
/// *"Daily-budget enforcement needs cross-device accounting"*).
///
/// **Why this is shared Rust and not seven per-app timers.** Every one of the 7
/// apps must agree on what a minute of "use" is, when to flush it, what to do
/// with a report that fails, and how to combine the nest's cross-device total
/// with time this device has not reported yet. Each of those is a place two
/// apps could silently disagree and give the same child two different bedtimes
/// (priority #2). The app supplies only what it alone knows: the clock, the
/// device's UTC offset, and whether it is being looked at.
///
/// **Accounting runs only while a daily budget is set** (goal doc: *"Reporting
/// and accounting run only while a daily budget is set"*) — [`Self::set_policy`]
/// with a budget-less policy drops all state, so no usage is accumulated, let
/// alone sent, for a ward whose guardian declared no budget.
///
/// **Time on the lock screen is not use.** Callers pass `active = foregrounded
/// AND not locked` to [`Self::set_active`]. A ward staring at their lock screen
/// is precisely *not* using the device, and crediting that time would inflate
/// the guardian's readout without bound — the readout § Screen time requires
/// ("The guardian's Family surface shows per-ward usage") would then report a
/// number that never happened. The cadence keeps running while locked, so the
/// zero-minute reports (which the goal doc defines as reads) still refresh the
/// total — which is what lifts the lock at local midnight, or when the guardian
/// raises the budget.
#[derive(Debug, Clone, Default)]
pub struct UsageHeartbeat {
    /// Whether the ward's policy sets a daily budget. All accounting is gated
    /// on this; `false` keeps every other field at its empty value.
    budget_set: bool,
    /// Epoch seconds at the last [`Self::set_active`]/[`Self::take_due`] call,
    /// the other end of the accrual gap. `None` = no reference point yet.
    last_seen: Option<i64>,
    /// Whether the app is currently being used (foregrounded and not locked).
    active: bool,
    /// Foreground seconds accrued but not yet successfully reported. Whole
    /// minutes are drained by [`Self::take_due`]; the remainder stays so a
    /// ward using the app in short bursts still accumulates honestly.
    unreported_secs: i64,
    /// Minutes handed to a report that has not yet replied. Re-credited whole
    /// by [`Self::report_failed`], because the goal doc defines the delta as
    /// *"since its last **successful** report"* — a dropped report must not
    /// silently forgive the time.
    in_flight_minutes: Option<u32>,
    /// Epoch seconds of the last successful report. `None` = never reported,
    /// which makes the first flush eager (mirroring `NotifyAccumulator`) so a
    /// fresh launch learns its total immediately instead of after one interval.
    last_report: Option<i64>,
    /// The nest's cross-device total for [`Self::nest_day`], from the last
    /// reply or the ward's own `fauna.family.status` read. `None` = never
    /// heard one, which is exactly how [`ScreenTimePolicy::lock_state`] reads
    /// "do not evaluate the budget yet".
    nest_total_minutes: Option<u32>,
    /// The local-day bucket [`Self::nest_total_minutes`] belongs to, as stamped
    /// by the nest. Purely informational to callers; kept so a day change is
    /// observable without a second read.
    nest_day: Option<i64>,
}

impl UsageHeartbeat {
    /// A heartbeat with no policy, no accrual and no known total — the state an
    /// unsupervised (or budget-less) account stays in forever.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt the ward's current screen-time policy, from `fauna.family.status`.
    /// Called on every status read, so a guardian setting or clearing a budget
    /// binds on the ward's next read.
    ///
    /// Clearing the budget **drops all accounting state**: no budget means no
    /// accounting at all, and a stale total left behind would keep locking a
    /// ward whose guardian just lifted the limit.
    pub fn set_policy(&mut self, policy: Option<&ScreenTimePolicy>) {
        let budget_set = policy.and_then(|p| p.daily_minutes).is_some();
        if budget_set == self.budget_set {
            return;
        }
        let active = self.active;
        let last_seen = self.last_seen;
        *self = Self {
            budget_set,
            active,
            last_seen,
            ..Self::default()
        };
    }

    /// Whether accounting is running — i.e. the ward's guardian has declared a
    /// daily budget. Callers use it to skip the whole heartbeat path.
    pub fn is_accounting(&self) -> bool {
        self.budget_set
    }

    /// Record whether the app is being used right now: `active` is
    /// **foregrounded AND not screen-locked** (see the type docs — lock-screen
    /// time is not use). Accrues the elapsed gap first, so the transition is
    /// credited at the instant it happened rather than at the next tick.
    pub fn set_active(&mut self, active: bool, now_secs: i64) {
        self.accrue(now_secs);
        self.active = active;
    }

    /// Seed the cross-device total from the ward's own `fauna.family.status`
    /// read, which carries `usage_today_minutes` for exactly this reason — the
    /// lock can then evaluate the budget on the very first paint, without
    /// waiting for a heartbeat round-trip. A `None` (no budget, or unsupervised)
    /// leaves the total unknown and the budget arm unevaluated.
    pub fn seed_total(&mut self, usage_today_minutes: Option<u32>) {
        if !self.budget_set {
            return;
        }
        if let Some(total) = usage_today_minutes {
            self.nest_total_minutes = Some(total);
        }
    }

    /// The report to send now, or `None` for "nothing due".
    ///
    /// `Some(0)` is a legitimate and important answer: the goal doc defines a
    /// zero-minute report as a **read**, and it is what refreshes the total for
    /// a ward who is locked out, has been idle, or has just crossed local
    /// midnight. The returned minutes are moved to in-flight — the caller MUST
    /// answer with [`Self::report_succeeded`] or [`Self::report_failed`].
    pub fn take_due(&mut self, now_secs: i64) -> Option<u32> {
        self.accrue(now_secs);
        if !self.budget_set || self.in_flight_minutes.is_some() {
            return None;
        }
        if let Some(last) = self.last_report
            && now_secs.saturating_sub(last) < USAGE_REPORT_INTERVAL_SECS
        {
            return None;
        }
        let minutes =
            (self.unreported_secs / 60).clamp(0, i64::from(MAX_USAGE_MINUTES_PER_REPORT)) as u32;
        self.unreported_secs -= i64::from(minutes) * 60;
        self.in_flight_minutes = Some(minutes);
        Some(minutes)
    }

    /// Land a successful `fauna.family.usage_report` reply: the nest's stamped
    /// day bucket and that day's cross-device total.
    pub fn report_succeeded(&mut self, day: i64, day_total_minutes: u32, now_secs: i64) {
        self.in_flight_minutes = None;
        self.last_report = Some(now_secs);
        self.nest_day = Some(day);
        self.nest_total_minutes = Some(day_total_minutes);
    }

    /// A report that never landed. Its minutes go back on the unreported pile —
    /// the delta is defined against the last *successful* report, so a nest
    /// blip must not quietly erase a ward's screen time. The cadence is not
    /// advanced either, so the next tick retries.
    pub fn report_failed(&mut self) {
        if let Some(minutes) = self.in_flight_minutes.take() {
            self.unreported_secs += i64::from(minutes) * 60;
        }
    }

    /// The number to hand [`ScreenTimePolicy::lock_state`] and to display: the
    /// nest's cross-device total **plus** what this device has accrued since,
    /// including minutes currently in flight.
    ///
    /// Adding the local remainder is what keeps the lock accurate to the minute
    /// without a per-minute heartbeat: the cadence bounds only how fast *other
    /// devices* learn about this one. `None` until a total has been heard —
    /// the ratified fail-open on the budget arm (a client that has never
    /// reached the nest does not lock a child out on a guess).
    pub fn used_today_minutes(&self, now_secs: i64) -> Option<u32> {
        let total = self.nest_total_minutes?;
        let local_secs = self.unreported_secs + self.pending_step_secs(now_secs);
        let local_minutes = (local_secs / 60).max(0) as u32;
        Some(
            total
                .saturating_add(local_minutes)
                .saturating_add(self.in_flight_minutes.unwrap_or(0)),
        )
    }

    /// The local-day bucket the current total belongs to, as stamped by the
    /// nest; `None` before the first successful report.
    pub fn nest_day(&self) -> Option<i64> {
        self.nest_day
    }

    /// Drop everything because the **identity is changing** — sign-out, account
    /// switch, factory reset. Without it a new account inherits the previous
    /// ward's accrual and locks against a budget that is not theirs (the bug
    /// class `clear_for_identity_change` exists for).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Fold the elapsed gap into the unreported pile and move the reference
    /// point forward. Only active time counts, and only up to
    /// [`MAX_ACCRUAL_STEP_SECS`] per step.
    fn accrue(&mut self, now_secs: i64) {
        let step = self.pending_step_secs(now_secs);
        if self.budget_set {
            self.unreported_secs += step;
        }
        self.last_seen = Some(now_secs);
    }

    /// The seconds the *next* accrual would credit, without committing them —
    /// so [`Self::used_today_minutes`] can read a live figure from `&self`.
    fn pending_step_secs(&self, now_secs: i64) -> i64 {
        if !self.active {
            return 0;
        }
        let Some(last) = self.last_seen else {
            return 0;
        };
        now_secs
            .saturating_sub(last)
            .clamp(0, MAX_ACCRUAL_STEP_SECS)
    }
}

/// The ward-client lock's owned state, in ONE place both a tui-shaped app
/// (an ordinary struct field) and a linux-shaped app (a `thread_local!` cell)
/// can hold an instance of — tui and linux each hand-rolled this identically
/// (the clock, the window/budget wiring, the test-clock skew) around the
/// policy/heartbeat types above; only *where* the instance lives differs by
/// platform (`family-safety.md` § Screen time). Native-only (`local-clock`):
/// every method needs the OS local clock.
///
/// Default = not supervised = never locked, which is also what an identity
/// change resets to.
#[cfg(feature = "local-clock")]
#[derive(Debug, Default)]
pub struct ScreenLockCore {
    /// The ward's own policy, last seen on a status read. `None` = no policy.
    policy: Option<ScreenTimePolicy>,
    /// The guardian's handle — the lock message names them. `None` = not
    /// supervised, which never locks whatever a stale policy says.
    guardian_handle: Option<String>,
    /// The budget half's engine. All of its rules are shared Rust already;
    /// this type adds only the clock and the window wiring around it.
    heartbeat: UsageHeartbeat,
    /// Test-only clock skew, in seconds (convention 14's fake clock).
    ///
    /// Screen time is the one pillar whose behavior is genuinely a function
    /// of elapsed time, and a test that *slept* for a heartbeat would be
    /// defunct by `testing.md` § point 14. Every clock read on this type goes
    /// through [`Self::now_secs`], so advancing this moves accrual, the
    /// report cadence AND the window verdict together, consistently — the
    /// property a real client-side bug once broke (linux's window check read
    /// the OS clock straight through, bypassing this field entirely).
    /// Compiled out of release artifacts (convention 15): gated on
    /// `debug_assertions`/`e2e-agent`, deliberately NOT `test` — a caller
    /// crate's own test build does not set THIS crate's `cfg(test)`.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    test_clock_skew_secs: i64,
}

#[cfg(feature = "local-clock")]
impl ScreenLockCore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Epoch seconds as this type reckons them — the real clock plus any
    /// test skew.
    fn now_secs(&self) -> i64 {
        let now = chrono::Utc::now().timestamp();
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        let now = now + self.test_clock_skew_secs;
        now
    }

    /// The device's local clock as minutes from local midnight — the unit
    /// [`ScreenTimePolicy`] stores its window bounds in. Derived from
    /// [`Self::now_secs`] rather than a fresh OS clock read, so the test
    /// clock moves the *window* verdict too, not only the *budget* one.
    fn local_minutes_from_midnight(&self) -> u16 {
        use chrono::{Local, TimeZone, Timelike};
        let local = match Local.timestamp_opt(self.now_secs(), 0).single() {
            Some(t) => t,
            // An out-of-range instant cannot be placed on a clock; treat it
            // as local midnight rather than panicking on a display path.
            None => return 0,
        };
        (local.hour() as u16) * 60 + local.minute() as u16
    }

    /// The device's local UTC offset in minutes — what
    /// `fauna.family.usage_report` carries so the nest can stamp the ward's
    /// local day (`family-safety.md` § Screen time: the nest is never told a
    /// timezone, only an offset, and clamps it). Reads the real clock, not
    /// [`Self::now_secs`]: a test session's UTC offset does not move with the
    /// fake clock. Delegates to [`crate::caltime::local_utc_offset_seconds`],
    /// the tree's one device-offset derivation — this
    /// used to hand-roll its own chrono read, independently of tui's and
    /// linux's own doors.
    fn utc_offset_minutes(&self) -> i32 {
        crate::caltime::local_utc_offset_seconds() / 60
    }

    /// Record the supervised viewer's own screen-time policy, guardian, and
    /// the day's cross-device usage total, from `fauna.family.status`. Called
    /// on every status read — post-auth and on each refresh — so a
    /// guardian's policy edit takes effect on the ward's next read rather
    /// than needing a restart. Unsupervised (`None` handle) clears the lock.
    ///
    /// `usage_today_minutes` is the ward's own figure off that same read; it
    /// seeds the heartbeat so the very first paint can already evaluate the
    /// budget, instead of leaving a ward over their limit unlocked until the
    /// first heartbeat round-trip completes.
    pub fn set_ward_screen_time(
        &mut self,
        policy: Option<ScreenTimePolicy>,
        guardian_handle: Option<String>,
        usage_today_minutes: Option<u32>,
    ) {
        self.policy = policy;
        self.guardian_handle = guardian_handle;
        self.heartbeat.set_policy(policy.as_ref());
        self.heartbeat.seed_total(usage_today_minutes);
    }

    /// Drop everything because the **identity is changing** — sign-out,
    /// account switch, factory reset. Without this a new account inherits
    /// the previous ward's lock/accrual, accusing (or crediting) a guardian
    /// the user does not have.
    pub fn clear_for_identity_change(&mut self) {
        self.policy = None;
        self.guardian_handle = None;
        self.heartbeat.reset();
    }

    /// The raw lock decision — no i18n, just whether the ward is currently
    /// locked. [`Self::lock_message`] resolves this into display text;
    /// [`Self::take_due_report`] only ever needed the boolean, so it no
    /// longer pays for a resolve+allocate it throws away.
    fn is_locked(&self) -> bool {
        if self.guardian_handle.is_none() {
            return false;
        }
        let Some(policy) = self.policy else {
            return false;
        };
        let used = self.heartbeat.used_today_minutes(self.now_secs());
        !matches!(
            policy.lock_state(self.local_minutes_from_midnight(), used),
            ScreenLockState::Allowed
        )
    }

    /// The current lock verdict as the localized message to display, or
    /// `None` for "not locked". The single decision point: both the
    /// surface's presence and its text come from here, so they cannot
    /// disagree. `lookup` resolves the machine-composed text (the app's own
    /// i18n table), matching [`CriticalAlerts::active_lines`]'s shape.
    pub fn lock_message<F, S>(&self, lookup: F) -> Option<String>
    where
        F: Fn(&str) -> Option<S>,
        S: AsRef<str>,
    {
        let guardian = self.guardian_handle.as_deref()?;
        let policy = self.policy?;
        let used = self.heartbeat.used_today_minutes(self.now_secs());
        screen_lock_message(&policy, self.local_minutes_from_midnight(), used, guardian)
            .map(|text| text.resolve(lookup))
    }

    /// Feed the heartbeat this moment's activity and ask whether a report is
    /// due.
    ///
    /// `focused` is whether the ward is actually looking at the app; the
    /// engine is told `focused AND not locked`, because time spent staring
    /// at the lock screen is not screen *time* — crediting it would inflate
    /// the guardian's readout with minutes the child never spent.
    ///
    /// Returns the `(minutes, utc_offset_minutes)` to send with
    /// `fauna.family.usage_report`, or `None` when nothing is due. A
    /// `Some(0)` is a real answer — the goal doc defines a zero-minute
    /// report as a *read*, and it is what lifts a budget lock at local
    /// midnight or after a guardian raises the limit. The caller MUST answer
    /// with [`Self::report_succeeded`] or [`Self::report_failed`].
    pub fn take_due_report(&mut self, focused: bool) -> Option<(u32, i32)> {
        let locked = self.is_locked();
        let now = self.now_secs();
        let offset = self.utc_offset_minutes();
        self.heartbeat.set_active(focused && !locked, now);
        self.heartbeat
            .take_due(now)
            .map(|minutes| (minutes, offset))
    }

    /// Land a `fauna.family.usage_report` reply — the nest's stamped day
    /// bucket and that day's cross-device total.
    pub fn report_succeeded(&mut self, day: i64, day_total_minutes: u32) {
        let now = self.now_secs();
        self.heartbeat.report_succeeded(day, day_total_minutes, now);
    }

    /// A report that never landed: its minutes go back on the unreported
    /// pile, so a nest blip cannot quietly forgive a ward's screen time.
    pub fn report_failed(&mut self) {
        self.heartbeat.report_failed();
    }

    /// This device's live usage figure: the nest's cross-device total PLUS
    /// the minutes accrued here since the last successful report.
    ///
    /// **Not what either readout renders** — § Screen time pins both
    /// surfaces to the `usage_today_minutes` field of the
    /// `fauna.family.status` read, precisely so the guardian and the ward
    /// cannot be shown different figures; a readout fed from here would
    /// drift ahead of the guardian's by up to one report interval. It
    /// exists for [`Self::is_locked`]/[`Self::lock_message`] (the budget
    /// lock fires the minute it is reached, not on a cadence) and for tests.
    pub fn used_today_minutes(&self) -> Option<u32> {
        self.heartbeat.used_today_minutes(self.now_secs())
    }

    /// Advance this type's clock by `secs` **for tests only**, in the
    /// accrual steps a real caller would tick in, so the heartbeat credits
    /// time exactly as it would over a real interval (the engine caps a
    /// single gap at `MAX_ACCRUAL_STEP_SECS`, so one big jump would credit
    /// only one step).
    ///
    /// This is convention 14's fake clock: the alternative — a test that
    /// sleeps for a heartbeat — is *defunct* under `testing.md` § point 14,
    /// not merely slow. Compiled out of release artifacts (convention 15).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn advance_test_clock(&mut self, secs: i64) {
        let step = MAX_ACCRUAL_STEP_SECS;
        // Prime the engine's reference point before advancing. It accrues
        // from the GAP between calls, so with no prior call the first step
        // would credit nothing and the poke would silently deliver less use
        // than it was asked for. (Production primes this on the eager first
        // report.)
        let now = self.now_secs();
        self.heartbeat.set_active(true, now);
        let mut remaining = secs.max(0);
        while remaining > 0 {
            let bump = remaining.min(step);
            self.test_clock_skew_secs += bump;
            remaining -= bump;
            // Tick the engine at each step, exactly as the one-minute timer
            // would.
            let now = self.now_secs();
            self.heartbeat.set_active(true, now);
        }
    }

    /// Advance the test clock by exactly `secs`, with **no** engine ticks —
    /// for a test that must move the window verdict without also touching
    /// heartbeat accrual (unlike [`Self::advance_test_clock`], which primes
    /// and ticks the engine as a real caller would).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn advance_test_clock_silently(&mut self, secs: i64) {
        self.test_clock_skew_secs += secs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unset() {
        let p = ScreenTimePolicy::default();
        assert!(p.is_unset());
        assert!(!p.has_window());
    }

    #[test]
    fn serde_omits_and_defaults_absent_fields() {
        // Wire is dag-cbor; the serde derive behavior asserted here is
        // format-agnostic (serde_json is the convenient proxy).
        let p: ScreenTimePolicy =
            serde_json::from_str(r#"{"window_start":1260,"window_end":420}"#).unwrap();
        assert_eq!(p.window_start, Some(1260));
        assert_eq!(p.window_end, Some(420)); // wrapping (bedtime) window
        assert_eq!(p.daily_minutes, None);
        assert!(p.has_window());
        assert!(!p.is_unset());
        // Empty document → the unsupervised-equivalent default.
        assert_eq!(
            serde_json::from_str::<ScreenTimePolicy>("{}").unwrap(),
            ScreenTimePolicy::default()
        );
    }

    #[test]
    fn validate_accepts_every_expressible_real_window() {
        // Plain, wrapping, until-midnight (end = 0 + wrap), and full-day-less-
        // a-minute windows; a zero budget is the deliberate full lock.
        for p in [
            ScreenTimePolicy::default(),
            ScreenTimePolicy {
                window_start: Some(480),
                window_end: Some(1200),
                daily_minutes: None,
            },
            ScreenTimePolicy {
                window_start: Some(1260),
                window_end: Some(420),
                daily_minutes: Some(120),
            },
            ScreenTimePolicy {
                window_start: Some(480),
                window_end: Some(0),
                daily_minutes: None,
            },
            ScreenTimePolicy {
                window_start: Some(0),
                window_end: Some(1439),
                daily_minutes: Some(0),
            },
            ScreenTimePolicy {
                window_start: None,
                window_end: None,
                daily_minutes: Some(1440),
            },
        ] {
            assert_eq!(p.validate(), Ok(()), "{p:?}");
        }
    }

    #[test]
    fn validate_refuses_what_the_nest_cannot_honestly_store() {
        let window = |start, end| ScreenTimePolicy {
            window_start: start,
            window_end: end,
            daily_minutes: None,
        };
        for p in [
            window(Some(1440), Some(420)),  // start out of range
            window(Some(1260), Some(1440)), // end out of range
            window(Some(600), Some(600)),   // empty window — ambiguous
            window(Some(600), None),        // half-set (start only)
            window(None, Some(600)),        // half-set (end only)
            ScreenTimePolicy {
                window_start: None,
                window_end: None,
                daily_minutes: Some(1441), // over a day
            },
        ] {
            assert!(p.validate().is_err(), "{p:?}");
        }
    }

    #[test]
    fn wrapping_bedtime_window_locks_and_unlocks_at_the_ratified_boundaries() {
        // The canonical bedtime case from § Screen time: 21:00–07:00 ==
        // [1260, 420) wrapping midnight. "In window" = allowed to use.
        let p = ScreenTimePolicy {
            window_start: Some(1260),
            window_end: Some(420),
            daily_minutes: None,
        };
        assert!(p.in_window(1380), "23:00 is inside the bedtime window");
        assert!(p.in_window(0), "midnight is inside");
        assert!(p.in_window(360), "06:00 is inside");
        assert!(!p.in_window(420), "07:00 is OUTSIDE — the end is exclusive");
        assert!(!p.in_window(720), "12:00 is outside");
        assert!(!p.in_window(1259), "20:59 is outside");
        assert!(
            p.in_window(1260),
            "21:00 is inside — the start is inclusive"
        );
        assert!(p.in_window(1439), "23:59 is inside");
    }

    #[test]
    fn plain_window_is_half_open() {
        let p = ScreenTimePolicy {
            window_start: Some(480),
            window_end: Some(1200),
            daily_minutes: None,
        };
        assert!(!p.in_window(479));
        assert!(p.in_window(480));
        assert!(p.in_window(1199));
        assert!(!p.in_window(1200), "the end is exclusive");
        // An out-of-range clock reading is normalized, never panics.
        assert!(p.in_window(480 + 1440));
    }

    #[test]
    fn no_window_never_locks_and_corrupt_empty_window_fails_closed() {
        let none = ScreenTimePolicy::default();
        for now in [0, 420, 1260, 1439] {
            assert!(none.in_window(now));
            assert_eq!(
                none.lock_state(now, Some(u32::MAX)),
                ScreenLockState::Allowed
            );
        }
        // start == end is refused at write; if it is ever *read* (corrupt
        // state), it locks — fail-closed, mirroring the content pillar.
        let corrupt = ScreenTimePolicy {
            window_start: Some(600),
            window_end: Some(600),
            daily_minutes: None,
        };
        for now in [0, 599, 600, 601, 1439] {
            assert!(!corrupt.in_window(now));
        }
    }

    #[test]
    fn budget_locks_at_exhaustion_and_window_takes_precedence() {
        let p = ScreenTimePolicy {
            window_start: Some(1260),
            window_end: Some(420),
            daily_minutes: Some(120),
        };
        // Inside the window: the budget decides.
        assert_eq!(p.lock_state(1380, Some(0)), ScreenLockState::Allowed);
        assert_eq!(p.lock_state(1380, Some(119)), ScreenLockState::Allowed);
        assert_eq!(p.lock_state(1380, Some(120)), ScreenLockState::LockedBudget);
        assert_eq!(p.lock_state(1380, Some(500)), ScreenLockState::LockedBudget);
        // Total not yet known → the budget is not evaluated.
        assert_eq!(p.lock_state(1380, None), ScreenLockState::Allowed);
        // Outside the window, the window names the lock even when the budget
        // is also exhausted.
        assert_eq!(p.lock_state(720, Some(500)), ScreenLockState::LockedWindow);
        // The deliberate zero budget locks from the first known minute.
        let grounded = ScreenTimePolicy {
            window_start: None,
            window_end: None,
            daily_minutes: Some(0),
        };
        assert_eq!(
            grounded.lock_state(720, Some(0)),
            ScreenLockState::LockedBudget
        );
        assert_eq!(grounded.lock_state(720, None), ScreenLockState::Allowed);
    }

    #[test]
    fn round_trips() {
        let p = ScreenTimePolicy {
            window_start: Some(0),
            window_end: Some(1439),
            daily_minutes: Some(120),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<ScreenTimePolicy>(&s).unwrap(), p);
    }

    #[test]
    fn time_of_day_parses_what_a_guardian_types() {
        assert_eq!(parse_time_of_day("21:00"), Ok(Some(1260)));
        assert_eq!(parse_time_of_day("7:00"), Ok(Some(420)));
        assert_eq!(parse_time_of_day("00:00"), Ok(Some(0)));
        assert_eq!(parse_time_of_day("23:59"), Ok(Some(1439)));
        assert_eq!(parse_time_of_day("  08:30  "), Ok(Some(510)));
        // Clearing a bound — how a guardian removes the window.
        assert_eq!(parse_time_of_day(""), Ok(None));
        assert_eq!(parse_time_of_day("   "), Ok(None));
    }

    #[test]
    fn time_of_day_refuses_what_the_policy_could_not_store() {
        // Every one of these would otherwise assemble a policy the nest
        // refuses, with nothing on screen explaining why.
        for bad in ["21", "21:60", "24:00", "9:5x", "abc", "-1:00", "21:00:00"] {
            assert!(parse_time_of_day(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn time_of_day_round_trips_through_its_display_form() {
        for minutes in [0u16, 1, 420, 510, 720, 1260, 1439] {
            let shown = format_time_of_day(minutes);
            assert_eq!(
                parse_time_of_day(&shown),
                Ok(Some(minutes)),
                "{minutes} rendered as {shown}"
            );
        }
        assert_eq!(format_time_of_day(1260), "21:00");
        assert_eq!(format_time_of_day(0), "00:00");
        // Total: a day-or-more value wraps rather than panicking.
        assert_eq!(format_time_of_day(1440), "00:00");
    }

    #[test]
    fn daily_budget_parses_and_refuses_at_the_same_bound_as_validate() {
        assert_eq!(parse_daily_minutes(""), Ok(None));
        assert_eq!(parse_daily_minutes("0"), Ok(Some(0))); // deliberate full lock
        assert_eq!(parse_daily_minutes("90"), Ok(Some(90)));
        assert_eq!(parse_daily_minutes(" 1440 "), Ok(Some(1440)));
        for bad in ["1441", "-5", "1.5", "ninety", "90m"] {
            assert!(
                parse_daily_minutes(bad).is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn lock_message_gates_exactly_as_lock_state_and_names_the_policy() {
        // The window is when use is ALLOWED, so the bedtime rule "no device
        // after 21:00" is the daytime window 07:00–21:00.
        let daytime = ScreenTimePolicy {
            window_start: Some(420), // 07:00
            window_end: Some(1260),  // 21:00
            daily_minutes: Some(120),
        };
        // Inside the window and under budget → no lock, no message.
        assert_eq!(screen_lock_message(&daytime, 720, Some(30), "mum"), None);

        // Past bedtime → named by when use resumes, plus the guardian.
        let m = screen_lock_message(&daytime, 1300, Some(0), "mum").expect("locked");
        assert_eq!(m.key, "family.screen_lock_window");
        assert_eq!(m.args.get("resumes").map(String::as_str), Some("07:00"));
        assert_eq!(m.args.get("guardian").map(String::as_str), Some("mum"));

        // Over budget inside the window → named by the budget that ran out.
        let m = screen_lock_message(&daytime, 720, Some(120), "mum").expect("locked");
        assert_eq!(m.key, "family.screen_lock_budget");
        assert_eq!(m.args.get("minutes").map(String::as_str), Some("120"));
        assert_eq!(m.args.get("guardian").map(String::as_str), Some("mum"));

        // The unsupervised-equivalent default never locks.
        assert_eq!(
            screen_lock_message(&ScreenTimePolicy::default(), 720, Some(9999), "mum"),
            None
        );
    }

    #[test]
    fn lock_message_agrees_with_lock_state_across_the_whole_day() {
        // The gating contract, swept rather than sampled: for every minute of
        // the day the message is present exactly when `lock_state` locks. This
        // is what stops a render surface from drifting off the decision fn.
        let policy = ScreenTimePolicy {
            window_start: Some(1260),
            window_end: Some(420),
            daily_minutes: Some(60),
        };
        for now in 0..MINUTES_PER_DAY {
            for used in [None, Some(0u32), Some(59), Some(60), Some(600)] {
                let locked = policy.lock_state(now, used) != ScreenLockState::Allowed;
                let message = screen_lock_message(&policy, now, used, "mum");
                assert_eq!(
                    locked,
                    message.is_some(),
                    "minute {now}, used {used:?}: lock_state and screen_lock_message disagree"
                );
            }
        }
    }

    #[test]
    fn a_corrupt_window_locks_with_a_message_rather_than_failing_open() {
        // `start == end` is refused at write, so it can only be corrupt state.
        // `in_window` fails closed; the surface must still be able to explain
        // itself (and the Family page stays reachable, so it is recoverable).
        let corrupt = ScreenTimePolicy {
            window_start: Some(600),
            window_end: Some(600),
            daily_minutes: None,
        };
        let m = screen_lock_message(&corrupt, 600, None, "mum").expect("fails closed to locked");
        assert_eq!(m.key, "family.screen_lock_window");
        assert_eq!(m.args.get("resumes").map(String::as_str), Some("10:00"));
    }

    #[test]
    fn a_parsed_editor_assembles_only_policies_validate_accepts() {
        // The editor contract: whatever the three parsers accept, `validate`
        // must also accept — otherwise a guardian's legal-looking entry is
        // refused by the nest with no local explanation. The one exception is
        // the pair rules (half-set, start == end), which are *cross-field* and
        // so belong to `validate` alone; the parsers cannot see them.
        for (start, end, budget) in [
            ("21:00", "07:00", "120"), // wrapping bedtime window + budget
            ("08:00", "20:00", ""),    // plain window, no budget
            ("", "", "90"),            // budget only
            ("", "", ""),              // nothing set
            ("00:00", "23:59", "0"),   // extremes + the deliberate full lock
        ] {
            let p = ScreenTimePolicy {
                window_start: parse_time_of_day(start).unwrap(),
                window_end: parse_time_of_day(end).unwrap(),
                daily_minutes: parse_daily_minutes(budget).unwrap(),
            };
            assert_eq!(p.validate(), Ok(()), "{start:?} {end:?} {budget:?}");
        }
    }

    // ── UsageHeartbeat — the cadence, on a mock clock ────────────────────
    //
    // Every one of these drives `now_secs` by hand. That is the point:
    // `testing.md` § point 14 rules a test whose correctness depends on
    // wall-clock timing DEFUNCT, and the cadence is the most timing-shaped
    // thing in this pillar. The behavior is a pure function of the call
    // sequence, so it is provable here in microseconds and nothing downstream
    // ever needs to sleep for a heartbeat.

    /// A ward whose guardian set a 120-minute budget.
    fn budgeted() -> ScreenTimePolicy {
        ScreenTimePolicy {
            daily_minutes: Some(120),
            ..Default::default()
        }
    }

    /// A heartbeat with a budget, active since `t0`.
    fn started(t0: i64) -> UsageHeartbeat {
        let mut h = UsageHeartbeat::new();
        h.set_policy(Some(&budgeted()));
        h.set_active(true, t0);
        h
    }

    /// Drive the clock the way a real app does — one tick a minute, which is
    /// what linux's `LOCK_TICK_SECS` and web's `lockTick` both already run.
    ///
    /// A test that jumped straight from `from` to `to` would be modelling a
    /// *suspended* device, not a used one: the engine caps a single gap at
    /// [`MAX_ACCRUAL_STEP_SECS`] precisely so a closed lid cannot bill a child
    /// for eight hours of screen time. Ticking here keeps the caller contract
    /// ("tick at least every two minutes") visible in the tests that depend
    /// on it — `a_suspended_device_credits_at_most_one_accrual_step` is the
    /// one that deliberately violates it.
    fn tick_to(h: &mut UsageHeartbeat, from: i64, to: i64) {
        let mut t = from;
        while t < to {
            t = (t + 60).min(to);
            h.set_active(h.active, t);
        }
    }

    #[test]
    fn no_budget_means_no_accounting_at_all() {
        let mut h = UsageHeartbeat::new();
        // A policy with a window but no budget: enforcement is pure local
        // clock, so there is nothing to report (goal doc: "Reporting and
        // accounting run only while a daily budget is set").
        h.set_policy(Some(&ScreenTimePolicy {
            window_start: Some(1260),
            window_end: Some(420),
            daily_minutes: None,
        }));
        assert!(!h.is_accounting());
        h.set_active(true, 0);
        assert_eq!(
            h.take_due(10_000),
            None,
            "a budget-less ward must send nothing"
        );
        assert_eq!(h.used_today_minutes(10_000), None);
    }

    #[test]
    fn the_first_report_is_eager_and_is_a_read() {
        let mut h = started(1_000);
        // Nothing accrued yet, but the client still wants the day's total so
        // the lock can evaluate the budget: a zero-minute report IS a read.
        assert_eq!(h.take_due(1_000), Some(0));
        assert_eq!(h.used_today_minutes(1_000), None, "no total heard yet");
        h.report_succeeded(20_000, 45, 1_000);
        assert_eq!(h.used_today_minutes(1_000), Some(45));
        assert_eq!(h.nest_day(), Some(20_000));
    }

    #[test]
    fn minutes_accrue_only_while_active_and_flush_on_the_cadence() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 0, 0);

        // Four minutes of use, then the app goes to the background for an hour.
        tick_to(&mut h, 0, 240);
        h.set_active(false, 240);
        tick_to(&mut h, 240, 240 + 3600);
        // Well past the cadence, but the backgrounded hour credits nothing.
        let due = h.take_due(240 + 3600).unwrap();
        assert_eq!(due, 4, "only foreground time counts");
    }

    #[test]
    fn the_cadence_holds_between_reports() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 0, 0);
        // A tick one second before the interval elapses is not due...
        assert_eq!(h.take_due(USAGE_REPORT_INTERVAL_SECS - 1), None);
        // ...and one at the interval is.
        assert!(h.take_due(USAGE_REPORT_INTERVAL_SECS).is_some());
    }

    #[test]
    fn a_failed_report_re_credits_its_minutes_and_retries() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 10, 0);

        let t = USAGE_REPORT_INTERVAL_SECS; // 300s = 5 min of use
        tick_to(&mut h, 0, t);
        let due = h.take_due(t).unwrap();
        assert_eq!(due, 5);
        // The nest never answered. The delta is defined against the last
        // SUCCESSFUL report, so those five minutes are still owed.
        h.report_failed();
        assert_eq!(
            h.take_due(t).unwrap(),
            5,
            "a dropped report must not forgive the time, and must not wait out \
             another interval"
        );
        h.report_succeeded(20_000, 15, t);
        assert_eq!(h.used_today_minutes(t), Some(15));
    }

    #[test]
    fn a_report_in_flight_is_not_duplicated() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        // The reply has not landed; a second tick must not fire a second report
        // (which would double-credit the same minutes on the nest).
        assert_eq!(h.take_due(USAGE_REPORT_INTERVAL_SECS * 3), None);
    }

    #[test]
    fn the_lock_sees_local_minutes_before_they_are_reported() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 118, 0);
        let p = budgeted();
        // 118 of 120 minutes used cross-device, and this device has now been
        // in use for two more. The cadence has not fired — and must not need
        // to: the ward is over budget NOW.
        assert_eq!(h.used_today_minutes(120), Some(120));
        assert_eq!(
            p.lock_state(600, h.used_today_minutes(120)),
            ScreenLockState::LockedBudget,
            "the lock cannot wait {USAGE_REPORT_INTERVAL_SECS}s for a heartbeat"
        );
    }

    #[test]
    fn in_flight_minutes_still_count_locally_until_confirmed() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 100, 0);
        let t = USAGE_REPORT_INTERVAL_SECS;
        tick_to(&mut h, 0, t);
        assert_eq!(h.take_due(t), Some(5));
        // The reply is in flight: the nest total is still the old 100, so the
        // five minutes must be counted here or the ward briefly gets them free.
        assert_eq!(h.used_today_minutes(t), Some(105));
        h.report_succeeded(20_000, 105, t);
        assert_eq!(h.used_today_minutes(t), Some(105), "and never twice");
    }

    #[test]
    fn a_suspended_device_credits_at_most_one_accrual_step() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 0, 0);
        // The laptop lid was shut for eight hours with the app foregrounded.
        // Crediting that would lock the ward out for time they never spent.
        let due = h.take_due(8 * 3600).unwrap();
        assert_eq!(due, (MAX_ACCRUAL_STEP_SECS / 60) as u32);
    }

    #[test]
    fn lock_screen_time_is_not_use() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 120, 0);
        // Over budget → the app locks → the caller passes active = false.
        h.set_active(false, 0);
        // An hour on the lock screen, ticking all the while.
        let mut t = 0;
        for _ in 0..60 {
            t += 60;
            if let Some(minutes) = h.take_due(t) {
                assert_eq!(minutes, 0, "a locked ward reports reads, never use");
                h.report_succeeded(20_000, 120, t);
            }
        }
        assert_eq!(
            h.used_today_minutes(t),
            Some(120),
            "an hour of lock screen must not read as an hour of screen time"
        );
    }

    #[test]
    fn a_locked_ward_keeps_reading_so_the_lock_can_lift() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 120, 0);
        h.set_active(false, 0);
        // Local midnight passes: the nest stamps a new day and reports zero.
        let t = USAGE_REPORT_INTERVAL_SECS;
        assert_eq!(
            h.take_due(t),
            Some(0),
            "the read must still fire while locked"
        );
        h.report_succeeded(20_001, 0, t);
        assert_eq!(h.used_today_minutes(t), Some(0));
        assert_eq!(
            budgeted().lock_state(600, h.used_today_minutes(t)),
            ScreenLockState::Allowed,
            "the new day must lift the budget lock with no user action"
        );
    }

    #[test]
    fn clearing_the_budget_drops_the_accounting_state() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 500, 0);
        // The guardian removes the budget. A stale 500 left behind would keep
        // locking a ward whose limit was just lifted.
        h.set_policy(Some(&ScreenTimePolicy::default()));
        assert!(!h.is_accounting());
        assert_eq!(h.used_today_minutes(0), None);
        assert_eq!(h.take_due(10_000), None);
    }

    #[test]
    fn setting_the_same_policy_twice_does_not_reset_progress() {
        // `set_policy` runs on EVERY status read, so it must be idempotent —
        // otherwise a periodic refresh would forever discard the accrual.
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 30, 0);
        h.set_policy(Some(&budgeted()));
        assert_eq!(h.used_today_minutes(0), Some(30));
        h.set_policy(Some(&ScreenTimePolicy {
            daily_minutes: Some(240), // a different budget is still a budget
            ..Default::default()
        }));
        assert_eq!(h.used_today_minutes(0), Some(30));
    }

    #[test]
    fn the_status_read_seeds_the_total_before_any_heartbeat() {
        let mut h = UsageHeartbeat::new();
        h.set_policy(Some(&budgeted()));
        // `fauna.family.status` carries the ward's own usage_today_minutes for
        // exactly this reason — the very first paint can evaluate the budget.
        h.seed_total(Some(121));
        assert_eq!(h.used_today_minutes(0), Some(121));
        assert_eq!(
            budgeted().lock_state(600, h.used_today_minutes(0)),
            ScreenLockState::LockedBudget,
        );
    }

    #[test]
    fn seeding_without_a_budget_is_ignored() {
        let mut h = UsageHeartbeat::new();
        h.seed_total(Some(90));
        assert_eq!(h.used_today_minutes(0), None);
    }

    #[test]
    fn an_identity_change_drops_the_previous_wards_usage() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 90, 0);
        h.reset();
        assert!(!h.is_accounting());
        assert_eq!(h.used_today_minutes(0), None);
        assert_eq!(h.nest_day(), None);
    }

    #[test]
    fn sub_minute_bursts_accumulate_instead_of_rounding_away() {
        let mut h = started(0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 0, 0);
        // Six 30-second visits across the interval: 3 minutes of real use that
        // a per-flush truncation would round to zero every time.
        let mut t = 0;
        for _ in 0..6 {
            h.set_active(true, t);
            t += 30;
            h.set_active(false, t);
            t += 30;
        }
        h.set_active(true, t);
        let t = USAGE_REPORT_INTERVAL_SECS.max(t);
        assert_eq!(h.take_due(t).unwrap(), 3);
    }

    #[test]
    fn one_report_never_exceeds_the_nests_clamp() {
        let mut h = UsageHeartbeat::new();
        h.set_policy(Some(&budgeted()));
        h.set_active(true, 0);
        assert_eq!(h.take_due(0), Some(0));
        h.report_succeeded(20_000, 0, 0);
        // Drive far past a day of accrual one honest step at a time.
        let mut t = 0;
        for _ in 0..(MAX_USAGE_MINUTES_PER_REPORT + 100) {
            t += MAX_ACCRUAL_STEP_SECS;
            h.set_active(true, t);
        }
        let due = h.take_due(t).unwrap();
        assert!(
            due <= MAX_USAGE_MINUTES_PER_REPORT,
            "sending more than the nest credits silently loses the excess: {due}"
        );
    }
}

#[cfg(all(test, feature = "local-clock"))]
mod screen_lock_core_tests {
    use super::*;

    /// A ward supervised by `guardian`, under `policy`, with the nest's total
    /// already seeded — the state the first paint after a status read is in.
    fn ward(policy: ScreenTimePolicy, used: Option<u32>) -> ScreenLockCore {
        let mut lock = ScreenLockCore::default();
        lock.set_ward_screen_time(Some(policy), Some("mum".into()), used);
        lock
    }

    /// A stub i18n table covering the two keys [`screen_lock_message`] can
    /// emit — real templates, so a test that checks the resolved TEXT (e.g.
    /// "names the guardian") exercises actual substitution rather than the
    /// no-lookup fallback, which would leave `{guardian}` unexpanded.
    fn lookup(k: &str) -> Option<&'static str> {
        match k {
            "family.screen_lock_window" => Some("Resumes at {resumes}. Set by {guardian}."),
            "family.screen_lock_budget" => Some("Budget of {minutes} reached. Set by {guardian}."),
            _ => None,
        }
    }

    /// A budget already exhausted at the seed locks on the FIRST paint — the
    /// whole point of seeding the heartbeat off the status read rather than
    /// waiting for a heartbeat round-trip. Without the seed a ward over their
    /// limit would get a free session every launch.
    #[test]
    fn a_seeded_over_budget_total_locks_on_the_first_paint() {
        let lock = ward(
            ScreenTimePolicy {
                daily_minutes: Some(30),
                ..Default::default()
            },
            Some(30),
        );
        let message = lock
            .lock_message(lookup)
            .expect("an exhausted budget locks");
        assert!(
            message.contains("mum"),
            "the lock names the guardian, got {message:?}"
        );
    }

    /// The same budget, under it, does NOT lock — the negative half, which is
    /// what makes the positive one meaningful.
    #[test]
    fn a_seeded_under_budget_total_does_not_lock() {
        let lock = ward(
            ScreenTimePolicy {
                daily_minutes: Some(30),
                ..Default::default()
            },
            Some(29),
        );
        assert!(lock.lock_message(lookup).is_none());
    }

    /// An unsupervised account never locks, even carrying a policy that
    /// would. The guard is the guardian handle, not the policy — a stale
    /// policy left by a previous identity must not accuse a guardian the
    /// user lacks.
    #[test]
    fn no_guardian_never_locks() {
        let mut lock = ScreenLockCore::default();
        lock.set_ward_screen_time(
            Some(ScreenTimePolicy {
                daily_minutes: Some(1),
                ..Default::default()
            }),
            None,
            Some(600),
        );
        assert!(lock.lock_message(lookup).is_none());
    }

    /// Clearing the budget lifts an existing lock on the next status read:
    /// the engine drops its accounting state with the policy, so a stale
    /// total cannot keep locking a ward whose guardian just raised the
    /// limit.
    #[test]
    fn clearing_the_budget_lifts_the_lock() {
        let mut lock = ward(
            ScreenTimePolicy {
                daily_minutes: Some(30),
                ..Default::default()
            },
            Some(45),
        );
        assert!(lock.lock_message(lookup).is_some());
        lock.set_ward_screen_time(Some(ScreenTimePolicy::default()), Some("mum".into()), None);
        assert!(lock.lock_message(lookup).is_none());
        // …and with no budget there is no readout at all, rather than a zero.
        assert_eq!(lock.used_today_minutes(), None);
    }

    /// The poke really accrues: advancing the test clock while a budget is
    /// set moves the ward's own figure.
    #[test]
    fn advancing_the_test_clock_accrues_and_can_reach_the_budget() {
        let mut lock = ward(
            ScreenTimePolicy {
                daily_minutes: Some(10),
                ..Default::default()
            },
            Some(0),
        );
        assert!(
            lock.lock_message(lookup).is_none(),
            "starts unlocked at 0/10"
        );
        lock.advance_test_clock(10 * 60);
        assert_eq!(
            lock.used_today_minutes(),
            Some(10),
            "ten minutes of use is ten minutes of use"
        );
        assert!(
            lock.lock_message(lookup).is_some(),
            "reaching the budget locks without waiting for a report"
        );
    }

    /// A failed report re-credits its minutes rather than forgiving them —
    /// the goal doc defines the delta against the last *successful* report,
    /// so a nest blip must not hand a ward free screen time.
    #[test]
    fn a_failed_report_keeps_the_minutes_on_this_devices_pile() {
        let mut lock = ward(
            ScreenTimePolicy {
                daily_minutes: Some(60),
                ..Default::default()
            },
            Some(0),
        );
        lock.advance_test_clock(5 * 60);
        let (minutes, _offset) = lock
            .take_due_report(true)
            .expect("the first report is eager");
        assert_eq!(minutes, 5);
        lock.report_failed();
        assert_eq!(
            lock.used_today_minutes(),
            Some(5),
            "a dropped report leaves the minutes accounted locally"
        );
    }

    /// Time on the lock screen is not use: once locked, [`ScreenLockCore::
    /// take_due_report`] tells the engine `active = false`, so a ward
    /// staring at the lock accrues nothing further.
    #[test]
    fn time_on_the_lock_screen_does_not_accrue() {
        let mut lock = ward(
            ScreenTimePolicy {
                daily_minutes: Some(5),
                ..Default::default()
            },
            Some(0),
        );
        lock.advance_test_clock(5 * 60);
        assert!(
            lock.lock_message(lookup).is_some(),
            "the budget is exhausted"
        );
        lock.take_due_report(true);
        let used_at_lock = lock.used_today_minutes();
        for _ in 0..4 {
            lock.advance_test_clock_silently(LOCK_TICK_SECS);
            lock.take_due_report(true);
        }
        assert_eq!(
            lock.used_today_minutes(),
            used_at_lock,
            "the lock screen must not bill the child"
        );
        assert!(
            lock.lock_message(lookup).is_some(),
            "and the lock is still up — nothing lifted it"
        );
    }

    /// How often a real client re-evaluates the lock — mirrors tui's
    /// `LOCK_TICK` / linux's `LOCK_TICK_SECS`, both 60 seconds.
    const LOCK_TICK_SECS: i64 = 60;

    /// Advancing the test clock must move the WINDOW verdict, not just the
    /// budget one — `local_minutes_from_midnight` has to read the same
    /// skewed clock every other read on this type goes through (the bug a
    /// hand-rolled linux twin of this exact type once had: it read the OS
    /// clock straight through, bypassing the skew entirely). `[start, end)`
    /// is the *allowed* range, so a window covering "now" through two hours
    /// out, followed by a clock advance past its end, must LOCK.
    #[test]
    fn advancing_the_test_clock_moves_the_window_verdict_too() {
        let mut lock = ScreenLockCore::default();
        let now = lock.local_minutes_from_midnight();
        let start = (now + 1430) % 1440; // now - 10 minutes, mod a day
        let end = (now + 130) % 1440;
        lock.set_ward_screen_time(
            Some(ScreenTimePolicy {
                window_start: Some(start),
                window_end: Some(end),
                daily_minutes: None,
            }),
            Some("mum".into()),
            None,
        );
        assert_eq!(
            lock.lock_message(lookup),
            None,
            "now sits inside [now-10, now+130), the allowed range"
        );

        lock.advance_test_clock(140 * 60);
        assert!(
            lock.lock_message(lookup).is_some(),
            "140 real minutes advanced must land past the window's end at now+130"
        );
    }
}

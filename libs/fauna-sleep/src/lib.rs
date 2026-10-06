//! The cross-target async sleep — `tokio::time::sleep` natively, the browser's
//! own timer on wasm32.
//!
//! ## Why a crate exists for ten lines
//!
//! tokio's timer does not build for `wasm32-unknown-unknown`, so every crate
//! that needs to wait and also ships to the web has to write the `#[cfg]` split
//! itself. Seven copies of it had accumulated across six crates, each dragging
//! its own `gloo-timers` dependency line, and they had already drifted into
//! three different signatures (`Duration`, `u64` milliseconds, `u64` seconds)
//! with three different names — `sleep`, `sleep_ms`, `sleep_cross_target`,
//! `cross_platform_sleep_secs`, `provision_backoff` — for one behaviour.
//!
//! The deps here are **target-gated, not feature-gated**, which is why this is
//! its own crate rather than a `fauna-core` module: fauna-core's `tokio` is
//! `network`-gated (redb, hickory-resolver) and declared untargeted, so a helper
//! there would either force `network` on six client crates or require moving
//! that entry — a manifest change to the workspace's most central crate to serve
//! this. Nothing new enters the dependency tree either way: `gloo-timers` was
//! already a direct dependency of all six consumers, so this crate replaces six
//! declarations with one.
//!
//! ## What this is NOT
//!
//! **Not an injectable clock.** Where a test needs to control time, the seam
//! belongs on the caller's own trait — `fauna-mail`'s IMAP transport
//! (`sleep_ms` on the transport trait) is the built example, and
//! `e2e-conventions.md` § convention 14 asks for exactly that. Do not collapse
//! such a seam into this function; it would delete the test hook. This is for
//! the other case: a real wait whose only variation is the target it compiles
//! for.
//!
//! **Not a timeout or a race.** `fauna_protocol::reconnect` deliberately takes
//! the timer future from its caller so it needs no clock, no runtime and no
//! randomness of its own; that dep-agnostic shape is right for a pure algorithm
//! and is untouched by this.

use core::time::Duration;

/// Sleep for `d`, on whichever target this was compiled for.
///
/// The one primitive. Callers that think in milliseconds or seconds construct
/// the [`Duration`] at the call site — the drifted `_ms` / `_secs` spellings this
/// replaced are what let one of them ship a **seconds value into a milliseconds
/// parameter**, turning a 5-second backoff into 83 minutes
/// (`fauna-onboarding-machine`'s own comment records that defect).
///
/// ⚠ **wasm caps at `u32::MAX` milliseconds (~49 days)** — the browser timer's
/// own parameter type. Saturating rather than wrapping is deliberate: a longer
/// request is a caller bug, and waiting too long is recoverable where waiting
/// almost no time at all is a silent hot loop.
pub async fn sleep(d: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::time::sleep(d).await;
    }
    #[cfg(target_arch = "wasm32")]
    {
        let ms = u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
        gloo_timers::future::TimeoutFuture::new(ms).await;
    }
}

/// Retry `op` up to `max_attempts` times (inclusive of the first try), waiting
/// `delay` between attempts, until it returns `Ok`.
///
/// Built on [`sleep`] so it needs the same one `#[cfg]` split as every other
/// wait in the workspace — none of its own. `max_attempts == 0` runs `op` once
/// (there is no "wait zero times" reading that skips the call). Three
/// hand-rolled copies of exactly this loop (`fauna-sync-engine`'s
/// `fetch_folders_retry` / `load_user_config_retry`, `fauna-linux`'s
/// `async_helper::hydrate_with_retry`) had accumulated by 2026-08-26, all
/// tolerating the same post-login WS-RPC warm-up race with the same 10×500ms
/// shape; this is the one home.
pub async fn retry<T, E, Fut, F>(max_attempts: usize, delay: Duration, mut op: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: core::future::Future<Output = Result<T, E>>,
{
    let mut result = op().await;
    for _ in 1..max_attempts {
        if result.is_ok() {
            break;
        }
        sleep(delay).await;
        result = op().await;
    }
    result
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// The clamp is the only branch here with a wrong answer available, and it
    /// is unreachable from the native arm — so it is factored out and tested
    /// directly. A wrapping conversion would turn a 50-day wait into a
    /// sub-second one, which reads downstream as a hot loop rather than as the
    /// caller bug it is.
    fn wasm_millis(d: Duration) -> u32 {
        u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
    }

    #[test]
    fn an_ordinary_duration_converts_exactly() {
        assert_eq!(wasm_millis(Duration::from_millis(5_000)), 5_000);
        assert_eq!(wasm_millis(Duration::from_secs(60)), 60_000);
        assert_eq!(wasm_millis(Duration::ZERO), 0);
    }

    #[test]
    fn a_duration_past_the_browser_timers_range_saturates_rather_than_wrapping() {
        // u32::MAX ms is ~49.7 days; ask for 60 and the answer must be the cap,
        // not the 10-day wrap a plain `as u32` would produce.
        let sixty_days = Duration::from_secs(60 * 24 * 60 * 60);
        assert_eq!(wasm_millis(sixty_days), u32::MAX);
        assert_eq!(
            wasm_millis(Duration::from_millis(u32::MAX as u64 + 1)),
            u32::MAX
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_native_arm_actually_waits_the_requested_span() {
        // A paused clock makes this assert the REQUEST, not wall-clock timing
        // (convention 14): tokio auto-advances only when the sleep is the sole
        // pending work, so the elapsed reading is exactly what was asked for.
        let start = tokio::time::Instant::now();
        sleep(Duration::from_secs(30)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_returns_the_first_success_without_sleeping() {
        let calls = std::cell::Cell::new(0);
        let start = tokio::time::Instant::now();
        let result = retry(5, Duration::from_millis(500), || {
            calls.set(calls.get() + 1);
            async { Ok::<_, &str>(7) }
        })
        .await;
        assert_eq!(result, Ok(7));
        assert_eq!(calls.get(), 1);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_succeeds_after_transient_failures_and_stops_sleeping() {
        let calls = std::cell::Cell::new(0);
        let start = tokio::time::Instant::now();
        let result = retry(5, Duration::from_millis(500), || {
            let n = calls.get() + 1;
            calls.set(n);
            async move { if n < 3 { Err("not yet") } else { Ok(n) } }
        })
        .await;
        assert_eq!(result, Ok(3));
        assert_eq!(calls.get(), 3);
        // Two failures, so two 500ms sleeps before the third (successful) call.
        assert_eq!(start.elapsed(), Duration::from_millis(1000));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_exhausts_attempts_and_returns_the_final_error() {
        let calls = std::cell::Cell::new(0);
        let start = tokio::time::Instant::now();
        let result = retry(3, Duration::from_millis(500), || {
            calls.set(calls.get() + 1);
            async { Err::<i32, _>("still failing") }
        })
        .await;
        assert_eq!(result, Err("still failing"));
        // max_attempts calls total, max_attempts - 1 sleeps between them.
        assert_eq!(calls.get(), 3);
        assert_eq!(start.elapsed(), Duration::from_millis(1000));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_with_max_attempts_zero_still_calls_once() {
        let calls = std::cell::Cell::new(0);
        let result = retry(0, Duration::from_millis(500), || {
            calls.set(calls.get() + 1);
            async { Err::<i32, _>("nope") }
        })
        .await;
        assert_eq!(result, Err("nope"));
        assert_eq!(calls.get(), 1);
    }
}

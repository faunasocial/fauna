//! The **client's own clock** — the one `now` every app-held bearer is anchored
//! and scheduled on (`login.md` § Token lifetime on the client's clock), plus
//! the e2e offset that lets a test make that clock *wrong*.
//!
//! **Why one clock, and why here.** A bearer's deadline is anchored at receipt
//! (`deadline = now_client + expires_in`, [`crate::auth::deadline_on_own_clock`])
//! and later compared against the same device's `now` — by the launch machine's
//! TTL loop on tui and linux (`fauna_launch_machine::launch_clock`), by
//! `fauna-client`'s `TokenCache` on the four UniFFI apps,
//! and at the mint itself (`fauna_anon_client::MintedBearer`). The wrong-clock
//! witnesses (`test_onboarding_launch_routing_smoke.py` cases L and M) skew
//! that clock through [`OFFSET_ENV`]; an offset reaching only SOME of those
//! reads would make a witness on a seat whose refresh reads another clock pass
//! without testing anything. So every one of them reads this module, and the
//! offset exists exactly once. It sits in this crate because this is the
//! lowest crate every reader reaches — `fauna-anon-client` is a native-only
//! leaf below `fauna-launch-machine`, which also builds for wasm — and beside
//! the deadline conversion it feeds.
//!
//! **What it deliberately does NOT reach.** The nest's clock: the nest never
//! reads this module, and it is not `fauna_core::data::Timestamp` (which is
//! the nest's clock too — an offset there would skew the server side of the
//! ±30 s handshake check case L measures against). Nor the per-domain e2e
//! clocks (`fauna_client_backup::audit_clock`,
//! `fauna_atproto_settings_machine::delegation_clock`,
//! `fauna_sync_engine::ceremony_clock`, `fauna_client_capabilities::trust_clock`),
//! which stay separate on purpose: a test of one of those domains must never
//! silently move the bearer clock, or the reverse.
//!
//! **The seed is an environment variable.** The offset must be live before an
//! app's launch runs its silent challenge, and every app's automation agent
//! starts in the same startup sequence that immediately drives the launch, so
//! no harness command can land first. [`OFFSET_ENV`] (signed seconds) is read
//! once, on first use, and seeds the same static the setter writes; the setter
//! stays for a mid-session move and for web, whose browser has no environment
//! (`fauna-wasm-launch` seeds it from a localStorage key of the same name).
//! Compile-gated outer, env inner (e2e convention 15): a release build without
//! this crate's `e2e-agent` feature never reads the variable and has no static
//! to seed — the production twin of every accessor is the real clock. Each
//! app's test flavor reaches the feature through `fauna-launch-machine`'s and
//! `fauna-client`'s own `e2e-agent`, which forward it.

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
use std::sync::atomic::Ordering;

use fauna_core::data::Timestamp;

/// The environment variable the offset is seeded from — signed seconds added
/// to the client's `now`; unset, empty or unparseable leaves the real clock.
/// Gated with the static it seeds, so the name is absent from a release
/// artifact along with the read (convention 15).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub const OFFSET_ENV: &str = "FAUNA_E2E_CLOCK_OFFSET_SECS";

/// Seconds added to the client's `now`. Zero in every real run.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static CLOCK_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// One-shot guard for the environment seed — native only: `std::env::var` has
/// nothing to read on `wasm32-unknown-unknown`.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(debug_assertions, feature = "e2e-agent")
))]
static ENV_SEEDED: std::sync::Once = std::sync::Once::new();

/// Parse the seed. Whitespace-tolerant; anything that is not a signed integer
/// is `None`, so a stray value falls back to the real clock rather than
/// failing a launch.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn parse_offset(raw: Option<&str>) -> Option<i64> {
    raw?.trim().parse().ok()
}

/// Seed the static from the environment exactly once. Called from both the
/// getter and the setter, so a setter that runs before the first read cannot
/// be overwritten by a later first read's seed.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(debug_assertions, feature = "e2e-agent")
))]
fn seed_from_env() {
    ENV_SEEDED.call_once(|| {
        if let Some(offset) = parse_offset(std::env::var(OFFSET_ENV).ok().as_deref()) {
            CLOCK_OFFSET_SECS.store(offset, Ordering::SeqCst);
        }
    });
}

/// The client clock's offset, in seconds — the environment seed or the last
/// [`set_clock_offset_secs`], zero in every real run and in every release
/// build.
pub fn clock_offset_secs() -> i64 {
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    {
        #[cfg(not(target_arch = "wasm32"))]
        seed_from_env();
        CLOCK_OFFSET_SECS.load(Ordering::SeqCst)
    }
    #[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
    {
        0
    }
}

/// Set the offset above. ⚠ **Process-wide and nothing auto-resets it** — a
/// test that leaves an offset behind skews the next launch, bearer anchor and
/// refresh schedule this process makes. A launch test passes it as the
/// environment seed and tears the process down; a mid-session move zeroes it
/// once its assertions are done.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn set_clock_offset_secs(offset: i64) {
    #[cfg(not(target_arch = "wasm32"))]
    seed_from_env();
    CLOCK_OFFSET_SECS.store(offset, Ordering::SeqCst);
}

/// Epoch **milliseconds** on the client clock — the real clock (or `0` when it
/// cannot be read, [`Timestamp::now_millis_or_zero`]) plus the offset.
pub fn now_millis_or_zero() -> u64 {
    Timestamp::now_millis_or_zero().saturating_add_signed(clock_offset_secs().saturating_mul(1_000))
}

/// Epoch **seconds** on the client clock, same `0`-fold as
/// [`now_millis_or_zero`].
pub fn now_secs_or_zero() -> i64 {
    Timestamp::now_secs_or_zero().saturating_add(clock_offset_secs())
}

/// Epoch seconds on the client clock, **`None` when it cannot be read** (a
/// real clock before the epoch, or an offset pulling it there) — the form a
/// bearer holder needs: [`crate::auth::deadline_on_own_clock`] keeps the
/// nest's deadline on `None`, and a cache treats `None` as "never proved
/// fresh" rather than folding it to epoch-0, which would read as maximally
/// fresh.
pub fn now_secs() -> Option<u64> {
    let real = Timestamp::now_secs_or_zero();
    if real <= 0 {
        return None;
    }
    u64::try_from(real.saturating_add(clock_offset_secs()))
        .ok()
        .filter(|&now| now > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_offset_accepts_signed_seconds_and_refuses_the_rest() {
        assert_eq!(parse_offset(Some("-21600")), Some(-21_600));
        assert_eq!(parse_offset(Some(" 7200 ")), Some(7_200));
        assert_eq!(parse_offset(Some("0")), Some(0));
        assert_eq!(parse_offset(Some("")), None);
        assert_eq!(parse_offset(Some("six hours")), None);
        assert_eq!(parse_offset(Some("1.5")), None);
        assert_eq!(parse_offset(None), None);
    }

    /// One test, not several: the offset is process-global, so separate test
    /// functions racing under the default thread pool would make the verdict
    /// depend on scheduling. No other test in this crate reads this clock; it
    /// is still reset before the test returns.
    #[test]
    fn the_offset_moves_every_accessor_together_and_resets_cleanly() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                set_clock_offset_secs(0);
            }
        }
        let _reset = Reset;

        let real_secs = Timestamp::now_secs_or_zero();
        set_clock_offset_secs(-18_000);
        assert_eq!(clock_offset_secs(), -18_000);
        let skewed_secs = now_secs_or_zero();
        let skewed_millis = now_millis_or_zero();
        let skewed_opt = now_secs().expect("five hours behind is still after the epoch");
        // Five hours behind, to within the few seconds a slow box can take
        // between the reads — hours apart from "not skewed at all".
        let behind_by = real_secs - skewed_secs;
        assert!(
            (17_990..=18_010).contains(&behind_by),
            "expected ~18000 s behind, measured {behind_by}"
        );
        assert!(
            (skewed_millis / 1_000).abs_diff(skewed_secs as u64) <= 10,
            "the millisecond accessor must read the same skewed instant"
        );
        assert!(
            skewed_opt.abs_diff(skewed_secs as u64) <= 10,
            "the Option accessor must read the same skewed instant"
        );

        // An offset pulling the clock before the epoch is an unreadable clock,
        // never a folded epoch-0.
        set_clock_offset_secs(-real_secs - 10_000);
        assert_eq!(now_secs(), None);

        set_clock_offset_secs(0);
        assert_eq!(clock_offset_secs(), 0);
        assert!(
            (Timestamp::now_secs_or_zero() - now_secs_or_zero()).abs() <= 10,
            "reset must return the client clock to the real one"
        );
    }
}

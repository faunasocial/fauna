//! A call over web's account port that arrives before the tab's runtime
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*, decision (j)).
//!
//! Web starts its runtime only once the conversations manager exists, so a
//! page reloaded on a store-backed route reads before the handle does. A
//! native caller of the same door waits for its seat's runtime
//! ([`fauna_account_plane::account_driver::wait_for_account_handle`]); the
//! port's caller waits the same bound, but only while the SPA says a runtime
//! is on its way for the port's account. Native seats have one runtime per
//! signed-in account, so absence there is a wait with no alternative; a tab
//! can know none is coming (a second tab of the account, a page machine that
//! outlived a switch, a start that failed), and it then refuses at once
//! instead of holding its page for the whole bound.

use core::time::Duration;

use fauna_account_plane::account_driver::ACCOUNT_HANDLE_WAIT;

/// How often a call re-reads the handle while a runtime is on its way.
const PORT_WAIT_POLL: Duration = Duration::from_millis(250);

/// How many polls make [`ACCOUNT_HANDLE_WAIT`], the bound a native reader of
/// the same door waits.
const PORT_WAIT_POLLS: u32 = (ACCOUNT_HANDLE_WAIT.as_millis() / PORT_WAIT_POLL.as_millis()) as u32;

/// `resolve`'s handle, waiting while `coming` holds — polled every
/// [`PORT_WAIT_POLL`], for at most [`ACCOUNT_HANDLE_WAIT`]. `None` at once
/// when no handle is up and none is coming. The answer is only a wait: the
/// caller's seams read the handle fresh, and their refusal stays theirs.
pub async fn wait_for_coming_handle<H>(
    resolve: impl Fn() -> Option<H>,
    coming: impl Fn() -> bool,
) -> Option<H> {
    wait_within(PORT_WAIT_POLLS, resolve, coming).await
}

async fn wait_within<H>(
    polls: u32,
    resolve: impl Fn() -> Option<H>,
    coming: impl Fn() -> bool,
) -> Option<H> {
    if let Some(handle) = resolve() {
        return Some(handle);
    }
    if !coming() {
        return None;
    }
    tracing::info!("account port: a call is waiting for the tab's runtime to start");
    for poll in 1..=polls {
        fauna_sleep::sleep(PORT_WAIT_POLL).await;
        if let Some(handle) = resolve() {
            tracing::info!(
                "account port: the waiting call found the runtime after {:?}",
                PORT_WAIT_POLL * poll
            );
            return Some(handle);
        }
        if !coming() {
            tracing::info!("account port: no runtime is coming any more — not waiting");
            return None;
        }
    }
    tracing::warn!(
        "account port: no runtime after {:?}",
        PORT_WAIT_POLL * polls
    );
    None
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// The time a wait took, read off its reads rather than a clock: the
    /// loop reads the handle once on entry and once after each poll's sleep,
    /// so `n` reads are `n - 1` polls. (The seams crate names no tokio timer
    /// or monotonic clock, its tests included — `tests/no_native_time.rs`.)
    fn waited(reads: &Cell<u32>) -> Duration {
        PORT_WAIT_POLL * reads.get().saturating_sub(1)
    }

    /// A handle that is never up, counting its reads.
    fn never(reads: &Cell<u32>) -> impl Fn() -> Option<u8> + '_ {
        move || {
            reads.set(reads.get() + 1);
            None
        }
    }

    #[tokio::test]
    async fn a_running_runtime_answers_without_asking_whether_one_is_coming() {
        let answer = wait_within(1, || Some(7), || panic!("nothing to wait for")).await;
        assert_eq!(answer, Some(7));
    }

    #[tokio::test(start_paused = true)]
    async fn no_runtime_coming_refuses_without_waiting() {
        let reads = Cell::new(0);
        let answer = wait_for_coming_handle(never(&reads), || false).await;
        assert_eq!(answer, None);
        assert_eq!(waited(&reads), Duration::ZERO);
    }

    /// The defect's order: the read is first, the runtime second.
    #[tokio::test(start_paused = true)]
    async fn a_call_ahead_of_the_runtime_waits_for_it() {
        let reads = Cell::new(0);
        let resolve = || {
            reads.set(reads.get() + 1);
            (reads.get() > 3).then_some(7)
        };
        assert_eq!(wait_for_coming_handle(resolve, || true).await, Some(7));
    }

    #[tokio::test(start_paused = true)]
    async fn a_start_given_up_ends_the_wait_at_the_next_poll() {
        let asks = Cell::new(0);
        let coming = || {
            asks.set(asks.get() + 1);
            asks.get() < 2
        };
        let reads = Cell::new(0);
        assert_eq!(wait_for_coming_handle(never(&reads), coming).await, None);
        assert_eq!(waited(&reads), PORT_WAIT_POLL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_runtime_that_never_comes_costs_the_native_bound() {
        let reads = Cell::new(0);
        assert_eq!(wait_for_coming_handle(never(&reads), || true).await, None);
        assert_eq!(waited(&reads), ACCOUNT_HANDLE_WAIT);
    }
}

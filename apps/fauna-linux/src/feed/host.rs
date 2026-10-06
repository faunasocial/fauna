//! The process-wide `fauna_feed::FeedManager` for the Linux Feed page.
//!
//! Mirrors `crate::conversations::host`, but `FeedManager::new(nest, secret)`
//! needs the authed transport + the local actor's Ed25519 secret seed, so the
//! manager is **built on auth** ([`init`]) rather than a no-arg `OnceLock`
//! singleton, and is **swappable** — a sign-out → sign-in with a different
//! identity rebuilds it against the new connection. [`manager`] returns `None`
//! before the first `init` (e.g. the E2E state serializer running pre-auth), so
//! callers degrade gracefully instead of panicking.

use std::sync::{Arc, Mutex, OnceLock};

use fauna_client::NestClient;
use fauna_feed::FeedManager;

/// The Linux feed manager type — `FeedManager` over the authed WS-RPC
/// `Arc<NestClient>` (which impls `RpcRequester`), consumed directly with no
/// FFI hop (priority #2: the Rust-native app uses the generic manager).
pub type LinuxFeedManager = FeedManager<Arc<NestClient>>;

fn slot() -> &'static Mutex<Option<Arc<LinuxFeedManager>>> {
    static MANAGER: OnceLock<Mutex<Option<Arc<LinuxFeedManager>>>> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(None))
}

/// Build (or rebuild, on re-auth) the process-wide feed manager over the authed
/// `nest` + the local actor's 32-byte signing secret, and install it as the
/// current instance. Returns the new `Arc` for the caller to attach an observer
/// to. Called once from `build_main_window` after the authed `FaunaClient` is
/// ready.
pub fn init(nest: Arc<NestClient>, secret: [u8; 32]) -> Arc<LinuxFeedManager> {
    let m = Arc::new(FeedManager::new(nest, secret));
    m.set_period_key_store(crate::account_runtime::period_key_store());
    m.set_preference_store(std::sync::Arc::new(crate::account_runtime::handle_source()));
    *slot().lock().unwrap() = Some(m.clone());
    m
}

/// The current feed manager, or `None` before the first [`init`]. The Feed
/// view, the post-interaction/reconnect refresh, and the E2E state serializer
/// all reach the snapshot through this.
pub fn manager() -> Option<Arc<LinuxFeedManager>> {
    slot().lock().unwrap().clone()
}

/// Drop the current manager, so the next [`init`] is the only thing that can
/// hand one out again. The twin of `crate::conversations::manager()
/// .clear_for_test()` on the identity-scoped teardown paths (test-agent reset,
/// and the actor-switch teardown a second `set_state` login performs).
///
/// Without this the slot keeps the OUTGOING actor's manager after a teardown:
/// [`manager`] is a process-wide accessor with no actor key, so the E2E state
/// serializer (`main.rs`'s posts arm) and any other reader between teardown and
/// the next `init` would serialize the previous identity's post list. `init`
/// overwrites the slot on the happy path, so this only closes the window where
/// no authenticated shell is mounted — but that window is exactly where a
/// cross-actor read is a wrong-data bug rather than a missing-data one.
pub fn clear() {
    *slot().lock().unwrap() = None;
}

/// Flush the debounced engagement-cue tail on window close
/// (engagement-cues.md § At rest: put on batch **or on background/close**).
///
/// `bounded` — the quit / sign-out paths, where the process (or the client
/// runtime) is about to go away — blocks the caller until the flush lands or
/// the bound expires; the hide-to-tray path passes `false` (the process stays
/// alive, so fire-and-forget completes on its own). A failed or timed-out
/// flush is deliberately swallowed: the rollup re-marks itself dirty on a put
/// failure, so the next session's debounce retries — there is no UI left to
/// surface it on. See `blocking_flush::run_bounded`'s doc for the worker-thread
/// + scratch-runtime mechanism (the close handler holds no tokio handle).
pub fn flush_cues_on_close(bounded: bool) {
    let Some(manager) = manager() else { return };
    crate::blocking_flush::run_bounded(
        async move {
            let _ = manager.flush_cues().await;
        },
        std::time::Duration::from_millis(2_000),
        std::time::Duration::from_millis(2_500),
        bounded,
    );
}

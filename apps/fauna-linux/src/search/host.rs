//! The process-wide `fauna_client_search::SearchManager` for the Linux Search
//! page.
//!
//! Mirrors `crate::feed::host`: `SearchManager::new(nest)` needs the authed
//! transport, so the manager is **built on auth** ([`init`]) rather than a
//! no-arg `OnceLock` singleton, and is **swappable** — a sign-out → sign-in
//! with a different identity rebuilds it against the new connection.
//! [`manager`] returns `None` before the first `init` (e.g. the E2E state
//! serializer running pre-auth), so callers degrade gracefully instead of
//! panicking.

use std::sync::{Arc, Mutex, OnceLock};

use fauna_client::NestClient;
use fauna_client_search::SearchManager;

/// The Linux search manager type — `SearchManager` over the authed WS-RPC
/// `Arc<NestClient>` (which impls `RpcRequester`), consumed directly with no
/// FFI hop (priority #2: the Rust-native app uses the generic manager;
/// linux is a direct-Rust consumer exactly like tui).
pub type LinuxSearchManager = SearchManager<Arc<NestClient>>;

fn slot() -> &'static Mutex<Option<Arc<LinuxSearchManager>>> {
    static MANAGER: OnceLock<Mutex<Option<Arc<LinuxSearchManager>>>> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(None))
}

/// Build (or rebuild, on re-auth) the process-wide search manager over the
/// authed `nest`, and install it as the current instance. Returns the new
/// `Arc` for the caller to attach an observer to. Called once from
/// `build_main_window` after the authed `FaunaClient` is ready.
pub fn init(nest: Arc<NestClient>) -> Arc<LinuxSearchManager> {
    let m = Arc::new(SearchManager::new(nest));
    *slot().lock().unwrap() = Some(m.clone());
    m
}

/// The current search manager, or `None` before the first [`init`].
pub fn manager() -> Option<Arc<LinuxSearchManager>> {
    slot().lock().unwrap().clone()
}

/// Drop the current manager, so the next [`init`] is the only thing that can
/// hand one out again — the search twin of `crate::feed::host::clear`, for
/// the same actor-switch teardown paths.
pub fn clear() {
    *slot().lock().unwrap() = None;
}

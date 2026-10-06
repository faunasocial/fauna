//! How a host runs the seams' tasks — the one thing this crate cannot decide
//! for itself.
//!
//! The read-position and contact-overlay seams each start a writer task and a
//! watcher task at registration. Natively those run on the tokio runtime the
//! host's store runs on, handed in as a `Handle` because not every registering
//! edge runs inside one (a UniFFI host may complete the (session, store) pair
//! from a synchronous foreign call). On web there is one thread and one
//! executor, the browser's, reached through `wasm_bindgen_futures::spawn_local`.
//! A trait with one method lets `conversation_seams::wire` take either
//! without naming a runtime — the same two-arm shape `fauna_sleep::sleep`
//! gives the seams' one wait.
//!
//! The task type follows the target: `Send` natively (a tokio task crosses
//! threads), not on wasm32 (the SPA's single-threaded process, where the
//! store handle and the manager are `!Send` by construction) — the
//! `fauna_core::MaybeSendSync` two-arm shape, spelled out because a boxed
//! future carries its bounds in its type.

use std::future::Future;
use std::pin::Pin;

/// A seam task, boxed for the host's spawner — `Send` natively, not on wasm32.
#[cfg(not(target_arch = "wasm32"))]
pub type SeamTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// See the native arm.
#[cfg(target_arch = "wasm32")]
pub type SeamTask = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Where a seam's tasks run. Implemented by `tokio::runtime::Handle` natively
/// and by [`LocalSpawner`] on web; a test may implement it over anything that
/// polls.
pub trait TaskSpawner {
    /// Run `task` to completion, detached: the seams end their own tasks
    /// (each ends with what it serves), so nothing is ever joined.
    fn spawn(&self, task: SeamTask);
}

#[cfg(not(target_arch = "wasm32"))]
impl TaskSpawner for tokio::runtime::Handle {
    fn spawn(&self, task: SeamTask) {
        // The join handle is dropped on purpose: a seam task detaches, and a
        // dropped tokio `JoinHandle` never cancels its task.
        drop(tokio::runtime::Handle::spawn(self, task));
    }
}

/// The web spawner — the browser's own executor, one per page.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalSpawner;

#[cfg(target_arch = "wasm32")]
impl TaskSpawner for LocalSpawner {
    fn spawn(&self, task: SeamTask) {
        wasm_bindgen_futures::spawn_local(task);
    }
}

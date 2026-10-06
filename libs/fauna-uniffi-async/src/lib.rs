//! Run every UniFFI async export on a tokio runtime worker, never on the
//! foreign executor's poll thread.
//!
//! A UniFFI async export is polled on whatever thread the foreign language
//! polls it from. On Apple that is a Swift cooperative-pool thread with a
//! 544 KB stack, whatever actor the calling view model is isolated to (the
//! generated `uniffiRustCallAsync` is nonisolated). `async_runtime = "tokio"`
//! only enters a runtime *context* there, so the whole Rust poll chain runs on
//! that small stack. In a debug build a connect chain (a machine's refresh →
//! `NestClient::connect` → the bearer mint → the WS handshake) is deep enough
//! to hit the stack guard: a `SIGBUS` with no panic and no log. A runtime
//! worker has the ~2 MB stack linux and tui have always run this code on.
//!
//! [`export`] is the fix, applied at the export so it covers every leg the
//! export drives, including ones added later
//! (`docs/goal/architecture/apps/native-async-execution.md` § The rule). It
//! replaces `#[uniffi::export(async_runtime = "tokio")]` on an inherent `impl`
//! block or a free function:
//!
//! ```ignore
//! #[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
//! impl MyMachine {
//!     pub fn snapshot(&self) -> Snapshot { .. }          // exported as-is
//!     pub async fn dispatch(&self, a: Action) -> Result<(), E> { .. }
//! }
//! ```
//!
//! Each `async fn` stays a plain, unexported Rust method under its own name
//! and signature — tui, linux, the wasm twin and tests await it inline as
//! before. Next to it the attribute generates the exported twin, under the
//! *same foreign name* (so the Swift / Kotlin / C# API is unchanged), which
//! runs the inline method through [`off_foreign_stack`]. The twin's receiver
//! is `self: Arc<Self>` and its borrowed arguments become owned (`&str` →
//! `String`, `&[T]` → `Vec<T>`, `&T` → `Arc<T>`, `Option<&str>` →
//! `Option<String>`) — the foreign side passes owned values either way, and a
//! spawned task must own what it uses. Synchronous items stay exported
//! unchanged.
//!
//! Every async export in the workspace uses this attribute:
//! `no_async_export_bypasses_the_attribute` (this crate's lib tests, run by the
//! merge-gate check's `workspace-test-check`) fails on a bare
//! `uniffi::export(async_runtime = …)` anywhere under `libs/`, `bins/` or
//! `apps/`, so a new export cannot forget it.

use std::future::Future;

pub use fauna_uniffi_async_macros::export;

/// Run `work` on a tokio runtime worker and await it.
///
/// Must be awaited inside a tokio runtime context — which every
/// `async_runtime = "tokio"` export provides.
///
/// The call contract is the inline one: `work`'s output is returned, a panic
/// in `work` is re-raised on the awaiting thread (so UniFFI reports it exactly
/// as before), and **dropping the returned future aborts `work`** at its next
/// await point, as dropping an inline future would have stopped it. A foreign
/// caller that cancels (a Swift `Task` cancelled with its view, a Kotlin scope
/// torn down) therefore never leaves the work running on behind it — no
/// orphaned task consuming the next event from a shared receiver.
pub async fn off_foreign_stack<T: Send + 'static>(
    work: impl Future<Output = T> + Send + 'static,
) -> T {
    let mut task = AbortOnDrop(tokio::spawn(work));
    match (&mut task.0).await {
        Ok(value) => value,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        // Only a runtime shutting down cancels a task we still await.
        Err(e) => panic!("UniFFI export task cancelled (runtime shutting down): {e}"),
    }
}

/// Aborts the task when the awaiting future is dropped. A no-op once the task
/// has finished.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// The tests export real objects through `export`, whose expansion names this
// crate by its extern path and the crate root's `UniFfiTag`.
#[cfg(test)]
extern crate self as fauna_uniffi_async;
#[cfg(test)]
uniffi::setup_scaffolding!();
#[cfg(test)]
mod tests;

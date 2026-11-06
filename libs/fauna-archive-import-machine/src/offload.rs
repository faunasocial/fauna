//! The run's synchronous archive reads, run off the async executor's thread.
//!
//! The parser reads a zip through [`fauna_archive::ArchiveSource::read_at`],
//! which is synchronous, while a folder-resident archive
//! (`rpc_glue::FolderArchiveSource`, what every resumed run reads) fetches
//! each block over the network, which is async. Bridging the two must never
//! block a thread the runtime needs to drive that very fetch — and on a
//! current-thread runtime, which is what the native apps' UniFFI face runs
//! on, the one such thread is the one the run itself is on (measured
//! 2026-10-28: every resumed windows run skipped each record that reads the
//! archive). So each read the run makes of the archive goes through
//! [`off_runtime`]: tokio's blocking pool runs it, the thread is marked, and
//! a folder source there waits on its fetch with `Handle::block_on` — valid on
//! a blocking thread of either runtime flavor — while the runtime's own
//! thread keeps driving the IO. On wasm there is one thread and no folder
//! source, and the read runs inline.

use crate::machine::ArchiveImportDispatchError;

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    static OFFLOADED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread is running an [`off_runtime`] read — the one place a
/// synchronous read may wait on an async fetch with `Handle::block_on`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn on_offloaded_read() -> bool {
    OFFLOADED.with(|flag| flag.get())
}

/// Clears the mark when the read ends, panic included: blocking-pool threads
/// are reused, and a stale mark would let a later non-offloaded caller on the
/// same thread block where it must not.
#[cfg(not(target_arch = "wasm32"))]
struct Marked;

#[cfg(not(target_arch = "wasm32"))]
impl Marked {
    fn set() -> Self {
        OFFLOADED.with(|flag| flag.set(true));
        Self
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Marked {
    fn drop(&mut self) {
        OFFLOADED.with(|flag| flag.set(false));
    }
}

/// Run `read` — a synchronous read of the archive — on tokio's blocking pool
/// and await it. A read that panics becomes the run's `Archive` error rather
/// than taking the app's runtime down with it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn off_runtime<T, F>(read: F) -> Result<T, ArchiveImportDispatchError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _marked = Marked::set();
        read()
    })
    .await
    .map_err(|e| ArchiveImportDispatchError::Archive(format!("the archive read stopped: {e}")))
}

/// wasm: one thread, and the browser `File` source reads without a fetch of
/// its own to drive — the read runs inline.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn off_runtime<T, F>(read: F) -> Result<T, ArchiveImportDispatchError>
where
    F: FnOnce() -> T,
{
    Ok(read())
}

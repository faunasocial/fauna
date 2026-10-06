//! Background thread that listens for FileStatusChanged events from
//! fauna-sync and updates the ShellCache.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use fauna_ipc::sync::{Event, EventKind};
use fauna_ipc::sync_pipe_client::SyncPipeClient;

use crate::cache::ShellCache;

const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);
const EVICTION_INTERVAL: Duration = Duration::from_secs(60);

pub struct EventListener {
    stop: Arc<AtomicBool>,
    #[allow(dead_code)] // Kept alive to prevent thread from being detached
    handle: Option<thread::JoinHandle<()>>,
}

impl EventListener {
    /// Spawn the event listener thread.
    pub fn spawn(cache: Arc<ShellCache>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();

        let handle = thread::spawn(move || {
            run_listener(cache, stop2);
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Signal the listener to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for EventListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Don't join — the reader may be blocked on a synchronous read
    }
}

fn run_listener(cache: Arc<ShellCache>, stop: Arc<AtomicBool>) {
    let mut last_eviction = std::time::Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // Try to connect
        #[cfg(windows)]
        let client = SyncPipeClient::connect_pipe();
        #[cfg(not(windows))]
        let client: Result<SyncPipeClient, std::io::Error> = Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "named pipes only on Windows",
        ));

        // On `Err` the service isn't running — fall through to the reconnect wait.
        if let Ok(client) = client {
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }

                match client.recv_event() {
                    Ok(event) => apply_event(&cache, event),
                    Err(_) => break, // Pipe disconnected
                }

                if last_eviction.elapsed() >= EVICTION_INTERVAL {
                    cache.evict_old();
                    last_eviction = std::time::Instant::now();
                }
            }
        }

        // Wait before reconnecting
        let deadline = std::time::Instant::now() + RECONNECT_INTERVAL;
        while std::time::Instant::now() < deadline {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_millis(250));
        }
    }
}

/// Apply one pushed event to the `ShellCache`. Only `FileStatusChanged` carries
/// a per-path status, so it is the sole kind that mutates the cache; every other
/// event kind is ignored. Extracted from `run_listener` so the consumer logic is
/// unit-testable without a live pipe/service.
fn apply_event(cache: &ShellCache, event: Event) {
    if let EventKind::FileStatusChanged { path, status } = event.event {
        cache.set(PathBuf::from(&path), status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_ipc::sync::{ConnectionState, FileStatus};
    use std::path::Path;

    #[test]
    fn apply_event_caches_file_status_changed() {
        let cache = ShellCache::new();
        apply_event(
            &cache,
            Event {
                event: EventKind::FileStatusChanged {
                    path: r"C:\Users\a\Fauna\doc.txt".into(),
                    status: FileStatus::Syncing,
                },
            },
        );
        assert_eq!(
            cache.get(Path::new(r"C:\Users\a\Fauna\doc.txt")),
            Some(FileStatus::Syncing),
        );
    }

    #[test]
    fn apply_event_ignores_events_without_a_path_status() {
        let cache = ShellCache::new();
        apply_event(
            &cache,
            Event {
                event: EventKind::ConnectionStateChanged(ConnectionState::Connected),
            },
        );
        assert!(cache.get(Path::new(r"C:\Users\a\Fauna\doc.txt")).is_none());
    }
}

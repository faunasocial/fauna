//! Shared consumer loop for the agent's pushed events (`sync-agent.md`
//! § Control plane split, seam op 3: "local introspection + shell integration
//! (…, pushed events)").
//!
//! The agent broadcasts an [`Event`](crate::sync::Event) frame to every
//! connected client (`unix_transport::serve` on unix, the per-SID named pipe
//! server on windows); this module is the client-side half every desktop
//! control surface runs to consume them — the linux GTK client and fauna-tui
//! link it directly, FaunaKit **and the windows C# app** consume it via the
//! UniFFI wrapper in `fauna-ffi`. The first (and so far only) consumer is the
//! per-file
//! completed-sync desktop notification: [`synced_filename`] filters the event
//! stream down to `FileStatusChanged{Synced}` basenames, and the platform
//! shell turns each into a native notification. Further event consumers
//! belong here too, not in per-client socket loops.
//!
//! Split mirrors `convergence`: the blocking loop + filter are shared; only
//! the notification *surface* (GTK/`UNUserNotificationCenter`) stays
//! platform-side, injected as a callback so the mechanism tests never touch a
//! desktop dependency (testing.md § point 10).

use std::path::Path;
#[cfg(any(unix, windows))]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(any(unix, windows))]
use crate::endpoint::AgentEndpoint;
use crate::sync::{EventKind, FileStatus};
use crate::sync_pipe_client::SyncPipeClient;

/// Whether a pushed [`EventKind`] should surface as a completed-sync
/// notification, and if so, the filename it names (basename only — a
/// notification body has no room for a full path, and the path's prefix is
/// the user's own folder anyway).
pub fn synced_filename(event: &EventKind) -> Option<String> {
    let EventKind::FileStatusChanged { path, status } = event else {
        return None;
    };
    if *status != FileStatus::Synced {
        return None;
    }
    Some(
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone()),
    )
}

/// Block on `client`'s pushed events, calling `on_synced` with the basename of
/// every `FileStatusChanged{Synced}`; every other event kind/status is ignored.
/// Returns when `stop` is set or the socket errors (agent restarted / went
/// away) — either way the caller decides whether to reconnect.
pub fn run_event_loop(
    client: &SyncPipeClient,
    stop: &AtomicBool,
    mut on_synced: impl FnMut(String),
) {
    while !stop.load(Ordering::Relaxed) {
        match client.recv_event() {
            Ok(event) => {
                if let Some(filename) = synced_filename(&event.event) {
                    on_synced(filename);
                }
            }
            Err(_) => return, // socket died; the caller reconnects
        }
    }
}

/// How long a listener with no agent to connect to sleeps before re-probing.
#[cfg(any(unix, windows))]
const CONNECT_RETRY: std::time::Duration = std::time::Duration::from_secs(2);
/// Pause after a dropped connection before reconnecting (agent restart churn).
#[cfg(any(unix, windows))]
const RECONNECT_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

/// Spawn the background thread that subscribes to the agent's pushed events at
/// `socket` for the whole post-auth session. Self-healing: a socket that isn't
/// up yet, or that drops (agent restart), is retried on a short backoff — this
/// needs no explicit resubscribe hook wired to the reachable edge, unlike the
/// provisioning convergence loop. The returned flag stops the thread
/// best-effort: it may be parked in a blocking `recv_event()` and only notices
/// on its next wake (an event or a socket error).
///
/// Connects through [`AgentEndpoint::connect`], so the caller names the agent
/// once and this loop reconnects to that same endpoint for its whole life — the
/// per-user unix socket on macOS/linux, the per-SID named pipe on windows.
/// Re-resolving per attempt would let a mid-session env change split one
/// listener across two agents (`endpoint.rs`, resolve-once contract).
#[cfg(any(unix, windows))]
pub fn spawn_event_listener_at(
    endpoint: AgentEndpoint,
    on_synced: impl Fn(String) + Send + Sync + 'static,
) -> std::io::Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("fauna-sync-events".into())
        .spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                let Ok(client) = endpoint.connect() else {
                    std::thread::sleep(CONNECT_RETRY);
                    continue;
                };
                run_event_loop(&client, &stop_for_thread, &on_synced);
                std::thread::sleep(RECONNECT_PAUSE);
            }
        })?;
    Ok(stop)
}

/// [`spawn_event_listener_at`] on this user's default agent endpoint — the
/// production entry point.
#[cfg(any(unix, windows))]
pub fn spawn_event_listener(
    on_synced: impl Fn(String) + Send + Sync + 'static,
) -> std::io::Result<Arc<AtomicBool>> {
    let endpoint = AgentEndpoint::default_for_user()?;
    spawn_event_listener_at(endpoint, on_synced)
}

/// The pure event filter, which is platform-free and therefore tested on every
/// platform. These lived in the unix-only module below until the windows
/// consumer arrived (`sync-agent.md` § Consumers), which left the filter every
/// windows notification will be built on with no coverage on windows at all.
#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::sync::ConnectionState;

    #[test]
    fn synced_status_yields_the_basename() {
        assert_eq!(
            synced_filename(&EventKind::FileStatusChanged {
                path: "/home/user/Fauna/sub/report.bin".into(),
                status: FileStatus::Synced,
            }),
            Some("report.bin".to_string())
        );
    }

    /// The windows agent pushes native separators, and `Path::file_name` on
    /// windows is the arm that understands them.
    #[test]
    fn a_windows_path_yields_the_basename_too() {
        let synced = synced_filename(&EventKind::FileStatusChanged {
            path: r"C:\Users\user\Fauna\sub\report.bin".into(),
            status: FileStatus::Synced,
        });
        #[cfg(windows)]
        assert_eq!(synced, Some("report.bin".to_string()));
        // On unix a backslash is an ordinary filename character, so the whole
        // string is one component — asserted so the expectation is stated, not
        // silently platform-dependent.
        #[cfg(not(windows))]
        assert_eq!(
            synced,
            Some(r"C:\Users\user\Fauna\sub\report.bin".to_string())
        );
    }

    #[test]
    fn non_synced_status_is_ignored() {
        for status in [
            FileStatus::Syncing,
            FileStatus::CloudOnly,
            FileStatus::Error,
            FileStatus::NotTracked,
        ] {
            assert_eq!(
                synced_filename(&EventKind::FileStatusChanged {
                    path: "/home/user/Fauna/report.bin".into(),
                    status,
                }),
                None,
                "status {status:?} must not notify"
            );
        }
    }

    #[test]
    fn non_file_status_changed_events_are_ignored() {
        assert_eq!(
            synced_filename(&EventKind::ConnectionStateChanged(
                ConnectionState::Connected
            )),
            None
        );
    }
}

/// The listener loop against a real agent socket — unix-only because it serves
/// a fake agent over `unix_transport::serve`. The windows twin of this proof is
/// the FFI-level listener test plus the live e2e suites.
#[cfg(all(test, unix))]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::sync::{Event, Request, Response, ResponseResult};

    /// Records every filename the callback was called with — the injected seam
    /// testing.md § point 10 requires so these tests never touch a desktop
    /// notification surface.
    #[derive(Default, Clone)]
    struct Recording(Arc<Mutex<Vec<String>>>);

    impl Recording {
        fn push(&self, filename: String) {
            self.0.lock().unwrap().push(filename);
        }
        fn snapshot(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    fn serve_fake_agent(
        sock: std::path::PathBuf,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        event_tx: tokio::sync::broadcast::Sender<Event>,
    ) -> tokio::task::JoinHandle<()> {
        let handler = |req: Request| async move {
            Response {
                id: req.id,
                result: ResponseResult::Err("unhandled".into()),
            }
        };
        tokio::spawn(async move {
            crate::unix_transport::serve(&sock, handler, shutdown_rx, event_tx)
                .await
                .unwrap();
        })
    }

    async fn wait_for(mut cond: impl FnMut() -> bool) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// tier_1: a fake event pushed over a REAL unix socket (the same
    /// `unix_transport::serve` the agent runs) must reach the listener loop
    /// and call the injected callback with just the basename.
    #[tokio::test]
    async fn file_status_changed_synced_over_the_real_socket_notifies_with_the_filename() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sync-agent.sock");

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel::<Event>(16);
        let server = serve_fake_agent(sock.clone(), shutdown_rx, event_tx.clone());

        wait_for(|| sock.exists()).await;
        assert!(sock.exists(), "server never bound the socket");

        let recording = Recording::default();
        let recording_for_thread = recording.clone();
        let sock_cli = sock.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let listener = tokio::task::spawn_blocking(move || {
            let client = SyncPipeClient::connect_socket(&sock_cli).unwrap();
            run_event_loop(&client, &stop_for_thread, |filename| {
                recording_for_thread.push(filename)
            });
        });

        // Give the blocking client a moment to be parked in recv_event() before
        // pushing — the broadcast channel has no durable backlog for a late
        // subscriber.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        event_tx
            .send(Event {
                event: EventKind::FileStatusChanged {
                    path: "/home/user/Fauna/sub/report.bin".into(),
                    status: FileStatus::Synced,
                },
            })
            .unwrap();

        wait_for(|| !recording.snapshot().is_empty()).await;
        assert_eq!(recording.snapshot(), ["report.bin"]);

        stop.store(true, Ordering::Relaxed);
        let _ = shutdown_tx.send(true);
        let _ = server.await;
        listener.abort(); // still parked in recv_event(); nothing more is coming
    }

    /// tier_1: the self-healing spawn wrapper connects to a socket that did not
    /// exist when the listener started (agent boots after the app) and still
    /// delivers events — the property the reconnect loop exists for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listener_spawned_before_the_socket_exists_still_receives_events() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sync-agent.sock");

        let recording = Recording::default();
        let recording_for_thread = recording.clone();
        let stop = spawn_event_listener_at(AgentEndpoint::Unix(sock.clone()), move |filename| {
            recording_for_thread.push(filename)
        })
        .unwrap();

        // Bind the socket only after the listener is already retrying.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel::<Event>(16);
        let server = serve_fake_agent(sock.clone(), shutdown_rx, event_tx.clone());
        wait_for(|| sock.exists()).await;

        // The listener re-probes on CONNECT_RETRY (2s); push events until one
        // lands after it has connected and parked in recv_event().
        for _ in 0..200 {
            let _ = event_tx.send(Event {
                event: EventKind::FileStatusChanged {
                    path: "/home/user/Fauna/late.bin".into(),
                    status: FileStatus::Synced,
                },
            });
            if !recording.snapshot().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            recording.snapshot().first().map(String::as_str),
            Some("late.bin")
        );

        stop.store(true, Ordering::Relaxed);
        let _ = shutdown_tx.send(true);
        let _ = server.await;
    }
}

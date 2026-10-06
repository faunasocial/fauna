//! Unix-domain-socket transport for the sync agent's local IPC seam — the
//! non-Windows sibling of the Windows named-pipe transport
//! (`sync_pipe_client::connect_pipe_to` + the agent's `run_pipe_server`).
//!
//! Same length-prefixed canonical dag-cbor frame as everywhere else
//! (`[u32 LE length][canonical dag-cbor payload]`, via [`crate::encode_frame`] /
//! [`crate::decode_payload`]). Per-user isolation is the unix analog of the
//! per-SID pipe DACL: a `0600` socket file inside a `0700` per-user directory, so
//! only the owning user can connect. Socket paths per `sync-agent.md`
//! § Control plane split:
//!   - linux:  `$XDG_RUNTIME_DIR/fauna/sync-agent.sock`
//!   - linux under Flatpak (`FLATPAK_ID` set):
//!     `$XDG_RUNTIME_DIR/app/<app-id>/fauna/sync-agent.sock` — the one runtime
//!     subdir flatpak shares between separate instances of the same app-id
//!     (the sandbox mounts a private tmpfs over the rest of `/run/user/<uid>`),
//!     so the sandboxed GTK app reaches the agent the host systemd user unit
//!     spawned via `flatpak run --command=fauna-sync-agent`
//!     (`installers/linux-desktop.md` § Flatpak)
//!   - macOS:  `~/Library/Application Support/Fauna/sync-agent.sock`
//!
//! The [`SyncPipeClient`](crate::sync_pipe_client::SyncPipeClient) reused as the
//! client keeps the transport symmetric: `connect_socket` clones a blocking
//! `UnixStream` into reader/writer halves and hands them to the same
//! `from_streams` machinery the pipe client uses — no tokio runtime required on
//! the *client* side (a GTK main loop / fauna-tui can call it directly). The
//! *server* here is tokio-based, matching the Windows `run_pipe_server` shape, so
//! it drops into the agent's async run loop unchanged.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;
use tokio::sync::{broadcast, watch};

use crate::sync::{Event, Request, Response};

/// The per-user sync-agent socket path for this platform.
///
/// - **macOS:** `~/Library/Application Support/Fauna/sync-agent.sock`
/// - **linux (+ other unix):** `$XDG_RUNTIME_DIR/fauna/sync-agent.sock`
/// - **linux under Flatpak** (`FLATPAK_ID` set — true for the sandboxed app
///   *and* for the agent the host unit starts via `flatpak run --command=`,
///   so every party derives the same path):
///   `$XDG_RUNTIME_DIR/app/<app-id>/fauna/sync-agent.sock` — see the module
///   docs; the plain `fauna/` subdir would land on each instance's private
///   tmpfs and the two sandboxes would never see each other's socket.
///
/// Snap needs no branch: snapd already rewrites `$XDG_RUNTIME_DIR` itself to
/// the per-snap `/run/user/<uid>/snap.<name>`, which all processes of the
/// snap share — including a future `daemon-scope: user` agent
/// (`installers/linux-desktop.md` § Snap).
///
/// On linux the agent runs in the user's logon session (or under
/// `loginctl enable-linger` on a headless box), where `$XDG_RUNTIME_DIR` is
/// always set; if it is genuinely absent this returns `NotFound` rather than
/// guessing a world-readable location.
pub fn default_socket_path() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        // `dirs::home_dir()` (not `env::var_os("HOME")`): a LaunchAgent
        // bootstrapped from the installer's root session starts with HOME unset,
        // and refusing there means the agent never binds its socket — observed
        // 2026-07-20 on a real `.pkg` install. `dirs` falls back to the passwd
        // database, which launchd always has. Unlike the linux `XDG_RUNTIME_DIR`
        // branch below this is safe to derive rather than demand: the resolved
        // path is the invoking user's own `~/Library/Application Support`, and
        // the socket is created 0600 inside a 0700 dir either way — so this
        // cannot widen the socket's audience the way guessing a runtime dir
        // could.
        Ok(macos_socket_path(dirs::home_dir()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let base = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "XDG_RUNTIME_DIR is not set — the sync agent needs a per-user runtime dir \
                 (a logon session, or `loginctl enable-linger` on a headless box)",
            )
        })?;
        Ok(linux_socket_path(
            Path::new(&base),
            std::env::var_os("FLATPAK_ID").as_deref(),
        ))
    }
}

/// `~/Library/Application Support/Fauna/sync-agent.sock` under `home`.
///
/// **Always ABSOLUTE**, the same guarantee `SyncPaths`'s macOS base (now the
/// user-domain `…/Fauna/sync` beside this socket, 2026-08-25)
/// makes for the sync base dir: `dirs::home_dir()` only filters an
/// EMPTY `$HOME` (falling back to the passwd database, always absolute), not a
/// present-but-relative one — so without this fallback a relative `$HOME` would
/// silently produce a relative socket path, which `UnixListener::bind` resolves
/// against the process cwd. Under launchd that cwd is `/`, read-only — the exact
/// failure mode the base-dir fix closed, left open here until now.
#[cfg(target_os = "macos")]
fn macos_socket_path(home: Option<PathBuf>) -> PathBuf {
    let home = match home {
        Some(h) if h.is_absolute() => h,
        _ => PathBuf::from("/tmp"),
    };
    home.join("Library/Application Support/Fauna/sync-agent.sock")
}

/// `$XDG_RUNTIME_DIR/fauna/sync-agent.sock` under `runtime_dir` — or, when
/// `flatpak_id` names a Flatpak app-id, the instance-shared
/// `$XDG_RUNTIME_DIR/app/<app-id>/fauna/sync-agent.sock` subdir (see
/// [`default_socket_path`]). An empty `FLATPAK_ID` counts as unset, matching
/// the app's `LaunchChannel` detection.
#[cfg(not(target_os = "macos"))]
fn linux_socket_path(runtime_dir: &Path, flatpak_id: Option<&std::ffi::OsStr>) -> PathBuf {
    match flatpak_id.filter(|id| !id.is_empty()) {
        Some(id) => runtime_dir
            .join("app")
            .join(id)
            .join("fauna/sync-agent.sock"),
        None => runtime_dir.join("fauna/sync-agent.sock"),
    }
}

/// RAII single-instance guard for the agent process: an exclusive advisory
/// `flock` on `<socket_path>.lock`, held for the lifetime of this value.
///
/// Why a kernel lock and not a connect-probe: two cold-starting agents can both
/// probe the socket before either binds (nothing there yet → both proceed), and
/// [`serve`]'s unlink-before-bind then lets the second binder silently steal the
/// path from the first — two live agents syncing the same DB as the same device.
/// The flock has no such window: the kernel arbitrates, exactly one acquirer
/// wins, and the loser exits before touching credentials or engines. A crashed
/// holder releases automatically (the lock dies with the process), so there is
/// no stale-lock state to reconcile at boot.
///
/// The lock file itself is deliberately **never deleted** — unlinking a lock
/// file re-opens the race it exists to close (a new acquirer can lock the old
/// inode while another creates a fresh file at the same path). It is a zero-byte
/// `0600` file next to the socket; [`serve`]'s socket cleanup leaves it alone.
///
/// **Why this keeps its own copy of the create+chmod prelude** (ruled
/// 2026-08-15, after the question sat open since 2026-08-13 — do NOT re-open it
/// as an oversight). Every other file lock in the tree mints its handle through
/// `fauna_core::fs_lock::open_lock_file`, which carries exactly the three
/// invariants above. This crate does not, because `fauna-ipc` has **no**
/// dependency on `fauna-core` and adding one to share six lines of `std` would
/// drag `fauna-core`'s whole cryptographic graph — argon2, the dalek pair,
/// aes-gcm/chacha20poly1305, blake3, zstd, fastcdc, a post-quantum KEM — into
/// the deliberately lean Windows shell extension, which links this crate (see
/// this crate's `Cargo.toml`, where even tokio is `cfg`-gated for that reason).
/// A leaf crate holding just the mint was the alternative and loses too: it
/// would move the primitive away from its four in-tree consumers to serve a
/// fifth with six lines. What the copy actually risks is silent drift, and that
/// is closed by a cheaper, stronger mechanism than a shared symbol — the
/// invariants are pinned by `instance_lock_file_is_owner_only_and_never_\
/// truncated_or_deleted` below, red-verified in both directions.
///
/// **Revisit when, and only when, a SECOND `fauna-core`-free crate needs the
/// mint** — at two consumers the leaf crate starts paying for itself.
///
/// The agent acquires this at startup, **before** restoring its capability or
/// starting engines (`bins/fauna-sync-agent/src/service.rs`); the windows named
/// pipe has its own single-instance story (per-SID pipe + SCM) and does not use
/// this.
#[derive(Debug)]
pub struct InstanceLock {
    /// Held (and thereby locked) until drop; never read.
    _file: std::fs::File,
}

impl InstanceLock {
    /// Try to become the single agent instance for `socket_path`.
    ///
    /// Returns `AddrInUse` when another live process holds the lock — the caller
    /// should log and exit cleanly (another agent is already serving this user).
    /// Any other error is a real filesystem failure.
    pub fn acquire(socket_path: &Path) -> io::Result<Self> {
        prepare_socket_dir(socket_path)?;
        let lock_path = instance_lock_path(socket_path);
        // A zero-content lock file, not secret-bearing — same class as
        // `fauna_core::fs_lock`'s mint (this crate cannot depend on
        // `fauna-core` without pulling its whole cryptographic graph into
        // this deliberately lean Windows shell extension, hence the
        // standalone copy). Reviewed and cleared, no rewrite needed.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        set_mode(&lock_path, 0o600)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "another sync-agent instance holds {} — exiting as duplicate",
                    lock_path.display()
                ),
            )),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

/// `<socket_path>.lock` — the instance-lock file beside the socket.
fn instance_lock_path(socket_path: &Path) -> PathBuf {
    let mut os = socket_path.as_os_str().to_os_string();
    os.push(".lock");
    PathBuf::from(os)
}

/// Bind a per-user unix socket at `socket_path` and serve IPC requests until
/// `shutdown` flips to `true` (or its sender drops).
///
/// Each accepted connection reads length-prefixed [`Request`] frames, passes each
/// to `handler`, and writes the [`Response`] frame back; server-pushed [`Event`]s
/// from `events` are forwarded to every connected client. `handler` is the seam
/// that keeps this crate free of the agent's state type — the agent passes a
/// closure that calls its own `handle_request(state, req)`.
///
/// The parent directory is created `0700` and the socket file `0600` (the unix
/// analog of the per-SID pipe DACL). A stale socket from a previous run is removed
/// before bind, and the socket is removed again on clean shutdown.
pub async fn serve<H, F>(
    socket_path: &Path,
    handler: H,
    mut shutdown: watch::Receiver<bool>,
    events: broadcast::Sender<Event>,
) -> io::Result<()>
where
    H: Fn(Request) -> F + Clone + Send + 'static,
    F: Future<Output = Response> + Send + 'static,
{
    prepare_socket_dir(socket_path)?;
    // A leftover socket file from a prior run makes bind fail with EADDRINUSE.
    // Removing it is safe from duplicate-agent steals only because the agent
    // serializes binders via [`InstanceLock`] before ever reaching here.
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    restrict_socket_perms(socket_path)?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                tokio::spawn(crate::frame_io::handle_conn(stream, handler.clone(), events.subscribe()));
            }
            changed = shutdown.changed() => {
                // Sender dropped (Err) or flipped to true → stop accepting.
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    // Best-effort cleanup so the next run binds cleanly.
    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

/// Usable bytes in `sockaddr_un.sun_path`, which the kernel NUL-terminates:
/// macOS's `sun_path` is 104 bytes, linux's 108. This is a hard ABI limit — a
/// longer path cannot be bound *or dialed*, no matter that it exists on disk.
#[cfg(target_os = "macos")]
const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
const SUN_PATH_MAX: usize = 108;

/// Reject a socket path too long for `sun_path` **before** bind, naming the path,
/// its length and the limit.
///
/// The raw kernel error is `EINVAL` rendered as the bare `path must be shorter
/// than SUN_LEN` — it names neither the path nor the budget, and it surfaces on
/// the *client* side as `agent unreachable for <op>`, which reads like a missing
/// agent rather than an unbindable one. That cost the 2026-07-24 macOS multiseat
/// seat a full round: an isolated-`HOME` e2e launch under macOS's ~48-byte
/// `/var/folders/…/T/` tmp root derives a 134-byte socket path, so the agent
/// spawned, bound nothing, and exited — while the app reported only "agent
/// unreachable". The path is derived from the home dir
/// ([`macos_socket_path`]) plus a fixed 50-byte
/// `Library/Application Support/Fauna/sync-agent.sock` suffix, so any caller
/// relocating `HOME` (every e2e launch, a sandbox container) can overshoot.
fn check_socket_path_len(socket_path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let len = socket_path.as_os_str().as_bytes().len();
    if len >= SUN_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sync-agent socket path is {len} bytes, over this platform's \
                 sun_path limit of {} usable ({}): {}. The path is derived from \
                 the home/runtime dir — relocate it somewhere shorter.",
                SUN_PATH_MAX - 1,
                SUN_PATH_MAX,
                socket_path.display()
            ),
        ));
    }
    Ok(())
}

fn prepare_socket_dir(socket_path: &Path) -> io::Result<()> {
    check_socket_path_len(socket_path)?;
    if let Some(dir) = socket_path.parent() {
        std::fs::create_dir_all(dir)?;
        set_mode(dir, 0o700)?;
    }
    Ok(())
}

/// A unix socket's mode cannot be set at `bind(2)` time, so this necessarily
/// runs after the bind — but `prepare_socket_dir` already set the PARENT
/// directory to `0o700` before the bind, so no other principal can traverse
/// into it and reach the socket during the window before this call lands.
/// Correct as-is and unfixable at the socket-mode layer; the parent
/// directory is the actual guard. Reviewed and cleared, no rewrite
/// needed.
fn restrict_socket_perms(socket_path: &Path) -> io::Result<()> {
    set_mode(socket_path, 0o600)
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{RequestMethod, ResponsePayload, ResponseResult, SyncStatusInfo};
    use crate::sync_pipe_client::SyncPipeClient;
    use std::time::Duration;

    /// The per-platform socket path shape, asserted on a fake base — pure path
    /// formatting, no env mutation, no filesystem.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_socket_path_is_under_application_support() {
        assert_eq!(
            macos_socket_path(Some(PathBuf::from("/Users/tester"))),
            PathBuf::from("/Users/tester/Library/Application Support/Fauna/sync-agent.sock")
        );
    }

    /// Twin of `SyncPaths::macos_group_container_base_is_always_absolute`
    /// (`bins/fauna-sync-agent/src/config.rs`) for the socket path: a
    /// missing/empty/relative home must never produce a relative socket path —
    /// `UnixListener::bind` resolves a relative path against the process cwd,
    /// which under launchd is `/`, read-only.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_socket_path_is_always_absolute() {
        for home in [
            None,
            Some(PathBuf::from("")),
            Some(PathBuf::from("relative/home")),
        ] {
            let path = macos_socket_path(home.clone());
            assert!(
                path.is_absolute(),
                "home {home:?} produced relative {path:?}"
            );
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_socket_path_is_under_xdg_runtime_dir() {
        assert_eq!(
            linux_socket_path(Path::new("/run/user/1000"), None),
            PathBuf::from("/run/user/1000/fauna/sync-agent.sock")
        );
    }

    /// Under Flatpak the socket must land in the one runtime subdir the sandbox
    /// shares between instances of the same app-id — the plain `fauna/` subdir
    /// sits on each instance's private tmpfs, so the app and the host-unit
    /// -spawned agent would never see each other's socket. The instance lock
    /// rides the socket path (`<socket>.lock`), so relocation moves the
    /// single-instance guard into the shared subdir with it.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_socket_path_relocates_into_shared_app_subdir_under_flatpak() {
        let flatpak = linux_socket_path(
            Path::new("/run/user/1000"),
            Some(std::ffi::OsStr::new("social.fauna.fauna")),
        );
        assert_eq!(
            flatpak,
            PathBuf::from("/run/user/1000/app/social.fauna.fauna/fauna/sync-agent.sock")
        );
        assert_eq!(
            instance_lock_path(&flatpak),
            PathBuf::from("/run/user/1000/app/social.fauna.fauna/fauna/sync-agent.sock.lock"),
            "the InstanceLock must guard the relocated socket, not the native path"
        );
        // Empty FLATPAK_ID counts as unset — same rule as LaunchChannel::from_env.
        assert_eq!(
            linux_socket_path(Path::new("/run/user/1000"), Some(std::ffi::OsStr::new(""))),
            PathBuf::from("/run/user/1000/fauna/sync-agent.sock")
        );
    }

    /// tier_1: a socket path over `sun_path` is rejected before bind, with an
    /// error naming the path, its length and the budget.
    ///
    /// Regression gate for the 2026-07-24 macOS multiseat seat: the agent
    /// spawned, tried to bind a 134-byte path under an isolated-`HOME` e2e tmp
    /// root, and died with the bare kernel `path must be shorter than SUN_LEN`
    /// while the app reported only "agent unreachable" — a failure that named
    /// neither the path nor the limit.
    #[test]
    fn socket_path_over_sun_path_limit_is_rejected_with_an_actionable_error() {
        let long = PathBuf::from(format!("/tmp/{}/sync-agent.sock", "d".repeat(SUN_PATH_MAX)));
        let err = check_socket_path_len(&long).expect_err("over-long path must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let msg = err.to_string();
        for needle in [
            long.display().to_string(),
            SUN_PATH_MAX.to_string(),
            "sun_path".to_string(),
        ] {
            assert!(msg.contains(&needle), "{needle:?} missing from {msg:?}");
        }
    }

    /// The guard fires *before* any filesystem side effect, so a doomed path
    /// leaves no half-made 0700 dir behind for the next run to trip over.
    #[test]
    fn prepare_socket_dir_rejects_an_over_long_path_before_creating_it() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("x".repeat(SUN_PATH_MAX));
        let sock = deep.join("sync-agent.sock");
        assert!(prepare_socket_dir(&sock).is_err());
        assert!(!deep.exists(), "the doomed socket dir must not be created");
    }

    /// The real per-platform default shape must clear the gate with room to
    /// spare — the guard exists to catch relocated homes, never to reject a
    /// production path.
    #[test]
    fn the_default_socket_path_shape_clears_the_length_gate() {
        #[cfg(target_os = "macos")]
        let path = macos_socket_path(Some(PathBuf::from("/Users/tester")));
        #[cfg(not(target_os = "macos"))]
        let path = linux_socket_path(Path::new("/run/user/1000"), None);
        check_socket_path_len(&path).expect("the production socket path must fit sun_path");
    }

    /// tier_1: the single-instance guard. A second acquirer must fail while the
    /// first holds the lock — kernel-arbitrated, so two cold-starting agents can
    /// never both pass (a connect-probe can: both probe before either binds) —
    /// and must succeed once the first releases (drop == process exit, so a
    /// crashed agent never leaves a stale lock).
    #[test]
    fn instance_lock_excludes_second_acquirer_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sync-agent.sock");

        let first = InstanceLock::acquire(&sock).expect("first acquire must succeed");
        let second = InstanceLock::acquire(&sock);
        let err = second.expect_err("second acquire must fail while the first instance is live");
        assert_eq!(
            err.kind(),
            io::ErrorKind::AddrInUse,
            "duplicate → AddrInUse: {err}"
        );

        drop(first);
        InstanceLock::acquire(&sock)
            .expect("acquire after release must succeed (crash leaves no stale lock)");
    }

    /// tier_1: the lock FILE's own three invariants — never truncate, owner-only,
    /// never delete.
    ///
    /// These are the invariants `fauna_core::fs_lock::open_lock_file` carries
    /// for every other lock in the tree; this crate deliberately keeps its own
    /// copy of the six-line prelude rather than take a dependency on
    /// `fauna-core` (see [`InstanceLock`]'s doc comment for why). A copy that
    /// agrees only by doc comment is a copy free to drift silently, so the
    /// agreement is pinned by execution instead. The first two assertions
    /// red-verify against a broken [`InstanceLock::acquire`] (`truncate(true)`;
    /// mode `0o644`); the third guards the mistake nobody has made yet — an
    /// unlink-on-drop, which no code path performs today.
    #[test]
    fn instance_lock_file_is_owner_only_and_never_truncated_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sync-agent.sock");
        let lock_path = instance_lock_path(&sock);

        // A pre-existing lock file with content — whatever wrote it, acquiring
        // must not write to state another process may be holding.
        prepare_socket_dir(&sock).unwrap();
        std::fs::write(&lock_path, b"x").unwrap();

        let held = InstanceLock::acquire(&sock).expect("acquire must succeed");
        assert_eq!(
            std::fs::read(&lock_path).unwrap(),
            b"x",
            "acquiring must never truncate the lock file"
        );

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&lock_path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the lock file must be owner-only, like the socket beside it"
        );

        drop(held);
        assert!(
            lock_path.exists(),
            "releasing must never delete the lock file — unlinking re-opens the \
             race the lock exists to close (a new acquirer locks the orphaned \
             inode while another creates a fresh file at the same path)"
        );
    }

    /// tier_1: bind the real agent socket, connect the real blocking client, and
    /// prove a request→response round-trips over the unix transport with the exact
    /// dag-cbor framing production uses. The 0700-dir / 0600-socket perms are
    /// asserted too — the unix analog of the per-SID pipe DACL.
    #[tokio::test]
    async fn unix_socket_request_response_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sync-agent.sock");

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (event_tx, _) = broadcast::channel::<Event>(16);

        // A minimal handler: answer GetSyncStatus, error on anything else.
        let handler = |req: Request| async move {
            match req.method {
                RequestMethod::GetSyncStatus => Response {
                    id: req.id,
                    result: ResponseResult::Ok(ResponsePayload::SyncStatus(SyncStatusInfo {
                        connected: true,
                        syncing: false,
                        files_pending: 0,
                        bytes_pending: 0,
                        last_sync: None,
                    })),
                },
                _ => Response {
                    id: req.id,
                    result: ResponseResult::Err("unhandled".into()),
                },
            }
        };

        let sock_srv = sock.clone();
        let server = tokio::spawn(async move {
            serve(&sock_srv, handler, shutdown_rx, event_tx)
                .await
                .unwrap();
        });

        // Wait for the socket to appear (bind happens inside serve).
        for _ in 0..200 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(sock.exists(), "server never bound the socket");

        // Perms: 0700 dir, 0600 socket.
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "socket dir must be 0700");
        assert_eq!(sock_mode, 0o600, "socket file must be 0600");

        // The blocking client runs on its own thread (no tokio runtime needed).
        let sock_cli = sock.clone();
        let resp = tokio::task::spawn_blocking(move || {
            let client = SyncPipeClient::connect_socket(&sock_cli).unwrap();
            client
                .request_with_timeout(RequestMethod::GetSyncStatus, Duration::from_secs(2))
                .unwrap()
        })
        .await
        .unwrap();

        match resp.result {
            ResponseResult::Ok(ResponsePayload::SyncStatus(s)) => assert!(s.connected),
            other => panic!("unexpected response: {other:?}"),
        }

        let _ = shutdown_tx.send(true);
        let _ = server.await;
    }
}

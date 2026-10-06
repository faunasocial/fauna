//! macOS nest service shell — the `social.fauna.nest` `LaunchDaemon` (runs as the
//! dedicated `_fauna` service user).
//!
//! This is the macOS analogue of the Windows `fauna-nest-service` SCM shell
//! (`apps/fauna-windows/fauna-nest-service`): it resolves the **system** data-dir
//! layout + the serving-port seed, then runs the nest **in-process** via the
//! shared cross-OS serve-and-restart loop
//! [`fauna_nest::desktop_serve::run_serve_loop`] — the *same* loop the Windows
//! shell drives (priority #2/#3: one nest-construction + serve/restart sequence,
//! not two). The per-OS *shell* differs (this `LaunchDaemon` `main` vs. the
//! Windows SCM `run_as_service`); the loop body is shared.
//!
//! # Runtime model — in-process, mirroring Windows `fauna-nest-service`
//!
//! The macOS daemon runs the nest **in-process** (calls the shared loop, which
//! calls `fauna_nest::start_server`), exactly as the Windows `run_service_loop`
//! does — NOT a child-spawn shell like the MDA `fauna-bridge-supervisor`.
//! Rationale: priority #3 (same concept as Windows), it reuses the proven
//! serve-loop-re-enter pattern (one cert-watcher around the loop, edge-triggered
//! restart via the shared serving-port policy), and it is cleaner for **launchd
//! socket activation** (the daemon itself receives the pre-bound `:443` fd rather
//! than forwarding it to a child). The MDA supervisor child-spawns only because
//! the MDA is a separate Go binary; the Rust nest has no such constraint.
//!
//! # launchd socket activation for `:443` (slice S1 — daemon side wired)
//!
//! A `LaunchDaemon` under the non-root `_fauna` user cannot bind the privileged
//! `:443` itself, so root launchd pre-binds it and hands the daemon the listening
//! fd. This daemon now **inherits that fd** via [`macos::activate_nest_listener`]
//! (`launch_activate_socket("FaunaNest")`) and serves on it directly through the
//! shared loop's pre-bound-listener seam (`fauna_nest::start_server`'s
//! `external_listener`). Off launchd — a foreground / dev run, or a plist with no
//! `Sockets` dict — the call reports no socket and the loop binds the direct
//! `0.0.0.0:3000` seed (the as-built network-reachable path).
//!
//! # What remains (the packaging slices)
//!
//! The `.pkg` postinstall that creates `_fauna` via `dscl`, drops the
//! `/Library/LaunchDaemons/social.fauna.nest.plist` (with `UserName` +
//! `FAUNA_DATA_DIR` + the `Sockets` dict naming `FaunaNest` → `:443` — slice S3).
//! The **live** launchd fd
//! inheritance remains untested (sudo + a real `LaunchDaemon`); the pure
//! activation decision is unit-tested (design tracked internally).

#[cfg(target_os = "macos")]
mod macos {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};

    use fauna_nest::desktop_serve::ServeLoopConfig;

    /// The fixed internal-loopback port the macOS nest must **also** bind for
    /// co-located IPC (the MDA bridge + the same-box desktop app), set via the
    /// `FAUNA_INTERNAL_LOOPBACK_PORT` env (by the shared loop) so it survives an
    /// admin `serving_port` change (`nest/common.md` § Same-box reach). =
    /// `fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT` = 3000.
    /// Kept as a local literal — the same duplication-with-citation pattern the
    /// shared supervisor crate uses for the nest flag consts and
    /// `fauna-bridge-supervisor` uses for its loopback endpoint — rather than
    /// depending on `fauna-protocol` from a service shell.
    const LOOPBACK_PORT: u16 = 3000;

    /// The macOS desktop external serving-port **seed** the admin's `serving_port`
    /// choice overrides on restart (the canonical `0.0.0.0:3000` direct-listener
    /// bind — covers loopback + LAN). Distinct from the Windows seed (443): a
    /// `LaunchDaemon` under the non-root `_fauna` user is unprivileged and cannot
    /// bind `:443` directly; the machine-daemon's `:443` comes from launchd
    /// **socket activation**, not this seed. This is the `default_port`
    /// the shared reconcile policy resolves against (parameterized precisely
    /// because it differs Windows-443 vs macOS-3000).
    const DEFAULT_SERVING_PORT: u16 = 3000;

    /// The macOS env var the nest reads to relocate its whole data layout to the
    /// artifact-set system data-root (bucket-2 IPC — see
    /// `bins/fauna-nest/src/main.rs::data_dir_db_path_override`). Local literal,
    /// duplicated-with-citation (the cross-binary IPC-env pattern).
    const FAUNA_DATA_DIR_ENV: &str = "FAUNA_DATA_DIR";

    /// The resolved launch configuration for the macOS nest daemon: where the
    /// nest's data lives + the two serving-port parameters the shared serve loop
    /// needs. Constructed by [`daemon_for`] / [`resolve`]; [`NestDaemon::serve_config`]
    /// turns it into the [`ServeLoopConfig`] the shared loop consumes.
    #[derive(Debug, PartialEq, Eq)]
    pub struct NestDaemon {
        /// The nest's data dir — the system data-root `/Library/Application
        /// Support/Fauna` (owned by `_fauna`) on the machine-daemon, or
        /// `~/Library/Application Support/Fauna` on the as-built per-user install.
        /// The nest writes `nest.db` + the serving-port flag here; passed straight
        /// to the shared loop (which resolves the layout from it).
        pub data_dir: PathBuf,
        /// The fixed co-located-IPC loopback port ([`LOOPBACK_PORT`]).
        pub loopback_port: u16,
        /// The external serving-port seed the admin's choice overrides
        /// ([`DEFAULT_SERVING_PORT`]).
        pub default_serving_port: u16,
    }

    impl NestDaemon {
        /// The external client-facing bind **seed**: `0.0.0.0:<seed>` — all
        /// interfaces, no distinction by client origin (the network-reachable
        /// nest, `installers/macos.md` § Network-reachable nest). `start_server`
        /// boot-resolves the admin `serving_port` singleton over this seed's port;
        /// launchd socket activation will instead supply the pre-bound `:443` fd
        /// to the `_fauna` daemon.
        pub fn bind_seed(&self) -> SocketAddr {
            SocketAddr::from(([0, 0, 0, 0], self.default_serving_port))
        }

        /// The [`ServeLoopConfig`] the shared cross-OS serve loop consumes. Asks
        /// for the fixed co-located loopback listener (so the MDA bridge + same-box
        /// app dial a port that never moves on a serving-port change). The co-located
        /// MDA worker-authenticates by **service-user enrollment** — presenting its
        /// own Ed25519 keypair (`<bridge>/keys/mda.key`, dialing the
        /// `--nest-endpoint https://127.0.0.1:3000` that `libs/fauna-mda-supervisor`
        /// hands it) so the nest resolves its `BridgeMda` role from the enrollment
        /// row (the cross-OS path Linux/Docker already share — no `device.toml`, no
        /// shared secret). The optional sidecar **storage worker** for a *public*
        /// nest (`nest/worker.md`) is an unrelated subsystem a personal desktop nest
        /// never runs.
        pub fn serve_config(&self) -> ServeLoopConfig {
            ServeLoopConfig {
                bind: self.bind_seed(),
                data_dir: self.data_dir.clone(),
                default_serving_port: self.default_serving_port,
                internal_loopback_port: Some(self.loopback_port),
            }
        }
    }

    /// Construct a [`NestDaemon`] from an explicit data dir (pure — no env /
    /// filesystem reads, so it is unit-testable).
    pub fn daemon_for(data_dir: &Path) -> NestDaemon {
        NestDaemon {
            data_dir: data_dir.to_path_buf(),
            loopback_port: LOOPBACK_PORT,
            default_serving_port: DEFAULT_SERVING_PORT,
        }
    }

    /// The system data-root from `FAUNA_DATA_DIR`, or `None` to fall back to the
    /// per-user home-derived path. Mirrors `fauna-bridge-supervisor`'s
    /// `data_dir_support_override` (the bridge + nest daemons share the same env +
    /// data-root); a whitespace-only / empty value is treated as unset.
    fn data_dir_override(raw: Option<&str>) -> Option<PathBuf> {
        fauna_deployment_flags::parse_dir_override(raw)
    }

    /// The per-user fallback data dir: `<home>/Library/Application Support/Fauna`
    /// (the foreground / dev-run location; the postinstall always sets
    /// `FAUNA_DATA_DIR`).
    fn home_data_dir(home: &Path) -> PathBuf {
        home.join("Library")
            .join("Application Support")
            .join("Fauna")
    }

    /// Resolve a production [`NestDaemon`]: the system data-root from
    /// `FAUNA_DATA_DIR` when set (the machine-service `_fauna` daemon), else the
    /// per-user home-derived path (a foreground / dev run).
    pub fn resolve() -> Result<NestDaemon, String> {
        let data_dir = match data_dir_override(std::env::var(FAUNA_DATA_DIR_ENV).ok().as_deref()) {
            Some(d) => d,
            None => {
                let home = dirs::home_dir().ok_or("could not resolve the user's home directory")?;
                home_data_dir(&home)
            }
        };
        Ok(daemon_for(&data_dir))
    }

    /// Install the shared logging stack ([`fauna_log::init`]) rooted at the
    /// daemon's data dir: the in-memory ring (backs the admin Logs view), the
    /// size-capped file `<data_dir>/logs/fauna.log.<date>` and stderr. The file
    /// is this daemon's only persistent log — the LaunchDaemon plist redirects
    /// nothing (`observability.md` § Persistence & privacy), the same reason the
    /// Windows `fauna-nest-service` uses `fauna_log::init` while the standalone
    /// `fauna-nest` (journald / Docker keep its stdout) does not. Hold the guard
    /// for the whole process.
    pub fn init_logging(data_dir: &Path) -> Option<fauna_log::LogGuard> {
        fauna_log::init(data_dir)
    }

    /// The graceful-shutdown future for the in-process loop: launchd sends
    /// **SIGTERM** to stop a `LaunchDaemon`; we also honour SIGINT for a foreground
    /// run. Delegates to the standalone `fauna-nest` binary's own shared
    /// implementation (`fauna_nest::unix_signal::shutdown_signal`) rather than
    /// hand-duplicating it.
    pub async fn shutdown_signal() {
        fauna_nest::unix_signal::shutdown_signal().await
    }

    // ---------------------------------------------------------------------------
    // launchd socket activation for the privileged `:443`
    //
    // A `LaunchDaemon` under the non-root `_fauna` user cannot bind `:443`
    // itself. Root launchd pre-binds the port (declared in the plist's `Sockets`
    // dict — added by the `.pkg` postinstall, slice S3) and hands this process the
    // listening fd via `launch_activate_socket`. The daemon serves directly on the
    // inherited fd (`installers/macos.md` § Network-reachable nest; `nest/common.md`
    // § Serving ports). Off launchd (a foreground / dev run) the call reports no socket and the shared serve loop binds
    // the direct `0.0.0.0:3000` seed instead — the as-built path.
    // ---------------------------------------------------------------------------
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::os::fd::FromRawFd;

    /// The launchd `Sockets` entry name the `.pkg`'s `social.fauna.nest.plist`
    /// declares for the client-facing `:443` listener (slice S3 adds the matching
    /// `Sockets` dict). Root launchd pre-binds it; this daemon inherits the fd.
    const NEST_SOCKET_NAME: &str = "FaunaNest";

    // errno values `launch_activate_socket` returns for the two "no socket, that's
    // fine, bind the seed" cases (`<sys/errno.h>`): the process is not managed by
    // launchd (a foreground / dev run), or the plist declares no such socket name.
    const ESRCH: c_int = 3;
    const ENOENT: c_int = 2;

    // SAFETY (block): these are the documented libSystem entry points; declaring
    // them here links against libSystem (always linked on macOS) — no extra crate.
    unsafe extern "C" {
        /// Hand back the launchd-pre-bound socket fd(s) for the `Sockets` entry
        /// named `name` in this job's plist. Returns `0` and fills `*fds` (a
        /// malloc'd array of `*cnt` fds the caller owns + frees) on success; a
        /// non-zero errno (`ESRCH` not-launchd-managed, `ENOENT` no-such-name)
        /// otherwise. Since macOS 10.10 — `man launch_activate_socket`.
        fn launch_activate_socket(
            name: *const c_char,
            fds: *mut *mut c_int,
            cnt: *mut usize,
        ) -> c_int;
        /// Free the fd array `launch_activate_socket` malloc'd (libc `free(3)`).
        fn free(ptr: *mut c_void);
    }

    /// The pure decision `launch_activate_socket`'s `(return code, fd count)`
    /// maps to — extracted from the FFI so it is unit-testable without a real
    /// launchd (the live fd inheritance remains untested: sudo + a real
    /// `LaunchDaemon`).
    #[derive(Debug, PartialEq, Eq)]
    pub enum Activation {
        /// launchd handed ≥1 pre-bound fd — serve on it (the privileged `:443`).
        Activated,
        /// No launchd socket for this name (not launchd-managed, or no such
        /// `Sockets` entry) — bind the direct `:3000` seed, the as-built path.
        NotPresent,
        /// `launch_activate_socket` failed for another reason — log it and still
        /// fall back to the seed (never refuse to serve — recoverability).
        Failed,
    }

    /// Map `launch_activate_socket`'s outcome to the [`Activation`] decision.
    pub fn classify_activation(rc: c_int, fd_count: usize) -> Activation {
        match rc {
            0 if fd_count >= 1 => Activation::Activated,
            // Success but zero fds — not expected; treat as "no socket" (defensive).
            0 => Activation::NotPresent,
            ESRCH | ENOENT => Activation::NotPresent,
            _ => Activation::Failed,
        }
    }

    /// Acquire the launchd socket-activated external listener for the nest's
    /// client-facing `:443`, or `None` to bind the direct seed. **Call once per
    /// process** — `launch_activate_socket` is one-shot per socket name. Not
    /// unit-tested (the FFI needs a real launchd); the pure decision it defers to
    /// ([`classify_activation`]) is.
    pub fn activate_nest_listener() -> Option<std::net::TcpListener> {
        // `NEST_SOCKET_NAME` is a static literal with no interior NUL.
        let name = CString::new(NEST_SOCKET_NAME).expect("socket name has no interior NUL");
        let mut fds: *mut c_int = std::ptr::null_mut();
        let mut cnt: usize = 0;
        // SAFETY: pass a valid NUL-terminated name + two out-pointers; read the
        // out-params only after checking `rc` via `classify_activation`.
        let rc = unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut cnt) };

        match classify_activation(rc, cnt) {
            Activation::Activated => {
                // SAFETY: rc==0 with cnt>=1 ⇒ `fds` is a valid array of `cnt` fds.
                let fd_slice = unsafe { std::slice::from_raw_parts(fds, cnt) };
                let primary = fd_slice[0];
                // launchd hands one fd per bound address; the `0.0.0.0:443` seed is
                // single-stack IPv4 today, so extras only appear if the socket is
                // later made dual-stack. Close any extras (dropping the listener
                // closes the fd) so we don't leak them — single-listener seam for now.
                for &extra in &fd_slice[1..] {
                    // SAFETY: each extra is a distinct valid listening fd we own.
                    drop(unsafe { std::net::TcpListener::from_raw_fd(extra) });
                }
                // SAFETY: `primary` is a valid listening-socket fd we now own.
                let listener = unsafe { std::net::TcpListener::from_raw_fd(primary) };
                // SAFETY: free the launchd-malloc'd array; the fds it held are now
                // owned by the TcpListeners, so we free only the array memory.
                unsafe { free(fds as *mut c_void) };
                tracing::info!(
                    count = cnt,
                    socket = NEST_SOCKET_NAME,
                    "inherited launchd socket-activated listener; serving on the pre-bound :443 fd"
                );
                Some(listener)
            }
            Activation::NotPresent => {
                // `fds` is untouched on the error paths, but free defensively in
                // case launchd allocated then reported a soft error.
                if !fds.is_null() {
                    // SAFETY: a non-null `fds` is a launchd-malloc'd array.
                    unsafe { free(fds as *mut c_void) };
                }
                tracing::info!(
                    rc,
                    socket = NEST_SOCKET_NAME,
                    "no launchd socket-activated listener (foreground/dev run or no Sockets entry); \
                     binding the direct serving-port seed"
                );
                None
            }
            Activation::Failed => {
                if !fds.is_null() {
                    // SAFETY: a non-null `fds` is a launchd-malloc'd array.
                    unsafe { free(fds as *mut c_void) };
                }
                tracing::error!(
                    rc,
                    socket = NEST_SOCKET_NAME,
                    "launch_activate_socket failed unexpectedly; falling back to the direct \
                     serving-port seed (the nest stays reachable on the seed)"
                );
                None
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The daemon's ONLY persistent log is its own bounded `fauna_log` file
        /// under `<data_dir>/logs/` — the `.pkg`'s LaunchDaemon keeps no
        /// stdout/stderr file (`observability.md` § Persistence & privacy), so a
        /// line `main` logs after [`init_logging`] must reach that file. (The one
        /// test in this binary that installs the process-global subscriber.)
        #[test]
        fn init_logging_writes_the_bounded_file_under_the_data_dir() {
            let data_dir = std::env::temp_dir()
                .join(format!("fauna-nest-daemon-log-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&data_dir);
            let guard = init_logging(&data_dir);
            assert!(guard.is_some(), "the global subscriber must install");
            tracing::info!("nest-daemon-log-marker");
            drop(guard); // flushes the non-blocking file writer

            let logs = data_dir.join("logs");
            let text: String = std::fs::read_dir(&logs)
                .unwrap_or_else(|e| panic!("no log dir at {logs:?}: {e}"))
                .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
                .collect();
            let _ = std::fs::remove_dir_all(&data_dir);
            assert!(
                text.contains("nest-daemon-log-marker"),
                "the marker line must land in <data_dir>/logs/fauna.log.*: {text:?}"
            );
        }

        /// `daemon_for` resolves the data dir + the fixed 3000 loopback + 3000
        /// serving seed.
        #[test]
        fn daemon_for_uses_explicit_data_dir_and_macos_ports() {
            let d = daemon_for(Path::new("/Library/Application Support/Fauna"));
            assert_eq!(
                d.data_dir,
                PathBuf::from("/Library/Application Support/Fauna")
            );
            assert_eq!(d.loopback_port, 3000);
            assert_eq!(d.default_serving_port, 3000);
        }

        /// The serve config carries the `0.0.0.0:3000` seed (all interfaces), the
        /// data dir, the macOS seed, and asks for the fixed 3000 co-located
        /// loopback listener.
        #[test]
        fn serve_config_carries_seed_loopback_and_data_dir() {
            let d = daemon_for(Path::new("/Library/Application Support/Fauna"));
            let cfg = d.serve_config();
            assert_eq!(cfg.bind, SocketAddr::from(([0, 0, 0, 0], 3000)));
            assert_eq!(
                cfg.data_dir,
                PathBuf::from("/Library/Application Support/Fauna")
            );
            assert_eq!(cfg.default_serving_port, 3000);
            assert_eq!(cfg.internal_loopback_port, Some(3000));
        }

        /// The pure socket-activation decision (`launch_activate_socket` outcome →
        /// use-the-fd vs bind-the-seed). The live fd inheritance remains
        /// untested; this is the testable half (S1).
        #[test]
        fn classify_activation_decides_use_fd_vs_seed() {
            // launchd handed ≥1 pre-bound fd ⇒ use it (the privileged :443).
            assert_eq!(classify_activation(0, 1), Activation::Activated);
            assert_eq!(classify_activation(0, 2), Activation::Activated);
            // Success but zero fds (defensive) ⇒ bind the direct seed.
            assert_eq!(classify_activation(0, 0), Activation::NotPresent);
            // Not launchd-managed (a foreground / dev run) ⇒ seed.
            assert_eq!(classify_activation(ESRCH, 0), Activation::NotPresent);
            // No such `Sockets` entry in the plist (a foreground / dev run) ⇒ seed.
            assert_eq!(classify_activation(ENOENT, 0), Activation::NotPresent);
            // Any other errno (e.g. EINVAL = 22) ⇒ logged failure, still seed.
            assert_eq!(classify_activation(22, 0), Activation::Failed);
        }

        /// `FAUNA_DATA_DIR` parsing: unset / empty / whitespace ⇒ `None` (home
        /// fallback); a real path ⇒ `Some(path)`. Home fallback derives
        /// `<home>/Library/Application Support/Fauna`.
        #[test]
        fn data_dir_override_and_home_fallback() {
            assert_eq!(data_dir_override(None), None);
            assert_eq!(data_dir_override(Some("  ")), None);
            assert_eq!(
                data_dir_override(Some("/Library/Application Support/Fauna")),
                Some(PathBuf::from("/Library/Application Support/Fauna"))
            );
            assert_eq!(
                home_data_dir(Path::new("/Users/testuser")),
                PathBuf::from("/Users/testuser/Library/Application Support/Fauna")
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    // The data dir roots the log file, so it resolves first; a failure here
    // has no file to reach yet — stderr is the only surface, and launchd's
    // `launchctl print system/social.fauna.nest` keeps the exit status.
    let daemon = match macos::resolve() {
        Ok(daemon) => daemon,
        Err(e) => {
            eprintln!("failed to resolve macOS nest daemon configuration: {e}");
            std::process::exit(1);
        }
    };
    let _log_guard = macos::init_logging(&daemon.data_dir);
    // Acquire the launchd socket-activated listener ONCE (the FFI is one-shot per
    // socket name per process): `Some(:443 fd)` under a `LaunchDaemon` whose plist
    // declares the `FaunaNest` socket (slice S3), `None` off launchd (a foreground / dev run) ⇒
    // the shared loop binds the direct `0.0.0.0:3000` seed.
    let activated = macos::activate_nest_listener();
    tracing::info!(
        data_dir = %daemon.data_dir.display(),
        serving_seed = daemon.default_serving_port,
        socket_activated = activated.is_some(),
        "macOS nest daemon starting (in-process serve loop)"
    );

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("failed to create tokio runtime: {e}");
            std::process::exit(1);
        }
    };

    // Run the nest in-process via the shared cross-OS serve-and-restart loop,
    // until launchd's SIGTERM (or a foreground SIGINT) ends it. The launchd
    // socket-activated listener (when present) serves the privileged `:443`;
    // `None` ready channel — only tests observe the bound addr.
    let result = rt.block_on(fauna_nest::desktop_serve::run_serve_loop(
        daemon.serve_config(),
        activated,
        macos::shutdown_signal(),
        None,
    ));

    if let Err(e) = result {
        tracing::error!("macOS nest daemon serve loop ended with error: {e}");
        std::process::exit(1);
    }
    tracing::info!("macOS nest daemon stopped cleanly");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    // This daemon's path resolution is macOS-specific (`~/Library/Application
    // Support/Fauna` / the `_fauna` system data-root). The Windows desktop uses
    // `apps/fauna-windows/fauna-nest-service` (SCM shell). The crate still builds
    // on every platform (workspace member) so CI stays green; it just refuses to
    // run off macOS.
    eprintln!("fauna-nest-daemon is only supported on macOS");
    std::process::exit(1);
}

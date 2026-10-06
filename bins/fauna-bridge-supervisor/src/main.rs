//! Desktop MDA supervisor — the macOS shell around [`fauna_mda_supervisor`].
//!
//! The `social.fauna.bridge` LaunchDaemon runs this binary (KeepAlive'd by launchd).
//! It resolves the macOS data-dir layout and then drives the shared reconcile
//! loop: spawn the Go `fauna-mail-bridge` MDA child (CalDAV + IMAP) when the nest
//! has enabled mail and/or CalDAV, restart it on exit, stop it when both clear,
//! and re-pin its CalDAV listener when the admin changes the port. This is the
//! macOS analogue of the Windows `fauna-bridge-service` SCM shell; the cross-OS
//! reconcile logic is shared in `libs/fauna-mda-supervisor`. See
//! `docs/goal/architecture/installers/macos.md` (design tracked internally).

#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;

    use fauna_mda_supervisor::MdaSupervisor;

    /// The fixed internal-loopback the co-located MDA dials —
    /// `fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT` = 3000, also
    /// the default `serving_port`/`listen` the macOS nest binds (`0.0.0.0:3000`,
    /// which covers loopback). Per `nest/common.md` § Same-box reach. The MDA's
    /// WS-RPC client upgrades `https://` → `wss://` and TOFU-accepts the loopback
    /// self-signed floor cert. (Kept as a local literal — same duplication-with-
    /// citation pattern the shared crate uses for the nest flag-name consts —
    /// rather than depending on `fauna-protocol` from a desktop service shell.)
    const NEST_LOOPBACK_ENDPOINT: &str = "https://127.0.0.1:3000";

    /// Construct an [`MdaSupervisor`] from the **Fauna support dir** (the dir
    /// holding `nest.db` + the nest's enable/port flags).
    ///
    /// This supervisor runs as the dedicated `_fauna-bridge` `LaunchDaemon` (no
    /// usable home dir), pointed at the **system** data-root `/Library/Application
    /// Support/Fauna` (`installers/macos.md` § File Layout) — the same root the
    /// `_fauna` nest daemon gets via `FAUNA_DATA_DIR`. `nest_data_dir` is that dir
    /// (flags read from it — no `nest` subdir, unlike the Windows
    /// `%PROGRAMDATA%\Fauna\nest` layout); `bridge_data_dir` is its `bridge/`
    /// child (`keys/mda.key` + the operator-hatch the supervisor writes);
    /// `mda_exe` is the `fauna-mail-bridge` binary staged beside this supervisor
    /// (both install to `/usr/local/bin/`, per `installers/macos.md` File Layout).
    /// Pure — no env / fs reads.
    pub fn supervisor_for_support_dir(support: &Path, exe_dir: &Path) -> MdaSupervisor {
        MdaSupervisor {
            mda_exe: exe_dir.join("fauna-mail-bridge"),
            bridge_data_dir: support.join("bridge"),
            nest_data_dir: support.to_path_buf(),
            nest_endpoint: NEST_LOOPBACK_ENDPOINT.to_string(),
            caldav_listen: fauna_mda_supervisor::CALDAV_LISTEN.to_string(),
            log_level: "info".to_string(),
        }
    }

    /// The Fauna support dir from the `FAUNA_DATA_DIR` bucket-2 IPC env, or `None`
    /// when it is unset.
    ///
    /// `FAUNA_DATA_DIR` is **artifact-wiring, never a human-edited knob** — a
    /// product invariant, the IPC sibling of `FAUNA_INTERNAL_LOOPBACK_PORT`: the
    /// `_fauna-bridge` `LaunchDaemon` sets it so the supervisor finds the system
    /// data-root without a home dir. The same env points the `_fauna` nest daemon at
    /// the same root (`bins/fauna-nest/src/main.rs::data_dir_db_path_override`), so
    /// the bridge reads the flags the nest writes there. A whitespace-only / empty
    /// value is treated as unset. (Kept as a local literal — the same
    /// duplication-with-citation pattern the shared crate uses for the nest flag
    /// consts — rather than a cross-binary const dependency.)
    fn data_dir_support_override(raw: Option<&str>) -> Option<std::path::PathBuf> {
        fauna_deployment_flags::parse_dir_override(raw)
    }

    /// Resolve a production [`MdaSupervisor`]: the system data-root from
    /// `FAUNA_DATA_DIR`, which the `_fauna-bridge` LaunchDaemon always sets
    /// (`installer/macos/scripts/bridge/postinstall`). Unset is an error, never a
    /// guessed home-derived path. `exe_dir` is this supervisor's own directory
    /// (the MDA is staged beside it).
    pub fn resolve() -> Result<MdaSupervisor, String> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .ok_or("could not resolve the supervisor's own exe directory")?;
        let support = data_dir_support_override(std::env::var("FAUNA_DATA_DIR").ok().as_deref())
            .ok_or("FAUNA_DATA_DIR is unset: the social.fauna.bridge LaunchDaemon sets it")?;
        Ok(supervisor_for_support_dir(&support, &exe_dir))
    }

    /// Install the shared logging stack ([`fauna_log::init`]) rooted at the
    /// supervisor's own `bridge/` dir — the one `_fauna-bridge` owns (the
    /// support root above it is `_fauna`'s and not writable here): the ring, the
    /// size-capped file `<support>/bridge/logs/fauna.log.<date>` and stderr. The
    /// file is the supervisor's only persistent log and also holds the MDA
    /// child's captured output (`fauna_mda_supervisor` pipes it into tracing):
    /// the LaunchDaemon plist redirects nothing (`observability.md`
    /// § Persistence & privacy). Hold the guard for the whole process.
    pub fn init_logging(supervisor: &MdaSupervisor) -> Option<fauna_log::LogGuard> {
        fauna_log::init(&supervisor.bridge_data_dir)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        /// The supervisor's only persistent log is its bounded `fauna_log` file
        /// under the `bridge/` dir it owns, and a line logged after
        /// [`init_logging`] reaches it. (The one test in this binary that
        /// installs the process-global subscriber.)
        #[test]
        fn init_logging_writes_the_bounded_file_under_the_bridge_dir() {
            let support = std::env::temp_dir().join(format!(
                "fauna-bridge-supervisor-log-test-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&support);
            let sup = supervisor_for_support_dir(&support, Path::new("/usr/local/bin"));
            let guard = init_logging(&sup);
            assert!(guard.is_some(), "the global subscriber must install");
            tracing::info!("bridge-supervisor-log-marker");
            drop(guard); // flushes the non-blocking file writer

            let logs = support.join("bridge").join("logs");
            let text: String = std::fs::read_dir(&logs)
                .unwrap_or_else(|e| panic!("no log dir at {logs:?}: {e}"))
                .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
                .collect();
            let _ = std::fs::remove_dir_all(&support);
            assert!(
                text.contains("bridge-supervisor-log-marker"),
                "the marker line must land in <support>/bridge/logs/fauna.log.*: {text:?}"
            );
        }

        /// `FAUNA_DATA_DIR` points straight at the support dir (the `_fauna-bridge`
        /// LaunchDaemon's `/Library/Application Support/Fauna`), with the bridge
        /// keypair under its `bridge/` child, and the co-located MDA dials the
        /// fixed `127.0.0.1:3000` loopback.
        #[test]
        fn supervisor_for_support_dir_uses_system_root_directly() {
            let sup = supervisor_for_support_dir(
                Path::new("/Library/Application Support/Fauna"),
                Path::new("/usr/local/bin"),
            );
            assert_eq!(
                sup.nest_data_dir,
                PathBuf::from("/Library/Application Support/Fauna"),
            );
            assert_eq!(
                sup.bridge_data_dir,
                PathBuf::from("/Library/Application Support/Fauna/bridge"),
            );
            assert_eq!(
                sup.keypair_path(),
                PathBuf::from("/Library/Application Support/Fauna/bridge/keys/mda.key"),
            );
            assert_eq!(
                sup.mda_exe,
                PathBuf::from("/usr/local/bin/fauna-mail-bridge")
            );
            assert_eq!(sup.nest_endpoint, "https://127.0.0.1:3000");
            assert_eq!(sup.caldav_listen, "0.0.0.0:8443");
        }

        /// `FAUNA_DATA_DIR` parsing: unset / empty / whitespace ⇒ `None` (which
        /// `resolve` refuses); a real path ⇒ `Some(path)`.
        #[test]
        fn data_dir_support_override_parses_env() {
            assert_eq!(data_dir_support_override(None), None);
            assert_eq!(data_dir_support_override(Some("")), None);
            assert_eq!(data_dir_support_override(Some("   ")), None);
            assert_eq!(
                data_dir_support_override(Some("/Library/Application Support/Fauna")),
                Some(PathBuf::from("/Library/Application Support/Fauna")),
            );
            // Trailing whitespace tolerated (trimmed before use).
            assert_eq!(
                data_dir_support_override(Some("/srv/fauna\n")),
                Some(PathBuf::from("/srv/fauna")),
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    // The bridge dir roots the log file, so it resolves first; a failure here
    // has no file to reach yet — stderr is the only surface, and launchd's
    // `launchctl print system/social.fauna.bridge` keeps the exit status.
    let supervisor = match macos::resolve() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to resolve macOS supervisor configuration: {e}");
            std::process::exit(1);
        }
    };
    let _log_guard = macos::init_logging(&supervisor);

    let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    // `run()` loops until the process is signalled (launchd stop / SIGTERM), at
    // which point the child is reaped via `kill_on_drop`.
    rt.block_on(supervisor.run());
}

#[cfg(not(target_os = "macos"))]
fn main() {
    // This supervisor's path resolution is macOS-specific (`~/Library/Application
    // Support/Fauna`). The Windows desktop uses `fauna-bridge-service` (SCM shell);
    // a Linux-native systemd shell is future work (design spec D2). The crate still
    // builds on every platform (workspace member) so CI stays green; it just refuses
    // to run off macOS.
    eprintln!("fauna-bridge-supervisor is only supported on macOS");
    std::process::exit(1);
}

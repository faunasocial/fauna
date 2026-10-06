//! Platform-agnostic supervision of the Go `fauna-mail-bridge` MDA child process.
//!
//! The desktop **MDA supervisor** is a shell process that spawns the Go
//! `fauna-mail-bridge` (the MDA role — IMAP + CalDAV) as a monitored child,
//! restarts it on unexpected exit, and stops it when the admin has disabled both
//! mail and CalDAV. It is the desktop port of the Linux
//! `fauna_nest::mail_enable::reconcile_supervisor` → s6 model.
//!
//! This crate holds the **reconcile logic only** — the genuinely cross-platform
//! policy. The per-OS *shells* that drive it differ and live with their consumer:
//! - **Windows** — `apps/fauna-windows/fauna-bridge-service` runs this loop under
//!   the SCM, resolving `%PROGRAMDATA%\Fauna\{bridge,nest}` paths + `.exe` staging.
//! - **macOS** — `bins/fauna-bridge-supervisor`, run by the `social.fauna.bridge`
//!   LaunchDaemon (KeepAlive'd by launchd) on the system root
//!   `/Library/Application Support/Fauna` (its `bridge/` child).
//!
//! Both shells receive the child's stdout/stderr through their own `tracing`
//! ([`MdaSupervisor::spawn_at`] pipes it), never an inherited stream.
//!
//! Each shell constructs an [`MdaSupervisor`] (all fields are public) with its own
//! resolved paths and calls [`MdaSupervisor::run`] (design tracked
//! internally, § 3.6 and § 4 — the supervision *logic* is shared; the
//! *shell* is per-OS).
//!
//! ## How the supervisor learns "should the MDA run?"
//!
//! The nest binary materializes an enable flag file in its data dir whenever the
//! admin toggles mail / CalDAV / CardDAV / WebDAV
//! (`fauna_nest::mail_enable::set_{mail,caldav,carddav,webdav}_enable_flag`,
//! re-asserted by a 60 s reconcile tick): `imap-enabled`, `caldav-enabled`,
//! `carddav-enabled`, `webdav-enabled`. The MDA hosts all four protocols, so it
//! must run iff *any* flag is present — the same gate the Linux s6 run-script uses
//! (`docker/s6/fauna-mail-bridge-mda/run`).
//! The supervisor reads these files rather than calling `fauna.bridges.fetch_config`
//! (which is `BridgeMta | BridgeMda`-only — the supervisor shell holds no bridge
//! enrollment; only the MDA child does). The MDA itself still calls `fetch_config`
//! to decide *which* listeners to bind, and exits cleanly on `config_changed` so the
//! supervisor restarts it bound to the new protocol set.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

/// Poll interval for re-reading the nest enable flags. Matches nest's 60 s
/// reconcile cadence loosely; a *running* MDA re-binds promptly on its own
/// `config_changed`-driven clean exit, so this tick only governs the
/// both-off ↔ something-on *process* transition (start-from-stopped / stop).
const RECONCILE_POLL: Duration = Duration::from_secs(15);

/// Delay before respawning the MDA after it exits — a crash backstop and the
/// clean config-rebind restart. Matches the Windows WiX service failure-action
/// cadence (`RestartServiceDelayInSeconds="5"`).
const RESTART_DELAY: Duration = Duration::from_secs(5);

/// The `tracing` target every line of the MDA child's stdout/stderr is
/// re-emitted under (see [`forward_child_output`]).
pub const MDA_LOG_TARGET: &str = "fauna_mail_bridge";

/// The level a line of the MDA's output is re-emitted at: the Go bridge logs
/// one JSON object per line with a `"level"` of `DEBUG`/`INFO`/`WARN`/`ERROR`
/// (`bins/fauna-bridges/internal/logging`); anything else — a Go panic dump, a
/// plain print — is `info`.
fn mda_line_level(line: &str) -> tracing::Level {
    if line.contains(r#""level":"ERROR""#) {
        tracing::Level::ERROR
    } else if line.contains(r#""level":"WARN""#) {
        tracing::Level::WARN
    } else if line.contains(r#""level":"DEBUG""#) {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    }
}

/// Re-emit every line of the spawned MDA's stdout and stderr through the
/// supervisor's own `tracing`, so they land in its bounded `fauna_log` file
/// (and its admin-visible ring). The child must not inherit the supervisor's
/// streams: under a LaunchDaemon (no stdout redirect — a deployment artifact
/// never redirects into a file, `observability.md` § Persistence & privacy) or
/// the Windows SCM nothing keeps them, so an inherited stream is lost. Each
/// forwarding task ends at the child's EOF.
fn forward_child_output(child: &mut tokio::process::Child) {
    use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

    fn forward(stream: impl AsyncRead + Unpin + Send + 'static) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match mda_line_level(&line) {
                    tracing::Level::ERROR => tracing::error!(target: MDA_LOG_TARGET, "{line}"),
                    tracing::Level::WARN => tracing::warn!(target: MDA_LOG_TARGET, "{line}"),
                    tracing::Level::DEBUG => tracing::debug!(target: MDA_LOG_TARGET, "{line}"),
                    _ => tracing::info!(target: MDA_LOG_TARGET, "{line}"),
                }
            }
        });
    }
    if let Some(out) = child.stdout.take() {
        forward(out);
    }
    if let Some(err) = child.stderr.take() {
        forward(err);
    }
}

/// The flag filenames the nest materializes in its data dir and this supervisor
/// reads.
///
/// These are an on-disk cross-process contract between the nest (writer) and this
/// supervisor (reader), so they are not written here: they are re-exported from
/// [`fauna_deployment_flags`], the zero-dependency crate that owns the whole set.
/// It exists exactly so this shell can hold the contract *without* depending on
/// the `fauna-nest` crate, which would be backwards — the constraint that used to
/// be met by keeping local literals matched by hand.
///
/// Re-exported rather than merely used so this crate's per-OS service-shell
/// consumers keep the names they already import.
///
/// [`CALDAV_PORT_FLAG`] is the odd one: unlike the four *enable* flags it is a
/// **value** carrier, not a presence gate. The nest materializes it on
/// `fauna.bridges.set_caldav_port` (+ a 60 s reconcile tick) so a desktop box's
/// supervisor — which can't call `fetch_config` (bridge-only) — learns the
/// admin's chosen port and re-pins the MDA's `caldav_listen_https`. Absent ⇒ the
/// default port (8443, carried by [`CALDAV_LISTEN`]). Per
/// `caldav-server.md` § Network exposure (Desktop / IP deployment).
pub use fauna_deployment_flags::{
    CALDAV_ENABLE_FLAG, CALDAV_PORT_FLAG, CARDDAV_ENABLE_FLAG, MAIL_ENABLE_FLAG, WEBDAV_ENABLE_FLAG,
};

/// Whether the MDA child should be running: true iff the nest has materialized
/// **any** of the four DAV/mail enable flags in its data dir. The one MDA process
/// hosts IMAP + CalDAV + CardDAV + WebDAV, so any enabled protocol keeps it up
/// (mirrors `fauna_nest::mail_enable::mda_should_run`).
///
/// `nest_data_dir` is the nest's data directory (`%PROGRAMDATA%\Fauna\nest` on a
/// desktop Windows box; `~/Library/Application Support/Fauna/nest` on macOS),
/// where the nest writes `imap-enabled` / `caldav-enabled` / `carddav-enabled` /
/// `webdav-enabled`.
pub fn mda_should_run(nest_data_dir: &Path) -> bool {
    nest_data_dir.join(MAIL_ENABLE_FLAG).exists()
        || nest_data_dir.join(CALDAV_ENABLE_FLAG).exists()
        || nest_data_dir.join(CARDDAV_ENABLE_FLAG).exists()
        || nest_data_dir.join(WEBDAV_ENABLE_FLAG).exists()
}

/// The CalDAV listen address (`<iface>:<port>`) a desktop supervisor starts the
/// MDA with: every interface at the default port (8443 =
/// `fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT`, tied by a unit test).
/// The interface is deployment topology, not anyone's choice, so it is this one
/// constant for both desktop supervisors (Windows service, macOS LaunchDaemon);
/// only the **port** is the admin's in-app choice, folded over it by
/// [`MdaSupervisor::effective_caldav_listen`]. Per `caldav-server.md` § Network
/// exposure (Desktop / IP deployment).
pub const CALDAV_LISTEN: &str = "0.0.0.0:8443";

/// Operator-hatch filename the MDA reads from its `--data-dir`.
///
/// The Go MDA loads `<data-dir>/operator-hatch.toml` for deployment-topology
/// overrides (`bins/fauna-bridges/internal/config`). On a desktop box this is
/// how we point its CalDAV listener at a routable, customizable port (there is no
/// SNI router fronting `:443`, so the listener binds directly).
pub const OPERATOR_HATCH_FILE: &str = "operator-hatch.toml";

/// The subset of the MDA operator-hatch this supervisor writes. Only
/// `caldav_listen_https` is set on the desktop box — IMAP keeps its own direct
/// 993/143 defaults, and everything else is nest-provisioned.
#[derive(Serialize)]
struct OperatorHatch<'a> {
    caldav_listen_https: &'a str,
}

/// Render the MDA operator-hatch TOML pinning the CalDAV listen address.
///
/// `caldav_listen_https` is an `<iface>:<port>` bind (e.g. `0.0.0.0:8443`) — the
/// deployment-topology seed for serving CalDAV directly to LAN MUAs, per
/// `caldav-server.md` § Network exposure (Desktop / IP deployment).
pub fn operator_hatch_toml(caldav_listen_https: &str) -> String {
    let hatch = OperatorHatch {
        caldav_listen_https,
    };
    toml::to_string(&hatch).expect("operator-hatch is trivially serializable")
}

/// Write `<bridge_data_dir>/operator-hatch.toml` pinning the CalDAV listen
/// address, creating the data dir if needed.
pub fn write_operator_hatch(
    bridge_data_dir: &Path,
    caldav_listen_https: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(bridge_data_dir)?;
    let path = bridge_data_dir.join(OPERATOR_HATCH_FILE);
    std::fs::write(path, operator_hatch_toml(caldav_listen_https))
}

/// What the supervise loop should do this tick, given the child's running state
/// and whether the nest enable flags say it should run.
#[derive(Debug, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Not running but should be → start it (also the restart-after-exit case).
    Spawn,
    /// Running but shouldn't be → stop it (both enable flags cleared).
    Stop,
    /// Running and should keep running, but the admin changed the CalDAV port
    /// (`<nest_data_dir>/caldav-port`) → kill + respawn so the MDA re-binds the
    /// listener (an in-process listener can't be re-bound live; the supervisor is
    /// the desktop analogue of the Docker s6 restart on the MDA's `config_changed`
    /// exit). Per `caldav-server.md` § Network exposure (Desktop / IP deployment).
    Restart,
    /// Already in the desired state → do nothing.
    Nothing,
}

/// Pure control decision for the supervise loop. `running` is whether the MDA
/// child is currently alive; `should_run` is [`mda_should_run`]; `port_changed`
/// is whether the admin-set CalDAV port now differs from the one the running
/// child was spawned with (requiring a rebind).
pub fn reconcile_action(running: bool, should_run: bool, port_changed: bool) -> ReconcileAction {
    match (running, should_run) {
        (false, true) => ReconcileAction::Spawn,
        (true, false) => ReconcileAction::Stop,
        (true, true) if port_changed => ReconcileAction::Restart,
        _ => ReconcileAction::Nothing,
    }
}

/// Resolved configuration for supervising one MDA child.
///
/// Constructed once by the per-OS shell (which resolves the data dirs + MDA exe
/// path for its platform) and then drives the spawn / monitor / reconcile loop.
/// Fields are public so each shell constructs it directly and tests can inject
/// temp paths.
pub struct MdaSupervisor {
    /// Path to the Go `fauna-mail-bridge` binary (staged beside the supervisor —
    /// `fauna-mail-bridge.exe` on Windows, `fauna-mail-bridge` on macOS).
    pub mda_exe: PathBuf,
    /// The bridge data dir (`%PROGRAMDATA%\Fauna\bridge` on Windows,
    /// `~/Library/Application Support/Fauna/bridge` on macOS) — the MDA's
    /// `--data-dir`; holds `keys/mda.key` and the `operator-hatch.toml` we write.
    pub bridge_data_dir: PathBuf,
    /// The nest data dir (`%PROGRAMDATA%\Fauna\nest` on Windows,
    /// `~/Library/Application Support/Fauna/nest` on macOS) — where the nest
    /// writes the `imap-enabled` / `caldav-enabled` flag files this supervisor reads.
    pub nest_data_dir: PathBuf,
    /// The local nest WS-RPC endpoint (`https://127.0.0.1:<loopback-port>`, from the
    /// device config — the FIXED internal-loopback port that survives an admin
    /// serving_port change, nest/common.md § Same-box reach).
    pub nest_endpoint: String,
    /// The CalDAV listen address pinned into the MDA operator-hatch (`<iface>:<port>`);
    /// both desktop supervisors set it to [`CALDAV_LISTEN`].
    pub caldav_listen: String,
    /// Log level passed to the MDA (`info` by default).
    pub log_level: String,
}

impl MdaSupervisor {
    /// Path to the MDA's Ed25519 service-user keypair (auto-generated by the MDA on
    /// first run). The MDA presents it to nest so `whoami` resolves its MDA role.
    pub fn keypair_path(&self) -> PathBuf {
        self.bridge_data_dir.join("keys").join("mda.key")
    }

    /// The command-line arguments to launch the MDA child with.
    ///
    /// Mirrors the Linux s6 run-script invocation
    /// (`docker/s6/fauna-mail-bridge-mda/run`), minus the Docker-specific paths:
    /// `--keypair-file <bridge>/keys/mda.key --nest-endpoint <nest>
    /// --data-dir <bridge> --log-level <level>`. The MDA discovers its MDA role
    /// from the keypair (no `--mode`/`--role` flag) and reads its CalDAV listen
    /// address from `<data-dir>/operator-hatch.toml` (written by the supervisor).
    pub fn mda_command_args(&self) -> Vec<String> {
        vec![
            "--keypair-file".to_string(),
            self.keypair_path().to_string_lossy().into_owned(),
            "--nest-endpoint".to_string(),
            self.nest_endpoint.clone(),
            "--data-dir".to_string(),
            self.bridge_data_dir.to_string_lossy().into_owned(),
            "--log-level".to_string(),
            self.log_level.clone(),
        ]
    }

    /// Whether the MDA should currently be running (either nest enable flag set).
    pub fn should_run(&self) -> bool {
        mda_should_run(&self.nest_data_dir)
    }

    /// The admin-set CalDAV port the nest has materialized in its data dir
    /// (`<nest_data_dir>/caldav-port`, decimal text), or `None` when the flag is
    /// absent / unparseable / `0`. `None` means "the admin never picked a port"
    /// (or the flag is corrupt) — the caller keeps the install-time default.
    fn admin_caldav_port(&self) -> Option<u16> {
        let path = self.nest_data_dir.join(CALDAV_PORT_FLAG);
        let raw = std::fs::read_to_string(&path).ok()?;
        let parsed = fauna_deployment_flags::parse_port_flag(&raw);
        if parsed.is_none() {
            tracing::warn!(
                flag = %path.display(),
                raw = %raw.trim(),
                "ignoring unparseable caldav-port flag — keeping install default",
            );
        }
        parsed
    }

    /// The CalDAV listen address (`<iface>:<port>`) the MDA should currently bind,
    /// folding the admin-set port (`<nest_data_dir>/caldav-port`) over the
    /// default interface. The interface comes from
    /// [`caldav_listen`](Self::caldav_listen) (the [`CALDAV_LISTEN`] constant);
    /// only the **port** is the admin's choice (`caldav-server.md` § Network
    /// exposure — the admin port drives the no-router direct listener). When the
    /// flag is absent or `caldav_listen` is malformed, it is used verbatim.
    pub fn effective_caldav_listen(&self) -> String {
        // Only override the port when both `caldav_listen` is well-formed
        // (`<iface>:<port>`) and the admin has set a valid port; otherwise keep it
        // verbatim (the interface is topology, never an admin choice).
        match (
            self.caldav_listen.rsplit_once(':'),
            self.admin_caldav_port(),
        ) {
            (Some((iface, _seed_port)), Some(admin_port)) => format!("{iface}:{admin_port}"),
            _ => self.caldav_listen.clone(),
        }
    }

    /// Filesystem prep before launching the MDA: ensure the keypair dir exists and
    /// pin `caldav_listen` via the operator-hatch. Separated from the process
    /// launch so it is unit-testable without spawning a child.
    pub fn prepare_spawn_at(&self, caldav_listen: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.bridge_data_dir.join("keys"))?;
        write_operator_hatch(&self.bridge_data_dir, caldav_listen)
    }

    /// Spawn the MDA child pinned to `caldav_listen` (`kill_on_drop` so a dropped
    /// supervisor — service stop — never orphans it), its stdout/stderr piped
    /// into this process's tracing ([`forward_child_output`]).
    pub fn spawn_at(&self, caldav_listen: &str) -> std::io::Result<tokio::process::Child> {
        self.prepare_spawn_at(caldav_listen)?;
        let mut cmd = tokio::process::Command::new(&self.mda_exe);
        cmd.args(self.mda_command_args())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        forward_child_output(&mut child);
        Ok(child)
    }

    /// The supervise loop: spawn the MDA when enabled, monitor it, restart on exit
    /// (the clean `config_changed` rebind *and* a crash) iff still enabled, and stop
    /// it when both enable flags clear. Runs until the future is dropped (service
    /// stop / Ctrl-C), at which point `kill_on_drop` reaps the child.
    pub async fn run(self) {
        let mut child: Option<tokio::process::Child> = None;
        // The CalDAV listen address the currently-running child was spawned with.
        // `None` when no child is running. Compared against the live effective
        // listen each tick to detect an admin port change (the nest rewrites
        // `<nest_data_dir>/caldav-port`), which forces a rebind restart.
        let mut spawned_listen: Option<String> = None;
        loop {
            let effective = self.effective_caldav_listen();
            let port_changed = spawned_listen
                .as_deref()
                .is_some_and(|pinned| pinned != effective);
            match reconcile_action(child.is_some(), self.should_run(), port_changed) {
                ReconcileAction::Spawn => match self.spawn_at(&effective) {
                    Ok(c) => {
                        tracing::info!(
                            exe = %self.mda_exe.display(),
                            caldav = %effective,
                            nest = %self.nest_endpoint,
                            "spawned fauna-mail-bridge MDA child",
                        );
                        child = Some(c);
                        spawned_listen = Some(effective.clone());
                    }
                    Err(e) => tracing::error!("failed to spawn MDA child: {e}"),
                },
                ReconcileAction::Stop => {
                    if let Some(mut c) = child.take() {
                        tracing::info!("stopping MDA child (mail + CalDAV both disabled)");
                        let _ = c.kill().await;
                    }
                    spawned_listen = None;
                }
                ReconcileAction::Restart => {
                    tracing::info!(
                        old = ?spawned_listen,
                        new = %effective,
                        "admin changed CalDAV port — restarting MDA child to rebind",
                    );
                    if let Some(mut c) = child.take() {
                        let _ = c.kill().await;
                    }
                    match self.spawn_at(&effective) {
                        Ok(c) => {
                            child = Some(c);
                            spawned_listen = Some(effective.clone());
                        }
                        Err(e) => {
                            tracing::error!("failed to respawn MDA child after port change: {e}");
                            spawned_listen = None;
                        }
                    }
                }
                ReconcileAction::Nothing => {}
            }

            match child.as_mut() {
                Some(c) => {
                    tokio::select! {
                        status = c.wait() => {
                            tracing::info!(?status, "MDA child exited; reconciling");
                            child = None;
                            tokio::time::sleep(RESTART_DELAY).await;
                        }
                        _ = tokio::time::sleep(RECONCILE_POLL) => {}
                    }
                }
                None => tokio::time::sleep(RECONCILE_POLL).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared listen constant is all interfaces at the protocol's default
    /// CalDAV port — the port half is tied to `fauna-protocol`, never retyped.
    #[test]
    fn caldav_listen_constant_is_all_interfaces_at_the_default_port() {
        assert_eq!(
            CALDAV_LISTEN,
            format!(
                "0.0.0.0:{}",
                fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT
            )
        );
    }

    fn supervisor(bridge_dir: PathBuf, nest_dir: PathBuf) -> MdaSupervisor {
        MdaSupervisor {
            mda_exe: PathBuf::from("/opt/fauna/fauna-mail-bridge"),
            bridge_data_dir: bridge_dir,
            nest_data_dir: nest_dir,
            nest_endpoint: "http://127.0.0.1:7450".to_string(),
            caldav_listen: "0.0.0.0:8443".to_string(),
            log_level: "info".to_string(),
        }
    }

    /// Create a fresh, uniquely-named temp dir for a test (no `tempfile` dep).
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fauna-mda-sup-{tag}-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn should_run_when_caldav_enabled_flag_present() {
        let dir = temp_dir("caldav-only");
        std::fs::write(dir.join(CALDAV_ENABLE_FLAG), b"").unwrap();
        assert!(mda_should_run(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_run_when_mail_enabled_flag_present() {
        let dir = temp_dir("imap-only");
        std::fs::write(dir.join(MAIL_ENABLE_FLAG), b"").unwrap();
        assert!(mda_should_run(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_run_when_both_flags_present() {
        let dir = temp_dir("both");
        std::fs::write(dir.join(MAIL_ENABLE_FLAG), b"").unwrap();
        std::fs::write(dir.join(CALDAV_ENABLE_FLAG), b"").unwrap();
        assert!(mda_should_run(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_run_when_carddav_only() {
        // CardDAV rides the same MDA (no separate port); a contacts-only desktop
        // box must keep the MDA up.
        let dir = temp_dir("carddav-only");
        std::fs::write(dir.join(CARDDAV_ENABLE_FLAG), b"").unwrap();
        assert!(mda_should_run(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_run_when_webdav_only() {
        // WebDAV rides the same MDA (no separate port); a files-only desktop box
        // must keep the MDA up (webdav-server.md § Independent enablement).
        let dir = temp_dir("webdav-only");
        std::fs::write(dir.join(WEBDAV_ENABLE_FLAG), b"").unwrap();
        assert!(mda_should_run(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_not_run_when_no_flags_present() {
        let dir = temp_dir("neither");
        assert!(!mda_should_run(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_not_run_when_dir_missing() {
        let dir = temp_dir("missing");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!mda_should_run(&dir));
    }

    #[test]
    fn operator_hatch_toml_sets_caldav_listen_and_parses_back() {
        let toml_str = operator_hatch_toml("0.0.0.0:8443");
        // The MDA's TOML loader keys on `caldav_listen_https`; the value must
        // round-trip exactly so the listener binds where we asked.
        let parsed: toml::Value = toml::from_str(&toml_str).expect("valid TOML");
        assert_eq!(
            parsed.get("caldav_listen_https").and_then(|v| v.as_str()),
            Some("0.0.0.0:8443")
        );
    }

    #[test]
    fn write_operator_hatch_creates_file_with_pinned_addr() {
        let dir = temp_dir("hatch-write");
        write_operator_hatch(&dir, "192.168.1.10:8443").unwrap();
        let path = dir.join(OPERATOR_HATCH_FILE);
        assert!(path.exists(), "operator-hatch.toml should be written");
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("caldav_listen_https").and_then(|v| v.as_str()),
            Some("192.168.1.10:8443")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_operator_hatch_creates_missing_data_dir() {
        let base = temp_dir("hatch-mkdir");
        let nested = base.join("bridge");
        // nested dir does not exist yet
        write_operator_hatch(&nested, "0.0.0.0:8443").unwrap();
        assert!(nested.join(OPERATOR_HATCH_FILE).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn keypair_path_is_keys_mda_key_under_bridge_data_dir() {
        let sup = supervisor(PathBuf::from("/data/bridge"), PathBuf::from("/data/nest"));
        let kp = sup.keypair_path();
        assert!(kp.ends_with("mda.key"), "got {kp:?}");
        assert_eq!(kp.parent().and_then(|p| p.file_name()).unwrap(), "keys");
        assert!(kp.starts_with("/data/bridge"), "got {kp:?}");
    }

    #[test]
    fn mda_command_args_carry_keypair_endpoint_datadir_loglevel() {
        let sup = supervisor(PathBuf::from("/data/bridge"), PathBuf::from("/data/nest"));
        let args = sup.mda_command_args();

        // Each flag is immediately followed by its value (the order the MDA's
        // flag parser is agnostic to, but the pairing must hold).
        let pos = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };

        assert_eq!(
            pos("--keypair-file"),
            Some(sup.keypair_path().to_string_lossy().into_owned())
        );
        assert_eq!(
            pos("--nest-endpoint"),
            Some("http://127.0.0.1:7450".to_string())
        );
        assert_eq!(
            pos("--data-dir"),
            Some(PathBuf::from("/data/bridge").to_string_lossy().into_owned())
        );
        assert_eq!(pos("--log-level"), Some("info".to_string()));
    }

    #[test]
    fn reconcile_spawns_when_enabled_and_not_running() {
        assert_eq!(reconcile_action(false, true, false), ReconcileAction::Spawn);
    }

    #[test]
    fn reconcile_stops_when_disabled_and_running() {
        assert_eq!(reconcile_action(true, false, false), ReconcileAction::Stop);
    }

    #[test]
    fn reconcile_noop_when_running_and_enabled() {
        assert_eq!(
            reconcile_action(true, true, false),
            ReconcileAction::Nothing
        );
    }

    #[test]
    fn reconcile_noop_when_stopped_and_disabled() {
        assert_eq!(
            reconcile_action(false, false, false),
            ReconcileAction::Nothing
        );
    }

    /// A running, still-enabled MDA whose admin CalDAV port changed must be
    /// restarted so it re-binds the listener (it cannot rebind in-process).
    #[test]
    fn reconcile_restarts_when_port_changed() {
        assert_eq!(reconcile_action(true, true, true), ReconcileAction::Restart);
    }

    /// A port "change" is irrelevant when the MDA is not running — the next
    /// spawn picks up the current port anyway, so it's a plain Spawn / Nothing.
    #[test]
    fn reconcile_ignores_port_change_when_not_running() {
        assert_eq!(reconcile_action(false, true, true), ReconcileAction::Spawn);
        assert_eq!(
            reconcile_action(false, false, true),
            ReconcileAction::Nothing
        );
    }

    /// With no `caldav-port` flag in the nest data dir, the effective listen is
    /// the install-time config seed verbatim (default port 8443).
    #[test]
    fn effective_caldav_listen_falls_back_when_flag_absent() {
        let bridge = temp_dir("eff-absent-bridge");
        let nest = temp_dir("eff-absent-nest");
        let sup = supervisor(bridge.clone(), nest.clone());
        assert_eq!(sup.effective_caldav_listen(), "0.0.0.0:8443");
        let _ = std::fs::remove_dir_all(&bridge);
        let _ = std::fs::remove_dir_all(&nest);
    }

    /// An admin-set `caldav-port` flag overrides the seed's port while keeping
    /// the install-time interface.
    #[test]
    fn effective_caldav_listen_uses_admin_port_flag() {
        let bridge = temp_dir("eff-port-bridge");
        let nest = temp_dir("eff-port-nest");
        std::fs::write(nest.join(CALDAV_PORT_FLAG), b"9443").unwrap();
        let sup = supervisor(bridge.clone(), nest.clone());
        assert_eq!(sup.effective_caldav_listen(), "0.0.0.0:9443");
        let _ = std::fs::remove_dir_all(&bridge);
        let _ = std::fs::remove_dir_all(&nest);
    }

    /// The admin port is folded onto a non-default install interface (a LAN bind
    /// box), not just `0.0.0.0`.
    #[test]
    fn effective_caldav_listen_preserves_non_default_interface() {
        let bridge = temp_dir("eff-iface-bridge");
        let nest = temp_dir("eff-iface-nest");
        std::fs::write(nest.join(CALDAV_PORT_FLAG), b"7000\n").unwrap();
        let mut sup = supervisor(bridge.clone(), nest.clone());
        sup.caldav_listen = "192.168.1.10:8443".to_string();
        assert_eq!(sup.effective_caldav_listen(), "192.168.1.10:7000");
        let _ = std::fs::remove_dir_all(&bridge);
        let _ = std::fs::remove_dir_all(&nest);
    }

    /// A malformed (non-numeric / zero / out-of-range) port flag is ignored — the
    /// supervisor keeps the install seed rather than binding a garbage address.
    #[test]
    fn effective_caldav_listen_ignores_malformed_flag() {
        let bridge = temp_dir("eff-bad-bridge");
        let nest = temp_dir("eff-bad-nest");
        let sup = supervisor(bridge.clone(), nest.clone());
        for bad in ["not-a-port", "0", "70000", ""] {
            std::fs::write(nest.join(CALDAV_PORT_FLAG), bad).unwrap();
            assert_eq!(
                sup.effective_caldav_listen(),
                "0.0.0.0:8443",
                "malformed flag {bad:?} must fall back to the seed"
            );
        }
        let _ = std::fs::remove_dir_all(&bridge);
        let _ = std::fs::remove_dir_all(&nest);
    }

    #[test]
    fn prepare_spawn_creates_keys_dir_and_pins_effective_caldav_listen() {
        let bridge = temp_dir("prep");
        let nest = temp_dir("prep-nest");
        let sup = supervisor(bridge.clone(), nest.clone());
        // No caldav-port flag in the nest dir → effective listen is the seed.
        sup.prepare_spawn_at(&sup.effective_caldav_listen())
            .unwrap();
        assert!(bridge.join("keys").is_dir(), "keys/ dir should be created");
        let hatch = std::fs::read_to_string(bridge.join("operator-hatch.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&hatch).unwrap();
        assert_eq!(
            parsed.get("caldav_listen_https").and_then(|v| v.as_str()),
            Some("0.0.0.0:8443")
        );
        let _ = std::fs::remove_dir_all(&bridge);
        let _ = std::fs::remove_dir_all(&nest);
    }

    /// One captured event: (level, target, message).
    type Captured = std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String, String)>>>;

    /// A layer that records every event's level, target and `message` field.
    struct Collect(Captured);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Collect {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Msg(String);
            impl tracing::field::Visit for Msg {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0 = format!("{v:?}");
                    }
                }
            }
            let mut msg = Msg(String::new());
            event.record(&mut msg);
            self.0.lock().unwrap().push((
                *event.metadata().level(),
                event.metadata().target().to_string(),
                msg.0,
            ));
        }
    }

    /// The Go MDA's JSON `level` decides the forwarded line's level; a line
    /// without one (a Go panic dump, a plain print) is forwarded at `info`.
    #[test]
    fn mda_line_level_reads_the_go_json_level() {
        use tracing::Level;
        assert_eq!(
            mda_line_level(r#"{"time":"t","level":"ERROR","msg":"x"}"#),
            Level::ERROR
        );
        assert_eq!(mda_line_level(r#"{"level":"WARN","msg":"x"}"#), Level::WARN);
        assert_eq!(mda_line_level(r#"{"level":"INFO","msg":"x"}"#), Level::INFO);
        assert_eq!(
            mda_line_level(r#"{"level":"DEBUG","msg":"x"}"#),
            Level::DEBUG
        );
        assert_eq!(mda_line_level("panic: runtime error"), Level::INFO);
    }

    /// The child's stdout and stderr are captured into the supervisor's own
    /// tracing (and so into its bounded `fauna_log` file), never inherited:
    /// under a LaunchDaemon or the SCM nothing keeps an inherited stream
    /// (`observability.md` § Persistence & privacy). `/bin/echo` stands in for
    /// the MDA — it prints the MDA's argv to stdout and exits.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_child_output_reaches_the_supervisor_tracing() {
        use tracing_subscriber::prelude::*;
        let captured: Captured = Default::default();
        let _default = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(Collect(captured.clone())),
        );

        let bridge = temp_dir("capture");
        let nest = temp_dir("capture-nest");
        let mut sup = supervisor(bridge.clone(), nest.clone());
        sup.mda_exe = PathBuf::from("/bin/echo");
        let mut child = sup.spawn_at("0.0.0.0:8443").unwrap();
        child.wait().await.unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let line = loop {
            let found = captured
                .lock()
                .unwrap()
                .iter()
                .find(|(_, _, m)| m.contains("--keypair-file"))
                .cloned();
            if let Some(found) = found {
                break found;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child's stdout never reached tracing: {:?}",
                captured.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(line.0, tracing::Level::INFO);
        assert_eq!(line.1, MDA_LOG_TARGET);
        let _ = std::fs::remove_dir_all(&bridge);
        let _ = std::fs::remove_dir_all(&nest);
    }
}

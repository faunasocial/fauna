//! Mail-enable lifecycle: the nest half of the Stage 0 ↔ Stage 1 contract
//! (`docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first claim,
//! § Wire shapes). When the admin toggles `mail.enabled` via
//! `fauna.bridges.set_mail_enabled`, nest materializes the `/data/imap-enabled`
//! flag file (the durable signal Stage 0's s6 run-script gates on) and signals
//! the supervisor sidekick socket so the service comes up without a restart or
//! filesystem polling.
//!
//! The supervisor socket (`/run/fauna-supervisor.sock`) is created by the
//! Stage 0 packaging work and does not exist in the e2e
//! harness or on a dev box; the notify is therefore **best-effort** — a failed
//! connect is logged and swallowed, and the flag file remains the durable
//! enable signal. The flag-file write itself is load-bearing (Stage 0's
//! run-script blocks on it) and surfaces its error to the caller.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Cadence of the flag-vs-state reconciliation tick (`mail-bridge-lifecycle.md`
/// § Default-off on first claim — "nest's periodic-reconciliation tick (every
/// 60 s)").
pub const MAIL_ENABLE_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);

/// Well-known path of the supervisor sidekick socket inside the container /
/// on a systemd-deployed bare-metal nest (`/run` tmpfs, owned by the `fauna`
/// user). Nest writes line-framed JSON commands; the supervisor dispatches via
/// `s6-svc` / `systemctl`. Per `mail-bridge-lifecycle.md` § Wire shapes.
pub const SUPERVISOR_SOCKET: &str = "/run/fauna-supervisor.sock";

/// The role-enable flag-file names under the deployment data dir, re-exported
/// from [`fauna_deployment_flags`] — the zero-dependency crate that owns this
/// nest↔supervisor contract so the reading service shells can hold it without
/// depending on `fauna-nest`.
///
/// The nest is the **writer**: presence enables the role. The MDA's s6 run-script
/// gates on `imap-enabled OR caldav-enabled OR carddav-enabled OR webdav-enabled`
/// — the one MDA process hosts all four protocols, so any of the four flags keeps
/// it up, and the MDA then binds each protocol's listener per its own
/// `fetch_config` flag. CardDAV and WebDAV ride the SAME DAV listener as CalDAV
/// (no separate port).
///
/// Per `mail-bridge-lifecycle.md` § Default-off on first claim,
/// `caldav-server.md` § Independent enablement, and
/// `docs/goal/behavior/webdav-server.md` § Independent enablement.
pub use fauna_deployment_flags::{
    CALDAV_ENABLE_FLAG, CARDDAV_ENABLE_FLAG, MAIL_ENABLE_FLAG, WEBDAV_ENABLE_FLAG,
};

/// Flag-file name under the deployment data dir. Presence = the out-of-process
/// ATProto PDS bridge (role `atproto.pds`) is enabled — the ATProto-host twin of
/// [`MAIL_ENABLE_FLAG`]. The atproto bridge's s6 run-script gates on this flag
/// exactly as the mail MTA/MDA gate on their flags: down by default, brought up
/// when the admin enables Bluesky in a Fauna app. Unlike the four mail/DAV
/// flags this one guards a *single* process (there is no MTA/MDA-style role
/// split). Per `docs/goal/behavior/atproto-pds-bridge.md` § Enable UX +
/// `mail-bridge-lifecycle.md` § Default-off on first claim. (The client toggle
/// that writes it is S4; S1 lands the nest-side write mechanism + tests.)
pub const ATPROTO_ENABLE_FLAG: &str = "atproto-enabled";

/// Flag-file name under the deployment data dir carrying the admin-set CalDAV
/// listener **port** as decimal text (e.g. `9443`). Unlike [`CALDAV_ENABLE_FLAG`]
/// this is a *value* carrier, not a listener-gating presence flag: it never
/// decides whether the MDA starts. It exists for the **desktop-supervisor**
/// path, which can't call `fetch_config` (bridge-enrollment-only) and so can't
/// learn the admin's chosen port the way the Docker MDA does — it reads this
/// flag to re-pin the MDA's `caldav_listen_https` operator-hatch and rebind. The
/// Docker MDA ignores the flag (it reads `fetch_config.caldav_port` directly).
/// Absent ⇒ the desktop supervisor keeps its install-time default
/// ([`fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT`] = 8443). Per
/// `caldav-server.md` § Network exposure (Desktop / IP deployment).
///
/// Owned by [`fauna_deployment_flags`] with the rest of the set — the reading
/// supervisor needs the same name.
pub use fauna_deployment_flags::CALDAV_PORT_FLAG;

/// Flag-file name under the deployment data dir carrying the admin-set
/// **client-facing API serving port** as decimal text (e.g. `8443`). The exact
/// twin of [`CALDAV_PORT_FLAG`], but for the **nest's own** HTTPS listener (the
/// WS-RPC transport + the served SPA) rather than the MDA's CalDAV listener. A
/// *value* carrier, not a presence flag. It exists for the **desktop-supervisor**
/// path (`apps/fauna-windows/fauna-bridge-service`), which can't reach a live
/// `AppState` reload and — because the nest cannot hot-rebind its own
/// `TcpListener` — must **restart the nest** to apply a new port. The supervisor
/// reads this flag, restarts the nest on the new bind, and (manual user step on
/// desktop) the off-box firewall rule is updated for the chosen port. The Docker
/// entrypoint ignores the flag (the external port there is the SNI router +
/// compose port-map, realized by the provisioning orchestrator — the nest is
/// `FAUNA_FRONTED_BY_ROUTER` and the singleton is inert for its own bind). Absent
/// ⇒ the supervisor keeps its install-time default
/// ([`fauna_protocol::node_policy::DEFAULT_SERVING_PORT`] = 443). Per
/// `nest/common.md` § Serving ports.
///
/// Owned by [`fauna_deployment_flags`] with the rest of the set — the reading
/// supervisor (`fauna_nest_supervisor`) needs the same name.
pub use fauna_deployment_flags::SERVING_PORT_FLAG;

/// The MTA role service (SMTP MX 25 + submission 465/587). Email-only **and**
/// public-axis-only — up iff [`mta_should_run`] (a private home box never runs
/// the perimeter parser). Per `mail-bridge-lifecycle.md` § Multi-role
/// multi-process + `deployment-home-with-public-relay.md` § Plaintext-mode
/// behavior.
pub const MTA_SERVICE: &str = "fauna-mail-bridge-mta";

/// The MDA role service (IMAP 993/143 + CalDAV/CardDAV/WebDAV 443). Hosts all
/// four protocols, so up iff
/// `mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`
/// ([`reconcile_supervisor`]).
pub const MDA_SERVICE: &str = "fauna-mail-bridge-mda";

/// The ATProto PDS bridge role service (role `atproto.pds`). A single process
/// (no MTA/MDA-style role split), down by default and up iff atproto is enabled
/// ([`set_atproto_enabled`]). It is in the supervisor sidekick's fixed allowlist
/// (`cmd/fauna-supervisor/supervisor.go`). Per `atproto-pds-bridge.md`
/// § Architecture.
pub const ATPROTO_BRIDGE_SERVICE: &str = "fauna-atproto-bridge";

/// The per-role supervisor service names, in approval order. One process per
/// role per `mail-bridge-lifecycle.md` § Multi-role multi-process, so the
/// supervisor commands are role-suffixed (matches § Wire shapes' example;
/// the § Default-off singular example is reconciled to this).
pub const MAIL_BRIDGE_SERVICES: [&str; 2] = [MTA_SERVICE, MDA_SERVICE];

/// The line-framed JSON command nest writes to the supervisor socket. `action`
/// is `"up"` / `"down"`; `service` is one of [`MAIL_BRIDGE_SERVICES`]. Both are
/// internal constants (no untrusted input), so a plain `format!` is sufficient
/// and keeps the newline framing explicit. Per `mail-bridge-lifecycle.md`
/// § Wire shapes.
pub fn supervisor_command(action: &str, service: &str) -> String {
    format!("{{\"action\":\"{action}\",\"service\":\"{service}\"}}\n")
}

/// Resolve the deployment data dir from the configured db path (the dir that
/// holds the SQLite file — `/data` in the docker image). `None` when the db
/// path is empty (`:memory:` / `AppState::for_test`) or has no parent, in which
/// case the flag-file write is skipped.
pub fn data_dir_from_db_path(db_path: &str) -> Option<PathBuf> {
    if db_path.is_empty() {
        return None;
    }
    Path::new(db_path).parent().and_then(|p| {
        if p.as_os_str().is_empty() {
            None
        } else {
            Some(p.to_path_buf())
        }
    })
}

/// Create (`enabled`) or remove (`!enabled`) `{data_dir}/{flag_name}`.
/// Idempotent both ways: creating an existing flag and removing an absent one
/// both succeed. The file is mode 0600 on unix (owner-only — it is sensitive
/// deployment state, never the admin's input). The shared primitive behind
/// [`set_mail_enable_flag`] (`imap-enabled`) and [`set_caldav_enable_flag`]
/// (`caldav-enabled`). Per `mail-bridge-lifecycle.md` § Default-off on first
/// claim + § Architectural rules.
pub fn set_enable_flag(data_dir: &Path, flag_name: &str, enabled: bool) -> std::io::Result<()> {
    let path = data_dir.join(flag_name);
    if enabled {
        use std::fs::OpenOptions;
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&path)?;
        Ok(())
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Create/remove the `{data_dir}/imap-enabled` mail-enable flag. Thin wrapper
/// over [`set_enable_flag`].
pub fn set_mail_enable_flag(data_dir: &Path, enabled: bool) -> std::io::Result<()> {
    set_enable_flag(data_dir, MAIL_ENABLE_FLAG, enabled)
}

/// Create/remove the `{data_dir}/caldav-enabled` CalDAV-enable flag. Thin
/// wrapper over [`set_enable_flag`].
pub fn set_caldav_enable_flag(data_dir: &Path, enabled: bool) -> std::io::Result<()> {
    set_enable_flag(data_dir, CALDAV_ENABLE_FLAG, enabled)
}

/// Create/remove the `{data_dir}/carddav-enabled` CardDAV-enable flag. Thin
/// wrapper over [`set_enable_flag`]; the contacts twin of
/// [`set_caldav_enable_flag`].
pub fn set_carddav_enable_flag(data_dir: &Path, enabled: bool) -> std::io::Result<()> {
    set_enable_flag(data_dir, CARDDAV_ENABLE_FLAG, enabled)
}

/// Create/remove the `{data_dir}/webdav-enabled` WebDAV-enable flag. Thin
/// wrapper over [`set_enable_flag`]; the files twin of
/// [`set_carddav_enable_flag`].
pub fn set_webdav_enable_flag(data_dir: &Path, enabled: bool) -> std::io::Result<()> {
    set_enable_flag(data_dir, WEBDAV_ENABLE_FLAG, enabled)
}

/// Create/remove the `{data_dir}/atproto-enabled` flag. Thin wrapper over
/// [`set_enable_flag`]; the ATProto-PDS twin of [`set_mail_enable_flag`].
pub fn set_atproto_enable_flag(data_dir: &Path, enabled: bool) -> std::io::Result<()> {
    set_enable_flag(data_dir, ATPROTO_ENABLE_FLAG, enabled)
}

/// The nest half of the ATProto-PDS enable mechanism (the atproto twin of the
/// `set_mail_enabled` flag-write + supervisor-`up`; `atproto-pds-bridge.md`
/// § Enable UX — "First enable does double duty: boot the nest PDS bridge …").
///
/// Materialize (`enabled`) or remove (`!enabled`) the durable
/// `/data/atproto-enabled` flag the bridge's s6 run-script gates on, then
/// best-effort signal the supervisor sidekick socket to bring the single
/// `fauna-atproto-bridge` service up/down without a restart or filesystem
/// polling. The flag write is **load-bearing** and its error surfaces to the
/// caller; the supervisor notify is **best-effort** (the socket is absent in the
/// e2e harness / on a dev box, and the flag is the durable signal the run-script
/// picks up at the next boot regardless — see [`notify_service`]).
///
/// Unlike `set_mail_enabled` there is no NAT-axis gate (the PDS bridge opens no
/// SMTP perimeter — it only publishes public posts outward) and no role split
/// (one process). The **client toggle** that calls this — plus the persisted
/// `atproto_enabled` DB state and its boot/periodic flag-vs-state reconcile (the
/// mail pattern's [`spawn_mail_enable_reconciliation`]) — is **S4**; S1 lands
/// this mechanism + tests. DID minting is layered on top by S2/S4, not here.
pub async fn set_atproto_enabled(data_dir: &Path, enabled: bool) -> std::io::Result<()> {
    set_atproto_enable_flag(data_dir, enabled)?;
    notify_service(ATPROTO_BRIDGE_SERVICE, enabled).await;
    Ok(())
}

/// Best-effort: signal the supervisor sidekick socket to bring one role service
/// up or down. Connect failures (socket absent in the e2e harness / on a dev
/// box, or Stage 0 packaging not yet deployed) are logged at debug and
/// swallowed — the flag file is the durable signal; the run-script picks the
/// service up off the flag at the next boot even if this notify no-ops. Per
/// `mail-bridge-lifecycle.md` § Why the supervisor sidekick socket.
async fn notify_service(service: &str, up: bool) {
    let action = if up { "up" } else { "down" };
    if let Err(e) = send_supervisor_command(SUPERVISOR_SOCKET, action, service).await {
        tracing::debug!(
            target: "mail_enable",
            socket = SUPERVISOR_SOCKET,
            action,
            service,
            error = %e,
            "supervisor notify skipped (socket unavailable — flag file is the durable signal)"
        );
    }
}

/// Whether the MTA (SMTP MX + submission) role service should run. The MTA
/// rides email **and** only the public NAT axis: a private home box never
/// terminates SMTP — inbound arrives over federation from its paired public
/// relay and outbound submission/MX delivery stay on that relay — so the
/// perimeter parser stays **down** on the private axis even when the admin
/// enables mail to serve IMAP/CalDAV from the box's own MDA. This makes the
/// "Perimeter parser process: NOT started" row of
/// `deployment-home-with-public-relay.md` § Plaintext-mode behavior an
/// **enforced** property of `set_mail_enabled` (it was previously only implicit
/// — "the installer deploys no MTA"). It reuses the existing `NodeMode` axis;
/// there is no "VPS-frontend mode" flag (§ Don't do these). NB: this is the
/// *runtime* half — the docker image's MTA s6 run-script carries the matching
/// `FAUNA_MODE=private` re-down so the service also stays down across a restart
/// where `imap-enabled` is present (the durable flag gate).
pub fn mta_should_run(node_mode: crate::config::NodeMode, mail_enabled: bool) -> bool {
    mail_enabled && node_mode != crate::config::NodeMode::Private
}

/// Whether the MDA (IMAP + CalDAV + CardDAV + WebDAV) role service should run.
/// The one MDA process hosts all four protocols, so it runs if **any** is
/// enabled — on **both** NAT axes: the private home box's MDA is exactly what
/// serves the relayed mail to LAN MUAs over IMAP/CalDAV/CardDAV/WebDAV
/// (`deployment-home-with-public-relay.md` § MUA reach). WebDAV rides the same
/// DAV listener as CalDAV/CardDAV, but its enable flag is independent — a
/// files-only deployment (mail + calendar + contacts off) still needs the MDA up
/// (`docs/goal/behavior/webdav-server.md` § Independent enablement).
pub fn mda_should_run(
    mail_enabled: bool,
    caldav_enabled: bool,
    carddav_enabled: bool,
    webdav_enabled: bool,
) -> bool {
    mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled
}

/// Bring each mail-bridge role service to its desired state given the NAT axis
/// and the four enable toggles. The MTA is up iff [`mta_should_run`]
/// (email-only **and** public axis); the MDA is up iff [`mda_should_run`]
/// (`mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`, both
/// axes). Best-effort (see [`notify_service`]) — the `imap-enabled` /
/// `caldav-enabled` / `carddav-enabled` / `webdav-enabled` flag files are the
/// durable signal the s6 run-scripts gate on at (re)start, and the MDA binds
/// each protocol's listener per its own `fetch_config` flag. Per
/// `caldav-server.md` § Independent enablement +
/// `docs/goal/behavior/webdav-server.md` § Independent enablement.
/// The four service toggles, each resolved to the value the supervisor acts on.
///
/// One shape rather than four hand-copied reads, because four copies of a list
/// is three chances to forget one — and this list had already been forgotten
/// three times: the DAV enable handlers each resolved an unset **mail** toggle
/// to `true` long after the Stage-5 flip made unset mean OFF, each under a
/// comment claiming to match `fetch_config`, which by then routed through
/// [`crate::db::CacheDb::effective_mail_enabled`] and disagreed
/// (`mail-bridge-lifecycle.md` § Default-off on first claim).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServiceToggles {
    pub mail: bool,
    pub caldav: bool,
    pub carddav: bool,
    pub webdav: bool,
}

/// Read the four service toggles as the supervisor should see them.
///
/// Two independent defaults, and they are not the same rule:
///
/// - **mail** is owned by [`crate::db::CacheDb::effective_mail_enabled`] —
///   unset means **OFF**. That is the single owner of the Stage-5 default-off
///   decision; nothing here may re-derive it.
/// - **caldav / carddav / webdav** are independent axes that *follow mail* when
///   unset, so a mail-enabled box serves DAV out of the box without a second
///   opt-in (`caldav-server.md` / `webdav-server.md` § Independent enablement).
///   An explicit `false` is never overridden by that inheritance.
///
/// Callers reconciling one axis they are *about to* set override that field on
/// the result rather than re-reading it — the write may not have landed in the
/// same snapshot, and the caller already knows the value.
pub async fn effective_service_toggles(db: &crate::db::CacheDb) -> anyhow::Result<ServiceToggles> {
    let mail = db.effective_mail_enabled().await?;
    Ok(ServiceToggles {
        mail,
        caldav: db.get_caldav_enabled().await?.unwrap_or(mail),
        carddav: db.get_carddav_enabled().await?.unwrap_or(mail),
        webdav: db.get_webdav_enabled().await?.unwrap_or(mail),
    })
}

pub async fn reconcile_supervisor(
    node_mode: crate::config::NodeMode,
    mail_enabled: bool,
    caldav_enabled: bool,
    carddav_enabled: bool,
    webdav_enabled: bool,
) {
    notify_service(MTA_SERVICE, mta_should_run(node_mode, mail_enabled)).await;
    notify_service(
        MDA_SERVICE,
        mda_should_run(
            mail_enabled,
            caldav_enabled,
            carddav_enabled,
            webdav_enabled,
        ),
    )
    .await;
}

/// One reconciliation pass: compare the persisted `mail_enabled` toggle against
/// the on-disk flag file. When they diverge — e.g. the flag was hand-edited,
/// which `mail-bridge-lifecycle.md` § Architectural rules forbids ("the
/// flag file is nest's output, never the admin's input") — nest's persisted
/// state wins: log `bridge_flag_file_diverged_from_state` and re-assert the
/// flag to match. Returns `Some(enabled)` when it re-asserted, `None` when the
/// flag already matched the toggle (or the toggle is unset, so there is no
/// authoritative state to reconcile against on a freshly-claimed nest).
pub async fn reconcile_mail_enable_flag_once(
    db: &crate::db::CacheDb,
    data_dir: &Path,
) -> std::io::Result<Option<bool>> {
    let enabled = match db.get_mail_enabled().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "mail_enable",
                error = %format!("{e:#}"),
                "mail-enable reconcile skipped — get_mail_enabled failed"
            );
            return Ok(None);
        }
    };
    reconcile_enable_flag(enabled, data_dir, MAIL_ENABLE_FLAG, "mail-enable")
}

/// Write `{data_dir}/caldav-port` carrying `port` as decimal text. Owner-only on
/// unix (0600 — it is nest's output, never the admin's input, like the enable
/// flags). Overwrites any previous value (latest write wins); there is no
/// "clear" — the admin only ever sets the port to a valid value. The
/// **desktop-supervisor** reads this to re-pin the MDA's CalDAV listener; the
/// Docker MDA ignores it. Per [`CALDAV_PORT_FLAG`].
pub fn set_caldav_port_flag(data_dir: &Path, port: u16) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let path = data_dir.join(CALDAV_PORT_FLAG);
    let mut opts = OpenOptions::new();
    // Truncate so a shorter new value never leaves stale trailing bytes from a
    // longer previous one (e.g. 9443 → 443).
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    f.write_all(port.to_string().as_bytes())
}

/// Write `{data_dir}/serving-port` carrying `port` as decimal text. The exact
/// twin of [`set_caldav_port_flag`] for the nest's own client-facing listener.
/// Owner-only on unix (0600 — nest's output, never admin input). Truncating
/// overwrite so a shorter new value leaves no stale trailing bytes. The
/// **desktop-supervisor** reads this to restart the nest on the new bind; the
/// Docker entrypoint ignores it. Per [`SERVING_PORT_FLAG`].
pub fn set_serving_port_flag(data_dir: &Path, port: u16) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let path = data_dir.join(SERVING_PORT_FLAG);
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    f.write_all(port.to_string().as_bytes())
}

/// Re-assert `{data_dir}/serving-port` from the persisted `serving_port`
/// singleton — the client-facing twin of [`reconcile_caldav_port_flag_once`].
/// `None` (admin never set a port) ⇒ leave the flag untouched (the supervisor
/// keeps its install default) and return `None`. When set and the on-disk flag
/// diverges (missing, malformed, or a different number), nest state wins:
/// re-write the flag and return `Some(port)`.
pub async fn reconcile_serving_port_flag_once(
    db: &crate::db::CacheDb,
    data_dir: &Path,
) -> std::io::Result<Option<u16>> {
    let port = match db.get_serving_port().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "mail_enable",
                error = %format!("{e:#}"),
                "serving-port reconcile skipped — get_serving_port failed"
            );
            return Ok(None);
        }
    };
    // Admin never set a port ⇒ nothing authoritative to assert; leave the flag as
    // it is (the supervisor keeps its install default).
    let port = match port {
        Some(p) => p,
        None => return Ok(None),
    };
    let current = std::fs::read_to_string(data_dir.join(SERVING_PORT_FLAG))
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok());
    if current == Some(port) {
        return Ok(None);
    }
    tracing::warn!(
        target: "mail_enable",
        event = "bridge_flag_file_diverged_from_state",
        flag = SERVING_PORT_FLAG,
        state_port = port,
        flag_port = ?current,
        "serving-port flag file diverged from persisted state — re-asserting (the flag is nest's output, not admin input)"
    );
    set_serving_port_flag(data_dir, port)?;
    Ok(Some(port))
}

/// Re-assert `{data_dir}/caldav-port` from the persisted `caldav_port` singleton.
/// `None` (admin never set a port) ⇒ leave the flag untouched (the supervisor
/// falls back to its install default) and return `None`. When set and the
/// on-disk flag diverges (missing, malformed, or a different number), nest state
/// wins: re-write the flag and return `Some(port)`. The value twin of
/// [`reconcile_caldav_enable_flag_once`].
pub async fn reconcile_caldav_port_flag_once(
    db: &crate::db::CacheDb,
    data_dir: &Path,
) -> std::io::Result<Option<u16>> {
    let port = match db.get_caldav_port().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "mail_enable",
                error = %format!("{e:#}"),
                "caldav-port reconcile skipped — get_caldav_port failed"
            );
            return Ok(None);
        }
    };
    // Admin never set a port ⇒ nothing authoritative to assert; leave the flag as
    // it is (the supervisor keeps its install default).
    let port = match port {
        Some(p) => p,
        None => return Ok(None),
    };
    let current = std::fs::read_to_string(data_dir.join(CALDAV_PORT_FLAG))
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok());
    if current == Some(port) {
        return Ok(None);
    }
    tracing::warn!(
        target: "mail_enable",
        event = "bridge_flag_file_diverged_from_state",
        flag = CALDAV_PORT_FLAG,
        state_port = port,
        flag_port = ?current,
        "caldav-port flag file diverged from persisted state — re-asserting (the flag is nest's output, not admin input)"
    );
    set_caldav_port_flag(data_dir, port)?;
    Ok(Some(port))
}

/// CalDAV twin of [`reconcile_mail_enable_flag_once`]: re-assert
/// `{data_dir}/caldav-enabled` from the persisted `caldav_enabled` toggle.
pub async fn reconcile_caldav_enable_flag_once(
    db: &crate::db::CacheDb,
    data_dir: &Path,
) -> std::io::Result<Option<bool>> {
    let enabled = match db.get_caldav_enabled().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "mail_enable",
                error = %format!("{e:#}"),
                "caldav-enable reconcile skipped — get_caldav_enabled failed"
            );
            return Ok(None);
        }
    };
    reconcile_enable_flag(enabled, data_dir, CALDAV_ENABLE_FLAG, "caldav-enable")
}

/// CardDAV twin of [`reconcile_caldav_enable_flag_once`]: re-assert
/// `{data_dir}/carddav-enabled` from the persisted `carddav_enabled` toggle.
pub async fn reconcile_carddav_enable_flag_once(
    db: &crate::db::CacheDb,
    data_dir: &Path,
) -> std::io::Result<Option<bool>> {
    let enabled = match db.get_carddav_enabled().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "mail_enable",
                error = %format!("{e:#}"),
                "carddav-enable reconcile skipped — get_carddav_enabled failed"
            );
            return Ok(None);
        }
    };
    reconcile_enable_flag(enabled, data_dir, CARDDAV_ENABLE_FLAG, "carddav-enable")
}

/// WebDAV twin of [`reconcile_carddav_enable_flag_once`]: re-assert
/// `{data_dir}/webdav-enabled` from the persisted `webdav_enabled` toggle.
pub async fn reconcile_webdav_enable_flag_once(
    db: &crate::db::CacheDb,
    data_dir: &Path,
) -> std::io::Result<Option<bool>> {
    let enabled = match db.get_webdav_enabled().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "mail_enable",
                error = %format!("{e:#}"),
                "webdav-enable reconcile skipped — get_webdav_enabled failed"
            );
            return Ok(None);
        }
    };
    reconcile_enable_flag(enabled, data_dir, WEBDAV_ENABLE_FLAG, "webdav-enable")
}

/// Shared flag-vs-state reconcile core. `enabled` is the persisted toggle
/// (`None` ⇒ admin has never set it, so there is nothing authoritative to
/// reconcile against — leave the flag untouched, the `fetch_config` derived
/// fallback owns read). When set and the on-disk flag diverges (e.g. it was
/// hand-edited, which `mail-bridge-lifecycle.md` § Architectural
/// rules forbids — "the flag file is nest's output, never the admin's input"),
/// persisted state wins: log `bridge_flag_file_diverged_from_state` and
/// re-assert. Returns `Some(enabled)` when it re-asserted, `None` otherwise.
fn reconcile_enable_flag(
    enabled: Option<bool>,
    data_dir: &Path,
    flag_name: &str,
    label: &str,
) -> std::io::Result<Option<bool>> {
    let enabled = match enabled {
        Some(e) => e,
        None => return Ok(None),
    };
    let flag_present = data_dir.join(flag_name).exists();
    if flag_present == enabled {
        return Ok(None);
    }
    tracing::warn!(
        target: "mail_enable",
        event = "bridge_flag_file_diverged_from_state",
        flag = flag_name,
        state_enabled = enabled,
        flag_present,
        "{label} flag file diverged from persisted state — re-asserting (the flag is nest's output, not admin input)"
    );
    set_enable_flag(data_dir, flag_name, enabled)?;
    Ok(Some(enabled))
}

/// Spawn the boot-plus-periodic flag-vs-state reconciliation. The first pass
/// runs immediately at boot (healing a torn `imap-enabled` flag a crash left
/// between the two `set_mail_enabled_handler` writes), then it re-runs every
/// `interval`. Each pass calls [`reconcile_mail_enable_flag_once`]; a divergence
/// re-asserts the flag file from nest's persisted toggle and logs
/// `bridge_flag_file_diverged_from_state`. Spawned only when the deployment has a
/// data dir (the real server) — the in-memory test/dev path has no flag file to
/// reconcile. Per `mail-bridge-lifecycle.md` § Default-off on first claim and
/// common.md § Client-state recoverability (single decision point + boot
/// reconcile).
pub fn spawn_mail_enable_reconciliation(
    db: std::sync::Arc<crate::db::CacheDb>,
    data_dir: PathBuf,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        // `interval`'s first tick fires immediately, so this reconciles at boot
        // (re-asserting the flag from persisted state, healing a flag a crash
        // tore mid-write) then every `interval` — the boot reconcile of
        // `common.md` § Client-state recoverability.
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = reconcile_mail_enable_flag_once(&db, &data_dir).await {
                tracing::warn!(
                    target: "mail_enable",
                    error = %e,
                    "mail-enable reconcile: flag re-assert failed"
                );
            }
        }
    })
}

/// CalDAV twin of [`spawn_mail_enable_reconciliation`]: periodically re-assert
/// `{data_dir}/caldav-enabled` from the persisted `caldav_enabled` toggle.
pub fn spawn_caldav_enable_reconciliation(
    db: std::sync::Arc<crate::db::CacheDb>,
    data_dir: PathBuf,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        // `interval`'s first tick fires immediately, so this reconciles at boot
        // (re-asserting the flag from persisted state, healing a flag a crash
        // tore mid-write) then every `interval` — the boot reconcile of
        // `common.md` § Client-state recoverability.
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = reconcile_caldav_enable_flag_once(&db, &data_dir).await {
                tracing::warn!(
                    target: "mail_enable",
                    error = %e,
                    "caldav-enable reconcile: flag re-assert failed"
                );
            }
        }
    })
}

/// CardDAV twin of [`spawn_caldav_enable_reconciliation`]: periodically re-assert
/// `{data_dir}/carddav-enabled` from the persisted `carddav_enabled` toggle.
pub fn spawn_carddav_enable_reconciliation(
    db: std::sync::Arc<crate::db::CacheDb>,
    data_dir: PathBuf,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        // `interval`'s first tick fires immediately, so this reconciles at boot
        // (re-asserting the flag from persisted state, healing a flag a crash
        // tore mid-write) then every `interval` — the boot reconcile of
        // `common.md` § Client-state recoverability.
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = reconcile_carddav_enable_flag_once(&db, &data_dir).await {
                tracing::warn!(
                    target: "mail_enable",
                    error = %e,
                    "carddav-enable reconcile: flag re-assert failed"
                );
            }
        }
    })
}

/// WebDAV twin of [`spawn_carddav_enable_reconciliation`]: periodically
/// re-assert `{data_dir}/webdav-enabled` from the persisted `webdav_enabled`
/// toggle.
pub fn spawn_webdav_enable_reconciliation(
    db: std::sync::Arc<crate::db::CacheDb>,
    data_dir: PathBuf,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        // `interval`'s first tick fires immediately, so this reconciles at boot
        // (re-asserting the flag from persisted state, healing a flag a crash
        // tore mid-write) then every `interval` — the boot reconcile of
        // `common.md` § Client-state recoverability.
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = reconcile_webdav_enable_flag_once(&db, &data_dir).await {
                tracing::warn!(
                    target: "mail_enable",
                    error = %e,
                    "webdav-enable reconcile: flag re-assert failed"
                );
            }
        }
    })
}

/// Value twin of [`spawn_caldav_enable_reconciliation`]: periodically re-assert
/// `{data_dir}/caldav-port` from the persisted `caldav_port` singleton, so a
/// desktop supervisor that hand-loses or staleness-drifts the flag re-converges
/// on the admin's chosen port. Per [`CALDAV_PORT_FLAG`].
pub fn spawn_caldav_port_reconciliation(
    db: std::sync::Arc<crate::db::CacheDb>,
    data_dir: PathBuf,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        // `interval`'s first tick fires immediately, so this reconciles at boot
        // (re-asserting the flag from persisted state, healing a flag a crash
        // tore mid-write) then every `interval` — the boot reconcile of
        // `common.md` § Client-state recoverability.
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = reconcile_caldav_port_flag_once(&db, &data_dir).await {
                tracing::warn!(
                    target: "mail_enable",
                    error = %e,
                    "caldav-port reconcile: flag re-assert failed"
                );
            }
        }
    })
}

/// Client-facing twin of [`spawn_caldav_port_reconciliation`]: periodically
/// re-assert `{data_dir}/serving-port` from the persisted `serving_port`
/// singleton, so a desktop supervisor that hand-loses or staleness-drifts the
/// flag re-converges on the admin's chosen port. Per [`SERVING_PORT_FLAG`].
pub fn spawn_serving_port_reconciliation(
    db: std::sync::Arc<crate::db::CacheDb>,
    data_dir: PathBuf,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        // `interval`'s first tick fires immediately, so this reconciles at boot
        // (re-asserting the flag from persisted state, healing a flag a crash
        // tore mid-write) then every `interval` — the boot reconcile of
        // `common.md` § Client-state recoverability.
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = reconcile_serving_port_flag_once(&db, &data_dir).await {
                tracing::warn!(
                    target: "mail_enable",
                    error = %e,
                    "serving-port reconcile: flag re-assert failed"
                );
            }
        }
    })
}

#[cfg(unix)]
async fn send_supervisor_command(
    socket_path: &str,
    action: &str,
    service: &str,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::net::UnixStream::connect(socket_path).await?;
    stream
        .write_all(supervisor_command(action, service).as_bytes())
        .await?;
    stream.flush().await
}

#[cfg(not(unix))]
async fn send_supervisor_command(
    _socket_path: &str,
    _action: &str,
    _service: &str,
) -> std::io::Result<()> {
    // The supervisor socket is a unix-domain socket; on non-unix nest builds
    // (none ship today) the notify is a no-op and the flag file stands alone.
    Ok(())
}

/// Auto-register `domain` as a `mail_domains` row — best-effort + idempotent.
///
/// Shared by the two provisioning triggers, so the row a claim writes and the
/// row the enable-time net writes are indistinguishable:
/// - **claim time** (`claim_admin_core`): the wizard handle carries the mail
///   domain (`alice@fauna.test`); the nest stores only the bare local-part
///   (its `validate_handle` rejects `@`), so the client forwards the `@domain`
///   suffix as the claim's `mail_domain` and we register it — the handle is a
///   routable address out of the box, no manual admin-dns "add domain" step.
/// - **enable / boot time** ([`ensure_primary_mail_domain`]): the safety net for
///   a claim that carried no `mail_domain` (e.g. a re-claim of a factory-reset
///   box that skips the wizard-handle entry).
///
/// The domain is the deployment's, not the user's ("the handle IS the email" —
/// `docs/goal/behavior/mail-aliases.md`); it appears on admin-dns where the
/// admin can still remove it (config stays client-set). Best-effort: a failure
/// leaves the admin claimed/enabled and the domain addable later via admin-dns.
/// Idempotent (skips an already-active domain). `is_primary` is auto-set (first
/// active domain = primary) and the MTA-STS defaults match the admin-dns "add
/// domain" path (`testing` / `expand_primary`,
/// `fauna_client_mail_settings::{DEFAULT_MTA_STS_MODE, DEFAULT_CERT_MODE}`), so
/// an auto-registered domain is indistinguishable from a UI-registered one
/// (`add_local_domain_handler`). No bridge `config_changed` notify here — no
/// bridge is approved at claim time, and the enable handler owns the supervisor
/// reconcile + config-changed push; the freshly-(re)launched MTA reads the
/// domain fresh at cold-boot. Per `mail-multidomain.md` § The primary domain.
pub async fn ensure_mail_domain_registered(state: &crate::routes::AppState, domain: &str) {
    let domain = domain.trim().to_ascii_lowercase();
    if domain.is_empty() {
        return;
    }
    match state.db.lookup_active_mail_domain(&domain).await {
        Ok(Some(_)) => return, // already active — idempotent no-op
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("auto mail-domain: lookup_active_mail_domain({domain}): {e}");
            return;
        }
    }
    let is_primary = match state.db.list_active_mail_domains().await {
        Ok(list) => list.is_empty(),
        Err(e) => {
            tracing::warn!("auto mail-domain: list_active_mail_domains: {e}");
            return;
        }
    };
    match state
        .db
        .add_mail_domain(&domain, is_primary, "testing", "expand_primary", None, None)
        .await
    {
        Ok(_) => {
            tracing::info!("auto-registered mail domain {domain} (primary={is_primary})");
            // The primary `mail_domains` row IS the deployment identity: when this
            // registration became the primary (claim, admin add-first-domain, or the
            // boot safety net), point the sync identity cache at it + self-heal TLS,
            // so `handle_domain()` / discovery / web / the ACME apex follow the new
            // domain with no restart (single source of truth — no separate identity
            // store; `identity_domain_core`).
            if is_primary {
                crate::identity_domain_core::apply_primary_identity(state, &domain);
            }
        }
        Err(e) => tracing::warn!("auto mail-domain: add_mail_domain({domain}): {e}"),
    }
}

/// Safety net: ensure the deployment has a primary mail domain whenever mail is
/// enabled. **No-op unless `mail_domains` is empty AND the nest's domain is a
/// real (non-loopback/IP/`.local`) domain.**
///
/// Fired from two triggers, so a real-domain box always ends up with a working
/// primary mail domain regardless of how it was (re-)claimed:
/// - **enable transition** — `set_mail_enabled(true)` calls it (before the
///   supervisor brings the freshly-cold-booting bridge up), so a normal
///   claim→enable flow provisions the domain.
/// - **nest boot** — `lib.rs` startup calls it (before the WS server accepts
///   bridge connections) when `mail_enabled` is already true, so a box that
///   booted enabled-but-domainless self-heals: the in-container MTA/MDA read a
///   non-empty `local_domains` on their cold-boot `fetch_config` and bind. This
///   matters because an idling-on-empty bridge does not re-poll — it only
///   re-reads `mail_domains` on a process cold-boot (`mda.go`/`mta.go` block on
///   `<-ctx.Done()` when `LocalDomains == 0`), so the domain must exist *before*
///   the bridge's first fetch.
///
/// Why it's needed: the only client-reachable path that writes a `mail_domains`
/// row is claim-time auto-register ([`ensure_mail_domain_registered`], fired iff
/// the claim carried a `mail_domain`). A re-claim of a factory-reset box may not
/// carry one — the client re-claiming a box it already knows can skip the
/// wizard-handle entry — leaving `mail_domains` empty. An empty table means the
/// bridge's `local_domains` projection **and** the ACME cert SAN set are both
/// empty, so the MTA/MDA bind nothing and mail never serves (there is no
/// post-claim `add_local_domain` client affordance yet —
/// `mail-bridge-lifecycle.md` § Implementation status). It registers the nest's
/// own domain ([`crate::routes::AppState::handle_domain`]) as the primary — the
/// works-out-of-the-box default (mail domain == box domain on a single-box
/// deploy; product invariant: works out-of-the-box).
///
/// Gated to a **real** domain via the same loopback/IP predicate the onboarding
/// enable-email checkbox default uses
/// (`resolve_handle_domain(d).is_public_dns_name`,
/// `fauna-onboarding-machine` § `default_enable_email_for_handle`): a `localhost`
/// / `*.localhost` / IP-literal / `.local` nest skips it (mail needs real DNS +
/// public TLS, so there is nothing to provision). **Composes** with claim-time
/// registration: it fires only when nothing is registered, so a user who chose a
/// *different* mail domain at claim is never overridden. Best-effort (mirrors
/// [`ensure_mail_domain_registered`]). Per `mail-bridge-lifecycle.md`
/// § Default-off on first claim + `mail-multidomain.md` § The primary domain.
pub async fn ensure_primary_mail_domain(state: &crate::routes::AppState) {
    // Only act when nothing is registered yet — composes with claim-time
    // registration (which handles a user choosing a different mail domain).
    match state.db.list_active_mail_domains().await {
        Ok(list) if !list.is_empty() => return,
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("enable-mail auto-domain: list_active_mail_domains: {e}");
            return;
        }
    }
    let domain = state.handle_domain();
    if !fauna_provisioning::probe::resolve_handle_domain(&domain).is_public_dns_name {
        tracing::info!(
            "mail auto-domain: nest domain {domain:?} is a loopback/IP/.local target — skipping \
             automatic primary mail-domain provisioning (mail needs a real domain with DNS)"
        );
        return;
    }
    tracing::info!(
        "mail auto-domain: no mail domain registered — auto-provisioning nest domain {domain:?} as primary"
    );
    ensure_mail_domain_registered(state, &domain).await;
}

/// Safety net: ensure every **admin actor** (the box claimer + any co-admin) has
/// its canonical `<handle-localpart>@<primary-domain>` exact alias, so the admin
/// can log in to — and receive mail at — its own handle address
/// (`docs/goal/behavior/mail-aliases.md` § Kind 1 — Exact).
///
/// **Why it's needed.** A *regular* user's canonical exact alias is written when
/// *they* call `fauna.bridges.provision_recipient_mls_pubkey` (the per-user
/// enable-mail path → [`crate::bridge_routing_handlers::ensure_canonical_handle_alias`]).
/// The **admin never travels that path**: it claims via `claim_admin` (which only
/// sets the bare handle in `users`, no `account_aliases` row) and toggles mail
/// with `fauna.bridges.set_mail_enabled`. So without this net the admin's own
/// `<handle>@<domain>` has no exact alias row, and the AUTH login resolver
/// `validate_recipient` (exact-only — `mail-aliases.md:423` "a login must resolve
/// an exact canonical identity, never an alias") rejects it `no such recipient`,
/// breaking IMAP / CalDAV / submission login for the box admin on its own
/// address (the symptom on a re-claimed factory-reset example.com).
///
/// Fired from the same two triggers as [`ensure_primary_mail_domain`] — the
/// `set_mail_enabled(true)` enable transition and nest boot — and ordered *after*
/// it, so a servable primary domain exists before the alias is written.
/// Best-effort + idempotent: [`crate::bridge_routing_handlers::ensure_canonical_handle_alias`]
/// no-ops when no mail domain is registered yet or the actor has no handle, and
/// never clobbers a `(local_domain, pattern)` another actor already owns. Unlike
/// [`ensure_primary_mail_domain`] it is **not** gated on an empty `mail_domains` —
/// it must also backfill the alias on a box whose domain was already registered
/// (a claim that carried a `mail_domain`). Per `mail-aliases.md` § Kind 1 — Exact.
pub async fn ensure_admin_recipient_aliases(state: &crate::routes::AppState) {
    let admins = match state.db.list_admin_actors().await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("admin recipient alias: list_admin_actors failed: {e}");
            return;
        }
    };
    for (actor, _added_at) in admins {
        let actor: [u8; 32] = match actor.as_slice().try_into() {
            Ok(a) => a,
            Err(_) => {
                tracing::warn!("admin recipient alias: admin actor_id is not 32 bytes — skipping");
                continue;
            }
        };
        if let Err(e) =
            crate::bridge_routing_handlers::ensure_canonical_handle_alias(state, &actor).await
        {
            tracing::warn!(
                "admin recipient alias: ensure_canonical_handle_alias({}) failed: {e:?}",
                hex::encode(actor)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-test unique tempdir under the OS temp dir (no `tempfile` dep). Best-
    /// effort cleanup on drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("mail-enable-test-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TmpDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn supervisor_command_is_line_framed_json() {
        assert_eq!(
            supervisor_command("up", "fauna-mail-bridge-mta"),
            "{\"action\":\"up\",\"service\":\"fauna-mail-bridge-mta\"}\n"
        );
        assert_eq!(
            supervisor_command("down", "fauna-mail-bridge-mda"),
            "{\"action\":\"down\",\"service\":\"fauna-mail-bridge-mda\"}\n"
        );
    }

    #[test]
    fn data_dir_from_db_path_handles_empty_and_relative() {
        assert_eq!(data_dir_from_db_path(""), None);
        assert_eq!(
            data_dir_from_db_path("/data/fauna.db"),
            Some(PathBuf::from("/data"))
        );
        // A bare filename has an empty parent → no data dir.
        assert_eq!(data_dir_from_db_path("fauna.db"), None);
    }

    #[test]
    fn set_flag_is_idempotent_both_ways() {
        let tmp = TmpDir::new();
        let flag = tmp.path().join(MAIL_ENABLE_FLAG);

        // Disable on a fresh dir: idempotent no-op.
        set_mail_enable_flag(tmp.path(), false).unwrap();
        assert!(!flag.exists());

        // Enable creates it; enabling again is fine.
        set_mail_enable_flag(tmp.path(), true).unwrap();
        assert!(flag.exists());
        set_mail_enable_flag(tmp.path(), true).unwrap();
        assert!(flag.exists());

        // Mode is owner-only on unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&flag).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "flag file must be owner-only");
        }

        // Disable removes it; disabling again is fine.
        set_mail_enable_flag(tmp.path(), false).unwrap();
        assert!(!flag.exists());
        set_mail_enable_flag(tmp.path(), false).unwrap();
        assert!(!flag.exists());
    }

    #[tokio::test]
    async fn reconcile_supervisor_is_best_effort_when_socket_absent() {
        use crate::config::NodeMode;
        // No socket at the well-known path on the test box → must not panic /
        // error; the flag files are the durable signal. Exercise a spread of
        // (mail, caldav, carddav, webdav) combinations on both NAT axes.
        for mode in [NodeMode::Public, NodeMode::Private] {
            reconcile_supervisor(mode, true, true, true, true).await;
            reconcile_supervisor(mode, true, false, false, false).await;
            reconcile_supervisor(mode, false, true, false, false).await;
            reconcile_supervisor(mode, false, false, true, false).await;
            reconcile_supervisor(mode, false, false, false, true).await;
            reconcile_supervisor(mode, false, false, false, false).await;
        }
    }

    #[test]
    fn mta_stays_down_on_the_private_axis_even_when_mail_enabled() {
        use crate::config::NodeMode;
        // Public axis: the MTA rides the mail toggle (the normal VPS box).
        assert!(mta_should_run(NodeMode::Public, true));
        assert!(!mta_should_run(NodeMode::Public, false));
        // Private home box: the perimeter parser is NEVER started — mail-enabled
        // there only brings up the MDA to serve relayed mail over LAN IMAP/CalDAV
        // (deployment-home-with-public-relay.md § Plaintext-mode behavior).
        assert!(!mta_should_run(NodeMode::Private, true));
        assert!(!mta_should_run(NodeMode::Private, false));
    }

    #[test]
    fn mda_runs_for_any_protocol_on_both_axes() {
        // The one MDA process hosts IMAP + CalDAV + CardDAV + WebDAV: up iff ANY
        // is enabled, independent of the NAT axis (the private box's MDA serves
        // LAN MUAs). WebDAV-only (mail + calendar + contacts off) still brings
        // the MDA up.
        assert!(mda_should_run(true, false, false, false)); // IMAP only
        assert!(mda_should_run(false, true, false, false)); // CalDAV only
        assert!(mda_should_run(false, false, true, false)); // CardDAV only
        assert!(mda_should_run(false, false, false, true)); // WebDAV only
        assert!(mda_should_run(true, true, true, true)); // all four
        assert!(!mda_should_run(false, false, false, false)); // none
    }

    #[test]
    fn caldav_flag_round_trips_independently_of_mail_flag() {
        let tmp = TmpDir::new();
        let imap = tmp.path().join(MAIL_ENABLE_FLAG);
        let caldav = tmp.path().join(CALDAV_ENABLE_FLAG);

        // CalDAV on, mail off — distinct files, no cross-talk.
        set_caldav_enable_flag(tmp.path(), true).unwrap();
        assert!(caldav.exists());
        assert!(!imap.exists());

        // Now mail on too.
        set_mail_enable_flag(tmp.path(), true).unwrap();
        assert!(caldav.exists());
        assert!(imap.exists());

        // CalDAV off again leaves the mail flag in place.
        set_caldav_enable_flag(tmp.path(), false).unwrap();
        assert!(!caldav.exists());
        assert!(imap.exists());
    }

    #[test]
    fn atproto_flag_round_trips_independently_of_mail_flags() {
        let tmp = TmpDir::new();
        let imap = tmp.path().join(MAIL_ENABLE_FLAG);
        let atproto = tmp.path().join(ATPROTO_ENABLE_FLAG);

        // atproto on, mail off — distinct files, no cross-talk.
        set_atproto_enable_flag(tmp.path(), true).unwrap();
        assert!(atproto.exists());
        assert!(!imap.exists());

        // mail on too — both present, independent.
        set_mail_enable_flag(tmp.path(), true).unwrap();
        assert!(atproto.exists());
        assert!(imap.exists());

        // atproto off again leaves the mail flag in place.
        set_atproto_enable_flag(tmp.path(), false).unwrap();
        assert!(!atproto.exists());
        assert!(imap.exists());
    }

    #[tokio::test]
    async fn set_atproto_enabled_writes_flag_and_is_best_effort_on_supervisor() {
        // The supervisor socket is absent on the test box, so the notify must be
        // swallowed (best-effort) while the flag write — the durable signal —
        // still takes effect. Enable creates the flag; disable removes it.
        let tmp = TmpDir::new();
        let flag = tmp.path().join(ATPROTO_ENABLE_FLAG);

        set_atproto_enabled(tmp.path(), true).await.unwrap();
        assert!(flag.exists(), "enable must materialize the durable flag");

        set_atproto_enabled(tmp.path(), false).await.unwrap();
        assert!(!flag.exists(), "disable must remove the durable flag");
    }

    #[test]
    fn supervisor_command_frames_the_atproto_service() {
        assert_eq!(
            supervisor_command("up", ATPROTO_BRIDGE_SERVICE),
            "{\"action\":\"up\",\"service\":\"fauna-atproto-bridge\"}\n"
        );
    }

    #[tokio::test]
    async fn caldav_reconcile_recreates_flag_when_enabled_but_flag_missing() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        db.set_caldav_enabled(true).await.unwrap();
        assert_eq!(
            reconcile_caldav_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(true)
        );
        assert!(tmp.path().join(CALDAV_ENABLE_FLAG).exists());
        // Idempotent second pass.
        assert_eq!(
            reconcile_caldav_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn webdav_flag_round_trips_independently_of_other_flags() {
        let tmp = TmpDir::new();
        let imap = tmp.path().join(MAIL_ENABLE_FLAG);
        let webdav = tmp.path().join(WEBDAV_ENABLE_FLAG);

        // WebDAV on, mail off — distinct files, no cross-talk.
        set_webdav_enable_flag(tmp.path(), true).unwrap();
        assert!(webdav.exists());
        assert!(!imap.exists());

        // WebDAV off again leaves other flags untouched.
        set_mail_enable_flag(tmp.path(), true).unwrap();
        set_webdav_enable_flag(tmp.path(), false).unwrap();
        assert!(!webdav.exists());
        assert!(imap.exists());
    }

    #[tokio::test]
    async fn webdav_reconcile_recreates_flag_when_enabled_but_flag_missing() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        db.set_webdav_enabled(true).await.unwrap();
        assert_eq!(
            reconcile_webdav_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(true)
        );
        assert!(tmp.path().join(WEBDAV_ENABLE_FLAG).exists());
        // Idempotent second pass.
        assert_eq!(
            reconcile_webdav_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn reconcile_noop_when_toggle_unset() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        // Freshly-claimed nest: no toggle row → nothing to reconcile, and the
        // flag is left untouched (the derived fetch_config fallback owns read).
        assert_eq!(
            reconcile_mail_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
        assert!(!tmp.path().join(MAIL_ENABLE_FLAG).exists());
    }

    #[tokio::test]
    async fn reconcile_recreates_flag_when_enabled_but_flag_missing() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        db.set_mail_enabled(true).await.unwrap();
        // The flag was removed by hand (forbidden hand-edit) while state says on.
        assert_eq!(
            reconcile_mail_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(true)
        );
        assert!(tmp.path().join(MAIL_ENABLE_FLAG).exists());
        // Second pass: now consistent → no-op.
        assert_eq!(
            reconcile_mail_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn reconcile_removes_flag_when_disabled_but_flag_present() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        db.set_mail_enabled(false).await.unwrap();
        // The flag was created by hand (forbidden hand-edit) while state says off.
        set_mail_enable_flag(tmp.path(), true).unwrap();
        assert_eq!(
            reconcile_mail_enable_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(false)
        );
        assert!(!tmp.path().join(MAIL_ENABLE_FLAG).exists());
    }

    #[tokio::test]
    async fn spawn_mail_enable_reconciliation_runs_a_boot_reconcile() {
        // A nest crash between the DB toggle write and the flag-file write in
        // `set_mail_enabled_handler` leaves a torn state (DB says disabled, the
        // `imap-enabled` flag still present). The reconciler must heal that at
        // BOOT — immediately on spawn — not only after one full interval, or the
        // box serves a stale flag for the whole interval after every restart
        // (common.md § Client-state recoverability: single atomic decision point
        // + boot reconcile). This is the wiring proof for the boot reconcile;
        // the heal LOGIC is proved deterministically by
        // `reconcile_removes_flag_when_disabled_but_flag_present` above.
        //
        // The interval is an hour, so the ONLY thing that can remove the flag
        // within the poll window is an immediate first-tick reconcile at boot —
        // a reconciler that skips its first tick times out here.
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let tmp = TmpDir::new();
        db.set_mail_enabled(false).await.unwrap();
        set_mail_enable_flag(tmp.path(), true).unwrap(); // the torn flag a crash left
        assert!(tmp.path().join(MAIL_ENABLE_FLAG).exists());

        let _handle = spawn_mail_enable_reconciliation(
            db.clone(),
            tmp.path().to_path_buf(),
            Duration::from_secs(3600),
        );

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tmp.path().join(MAIL_ENABLE_FLAG).exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "boot reconcile did not remove the torn `imap-enabled` flag within \
                 5s (interval=1h, so only an immediate first-tick reconcile could) \
                 — the reconciler is skipping its first tick"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // ── enable-time mail-domain provisioning safety net ─────────────────────

    /// In-memory AppState whose resolved `handle_domain()` is `domain` (set via
    /// `registration.handle_domain`, the first resolver source).
    fn state_with_handle_domain(domain: Option<&str>) -> crate::routes::AppState {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let mut state = crate::routes::AppState::for_test(std::sync::Arc::new(db));
        // seed-read-ok(test): this helper's whole job is to seed the middle
        // rung of the resolver chain, so a unit can drive `handle_domain()`
        // without standing up a claim.
        state.auth.registration.handle_domain = domain.map(|d| d.to_string());
        state
    }

    #[tokio::test]
    async fn enable_net_provisions_primary_for_real_domain() {
        let state = state_with_handle_domain(Some("example.com"));
        // Precondition: a freshly-reset box with no mail domain registered.
        assert!(
            state
                .db
                .list_active_mail_domains()
                .await
                .unwrap()
                .is_empty()
        );

        ensure_primary_mail_domain(&state).await;

        let domains = state.db.list_active_mail_domains().await.unwrap();
        assert_eq!(domains.len(), 1, "real domain should be auto-provisioned");
        assert_eq!(domains[0].domain_name, "example.com");
        assert!(
            domains[0].is_primary,
            "the first auto-domain is the primary"
        );
    }

    #[tokio::test]
    async fn registering_primary_refreshes_identity_cache() {
        // The primary `mail_domains` row IS the deployment identity: registering it
        // must make `handle_domain()` (the sync identity accessor) return that
        // domain — no separate identity store. A domainless test box (no cache, no
        // registration.handle_domain, no node.domain) falls back to "localhost".
        let state = state_with_handle_domain(None);
        assert_eq!(state.handle_domain(), "localhost");

        ensure_mail_domain_registered(&state, "example.com").await;

        assert_eq!(
            state.handle_domain(),
            "example.com",
            "the identity cache follows the newly-registered primary domain"
        );
        let domains = state.db.list_active_mail_domains().await.unwrap();
        assert_eq!(domains.len(), 1);
        assert!(domains[0].is_primary);
    }

    #[tokio::test]
    async fn enable_net_skips_localhost() {
        let state = state_with_handle_domain(Some("localhost"));
        ensure_primary_mail_domain(&state).await;
        assert!(
            state
                .db
                .list_active_mail_domains()
                .await
                .unwrap()
                .is_empty(),
            "loopback nests have no real DNS — nothing to provision"
        );
    }

    #[tokio::test]
    async fn enable_net_skips_ip_literal_and_mdns_local() {
        for d in ["192.0.2.10", "10.0.0.5:3000", "pi.local", "alice.localhost"] {
            let state = state_with_handle_domain(Some(d));
            ensure_primary_mail_domain(&state).await;
            assert!(
                state
                    .db
                    .list_active_mail_domains()
                    .await
                    .unwrap()
                    .is_empty(),
                "{d} is a local target — must be skipped"
            );
        }
    }

    #[tokio::test]
    async fn enable_net_is_noop_when_a_domain_is_already_registered() {
        // The user chose a *different* mail domain at claim time; the enable-time
        // net must not override it (composes — fires only when nothing exists).
        let state = state_with_handle_domain(Some("example.com"));
        state
            .db
            .add_mail_domain(
                "chosen.example",
                true,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        ensure_primary_mail_domain(&state).await;

        let domains = state.db.list_active_mail_domains().await.unwrap();
        assert_eq!(domains.len(), 1, "net must not add a second domain");
        assert_eq!(domains[0].domain_name, "chosen.example");
    }

    #[tokio::test]
    async fn enable_net_falls_back_to_node_domain_when_handle_domain_unset() {
        // No deployment artifact passes `--handle-domain`, so the resolver falls
        // through to the `[nest] domain` seed — which `docker/entrypoint.sh`
        // deliberately does not write either ("[nest] overlay: CORS seed (NO
        // domain)"). So this unit pins the middle-to-last rung of the chain in
        // isolation; it is not a picture of the image, where BOTH seeds are unset
        // and the domain arrives at claim. (It said "entrypoint.sh writes
        // example.com" until 2026-09-02; the entrypoint has never written one.)
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let mut state = crate::routes::AppState::for_test(std::sync::Arc::new(db));
        let cfg = std::sync::Arc::get_mut(&mut state.config).unwrap();
        cfg.nest.domain = Some("example.com".to_string());
        // seed-read-ok(test): asserting the seed is unset is what makes the
        // assertion below about the `[nest] domain` rung rather than this one.
        assert!(state.auth.registration.handle_domain.is_none());

        ensure_primary_mail_domain(&state).await;

        let domains = state.db.list_active_mail_domains().await.unwrap();
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].domain_name, "example.com");
        assert!(domains[0].is_primary);
    }

    #[tokio::test]
    async fn ensure_mail_domain_registered_is_idempotent() {
        let state = state_with_handle_domain(None);
        ensure_mail_domain_registered(&state, "example.com").await;
        ensure_mail_domain_registered(&state, "example.com").await; // second = no-op
        let domains = state.db.list_active_mail_domains().await.unwrap();
        assert_eq!(domains.len(), 1);
        assert!(domains[0].is_primary);
    }

    // ── enable-time admin canonical-recipient-alias safety net ──────────────

    /// Register an admin actor with a bare handle (mirrors what `claim_admin_core`
    /// leaves behind: a `users` row + admin grant + bare handle, no alias row).
    async fn seed_admin_with_handle(
        state: &crate::routes::AppState,
        actor: &[u8; 32],
        handle: &str,
    ) {
        state.db.create_user(actor, "free", "test").await.unwrap();
        state.db.add_admin_actor(actor).await.unwrap();
        state.db.set_handle(actor, handle).await.unwrap();
    }

    #[tokio::test]
    async fn admin_net_makes_admin_handle_a_routable_recipient() {
        // A re-claimed factory-reset box: the admin claimed with a bare handle and
        // the mail domain auto-provisioned at enable time, but the admin never
        // travelled the per-user `provision_recipient_mls_pubkey` canonical-alias
        // path — so its own `test@example.com` has no exact alias and the AUTH login
        // resolver `validate_recipient` (exact-only) would reject it
        // `no such recipient`, breaking IMAP/CalDAV/submission login.
        let state = state_with_handle_domain(Some("example.com"));
        let admin = [9u8; 32];
        seed_admin_with_handle(&state, &admin, "test").await;

        // Enable-time safety net step 1: the primary mail domain is provisioned.
        ensure_primary_mail_domain(&state).await;
        // The bug this fixes — the domain alone is not enough; the canonical alias
        // is still absent after the domain net.
        assert!(
            state
                .db
                .lookup_exact_alias("example.com", "test")
                .await
                .unwrap()
                .is_none(),
            "precondition: no canonical alias before the admin net runs"
        );

        // Step 2 (the fix): ensure the admin's canonical recipient alias.
        ensure_admin_recipient_aliases(&state).await;

        assert_eq!(
            state
                .db
                .lookup_exact_alias("example.com", "test")
                .await
                .unwrap(),
            Some(admin),
            "the admin's <handle>@<primary-domain> must validate as its own recipient"
        );

        // Idempotent: a second pass (e.g. every boot) neither errors nor changes
        // the resolved owner.
        ensure_admin_recipient_aliases(&state).await;
        assert_eq!(
            state
                .db
                .lookup_exact_alias("example.com", "test")
                .await
                .unwrap(),
            Some(admin)
        );
    }

    #[tokio::test]
    async fn admin_net_skips_when_no_mail_domain() {
        // A loopback box has no servable mail domain (the domain net skips it), so
        // there is nothing routable — the admin net must be a no-op too.
        let state = state_with_handle_domain(Some("localhost"));
        let admin = [9u8; 32];
        seed_admin_with_handle(&state, &admin, "test").await;

        ensure_primary_mail_domain(&state).await;
        ensure_admin_recipient_aliases(&state).await;

        assert!(
            state
                .db
                .lookup_exact_alias("localhost", "test")
                .await
                .unwrap()
                .is_none(),
            "loopback box has no mail domain — no canonical alias to write"
        );
    }

    #[test]
    fn caldav_port_flag_writes_decimal_value() {
        let tmp = TmpDir::new();
        let flag = tmp.path().join(CALDAV_PORT_FLAG);

        set_caldav_port_flag(tmp.path(), 9443).unwrap();
        assert_eq!(std::fs::read_to_string(&flag).unwrap(), "9443");

        // Owner-only on unix, like the enable flags (nest output, not admin input).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&flag).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "caldav-port flag must be owner-only");
        }
    }

    #[test]
    fn caldav_port_flag_overwrites_previous_value() {
        let tmp = TmpDir::new();
        let flag = tmp.path().join(CALDAV_PORT_FLAG);

        set_caldav_port_flag(tmp.path(), 9443).unwrap();
        set_caldav_port_flag(tmp.path(), 8443).unwrap();
        // Latest write wins, with no stale trailing bytes from the longer value.
        assert_eq!(std::fs::read_to_string(&flag).unwrap(), "8443");
    }

    #[tokio::test]
    async fn caldav_port_reconcile_noop_when_unset() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        // Admin never picked a port → nothing authoritative to assert; the flag is
        // left absent (the supervisor keeps its install default).
        assert_eq!(
            reconcile_caldav_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
        assert!(!tmp.path().join(CALDAV_PORT_FLAG).exists());
    }

    #[tokio::test]
    async fn caldav_port_reconcile_writes_flag_when_set_but_missing() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        db.set_caldav_port(9443).await.unwrap();

        assert_eq!(
            reconcile_caldav_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(9443)
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(CALDAV_PORT_FLAG)).unwrap(),
            "9443"
        );
        // Idempotent second pass: flag already matches → no re-write.
        assert_eq!(
            reconcile_caldav_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn caldav_port_reconcile_rewrites_on_divergence() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        // The flag was hand-edited to a stale value (forbidden) while state
        // says 9443 — persisted state wins, the flag is re-asserted.
        set_caldav_port_flag(tmp.path(), 8443).unwrap();
        db.set_caldav_port(9443).await.unwrap();

        assert_eq!(
            reconcile_caldav_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(9443)
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(CALDAV_PORT_FLAG)).unwrap(),
            "9443"
        );
    }

    // ── serving-port value flag (the nest's own client-facing listener twin) ──

    #[test]
    fn serving_port_flag_writes_decimal_value_owner_only() {
        let tmp = TmpDir::new();
        let flag = tmp.path().join(SERVING_PORT_FLAG);

        set_serving_port_flag(tmp.path(), 8443).unwrap();
        assert_eq!(std::fs::read_to_string(&flag).unwrap(), "8443");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&flag).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "serving-port flag must be owner-only");
        }
    }

    #[test]
    fn serving_port_flag_overwrites_previous_value() {
        let tmp = TmpDir::new();
        let flag = tmp.path().join(SERVING_PORT_FLAG);

        set_serving_port_flag(tmp.path(), 8443).unwrap();
        set_serving_port_flag(tmp.path(), 443).unwrap();
        // Latest write wins, with no stale trailing bytes from the longer value.
        assert_eq!(std::fs::read_to_string(&flag).unwrap(), "443");
    }

    #[tokio::test]
    async fn serving_port_reconcile_noop_when_unset() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        // Admin never picked a port → the flag is left absent (the supervisor
        // keeps its install default, 443).
        assert_eq!(
            reconcile_serving_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
        assert!(!tmp.path().join(SERVING_PORT_FLAG).exists());
    }

    #[tokio::test]
    async fn serving_port_reconcile_writes_then_idempotent() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        db.set_serving_port(8443).await.unwrap();

        assert_eq!(
            reconcile_serving_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(8443)
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(SERVING_PORT_FLAG)).unwrap(),
            "8443"
        );
        // Idempotent second pass: flag already matches → no re-write.
        assert_eq!(
            reconcile_serving_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn serving_port_reconcile_rewrites_on_divergence() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let tmp = TmpDir::new();
        // Stale on-disk flag while state says 8443 — persisted state wins.
        set_serving_port_flag(tmp.path(), 443).unwrap();
        db.set_serving_port(8443).await.unwrap();

        assert_eq!(
            reconcile_serving_port_flag_once(&db, tmp.path())
                .await
                .unwrap(),
            Some(8443)
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(SERVING_PORT_FLAG)).unwrap(),
            "8443"
        );
    }

    // ── The service-toggle read, and the class of bug it exists to make
    //    unrepresentable ──────────────────────────

    /// **The bug row 118 names: enabling a DAV axis must not start the MTA.**
    ///
    /// A DAV-only nest has never enabled mail, so its toggle is unset. Before
    /// this, the three DAV enable handlers each read that unset toggle as
    /// `.unwrap_or(true)` and handed `reconcile_supervisor` a live `up` for the
    /// internet-facing SMTP perimeter parser. It did not *serve* — the s6
    /// run-script re-downs a spurious up while `/data/imap-enabled` is absent —
    /// so the whole defect was held by a shell guard one layer down, in a
    /// different language, and would go live on any deployment shape without
    /// that run-script (bare-binary, systemd, tier_3 standalone).
    #[tokio::test]
    async fn enabling_a_dav_axis_on_an_unset_mail_nest_does_not_start_the_mta() {
        use crate::config::NodeMode;
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        db.set_caldav_enabled(true).await.unwrap();

        let t = effective_service_toggles(&db).await.unwrap();

        assert!(
            !t.mail,
            "an unset mail toggle reads OFF (Stage-5 default-off, owned by \
             `effective_mail_enabled`) — the pre-fix `.unwrap_or(true)` is what \
             opened the SMTP perimeter on a calendar-only box"
        );
        assert!(
            !mta_should_run(NodeMode::Public, t.mail),
            "and so the MTA must stay down on a public DAV-only deployment"
        );
        assert!(
            mda_should_run(t.mail, t.caldav, t.carddav, t.webdav),
            "while the MDA comes up, which is the whole point of enabling CalDAV"
        );
    }

    /// The DAV axes still follow mail when unset — this fix changes what an
    /// unset *mail* toggle means, and nothing else.
    #[tokio::test]
    async fn the_dav_axes_still_follow_the_mail_toggle_when_unset() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        db.set_mail_enabled(true).await.unwrap();

        let t = effective_service_toggles(&db).await.unwrap();

        assert!(t.mail);
        assert!(
            t.caldav && t.carddav && t.webdav,
            "unset DAV axes follow mail"
        );

        // …and an explicit `false` is never overridden by that inheritance.
        db.set_caldav_enabled(false).await.unwrap();
        let t = effective_service_toggles(&db).await.unwrap();
        assert!(t.mail && !t.caldav);
    }

    /// **Class guard: no mail/DAV-enable read may default to ON.**
    ///
    /// The behavioural pin above covers the shared helper, but the defect was
    /// hand-copied reads that each drifted from it — so a fourth handler
    /// could reintroduce it tomorrow and every behavioural test would stay
    /// green. Source-level and tree-walking on purpose: **a completeness
    /// guard walks, it never lists** (the lesson), so a DAV axis nobody
    /// has written yet is covered by construction. `effective_mail_enabled` /
    /// `effective_service_toggles` are the only legitimate places the unset
    /// case is resolved, and they resolve it to `false`.
    ///
    /// Two reach gaps in the original, both fixed here:
    /// (1) it keyed on the single literal spelling `.unwrap_or(true)` and
    /// missed every other way to resolve `None` to `true` (`!= Some(false)`
    /// PROBE-362-A-proven; `matches!(x, None | Some(true))`; etc) — this walk
    /// keys on a small set of known ON-defaulting spellings instead of one;
    /// (2) it watched only `get_mail_enabled`, leaving the three DAV getters
    /// uncovered — this walk keys on all four. It also carries a coverage
    /// floor (the idiom this codebase already uses for exactly this — the
    /// macOS custodian cloud-backup test's `checked >= 8`): zero sites
    /// visited must not read as zero offenders, so a rename of a getter (or a
    /// walk pointed at the wrong tree) reds loudly instead of going quiet.
    #[test]
    fn no_mail_enable_read_defaults_to_on() {
        const TOGGLE_GETTERS: [&str; 4] = [
            "get_mail_enabled",
            "get_caldav_enabled",
            "get_carddav_enabled",
            "get_webdav_enabled",
        ];
        // Known ON-defaulting spellings for an `Option<bool>` toggle read —
        // deliberately a small, explicit set rather than a bare "contains
        // `true`" scan, which would also flag the many CORRECT sites that
        // spell an explicit-only check as `Some(true)` (no `None` arm) or
        // `matches!(.., Ok(Some(true)))`.
        const ON_DEFAULT_SPELLINGS: [&str; 5] = [
            ".unwrap_or(true)",
            "unwrap_or_else(|| true)",
            ".map_or(true,",
            "None | Some(true)",
            "!= Some(false)",
        ];

        // Key on the CALL/DEFINITION form `getter(`, never the bare name: this
        // walk scans its own file too, and the bare name sits in
        // `TOGGLE_GETTERS`, `EXPECTED_TOGGLE_GETTERS`, the doc comment above
        // and a log string in this file, so a bare-name key let those lines
        // satisfy the per-getter floor by themselves — a production RENAME of
        // a getter stayed green, the exact blindness the floor exists to red
        // (measured 2026-08-26: rename every call of `get_webdav_enabled` —
        // the `name(` form — to `webdav_on`: bare-name key green, this key red
        // naming `get_webdav_enabled`). String literals and prose
        // never carry the `(`; every call site and the `fn` definition do —
        // and this comment deliberately spells no getter in that form either.
        let call_keys: Vec<(&str, String)> = TOGGLE_GETTERS
            .iter()
            .map(|g| (*g, format!("{g}(")))
            .collect();

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut checked_by_getter: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read source");
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    let Some((getter, _)) = call_keys
                        .iter()
                        .find(|(_, key)| line.contains(key.as_str()))
                    else {
                        continue;
                    };
                    *checked_by_getter.entry(*getter).or_insert(0) += 1;
                    // The read and its default may be split across the await /
                    // map_err chain, so look a few lines ahead.
                    let window = lines[i..(i + 6).min(lines.len())].join(" ");
                    if window.contains("effective_service_toggles")
                        || window.contains("effective_mail_enabled")
                    {
                        continue; // routed through the single owner — never an offense.
                    }
                    if let Some(spelling) =
                        ON_DEFAULT_SPELLINGS.iter().find(|s| window.contains(**s))
                    {
                        offenders.push(format!(
                            "{}:{} ({getter}, `{spelling}`)",
                            path.strip_prefix(&src).unwrap_or(&path).display(),
                            i + 1
                        ));
                    }
                }
            }
        }
        // Per getter, not a total: a total floor
        // cannot detect a per-getter blindness — dropping `get_mail_enabled`
        // from `TOGGLE_GETTERS` still clears an 8-site total from the three
        // remaining DAV getters alone. This roster
        // is deliberately a SEPARATE literal from `TOGGLE_GETTERS`, not derived
        // from it, so a `TOGGLE_GETTERS` edit that drops a getter reds here by
        // naming exactly which one vanished, instead of shrinking the floor
        // along with the list it's supposed to police.
        const EXPECTED_TOGGLE_GETTERS: [&str; 4] = [
            "get_mail_enabled",
            "get_caldav_enabled",
            "get_carddav_enabled",
            "get_webdav_enabled",
        ];
        for getter in EXPECTED_TOGGLE_GETTERS {
            let visited = checked_by_getter.get(getter).copied().unwrap_or(0);
            assert!(
                visited >= 1,
                "the scan visited {visited} call sites for `{getter}` — a rename \
                 of this getter, its removal from TOGGLE_GETTERS, or a walk \
                 pointed at the wrong tree would leave this guard silently \
                 vacuous for exactly this getter rather than protective"
            );
        }
        assert!(
            offenders.is_empty(),
            "these sites resolve an UNSET deployment mail/DAV toggle to ON: \
             {offenders:?}. Unset means OFF since the Stage-5 default-off flip, \
             and the single owner of that decision is \
             `CacheDb::effective_mail_enabled` (mail) / \
             `mail_enable::effective_service_toggles` (all four) — read through \
             one of those rather than hand-rolling the default. A site that \
             defaults a toggle ON hands `reconcile_supervisor` a live `up` for \
             a service perimeter the deployment never asked for."
        );
    }
}

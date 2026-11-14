//! Dev/test fixture for the Windows shell-extension observation harness.
//!
//! Puts a file into a **tracked** state that the shell-ext `GetState` pipe reports
//! as `ECS_ENABLED` — **without** FaunaApp, a login, a capability provision, or
//! cfapi. The status read is purely a `sync_entries` DB lookup gated by config
//! (`pipe_server::handle_get_file_status` → `resolve_to_folder_rel` → per-folder
//! `SyncDb`), so a tracked file needs only three things, all of which this tool
//! arranges against the **already-running** `fauna-sync` service:
//!
//!   1. config maps the folder as **bound to a folder**, on-demand by default or
//!      always-resident with `--mode always` (driven at runtime via the service's
//!      own pipe: `AddLocation` + `SetLocationSyncMode(<mode>)` + the ref-keyed
//!      `SetLocationFolder`; the status read resolves both modes), and
//!   2. a `sync_entries` row at the folder-relative path with a non-`Deleted`
//!      state (seeded directly into the set's identity-keyed state DB,
//!      `%LOCALAPPDATA%\Fauna\sync\fsid-<ref>.db`), and
//!   3. the file present on disk (so Explorer can display + right-click it).
//!
//! Reusing the running service's per-SID pipe (the one the shell DLL hardcodes,
//! `SyncPipeClient::connect_pipe`) is deliberate: the DLL cannot be pointed at a
//! test pipe, so the live overlay/menu can only be observed against whatever owns
//! `\\.\pipe\fauna-sync.<SID>`. This tool leaves that service running and reverses
//! its config mutation in `teardown`.
//!
//! Paired with a dev-fleet UIA script that drives Explorer's context menu and
//! reports whether the "Fauna" verb renders (gates ②③, tracked internally); a
//! sibling orchestration script runs a **no-capability** `fauna-sync` (which
//! registers no cfapi root, so the seeded folder stays enumerable) on the per-SID
//! pipe, seeds one file with this tool, reads the menu, and restores production.
//! Full method is documented alongside those dev-fleet scripts. NOT shipped —
//! never staged into the MSI.
//!
//! ```text
//! fauna-shellext-fixture seed     --dir <folder> --file <name> --folder <set> [--folder-id <ref>] [--mode <on-demand|always>] [--content <text>]
//! fauna-shellext-fixture status   --file <abs-path>
//! fauna-shellext-fixture teardown --dir <folder> --folder <set> [--folder-id <ref>] [--file <name>]
//! fauna-shellext-fixture register-shell   --dll <abs-path-to-fauna_shell.dll>
//! fauna-shellext-fixture unregister-shell
//! ```
//! `register-shell` writes the shell extension's per-user registration
//! (`fauna_shell::per_user`: every COM class, the file and folder verbs, under
//! `HKEY_CURRENT_USER`, no elevation) naming `--dll`, so Explorer loads a build
//! output for the run; it steps aside, writing nothing, when the machine-wide
//! (MSI) registration is present. It prints `SHELL_REGISTRATION=per-user` or
//! `SHELL_REGISTRATION=machine-wide`; `unregister-shell` removes the per-user keys
//! and prints `SHELL_REGISTRATION=removed`. The caller holds the registration's
//! named mutex (`fauna_shell::per_user::REGISTRATION_MUTEX`) from before
//! `register-shell` until after `unregister-shell`.
//! `--folder-id` is the set's `FolderRef` wire form (default [`FIXTURE_REF`]):
//! the binding is keyed by it, and so is the state DB the status read opens.
//! `--mode` is the location's sync mode (default `on-demand`): `always` binds an
//! always-resident root, whose files are ordinary files on any service, so its
//! folder is enumerable even with a capability present.
//! `seed`/`status` print `STATUS=<Synced|CloudOnly|...>` and `FILE=<abs-path>` on
//! success so the PowerShell orchestrator can parse the result.
//!
//! A binding is an account's (since 2026-11-06 the agent refuses `AddLocation`
//! while no account is provisioned), so `seed` against an agent with none first
//! provisions a fixture account whose nest is a loopback port nothing serves —
//! the agent's content-key resolution then names no set, so no engine starts and
//! no cfapi root is registered — and seeds the row under that account's scope,
//! printing `ACCOUNT=<actor hex>`. An agent that already has an account keeps
//! its own capability: the fixture never provisions over one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fauna_core::folder_keys::FolderRef;
use fauna_ipc::sync::{
    BINDINGS_NEED_AN_ACCOUNT_PREFIX, BearerToken, FileStatus, RequestMethod, ResponsePayload,
    ResponseResult, SyncCapability,
};
use fauna_ipc::sync_pipe_client::SyncPipeClient;
use fauna_sync_engine::db::{SyncDb, SyncState};

/// Base dir where the target service opens `config.toml` + per-folder DBs. With
/// `--data-dir` it matches a test service's `SyncPaths::at(root)` (everything
/// under the root); without it, the production layout —
/// `fauna_sync_engine::root::platform_state_base()` (`%LOCALAPPDATA%\Fauna\sync`),
/// the same per-user root `SyncPaths` itself delegates to, deduping the hand-rolled copy this fixture used to carry.
fn base_dir(args: &[String]) -> Result<PathBuf> {
    if let Some(root) = flag(args, "--data-dir") {
        return Ok(PathBuf::from(root));
    }
    fauna_sync_engine::root::platform_state_base()
}

/// Connect to the shell-ext status pipe. `--pipe <name>` targets a specific pipe
/// (an isolated test service started with `--pipe-name`); otherwise the calling
/// user's per-SID pipe `\\.\pipe\fauna-sync.<SID>` that the shell DLL hardcodes.
fn connect(args: &[String]) -> Result<SyncPipeClient> {
    match flag(args, "--pipe") {
        Some(name) => SyncPipeClient::connect_pipe_to(&name)
            .with_context(|| format!("connect to pipe {name}")),
        None => SyncPipeClient::connect_pipe()
            .context("connect to \\\\.\\pipe\\fauna-sync.<SID> (is a service running?)"),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn require(args: &[String], name: &str) -> Result<String> {
    flag(args, name).with_context(|| format!("missing required flag {name}"))
}

/// The ref a fixture binding carries when `--folder-id` is not given. Any
/// well-formed ref serves: against a no-capability service nothing consults the
/// nest, and the ref only keys the binding and its state DB.
const FIXTURE_REF: FolderRef = FolderRef::Local(1);

/// The binding's set identity (`--folder-id`, else [`FIXTURE_REF`]), refused
/// when it does not parse — the agent would refuse the bind anyway.
fn folder_ref(args: &[String]) -> Result<FolderRef> {
    match flag(args, "--folder-id") {
        Some(wire) => FolderRef::parse(&wire).with_context(|| format!("not a folder id: {wire}")),
        None => Ok(FIXTURE_REF),
    }
}

/// The location's sync mode (`--mode`, else `on-demand`), in the wire spelling
/// `SetLocationSyncMode` takes. Anything else is refused here rather than by the
/// service, so a typo never leaves a half-seeded binding.
fn sync_mode(args: &[String]) -> Result<String> {
    let mode = flag(args, "--mode").unwrap_or_else(|| "on-demand".to_string());
    match mode.as_str() {
        "on-demand" | "always" => Ok(mode),
        other => bail!("--mode must be on-demand or always, got {other}"),
    }
}

/// The set's identity-keyed state DB — the path the agent's status read opens
/// (`SyncPaths::sync_db_path_for_ref`): under the account's scope
/// (`<base>/<actor hex>`, the one shared derivation) when `scope` names one,
/// else the flat base.
fn set_db_path(args: &[String], set: FolderRef, scope: Option<&str>) -> Result<PathBuf> {
    let base = base_dir(args)?;
    let dir = match scope {
        Some(hex) => fauna_sync_engine::db::actor_state_dir(&base, hex)?,
        None => base,
    };
    Ok(set.state_db_path(&dir))
}

/// The account the fixture provisions on an agent that has none — a binding
/// write is refused until one is (`fauna_ipc::sync::BINDINGS_NEED_AN_ACCOUNT_PREFIX`,
/// the binding store being an account's). Its nest is a loopback port nothing
/// serves, so the agent's content-key resolution names no set and no engine
/// starts: no cfapi root is registered, and the seeded folder stays an
/// ordinary, enumerable one whatever its mode.
const FIXTURE_ACTOR: [u8; 32] = [0xf1; 32];
const FIXTURE_NEST_URL: &str = "http://127.0.0.1:9";

/// The fixture account's scope component, in the agent's spelling (lowercase hex).
fn fixture_actor_hex() -> String {
    FIXTURE_ACTOR.iter().map(|b| format!("{b:02x}")).collect()
}

fn fixture_capability() -> SyncCapability {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    SyncCapability::new(
        vec![0x5e; 32],
        FIXTURE_ACTOR.to_vec(),
        FIXTURE_NEST_URL.to_string(),
        "shellext-fixture".to_string(),
        BearerToken::new("shellext-fixture".to_string(), now.as_secs() + 86_400),
    )
    .with_provisioned_at_ms(u64::try_from(now.as_millis()).unwrap_or(u64::MAX))
}

/// `AddLocation`, provisioning [`fixture_capability`] first when the agent
/// refuses it for want of an account. Returns the account scope the binding
/// landed in when the fixture provisioned it; `None` when the agent already
/// had an account (its capability is never replaced), whose scope the fixture
/// cannot know — the seed then writes the flat path, and the status check
/// below reports `NotTracked` if that is not where the agent reads.
fn add_location(client: &SyncPipeClient, dir: &str) -> Result<Option<String>> {
    let first = client
        .request(RequestMethod::AddLocation {
            path: dir.to_string(),
        })
        .context("pipe request AddLocation failed")?;
    let refused = match first.result {
        ResponseResult::Ok(_) => return Ok(None),
        ResponseResult::Err(msg) if msg.starts_with(BINDINGS_NEED_AN_ACCOUNT_PREFIX) => msg,
        ResponseResult::Err(msg) => bail!("service rejected AddLocation: {msg}"),
    };
    eprintln!("agent has no account ({refused}); provisioning the fixture account");
    call(
        client,
        RequestMethod::ProvisionCapability(fixture_capability()),
    )?;
    call(
        client,
        RequestMethod::AddLocation {
            path: dir.to_string(),
        },
    )?;
    Ok(Some(fixture_actor_hex()))
}

/// Send one request and unwrap the service's `Ok`/`Err` reply.
fn call(client: &SyncPipeClient, method: RequestMethod) -> Result<ResponsePayload> {
    let name = method.name();
    let resp = client
        .request(method)
        .with_context(|| format!("pipe request {name} failed"))?;
    match resp.result {
        ResponseResult::Ok(payload) => Ok(payload),
        ResponseResult::Err(msg) => bail!("service rejected {name}: {msg}"),
    }
}

fn file_status(client: &SyncPipeClient, abs_path: &str) -> Result<FileStatus> {
    match call(
        client,
        RequestMethod::GetFileStatus {
            path: abs_path.to_string(),
        },
    )? {
        ResponsePayload::FileStatus(info) => Ok(info.status),
        other => bail!("unexpected reply to GetFileStatus: {other:?}"),
    }
}

fn status_name(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Synced => "Synced",
        FileStatus::Syncing => "Syncing",
        FileStatus::CloudOnly => "CloudOnly",
        FileStatus::Error => "Error",
        FileStatus::NotTracked => "NotTracked",
        FileStatus::Unknown => "Unknown",
    }
}

fn seed(args: &[String]) -> Result<()> {
    let dir = require(args, "--dir")?;
    let file = require(args, "--file")?;
    let folder = require(args, "--folder")?;
    let set = folder_ref(args)?;
    let mode = sync_mode(args)?;
    let content =
        flag(args, "--content").unwrap_or_else(|| "fauna shell-ext fixture\n".to_string());

    // 1. Materialize the folder + file on disk (plain `C:\...` path — no `\\?\`
    //    canonicalization, so it prefix-matches the path Explorer hands the DLL).
    std::fs::create_dir_all(&dir).with_context(|| format!("create_dir_all {dir}"))?;
    let abs_file = Path::new(&dir).join(&file);
    std::fs::write(&abs_file, content.as_bytes())
        .with_context(|| format!("write {}", abs_file.display()))?;
    let size = content.len() as i64;
    let abs_file = abs_file.to_string_lossy().to_string();

    // 2. Drive the target service's config via its own pipe so the folder is a
    //    served, bound root in `mode`. `SetLocationSyncMode`/`SetLocationFolder`
    //    reconcile, which builds an engine only for a set the agent's content-key
    //    resolution names — and with the fixture account's nest unserved it names
    //    none — so NO cfapi root is registered and an on-demand folder stays a
    //    normal, enumerable folder (the whole point of the harness).
    let client = connect(args)?;
    let scope = add_location(&client, &dir)?;
    if let Some(hex) = &scope {
        println!("ACCOUNT={hex}");
    }
    call(
        &client,
        RequestMethod::SetLocationSyncMode {
            path: dir.clone(),
            mode,
        },
    )?;
    call(
        &client,
        RequestMethod::SetLocationFolder {
            path: dir.clone(),
            folder: folder.clone(),
            folder_id: set.to_wire(),
        },
    )?;

    // 3. Seed the per-set DB row (folder-relative path, forward slashes) as
    //    Synced. `%LOCALAPPDATA%\Fauna\sync\fsid-<ref>.db`; opened on demand by the
    //    service per query (WAL, 5 s busy_timeout) so an external writer is safe.
    let db_path = set_db_path(args, set, scope.as_deref())?;
    let rel = file.replace('\\', "/");
    {
        let db = SyncDb::open(&db_path)
            .with_context(|| format!("open sync db {}", db_path.display()))?;
        db.upsert_entry(
            &rel,
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            size,
            1,
            None,
        )
        .context("seed sync_entries row")?;
    }

    // 4. Verify the shell-ext status pipe now reports it tracked.
    let status = file_status(&client, &abs_file)?;
    if status == FileStatus::NotTracked {
        bail!(
            "seeded row but GetFileStatus still returns NotTracked for {abs_file} \
             (folder registration / path normalization mismatch?)"
        );
    }
    println!("STATUS={}", status_name(status));
    println!("FILE={abs_file}");
    Ok(())
}

fn status(args: &[String]) -> Result<()> {
    let file = require(args, "--file")?;
    let client = connect(args)?;
    let status = file_status(&client, &file)?;
    println!("STATUS={}", status_name(status));
    println!("FILE={file}");
    Ok(())
}

fn teardown(args: &[String]) -> Result<()> {
    let dir = require(args, "--dir")?;
    let _folder = require(args, "--folder")?;
    let set = folder_ref(args)?;

    // Reverse the config mutation (best-effort — the service may already be gone).
    if let Ok(client) = connect(args) {
        let _ = call(&client, RequestMethod::RemoveLocation { path: dir.clone() });
    }

    // Delete the seeded DB (+ WAL/SHM sidecars) — at either path a seed writes:
    // under the fixture account's scope, or flat — and the on-disk folder.
    let hex = fixture_actor_hex();
    for scope in [Some(hex.as_str()), None] {
        let db_path = set_db_path(args, set, scope)?;
        for suffix in ["", "-wal", "-shm"] {
            let p = PathBuf::from(format!("{}{suffix}", db_path.display()));
            let _ = std::fs::remove_file(&p);
        }
    }
    if Path::new(&dir).exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove_dir_all {dir}"))?;
    }
    println!("TEARDOWN=ok");
    Ok(())
}

/// Register the shell extension per user for the run, naming `--dll` (an absolute
/// path, so the `InprocServer32` value resolves whatever the loader's cwd).
#[cfg(windows)]
fn register_shell(args: &[String]) -> Result<()> {
    let dll = PathBuf::from(require(args, "--dll")?);
    if !dll.is_file() {
        bail!("--dll {} is not a file", dll.display());
    }
    if fauna_shell::per_user::machine_registered() {
        println!("SHELL_REGISTRATION=machine-wide");
        return Ok(());
    }
    // `absolute`, not `canonicalize`: no `\\?\` prefix in a value COM loads from.
    let dll =
        std::path::absolute(&dll).with_context(|| format!("absolute path of {}", dll.display()))?;
    fauna_shell::per_user::register(&dll.to_string_lossy())
        .context("write the per-user shell registration")?;
    if !fauna_shell::per_user::registered() {
        bail!("wrote the per-user shell registration but its verb key is not readable");
    }
    println!("SHELL_REGISTRATION=per-user");
    Ok(())
}

/// Remove the per-user registration `register-shell` wrote.
#[cfg(windows)]
fn unregister_shell() -> Result<()> {
    fauna_shell::per_user::unregister().context("remove the per-user shell registration")?;
    if fauna_shell::per_user::registered() {
        bail!("removed the per-user shell registration but its verb key is still there");
    }
    println!("SHELL_REGISTRATION=removed");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str);
    match cmd {
        Some("seed") => seed(&args[2..]),
        Some("status") => status(&args[2..]),
        Some("teardown") => teardown(&args[2..]),
        #[cfg(windows)]
        Some("register-shell") => register_shell(&args[2..]),
        #[cfg(windows)]
        Some("unregister-shell") => unregister_shell(),
        other => {
            eprintln!(
                "usage: fauna-shellext-fixture <seed|status|teardown|register-shell|unregister-shell> ...\n\
                 got: {other:?}\n\
                 see the module doc for flags."
            );
            std::process::exit(2);
        }
    }
}

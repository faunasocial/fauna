//! Dev/test fixture for the Windows shell-extension observation harness.
//!
//! Puts a file into a **tracked** state that the shell-ext `GetState` pipe reports
//! as `ECS_ENABLED` — **without** FaunaApp, a login, a capability provision, or
//! cfapi. The status read is purely a `sync_entries` DB lookup gated by config
//! (`pipe_server::handle_get_file_status` → `resolve_to_folder_rel` → per-folder
//! `SyncDb`), so a tracked file needs only three things, all of which this tool
//! arranges against the **already-running** `fauna-sync` service:
//!
//!   1. config maps the folder as **on-demand + bound to a folder**
//!      (driven at runtime via the service's own pipe: `AddLocation` +
//!      `SetLocationSyncMode("on-demand")` + the ref-keyed `SetLocationFolder`), and
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
//! fauna-shellext-fixture seed     --dir <folder> --file <name> --folder <set> [--folder-id <ref>] [--content <text>]
//! fauna-shellext-fixture status   --file <abs-path>
//! fauna-shellext-fixture teardown --dir <folder> --folder <set> [--folder-id <ref>] [--file <name>]
//! ```
//! `--folder-id` is the set's `FolderRef` wire form (default [`FIXTURE_REF`]):
//! the binding is keyed by it, and so is the state DB the status read opens.
//! `seed`/`status` print `STATUS=<Synced|CloudOnly|...>` and `FILE=<abs-path>` on
//! success so the PowerShell orchestrator can parse the result.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fauna_core::folder_keys::FolderRef;
use fauna_ipc::sync::{FileStatus, RequestMethod, ResponsePayload, ResponseResult};
use fauna_ipc::sync_pipe_client::SyncPipeClient;
use fauna_sync_engine::db::{SyncDb, SyncState};

/// Base dir where the target service opens `config.toml` + per-folder DBs. With
/// `--data-dir` it matches a test service's `SyncPaths::new(Some(root))` (everything
/// under the root); without it, the production layout —
/// `fauna_sync_engine::root::platform_state_base()` (`%LOCALAPPDATA%\Fauna\sync`),
/// the same per-user root `SyncPaths` itself delegates to, deduping the hand-rolled copy this fixture used to carry.
fn base_dir(args: &[String]) -> PathBuf {
    if let Some(root) = flag(args, "--data-dir") {
        return PathBuf::from(root);
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

/// The set's identity-keyed state DB — the path the agent's status read opens
/// (`SyncPaths::sync_db_path_for_ref`).
fn set_db_path(args: &[String], set: FolderRef) -> PathBuf {
    base_dir(args).join(format!("{}.db", set.db_component()))
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
    //    served on-demand + bound root. `SetLocationSyncMode`/`SetLocationFolder`
    //    reconcile, which no-ops without a provisioned capability — so against a
    //    NO-capability service NO cfapi root is registered and the folder stays a
    //    normal, enumerable folder (the whole point of the harness).
    let client = connect(args)?;
    call(&client, RequestMethod::AddLocation { path: dir.clone() })?;
    call(
        &client,
        RequestMethod::SetLocationSyncMode {
            path: dir.clone(),
            mode: "on-demand".to_string(),
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
    let db_path = set_db_path(args, set);
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

    // Delete the seeded DB (+ WAL/SHM sidecars) and the on-disk folder.
    let db_path = set_db_path(args, set);
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", db_path.display()));
        let _ = std::fs::remove_file(&p);
    }
    if Path::new(&dir).exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove_dir_all {dir}"))?;
    }
    println!("TEARDOWN=ok");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str);
    match cmd {
        Some("seed") => seed(&args[2..]),
        Some("status") => status(&args[2..]),
        Some("teardown") => teardown(&args[2..]),
        other => {
            eprintln!(
                "usage: fauna-shellext-fixture <seed|status|teardown> ...\n\
                 got: {other:?}\n\
                 see the module doc for flags."
            );
            std::process::exit(2);
        }
    }
}

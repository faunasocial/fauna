//! A LIVE cfapi harness for a **reader's** root (`on-demand-files-shared-sets.md` § Shared
//! sets on a capability host, decision 3d (4)): the measurements its windows build rests on,
//! kept as tests — the reader's own harness, beside `live_population`'s.

#![cfg(windows)]

use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

use fauna_cfapi::PlaceholderInfo;
use windows::Win32::Storage::CloudFilters::{CF_CALLBACK_INFO, CF_CALLBACK_PARAMETERS};

const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

unsafe extern "system" fn noop_cb(_: *const CF_CALLBACK_INFO, _: *const CF_CALLBACK_PARAMETERS) {}

// ---------------------------------------------------------------------------
// A reader's root, refused by the OS (`on-demand-files-shared-sets.md` § Shared sets on a
// capability host, decision 3d (4)) — the measurements its windows build rests on.
// ---------------------------------------------------------------------------

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const CF_PLACEHOLDER_STATE_IN_SYNC: u32 = 0x8;
const DELETE: u32 = 0x0001_0000;
const STATUS_ACCESS_DENIED: windows::Win32::Foundation::NTSTATUS =
    windows::Win32::Foundation::NTSTATUS(0xC000_0022_u32 as i32);

/// A reader test's root: what a listing answers, the bytes a fetch serves, and where the
/// callbacks report — keyed by connection key, as [`cases`] is.
struct Reader {
    entries: Vec<PlaceholderInfo>,
    bodies: HashMap<String, Vec<u8>>,
    tx: Sender<String>,
}

fn readers() -> &'static Mutex<HashMap<i64, Reader>> {
    static READERS: OnceLock<Mutex<HashMap<i64, Reader>>> = OnceLock::new();
    READERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn reader_report(key: i64, msg: String) {
    if let Some(r) = readers().lock().unwrap().get(&key) {
        let _ = r.tx.send(msg);
    }
}

unsafe extern "system" fn reader_list_cb(
    info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let entries = readers()
        .lock()
        .unwrap()
        .get(&info.ConnectionKey.0)
        .map(|r| r.entries.clone())
        .unwrap_or_default();
    let result = fauna_cfapi::transfer_placeholders(
        &info.ConnectionKey,
        info.TransferKey,
        info.RequestKey,
        &entries,
    );
    reader_report(info.ConnectionKey.0, format!("LIST {}", result.is_ok()));
}

unsafe extern "system" fn reader_fetch_cb(
    info: *const CF_CALLBACK_INFO,
    params: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let fetch = unsafe { (*params).Anonymous.FetchData };
    let rel = String::from_utf8_lossy(unsafe {
        std::slice::from_raw_parts(
            info.FileIdentity as *const u8,
            info.FileIdentityLength as usize,
        )
    })
    .into_owned();
    let body = readers()
        .lock()
        .unwrap()
        .get(&info.ConnectionKey.0)
        .and_then(|r| r.bodies.get(&rel).cloned())
        .unwrap_or_default();
    let off = (fetch.RequiredFileOffset as usize).min(body.len());
    let end = (off + fetch.RequiredLength as usize).min(body.len());
    let result = fauna_cfapi::transfer_data(
        &info.ConnectionKey,
        info.TransferKey,
        info.RequestKey,
        &body[off..end],
        off as i64,
    );
    reader_report(
        info.ConnectionKey.0,
        format!("FETCH {rel} {}", result.is_ok()),
    );
}

/// A reader's root refuses every delete and rename, but never this process's own.
fn refusal(info: &CF_CALLBACK_INFO) -> windows::Win32::Foundation::NTSTATUS {
    let own = !info.ProcessInfo.is_null()
        && unsafe { (*info.ProcessInfo).ProcessId } == std::process::id();
    if own {
        windows::Win32::Foundation::NTSTATUS(0)
    } else {
        STATUS_ACCESS_DENIED
    }
}

unsafe extern "system" fn reader_delete_cb(
    info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let status = refusal(info);
    let _ = fauna_cfapi::ack_delete(
        &info.ConnectionKey,
        info.TransferKey,
        info.RequestKey,
        status,
    );
    reader_report(
        info.ConnectionKey.0,
        format!("DELETE refused={}", status.0 != 0),
    );
}

unsafe extern "system" fn reader_rename_cb(
    info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let status = refusal(info);
    let _ = fauna_cfapi::ack_rename(
        &info.ConnectionKey,
        info.TransferKey,
        info.RequestKey,
        status,
    );
    reader_report(
        info.ConnectionKey.0,
        format!("RENAME refused={}", status.0 != 0),
    );
}

/// A registered, connected root listing `entries` (each `read_only` as given), with the
/// reader's list applied BEFORE the connection when `refused`, and the delete and rename
/// notifications registered when `notify`.
fn reader_root(
    entries: Vec<PlaceholderInfo>,
    refused: bool,
    notify: bool,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    windows::Win32::Storage::CloudFilters::CF_CONNECTION_KEY,
    std::sync::mpsc::Receiver<String>,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rs = root.to_string_lossy().to_string();
    fauna_cfapi::register_sync_root(&rs, "FaunaTest").expect("register_sync_root");
    if refused {
        fauna_cfapi::deny_user_writes(&root).expect("deny_user_writes");
        fauna_cfapi::set_read_only(&root, true).expect("read-only root");
    }
    let key = fauna_cfapi::connect_with(
        &rs,
        Some(reader_fetch_cb),
        Some(noop_cb),
        Some(reader_list_cb),
        Some(noop_cb),
        notify.then_some(fauna_cfapi::ChangeNotifications {
            delete: Some(reader_delete_cb),
            rename: Some(reader_rename_cb),
        }),
    )
    .expect("connect");
    let (tx, rx) = channel();
    let bodies = entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| (e.rel_path.clone(), e.rel_path.repeat(2).into_bytes()))
        .collect();
    readers().lock().unwrap().insert(
        key.0,
        Reader {
            entries,
            bodies,
            tx,
        },
    );
    (tmp, root, key, rx)
}

/// Tear a reader test's root down: lift the list and the read-only bits so the tempdir can
/// go, disconnect, unregister.
fn drop_reader_root(
    root: &std::path::Path,
    key: windows::Win32::Storage::CloudFilters::CF_CONNECTION_KEY,
) {
    let _ = fauna_cfapi::allow_user_writes(root);
    fauna_cfapi::disconnect(key);
    let _ = fauna_cfapi::unregister_sync_root(&root.to_string_lossy());
    readers().lock().unwrap().remove(&key.0);
    fn walk(p: &std::path::Path) {
        let _ = fauna_cfapi::set_read_only(p, false);
        if let Ok(entries) = std::fs::read_dir(p) {
            for e in entries.flatten() {
                walk(&e.path());
            }
        }
    }
    walk(root);
}

/// Run `line` under another process's `cmd /c`, verbatim; what it printed.
fn cmd(line: &str) -> String {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd")
        .raw_arg(format!("/c {line}"))
        .output()
        .expect("spawn cmd");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn entry(rel: &str, is_dir: bool, read_only: bool) -> PlaceholderInfo {
    PlaceholderInfo {
        rel_path: rel.to_string(),
        size: if is_dir { 0 } else { (rel.len() * 2) as u64 },
        mtime: 1_700_000_000,
        is_dir,
        read_only,
    }
}

fn attrs(p: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(p)
        .expect("stat")
        .file_attributes()
}

fn in_sync(p: &std::path::Path) -> bool {
    fauna_cfapi::placeholder_state(p).expect("state") & CF_PLACEHOLDER_STATE_IN_SYNC != 0
}

/// **The reader's access list is applied once, at registration — this provider's own
/// placeholder writes pass under it** (the windows build's first measurement, decision 3d
/// (4)). The list denies the user — whom the provider runs as — create, write, delete and
/// rename; yet a listing's `TRANSFER_PLACEHOLDERS` creates read-only placeholders, a fetch's
/// `TRANSFER_DATA` hydrates, an eager `CfCreatePlaceholders` creates, the oplock dehydrate
/// frees and the update falls back to a `WRITE_DAC` handle and re-describes. Another
/// process's write, create,
/// new directory, delete and rename are each refused. The provider's own plain unlink is
/// refused too — until it grants itself `DELETE` on that one file. Lifted, the list leaves
/// every child its inherited entries (a NULL DACL here once left each child an EMPTY one).
#[test]
fn reader_acl_the_providers_own_placeholder_writes_pass_and_every_user_change_is_refused() {
    let (_tmp, root, key, rx) = reader_root(
        vec![
            entry("a.txt", false, true),
            entry("b.txt", false, true),
            entry("sub", true, true),
        ],
        true,
        false,
    );
    let rs = root.to_string_lossy().to_string();
    let (a, b) = (root.join("a.txt"), root.join("b.txt"));

    // Listed and hydrated for another process, read-only from creation, in sync.
    let listed = cmd(&format!("dir /b {rs}"));
    assert!(
        listed.contains("a.txt") && listed.contains("sub"),
        "listed: {listed}"
    );
    assert_eq!(cmd(&format!("type {}", a.display())).trim(), "a.txta.txt");
    let said: Vec<String> = rx.try_iter().collect();
    assert!(said.contains(&"LIST true".to_string()), "{said:?}");
    assert!(said.contains(&"FETCH a.txt true".to_string()), "{said:?}");
    assert_ne!(attrs(&a) & FILE_ATTRIBUTE_READONLY, 0, "created read-only");
    assert!(in_sync(&a), "a read-only placeholder is created in sync");

    // Another process's every change is refused.
    for line in [
        format!("echo x>> {}", a.display()),
        format!("echo x> {}", root.join("made.txt").display()),
        format!("mkdir {}", root.join("made-dir").display()),
        format!("del /f /q {}", b.display()),
        format!("ren {} renamed.txt", a.display()),
        format!("attrib -r {} && echo x>> {}", b.display(), b.display()),
    ] {
        let out = cmd(&line);
        assert!(out.contains("denied"), "{line:?} must be refused: {out}");
    }
    assert!(b.exists() && !root.join("made.txt").exists() && !root.join("made-dir").exists());
    assert_eq!(std::fs::read(&a).unwrap(), b"a.txta.txt", "unchanged");

    // The provider's own placeholder writes pass under the list.
    fauna_cfapi::create_placeholder(&root, &entry("new.txt", false, true))
        .expect("an eager CfCreatePlaceholders passes under the list");
    fauna_cfapi::dehydrate_placeholder(&a).expect("the oplock dehydrate passes under the list");
    assert!(in_sync(&a));
    fauna_cfapi::supersede_placeholder(&a, 3, 1_700_000_100)
        .expect("the update's WRITE_DAC fallback passes under the list");
    assert_ne!(
        attrs(&a) & FILE_ATTRIBUTE_READONLY,
        0,
        "the update keeps read-only"
    );

    // Its own plain unlink is refused like the user's — until it grants itself the file.
    let new = root.join("new.txt");
    assert!(
        std::fs::remove_file(&new).is_err(),
        "the bare unlink is refused"
    );
    fauna_cfapi::grant_own(&new, DELETE).expect("grant_own");
    fauna_cfapi::set_read_only(&new, false).expect("clear read-only");
    std::fs::remove_file(&new).expect("granted, the provider's own unlink passes");

    // Lifted: the user may create again, and a child carries its inherited entries.
    fauna_cfapi::allow_user_writes(&root).expect("allow_user_writes");
    assert!(!fauna_cfapi::denies_user_writes(&root).unwrap());
    let out = cmd(&format!("echo x> {}", root.join("after.txt").display()));
    assert!(
        root.join("after.txt").exists(),
        "lifted, a create passes: {out}"
    );
    fauna_cfapi::set_read_only(&b, true)
        .expect("a child keeps its inherited entries: an attribute write on it passes");

    drop_reader_root(&root, key);
}

/// **The provider's own read-only flip keeps a placeholder in sync, and never hydrates one**
/// — what a demoted writer's tree (and a promoted reader's) is flipped with. A plain
/// attribute write takes the file out of sync (`CF_INSYNC_POLICY_TRACK_ALL` tracks the bit,
/// whoever writes it), and a not-in-sync placeholder refuses every later update and
/// dehydrate; [`fauna_cfapi::set_read_only_keeping_sync`] flips it in one verified update,
/// through a handle that reads no data — opening a cloud-only placeholder for read data, in
/// this very process, fetches its whole body.
#[test]
fn reader_acl_a_read_only_flip_keeps_sync_and_never_hydrates() {
    let (_tmp, root, key, rx) = reader_root(
        vec![
            entry("hydrated.txt", false, false),
            entry("cloud.txt", false, false),
            entry("plain.txt", false, false),
            entry("sub", true, false),
        ],
        false,
        false,
    );
    let rs = root.to_string_lossy().to_string();
    cmd(&format!("dir /b {rs}"));
    cmd(&format!("type {}", root.join("hydrated.txt").display()));
    fauna_cfapi::deny_user_writes(&root).expect("a demotion's list first");
    let _: Vec<String> = rx.try_iter().collect();

    for rel in ["hydrated.txt", "cloud.txt", "sub"] {
        let p = root.join(rel);
        assert!(in_sync(&p), "precondition: {rel} in sync");
        fauna_cfapi::set_read_only_keeping_sync(&p, true).expect("demote");
        assert!(in_sync(&p), "{rel} stays in sync through its demotion");
        assert_ne!(attrs(&p) & FILE_ATTRIBUTE_READONLY, 0, "{rel} is read-only");
    }
    assert_ne!(
        attrs(&root.join("cloud.txt")) & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        0,
        "the cloud-only file stays cloud-only"
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let said: Vec<String> = rx.try_iter().collect();
    assert!(
        !said.iter().any(|m| m.starts_with("FETCH")),
        "no flip fetched a body: {said:?}"
    );
    fauna_cfapi::supersede_placeholder(&root.join("hydrated.txt"), 4, 1_700_000_100)
        .expect("a demoted placeholder still takes an update");

    // The plain write is what the keeping form avoids: it takes the file out of sync.
    let plain = root.join("plain.txt");
    fauna_cfapi::set_read_only(&plain, true).expect("plain flip");
    assert!(
        !in_sync(&plain),
        "a plain attribute write takes the file out of sync"
    );

    // A promotion's flips, still under the list, keep sync too.
    fauna_cfapi::set_read_only_keeping_sync(&root.join("cloud.txt"), false).expect("promote");
    assert!(in_sync(&root.join("cloud.txt")));
    assert_eq!(attrs(&root.join("cloud.txt")) & FILE_ATTRIBUTE_READONLY, 0);

    drop_reader_root(&root, key);
}

/// **The delete and rename notifications, acknowledged with access denied, refuse another
/// process's delete, rename and rmdir** — the provider's own unlink and rename fire none,
/// so they pass. The second line of a reader's refusal, beside its access list.
#[test]
fn reader_acl_delete_and_rename_notifications_refuse_another_process() {
    let (_tmp, root, key, rx) = reader_root(
        vec![
            entry("a.txt", false, false),
            entry("b.txt", false, false),
            entry("c.txt", false, false),
            entry("sub", true, false),
        ],
        false,
        true,
    );
    let rs = root.to_string_lossy().to_string();
    cmd(&format!("dir /b {rs}"));
    for line in [
        format!("del /q {}", root.join("a.txt").display()),
        format!("ren {} renamed.txt", root.join("b.txt").display()),
        format!("rmdir {}", root.join("sub").display()),
    ] {
        let out = cmd(&line);
        assert!(
            out.contains("cloud operation"),
            "{line:?} must be refused: {out}"
        );
    }
    assert!(
        root.join("a.txt").exists() && root.join("b.txt").exists() && root.join("sub").exists()
    );
    let said: Vec<String> = rx.try_iter().collect();
    assert!(
        said.contains(&"DELETE refused=true".to_string()),
        "{said:?}"
    );
    assert!(
        said.contains(&"RENAME refused=true".to_string()),
        "{said:?}"
    );

    std::fs::remove_file(root.join("c.txt")).expect("the provider's own unlink passes");
    std::fs::rename(root.join("a.txt"), root.join("a2.txt")).expect("its own rename passes");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let said: Vec<String> = rx.try_iter().collect();
    assert!(
        said.iter().all(|m| !m.contains("refused=true")),
        "the provider's own acts fire no refusal: {said:?}"
    );

    drop_reader_root(&root, key);
}

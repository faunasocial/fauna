//! A LIVE cfapi harness: register a real sync root on a temp dir, connect, get Windows to
//! enumerate it, and assert what `CfExecute(TRANSFER_PLACEHOLDERS)` actually does.
//!
//! **Why this exists.** `fauna-cfapi` had no way to exercise its own core call, and that is
//! precisely how a fatal bug survived for months: `CfExecute(TRANSFER_PLACEHOLDERS)` failed
//! with `ERROR_CLOUD_FILE_INVALID_REQUEST` on *every* input, so Windows on-demand sync had
//! **never once worked** — yet the only way to see it was to launch FaunaApp, log in by hand,
//! bind a folder, and read a service log. Each hypothesis cost ~5 minutes of a human's
//! attention, and three wrong diagnoses stacked up behind one another.
//!
//! These tests turn that loop into ~2 seconds and no human. They are real integration tests:
//! the actual Cloud Filter API, a real sync root, a real FETCH_PLACEHOLDERS callback fired by
//! a real directory enumeration. They need the Cloud Files service (present on any normal
//! Windows 11); everything is created under a temp dir and unregistered on the way out.
//!
//! ## Two facts about cfapi that these tests exist to keep true
//!
//! 1. **Registration alone makes the sync root on-demand-populatable.** Nothing converts it;
//!    `CfConvertToPlaceholder` on the root in fact *fails*, because it already is one.
//! 2. **cfapi does not fire callbacks for I/O originating in the provider's own process.**
//!    An in-process `read_dir` of the root returns empty and no callback ever arrives — which
//!    reads exactly like "the root was never made populatable" and sent an earlier session
//!    chasing that ghost. The enumeration MUST come from another process, as Explorer's does.

#![cfg(windows)]

use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

use fauna_cfapi::PlaceholderInfo;
use windows::Win32::Storage::CloudFilters::{CF_CALLBACK_INFO, CF_CALLBACK_PARAMETERS};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// What each connected root should answer FETCH_PLACEHOLDERS with, plus where to report the
/// outcome — keyed by `CF_CONNECTION_KEY`, exactly as the real host keys its callback
/// contexts. Keying (rather than a single global) is what lets these tests run in parallel:
/// an `extern "system"` callback can capture nothing, so without the key one test's callback
/// would report into another's channel.
type Case = (Vec<PlaceholderInfo>, Sender<String>);
fn cases() -> &'static Mutex<HashMap<i64, Case>> {
    static CASES: OnceLock<Mutex<HashMap<i64, Case>>> = OnceLock::new();
    CASES.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe extern "system" fn fetch_placeholders_cb(
    info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    if info.is_null() {
        return;
    }
    let info = unsafe { &*info };
    let Some((entries, tx)) = cases()
        .lock()
        .unwrap()
        .get(&info.ConnectionKey.0)
        .map(|(e, tx)| (e.clone(), tx.clone()))
    else {
        return;
    };

    let msg = match fauna_cfapi::transfer_placeholders(
        &info.ConnectionKey,
        info.TransferKey,
        info.RequestKey,
        &entries,
    ) {
        Ok(transfer) => format!("OK processed={}", transfer.processed),
        Err(e) => format!("ERR {e:#}"),
    };
    let _ = tx.send(msg);
}

unsafe extern "system" fn noop_cb(_: *const CF_CALLBACK_INFO, _: *const CF_CALLBACK_PARAMETERS) {}

struct Outcome {
    /// What the FETCH_PLACEHOLDERS callback reported (`OK processed=N` / `ERR …`).
    callback: String,
    /// What a *different process* sees in the directory — the user's ground truth.
    listing: Vec<String>,
    /// The root's file attributes right after `CfRegisterSyncRoot`, before `connect`.
    attrs_after_register: u32,
}

/// Register + connect a fresh sync root that will answer FETCH_PLACEHOLDERS with `entries`,
/// make another process enumerate it, and report what happened. Always tears the root down.
fn run_case(entries: Vec<PlaceholderInfo>) -> Outcome {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let root_str = root.to_string_lossy().to_string();

    fauna_cfapi::register_sync_root(&root_str, "FaunaTest").expect("register_sync_root");
    let attrs_after_register = std::fs::symlink_metadata(&root)
        .expect("stat root")
        .file_attributes();

    let (tx, rx) = channel::<String>();
    let key = fauna_cfapi::connect(
        &root_str,
        Some(noop_cb),
        Some(noop_cb),
        Some(fetch_placeholders_cb),
        Some(noop_cb),
    )
    .expect("connect");
    cases().lock().unwrap().insert(key.0, (entries, tx));

    // MUST be another process: cfapi suppresses callbacks for the provider's own I/O.
    let out = std::process::Command::new("cmd")
        .args(["/c", "dir", "/b", &root_str])
        .output()
        .expect("spawn dir");
    let listing: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let callback = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| "NO CALLBACK — cfapi never asked us to populate".to_string());

    fauna_cfapi::disconnect(key);
    let _ = fauna_cfapi::unregister_sync_root(&root_str);
    cases().lock().unwrap().remove(&key.0);

    Outcome {
        callback,
        listing,
        attrs_after_register,
    }
}

fn one_file() -> Vec<PlaceholderInfo> {
    vec![PlaceholderInfo {
        rel_path: "hello.txt".to_string(),
        size: 5,
        mtime: 1_700_000_000, // Unix SECONDS
        is_dir: false,
    }]
}

/// **The core contract.** A connected sync root, browsed by anyone, answers
/// FETCH_PLACEHOLDERS with `CfExecute(TRANSFER_PLACEHOLDERS)` — and that call SUCCEEDS,
/// materializing a file that exists nowhere on disk. This is the test that would have caught
/// the `FileIdentity` bug the day it shipped; it failed `0x8007017C` before the fix.
#[test]
fn transfer_placeholders_succeeds_against_a_real_sync_root() {
    let outcome = run_case(one_file());

    assert!(
        outcome.callback.starts_with("OK"),
        "CfExecute(TRANSFER_PLACEHOLDERS) must succeed against a real sync root, got: {}\n\
         (a 0x8007017C here means a placeholder carried an empty FileIdentity)",
        outcome.callback
    );
    assert!(
        outcome.callback.contains("processed=1"),
        "the one placeholder we offered must be accepted, got: {}",
        outcome.callback
    );
    assert_eq!(
        outcome.listing,
        vec!["hello.txt".to_string()],
        "the placeholder must appear in another process's directory listing — that is the \
         whole point of on-demand population, and it is what the user sees in Explorer"
    );
}

/// A **brand-new folder is empty**, so a zero-entry transfer is the very first call a new
/// user hits. It must still complete the operation (marking the directory populated-but-empty)
/// rather than fail — cfapi wants a NULL `PlaceholderArray` for the empty case, not the
/// dangling-but-non-null pointer an empty `Vec` hands out.
#[test]
fn an_empty_placeholder_set_still_completes_the_operation() {
    let outcome = run_case(Vec::new());

    assert!(
        outcome.callback.starts_with("OK"),
        "an empty folder must populate cleanly (it is what every new user hits first), \
         got: {}",
        outcome.callback
    );
    assert!(
        outcome.listing.is_empty(),
        "an empty folder must leave the folder empty, got: {:?}",
        outcome.listing
    );
}

/// **Registration alone makes the root on-demand-populatable** — the fact that resolves the
/// contradiction which stalled this track: the running product's root carried placeholder
/// attributes even though the service calls neither `convert_to_placeholder` nor
/// `create_placeholder` anywhere. `CfRegisterSyncRoot` is what does it, via
/// `CF_POPULATION_POLICY_FULL` (the opt-*out* being
/// `CF_REGISTER_FLAG_DISABLE_ON_DEMAND_POPULATION_ON_ROOT`, which we don't pass).
///
/// If this ever fails, FETCH_PLACEHOLDERS will go silent and on-demand sync will die — do NOT
/// "fix" that by converting the root; converting an already-placeholder root fails 0x8007017C,
/// which is the false trail that cost a whole session.
#[test]
fn registering_a_sync_root_makes_it_an_on_demand_placeholder() {
    let outcome = run_case(one_file());

    assert_ne!(
        outcome.attrs_after_register & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        0,
        "CfRegisterSyncRoot must leave the root RECALL_ON_DATA_ACCESS (attrs {:#x}) — that is \
         what makes Windows ask us to populate it",
        outcome.attrs_after_register
    );
    // A plain directory is not a reparse point; a cloud placeholder root becomes one.
    assert_eq!(
        outcome.attrs_after_register & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the reparse point appears at connect, not at register — if this changes, the \
         comment on register_sync_root is out of date (attrs {:#x})",
        outcome.attrs_after_register
    );
}

/// Register + connect a scratch sync root with no-op callbacks; returns (tempdir, root, key).
fn scratch_root() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    windows::Win32::Storage::CloudFilters::CF_CONNECTION_KEY,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let root_str = root.to_string_lossy().to_string();
    fauna_cfapi::register_sync_root(&root_str, "FaunaTest").expect("register_sync_root");
    let key = fauna_cfapi::connect(
        &root_str,
        Some(noop_cb),
        Some(noop_cb),
        Some(noop_cb),
        Some(noop_cb),
    )
    .expect("connect");
    (tmp, root, key)
}

/// Overwrite `path`'s bytes IN PLACE (no truncate, no replace), so a placeholder stays a
/// placeholder — the in-place edit the not-in-sync bit and the USN both record.
fn overwrite_in_place(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for in-place write");
    f.write_all(bytes).expect("in-place write");
}

/// **The file road to ✅: `convert_to_placeholder_anchored`, then a USN-conditioned
/// `set_in_sync`.** This is the repair the on-demand host runs when a replace-save editor
/// has destroyed a synced file's placeholder-ness (`file-sync.md` § Per-file sync-status
/// display). A convert that marked the file in-sync itself would take no USN condition, so
/// the road is split: anchor vouching for nothing, then the conditioned assertion.
///
/// The contract, headless against a real root:
/// 1. a plain ordinary file has no reparse point and **cannot** be dehydrated
///    (`0x80070178` "not a cloud file") — the "before" state the repair fixes;
/// 2. after the anchor it IS a reparse-point placeholder whose bytes are still local and
///    readable byte-for-byte — and it still refuses a dehydrate, because the anchor vouched
///    for nothing (`CF_INSYNC_POLICY_TRACK_ALL` refuses a not-in-sync placeholder);
/// 3. after `set_in_sync` at its current USN it can be dehydrated — the whole point.
///
/// In-process dehydrate is the *provider's own* I/O, which cfapi serves (unlike the
/// cross-process call that hangs — `cfapi_live_integration::diag_cross_process_dehydrate`).
#[test]
fn an_anchored_plain_file_frees_only_after_a_conditioned_in_sync_assertion() {
    let (_tmp, root, key) = scratch_root();

    // A plain, ordinary file — what a replace-save/truncate editor leaves behind.
    let file = root.join("edited.txt");
    let body = b"the user's freshly-synced local edit";
    std::fs::write(&file, body).unwrap();

    // (1) Before: no reparse point, and it cannot be dehydrated.
    let plain_attrs = std::fs::symlink_metadata(&file).unwrap().file_attributes();
    assert_eq!(
        plain_attrs & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "a plain file is not a placeholder (attrs {plain_attrs:#x})"
    );
    let before = fauna_cfapi::dehydrate_placeholder(&file);
    assert!(
        before.is_err(),
        "a plain (non-cloud) file must NOT be dehydratable before the anchor; got {before:?}"
    );

    // (2) Anchor it — folder-relative identity, NOT in-sync, bytes local.
    fauna_cfapi::convert_to_placeholder_anchored(&file, "edited.txt")
        .expect("convert_to_placeholder_anchored");
    let attrs = std::fs::symlink_metadata(&file).unwrap().file_attributes();
    assert_ne!(
        attrs & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the anchor must make it a reparse-point cloud placeholder (attrs {attrs:#x})"
    );
    assert_eq!(
        attrs & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        0,
        "the anchor keeps the bytes local (attrs {attrs:#x})"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        body,
        "the user's local bytes must survive the conversion verbatim"
    );
    let unvouched = fauna_cfapi::dehydrate_placeholder(&file);
    assert!(
        unvouched.is_err(),
        "an anchored-but-unvouched placeholder must refuse a dehydrate; got {unvouched:?}"
    );

    // (3) The conditioned assertion at the current USN releases it.
    let usn = fauna_cfapi::file_usn(&file).expect("file_usn");
    assert_ne!(usn, 0, "the temp volume keeps a USN journal");
    fauna_cfapi::set_in_sync(&file, usn).expect("set_in_sync at the current USN");
    fauna_cfapi::dehydrate_placeholder(&file)
        .expect("an in-sync placeholder must dehydrate (the whole point of the repair)");
    let after = std::fs::symlink_metadata(&file).unwrap().file_attributes();
    assert_ne!(
        after & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        0,
        "after dehydrate the local bytes are freed — recall-on-data-access set (attrs {after:#x})"
    );

    fauna_cfapi::disconnect(key);
    let _ = fauna_cfapi::unregister_sync_root(&root.to_string_lossy());
}

/// **`set_in_sync` refuses a write newer than its USN — the OS fact the whole fix
/// rests on.** The host reads a file's USN, proves the content (a hash), then asserts
/// in-sync at that USN; a save landing anywhere after the USN read must make the platform
/// refuse, so the not-in-sync bit stands and the next dehydrate refuses too. Measured here
/// against a real root: the stale-USN assertion fails, the newer bytes stay local and
/// un-freeable, and the assertion at the fresh USN succeeds.
#[test]
fn set_in_sync_refuses_a_write_newer_than_its_usn() {
    let (_tmp, root, key) = scratch_root();

    let file = root.join("notes.txt");
    std::fs::write(&file, b"E1: the content the provider proved").unwrap();
    fauna_cfapi::convert_to_placeholder_anchored(&file, "notes.txt").expect("anchor");

    let proven_at = fauna_cfapi::file_usn(&file).expect("file_usn");
    // A save lands after the USN read (during or after the proof).
    overwrite_in_place(&file, b"E2: a newer save, never proven or uploaded");
    let moved = fauna_cfapi::file_usn(&file).expect("file_usn after the write");
    assert_ne!(
        moved, proven_at,
        "an in-place write must advance the file's USN"
    );

    let stale = fauna_cfapi::set_in_sync(&file, proven_at);
    assert!(
        stale.is_err(),
        "an in-sync assertion at a USN older than the file's newest write must be refused"
    );
    let freed = fauna_cfapi::dehydrate_placeholder(&file);
    assert!(
        freed.is_err(),
        "with the assertion refused the newer bytes must stay un-freeable; got {freed:?}"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"E2: a newer save, never proven or uploaded",
        "the newer save must still be on disk"
    );

    // Unconditioned (USN 0) is refused outright — it would drop the guard silently.
    assert!(
        fauna_cfapi::set_in_sync(&file, 0).is_err(),
        "USN 0 means 'no condition' to cfapi and must never be sent"
    );

    // At the fresh USN the assertion holds.
    fauna_cfapi::set_in_sync(&file, moved).expect("set_in_sync at the fresh USN");
    fauna_cfapi::dehydrate_placeholder(&file).expect("dehydrate after the fresh assertion");

    fauna_cfapi::disconnect(key);
    let _ = fauna_cfapi::unregister_sync_root(&root.to_string_lossy());
}

/// **`convert_to_placeholder_in_sync` is directories-only.** It marks in-sync with no USN
/// condition, so on a file it would vouch for whatever bytes were on disk; it must refuse a file, and still re-anchor a directory — the folder-✅ leg.
#[test]
fn convert_to_placeholder_in_sync_refuses_a_file_and_anchors_a_directory() {
    let (_tmp, root, key) = scratch_root();

    let file = root.join("plain.txt");
    std::fs::write(&file, b"bytes").unwrap();
    assert!(
        fauna_cfapi::convert_to_placeholder_in_sync(&file, "plain.txt").is_err(),
        "a file must never be marked in-sync by an unconditioned convert"
    );
    let attrs = std::fs::symlink_metadata(&file).unwrap().file_attributes();
    assert_eq!(
        attrs & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the refused file is untouched"
    );

    let dir = root.join("sub");
    std::fs::create_dir_all(&dir).unwrap();
    fauna_cfapi::convert_to_placeholder_in_sync(&dir, "sub").expect("a directory re-anchors");
    let dir_attrs = std::fs::symlink_metadata(&dir).unwrap().file_attributes();
    assert_ne!(
        dir_attrs & FILE_ATTRIBUTE_REPARSE_POINT,
        0,
        "the directory is a cloud placeholder now (attrs {dir_attrs:#x})"
    );

    fauna_cfapi::disconnect(key);
    let _ = fauna_cfapi::unregister_sync_root(&root.to_string_lossy());
}

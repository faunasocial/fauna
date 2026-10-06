//! MEASUREMENT PROBE (`#[ignore]`d): which query surface actually sees a
//! `StorageProviderSyncRootManager::Register`-ed root?
//!
//! Written 2026-07-16 when the first `GetCurrentSyncRoots`-based enumeration
//! returned `[]` for roots that had demonstrably just registered (their
//! unregister-by-id succeeded). Run it to re-measure rather than trust prose:
//!
//! ```text
//! cmd /c "scripts\cargo-win.cmd test -p fauna-cfapi --test shell_enumeration_probe -- --ignored --nocapture"
//! ```

#![cfg(windows)]

use windows::Storage::Provider::StorageProviderSyncRootManager;
use windows::Storage::StorageFolder;
use windows::core::HSTRING;

#[test]
#[ignore = "measurement probe: run with --ignored --nocapture"]
fn measure_shell_enumeration_surfaces() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("probe-root");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dir_str = dir.to_string_lossy().to_string();

    let id = fauna_cfapi::register_sync_root_with_shell(&dir_str, "Fauna probe", "enum-probe")
        .expect("register with shell");
    eprintln!("PROBE: registered id={id}");

    // Surface 1: GetCurrentSyncRoots.
    match StorageProviderSyncRootManager::GetCurrentSyncRoots() {
        Ok(roots) => {
            eprintln!("PROBE: GetCurrentSyncRoots ok, Size={:?}", roots.Size());
            match roots.Size() {
                Ok(n) => {
                    for i in 0..n {
                        match roots.GetAt(i) {
                            Ok(info) => eprintln!(
                                "PROBE:   [{i}] id={:?} path={:?}",
                                info.Id().map(|h| h.to_string()),
                                info.Path().and_then(|f| f.Path()).map(|h| h.to_string()),
                            ),
                            Err(e) => eprintln!("PROBE:   [{i}] GetAt failed: {e:?}"),
                        }
                    }
                }
                Err(e) => eprintln!("PROBE: Size failed: {e:?}"),
            }
        }
        Err(e) => eprintln!("PROBE: GetCurrentSyncRoots FAILED: {e:?}"),
    }

    // Surface 2: GetSyncRootInformationForFolder.
    match StorageFolder::GetFolderFromPathAsync(&HSTRING::from(dir_str.as_str()))
        .and_then(|op| op.get())
    {
        Ok(folder) => {
            match StorageProviderSyncRootManager::GetSyncRootInformationForFolder(&folder) {
                Ok(info) => eprintln!(
                    "PROBE: GetSyncRootInformationForFolder id={:?}",
                    info.Id().map(|h| h.to_string())
                ),
                Err(e) => eprintln!("PROBE: GetSyncRootInformationForFolder FAILED: {e:?}"),
            }
        }
        Err(e) => eprintln!("PROBE: GetFolderFromPathAsync FAILED: {e:?}"),
    }

    // Surface 3: the SyncRootManager registry key (what read).
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager",
        ])
        .output()
        .expect("reg query");
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines().filter(|l| l.contains("Fauna")) {
        eprintln!("PROBE: registry: {}", line.trim());
    }
    if !text.contains("Fauna") {
        eprintln!("PROBE: registry: NO Fauna entries under SyncRootManager");
    }

    // Cleanup: filter first (Register also filter-registers), then shell.
    let _ = fauna_cfapi::unregister_sync_root(&dir_str);
    fauna_cfapi::unregister_sync_root_with_shell(&id).expect("unregister shell");
    eprintln!("PROBE: cleaned up");
}

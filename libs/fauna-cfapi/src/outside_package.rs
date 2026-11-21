//! `HKEY_CURRENT_USER` deletes that reach the real hive from inside a package.
//!
//! A process running with package identity deletes an `HKCU` key only from its
//! package's own registry view: the key is gone for that process and still
//! there for everyone else, Explorer included (`installers/windows.md` §
//! Direct download, 5, *Removal*, measured on Windows 2026-11-20). The direct
//! package keeps registry writes virtualized, and the Store channel cannot opt
//! out (the opt-out needs the `unvirtualizedResources` capability the Store
//! declines), so a packaged agent cannot make those deletes itself on either
//! channel. It hands them to a process that runs outside the package.
//!
//! **Which process, by measurement (Windows, Windows 11 26300, 2026-11-20).** A
//! probe registered as a direct-channel test package ran the same matrix as its
//! package-root agent under three launches: a real shell activation, the e2e
//! harness's `Invoke-CommandInDesktopPackage` chain, and that chain with
//! `-PreventBreakaway` (children kept inside by the parent's policy). Each
//! experiment deleted real keys the driver had made from outside, and the
//! driver read from outside which were gone:
//!
//! * an in-process `RegDeleteTreeW` reached nothing real, through
//!   `HKEY_CURRENT_USER` or through `HKEY_USERS\<SID>` (and `<SID>_Classes`);
//! * the agent's own exe, a file inside the package, ran WITH the package
//!   identity whatever it was started with — any desktop-app policy, or
//!   `CREATE_BREAKAWAY_FROM_JOB` — so it cannot be the deleter;
//! * System32 `reg.exe`, a file outside the package, started with no attribute
//!   broke away under the first two launches and stayed inside under the
//!   third; started with `PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY` =
//!   `PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE` it broke away
//!   under all three, and its deletes reached the real hive. With
//!   `..._DISABLE_PROCESS_TREE` or `..._OVERRIDE` it stayed inside.
//!
//! So the deleter is `reg.exe` started with that policy.

use anyhow::{Context, Result};
use windows::Win32::Foundation::{
    APPMODEL_ERROR_NO_PACKAGE, CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY, PROCESS_INFORMATION, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// `PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE` (winbase.h,
/// documented under `UpdateProcThreadAttribute`), which the `windows` crate
/// does not bind.
const PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE: u32 = 0x01;

/// How long one `reg.exe` may take before it is ended and the delete counted
/// failed. A delete of one key returns in milliseconds; the bound only keeps a
/// wedged child from holding the agent's teardown.
const REG_EXE_BUDGET_MS: u32 = 30_000;

/// Whether this process runs with package identity: `GetCurrentPackageFullName`
/// asked for the length alone answers `ERROR_INSUFFICIENT_BUFFER` in a package
/// and `APPMODEL_ERROR_NO_PACKAGE` outside one. Any other answer is an error,
/// never a guess.
pub(crate) fn running_in_package() -> Result<bool> {
    let mut len = 0u32;
    match unsafe { GetCurrentPackageFullName(&mut len, None) } {
        ERROR_INSUFFICIENT_BUFFER => Ok(true),
        APPMODEL_ERROR_NO_PACKAGE => Ok(false),
        other => anyhow::bail!("GetCurrentPackageFullName answered {other:?}"),
    }
}

/// Delete `HKEY_CURRENT_USER\<sub>` and everything under it in the real hive,
/// through `reg.exe` started outside this process's package (module doc). An
/// absent key is already deleted.
///
/// `reg.exe delete` answers 1 for a key that is not there as for a key it could
/// not delete, so a non-zero answer is followed by `reg.exe query`, which
/// answers 0 only while the key stands. Not measured: a `reg.exe` an
/// administrator's policy disables, which may answer both calls 1 and so read
/// as an absent key.
pub(crate) fn delete_hkcu_tree(sub: &str) -> Result<()> {
    anyhow::ensure!(
        is_plain_key_path(sub),
        "refusing to hand reg.exe the key path {sub:?}"
    );
    let reg = reg_exe()?;
    let key = format!(r"HKCU\{sub}");
    let deleted = run_outside_package(&reg, &reg_command_line(&reg, "delete", &key))
        .with_context(|| format!("reg.exe delete {key}"))?;
    if deleted == 0 {
        return Ok(());
    }
    match run_outside_package(&reg, &reg_command_line(&reg, "query", &key))
        .with_context(|| format!("reg.exe query {key}"))?
    {
        0 => anyhow::bail!("reg.exe delete {key} answered {deleted} and the key stands"),
        _ => Ok(()),
    }
}

/// A path under a hive's root that `reg.exe` takes as one quoted argument:
/// non-empty, relative (no leading separator), no quote, no control character.
fn is_plain_key_path(sub: &str) -> bool {
    !sub.is_empty()
        && !sub.starts_with('\\')
        && !sub.ends_with('\\')
        && !sub.chars().any(|c| c == '"' || c.is_control())
}

/// The command line of one `reg.exe` call on `key` (`HKCU\…`): `delete` forced
/// (no prompt), `query` plain — `/f` would make a query a search.
fn reg_command_line(reg: &str, verb: &str, key: &str) -> String {
    match verb {
        "delete" => format!("\"{reg}\" delete \"{key}\" /f"),
        _ => format!("\"{reg}\" {verb} \"{key}\""),
    }
}

/// `%SystemRoot%\System32\reg.exe`, by its full path — never a bare `reg.exe`,
/// which `CreateProcessW` would look for in the package's own directory first.
/// `SystemRoot` is in every process's environment; the agent is a 64-bit
/// process, so `System32` is the native directory.
fn reg_exe() -> Result<String> {
    let root = std::env::var("SystemRoot").context("SystemRoot is not set")?;
    anyhow::ensure!(
        std::path::Path::new(&root).is_absolute(),
        "SystemRoot is not an absolute path: {root:?}"
    );
    Ok(format!(r"{}\System32\reg.exe", root.trim_end_matches('\\')))
}

/// Start `exe` with `command_line`, no window, outside this process's package
/// (`PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE`; module doc),
/// wait for it within [`REG_EXE_BUDGET_MS`] and return its exit code. A child
/// still running at the bound is ended and counted a failure.
fn run_outside_package(exe: &str, command_line: &str) -> Result<u32> {
    let exe_w: Vec<u16> = exe.encode_utf16().chain([0]).collect();
    let mut cmd_w: Vec<u16> = command_line.encode_utf16().chain([0]).collect();
    let attributes = BreakawayAttributes::new()?;
    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attributes.list;
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(cmd_w.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            &si.StartupInfo,
            &mut pi,
        )
    }
    .context("CreateProcessW")?;
    let _thread = OwnedHandle(pi.hThread);
    let process = OwnedHandle(pi.hProcess);
    let waited = unsafe { WaitForSingleObject(process.0, REG_EXE_BUDGET_MS) };
    if waited == WAIT_TIMEOUT {
        let _ = unsafe { TerminateProcess(process.0, 1) };
        anyhow::bail!("{exe} did not finish within {REG_EXE_BUDGET_MS} ms and was ended");
    }
    anyhow::ensure!(
        waited == WAIT_OBJECT_0,
        "waiting on {exe} answered {waited:?}"
    );
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(process.0, &mut code) }.context("GetExitCodeProcess")?;
    Ok(code)
}

/// A process-and-thread attribute list holding the one desktop-app policy,
/// deleted on drop. The policy value lives beside the list because the list
/// points at it until it is deleted.
struct BreakawayAttributes {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _buffer: Vec<u8>,
    _policy: Box<u32>,
}

impl BreakawayAttributes {
    fn new() -> Result<Self> {
        let mut size = 0usize;
        // Sizing call: answers ERROR_INSUFFICIENT_BUFFER by design, with `size` set.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
        anyhow::ensure!(size > 0, "InitializeProcThreadAttributeList sized nothing");
        let mut buffer = vec![0u8; size];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size) }
            .context("InitializeProcThreadAttributeList")?;
        let policy = Box::new(PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE);
        let this = Self {
            list,
            _buffer: buffer,
            _policy: policy,
        };
        unsafe {
            UpdateProcThreadAttribute(
                this.list,
                0,
                PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY as usize,
                Some(std::ptr::from_ref::<u32>(&this._policy).cast()),
                std::mem::size_of::<u32>(),
                None,
                None,
            )
        }
        .context("UpdateProcThreadAttribute(PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY)")?;
        Ok(this)
    }
}

impl Drop for BreakawayAttributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key path reaches `reg.exe` as one quoted argument, so nothing that
    /// could end the quote or name a hive's whole root passes.
    #[test]
    fn only_a_plain_relative_key_path_is_handed_to_reg_exe() {
        assert!(is_plain_key_path(
            r"Software\Classes\CLSID\{6840AAAA-0000-4000-8000-000000000001}"
        ));
        for not_one in [
            "",
            r"\Software",
            r"Software\",
            "Software\\x\" /f \"HKCU",
            "Software\nx",
        ] {
            assert!(!is_plain_key_path(not_one), "{not_one:?}");
        }
    }

    #[test]
    fn a_delete_is_forced_and_a_query_is_plain() {
        assert_eq!(
            reg_command_line(r"C:\W\System32\reg.exe", "delete", r"HKCU\Software\K"),
            r#""C:\W\System32\reg.exe" delete "HKCU\Software\K" /f"#
        );
        assert_eq!(
            reg_command_line(r"C:\W\System32\reg.exe", "query", r"HKCU\Software\K"),
            r#""C:\W\System32\reg.exe" query "HKCU\Software\K""#
        );
    }

    /// A test process runs with no package identity.
    #[test]
    fn a_test_process_is_not_in_a_package() {
        assert!(!running_in_package().expect("the package query answers"));
    }

    /// The child path deletes a real `HKCU` key and everything under it, and
    /// takes an absent key for a deleted one — from an unpackaged process, where
    /// the policy attribute is accepted and changes nothing. (Its effect inside
    /// a package is the module doc's measurement, witnessed by the e2e test
    /// `test_direct_package_agent_first_root.py`.)
    #[test]
    fn the_child_deletes_a_real_key_tree_and_an_absent_key_is_deleted() {
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, REG_OPTION_NON_VOLATILE, RegCloseKey,
            RegCreateKeyExW, RegOpenKeyExW,
        };
        use windows::core::HSTRING;
        let sub = format!(
            r"Software\FaunaCfapiOutsidePackageTest-{}",
            std::process::id()
        );
        let opens = |path: &str| {
            let mut key = HKEY::default();
            let ok = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(path),
                    None,
                    KEY_READ,
                    &mut key,
                )
            }
            .is_ok();
            if ok {
                let _ = unsafe { RegCloseKey(key) };
            }
            ok
        };
        let mut key = HKEY::default();
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(format!(r"{sub}\child")),
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_READ,
                None,
                &mut key,
                None,
            )
        }
        .ok()
        .expect("create the test key");
        let _ = unsafe { RegCloseKey(key) };
        assert!(opens(&sub));

        delete_hkcu_tree(&sub).expect("the child deletes the tree");
        assert!(!opens(&sub), "{sub} still stands after the child's delete");
        delete_hkcu_tree(&sub).expect("an absent key is already deleted");
    }
}

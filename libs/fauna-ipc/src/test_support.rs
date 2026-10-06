//! Test scaffolding this crate lends its consumers, gated like [`crate::endpoint`]'s
//! harness overrides: present in test/debug builds and wherever a consumer forwards
//! its own `test-helpers` feature, absent from a plain `--release` artifact.
//!
//! [`NamedMutexTestGuard`] is a cross-process critical section backed by a named
//! Win32 mutex, for serializing tests that touch shared, non-per-process OS state
//! (a registry hive, a Cloud Filter sync root under a fixed name) — state scoped
//! to the login session, not to a checkout or a process, so two `cargo test` runs
//! from different checkouts (or even two threads inside one test binary, since
//! the default harness runs tests concurrently) clobber each other without this.
//!
//! The pipe-DACL helpers below are the other kind of lent scaffolding: the shared
//! "walk a built [`pipe_transport::PipeSecurity`](crate::pipe_transport::PipeSecurity)
//! down to its DACL and inspect it" steps every pipe-DACL test needs.
//!
//! [`SYSTEM_SERVED_PIPES`] is the third: the pipes another account already serves,
//! which a server-identity test aims a real connect path at.

use std::pin::Pin;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, GetAce, GetAclInformation,
    GetSecurityDescriptorDacl, PSECURITY_DESCRIPTOR, PSID,
};
use windows::core::BOOL;

use crate::pipe_transport::PipeSecurity;

/// Holds a named mutex for the guard's lifetime, blocking [`acquire`](Self::acquire) until
/// it is free.
///
/// `Local\` scopes the name to this login session — the scope shared by every checkout's
/// `cargo test` run on this machine — no `Global\` (cross-session) privilege is needed. A
/// crashed test process still releases the mutex (the OS marks it abandoned; the next
/// `WaitForSingleObject` acquires it rather than hanging forever).
pub struct NamedMutexTestGuard(HANDLE);

impl NamedMutexTestGuard {
    /// Acquire (blocking) the named mutex `Local\<name>`.
    pub fn acquire(name: &str) -> Self {
        use windows::Win32::Foundation::WAIT_FAILED;
        use windows::Win32::System::Threading::{CreateMutexW, INFINITE, WaitForSingleObject};
        use windows::core::PCWSTR;

        let wide: Vec<u16> = format!("Local\\{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }
            .unwrap_or_else(|e| panic!("create named test mutex {name}: {e}"));
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        assert!(
            wait != WAIT_FAILED,
            "wait for named test mutex {name} failed"
        );
        Self(handle)
    }
}

impl Drop for NamedMutexTestGuard {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Foreign-served pipes
// ---------------------------------------------------------------------------

/// Leaf names of pipes Windows itself serves under a **different account**
/// (`NT AUTHORITY\SYSTEM`'s RPC endpoints) that any ordinary user may open.
///
/// Pointing a client's production connect path at one is exactly the shape of a
/// squatted `\\.\pipe\fauna-sync.<SID>` — a pipe answering under an account that is
/// not ours — with no second local account and no privilege
/// (`sync-agent.md` § Implementation status today, S1). Every consumer whose
/// test proves "we refuse a pipe someone else is serving" draws its candidates
/// from this one list, so a Windows retiring one name is fixed once.
///
/// It is a list, not a single name, on purpose: a test built on it must assert
/// that **at least one** candidate could be opened, or a future Windows that
/// retires them all would turn the test into a vacuous pass
/// (`e2e-conventions.md` convention 7 — a skip is not coverage).
pub const SYSTEM_SERVED_PIPES: &[&str] = &[
    "eventlog",
    "srvsvc",
    "wkssvc",
    "atsvc",
    "epmapper",
    "lsass",
    "InitShutdown",
    "LSM_API_service",
];

// ---------------------------------------------------------------------------
// Pipe-DACL test helpers
// ---------------------------------------------------------------------------
//
// The shared "walk a built `PipeSecurity` down to its DACL and inspect it"
// steps a pipe-DACL test needs — this crate's own `pipe_transport::dacl_tests`
// and the per-binary DACL tests used to hand-roll this identically. One owner here; callers add only their own
// policy assertion.

/// Walk a built [`PipeSecurity`] down to its DACL.
pub fn dacl_of(sec: &Pin<Box<PipeSecurity>>) -> *mut ACL {
    let sa = unsafe { &*sec.as_ptr() };
    let sd = PSECURITY_DESCRIPTOR(sa.lpSecurityDescriptor);

    let mut present = BOOL::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = BOOL::default();
    unsafe {
        GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)
            .expect("GetSecurityDescriptorDacl");
    }
    assert!(present.as_bool(), "DACL must be present");
    assert!(!dacl.is_null(), "DACL pointer must be non-null");
    dacl
}

/// The number of ACEs on `dacl`.
///
/// # Safety
/// `dacl` must be a valid `*mut ACL`, e.g. one [`dacl_of`] returned.
pub unsafe fn ace_count(dacl: *mut ACL) -> u32 {
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            dacl,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
        .expect("GetAclInformation");
    }
    info.AceCount
}

/// The SID of ACE `index`, as a raw `PSID` into the ACL's own storage.
///
/// # Safety
/// `dacl` must be a valid `*mut ACL`, e.g. one [`dacl_of`] returned, and
/// `index` must be within its `AceCount` (see [`ace_count`]).
pub unsafe fn ace_sid(dacl: *mut ACL, index: u32) -> PSID {
    let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
    unsafe {
        GetAce(dacl, index, &mut ace).expect("GetAce");
        let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
        PSID(&ace.SidStart as *const u32 as *mut _)
    }
}

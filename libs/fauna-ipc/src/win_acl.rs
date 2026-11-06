//! Owner-only file and directory ACLs on windows — the windows twin of a unix
//! `0o600` file / `0o700` directory, for a secret the per-user boundary holds.
//!
//! Windows has no mode bits. A new file takes the ACL its directory hands down,
//! so a secret written into a directory whose inheritable ACL names anyone but
//! its owner — or names the owner in a form a consumer refuses — is not the
//! owner-only file the caller meant. Measured 2026-11-05: an SSH door key
//! written into a pytest temp directory inherited `OWNER RIGHTS` (S-1-3-4),
//! which the Windows OpenSSH client refuses ("Bad permissions"), while the same
//! key under a stock `%USERPROFILE%\.ssh` inherited the owner, SYSTEM and
//! Administrators and was accepted. Inheritance makes the outcome the
//! directory's choice; [`restrict_to_owner`] makes it the caller's.
//!
//! The trustee is the current process's token user and nobody else — the one
//! [`crate::pipe_transport::PipeSecurity::for_owner`] grants the pipe, and the
//! one the Windows OpenSSH client accepts on a private key. As there, there is
//! deliberately no parameter to widen it.

use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::core::{HSTRING, PWSTR};

/// `FILE_ALL_ACCESS` — what the owner is granted: the specific rights `(F)`
/// names, so the stored ACE reads the same to every reader without a
/// generic-rights mapping.
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;

/// Replace `path`'s DACL with a PROTECTED one (nothing inherited from its
/// parent) granting the current process's token user full control and no other
/// trustee. A directory's grant is inheritable by the files and directories
/// created in it afterwards, as a `0o700` directory's are owner-only by
/// default; a file's is not.
///
/// The caller owns the object, so it holds `WRITE_DAC` whatever the old DACL
/// said. A handle already open on `path` keeps the access it was granted:
/// create the file, restrict it, then write the secret, and no reader can ever
/// open it under the inherited ACL with the secret inside.
pub fn restrict_to_owner(path: &Path) -> Result<()> {
    let is_dir = std::fs::metadata(path)
        .with_context(|| format!("metadata of {}", path.display()))?
        .is_dir();
    let inheritance = if is_dir {
        SUB_CONTAINERS_AND_OBJECTS_INHERIT
    } else {
        NO_INHERITANCE
    };
    unsafe {
        // `token_info` must outlive `user_sid`, a `PSID` borrowing into it.
        let token_info = crate::win_token::current_user_token_info()?;
        let user_sid = crate::win_token::token_user_sid(&token_info);

        let entries = [EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: SET_ACCESS,
            grfInheritance: inheritance,
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: PWSTR(user_sid.0 as *mut u16),
                ..Default::default()
            },
        }];
        let mut acl = std::ptr::null_mut::<ACL>();
        let err = SetEntriesInAclW(Some(&entries), None, &mut acl);
        if err.0 != 0 {
            anyhow::bail!("SetEntriesInAclW failed: error {}", err.0);
        }

        let set = SetNamedSecurityInfoW(
            &HSTRING::from(path.as_os_str()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl),
            None,
        );
        let _ = LocalFree(Some(HLOCAL(acl as *mut _)));
        if set.0 != 0 {
            anyhow::bail!(
                "SetNamedSecurityInfoW on {} failed: error {}",
                path.display(),
                set.0
            );
        }
    }
    Ok(())
}

/// Whether `path`'s DACL grants the current process's token user and no other
/// trustee — every ACE names that user, whether set here or inherited from a
/// directory [`restrict_to_owner`] restricted. An empty or absent DACL is not
/// owner-only (absent grants everyone; empty grants no one, the owner
/// included, but for its implicit `WRITE_DAC`).
pub fn is_owner_only(path: &Path) -> Result<bool> {
    unsafe {
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let err = GetNamedSecurityInfoW(
            &HSTRING::from(path.as_os_str()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        );
        if err.0 != 0 {
            anyhow::bail!(
                "GetNamedSecurityInfoW on {} failed: error {}",
                path.display(),
                err.0
            );
        }
        // `dacl` points into `sd`, which is freed below whatever the answer.
        let answer = dacl_names_only_the_user(dacl);
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        answer
    }
}

/// # Safety
/// `dacl` must be null or a valid `*mut ACL` that outlives the call.
unsafe fn dacl_names_only_the_user(dacl: *mut ACL) -> Result<bool> {
    if dacl.is_null() {
        return Ok(false);
    }
    unsafe {
        let mut info = ACL_SIZE_INFORMATION::default();
        GetAclInformation(
            dacl,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
        .context("GetAclInformation")?;
        if info.AceCount == 0 {
            return Ok(false);
        }
        let token_info = crate::win_token::current_user_token_info()?;
        let user = crate::win_token::token_user_sid(&token_info);
        for index in 0..info.AceCount {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            GetAce(dacl, index, &mut ace).context("GetAce")?;
            // ACCESS_ALLOWED_ACE and ACCESS_DENIED_ACE share this layout; a
            // deny ACE for another trustee still names someone else, so it
            // fails the test like an allow would.
            let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
            let sid = PSID(&ace.SidStart as *const u32 as *mut _);
            if EqualSid(sid, user).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file under a temp directory inherits the directory's ACEs; restricted,
    /// it grants its owner alone. Red-verify: without the restrict the file
    /// reads its inherited ACEs (SYSTEM and Administrators beside the user under
    /// `%TEMP%`), so the beside-control below holds and the last assert reds.
    #[test]
    fn a_restricted_file_grants_its_owner_and_nobody_else() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret");
        std::fs::write(&file, b"").unwrap();
        assert!(
            !is_owner_only(&file).unwrap(),
            "beside-control: an unrestricted file under the temp dir must carry its \
             inherited, wider ACL"
        );

        restrict_to_owner(&file).unwrap();

        assert!(is_owner_only(&file).unwrap());
    }

    /// A restricted directory hands its owner-only grant down: a file created in
    /// it afterwards is the owner's alone, as a file in a `0o700` directory is.
    #[test]
    fn a_file_created_in_a_restricted_directory_inherits_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join(".ssh");
        std::fs::create_dir(&sub).unwrap();
        restrict_to_owner(&sub).unwrap();
        assert!(is_owner_only(&sub).unwrap());

        let file = sub.join("config");
        std::fs::write(&file, b"").unwrap();
        assert!(is_owner_only(&file).unwrap());
    }
}

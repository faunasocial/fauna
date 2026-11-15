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
//!
//! [`copy_dacl`] is the other half of a unix file's mode: a file rewritten
//! through a temp sibling keeps the DACL its owner gave the old one, whatever
//! that is — it grants nobody the old file did not.

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
/// said. A handle already open on `path` keeps the access it was granted — a
/// reader's included, if the inherited ACL let it open the file before this
/// call. So for a secret: create the file with NO sharing (`share_mode(0)`) and
/// hold that handle, restrict it (this by-path set asks for no data access,
/// which no share mode refuses), then write the secret. Then no other handle
/// can exist on the file until the DACL governs it.
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

/// Give `to` the DACL `from` carries — its ACEs, and whether it is PROTECTED
/// (nothing inherited from its parent) — the windows twin of copying a unix
/// file's mode onto its replacement. For a file rewritten through a temp
/// sibling and a rename: the rename keeps the temp file's own DACL, so copying
/// the old file's onto it first keeps the admin's choice. An unprotected DACL is
/// set unprotected, so its inherited ACEs come again from `to`'s parent — the
/// same ones, for a sibling in the same directory.
///
/// As [`restrict_to_owner`], this by-path set asks for no data access, so it
/// lands on a file held open with no sharing.
pub fn copy_dacl(from: &Path, to: &Path) -> Result<()> {
    unsafe {
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let err = GetNamedSecurityInfoW(
            &HSTRING::from(from.as_os_str()),
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
                from.display(),
                err.0
            );
        }
        // `dacl` points into `sd`, freed below whatever the outcome.
        let set = (|| -> Result<()> {
            let mut control: u16 = 0;
            let mut revision: u32 = 0;
            GetSecurityDescriptorControl(sd, &mut control, &mut revision)
                .context("GetSecurityDescriptorControl")?;
            let protection = if control & SE_DACL_PROTECTED.0 != 0 {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
            let set = SetNamedSecurityInfoW(
                &HSTRING::from(to.as_os_str()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | protection,
                None,
                None,
                Some(dacl),
                None,
            );
            if set.0 != 0 {
                anyhow::bail!(
                    "SetNamedSecurityInfoW on {} failed: error {}",
                    to.display(),
                    set.0
                );
            }
            Ok(())
        })();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        set
    }
}

/// `path`'s DACL in SDDL (`D:P(A;;FA;;;S-1-5-…)`), its protection flag included
/// — the form in which two files' DACLs compare equal exactly when they grant the
/// same, inherited or not. The auto-inheritance bookkeeping flags (`AI`, `AR`) are
/// dropped: they record how the system propagated the ACEs, not what they grant,
/// and a by-path set marks `AI` where a freshly created file carries none
/// (measured 2026-11-13: the same inherited ACEs read `D:AI(…)` after
/// [`copy_dacl`] and `D:(…)` on the file it copied from).
pub fn dacl_sddl(path: &Path) -> Result<String> {
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let err = GetNamedSecurityInfoW(
            &HSTRING::from(path.as_os_str()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
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
        let mut text = PWSTR::null();
        let converted = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        );
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        converted.context("ConvertSecurityDescriptorToStringSecurityDescriptorW")?;
        let sddl = text.to_string();
        let _ = LocalFree(Some(HLOCAL(text.0 as *mut _)));
        let sddl = sddl.context("the DACL's SDDL is not UTF-16")?;
        Ok(without_auto_inherit_flags(&sddl))
    }
}

/// `D:<flags>(ace)…` with `AI` and `AR` taken out of the flags (see [`dacl_sddl`]).
fn without_auto_inherit_flags(sddl: &str) -> String {
    let Some(rest) = sddl.strip_prefix("D:") else {
        return sddl.to_string();
    };
    let (flags, aces) = rest.split_at(rest.find('(').unwrap_or(rest.len()));
    format!("D:{}{aces}", flags.replace("AI", "").replace("AR", ""))
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

    /// A copied DACL is the source's own, protection included, both ways: a
    /// protected owner-only grant lands on a file that inherited a wider one,
    /// and an unprotected inherited DACL lands back on a file that was
    /// restricted. Red-verify: without the copy each target keeps its own DACL,
    /// so the beside-controls hold and the equality asserts red.
    #[test]
    fn a_copied_dacl_is_the_source_files_own_protected_or_not() {
        let dir = tempfile::tempdir().unwrap();
        let narrowed = dir.path().join("narrowed");
        let inheriting = dir.path().join("inheriting");
        std::fs::write(&narrowed, b"").unwrap();
        std::fs::write(&inheriting, b"").unwrap();
        restrict_to_owner(&narrowed).unwrap();
        let narrowed_sddl = dacl_sddl(&narrowed).unwrap();
        let inheriting_sddl = dacl_sddl(&inheriting).unwrap();
        assert_ne!(
            narrowed_sddl, inheriting_sddl,
            "beside-control: the two files must start with different DACLs"
        );
        assert!(narrowed_sddl.starts_with("D:P"), "{narrowed_sddl}");
        assert!(!inheriting_sddl.starts_with("D:P"), "{inheriting_sddl}");

        // Protected onto inherited.
        let onto_inheriting = dir.path().join("onto-inheriting");
        std::fs::write(&onto_inheriting, b"").unwrap();
        copy_dacl(&narrowed, &onto_inheriting).unwrap();
        assert_eq!(dacl_sddl(&onto_inheriting).unwrap(), narrowed_sddl);
        assert!(is_owner_only(&onto_inheriting).unwrap());

        // Unprotected onto restricted: it inherits from the directory again.
        let onto_narrowed = dir.path().join("onto-narrowed");
        std::fs::write(&onto_narrowed, b"").unwrap();
        restrict_to_owner(&onto_narrowed).unwrap();
        copy_dacl(&inheriting, &onto_narrowed).unwrap();
        assert_eq!(dacl_sddl(&onto_narrowed).unwrap(), inheriting_sddl);
    }

    /// The bookkeeping flags go, the protection flag and every ACE stay.
    #[test]
    fn the_sddl_comparison_form_drops_only_the_auto_inherit_flags() {
        let ace = "(A;ID;FA;;;SY)";
        assert_eq!(
            without_auto_inherit_flags(&format!("D:AI{ace}")),
            format!("D:{ace}")
        );
        assert_eq!(
            without_auto_inherit_flags(&format!("D:PAI{ace}")),
            format!("D:P{ace}")
        );
        assert_eq!(
            without_auto_inherit_flags(&format!("D:PARAI{ace}")),
            format!("D:P{ace}")
        );
        assert_eq!(
            without_auto_inherit_flags(&format!("D:{ace}")),
            format!("D:{ace}")
        );
        assert_eq!(without_auto_inherit_flags("D:P"), "D:P");
    }
}

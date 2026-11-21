"""Whether only a file's owner can open it, as the platform spells it.

The app writes a secret — the SSH door key, a config holding it — owner-only:
``0o600`` (a directory ``0o700``) on unix; on windows, which has no mode bits, a
protected DACL naming the current user alone (``fauna_ipc::win_acl``). A test
reading the unix mode on windows reads ``0o666`` whatever the ACL says, so it
refuses every key the app wrote there; this module is the harness's one reading
of the same rule ``fauna_provisioning::ssh_door``'s ``assert_owner_only`` holds
the writer to.

Stdlib only (``ctypes`` on windows): it is imported on every OS at collection,
so the windows half loads lazily.
"""
from __future__ import annotations

import os
import stat
from pathlib import Path


def is_owner_only(path: str | os.PathLike, unix_mode: int | None = None) -> bool:
    """Whether ``path`` is readable by its owner alone.

    On unix: its permission bits are exactly ``unix_mode`` — ``0o600`` for a
    file and ``0o700`` for a directory when not given. On windows: its DACL
    holds at least one ACE and every ACE names the current process's token
    user, as ``fauna_ipc::win_acl::is_owner_only`` reads it (an absent DACL
    grants everyone, an empty one no one — neither is owner-only)."""
    p = Path(path)
    if os.name == "nt":
        return _win_is_owner_only(p)
    if unix_mode is None:
        unix_mode = 0o700 if p.is_dir() else 0o600
    return stat.S_IMODE(p.stat().st_mode) == unix_mode


def reading(path: str | os.PathLike) -> str:
    """What a failure message shows: the octal mode on unix, the DACL's SDDL on
    windows (``D:P(A;;FA;;;S-1-5-…)`` when restricted)."""
    p = Path(path)
    if os.name == "nt":
        return _win_dacl_sddl(p)
    return oct(stat.S_IMODE(p.stat().st_mode))


def restrict_to_owner(path: str | os.PathLike) -> None:
    """Make ``path`` owner-only, as the app's writer does — for a test fixture
    standing in for a file the app wrote. ``0o600``/``0o700`` on unix; on
    windows a protected DACL granting the current user full control and no one
    else (``fauna_ipc::win_acl::restrict_to_owner``), inheritable by what a
    directory holds."""
    p = Path(path)
    if os.name == "nt":
        _win_restrict_to_owner(p)
    else:
        p.chmod(0o700 if p.is_dir() else 0o600)


# ---- windows -------------------------------------------------------------

_SE_FILE_OBJECT = 1
_DACL_SECURITY_INFORMATION = 0x00000004
_PROTECTED_DACL_SECURITY_INFORMATION = 0x80000000
_ACL_SIZE_INFORMATION_CLASS = 2
_SDDL_REVISION_1 = 1
_TOKEN_QUERY = 0x0008
_TOKEN_USER_CLASS = 1
#: ``ACCESS_ALLOWED_ACE`` / ``ACCESS_DENIED_ACE``: a 4-byte header, a 4-byte
#: mask, then the SID. A deny ACE naming someone else still names someone else.
_ACE_SID_OFFSET = 8


def _win():
    import ctypes
    from ctypes import wintypes

    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    vp = ctypes.c_void_p
    advapi32.GetNamedSecurityInfoW.argtypes = [
        wintypes.LPCWSTR, ctypes.c_int, wintypes.DWORD, ctypes.POINTER(vp),
        ctypes.POINTER(vp), ctypes.POINTER(vp), ctypes.POINTER(vp), ctypes.POINTER(vp)]
    advapi32.GetNamedSecurityInfoW.restype = wintypes.DWORD
    advapi32.SetNamedSecurityInfoW.argtypes = [
        wintypes.LPWSTR, ctypes.c_int, wintypes.DWORD, vp, vp, vp, vp]
    advapi32.SetNamedSecurityInfoW.restype = wintypes.DWORD
    advapi32.GetAclInformation.argtypes = [vp, vp, wintypes.DWORD, ctypes.c_int]
    advapi32.GetAclInformation.restype = wintypes.BOOL
    advapi32.GetAce.argtypes = [vp, wintypes.DWORD, ctypes.POINTER(vp)]
    advapi32.GetAce.restype = wintypes.BOOL
    advapi32.EqualSid.argtypes = [vp, vp]
    advapi32.EqualSid.restype = wintypes.BOOL
    advapi32.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                          ctypes.POINTER(wintypes.HANDLE)]
    advapi32.OpenProcessToken.restype = wintypes.BOOL
    advapi32.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, vp,
                                             wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    advapi32.GetTokenInformation.restype = wintypes.BOOL
    advapi32.ConvertSidToStringSidW.argtypes = [vp, ctypes.POINTER(wintypes.LPWSTR)]
    advapi32.ConvertSidToStringSidW.restype = wintypes.BOOL
    advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, ctypes.POINTER(vp), ctypes.POINTER(wintypes.ULONG)]
    advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW.restype = wintypes.BOOL
    advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW.argtypes = [
        vp, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(wintypes.LPWSTR),
        ctypes.POINTER(wintypes.ULONG)]
    advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW.restype = wintypes.BOOL
    advapi32.GetSecurityDescriptorDacl.argtypes = [
        vp, ctypes.POINTER(wintypes.BOOL), ctypes.POINTER(vp), ctypes.POINTER(wintypes.BOOL)]
    advapi32.GetSecurityDescriptorDacl.restype = wintypes.BOOL
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.LocalFree.argtypes = [vp]
    kernel32.LocalFree.restype = vp
    return ctypes, wintypes, advapi32, kernel32


def _win_fail(ctypes, what: str, code: int | None = None) -> OSError:
    code = ctypes.get_last_error() if code is None else code
    return OSError(code, f"{what} failed: {ctypes.FormatError(code)}")


def _win_user_sid(ctypes, wintypes, advapi32, kernel32):
    """The current process's token user: ``(buffer, PSID)`` — the PSID points
    into the buffer, which must outlive it."""
    token = wintypes.HANDLE()
    if not advapi32.OpenProcessToken(kernel32.GetCurrentProcess(), _TOKEN_QUERY,
                                      ctypes.byref(token)):
        raise _win_fail(ctypes, "OpenProcessToken")
    try:
        size = wintypes.DWORD()
        advapi32.GetTokenInformation(token, _TOKEN_USER_CLASS, None, 0, ctypes.byref(size))
        buf = ctypes.create_string_buffer(size.value)
        if not advapi32.GetTokenInformation(token, _TOKEN_USER_CLASS, buf, size,
                                            ctypes.byref(size)):
            raise _win_fail(ctypes, "GetTokenInformation(TokenUser)")
    finally:
        kernel32.CloseHandle(token)
    # TOKEN_USER { SID_AND_ATTRIBUTES User { PSID Sid; DWORD Attributes } }
    return buf, ctypes.cast(buf, ctypes.POINTER(ctypes.c_void_p))[0]


def _win_is_owner_only(p: Path) -> bool:
    ctypes, wintypes, advapi32, kernel32 = _win()

    class _AclSizeInformation(ctypes.Structure):
        _fields_ = [("AceCount", wintypes.DWORD), ("AclBytesInUse", wintypes.DWORD),
                    ("AclBytesFree", wintypes.DWORD)]

    dacl, sd = ctypes.c_void_p(), ctypes.c_void_p()
    err = advapi32.GetNamedSecurityInfoW(str(p), _SE_FILE_OBJECT, _DACL_SECURITY_INFORMATION,
                                         None, None, ctypes.byref(dacl), None, ctypes.byref(sd))
    if err:
        raise _win_fail(ctypes, f"GetNamedSecurityInfoW on {p}", err)
    try:
        if not dacl.value:
            return False
        info = _AclSizeInformation()
        if not advapi32.GetAclInformation(dacl, ctypes.byref(info), ctypes.sizeof(info),
                                          _ACL_SIZE_INFORMATION_CLASS):
            raise _win_fail(ctypes, "GetAclInformation")
        if info.AceCount == 0:
            return False
        _keep, user = _win_user_sid(ctypes, wintypes, advapi32, kernel32)
        for index in range(info.AceCount):
            ace = ctypes.c_void_p()
            if not advapi32.GetAce(dacl, index, ctypes.byref(ace)):
                raise _win_fail(ctypes, "GetAce")
            if not advapi32.EqualSid(ctypes.c_void_p(ace.value + _ACE_SID_OFFSET), user):
                return False
        return True
    finally:
        kernel32.LocalFree(sd)


def _win_dacl_sddl(p: Path) -> str:
    ctypes, wintypes, advapi32, kernel32 = _win()
    dacl, sd = ctypes.c_void_p(), ctypes.c_void_p()
    err = advapi32.GetNamedSecurityInfoW(str(p), _SE_FILE_OBJECT, _DACL_SECURITY_INFORMATION,
                                         None, None, ctypes.byref(dacl), None, ctypes.byref(sd))
    if err:
        raise _win_fail(ctypes, f"GetNamedSecurityInfoW on {p}", err)
    try:
        text = wintypes.LPWSTR()
        if not advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd, _SDDL_REVISION_1, _DACL_SECURITY_INFORMATION, ctypes.byref(text), None):
            raise _win_fail(ctypes, "ConvertSecurityDescriptorToStringSecurityDescriptorW")
        try:
            return text.value
        finally:
            kernel32.LocalFree(text)
    finally:
        kernel32.LocalFree(sd)


def _win_restrict_to_owner(p: Path) -> None:
    ctypes, wintypes, advapi32, kernel32 = _win()
    _keep, user = _win_user_sid(ctypes, wintypes, advapi32, kernel32)
    sid_text = wintypes.LPWSTR()
    if not advapi32.ConvertSidToStringSidW(user, ctypes.byref(sid_text)):
        raise _win_fail(ctypes, "ConvertSidToStringSidW")
    try:
        sid = sid_text.value
    finally:
        kernel32.LocalFree(sid_text)
    # FA = FILE_ALL_ACCESS; OICI = inherited by the files and directories a
    # directory holds, as `SUB_CONTAINERS_AND_OBJECTS_INHERIT` is in the writer.
    inherit = "OICI" if p.is_dir() else ""
    sd = ctypes.c_void_p()
    if not advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW(
            f"D:P(A;{inherit};FA;;;{sid})", _SDDL_REVISION_1, ctypes.byref(sd), None):
        raise _win_fail(ctypes, "ConvertStringSecurityDescriptorToSecurityDescriptorW")
    try:
        present, defaulted, dacl = wintypes.BOOL(), wintypes.BOOL(), ctypes.c_void_p()
        if not advapi32.GetSecurityDescriptorDacl(sd, ctypes.byref(present),
                                                  ctypes.byref(dacl), ctypes.byref(defaulted)):
            raise _win_fail(ctypes, "GetSecurityDescriptorDacl")
        err = advapi32.SetNamedSecurityInfoW(
            str(p), _SE_FILE_OBJECT,
            _DACL_SECURITY_INFORMATION | _PROTECTED_DACL_SECURITY_INFORMATION,
            None, None, dacl, None)
        if err:
            raise _win_fail(ctypes, f"SetNamedSecurityInfoW on {p}", err)
    finally:
        kernel32.LocalFree(sd)

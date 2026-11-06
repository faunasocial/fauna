"""Which processes hold a file open, asked of the kernel.

Windows has no lock table to read: nothing here answers what ``/proc/locks``
answers on linux. What it does answer is what ``lsof`` answers on macOS, and
that is enough for a lock file a process opens only to lock it: a pid listed
here while the caller holds the lock can only be a process parked inside the
lock call. ``FileProcessIdsUsingFileInformation`` is a single call with no
session to end; Restart Manager's ``RmGetList`` gives the same answer, measured
on NTFS and ReFS against a process parked in ``LockFileEx``. Never Sysinternals
``handle.exe``, which would be an unvetted install on a dev machine.
"""

from __future__ import annotations

import ctypes
from ctypes import wintypes

_FILE_READ_ATTRIBUTES = 0x80
_FILE_SHARE_READ_WRITE_DELETE = 0x7
_OPEN_EXISTING = 3
_FILE_PROCESS_IDS_USING_FILE_INFORMATION = 47  # FILE_INFORMATION_CLASS, ntifs.h
_INVALID_HANDLE_VALUE = wintypes.HANDLE(-1).value


class _IoStatusBlock(ctypes.Structure):
    _fields_ = [("status", ctypes.c_void_p), ("information", ctypes.c_size_t)]


class _ProcessIdsUsingFile(ctypes.Structure):
    _fields_ = [("count", wintypes.ULONG), ("pids", ctypes.c_size_t * 256)]


# Bound lazily, never at module scope: `ctypes` has no `WinDLL` off Windows and
# this file collects on every host OS (`windows_session.py` measured it).
_dlls = None


def _bind():
    global _dlls
    if _dlls is None:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateFileW.argtypes = [
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.LPVOID,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HANDLE,
        ]
        kernel32.CreateFileW.restype = wintypes.HANDLE
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CloseHandle.restype = wintypes.BOOL
        ntdll = ctypes.WinDLL("ntdll")
        ntdll.NtQueryInformationFile.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(_IoStatusBlock),
            ctypes.c_void_p,
            wintypes.ULONG,
            ctypes.c_int,
        ]
        ntdll.NtQueryInformationFile.restype = ctypes.c_long
        _dlls = (kernel32, ntdll)
    return _dlls


def file_opener_pids(path) -> set[int]:
    """The pids holding ``path`` open. The caller's own pid is in the answer
    whenever it holds the file itself; this read's own handle does not appear."""
    kernel32, ntdll = _bind()
    handle = kernel32.CreateFileW(
        str(path),
        _FILE_READ_ATTRIBUTES,
        _FILE_SHARE_READ_WRITE_DELETE,
        None,
        _OPEN_EXISTING,
        0,
        None,
    )
    if handle == _INVALID_HANDLE_VALUE:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        info = _ProcessIdsUsingFile()
        status = ntdll.NtQueryInformationFile(
            handle,
            ctypes.byref(_IoStatusBlock()),
            ctypes.byref(info),
            ctypes.sizeof(info),
            _FILE_PROCESS_IDS_USING_FILE_INFORMATION,
        )
        if status != 0:
            raise OSError(
                f"NtQueryInformationFile({path}) answered NTSTATUS {status & 0xFFFFFFFF:#x}"
            )
        return {int(info.pids[i]) for i in range(info.count)}
    finally:
        kernel32.CloseHandle(handle)

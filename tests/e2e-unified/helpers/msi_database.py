"""A read-only view of a built MSI's database, through the Windows Installer API
(msi.dll via ctypes) — what the two MSI structure suites query
(`tests/platform/windows/test_installer_structure.py`, the full-feature package;
`test_nest_installer_structure.py`, the nest MSI). Windows only: msi.dll is
loaded at construction, never at import."""

import ctypes
from ctypes import wintypes


class MsiDatabase:
    """Read-only wrapper around an MSI database using msi.dll via ctypes."""

    def __init__(self, msi_path):
        self._msi = ctypes.windll.msi
        self._handle = wintypes.HANDLE()
        rc = self._msi.MsiOpenDatabaseW(msi_path, 0, ctypes.byref(self._handle))
        if rc != 0:
            raise RuntimeError(f"MsiOpenDatabase failed: rc={rc}")

    def query(self, sql):
        """Execute a SQL query and return rows as lists of strings."""
        hview = wintypes.HANDLE()
        rc = self._msi.MsiDatabaseOpenViewW(self._handle, sql, ctypes.byref(hview))
        if rc != 0:
            raise RuntimeError(f"MsiDatabaseOpenView failed: rc={rc} sql={sql}")
        self._msi.MsiViewExecute(hview, 0)

        ERROR_MORE_DATA = 234
        rows = []
        buf = ctypes.create_unicode_buffer(1024)
        while True:
            hrec = wintypes.HANDLE()
            if self._msi.MsiViewFetch(hview, ctypes.byref(hrec)) != 0:
                break
            field_count = self._msi.MsiRecordGetFieldCount(hrec.value)
            row = []
            for i in range(1, field_count + 1):
                sz = wintypes.DWORD(len(buf) - 1)
                rc = self._msi.MsiRecordGetStringW(hrec.value, i, buf, ctypes.byref(sz))
                if rc == ERROR_MORE_DATA:
                    # buf was too small (e.g. a long deferred-CA PowerShell Target) —
                    # sz now holds the required length (excl. null terminator);
                    # MSI truncates on overflow rather than erroring, so a caller that
                    # ignores rc silently gets a cut-off string (proven live 2026-07-17,
                    # a 1023-char CleanShellDlls Target read back missing its tail).
                    big = ctypes.create_unicode_buffer(sz.value + 1)
                    sz = wintypes.DWORD(len(big) - 1)
                    self._msi.MsiRecordGetStringW(hrec.value, i, big, ctypes.byref(sz))
                    row.append(big.value)
                else:
                    row.append(buf.value)
            self._msi.MsiCloseHandle(hrec.value)
            rows.append(row)

        self._msi.MsiCloseHandle(hview.value)
        return rows

    def close(self):
        if self._handle.value:
            self._msi.MsiCloseHandle(self._handle.value)
            self._handle.value = 0

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

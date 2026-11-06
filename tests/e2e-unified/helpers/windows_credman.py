"""The harness's own window on the box's real Windows Credential Manager.

Every ordinary windows e2e launch rests its credentials in the shared store's
file arm, so the suite never touches this store (convention 10). The one
sanctioned exception is convention 12's Windows arm: a witness whose subject is
the row Credential Manager really holds, which no file arm can stand in for.
This module is what such a witness reads with — the Win32 ``CredReadW`` /
``CredEnumerateW`` the store's own observer test uses
(``SharedSecretBackendTests``), an observer sharing no code with the writer
(``libs/fauna-credential-store/src/win_credman.rs``) — and what gives the box
its store back afterwards.

Credential Manager is one set per user: there is no private instance a test can
run for itself, the way linux's runs a private ``gnome-keyring-daemon``. So what
keeps a test off every real row is the namespace alone, and both ends are
fenced:

* :func:`refuse_unless_unique` is the launch end. A real-store launch names a
  namespace of the :func:`common.keyring.unique_namespace` shape under
  :data:`NAMESPACE_PREFIX`, or both drivers refuse it. The production
  namespaces (``fauna-windows``, the terminal app's, the sync agent's) cannot
  pass, and neither can a fixed name a sibling run would share.
* :func:`sweep` is the give-back end, and it refuses any prefix outside
  :data:`NAMESPACE_PREFIX`, so a harness bug cannot walk into a real row.

One witness cannot name a namespace at all: a Store build, whose override is
compiled out, writes the production ones. Its give-back is :func:`give_back`,
which deletes by exact target only the rows a snapshot taken before the run did
not hold (:func:`rows_gained`), and writes or restores nothing.

A row costs its share of a per-user budget of about 730 KB, and a full set
refuses every program's writes (`apps/windows.md` § Key storage), so a leaked
row is not inert debris — which is why a sweep polls until the prefix reads
empty rather than deleting once: Credential Manager loses overlapping updates,
and a deleted row can come back a moment later if another writer overlapped.

``ctypes`` is the whole dependency. The module imports on every platform (a
tier_1 test of the tui driver's refusal runs on any box); only a call that
reaches the store needs Windows.
"""

from __future__ import annotations

import ctypes
import re
from collections.abc import Iterable, Sequence
from ctypes import wintypes
from dataclasses import dataclass

from helpers.waiting import wait_until

CRED_TYPE_GENERIC = 1
CRED_PERSIST_SESSION = 1
CRED_PERSIST_LOCAL_MACHINE = 2
CRED_PERSIST_ENTERPRISE = 3

_ERROR_NOT_FOUND = 1168

#: Every namespace the harness may put on the box's real store starts here.
NAMESPACE_PREFIX = "fauna-e2e-"

#: :func:`common.keyring.unique_namespace`'s shape under that prefix: a stem,
#: then eight hex digits of a fresh uuid.
_UNIQUE_NAMESPACE = re.compile(rf"^{re.escape(NAMESPACE_PREFIX)}[a-z0-9-]+-[0-9a-f]{{8}}$")

#: How long a sweep waits for its prefix to read empty. Generous on purpose: it
#: is a bound on a store that is normally empty at the first read, and what it
#: has to outlast is a process still dying.
_SWEEP_BUDGET_S = 30


@dataclass(frozen=True)
class CredentialRow:
    """One generic credential, as ``CredReadW`` hands it back."""

    target: str
    type: int
    persist: int
    blob: bytes


class _Credential(ctypes.Structure):
    """Win32 ``CREDENTIALW``."""

    _fields_ = [
        ("Flags", wintypes.DWORD),
        ("Type", wintypes.DWORD),
        ("TargetName", wintypes.LPWSTR),
        ("Comment", wintypes.LPWSTR),
        ("LastWritten", wintypes.FILETIME),
        ("CredentialBlobSize", wintypes.DWORD),
        ("CredentialBlob", ctypes.POINTER(ctypes.c_ubyte)),
        ("Persist", wintypes.DWORD),
        ("AttributeCount", wintypes.DWORD),
        ("Attributes", ctypes.c_void_p),
        ("TargetAlias", wintypes.LPWSTR),
        ("UserName", wintypes.LPWSTR),
    ]


_CredentialPtr = ctypes.POINTER(_Credential)


def _advapi32():
    lib = ctypes.WinDLL("advapi32", use_last_error=True)
    lib.CredReadW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(_CredentialPtr),
    ]
    lib.CredReadW.restype = wintypes.BOOL
    lib.CredEnumerateW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD),
        ctypes.POINTER(ctypes.POINTER(_CredentialPtr)),
    ]
    lib.CredEnumerateW.restype = wintypes.BOOL
    lib.CredDeleteW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD]
    lib.CredDeleteW.restype = wintypes.BOOL
    lib.CredFree.argtypes = [ctypes.c_void_p]
    lib.CredFree.restype = None
    return lib


def _row(cred: _Credential) -> CredentialRow:
    size = cred.CredentialBlobSize
    blob = bytes(cred.CredentialBlob[:size]) if size and cred.CredentialBlob else b""
    return CredentialRow(cred.TargetName, cred.Type, cred.Persist, blob)


def refuse_unless_unique(keyring_app: str | None, launcher: str) -> None:
    """Refuse a real-store launch whose namespace is not unique to it.

    ``launcher`` names the driver mode in the refusal. Raised before anything
    starts, so a refused launch writes nothing."""
    if not _UNIQUE_NAMESPACE.match(keyring_app or ""):
        raise RuntimeError(
            f"{launcher} reaches the box's real Credential Manager, so it needs a "
            f"keyring_app of its own, from common.keyring.unique_namespace under "
            f"{NAMESPACE_PREFIX!r}; got {keyring_app!r}. Without one the launch "
            "would write into the installed product's rows or a sibling run's"
        )


def read_row(target: str) -> CredentialRow | None:
    """The generic credential named ``target``, or ``None`` if there is none."""
    lib = _advapi32()
    cred = _CredentialPtr()
    if not lib.CredReadW(target, CRED_TYPE_GENERIC, 0, ctypes.byref(cred)):
        error = ctypes.get_last_error()
        if error == _ERROR_NOT_FOUND:
            return None
        raise ctypes.WinError(error)
    try:
        return _row(cred.contents)
    finally:
        lib.CredFree(cred)


def rows_under(prefix: str) -> list[CredentialRow]:
    """Every credential whose target starts with ``prefix``."""
    lib = _advapi32()
    count = wintypes.DWORD()
    creds = ctypes.POINTER(_CredentialPtr)()
    if not lib.CredEnumerateW(f"{prefix}*", 0, ctypes.byref(count), ctypes.byref(creds)):
        error = ctypes.get_last_error()
        if error == _ERROR_NOT_FOUND:
            return []
        raise ctypes.WinError(error)
    try:
        return [_row(creds[i].contents) for i in range(count.value)]
    finally:
        lib.CredFree(creds)


def sweep(prefix: str) -> None:
    """Delete every row under ``prefix`` and wait for the prefix to read empty.

    Raises if it never does: a row left on the box's real store is a failure of
    the test that left it, never something to swallow in a teardown."""
    if not prefix.startswith(NAMESPACE_PREFIX) or prefix == NAMESPACE_PREFIX:
        raise RuntimeError(
            f"refusing to sweep {prefix!r} from the box's real Credential Manager: "
            f"the harness owns only namespaces under {NAMESPACE_PREFIX!r}, and only "
            "by a stem of their own"
        )
    lib = _advapi32()

    def emptied() -> bool:
        for row in rows_under(prefix):
            lib.CredDeleteW(row.target, row.type, 0)
        return not rows_under(prefix)

    wait_until(
        emptied, _SWEEP_BUDGET_S,
        diagnose=lambda: (
            "rows still under this prefix on the box's real Credential Manager: "
            f"{[row.target for row in rows_under(prefix)]}"
        ),
    )


#: A prefix :func:`give_back` walks: one namespace, slash-terminated. An empty or
#: unterminated prefix would enumerate other programs' rows too.
_NAMESPACE_DIR = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*/$")


def rows_gained(
    before: Iterable[CredentialRow], after: Iterable[CredentialRow]
) -> list[CredentialRow]:
    """The rows of ``after`` whose target ``before`` did not hold, by target.

    What a run owes back on a namespace it shares with the installed product: a
    row it added. A row it changed or removed is not in this set, and nothing
    here would write one back."""
    held = {row.target for row in before}
    return sorted((row for row in after if row.target not in held), key=lambda row: row.target)


def give_back(before: Sequence[CredentialRow], prefixes: Sequence[str]) -> None:
    """Delete every row under ``prefixes`` that ``before`` did not hold, and wait
    until none of them reads back.

    The give-back of a run whose rows land under namespaces the harness does not
    own, which :func:`sweep` refuses: a Store build's production ones. ``before``
    is the snapshot of the same prefixes taken before the run; only a target it
    did not hold is deleted, so a row the box held stays, and nothing is written
    or restored. Raises if a gained row will not stay gone, as :func:`sweep` does."""
    for prefix in prefixes:
        if not _NAMESPACE_DIR.match(prefix):
            raise RuntimeError(
                f"refusing to give back under {prefix!r}: a give-back walks whole "
                "namespaces, each named as '<namespace>/'"
            )
    lib = _advapi32()

    def rows_now() -> list[CredentialRow]:
        return [row for prefix in prefixes for row in rows_under(prefix)]

    def given_back() -> bool:
        for row in rows_gained(before, rows_now()):
            lib.CredDeleteW(row.target, row.type, 0)
        return not rows_gained(before, rows_now())

    wait_until(
        given_back, _SWEEP_BUDGET_S,
        diagnose=lambda: (
            "rows the run added still on the box's real Credential Manager: "
            f"{[row.target for row in rows_gained(before, rows_now())]}"
        ),
    )

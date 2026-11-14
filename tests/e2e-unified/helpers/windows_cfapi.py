"""What a user — or another tool — does to a windows cloud-files (cfapi) sync root,
driven from a test through the same Win32 calls, never through the agent.

A windows on-demand binding makes the bound directory a cfapi sync root the
agent registers and serves (`on-demand-files.md` § On-Demand Files). Explorer's
**"Free up space"** acts on such a root from OUTSIDE the agent: it is a pure
attribute write (the file goes *unpinned*), and the agent's pin reaction
observes it and frees the bytes (`bridge.rs::react_to_pin`).
:func:`free_up_space` writes the same attribute. Whether a directory IS such a
root is the cloud-files filter's own answer, read by :func:`is_sync_root`.

**Releasing what a finished run left** (:func:`release_sync_roots`). The agent
keeps a root's registration when its process stops — by design, so a service
restart re-connects (`cfapi_host.rs`, `TeardownMode::KeepUnlessMarked`). A root a
test bound under its ``tmp_path`` therefore outlives the run, and its
placeholders cannot be deleted (``WinError -2145452027``, 0x801F0005), so pytest
renames the dir to ``garbage-*`` and every later run on the box retries it. The
release is the product's own full-teardown order: the provider disconnected
(here: its process gone, which the filter reports as provider status
DISCONNECTED or TERMINATED), the filter registration (``CfUnregisterSyncRoot``),
then the shell registration by its path-keyed Id. Its callers: the e2e session's
end (`conftest.py`, after the session's apps and agents are gone), and the dev
machines' temp-dir sweep, which loads this file by path — so this module stays
stdlib-only. Owner:
`testing.md` § Test temp-dir hygiene.

**Staging a root without an agent** (:func:`register_sync_root`,
:func:`connect_sync_root`, :func:`disconnect_sync_root`): the calls a provider
makes, for a test that needs a registered or connected root and no agent — the
release's own tests stage every state it judges this way.

Process safety: this module spawns nothing.
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes
import os
import time
from pathlib import Path
from typing import NamedTuple

#: `FILE_ATTRIBUTE_PINNED` / `FILE_ATTRIBUTE_UNPINNED` — the two pin bits
#: Explorer's "Always keep on this device" / "Free up space" verbs write.
_FILE_ATTRIBUTE_PINNED = 0x00080000
_FILE_ATTRIBUTE_UNPINNED = 0x00100000
_INVALID_FILE_ATTRIBUTES = 0xFFFFFFFF

#: `CF_SYNC_ROOT_INFO_BASIC` — the smallest info class; its struct is one
#: `LARGE_INTEGER` (the root's file id), which this module never reads.
_CF_SYNC_ROOT_INFO_BASIC = 0
#: `HRESULT_FROM_WIN32(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT)` — the filter's "no".
_NOT_UNDER_SYNC_ROOT = 0x80070186


def _under_sync_root(path: Path) -> bool:
    """Whether the cloud-files filter places ``path`` inside a registered sync
    root — ``CfGetSyncRootInfoByPath``, the probe `fauna_cfapi::is_filter_registered`
    makes. Any answer but yes or no raises, naming the HRESULT."""
    cld = ctypes.WinDLL("cldapi")
    probe = cld.CfGetSyncRootInfoByPath
    probe.argtypes = [
        ctypes.wintypes.LPCWSTR,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.wintypes.DWORD,
        ctypes.POINTER(ctypes.wintypes.DWORD),
    ]
    # A plain signed result, never `ctypes.HRESULT`, which raises on the "no".
    probe.restype = ctypes.c_long
    info = ctypes.c_longlong()
    hr = probe(str(path), _CF_SYNC_ROOT_INFO_BASIC, ctypes.byref(info), ctypes.sizeof(info), None)
    if hr == 0:
        return True
    if hr & 0xFFFFFFFF == _NOT_UNDER_SYNC_ROOT:
        return False
    raise OSError(f"CfGetSyncRootInfoByPath({path}) failed: HRESULT 0x{hr & 0xFFFFFFFF:08X}")


def is_sync_root(path: Path) -> bool:
    """Whether ``path`` is itself a registered cfapi sync root right now — the
    windows on-demand root standing over a bound directory, as every other
    process on the box sees it (Explorer's cloud verbs key off the same
    registration). The filter answers "inside a root" for every path under one
    and cfapi refuses to nest roots, so ``path`` is the root exactly when it is
    inside one and its parent is not."""
    path = Path(path)
    return _under_sync_root(path) and not _under_sync_root(path.parent)


#: The provider name every Fauna root registers under (`cfapi_host.rs`
#: `SYNC_ROOT_DISPLAY`; the shell path's display name is the same word). A root
#: naming another provider is never released here.
FAUNA_PROVIDER_NAME = "Fauna"

#: `CF_SYNC_ROOT_INFO_PROVIDER` — `CF_SYNC_ROOT_PROVIDER_INFO`: the provider's
#: status (a `ULONG`) then its name (`WCHAR[256]`) and version (`WCHAR[256]`);
#: measured 2026-11-10 on a leftover root: 1028 bytes, `Fauna` at offset 4.
_CF_SYNC_ROOT_INFO_PROVIDER = 2
_PROVIDER_INFO_SIZE = 4 + 2 * 256 + 2 * 256
#: The provider statuses that mean no provider holds a connection to the root.
#: Measured 2026-11-10 against a provider process of our own: registered and
#: never connected, or cleanly disconnected (`CfDisconnectSyncRoot`), reads
#: DISCONNECTED (0); connected reads IDLE (1); a provider whose process ENDED
#: while connected — the app's tree kill taking its agent at a session's end —
#: reads TERMINATED (0xC0000001). Fauna's provider never sets its own status
#: (`fauna_cfapi` has no `CfUpdateSyncProviderStatus`), so every other value is
#: read as a live connection and spared.
_NO_PROVIDER_STATUSES = frozenset({0x00000000, 0xC0000001})

#: The attributes a directory scan sees for free on Windows and that every
#: registered root carries: registration sets RECALL_ON_DATA_ACCESS, a connected
#: provider adds REPARSE_POINT and OFFLINE (`fauna_cfapi::register_sync_root`,
#: measured). Only an entry carrying one of them is asked of the filter.
_FILE_ATTRIBUTE_REPARSE_POINT = 0x00000400
_FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS = 0x00400000

#: The sync-root manager's statics interface, `IStorageProviderSyncRootManagerStatics`,
#: and the vtable slot of its `Unregister(HSTRING id)`: IUnknown takes 0-2,
#: IInspectable 3-5, then `Register` 6 and `Unregister` 7. Both read off the
#: OS's own metadata (`Windows.Storage.winmd`, reflected 2026-11-10).
_SYNC_ROOT_MANAGER_CLASS = "Windows.Storage.Provider.StorageProviderSyncRootManager"
_IID_SYNC_ROOT_MANAGER_STATICS = (
    0x3E99FBBF, 0x8FE3, 0x4B40, (0xAB, 0xC7, 0xF6, 0xFC, 0x3D, 0x74, 0xC9, 0x8E),
)
_UNREGISTER_SLOT = 7
_RO_INIT_MULTITHREADED = 1

#: The registry home of shell sync-root registrations — the one surface an
#: unpackaged process can READ them from (`fauna_cfapi::SYNC_ROOT_MANAGER_KEY`
#: says why WinRT's getters answer empty here).
_SYNC_ROOT_MANAGER_KEY = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager"


class Provider(NamedTuple):
    """The provider of the root covering a path, as the filter reports it."""

    name: str
    status: int


class Released(NamedTuple):
    """What :func:`release_sync_roots` did: the roots it unregistered, the roots
    it left (each with the reason), and the released roots whose shell entry
    could not be removed (each with the failure — the tree removes all the same,
    and the entry becomes a ghost the agent's startup janitor sweeps)."""

    released: list[Path]
    spared: list[tuple[Path, str]]
    shell_kept: tuple[tuple[Path, str], ...] = ()


def sync_root_provider(path: Path) -> Provider | None:
    """The provider of the root ``path`` lies in (or is), or ``None`` when no
    root covers it — ``CfGetSyncRootInfoByPath`` with the provider info class.
    Any answer but those two raises, naming the HRESULT."""
    cld = ctypes.WinDLL("cldapi")
    probe = cld.CfGetSyncRootInfoByPath
    probe.argtypes = [
        ctypes.wintypes.LPCWSTR,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.wintypes.DWORD,
        ctypes.POINTER(ctypes.wintypes.DWORD),
    ]
    probe.restype = ctypes.c_long
    buf = ctypes.create_string_buffer(_PROVIDER_INFO_SIZE)
    got = ctypes.wintypes.DWORD(0)
    hr = probe(str(path), _CF_SYNC_ROOT_INFO_PROVIDER, buf, _PROVIDER_INFO_SIZE, ctypes.byref(got))
    if hr & 0xFFFFFFFF == _NOT_UNDER_SYNC_ROOT:
        return None
    if hr != 0:
        raise OSError(f"CfGetSyncRootInfoByPath({path}) failed: HRESULT 0x{hr & 0xFFFFFFFF:08X}")
    raw = buf.raw
    status = int.from_bytes(raw[0:4], "little")
    name = raw[4 : 4 + 512].decode("utf-16-le", "replace").split("\x00", 1)[0]
    return Provider(name, status)


def find_sync_roots(base: Path) -> list[Path]:
    """Every registered sync root at or under ``base``, in walk order.

    A top-down walk that never follows a link or junction and never descends
    into a root (cfapi refuses nested roots, and listing a root would ask its
    provider to populate it). Only an entry carrying a root's attributes is asked
    of the filter, so a tree without roots costs one directory scan. ``base``
    lying inside a root it is not itself answers nothing: that tree is the root's,
    not ours. An entry the walk cannot read is skipped."""
    base = Path(base)
    try:
        if _under_sync_root(base):
            return [base] if not _under_sync_root(base.parent) else []
    except OSError:
        return []
    found: list[Path] = []
    stack = [base]
    while stack:
        here = stack.pop()
        try:
            with os.scandir(here) as it:
                entries = sorted(it, key=lambda e: e.name)
        except OSError:
            continue
        below = []
        for entry in entries:
            try:
                if not entry.is_dir(follow_symlinks=False):
                    continue
                attrs = getattr(entry.stat(follow_symlinks=False), "st_file_attributes", 0)
            except OSError:
                continue
            path = Path(entry.path)
            if attrs & (_FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | _FILE_ATTRIBUTE_REPARSE_POINT):
                try:
                    if _under_sync_root(path):
                        found.append(path)
                        continue
                except OSError:
                    continue
                if attrs & _FILE_ATTRIBUTE_REPARSE_POINT:
                    continue  # a link or junction: never followed
            below.append(path)
        stack.extend(reversed(below))
    return found


def unregister_sync_root(root: Path) -> None:
    """Drop ``root``'s registration with the cloud-files filter
    (``CfUnregisterSyncRoot``) — which also removes its cloud-only placeholders,
    leaving hydrated files as plain files (measured). Only for a root no provider
    is connected to: the caller checks :func:`sync_root_provider` first."""
    cld = ctypes.WinDLL("cldapi")
    cld.CfUnregisterSyncRoot.argtypes = [ctypes.wintypes.LPCWSTR]
    cld.CfUnregisterSyncRoot.restype = ctypes.c_long
    hr = cld.CfUnregisterSyncRoot(str(root))
    if hr != 0:
        raise OSError(f"CfUnregisterSyncRoot({root}) failed: HRESULT 0x{hr & 0xFFFFFFFF:08X}")


class _GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", ctypes.c_ulong),
        ("Data2", ctypes.c_ushort),
        ("Data3", ctypes.c_ushort),
        ("Data4", ctypes.c_ubyte * 8),
    ]


class _Registration(ctypes.Structure):
    """`CF_SYNC_REGISTRATION`."""

    _fields_ = [
        ("StructSize", ctypes.wintypes.ULONG),
        ("ProviderName", ctypes.wintypes.LPCWSTR),
        ("ProviderVersion", ctypes.wintypes.LPCWSTR),
        ("SyncRootIdentity", ctypes.c_void_p),
        ("SyncRootIdentityLength", ctypes.wintypes.DWORD),
        ("FileIdentity", ctypes.c_void_p),
        ("FileIdentityLength", ctypes.wintypes.DWORD),
        ("ProviderId", _GUID),
    ]


class _Policies(ctypes.Structure):
    """`CF_SYNC_POLICIES`, flattened: each policy is a pair of `USHORT`s or a `ULONG`."""

    _fields_ = [
        ("StructSize", ctypes.wintypes.ULONG),
        ("HydrationPrimary", ctypes.c_ushort),
        ("HydrationModifier", ctypes.c_ushort),
        ("PopulationPrimary", ctypes.c_ushort),
        ("PopulationModifier", ctypes.c_ushort),
        ("InSync", ctypes.wintypes.ULONG),
        ("HardLink", ctypes.wintypes.ULONG),
        ("PlaceholderManagement", ctypes.wintypes.ULONG),
    ]


class _CallbackRegistration(ctypes.Structure):
    """`CF_CALLBACK_REGISTRATION` — only the table's terminator is ever passed."""

    _fields_ = [("Type", ctypes.wintypes.DWORD), ("Callback", ctypes.c_void_p)]


#: `fauna_cfapi`'s PROVIDER_GUID — the provider every Fauna root registers under.
_FAUNA_PROVIDER_ID = (0xFAD7A001, 0xFA07, 0x4E5A, (0xB0, 0x0D, 0xFA, 0x07, 0xA0, 0x01, 0x00, 0x01))
_CF_CALLBACK_TYPE_NONE = 0xFFFFFFFF


def register_sync_root(path: Path, provider: str = FAUNA_PROVIDER_NAME) -> None:
    """Register ``path`` with the cloud-files filter as a provider named
    ``provider`` would, with the policies `fauna_cfapi::register_sync_root` sets
    (hydration FULL, population FULL, in-sync TRACK_ALL)."""
    cld = ctypes.WinDLL("cldapi")
    cld.CfRegisterSyncRoot.argtypes = [
        ctypes.wintypes.LPCWSTR,
        ctypes.POINTER(_Registration),
        ctypes.POINTER(_Policies),
        ctypes.wintypes.DWORD,
    ]
    cld.CfRegisterSyncRoot.restype = ctypes.c_long
    d1, d2, d3, d4 = _FAUNA_PROVIDER_ID
    name = ctypes.create_unicode_buffer(provider)
    version = ctypes.create_unicode_buffer("1.0")
    registration = _Registration(
        StructSize=ctypes.sizeof(_Registration),
        ProviderName=ctypes.cast(name, ctypes.wintypes.LPCWSTR),
        ProviderVersion=ctypes.cast(version, ctypes.wintypes.LPCWSTR),
        ProviderId=_GUID(d1, d2, d3, (ctypes.c_ubyte * 8)(*d4)),
    )
    policies = _Policies(
        StructSize=ctypes.sizeof(_Policies),
        HydrationPrimary=2,  # CF_HYDRATION_POLICY_FULL
        PopulationPrimary=2,  # CF_POPULATION_POLICY_FULL
        InSync=0x00FFFFFF,  # CF_INSYNC_POLICY_TRACK_ALL
    )
    hr = cld.CfRegisterSyncRoot(str(path), ctypes.byref(registration), ctypes.byref(policies), 0)
    if hr != 0:
        raise OSError(f"CfRegisterSyncRoot({path}) failed: HRESULT 0x{hr & 0xFFFFFFFF:08X}")


def connect_sync_root(path: Path) -> int:
    """Connect to the root ``path`` as its provider, registering no callback —
    the connected state a live agent's root is in, for as long as the calling
    process holds it. Returns the connection key :func:`disconnect_sync_root` takes."""
    cld = ctypes.WinDLL("cldapi")
    cld.CfConnectSyncRoot.argtypes = [
        ctypes.wintypes.LPCWSTR,
        ctypes.POINTER(_CallbackRegistration),
        ctypes.c_void_p,
        ctypes.wintypes.DWORD,
        ctypes.POINTER(ctypes.c_longlong),
    ]
    cld.CfConnectSyncRoot.restype = ctypes.c_long
    table = (_CallbackRegistration * 1)(_CallbackRegistration(_CF_CALLBACK_TYPE_NONE, None))
    key = ctypes.c_longlong()
    hr = cld.CfConnectSyncRoot(str(path), table, None, 0, ctypes.byref(key))
    if hr != 0:
        raise OSError(f"CfConnectSyncRoot({path}) failed: HRESULT 0x{hr & 0xFFFFFFFF:08X}")
    return key.value


def disconnect_sync_root(key: int) -> None:
    """End the connection :func:`connect_sync_root` made."""
    cld = ctypes.WinDLL("cldapi")
    cld.CfDisconnectSyncRoot.argtypes = [ctypes.c_longlong]
    cld.CfDisconnectSyncRoot.restype = ctypes.c_long
    cld.CfDisconnectSyncRoot(key)


def unregister_shell_sync_root(sync_root_id: str) -> None:
    """Remove the shell registration ``sync_root_id`` (``Fauna!<SID>!<account>``)
    through `StorageProviderSyncRootManager.Unregister` — the call
    `fauna_cfapi::unregister_sync_root_with_shell` makes, reached here through
    the activation factory. It fails 0x8007017C while the root is still
    connected, so it comes last (`cfapi_host.rs`, the full-teardown order). An Id
    the manager does not hold answers 0x80070490 (ERROR_NOT_FOUND). Raises
    OSError naming the HRESULT on any failure."""
    combase = ctypes.WinDLL("combase")
    combase.RoInitialize.argtypes = [ctypes.c_int]
    combase.RoInitialize.restype = ctypes.c_long
    combase.RoUninitialize.argtypes = []
    combase.RoUninitialize.restype = None
    combase.WindowsCreateString.argtypes = [
        ctypes.wintypes.LPCWSTR,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    combase.WindowsCreateString.restype = ctypes.c_long
    combase.WindowsDeleteString.argtypes = [ctypes.c_void_p]
    combase.WindowsDeleteString.restype = ctypes.c_long
    combase.RoGetActivationFactory.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(_GUID),
        ctypes.POINTER(ctypes.c_void_p),
    ]
    combase.RoGetActivationFactory.restype = ctypes.c_long

    def hstring(text: str) -> ctypes.c_void_p:
        handle = ctypes.c_void_p()
        hr = combase.WindowsCreateString(text, len(text), ctypes.byref(handle))
        if hr != 0:
            raise OSError(f"WindowsCreateString failed: HRESULT 0x{hr & 0xFFFFFFFF:08X}")
        return handle

    # S_OK and S_FALSE are balanced by RoUninitialize; RPC_E_CHANGED_MODE (the
    # thread is already in another apartment) is usable as it stands.
    init = combase.RoInitialize(_RO_INIT_MULTITHREADED) & 0xFFFFFFFF
    class_name = hid = None
    factory = ctypes.c_void_p()
    try:
        class_name = hstring(_SYNC_ROOT_MANAGER_CLASS)
        d1, d2, d3, d4 = _IID_SYNC_ROOT_MANAGER_STATICS
        iid = _GUID(d1, d2, d3, (ctypes.c_ubyte * 8)(*d4))
        hr = combase.RoGetActivationFactory(class_name, ctypes.byref(iid), ctypes.byref(factory))
        if hr != 0 or not factory.value:
            raise OSError(
                f"RoGetActivationFactory({_SYNC_ROOT_MANAGER_CLASS}) failed: "
                f"HRESULT 0x{hr & 0xFFFFFFFF:08X}"
            )
        vtable = ctypes.cast(factory, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        unregister = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p)(
            vtable[_UNREGISTER_SLOT]
        )
        hid = hstring(sync_root_id)
        hr = unregister(factory, hid)
        if hr != 0:
            raise OSError(
                f"StorageProviderSyncRootManager.Unregister({sync_root_id}) failed: "
                f"HRESULT 0x{hr & 0xFFFFFFFF:08X}"
            )
    finally:
        if factory.value:
            vtable = ctypes.cast(factory, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
            ctypes.WINFUNCTYPE(ctypes.c_ulong, ctypes.c_void_p)(vtable[2])(factory)
        for handle in (hid, class_name):
            if handle is not None:
                combase.WindowsDeleteString(handle)
        if init in (0, 1):
            combase.RoUninitialize()


def list_shell_sync_roots() -> list[tuple[str, str | None]]:
    """Every Fauna shell registration as ``(Id, recorded folder)`` — the OS's own
    record, read the way `fauna_cfapi::list_shell_sync_roots` reads it: each
    ``Fauna!`` key's ``UserSyncRoots\\<SID>`` value. The recorded folder persists
    after the folder is gone; ``None`` when the entry names none. Read-only."""
    import winreg

    try:
        root = winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, _SYNC_ROOT_MANAGER_KEY)
    except FileNotFoundError:
        return []  # no shell registration on this machine at all
    out: list[tuple[str, str | None]] = []
    with root:
        index = 0
        while True:
            try:
                sync_root_id = winreg.EnumKey(root, index)
            except OSError:
                return out  # ERROR_NO_MORE_ITEMS
            index += 1
            if not sync_root_id.startswith("Fauna!"):
                continue
            sid = sync_root_id.split("!")[1]
            try:
                with winreg.OpenKey(root, rf"{sync_root_id}\UserSyncRoots") as user_roots:
                    registered, _kind = winreg.QueryValueEx(user_roots, sid)
            except OSError:
                registered = None
            out.append((sync_root_id, registered))


def _norm(path) -> str:
    return str(path).rstrip("\\/").lower()


def shell_sync_root_id_for(path) -> str | None:
    """The ``Fauna!<SID>!<account>`` Id Explorer's ``SyncRootManager`` holds for
    ``path``, or ``None`` when no Fauna shell registration covers it — compared
    case-insensitively with trailing separators ignored, as
    `fauna_cfapi::shell_registration_for` compares. Read-only."""
    wanted = _norm(path)
    for sync_root_id, registered in list_shell_sync_roots():
        if registered is not None and _norm(registered) == wanted:
            return sync_root_id
    return None


def ghost_shell_sync_roots_under(base) -> list[str]:
    """The Fauna shell registrations whose recorded folder lies under ``base``
    and no longer exists — the ghost a renamed or deleted test root leaves (the
    product's startup janitor removes ghosts machine-wide,
    `cfapi_host.rs::sweep_ghost_shell_registrations`; this names the ones under
    one tree). Read-only."""
    prefix = _norm(base) + "\\"
    return [
        sync_root_id
        for sync_root_id, registered in list_shell_sync_roots()
        if registered is not None
        and (_norm(registered) + "\\").startswith(prefix)
        and not os.path.exists(registered)
    ]


def release_sync_roots(base: Path, *, wait_s: float = 0.0) -> Released:
    """Release every Fauna sync root at or under ``base`` whose provider is gone,
    in the product's full-teardown order: the provider disconnected, then the
    filter registration, then the shell registration by its path-keyed Id
    (`cfapi_host.rs`, the `KeepUnlessMarked` arm). The tree itself is left to the
    caller: once released it removes like any other.

    Never touched: a root another provider registered, and a root a provider is
    still connected to — a live agent's. ``wait_s`` bounds how long a connected
    root is watched for its provider to go (the session's end, where the apps
    were just stopped); 0 judges each root once. Each spared root comes back
    with its reason; a release that fails is spared too, naming the failure."""
    released: list[Path] = []
    spared: list[tuple[Path, str]] = []
    shell_kept: list[tuple[Path, str]] = []
    deadline = time.monotonic() + wait_s
    for root in find_sync_roots(base):
        while True:
            try:
                provider = sync_root_provider(root)
            except OSError as exc:
                spared.append((root, str(exc)))
                break
            if provider is None:
                break  # released meanwhile, by its provider or another caller
            if provider.name != FAUNA_PROVIDER_NAME:
                spared.append((root, f"registered by provider {provider.name!r}, not Fauna's"))
                break
            if provider.status in _NO_PROVIDER_STATUSES:
                try:
                    unregister_sync_root(root)
                except OSError as exc:
                    spared.append((root, str(exc)))
                    break
                released.append(root)
                shell_id = shell_sync_root_id_for(root)
                if shell_id is not None:
                    try:
                        unregister_shell_sync_root(shell_id)
                    except OSError as exc:
                        shell_kept.append((root, str(exc)))
                break
            if time.monotonic() >= deadline:
                spared.append(
                    (root, f"a provider is still connected (status 0x{provider.status:X})")
                )
                break
            time.sleep(0.25)  # sleep-ok: poll cadence of a deadline poll on the provider's status
    return Released(released, spared, tuple(shell_kept))


def free_up_space(path: Path) -> None:
    """Mark ``path`` unpinned (and not pinned) — Explorer's "Free up space",
    which is an attribute write and nothing more: the provider does the byte
    work when it sees it."""
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.GetFileAttributesW.restype = ctypes.wintypes.DWORD
    k32.GetFileAttributesW.argtypes = [ctypes.wintypes.LPCWSTR]
    k32.SetFileAttributesW.argtypes = [ctypes.wintypes.LPCWSTR, ctypes.wintypes.DWORD]
    attrs = k32.GetFileAttributesW(str(path))
    if attrs == _INVALID_FILE_ATTRIBUTES:
        raise OSError(f"GetFileAttributesW({path}) failed: Win32 error {ctypes.get_last_error()}")
    new = (attrs & ~_FILE_ATTRIBUTE_PINNED) | _FILE_ATTRIBUTE_UNPINNED
    if not k32.SetFileAttributesW(str(path), new):
        raise OSError(f"SetFileAttributesW({path}) failed: Win32 error {ctypes.get_last_error()}")


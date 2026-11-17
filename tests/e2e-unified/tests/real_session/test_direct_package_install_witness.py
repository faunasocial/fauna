"""The direct-download package's install witness, on a real Windows user session.

Goal doc: `docs/goal/architecture/installers/windows.md` § Direct download (MSIX app +
nest MSI) → *2. Updates* and *5. Identity, coexistence and data*; deployment path 3 of
§ Store distribution (MSIX) → *Implementation status (Store channel)* for how a layout
registers unattended. The six measurements that section leaves to this witness, each made on the real package catalogue
of this user, each recorded with its date in § Direct download and § Testing:

1. **Registration.** The direct layout registers (Status Ok); its family is the name
   `FaunaSocial.FaunaDesktop` plus the publisher-id hash of the dev publisher; the
   registered manifest keeps the four extensions, no service, `Fauna` as its display
   name and the two capabilities.
2. **Data.** A process started in the package's context writes under
   `%LocalAppData%\\Fauna` and the file is at the REAL path from outside the package;
   a write elsewhere in `%LocalAppData%` stays the package's own (Windows 11 takes the
   scoped exclusion over the blanket switch). After `Remove-AppxPackage` the real file
   is still there and the package's own copy is gone.
3. **Update in place.** Version N registered, the app and the agent running in its
   context, version N+1 of the same family registered over it: what a plain deployment
   does to the running processes, what a deployment that may close them does, and that
   the data is kept.
4. **Sync roots after removal, both MSIX channels.** A shell sync root registered from
   inside the package's context, then the package removed: is the `Fauna!` root gone?
5. **Coexistence with a Store-shape package.** Both families registered at once; an
   agent of each started on one pipe; the second exits as a duplicate on the per-user
   mutex (the 2026-08-22 measurement, now across the two families).
6. **The archive's terminal app beside the package.** An unpackaged client reaches the
   packaged agent on the per-user pipe name it derives from its own SID.

The layout
----------
The subject is the bundle the direct packer built and graded: its per-arch loose layout
`build/installer/direct-stage/<arch>/`, the bytes of the graded package, staged as the
packer left it (`just windows-direct-package`). It is COPIED into this module's scratch
(the repo's `build/` tree, outside `%LocalAppData%`, so a packaged process and the test
see the same files) and the copy is registered (`Add-AppxPackage -Register`, Developer
Mode, no admin — deployment path 3), so the build output is never the install location.
Version N+1 is a second copy whose manifest carries the fourth version part plus one;
the Store-shape package is a third copy with the Store channel's manifest rendered by the
packer's own `render_manifest` under a test-only name. Item 4 renders each channel's
manifest into a fourth copy under a family of its own for that run (`ROOT_WITNESS_PREFIX`
says why).

What a process in the package's context is
------------------------------------------
`Invoke-CommandInDesktopPackage` starts Windows PowerShell 5.1 with the package's
identity (it reports `Package.Current`), and every process it starts inherits it — the
model test's mechanism (`test_store_package_registration.py`). Probes are PowerShell
scripts named with this module's `TOKEN`; the app and the agent are the package's own
`App\\FaunaApp.exe` and `fauna-sync-agent.exe`, the production `full` build.

Two measurements are made through a stand-in, each for a stated reason:

* **Item 4's root is registered by a probe, not by the agent's engine.** The production
  agent registers a root only when an engine starts for an on-demand binding, and an
  engine starts only for a set the provisioned capability names — an account, a nest and
  sealed set keys, none of which a pipe client can mint. The OS mechanism under question
  is keyed on the CALLER's package identity, so the probe makes, from inside the package,
  the call the agent makes (`fauna_cfapi::register_sync_root_with_shell`: the same Id
  shape `Fauna!<SID>!<account>`, provider id, policies and display name).
* **Item 6's client is this test, not `fauna-tui.exe`.** The terminal app has no headless
  verb that talks to the agent; what decides the item is whether an unpackaged process
  reaches a pipe a packaged agent created under the derived name, and this test is an
  unpackaged process speaking the agent's wire (`helpers/sync_agent_ipc.py`, pinned to
  the Rust encoder by its conformance test).

Borrow and give back
--------------------
Every package this module registers is removed at the end of the test that registered
it. The direct name is the real one, so a `FaunaSocial.FaunaDesktop` registered from
anywhere but this module's scratch is somebody's install and fails the run rather than
being displaced; one registered from the scratch is a crashed run's and is removed. Every
app and agent launch relocates `LOCALAPPDATA` into the scratch, so no launch mounts this
user's real accounts (the box's Credential Manager rows stay unread), and agents take a
`--data-dir` there too. The only file written into the real `%LocalAppData%\\Fauna` is
item 2's and item 3's marker, removed afterwards. Item 4's roots live in a folder of this
user's profile (the scratch is on Windows's Dev Drive, where the cloud-files filter does not
attach) and every one an attempt left is given back from outside the package: the filter
registration unregistered, then the `SyncRootManager` key deleted, since `Unregister` keeps
the key of a root registered from inside a package. Processes this module started are
stopped by pid, each pid checked against the scratch path first.

Tier 4: a real deployment artifact under the real OS deployment stack.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import string
import subprocess
import sys
from pathlib import Path

import pytest

from helpers import sync_agent_ipc as ipc
from helpers import windows_cfapi, windows_sync_agent
from helpers.budgets import RPC_ROUNDTRIP_S, SERVICE_BOOT_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="MSIX registration is Windows-only"),
    pytest.mark.tier_4,
]

_REPO = Path(__file__).resolve().parents[4]

APP_ID = "FaunaApp"
AGENT_EXE = "fauna-sync-agent.exe"
# A Store-shape package of the test's own: never the reservation, so it can neither
# collide with nor displace a real Store install.
STORE_SHAPE_NAME = "FaunaSocialTest.FaunaStoreDirectWitness"
# The four ratified extensions both channels declare (§ Feature subset).
EXTENSIONS = ("windows.protocol", "windows.startupTask", "windows.comServer",
              "windows.fileExplorerContextMenus")
# Where every copy, probe, data dir and relocated LOCALAPPDATA of this module lives.
SCRATCH_PARENT = _REPO / "build" / "direct-install-witness"
# What every probe, pipe, marker and root Id this module makes is named with, so one
# met on the box or in a prompt reads straight back to this witness.
TOKEN = "direct-witness"


def _packer():
    """`scripts/build-store-package.py`, imported by path: the identity values, the
    dev publisher and `render_manifest` are the build's own, never restated."""
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "build_store_package_direct_witness", _REPO / "scripts" / "build-store-package.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


PACKER = _packer()
DIRECT_NAME = PACKER.DIRECT_IDENTITY_NAME
PUBLISHER = PACKER.DEV_PUBLISHER


def _host_arch() -> str:
    native = os.environ.get("PROCESSOR_ARCHITEW6432") or os.environ.get("PROCESSOR_ARCHITECTURE", "")
    return "arm64" if native.upper() == "ARM64" else "x64"


def publisher_id(publisher: str) -> str:
    """The 13-character publisher id a package family name ends in: the first 8 bytes
    of SHA-256 over the publisher string in UTF-16LE, read as 64 bits, padded to 65
    and written 5 bits a character in the deployment stack's base32 alphabet. Computed
    here so item 1 asserts the family DERIVES from the dev publisher, not just that it
    starts with the name."""
    digest = hashlib.sha256(publisher.encode("utf-16-le")).digest()[:8]
    bits = int.from_bytes(digest, "big") << 1
    # The digits, then the lowercase letters without i, l, o and u: 32 symbols.
    alphabet = string.digits + "".join(c for c in string.ascii_lowercase if c not in "ilou")
    return "".join(alphabet[(bits >> (60 - 5 * i)) & 31] for i in range(13))


def _ps(script: str, timeout: float = 600) -> tuple[int, str, str]:
    """Run PowerShell 7 and return (rc, stdout, stderr)."""
    r = subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True, text=True, timeout=timeout,
    )
    return r.returncode, (r.stdout or "").strip(), (r.stderr or "").strip()


def _package(name: str) -> dict | None:
    """This user's registered package ``name`` as the deployment stack reports it, or
    None. Enums are stringified on the PowerShell side."""
    rc, out, err = _ps(
        f"$p = Get-AppxPackage -Name '{name}'; if ($p) {{ $p | Select-Object Name, "
        "@{n='Version';e={\"$($_.Version)\"}}, PackageFamilyName, PackageFullName, "
        "@{n='Status';e={\"$($_.Status)\"}}, InstallLocation | ConvertTo-Json -Compress }")
    assert rc == 0, f"Get-AppxPackage -Name {name} failed: {err}"
    if not out:
        return None
    facts = json.loads(out)
    return facts[0] if isinstance(facts, list) else facts


def _remove(name: str) -> None:
    _ps(f"Get-AppxPackage -Name '{name}' | Remove-AppxPackage -ErrorAction SilentlyContinue")


def _register(manifest: Path, *flags: str) -> tuple[int, str]:
    """`Add-AppxPackage -Register` of ``manifest``; (rc, the whole error text or '')."""
    rc, out, err = _ps(
        "$ErrorActionPreference='Stop'; "
        f"try {{ Add-AppxPackage -Register '{manifest}' {' '.join(flags)}; 'REGISTERED' }} "
        "catch { 'ERROR ' + ($_.Exception.Message -replace '\\s+', ' ') }")
    lines = out.splitlines()
    if lines and lines[-1] == "REGISTERED":
        return 0, ""
    error = next((l for l in lines if l.startswith("ERROR ")), None)
    return 1, (error[6:] if error else f"{out} {err}".strip())


def _hresult(error: str) -> str | None:
    """The deployment HRESULT an Add-AppxPackage error names (`0x80073D02`, …)."""
    m = re.search(r"0x[0-9A-Fa-f]{8}", error)
    return m.group(0).upper().replace("0X", "0x") if m else None


def _sid() -> str:
    rc, out, err = _ps("[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value")
    assert rc == 0 and out.startswith("S-1-"), f"could not read this user's SID: {out!r} {err}"
    return out.splitlines()[-1].strip()


def _file_text(path: Path) -> str | None:
    """The file's text once it is non-empty, else None (`Set-Content` creates before it
    writes, so existence alone races a half-written value)."""
    if path.exists() and path.stat().st_size:
        return path.read_text(encoding="utf-8", errors="replace").strip() or None
    return None


def _process_path(pid: int) -> str | None:
    rc, out, _ = _ps(f"(Get-Process -Id {pid} -ErrorAction SilentlyContinue).Path")
    return out.splitlines()[-1].strip() if rc == 0 and out else None


def _alive(pid: int) -> bool:
    rc, out, _ = _ps(f"if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ 'ALIVE' }}")
    return "ALIVE" in out


def _stop_ours(pid: int, scratch: Path) -> None:
    """Stop ``pid`` only if its image lives under this module's scratch — a pid this
    module started; a reused pid belongs to someone else and is left alone."""
    windows_sync_agent.stop_process_under(pid, scratch)


def _agent_log(data_dir: Path) -> str:
    """The agent's rolling log under ``data_dir`` — a GUI-subsystem binary, so the log
    is the only witness of which exit path it took."""
    logs = data_dir / "logs"
    if not logs.is_dir():
        return ""
    return "\n".join(p.read_text(encoding="utf-8", errors="replace")
                     for p in sorted(logs.iterdir()) if p.name.startswith("fauna.log"))


# ── Probes run inside the package's context (Windows PowerShell 5.1: the WinRT type
#    syntax is 5.1's; PowerShell 7 dropped the in-box projection) ──────────────────

_IDENTITY_PS1 = r"""
try {
    [void][Windows.ApplicationModel.Package, Windows.ApplicationModel, ContentType = WindowsRuntime]
    $fam = [Windows.ApplicationModel.Package]::Current.Id.FamilyName
} catch { $fam = 'no-package-identity' }
"""

# Starts an exe with the package's identity, its environment first set from a JSON
# file, and writes its pid (and, with -RcFile, waits and writes its exit code).
_LAUNCH_PS1 = r"""
param([string]$Exe, [string]$ArgFile, [string]$EnvFile, [string]$PidFile,
      [string]$IdFile, [string]$RcFile, [switch]$Hidden)
""" + _IDENTITY_PS1 + r"""
Set-Content -LiteralPath $IdFile -Value $fam
try {
    $vars = Get-Content -Raw -LiteralPath $EnvFile | ConvertFrom-Json
    foreach ($p in $vars.PSObject.Properties) {
        [Environment]::SetEnvironmentVariable($p.Name, [string]$p.Value, 'Process')
    }
    $a = "$(Get-Content -Raw -LiteralPath $ArgFile)".Trim()
    $start = @{ FilePath = $Exe; PassThru = $true }
    if ($a) { $start.ArgumentList = $a }
    if ($Hidden -or $RcFile) { $start.WindowStyle = 'Hidden' }
    if ($RcFile) {
        $proc = Start-Process @start -Wait
        Set-Content -LiteralPath $PidFile -Value $proc.Id
        Set-Content -LiteralPath $RcFile -Value $proc.ExitCode
    } else {
        $proc = Start-Process @start
        Set-Content -LiteralPath $PidFile -Value $proc.Id
    }
} catch {
    Set-Content -LiteralPath $PidFile -Value ('error ' + $_.Exception.Message)
}
"""

# Item 2's writes: one under %LocalAppData%\Fauna, one elsewhere in %LocalAppData%.
_DATA_PS1 = r"""
param([string]$Leaf, [string]$Elsewhere, [string]$DoneFile)
""" + _IDENTITY_PS1 + r"""
try {
    $fauna = Join-Path $env:LOCALAPPDATA 'Fauna'
    New-Item -ItemType Directory -Force -Path $fauna | Out-Null
    Set-Content -LiteralPath (Join-Path $fauna $Leaf) -Value 'direct-package witness'
    $other = Join-Path $env:LOCALAPPDATA $Elsewhere
    New-Item -ItemType Directory -Force -Path $other | Out-Null
    Set-Content -LiteralPath (Join-Path $other $Leaf) -Value 'direct-package witness'
    Set-Content -LiteralPath $DoneFile -Value ('ok ' + $fam)
} catch {
    Set-Content -LiteralPath $DoneFile -Value ('error ' + $_.Exception.Message)
}
"""

# Item 4's registration: the call `fauna_cfapi::register_sync_root_with_shell` makes,
# field for field, from inside the package's context.
_SYNC_ROOT_PS1 = r"""
param([string]$Folder, [string]$Id, [string]$DoneFile)
""" + _IDENTITY_PS1 + r"""
try {
    Add-Type -AssemblyName System.Runtime.WindowsRuntime
    $asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
        $_.Name -eq 'AsTask' -and $_.IsGenericMethod -and $_.GetParameters().Count -eq 1 -and
        $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' } | Select-Object -First 1
    [void][Windows.Storage.StorageFolder, Windows.Storage, ContentType = WindowsRuntime]
    [void][Windows.Storage.Provider.StorageProviderSyncRootManager, Windows.Storage, ContentType = WindowsRuntime]
    [void][Windows.Storage.Provider.StorageProviderSyncRootInfo, Windows.Storage, ContentType = WindowsRuntime]
    $op = [Windows.Storage.StorageFolder]::GetFolderFromPathAsync($Folder)
    $task = $asTask.MakeGenericMethod([Windows.Storage.StorageFolder]).Invoke($null, @($op))
    [void]$task.Wait(60000)
    $info = New-Object Windows.Storage.Provider.StorageProviderSyncRootInfo
    $info.Id = $Id
    $info.Path = $task.Result
    $info.DisplayNameResource = 'Fauna'
    $info.IconResource = '%SystemRoot%\system32\imageres.dll,-1043'
    $info.Version = '1.0'
    $info.ProviderId = [Guid]'FAD7A001-FA07-4E5A-B00D-FA07A0010001'
    $info.HydrationPolicy = [Windows.Storage.Provider.StorageProviderHydrationPolicy]::Full
    $info.HydrationPolicyModifier = [Windows.Storage.Provider.StorageProviderHydrationPolicyModifier]::None
    $info.PopulationPolicy = [Windows.Storage.Provider.StorageProviderPopulationPolicy]::Full
    $info.InSyncPolicy = [Enum]::ToObject([Windows.Storage.Provider.StorageProviderInSyncPolicy], 16777215)
    $info.AllowPinning = $true
    $info.ShowSiblingsAsGroup = $false
    [Windows.Storage.Provider.StorageProviderSyncRootManager]::Register($info)
    Set-Content -LiteralPath $DoneFile -Value ('ok ' + $fam)
} catch {
    Set-Content -LiteralPath $DoneFile -Value ('error ' + $_.Exception.ToString())
}
"""


class Scratch:
    """This module's scratch tree and the probes written into it."""

    def __init__(self, base: Path):
        self.base = base
        self._n = 0
        self.launcher = self._script(f"launch-{TOKEN}-in-package.ps1", _LAUNCH_PS1)
        self.data_probe = self._script(f"probe-{TOKEN}-data.ps1", _DATA_PS1)
        self.root_probe = self._script(f"probe-{TOKEN}-sync-root.ps1", _SYNC_ROOT_PS1)

    def _script(self, name: str, text: str) -> Path:
        path = self.base / name
        path.write_text(text, encoding="utf-8")
        return path

    def fresh(self, tag: str) -> Path:
        self._n += 1
        path = self.base / f"{tag}-{self._n}"
        path.mkdir(parents=True)
        return path


def _in_package(pfn: str, script: Path, args: str) -> None:
    """Run ``script`` (Windows PowerShell 5.1) with the identity of package ``pfn``."""
    inner = f'-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{script}" {args}'
    rc, _, err = _ps(
        f"Invoke-CommandInDesktopPackage -PackageFamilyName '{pfn}' "
        f"-AppId '{APP_ID}' -Command 'powershell.exe' -Args '{inner}'")
    assert rc == 0, f"Invoke-CommandInDesktopPackage ({pfn}) failed: {err}"


class Launched:
    def __init__(self, pid: int, identity: str, rc_file: Path | None):
        self.pid, self.identity, self.rc_file = pid, identity, rc_file


def launch_in_package(scratch: Scratch, pfn: str, exe: Path, args: list[str],
                      env: dict[str, str], *, wait: bool = False,
                      hidden: bool = True) -> Launched:
    """Start ``exe`` with package ``pfn``'s identity; its pid, the identity the launcher
    reported, and with ``wait`` the file its exit code lands in."""
    work = scratch.fresh("launch")
    arg_file, env_file = work / "args.txt", work / "env.json"
    pid_file, id_file, rc_file = work / "pid.txt", work / "identity.txt", work / "rc.txt"
    arg_file.write_text(" ".join(f'"{a}"' if " " in a else a for a in args) or " ",
                        encoding="utf-8")
    env_file.write_text(json.dumps(env), encoding="utf-8")
    flags = (f'-Exe "{exe}" -ArgFile "{arg_file}" -EnvFile "{env_file}" '
             f'-PidFile "{pid_file}" -IdFile "{id_file}"')
    if wait:
        flags += f' -RcFile "{rc_file}"'
    if hidden:
        flags += " -Hidden"
    _in_package(pfn, scratch.launcher, flags)
    pid_text = wait_until(lambda: _file_text(pid_file), SERVICE_BOOT_S, diagnose=lambda: (
        f"the package-context launcher never wrote {pid_file} — it did not run"))
    assert not pid_text.startswith("error"), f"starting {exe} in {pfn} failed: {pid_text}"
    return Launched(int(pid_text), _file_text(id_file) or "", rc_file if wait else None)


# ── Fixtures ─────────────────────────────────────────────────────────────────────

def _clear_leftovers() -> None:
    """Remove a crashed run's registrations; refuse to displace anyone else's."""
    facts = _package(DIRECT_NAME)
    if facts:
        location = (facts.get("InstallLocation") or "").lower()
        if str(SCRATCH_PARENT).lower() not in location:
            pytest.fail(
                f"{DIRECT_NAME} is already registered for this user from {location!r} — "
                "somebody's install, which this witness will not displace. Remove it "
                f"(Get-AppxPackage -Name {DIRECT_NAME} | Remove-AppxPackage) and re-run.",
                pytrace=False)
        _remove(DIRECT_NAME)
    _remove(STORE_SHAPE_NAME)
    _ps(f"Get-AppxPackage -Name '{ROOT_WITNESS_PREFIX}*' | Remove-AppxPackage "
        "-ErrorAction SilentlyContinue")


@pytest.fixture(scope="module")
def scratch():
    _clear_leftovers()
    shutil.rmtree(SCRATCH_PARENT, ignore_errors=True)
    base = SCRATCH_PARENT / str(os.getpid())
    base.mkdir(parents=True)
    try:
        yield Scratch(base)
    finally:
        _remove(DIRECT_NAME)
        _remove(STORE_SHAPE_NAME)
        _ps(f"Get-AppxPackage -Name '{ROOT_WITNESS_PREFIX}*' | Remove-AppxPackage "
            "-ErrorAction SilentlyContinue")
        shutil.rmtree(SCRATCH_PARENT, ignore_errors=True)


@pytest.fixture(scope="module")
def source_layout() -> Path:
    """The graded direct layout of this box's arch, as `just windows-direct-package`
    staged it."""
    src = _REPO / "build" / "installer" / "direct-stage" / _host_arch()
    manifest = src / "AppxManifest.xml"
    if not manifest.exists():
        pytest.skip(f"no staged direct layout at {src} — build it with "
                    "`just windows-direct-package`")
    text = manifest.read_text(encoding="utf-8")
    identity = re.search(r"<Identity\b[^>]*>", text, re.S).group(0)
    if f'Name="{DIRECT_NAME}"' not in identity or f'Publisher="{PUBLISHER}"' not in identity:
        pytest.skip(f"the staged layout at {src} is not a dev direct build ({identity!r})")
    return src


def _version(manifest: Path) -> str:
    identity = re.search(r"<Identity\b[^>]*>", manifest.read_text(encoding="utf-8"), re.S)
    return re.search(r'\bVersion="([^"]+)"', identity.group(0)).group(1)


def _copy_layout(src: Path, dst: Path) -> Path:
    shutil.copytree(src, dst)
    return dst


@pytest.fixture(scope="module")
def direct_layout(scratch, source_layout) -> Path:
    return _copy_layout(source_layout, scratch.base / "direct-n")


@pytest.fixture(scope="module")
def direct_update_layout(scratch, source_layout) -> Path:
    """Version N+1 of the same family: the fourth version part plus one."""
    dst = _copy_layout(source_layout, scratch.base / "direct-n1")
    manifest = dst / "AppxManifest.xml"
    parts = _version(manifest).split(".")
    parts[3] = str(int(parts[3]) + 1)
    text = manifest.read_text(encoding="utf-8")
    text = re.sub(r'(<Identity\b[^>]*?\bVersion=")[^"]+(")',
                  rf"\g<1>{'.'.join(parts)}\g<2>", text, count=1, flags=re.S)
    manifest.write_text(text, encoding="utf-8")
    return dst


@pytest.fixture(scope="module")
def store_layout(scratch, source_layout) -> Path:
    """The Store channel's shape over the same payload: the packer's Store rendering
    under a test-only name and the dev publisher."""
    dst = _copy_layout(source_layout, scratch.base / "store-shape")
    template = (Path(PACKER.STORE) / "AppxManifest.xml.in").read_text(encoding="utf-8")
    manifest = PACKER.render_manifest(template, STORE_SHAPE_NAME, PUBLISHER,
                                      _version(source_layout / "AppxManifest.xml"),
                                      _host_arch(), channel="store")
    (dst / "AppxManifest.xml").write_text(manifest, encoding="utf-8")
    return dst


class Registration:
    """A registered package and the processes started in its context."""

    def __init__(self, name: str, scratch: Scratch):
        self.name, self.scratch, self.pids = name, scratch, []

    @property
    def facts(self) -> dict:
        facts = _package(self.name)
        assert facts, f"{self.name} is not registered"
        return facts

    @property
    def pfn(self) -> str:
        return self.facts["PackageFamilyName"]

    @property
    def install_location(self) -> Path:
        return Path(self.facts["InstallLocation"])

    def launch(self, exe: Path, args: list[str], env: dict[str, str], **kw) -> Launched:
        launched = launch_in_package(self.scratch, self.pfn, exe, args, env, **kw)
        self.pids.append(launched.pid)
        return launched


@pytest.fixture
def register(scratch):
    """``register(layout, name)`` registers a layout; everything registered and every
    process launched in its context is given back when the test ends."""
    made: list[Registration] = []

    def _do(layout: Path, name: str) -> Registration:
        rc, error = _register(layout / "AppxManifest.xml")
        assert rc == 0, f"Add-AppxPackage -Register {layout} failed: {error}"
        reg = Registration(name, scratch)
        made.append(reg)
        return reg

    try:
        yield _do
    finally:
        for reg in made:
            for pid in reg.pids:
                _stop_ours(pid, scratch.base)
        for reg in made:
            _remove(reg.name)


def _relocated_env(scratch: Scratch, tag: str) -> dict[str, str]:
    """A LOCALAPPDATA of the launch's own, in the scratch: the agent's account store
    and the app's state resolve under it (`platform_ids::sync_state_base`), so no launch
    mounts this user's real accounts."""
    local = scratch.fresh(f"{tag}-localappdata")
    return {"LOCALAPPDATA": str(local)}


# ── 1. Registration ──────────────────────────────────────────────────────────────

def test_the_direct_layout_registers_with_its_own_family_and_the_ratified_shape(
        register, direct_layout):
    reg = register(direct_layout, DIRECT_NAME)
    facts = reg.facts
    assert facts["Status"] == "Ok", (
        f"registered {DIRECT_NAME} reports Status {facts['Status']!r}, want Ok")
    expected_family = f"{DIRECT_NAME}_{publisher_id(PUBLISHER)}"
    assert facts["PackageFamilyName"] == expected_family, (
        f"family {facts['PackageFamilyName']!r}, want {expected_family!r} — the name "
        f"{DIRECT_NAME} plus the publisher id of {PUBLISHER!r}")

    rc, out, err = _ps(
        f"$m = Get-AppxPackageManifest -Package '{facts['PackageFullName']}'; "
        "$m.Package.Applications.Application.Extensions.Extension | "
        "ForEach-Object { 'EXT=' + $_.Category }; "
        "$m.Package.Capabilities.ChildNodes | ForEach-Object { 'CAP=' + $_.GetAttribute('Name') }; "
        "'DISPLAY=' + $m.Package.Properties.DisplayName; "
        "$m.Package.Properties.ChildNodes | ForEach-Object { 'PROP=' + $_.Name }")
    assert rc == 0, f"could not read the registered manifest: {err}"
    lines = out.splitlines()
    extensions = [l[4:] for l in lines if l.startswith("EXT=")]
    for category in EXTENSIONS:
        assert category in extensions, f"{category} missing from {extensions}"
    assert "windows.service" not in extensions, f"a service was registered: {extensions}"
    capabilities = {l[4:] for l in lines if l.startswith("CAP=")}
    assert capabilities == {"runFullTrust", "unvirtualizedResources"}, (
        f"registered capabilities {sorted(capabilities)}, want runFullTrust and "
        "unvirtualizedResources (§ Direct download, 5)")
    assert "DISPLAY=Fauna" in lines, f"display name is not 'Fauna': {lines}"
    props = [l[5:] for l in lines if l.startswith("PROP=")]
    assert props.count("desktop6:FileSystemWriteVirtualization") == 1 and \
        props.count("virtualization:FileSystemWriteVirtualization") == 1, (
            f"the registered manifest lost a write-virtualization form: {props}")


# ── 2. Data ──────────────────────────────────────────────────────────────────────

def test_data_in_the_package_context_rests_in_the_real_local_app_data_and_outlives_removal(
        register, direct_layout, scratch):
    real_local = Path(os.environ["LOCALAPPDATA"])
    fauna = real_local / "Fauna"
    fauna_existed = fauna.is_dir()
    leaf = f"{TOKEN}-witness-{os.getpid()}.txt"
    elsewhere = f"FaunaDirectWitness-{os.getpid()}"
    real_marker = fauna / leaf
    real_elsewhere = real_local / elsewhere
    try:
        reg = register(direct_layout, DIRECT_NAME)
        pfn = reg.pfn
        done = scratch.fresh("data") / "done.txt"
        _in_package(pfn, scratch.data_probe,
                    f'-Leaf "{leaf}" -Elsewhere "{elsewhere}" -DoneFile "{done}"')
        result = wait_until(lambda: _file_text(done), SERVICE_BOOT_S,
                            diagnose=lambda: f"the data probe never wrote {done}")
        assert result == f"ok {pfn}", f"the data probe did not run as {pfn}: {result!r}"

        assert real_marker.is_file(), (
            f"a write to %LocalAppData%\\Fauna in {pfn}'s context is not at the real path "
            f"{real_marker} — the write-virtualization opt-out did not take")
        private = real_local / "Packages" / pfn / "LocalCache" / "Local" / elsewhere / leaf
        assert not (real_elsewhere / leaf).exists() and private.is_file(), (
            f"a write elsewhere in %LocalAppData% (real path {real_elsewhere / leaf}: "
            f"{(real_elsewhere / leaf).exists()}; package copy {private}: "
            f"{private.exists()}) — Windows 11 should take the scoped exclusion of "
            "%LocalAppData%\\Fauna alone and keep the rest the package's own")

        _remove(DIRECT_NAME)
        assert _package(DIRECT_NAME) is None, f"{DIRECT_NAME} is still registered"
        assert real_marker.is_file(), (
            f"{real_marker} is gone after Remove-AppxPackage — removal deleted user data "
            "the design keeps")
        assert not private.exists(), (
            f"the package's own copy {private} outlived the package's removal")
    finally:
        real_marker.unlink(missing_ok=True)
        shutil.rmtree(real_elsewhere, ignore_errors=True)
        if not fauna_existed and fauna.is_dir() and not any(fauna.iterdir()):
            fauna.rmdir()


# ── 3. Update in place ───────────────────────────────────────────────────────────

def test_an_update_over_a_running_app_and_agent(register, direct_layout,
                                                direct_update_layout, scratch):
    real_local = Path(os.environ["LOCALAPPDATA"])
    leaf = f"{TOKEN}-update-{os.getpid()}.txt"
    elsewhere = f"FaunaDirectWitnessUpdate-{os.getpid()}"
    real_marker = real_local / "Fauna" / leaf
    fauna_existed = (real_local / "Fauna").is_dir()
    try:
        reg = register(direct_layout, DIRECT_NAME)
        before = reg.facts
        pfn = before["PackageFamilyName"]
        install = Path(before["InstallLocation"])

        done = scratch.fresh("update-data") / "done.txt"
        _in_package(pfn, scratch.data_probe,
                    f'-Leaf "{leaf}" -Elsewhere "{elsewhere}" -DoneFile "{done}"')
        assert wait_until(lambda: _file_text(done), SERVICE_BOOT_S) == f"ok {pfn}"
        private = real_local / "Packages" / pfn / "LocalCache" / "Local" / elsewhere / leaf
        assert real_marker.is_file() and private.is_file()

        pipe = rf"\\.\pipe\fauna-sync-{TOKEN}-update-{os.getpid()}"
        agent_data = scratch.fresh("update-agent-data")
        agent = reg.launch(install / AGENT_EXE, ["--pipe-name", pipe, "--data-dir", str(agent_data)],
                           _relocated_env(scratch, "update-agent"))
        assert agent.identity == pfn, f"the agent ran as {agent.identity!r}, not {pfn}"
        ipc.wait_for_pipe(pipe, timeout=SERVICE_BOOT_S)
        assert ipc.pipe_server_pid(pipe, timeout=RPC_ROUNDTRIP_S) == agent.pid
        app = reg.launch(install / "App" / "FaunaApp.exe", [],
                         _relocated_env(scratch, "update-app"), hidden=False)
        assert app.identity == pfn
        assert wait_until(lambda: _alive(app.pid), SERVICE_BOOT_S), "the app did not stay up"

        # A plain deployment over the running package.
        rc, plain_error = _register(direct_update_layout / "AppxManifest.xml")
        after_plain = reg.facts
        plain = {
            "rc": rc, "hresult": _hresult(plain_error), "error": plain_error,
            "version": after_plain["Version"],
            "app_alive": _alive(app.pid), "agent_alive": _alive(agent.pid),
        }
        print(f"MEASURED plain update over the running package: {plain}")

        # A deployment allowed to close the package's processes: what App Installer does
        # once the user agrees to close the running app.
        if after_plain["Version"] == before["Version"]:
            rc, forced_error = _register(direct_update_layout / "AppxManifest.xml",
                                         "-ForceApplicationShutdown")
            assert rc == 0, f"the forced update failed: {forced_error}"
        after = reg.facts
        print(f"MEASURED final: version {after['Version']}, app alive {_alive(app.pid)}, "
              f"agent alive {_alive(agent.pid)}")

        assert after["Version"] == _version(direct_update_layout / "AppxManifest.xml"), (
            f"the update did not install: version {after['Version']}")
        assert after["PackageFamilyName"] == pfn and after["Status"] == "Ok"
        assert wait_until(lambda: not _alive(app.pid) and not _alive(agent.pid), 60,
                          diagnose=lambda: f"app alive {_alive(app.pid)}, agent alive "
                                           f"{_alive(agent.pid)} after the update"), (
            "an installed update left the old version's processes running")
        assert real_marker.is_file(), f"{real_marker} is gone after the update"
        assert private.is_file(), f"the package's own data {private} is gone after the update"
        # Measured 2026-11-16: a deployment that may not close the package's processes
        # refuses (0x80073D02, "the following apps need to be closed"), keeps the old
        # version and leaves the agent running. The app's own state after it is printed,
        # not pinned: in three runs it was still running, in one Windows had ended it
        # and filed a MoAppHang report for the package.
        assert plain["rc"] != 0 and plain["hresult"] == "0x80073D02" \
            and plain["version"] == before["Version"] and plain["agent_alive"], (
                f"a plain deployment over the running package did not refuse with "
                f"0x80073D02 and keep the old version and its agent: {plain}")
    finally:
        real_marker.unlink(missing_ok=True)
        fauna = real_local / "Fauna"
        if not fauna_existed and fauna.is_dir() and not any(fauna.iterdir()):
            fauna.rmdir()


# ── 4. Sync roots after removal ──────────────────────────────────────────────────

_SYNC_ROOT_MANAGER = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager"


def _delete_shell_entry(root_id: str) -> None:
    """Delete the `SyncRootManager` key of ``root_id`` and its subkeys. The key is
    this user's (its creator owns it); this is the give-back for an entry whose package
    is gone, which `StorageProviderSyncRootManager.Unregister` from an unpackaged
    process answers S_OK and keeps (measured 2026-11-16)."""
    import winreg

    def drop(path: str) -> None:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, path, 0,
                            winreg.KEY_READ | winreg.KEY_WRITE) as key:
            while True:
                try:
                    child = winreg.EnumKey(key, 0)
                except OSError:
                    break
                drop(rf"{path}\{child}")
        winreg.DeleteKey(winreg.HKEY_LOCAL_MACHINE, path)

    drop(rf"{_SYNC_ROOT_MANAGER}\{root_id}")


def _sync_root_folder(channel: str) -> Path:
    """Where item 4's root lives: in this user's profile, outside AppData and on the
    system volume. Not the scratch, which sits on Windows's ReFS Dev Drive, where the
    cloud-files minifilter does not attach (`Register` answers 0x800701DE); and not
    under AppData, where a package's write virtualization stands between the package
    and the path (the direct package's `Register` of a folder under
    `%LocalAppData%\\Temp` answered 0x80070490 and left a half-written entry; both
    measured 2026-11-16). A product root is a folder the user chose, which is here."""
    return Path(os.environ["USERPROFILE"]) / f"FaunaDirectWitness-{os.getpid()}" / channel


#: Item 4's families: one per channel and per run, never registered again. A family
#: whose roots have come and gone keeps OS state the key's deletion does not clear, and
#: from then on `Register` from it answers 0x80070490 (measured 2026-11-16: three
#: refusals in a row from a family that had registered and lost roots in earlier runs;
#: a fresh family was refused at most once in a run and registered on the retry).
#: Whatever the OS keeps for a fresh family afterwards is keyed by a name nothing will
#: register again. The channel is the manifest's shape, which the packer renders for it.
ROOT_WITNESS_PREFIX = "FaunaSocialTest.RootWitness"


@pytest.fixture(scope="module")
def root_layout(scratch, source_layout) -> Path:
    """One more copy of the layout, its manifest rendered per channel by item 4."""
    return _copy_layout(source_layout, scratch.base / "root-witness")


@pytest.mark.parametrize("channel", ["direct", "store"])
def test_a_sync_root_registered_in_the_package_context_after_removal(
        register, scratch, channel, root_layout, source_layout):
    name = f"{ROOT_WITNESS_PREFIX}{channel.capitalize()}{os.getpid()}"
    template = (Path(PACKER.STORE) / "AppxManifest.xml.in").read_text(encoding="utf-8")
    (root_layout / "AppxManifest.xml").write_text(
        PACKER.render_manifest(template, name, PUBLISHER,
                               _version(source_layout / "AppxManifest.xml"),
                               _host_arch(), channel=channel),
        encoding="utf-8")
    layout = root_layout
    sid = _sid()
    # Every Id and folder an attempt used, for the give-back.
    attempts: list[tuple[str, Path]] = []

    def listed(root_id: str) -> bool:
        return any(i == root_id for i, _ in windows_cfapi.list_shell_sync_roots())

    try:
        reg = register(layout, name)
        pfn = reg.pfn
        # The registration is this test's precondition, not its subject. A refused one
        # (0x80070490, "Element not found") still writes a half entry with no folder.
        # The fresh family above makes it rare, not absent (once in a run, then the
        # retry registered), so each attempt takes a fresh Id and folder, a failed one
        # is given back at once, and three in a row fail the test, quoting each.
        failures = []
        for n in range(1, 4):
            root_id = f"Fauna!{sid}!{TOKEN}-{channel}-{os.getpid()}-{n}"
            # Created from outside the package, so it is at the real path for both
            # channels; the packaged probe only resolves it.
            folder = _sync_root_folder(channel) / str(n)
            folder.mkdir(parents=True)
            attempts.append((root_id, folder))
            done = scratch.fresh(f"sync-root-{channel}-done") / "done.txt"
            _in_package(pfn, scratch.root_probe,
                        f'-Folder "{folder}" -Id "{root_id}" -DoneFile "{done}"')
            result = wait_until(lambda: _file_text(done), SERVICE_BOOT_S,
                                diagnose=lambda: f"the sync-root probe never wrote {done}")
            if result == f"ok {pfn}":
                break
            failures.append(result.splitlines()[0][:300])
            print(f"MEASURED {channel}: Register attempt {n} failed: {failures[-1]}")
            _give_back_root(root_id, folder, listed)
        else:
            pytest.fail(f"no sync root registered from inside {pfn} in three attempts: "
                        f"{failures}", pytrace=False)
        assert listed(root_id), (
            f"{root_id}, registered from inside {pfn}, is not in the SyncRootManager "
            "registry an unpackaged agent reads (`fauna_cfapi::list_shell_sync_roots`)")
        assert windows_cfapi.is_sync_root(folder), f"{folder} is not a filter sync root"

        _remove(name)
        assert _package(name) is None
        # The deployment stack's cleanup, if any, may finish after the removal returns:
        # a bounded poll on the registry's own state (convention 14).
        try:
            wait_until(lambda: not listed(root_id), 60)
            left = False
        except AssertionError:
            left = True
        filter_left = windows_cfapi.is_sync_root(folder)
        print(f"MEASURED {channel}: shell entry left after removal: {left}; "
              f"filter root left: {filter_left}")
        if left or filter_left:
            # The design's answer is "gone"; measured 2026-11-16 on both channels, both
            # halves stay. An MSIX has no uninstall action, so the cleanup is the
            # agent's to build; until it lands this
            # is the known defect, and the day it is fixed the test passes.
            pytest.xfail(
                f"the {channel} package's removal left the sync root registered "
                f"(shell entry {left}, filter root {filter_left}) — an MSIX has no "
                "uninstall action and the agent does not yet clean it (installers/"
                "windows.md § Direct download, 5, Removal)")
    finally:
        for root_id, folder in attempts:
            _give_back_root(root_id, folder, listed)
        shutil.rmtree(_sync_root_folder(channel).parent, ignore_errors=True)


def _give_back_root(root_id: str, folder: Path, listed) -> None:
    """Give back a root an attempt left, from outside the package: the agent's own
    teardown order (filter, then shell), the shell half by deleting the key once
    `Unregister` has kept it — which it does for a root registered from inside a
    package, before and after the package's removal (measured 2026-11-16)."""
    try:
        if windows_cfapi.is_sync_root(folder):
            windows_cfapi.unregister_sync_root(folder)
    except OSError as exc:
        print(f"GIVE-BACK of the filter root {folder}: {exc}")
    if listed(root_id):
        try:
            windows_cfapi.unregister_shell_sync_root(root_id)
        except OSError as exc:
            print(f"GIVE-BACK of {root_id} through Unregister: {exc}")
    if listed(root_id):
        try:
            _delete_shell_entry(root_id)
        except OSError as exc:
            print(f"GIVE-BACK of {root_id} by its key: {exc}")
    if listed(root_id):
        print(f"GIVE-BACK FAILED: {root_id} is still registered")


# ── 5. Coexistence with a Store-shape package ────────────────────────────────────

def test_one_agent_survives_across_the_two_families(register, direct_layout, store_layout,
                                                    scratch):
    direct = register(direct_layout, DIRECT_NAME)
    store = register(store_layout, STORE_SHAPE_NAME)
    assert direct.pfn != store.pfn

    leaf = f"fauna-sync-{TOKEN}-coexist-{os.getpid()}"
    pipe = rf"\\.\pipe\{leaf}"
    direct_data = scratch.fresh("coexist-direct-data")
    store_data = scratch.fresh("coexist-store-data")

    first = direct.launch(direct.install_location / AGENT_EXE,
                          ["--pipe-name", pipe, "--data-dir", str(direct_data)],
                          _relocated_env(scratch, "coexist-direct"))
    assert first.identity == direct.pfn
    ipc.wait_for_pipe(pipe, timeout=SERVICE_BOOT_S)
    assert ipc.pipe_server_pid(pipe, timeout=RPC_ROUNDTRIP_S) == first.pid

    second = store.launch(store.install_location / AGENT_EXE,
                          ["--pipe-name", pipe, "--data-dir", str(store_data)],
                          _relocated_env(scratch, "coexist-store"), wait=True)
    assert second.identity == store.pfn, (
        f"the second agent ran as {second.identity!r}, not the Store-shape {store.pfn}")
    exit_code = wait_until(lambda: _file_text(second.rc_file), SERVICE_BOOT_S, diagnose=lambda: (
        "the Store-shape agent never exited — both agents run. Its log:\n"
        + _agent_log(store_data)[-2000:]))
    log = _agent_log(store_data)
    assert exit_code == "0", f"the duplicate exited {exit_code!r}, want 0. Log:\n{log[-3000:]}"
    assert "exiting as duplicate" in log and rf"Local\FaunaSyncAgent.{leaf}" in log, (
        f"the duplicate did not lose on the per-user mutex. Log:\n{log[-3000:]}")
    assert _alive(first.pid), "the direct package's agent died when the second started"
    assert ipc.pipe_server_pid(pipe, timeout=RPC_ROUNDTRIP_S) == first.pid


# ── 6. The archive's terminal app beside the package ─────────────────────────────

def test_an_unpackaged_client_reaches_the_packaged_agent_on_the_derived_pipe(
        register, direct_layout, scratch):
    # The name every client derives for this user (`fauna_ipc::sync::pipe_name_for_sid`).
    pipe = rf"\\.\pipe\fauna-sync.{_sid()}"
    try:
        holder = ipc.pipe_server_pid(pipe, timeout=2)
    except TimeoutError:
        holder = None
    if holder is not None:
        pytest.fail(f"{pipe} is already served by pid {holder} ({_process_path(holder)}) — "
                    "this user's own agent runs on the box, which this witness will not "
                    "displace", pytrace=False)

    reg = register(direct_layout, DIRECT_NAME)
    agent = reg.launch(reg.install_location / AGENT_EXE,
                       ["--data-dir", str(scratch.fresh("derived-agent-data"))],
                       _relocated_env(scratch, "derived-agent"))
    assert agent.identity == reg.pfn
    ipc.wait_for_pipe(pipe, timeout=SERVICE_BOOT_S)
    assert ipc.pipe_server_pid(pipe, timeout=RPC_ROUNDTRIP_S) == agent.pid, (
        "the per-user pipe is served by another process than the packaged agent")
    reply = ipc.unwrap_ok(ipc.send_request(
        pipe, ipc.encode_frame(ipc.get_service_status_request(1)), timeout=RPC_ROUNDTRIP_S))
    assert "ServiceStatus" in reply, f"GetServiceStatus answered {reply!r}"
    ipc.unwrap_ok(ipc.send_request(pipe, ipc.encode_frame({"id": 2, "method": "Shutdown"}),
                                   timeout=RPC_ROUNDTRIP_S))
    assert wait_until(lambda: not _alive(agent.pid), 60), (
        "the packaged agent accepted Shutdown from the unpackaged client but kept running")

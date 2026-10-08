"""The Store MSIX registers with real Windows and its extensions take effect.

Opt-in (`--real-session`) because it mutates the box's real per-user package
catalogue — the one thing MSIX registration cannot be isolated from. Contract of
the category applies: **borrow and give back** (register, assert, always remove).

Why this is a TEST and not a manual checklist
---------------------------------------------
S4 was written as a human pass: "package registers, app
launches, startupTask visible in Task Manager, context menu renders, `fauna://`
resolves". Attacking that needs-a-human claim leaves exactly one item that truly
needs eyes — whether the submenu *looks* right. The rest are mechanisms wearing a
GUI costume, each with a witness that is not a screen:

  * "registers"                → `Add-AppxPackage` rc + `Get-AppxPackage … Status`.
  * "startupTask visible in
     Task Manager"             → Task Manager only *renders* what the deployment
                                 stack recorded; the recorded manifest is readable
                                 back out of the registered package.
  * "context menu renders from
     the packaged COM surrogate" → the mechanism is that the CLSID is activatable
                                 *under package identity*. `CoCreateInstance`
                                 answers that; prettiness is the leftover inch.
  * "`fauna://` resolves"      → the shell writes a per-user protocol class on
                                 registration; read it rather than popping a window
                                 on a shared dev box.

How this registers, and why it is NOT elevated
----------------------------------------------
Three deployment paths exist; only the third is both non-elevated and able to carry
our extension set. Both dead ends are recorded because each *looks* right and costs
a build to disprove:

  1. `Add-AppxPackage <msix>` signed → needs the cert in
     `LocalMachine\\TrustedPeople`, i.e. **admin**. Rejected: this test is meant to
     run unattended.
  2. `Add-AppxPackage <msix> -AllowUnsigned` → is not a blanket escape hatch. It
     first demands a Publisher in the *unsigned namespace* (a specific OID suffix,
     else `0x80073D2C`), and then refuses the package anyway:
     **`0x80073D2B — an unsigned package cannot include Executable activations`**.
     Our startupTask and COM-surrogate extensions are exactly such activations, so
     this path can never validate the thing we care about.
  3. **`Add-AppxPackage -Register <AppxManifest.xml>`** on the staged *loose layout*
     — the developer registration Visual Studio's F5 uses. Needs Developer Mode
     (asserted by the category conftest), no signing, no admin, and it fully
     supports Executable activations. Note its Publisher requirement is the exact
     *inverse* of path 2: it wants the **signed** namespace, i.e. a plain DN with no
     OID suffix (`0x80073D2D` otherwise).

Residual gap, deliberate: this registers the loose layout, not the packed `.msix`,
so the container itself is out of scope here — `test_store_package_pack.py` covers
that (real `makeappx` schema validation + both arch packages). Registering a
*signed* `.msix` is the remaining inch and needs admin.

Tier 4: a real deployment artifact under real OS supervision — the only tier that
catches registration bugs a tier_3 structural read of the same manifest cannot.
"""

import os
import shutil
import subprocess
import sys

import pytest

from drivers.port_util import popen_group_kwargs, reap_descendants_of
from helpers import sync_agent_ipc as ipc
from helpers.budgets import SERVICE_BOOT_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="MSIX registration is Windows-only"),
    pytest.mark.tier_4,
]

# Deliberately NOT the identity a real Store build uses: registering under a
# distinct name keeps this test's package from colliding with (or being mistaken
# for) a developer's own locally-registered build.
TEST_IDENTITY = "FaunaSocialTest.FaunaStoreRealSession"
# Plain DN, no OID suffix — the signed namespace, which is what `-Register` wants
# (see the module docstring's path 3).
TEST_PUBLISHER = "CN=Fauna Social (Real Session Test)"
STARTUP_TASK_ID = "FaunaSyncAgent"
SYNC_AGENT_EXE = "fauna-sync-agent.exe"
ROOT_CLSID = "{4A7B8C10-F1E2-4D3A-B5C6-D7E8F9A0B1C2}"
APP_ID = "FaunaApp"

# Launched *under package identity* by `Invoke-CommandInDesktopPackage`, which
# takes no `-PassThru` on Windows 11 26200 (probed 2026-08-22) — so the child's
# exit code has to come back through a file. This wrapper is also the identity
# witness: `Package.Current` only resolves inside a process that genuinely has
# package identity, which is the premise the whole test rests on. Identity is
# inherited by the child it starts (validated against a known package before this
# test was written), so the agent runs exactly as the startupTask would launch it.
#
# ⚠ Run this with **`powershell.exe` (5.1), not `pwsh` (7)**. The WinRT type
# syntax below is Windows PowerShell only — PowerShell 7 dropped in-box WinRT
# projection, so under `pwsh` the identity witness would throw and this wrapper
# would report no-package-identity on a perfectly packaged process. (`_ps` above
# still uses `pwsh` for everything else; only this launcher is version-pinned.)
_PACKAGED_SID_PS1 = r"""
param([string]$OutFile)
Set-Content -LiteralPath $OutFile `
    -Value ([System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value)
"""

_PACKAGED_LAUNCHER_PS1 = r"""
param([string]$Exe, [string]$PipeName, [string]$DataDir,
      [string]$IdFile, [string]$RcFile)
try {
    [void][Windows.ApplicationModel.Package, Windows.ApplicationModel, ContentType = WindowsRuntime]
    $fam = [Windows.ApplicationModel.Package]::Current.Id.FamilyName
} catch {
    $fam = 'no-package-identity'
}
Set-Content -LiteralPath $IdFile -Value $fam
$p = Start-Process -FilePath $Exe `
    -ArgumentList @('--pipe-name', $PipeName, '--data-dir', $DataDir) `
    -PassThru -Wait -WindowStyle Hidden
Set-Content -LiteralPath $RcFile -Value $p.ExitCode
"""


def _ps(script, timeout=300):
    """Run PowerShell and return (rc, stdout, stderr)."""
    r = subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True, text=True, timeout=timeout,
    )
    return r.returncode, (r.stdout or "").strip(), (r.stderr or "").strip()


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    ).stdout.strip()
    if out.startswith("/"):
        out = out[1].upper() + ":" + out[2:]
    return os.path.normpath(out)


def _package_family_name():
    """PFN of the registered test package — `Invoke-CommandInDesktopPackage`'s key."""
    rc, out, err = _ps(f"(Get-AppxPackage -Name '{TEST_IDENTITY}').PackageFamilyName")
    assert rc == 0 and out, f"could not read the PackageFamilyName: {out!r} {err}"
    return out.splitlines()[-1].strip()


def _package_install_location():
    """Where the deployment stack says the registered package lives.

    Read back from the OS rather than assumed from the packer's output dir: it is
    the path the startupTask would actually launch from.
    """
    rc, out, err = _ps(f"(Get-AppxPackage -Name '{TEST_IDENTITY}').InstallLocation")
    assert rc == 0 and out, f"could not read the InstallLocation: {out!r} {err}"
    return out.splitlines()[-1].strip()


def _staged_payload(repo, tmp_path):
    """Assemble the packer's `--payload-root` from the MSI's own staged layout.

    Skips (does not fail) when the payload is absent: it takes a self-contained
    WinUI publish plus two cargo release builds to produce, which is a build
    prerequisite, not a product defect.
    """
    arch = "arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "x64"
    stage = os.path.join(repo, "build", "installer", "stage", arch)
    app_exe = os.path.join(stage, "app", "FaunaApp.exe")
    if not os.path.exists(app_exe):
        pytest.skip(
            f"no staged payload at {stage} (need app/FaunaApp.exe, {SYNC_AGENT_EXE}, "
            "fauna_shell.dll). Build the MSI payload first — "
            "apps/fauna-windows/installer/README.md § Building from a clean checkout."
        )
    # The MSI stages `app/`; the package wants `App/`.
    shutil.copytree(os.path.join(stage, "app"), os.path.join(str(tmp_path), "App"))
    for name in (SYNC_AGENT_EXE, "fauna_shell.dll"):
        src = os.path.join(stage, name)
        if not os.path.exists(src):
            pytest.skip(f"staged payload is missing {name} ({src})")
        shutil.copy2(src, os.path.join(str(tmp_path), name))
    return arch


@pytest.fixture(scope="module")
def registered_package(tmp_path_factory):
    """Build a Store-shape layout from the real payload, register it, remove it.

    Give-back is unconditional and keyed on the identity NAME, so a package left
    behind by a failed assertion (or an earlier crashed run) is still cleaned up.
    """
    repo = _repo_root()
    payload = tmp_path_factory.mktemp("store-real-payload")
    arch = _staged_payload(repo, payload)

    out_dir = tmp_path_factory.mktemp("store-real-out")
    build = subprocess.run(
        [sys.executable, os.path.join(repo, "scripts", "build-store-package.py"),
         "--arch", arch,
         "--payload-root", str(payload),
         "--identity-name", TEST_IDENTITY,
         "--publisher", TEST_PUBLISHER,
         "--out-dir", str(out_dir),
         # The MSI helper's stage is test-flavoured (the e2e agent compiled in);
         # this package is registered locally, never uploaded.
         "--allow-test-surface"],
        capture_output=True, text=True, timeout=1800, cwd=repo,
    )
    assert build.returncode == 0, (
        f"build-store-package.py failed:\n{build.stdout[-3000:]}\n{build.stderr[-3000:]}"
    )

    # The packer stages the loose layout under our own --out-dir on its way to
    # `makeappx`. Registering THAT is what keeps this non-elevated (module
    # docstring, path 3).
    #
    # It used to read a fixed repo path (build/store/<arch>), justified by this
    # category's machine-wide exclusivity lock. That lock only excludes other
    # real_session runs — it says nothing about the tier_3 pack suite or a human
    # running the build script, both of which wrote the same directory. On
    # 2026-08-23 that cost a live debugging round: the pack suite's fake identity
    # landed in the developer's staged layout, so registering "the Store package"
    # silently produced the test one. Deriving it from --out-dir removes the
    # shared mutable path entirely.
    manifest = os.path.join(str(out_dir), "store-stage", arch, "AppxManifest.xml")
    assert os.path.exists(manifest), f"packer did not stage a loose layout at {manifest}"

    rc, out, err = _ps(
        f"$ErrorActionPreference='Stop'; "
        f"Add-AppxPackage -Register '{manifest}'; "
        f"(Get-AppxPackage -Name '{TEST_IDENTITY}').PackageFullName")
    if rc != 0:
        pytest.fail(f"Add-AppxPackage -Register failed (rc={rc}):\n"
                    f"stdout: {out}\nstderr: {err}")

    try:
        yield {"full_name": out.splitlines()[-1].strip() if out else "", "arch": arch}
    finally:
        _ps(f"Get-AppxPackage -Name '{TEST_IDENTITY}' | Remove-AppxPackage "
            "-ErrorAction SilentlyContinue")


@pytest.fixture(scope="module")
def registered_extensions(registered_package):
    """Extension categories read back out of the REGISTERED package's manifest.

    Reading the registered copy (not the source template) is the point: it shows
    what the deployment stack actually accepted and kept, which no amount of
    template parsing can establish.
    """
    rc, out, err = _ps(
        f"$full = (Get-AppxPackage -Name '{TEST_IDENTITY}').PackageFullName; "
        "$m = Get-AppxPackageManifest -Package $full; "
        "$m.Package.Applications.Application.Extensions.Extension | "
        "ForEach-Object { $_.Category }")
    assert rc == 0, f"could not read the registered manifest: {err}"
    return [line.strip() for line in out.splitlines() if line.strip()]


class TestStorePackageRegisters:
    def test_package_is_registered_and_ok(self, registered_package):
        """The deployment stack accepted the package shape we intend to ship."""
        rc, out, _ = _ps(f"(Get-AppxPackage -Name '{TEST_IDENTITY}').Status")
        assert rc == 0 and "Ok" in out, (
            f"registered package Status is {out!r}, want Ok — a non-Ok status means "
            "Windows registered it but considers it damaged or tampered with"
        )

    def test_package_family_name_derives_from_the_publisher(self, registered_package):
        """PFN = Name + hash(Publisher) — the standing identity gotcha, made concrete.

        This is what turns "a dev-signed package is a DIFFERENT app than the
        Store-signed one" from folklore into a checked fact: change the publisher and
        this suffix changes, which is exactly why an in-place upgrade across the
        eventual Partner-Center identity swap cannot work.
        """
        rc, out, _ = _ps(f"(Get-AppxPackage -Name '{TEST_IDENTITY}').PackageFamilyName")
        assert rc == 0 and out.startswith(f"{TEST_IDENTITY}_"), (
            f"PackageFamilyName is {out!r}, expected '{TEST_IDENTITY}_<publisherhash>'"
        )

    def test_registered_manifest_keeps_the_ratified_extension_set(self, registered_extensions):
        """All four ratified extensions survived registration — and no service did.

        The absence check is the design assertion: MSIX services can only run as
        localSystem/localService/networkService, so shipping Nest or Bridge here
        would abandon the ratified `NT SERVICE\\FaunaNest` virtual-account shape.
        """
        for category in ("windows.protocol", "windows.startupTask",
                         "windows.comServer", "windows.fileExplorerContextMenus"):
            assert category in registered_extensions, (
                f"{category} missing from the registered extensions "
                f"({registered_extensions}) — the Store install would lack it"
            )
        assert "windows.service" not in registered_extensions, (
            f"a service extension reached a registered package ({registered_extensions})"
        )

    def test_startup_task_registration_names_the_sync_agent(self, registered_package):
        """Task Manager's Startup tab renders THIS record; assert the record.

        Scope note: this asserts the *registration* — the extension the deployment
        stack kept, with the right executable and task id. The OS only materialises
        StartupTask **state** (and `StartupTask.GetAsync` only answers) after the app
        has run once under package identity, which this test does not do; launching
        a 277 MB WinUI app on a shared dev box would buy a repaint, not a mechanism.
        """
        rc, out, err = _ps(
            f"$full = (Get-AppxPackage -Name '{TEST_IDENTITY}').PackageFullName; "
            "$m = Get-AppxPackageManifest -Package $full; "
            "$e = $m.Package.Applications.Application.Extensions.Extension | "
            "Where-Object { $_.Category -eq 'windows.startupTask' }; "
            "'EXE=' + $e.Executable; "
            "'TASKID=' + $e.StartupTask.TaskId; "
            "'ENABLED=' + $e.StartupTask.Enabled")
        assert rc == 0, f"could not read the startupTask extension: {err}"
        assert f"EXE={SYNC_AGENT_EXE}" in out, (
            f"registered startupTask does not launch {SYNC_AGENT_EXE} ({out!r})"
        )
        assert f"TASKID={STARTUP_TASK_ID}" in out, (
            f"registered startupTask has the wrong TaskId ({out!r})"
        )
        # Case-insensitive on BOTH sides: the manifest spells it `true`, but XML
        # attribute round-tripping through PowerShell has no case guarantee.
        assert "enabled=true" in out.lower(), (
            f"registered startupTask is not enabled ({out!r}) — sync would not start "
            "at logon, breaking works-out-of-the-box"
        )

    def test_protocol_class_is_registered_for_this_user(self, registered_package):
        """The shell wrote a real `fauna` class on registration.

        This is the registry fact behind "fauna:// resolves", asserted instead of
        launching the URL — launching would pop a window on a shared dev box and
        prove the same association plus a repaint.
        """
        rc, out, _ = _ps(
            r"if (Get-Item 'HKCU:\Software\Classes\fauna' -ErrorAction SilentlyContinue) "
            r"{ 'PRESENT' } else { 'ABSENT' }")
        assert rc == 0 and "PRESENT" in out, (
            "no per-user `fauna` protocol class after registering the package — "
            "fauna:// links would not reach the Store-installed app"
        )

    def test_shell_extension_class_activates_under_package_identity(self, registered_package):
        """The context menu's real mechanism: does the CLSID actually activate?

        Explorer renders the Fauna submenu by CoCreateInstance-ing this class in the
        packaged COM surrogate (dllhost.exe), which is what carries package identity
        into the handler. If activation fails, no amount of looking at a menu helps;
        if it succeeds, the only open question is cosmetic. This is the split-the-mile
        line — mechanism asserted here, pixels left to a human.
        """
        rc, out, err = _ps(
            f"$t = [Type]::GetTypeFromCLSID([Guid]'{ROOT_CLSID}'); "
            "if (-not $t) { 'NOTYPE'; exit 0 } "
            "try { $o = [Activator]::CreateInstance($t); "
            "if ($o) { 'ACTIVATED'; "
            "[void][Runtime.InteropServices.Marshal]::ReleaseComObject($o) } "
            "else { 'NULL' } } "
            "catch { 'ERR:' + $_.Exception.Message }")
        assert rc == 0, f"activation probe crashed: {err}"
        assert "ACTIVATED" in out, (
            f"the shell-extension CLSID {ROOT_CLSID} did not activate ({out!r}) — "
            "Explorer would render no Fauna context menu from this package"
        )


def _agent_log(data_dir):
    """Concatenate the agent's rolling log under `data_dir`, '' if none yet.

    The shipped agent is a **GUI-subsystem** binary (no console — stderr is
    discarded by the OS), so `<data_dir>/logs/fauna.log.<date>` is not a
    convenience here, it is the only witness of which exit path it took.
    """
    log_dir = os.path.join(data_dir, "logs")
    if not os.path.isdir(log_dir):
        return ""
    text = []
    for name in sorted(os.listdir(log_dir)):
        if name.startswith("fauna.log"):
            with open(os.path.join(log_dir, name), encoding="utf-8", errors="replace") as fh:
                text.append(fh.read())
    return "\n".join(text)


@pytest.fixture
def unvirtualized_dir(request):
    """A scratch dir a PACKAGED process can write where the test can read it.

    pytest's `tmp_path` lives under `%LocalAppData%\\Temp`, and that is precisely
    the tree a packaged app's writes get **virtualized** out of: this package
    declares `unvirtualizedResources` scoped by flexible virtualization to
    `%LocalAppData%\\Fauna` *only* (§ Data continuity), so every other AppData path
    keeps the default redirect into `%LocalAppData%\\Packages\\<PFN>\\LocalCache\\…`.
    A packaged agent's log — and the launcher's exit-code file — would land there
    and the test would read an empty dir, failing as a 180s timeout that looks
    like a hang rather than the redirect it is.

    The repo tree is not under `%LocalAppData%`, so a scratch dir inside it is
    seen identically by both processes. Named for the test and removed after.
    """
    base = os.path.join(_repo_root(), "build", "store-second-instance",
                        f"{request.node.name}-{os.getpid()}")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    try:
        yield base
    finally:
        shutil.rmtree(base, ignore_errors=True)


def _file_text_once_written(path):
    """Predicate for `wait_until`: the file's text once it is non-empty, else None.

    Non-empty, not merely present: `Set-Content` creates the file before it has
    written to it, so existence alone would race a half-written value.
    """
    path = str(path)
    if os.path.exists(path) and os.path.getsize(path):
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read().strip() or None
    return None


class TestStoreAgentSecondInstance:
    """S5 — the MSI Run-key agent and the Store startupTask agent at one logon.

    The two channels coexist by design (`installers/windows.md` § Identity &
    coexistence): a box can carry the direct-download MSI *and* the Store package,
    and then **both** logon hooks fire — the MSI's `HKCU\\…\\Run` value and the
    package's `windows.startupTask` — each launching its own copy of
    `fauna-sync-agent.exe`. They would sync the same per-user DB as the same
    device, so exactly one must survive.

    Why this is a TEST and not a manual "watch Task Manager at logon"
    ----------------------------------------------------------------
    S5 was written as an observation. But nothing here needs an eye: the
    survivor-selection is a kernel-arbitrated named mutex
    (`pipe_server::InstanceLock`), and it has three witnesses that are not a
    screen — the loser's **exit code**, the loser's **log line**, and the
    winner **still answering its pipe**. A logon is only a *way of starting two
    agents*; starting two agents directly is the same mechanism without waiting
    for a reboot, and it keeps the run non-elevated.

    What makes it faithful rather than a re-run of the tier_1 unit test
    ------------------------------------------------------------------
    `pipe_server.rs`'s own test already proves two acquires in ONE process
    contend. The S5 question is the one a unit test structurally cannot reach:
    the second agent runs **under MSIX package identity**, and package identity
    is exactly the thing that could put it in a different `Local\\` object
    namespace — in which case the mutex would silently NOT collide, both agents
    would run, and the only remaining stop would be the machine-global
    `FILE_FLAG_FIRST_PIPE_INSTANCE` fallback: a later, louder failure the design
    deliberately does not rely on. So the packaged launch is the point, and
    `Invoke-CommandInDesktopPackage` (not a plain spawn) is how the test gets it.

    Isolation: both agents take a test-unique `--pipe-name` and separate throwaway
    `--data-dir`s, so the run never touches — or is confused by — the developer's
    real agent on this shared box (`helpers/windows_sync_agent.py` documents that
    trap). `test_package_identity_keeps_the_user_sid` below is what makes the
    explicit `--pipe-name` faithful to production, where both derive it instead.
    """

    def test_package_identity_keeps_the_user_sid(
            self, registered_package, unvirtualized_dir):
        """Both installs name the SAME mutex — the premise the guard rests on.

        `current_user_pipe_name()` (`libs/fauna-ipc/src/sync.rs`) derives the pipe
        from the process token's user SID, and `mutex_name_for_pipe` derives
        `Local\\FaunaSyncAgent.<SID>` from that. So the guard fires across the two
        channels **iff** a packaged process and an unpackaged one report the same
        user SID. That is not obvious — package identity attaches its own SIDs to
        the token — and if it were false the two agents would name different
        mutexes and both would run.
        """
        # Both halves read the SID the same PATH-free way, via .NET rather than
        # `whoami.exe`. The first cut of this test shelled out to `whoami /user`
        # and the UNPACKAGED half came back empty on 2026-08-22: `whoami` is not
        # resolvable on the PATH pytest runs with, and pwsh exits **0** when a
        # native command is missing, so an rc check does not catch it. Reading the
        # process token directly has no PATH dependency, and using the identical
        # expression on both sides means a difference can only come from identity.
        sid_script = os.path.join(unvirtualized_dir, "read-sid.ps1")
        with open(sid_script, "w", encoding="utf-8") as fh:
            fh.write(_PACKAGED_SID_PS1)

        rc, unpackaged, err = _ps(
            "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value")
        assert rc == 0 and unpackaged.startswith("S-1-"), (
            f"could not read this process's own user SID (rc={rc}, "
            f"out={unpackaged!r}, err={err})"
        )

        out_file = os.path.join(unvirtualized_dir, "packaged-sid.txt")
        pfn = _package_family_name()
        rc, _, err = _ps(
            f"Invoke-CommandInDesktopPackage -PackageFamilyName '{pfn}' "
            f"-AppId '{APP_ID}' -Command 'powershell.exe' "
            f"-Args '-NoProfile -NonInteractive -ExecutionPolicy Bypass "
            f"-File \"{sid_script}\" -OutFile \"{out_file}\"'")
        assert rc == 0, f"Invoke-CommandInDesktopPackage failed: {err}"

        packaged = wait_until(
            lambda: _file_text_once_written(out_file),
            SERVICE_BOOT_S,
            diagnose=lambda: (
                f"the packaged SID probe never wrote {out_file} — "
                "Invoke-CommandInDesktopPackage reported success but the process "
                "produced nothing"
            ),
        )

        assert packaged.startswith("S-1-"), (
            f"the packaged probe did not return a SID ({packaged!r})"
        )
        assert packaged == unpackaged, (
            f"package identity changed the user SID (packaged {packaged} vs "
            f"unpackaged {unpackaged}) — the two channels would derive DIFFERENT "
            "pipe names, so the single-instance mutex could never collide and both "
            "agents would run"
        )

    def test_a_packaged_second_agent_exits_cleanly_as_a_duplicate(
            self, registered_package, unvirtualized_dir):
        """The S5 mechanism end to end, across the package-identity boundary.

        Unpackaged agent (the MSI's role) takes the mutex and serves the pipe;
        the packaged agent (the startupTask's role) then starts on the SAME pipe
        name and must lose. "Cleanly" is asserted three ways, because a crash, a
        hang, and a silent double-run all *look* the same from a screenshot:
        exit code 0, the duplicate-mutex log line, and the winner still serving.
        """
        install_root = _package_install_location()
        packaged_exe = os.path.join(install_root, SYNC_AGENT_EXE)
        assert os.path.exists(packaged_exe), (
            f"the registered package has no {SYNC_AGENT_EXE} at {packaged_exe} — "
            "the startupTask could not launch"
        )
        # The unpackaged half is the MSI's own staged binary, the same artifact the
        # direct-download channel installs.
        msi_exe = os.path.join(
            _repo_root(), "build", "installer", "stage",
            registered_package["arch"], SYNC_AGENT_EXE)
        assert os.path.exists(msi_exe), f"no staged MSI-role agent at {msi_exe}"

        # Test-unique so this never contends with the developer's real agent. A
        # dotless leaf on purpose: it is the shape every non-production
        # `--pipe-name` takes, and the one `mutex_name_for_pipe` was fixed for.
        leaf = f"fauna-sync-store-s5-{os.getpid()}"
        pipe_name = rf"\\.\pipe\{leaf}"
        expected_mutex = rf"Local\FaunaSyncAgent.{leaf}"

        # Both outside `%LocalAppData%` — see `unvirtualized_dir`. The MSI-role
        # agent is unpackaged and would be fine anywhere, but keeping the pair
        # symmetrical is what makes the two logs comparable evidence.
        msi_data = os.path.join(unvirtualized_dir, "msi-agent-data")
        store_data = os.path.join(unvirtualized_dir, "store-agent-data")
        os.makedirs(msi_data)
        os.makedirs(store_data)

        # Point 9's die-with-the-run pair, both halves: `popen_group_kwargs` is the
        # POSIX half (a no-op here, but the contract is armed at every spawn site,
        # not only where this module can run) and `reap_descendants_of` is the
        # Windows half — the agent is exactly the kind of child that outlives its
        # spawner by design, so an un-reaped one would hold its build artifact and
        # answer a pipe long after this run.
        first = subprocess.Popen(
            [msi_exe, "--pipe-name", pipe_name, "--data-dir", str(msi_data)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            **popen_group_kwargs(),
        )
        reap_descendants_of(first.pid)
        try:
            # Causal barrier, not a settle-sleep: the pipe answering proves the
            # first agent already passed `InstanceLock::acquire` (the gate runs
            # FIRST in `run_agent`, before the pipe is ever created), so the
            # second's contention below is guaranteed to be a real race-loser.
            try:
                ipc.wait_for_pipe(pipe_name, timeout=SERVICE_BOOT_S)
            except Exception as exc:
                raise AssertionError(
                    f"the MSI-role agent never served {pipe_name} within "
                    f"{SERVICE_BOOT_S:.0f}s ({exc}); its log said:\n"
                    f"{_agent_log(str(msi_data))[-3000:]}"
                ) from exc

            launcher = os.path.join(unvirtualized_dir, "launch-packaged-agent.ps1")
            with open(launcher, "w", encoding="utf-8") as fh:
                fh.write(_PACKAGED_LAUNCHER_PS1)
            id_file = os.path.join(unvirtualized_dir, "packaged-identity.txt")
            rc_file = os.path.join(unvirtualized_dir, "packaged-rc.txt")

            pfn = _package_family_name()
            inner = (
                f'-NoProfile -NonInteractive -ExecutionPolicy Bypass '
                f'-File \"{launcher}\" '
                f'-Exe \"{packaged_exe}\" -PipeName \"{pipe_name}\" '
                f'-DataDir \"{store_data}\" -IdFile \"{id_file}\" -RcFile \"{rc_file}\"'
            )
            rc, _, err = _ps(
                f"Invoke-CommandInDesktopPackage -PackageFamilyName '{pfn}' "
                f"-AppId '{APP_ID}' -Command 'powershell.exe' -Args '{inner}'")
            assert rc == 0, f"could not launch the agent under package identity: {err}"

            # 1. It really ran with package identity — otherwise this test proves
            #    nothing about the MSIX namespace and is just two plain processes.
            identity = wait_until(
                lambda: _file_text_once_written(id_file),
                SERVICE_BOOT_S,
                diagnose=lambda: (
                    "the packaged launcher never reported an identity — it did not "
                    f"run at all. Store-agent log so far:\n"
                    f"{_agent_log(str(store_data))[-2000:]}"
                ),
            )
            assert identity.startswith(f"{TEST_IDENTITY}_"), (
                f"the second agent ran WITHOUT package identity (got {identity!r}, "
                f"wanted {TEST_IDENTITY}_<publisherhash>) — nothing here would then "
                "say anything about the Store channel's namespace"
            )

            # 2. It exited, and cleanly: `run_agent` returns Ok(()) on the
            #    duplicate path, so a clean loss is rc 0. A non-zero rc means it
            #    died some other way (the `FILE_FLAG_FIRST_PIPE_INSTANCE`
            #    fallback, or worse); no exit at all means both agents are running.
            exit_code = wait_until(
                lambda: _file_text_once_written(rc_file),
                SERVICE_BOOT_S,
                diagnose=lambda: (
                    "the packaged duplicate never exited — BOTH agents are running, "
                    "which is the S5 failure this test exists to catch. Its log:\n"
                    f"{_agent_log(str(store_data))[-2000:]}"
                ),
            )
            store_log = _agent_log(str(store_data))
            assert exit_code == "0", (
                f"the packaged duplicate exited {exit_code!r}, want 0 — it did not "
                f"lose the mutex race cleanly. Its log:\n{store_log[-3000:]}"
            )

            # 3. It lost on the MUTEX, not on the later pipe-creation fallback.
            #    This is the assertion the whole packaged launch exists for.
            assert "exiting as duplicate" in store_log, (
                "the packaged duplicate exited 0 but never logged the "
                f"single-instance path — so MSIX package identity likely put it in "
                f"a different `Local\\` namespace and the mutex did not collide. "
                f"Its log:\n{store_log[-3000:]}"
            )
            assert expected_mutex in store_log, (
                f"the packaged duplicate contended on a different mutex than "
                f"{expected_mutex!r} — its log:\n{store_log[-3000:]}"
            )

            # 4. The winner is untouched: the loser exited "having touched
            #    nothing", which means the surviving agent still serves.
            assert first.poll() is None, (
                f"the FIRST agent died (rc={first.returncode}) while the packaged "
                f"duplicate started — the wrong instance lost. Its log:\n"
                f"{_agent_log(str(msi_data))[-3000:]}"
            )
            ipc.wait_for_pipe(pipe_name, timeout=SERVICE_BOOT_S)
        finally:
            first.terminate()
            try:
                first.wait(timeout=30)
            except subprocess.TimeoutExpired:
                first.kill()

"""Structural validation of the Windows MSI installer — no admin required.

Queries the MSI database directly via the Windows Installer API (msi.dll)
to verify the feature tree, component mappings, and file table without
actually installing anything.
"""

import ctypes
from ctypes import wintypes
import os
import subprocess
import sys

import pytest

pytestmark = [pytest.mark.skipif(sys.platform != "win32", reason="Windows-only"), pytest.mark.tier_3]


# ── MSI database helpers ──

def _get_repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    path = result.stdout.strip()
    if sys.platform == "win32" and path.startswith("/"):
        path = path[1].upper() + ":" + path[2:]
    return os.path.normpath(path)


def _pe_subsystem(path):
    """Return the PE Optional Header *Subsystem* field of a Windows binary.

    2 = WINDOWS_GUI (no console), 3 = WINDOWS_CUI (console). Layout: e_lfanew is a
    DWORD at 0x3C pointing at the "PE\\0\\0" signature; Subsystem sits at offset 68
    inside the Optional Header, i.e. e_lfanew + 4 (sig) + 20 (COFF header) + 68 =
    e_lfanew + 92. That offset is identical for PE32 and PE32+ (the fields up to
    Subsystem are laid out the same), so no magic-number branch is needed.
    """
    with open(path, "rb") as f:
        data = f.read()
    e_lfanew = int.from_bytes(data[0x3C:0x40], "little")
    off = e_lfanew + 92
    return int.from_bytes(data[off:off + 2], "little")


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


# ── Fixture ──

@pytest.fixture(scope="session")
def msi_db():
    """Open the built MSI for structural queries."""
    repo = _get_repo_root()
    msi_path = os.path.join(repo, "build", "installer", "Fauna-Setup-arm64.msi")
    if not os.path.exists(msi_path):
        pytest.skip(f"MSI not found at {msi_path}. Build it first.")
    with MsiDatabase(msi_path) as db:
        yield db


def _product_version(msi_db):
    """The MSI ProductVersion (Property table) — the source the version-stamped shell
    DLL filename must track (Path 1)."""
    rows = msi_db.query("SELECT Value FROM Property WHERE Property = 'ProductVersion'")
    assert rows, "ProductVersion missing from Property table"
    return rows[0][0]


# ── Feature tree tests ──

class TestFeatureTree:
    """Verify the MSI feature tree structure."""

    def test_top_level_features(self, msi_db):
        """Four top-level features: DesktopApp, Sync, TerminalApp, Nest.

        TerminalApp is top-level on purpose (TerminalApp.wxs): the terminal app
        must survive a user unticking either Sync or Desktop App."""
        rows = msi_db.query(
            "SELECT Feature, Feature_Parent FROM Feature"
        )
        features = {r[0]: r[1] for r in rows}

        top_level = [f for f, parent in features.items() if parent == ""]
        assert set(top_level) == {"DesktopApp", "Sync", "TerminalApp", "Nest"}

    def test_feature_order(self, msi_db):
        """DesktopApp appears first in the feature table."""
        rows = msi_db.query("SELECT Feature FROM Feature")
        feature_ids = [r[0] for r in rows]
        assert feature_ids[0] == "DesktopApp", \
            f"Expected DesktopApp first, got: {feature_ids}"

    def test_shell_ext_is_child_of_sync(self, msi_db):
        rows = msi_db.query(
            "SELECT Feature, Feature_Parent FROM Feature WHERE Feature = 'ShellExt'"
        )
        assert len(rows) == 1
        assert rows[0][1] == "Sync"

    def test_bridge_is_child_of_nest(self, msi_db):
        rows = msi_db.query(
            "SELECT Feature, Feature_Parent FROM Feature WHERE Feature = 'Bridge'"
        )
        assert len(rows) == 1
        assert rows[0][1] == "Nest"

    def test_all_features_are_optional(self, msi_db):
        """No feature should have Level=0 (hidden required)."""
        rows = msi_db.query("SELECT Feature, Level FROM Feature")
        for fid, level in rows:
            assert level != "0", f"Feature {fid} has Level=0 (hidden required)"

    def test_bridge_default_off(self, msi_db):
        """Bridge should have Level >= 1000 (not selected by default)."""
        rows = msi_db.query(
            "SELECT Level FROM Feature WHERE Feature = 'Bridge'"
        )
        assert int(rows[0][0]) >= 1000

    def test_nest_default_off(self, msi_db):
        """Nest should have Level >= 1000 (not selected by default) — the Windows
        desktop is primarily a client; self-hosting a nest is opt-in
        (USER-ratified 2026-06-21)."""
        rows = msi_db.query("SELECT Level FROM Feature WHERE Feature = 'Nest'")
        assert int(rows[0][0]) >= 1000

    def test_shellext_default_on(self, msi_db):
        """ShellExt (Explorer integration) should be Level=1 (selected by default,
        USER-decision 2026-06-23). The shell-ext DLL loads into explorer.exe via COM; under
        Path 1 (2026-06-25) the DLL is installed side-by-side under a version-stamped
        filename and never removed/replaced in place, so an upgrade never closes Explorer
        and can never strand the desktop (TestShellExtUpgradeSafety) — the hazard that
        briefly forced default-OFF (2026-06-22) is resolved and the overlay ships by
        default. A change applies on the next sign-out / Explorer restart (no reboot)."""
        rows = msi_db.query("SELECT Level FROM Feature WHERE Feature = 'ShellExt'")
        assert int(rows[0][0]) == 1

    def test_default_on_features(self, msi_db):
        """DesktopApp, Sync, ShellExt, TerminalApp should be Level=1 (default on).
        Nest + Bridge are default OFF — see test_nest_default_off /
        test_bridge_default_off."""
        rows = msi_db.query("SELECT Feature, Level FROM Feature")
        by_id = {r[0]: r[1] for r in rows}
        for fid in ["DesktopApp", "Sync", "ShellExt", "TerminalApp"]:
            assert by_id[fid] == "1", f"Feature {fid} has Level={by_id[fid]}, expected 1"

    def test_every_feature_deselectable(self, msi_db):
        """Sync became optional (AllowAbsent="yes"), so EVERY feature is now
        deselectable — the UIDisallowAbsent bit (0x10) must be clear on all of
        them. This is what enables a headless nest-only install (Nest [+Bridge],
        no Sync, no Desktop App)."""
        rows = msi_db.query("SELECT Feature, Attributes FROM Feature")
        attrs = {f: (int(a) if a else 0) for f, a in rows}
        UI_DISALLOW_ABSENT = 0x10
        for fid in ["DesktopApp", "Sync", "ShellExt", "TerminalApp", "Nest", "Bridge"]:
            assert attrs[fid] & UI_DISALLOW_ABSENT == 0, \
                f"Feature {fid} is UIDisallowAbsent (Attributes={attrs[fid]})"


# ── Component mapping tests ──

class TestComponentMappings:
    """Verify components are assigned to the correct features."""

    def _feature_components(self, msi_db):
        rows = msi_db.query(
            "SELECT Feature_, Component_ FROM FeatureComponents"
        )
        mapping = {}
        for feature, component in rows:
            mapping.setdefault(feature, set()).add(component)
        return mapping

    def test_sync_has_sync_exe(self, msi_db):
        fc = self._feature_components(msi_db)
        assert "FaunaSyncAgentExe" in fc["Sync"]

    def test_nest_has_nest_exe(self, msi_db):
        fc = self._feature_components(msi_db)
        assert "FaunaNestSvcExe" in fc["Nest"]

    def test_no_fauna_ctl_component(self, msi_db):
        """fauna-ctl is NOT shipped (USER-ratified 2026-06-20; the crate was deleted 2026-10-02 —
        a headless box is configured entirely from a client at test@<ip>). No
        feature may reference it."""
        rows = msi_db.query("SELECT Component_ FROM FeatureComponents")
        comps = {c for (c,) in rows}
        assert "FaunaCtlExe" not in comps, "fauna-ctl leaked back into the MSI"

    def test_data_cleanup_under_sync_and_nest(self, msi_db):
        """DataDirRegistry rides BOTH Sync and Nest so the DataDir reg value is
        written whenever either is installed — Sync is now optional, so the Nest
        ref covers a headless nest-only install."""
        fc = self._feature_components(msi_db)
        assert "DataDirRegistry" in fc["Sync"]
        assert "DataDirRegistry" in fc["Nest"]

    def test_bridge_has_bridge_exe(self, msi_db):
        fc = self._feature_components(msi_db)
        assert "FaunaBridgeSvcExe" in fc["Bridge"]

    def test_bridge_has_mail_bridge_exe(self, msi_db):
        """The Bridge feature ships the Go MDA the service supervises as a child."""
        fc = self._feature_components(msi_db)
        assert "FaunaMailBridgeExe" in fc["Bridge"]

    def test_bridge_has_mail_bridge_ffi_dll(self, msi_db):
        """The MDA's gnullvm fauna_ffi.dll ships in the Bridge feature."""
        fc = self._feature_components(msi_db)
        assert "FaunaMailBridgeFfiDll" in fc["Bridge"]

    def test_bridge_has_mail_bridge_libunwind_dll(self, msi_db):
        """The gnullvm dll's libunwind.dll runtime dep ships in the Bridge feature."""
        fc = self._feature_components(msi_db)
        assert "FaunaMailBridgeLibunwindDll" in fc["Bridge"]

    def test_shell_ext_has_dll(self, msi_db):
        fc = self._feature_components(msi_db)
        assert "FaunaShellDll" in fc["ShellExt"]

    def test_shell_ext_has_overlay_icons(self, msi_db):
        fc = self._feature_components(msi_db)
        assert "OverlayIcons" in fc["ShellExt"]

    def test_desktop_app_has_protocol(self, msi_db):
        fc = self._feature_components(msi_db)
        assert "FaunaProtocol" in fc["DesktopApp"]

    def test_desktop_app_has_shortcut(self, msi_db):
        fc = self._feature_components(msi_db)
        assert "FaunaShortcut" in fc["DesktopApp"]

    def test_desktop_app_does_not_have_services(self, msi_db):
        """DesktopApp must not contain any non-desktop component. FaunaSyncAgentExe is
        now the per-user sync *agent* component (ships the exe + the HKLM \\Run
        logon launch, not a service), but it still belongs to Sync, never DesktopApp."""
        fc = self._feature_components(msi_db)
        non_desktop_components = {"FaunaSyncAgentExe", "FaunaNestSvcExe", "FaunaBridgeSvcExe"}
        assert fc["DesktopApp"].isdisjoint(non_desktop_components), \
            f"non-desktop component leaked into DesktopApp: " \
            f"{fc['DesktopApp'] & non_desktop_components}"


# ── File table tests ──

class TestFileTable:
    """Verify expected files are in the MSI."""

    def _files(self, msi_db):
        rows = msi_db.query("SELECT FileName, Component_ FROM File")
        result = {}
        for raw_name, component in rows:
            name = raw_name.split("|")[-1] if "|" in raw_name else raw_name
            result[name] = component
        return result

    def test_sync_exe_present(self, msi_db):
        assert "fauna-sync-agent.exe" in self._files(msi_db)

    def test_tui_exe_present(self, msi_db):
        """The terminal app rides the MSI as its signed Windows form
        (installers/tui.md § The ratified channel) — in INSTALLFOLDER, beside
        the agent it resolves as a sibling."""
        files = self._files(msi_db)
        assert "fauna-tui.exe" in files
        assert files["fauna-tui.exe"] == "FaunaTuiExe"
        assert files["fauna-tui.exe"] != files["fauna-sync-agent.exe"], (
            "fauna-tui.exe must be its own component (its own feature, TerminalApp), "
            "never bundled into the agent's"
        )

    def test_nest_exe_present(self, msi_db):
        assert "fauna-nest-svc.exe" in self._files(msi_db)

    def test_bridge_exe_present(self, msi_db):
        assert "fauna-bridge-svc.exe" in self._files(msi_db)

    def test_mail_bridge_exe_present(self, msi_db):
        assert "fauna-mail-bridge.exe" in self._files(msi_db)

    def test_mail_bridge_libunwind_dll_present(self, msi_db):
        assert "libunwind.dll" in self._files(msi_db)

    def test_mail_bridge_ffi_dll_present(self, msi_db):
        """The bridge's gnullvm fauna_ffi.dll ships under its own component. The
        WinUI app ships a separate MSVC fauna_ffi.dll under App\\, so the FileName
        alone is ambiguous (the name-keyed _files dict collapses the two) — assert
        via the component, which also pins that the name stays fauna_ffi.dll (cgo
        loads it by that exact name)."""
        rows = msi_db.query(
            "SELECT FileName FROM File WHERE Component_ = 'FaunaMailBridgeFfiDll'"
        )
        names = [r[0].split("|")[-1] if "|" in r[0] else r[0] for r in rows]
        assert names == ["fauna_ffi.dll"], f"got {names}"

    def test_ctl_exe_absent(self, msi_db):
        """fauna-ctl.exe is no longer shipped (the crate was deleted 2026-10-02)."""
        assert "fauna-ctl.exe" not in self._files(msi_db)

    def test_shell_dll_present(self, msi_db):
        """Path 1: the shell DLL ships under a VERSION-STAMPED filename
        (fauna_shell_<ProductVersion>.dll), not the bare fauna_shell.dll — a genuine
        change ships under a new filename so the install never touches the in-use old DLL
        (TestShellExtUpgradeSafety)."""
        version = _product_version(msi_db)
        assert f"fauna_shell_{version}.dll" in self._files(msi_db)

    def test_app_exe_present(self, msi_db):
        assert "FaunaApp.exe" in self._files(msi_db)

    def test_icon_files_present(self, msi_db):
        files = self._files(msi_db)
        for ico in ["synced.ico", "syncing.ico", "cloud.ico", "error.ico"]:
            assert ico in files, f"{ico} missing from MSI"

    def test_app_exe_in_desktop_component(self, msi_db):
        """FaunaApp.exe should belong to a DesktopApp component."""
        rows = msi_db.query(
            "SELECT Feature_, Component_ FROM FeatureComponents"
        )
        desktop_components = {c for f, c in rows if f == "DesktopApp"}
        files = self._files(msi_db)
        assert files["FaunaApp.exe"] in desktop_components


# ── Upgrade table tests ──

# Stable across releases — Package.wxs Package/@UpgradeCode (uppercased + braced
# by WiX in the Upgrade table). Changing it silently breaks the upgrade chain.
EXPECTED_UPGRADE_CODE = "{4A7B8C00-F1E2-4D3A-B5C6-000000000001}"


class TestUpgradeTable:
    """MSI major-upgrade authoring — the runtime upgrade itself needs a real
    install, but the authoring that drives it is fully checkable here."""

    def test_upgrade_code_stable(self, msi_db):
        rows = msi_db.query("SELECT UpgradeCode FROM Upgrade")
        assert rows, "Upgrade table empty — MajorUpgrade not authored"
        for (code,) in rows:
            assert code == EXPECTED_UPGRADE_CODE, \
                f"UpgradeCode drift: {code} != {EXPECTED_UPGRADE_CODE}"

    def test_downgrade_detected(self, msi_db):
        """MajorUpgrade emits a downgrade-detection row (blocks installing an older
        version over a newer one)."""
        props = {r[0] for r in msi_db.query("SELECT ActionProperty FROM Upgrade")}
        assert "WIX_DOWNGRADE_DETECTED" in props, f"no downgrade row; got {props}"

    def test_remove_existing_products_after_init(self, msi_db):
        """RemoveExistingProducts runs after InstallInitialize, so a major upgrade
        removes the old version instead of installing alongside it."""
        seq = {a: int(s)
               for a, s in msi_db.query(
                   "SELECT Action, Sequence FROM InstallExecuteSequence")
               if s}
        assert "RemoveExistingProducts" in seq, \
            "RemoveExistingProducts not sequenced — old version won't be removed"
        assert seq["RemoveExistingProducts"] > seq["InstallInitialize"], \
            "RemoveExistingProducts must run after InstallInitialize"


# ── Service table tests ──

class TestServiceInstall:
    """The two machine Windows services install under per-service virtual accounts
    with auto-start and flat-5 s restart recovery (§ Windows Services)."""

    EXPECTED_ACCOUNTS = {
        "FaunaNest": "NT SERVICE\\FaunaNest",
        "FaunaBridge": "NT SERVICE\\FaunaBridge",
    }

    def test_two_machine_services_under_virtual_accounts(self, msi_db):
        rows = msi_db.query("SELECT Name, StartName FROM ServiceInstall")
        assert {name: account for name, account in rows} == self.EXPECTED_ACCOUNTS

    def test_services_auto_start_own_process(self, msi_db):
        """ServiceType=16 (own-process), StartType=2 (auto), ErrorControl=1 (normal)."""
        rows = msi_db.query(
            "SELECT Name, ServiceType, StartType, ErrorControl FROM ServiceInstall"
        )
        for name, stype, start, errctl in rows:
            assert stype == "16", f"{name} ServiceType={stype}, want 16 (ownProcess)"
            assert start == "2", f"{name} StartType={start}, want 2 (auto)"
            assert errctl == "1", f"{name} ErrorControl={errctl}, want 1 (normal)"

    def test_service_control_full_lifecycle(self, msi_db):
        """ServiceControl Event=163 = Start(1)+Stop(2)+UninstallStop(32)+
        UninstallDelete(128): started on install, stopped on install+uninstall,
        deleted on uninstall — so an uninstall fully deregisters the service."""
        rows = msi_db.query("SELECT Name, Event FROM ServiceControl")
        assert {name for name, _ in rows} == set(self.EXPECTED_ACCOUNTS)
        for name, event in rows:
            assert event == "163", f"{name} ServiceControl Event={event}, want 163"

    def test_failure_recovery_flat_5s_restart(self, msi_db):
        """util:ServiceConfig → Wix4ServiceConfig: restart x3, flat 5 s delay,
        reset after 1 day, for both services."""
        rows = msi_db.query(
            "SELECT ServiceName, FirstFailureActionType, SecondFailureActionType, "
            "ThirdFailureActionType, RestartServiceDelayInSeconds, ResetPeriodInDays "
            "FROM Wix4ServiceConfig"
        )
        assert {r[0] for r in rows} == set(self.EXPECTED_ACCOUNTS)
        for name, a1, a2, a3, delay, reset in rows:
            assert (a1, a2, a3) == ("restart", "restart", "restart"), \
                f"{name} failure actions {(a1, a2, a3)}"
            assert delay == "5", f"{name} restart delay {delay}s, want 5"
            assert reset == "1", f"{name} reset period {reset}d, want 1"

    def test_no_faunasync_service(self, msi_db):
        """Sync migrated to a per-user logon agent — no FaunaSync in any service table."""
        for table in ("ServiceInstall", "ServiceControl"):
            names = {r[0] for r in msi_db.query(f"SELECT Name FROM {table}")}
            assert "FaunaSync" not in names, f"FaunaSync still in {table}: {names}"
        cfg = {r[0] for r in msi_db.query("SELECT ServiceName FROM Wix4ServiceConfig")}
        assert "FaunaSync" not in cfg, f"FaunaSync still in Wix4ServiceConfig: {cfg}"


# ── Sync-agent PE subsystem (no console window at logon) ──

class TestSyncAgentSubsystem:
    """The shipped fauna-sync-agent.exe MUST be a GUI-subsystem (PE subsystem 2) binary so
    the per-user HKLM \\Run logon launch never flashes a console window — OneDrive
    model (installers/windows.md § Services and the per-user sync agent; the flip is
    the `windows_subsystem = "windows"` crate attribute in
    bins/fauna-sync-agent/src/main.rs, active on release/dist). A pre-flip
    *console* build (subsystem 3) passes every other
    structural check here — file count, services, firewall, dual-ffi — yet pops a
    terminal at logon: the 2026-06-23 stale-binary reship, where a cargo `dist`
    artifact built *before* the flip was re-staged into the MSI. The MSI database
    can't expose the subsystem (the exe is LZX-compressed in the embedded cabinet),
    so this reads the *staged* binary — the exact bytes `wix build` harvests."""

    def test_staged_sync_agent_is_gui_subsystem(self):
        repo = _get_repo_root()
        # Both staging roots: `stage/` is where the installer README's hand build
        # stages, `build/installer/stage/` is where test_installer.py's
        # `_build_msi` does. Reading only the first skipped this gate for every
        # MSI the test builder produced.
        candidates = [
            os.path.join(repo, *root, arch, "fauna-sync-agent.exe")
            for root in (("stage",), ("build", "installer", "stage"))
            for arch in ("arm64", "x64")
        ]
        present = [p for p in candidates if os.path.exists(p)]
        if not present:
            pytest.skip(
                "no staged fauna-sync-agent.exe under stage/{arm64,x64}/ or "
                "build/installer/stage/{arm64,x64}/ — build the MSI first "
                "(apps/fauna-windows/installer/README.md § Building from a "
                "clean checkout)"
            )
        for p in present:
            sub = _pe_subsystem(p)
            assert sub == 2, (
                f"{p} is PE subsystem {sub} (3 = console → a terminal flashes at "
                f"logon). A shipped dist/release fauna-sync-agent.exe must be subsystem 2 "
                f"(GUI). A stale pre-flip artifact was re-staged — rebuild the dist "
                f"binaries from current source (installer README step 7, `dist` profile)."
            )


# ── Firewall table tests ──

class TestFirewall:
    """The network-reachable nest opens an inbound TCP :443 firewall exception
    scoped to fauna-nest-svc.exe (WixToolset.Firewall.wixext → Wix5FirewallException
    table — note the v5 prefix even though Util/UI are v4). It rides the
    FaunaNestSvcExe component, so it installs only when the Nest feature is selected
    and is removed on uninstall with the component."""

    def test_nest_inbound_443_exception(self, msi_db):
        rows = msi_db.query(
            "SELECT Name, Port, Program, Component_ FROM Wix5FirewallException"
        )
        assert rows, "no firewall exception authored (Wix5FirewallException empty)"
        assert len(rows) == 1, f"expected exactly 1 firewall rule, got {rows}"
        name, port, program, component = rows[0]
        assert port == "443", f"firewall port = {port!r}, want 443"
        assert component == "FaunaNestSvcExe", \
            f"firewall rule on component {component!r}, want FaunaNestSvcExe"
        assert program == "[#FaunaNestSvcExe]", \
            f"firewall not scoped to the nest exe: Program={program!r}"


class TestShellExtUpgradeSafety:
    """Path 1 (USER-decision 2026-06-25): the shell DLL is installed side-by-side under a
    VERSION-STAMPED filename and made UNTOUCHABLE by the installer, so an upgrade or
    uninstall never has to free the in-use DLL's lock. explorer.exe holds fauna_shell.dll
    via COM; MSI 4.0+ auto-engages the Restart Manager for ANY in-use file it removes or
    replaces (regardless of authoring), which CLOSES Explorer and strands the desktop.
    The prior in-place close-and-restart machinery — the RestartExplorer CA, the three
    AutoRestartShell-suppression CAs (B1), and the util:RestartResource RM registration —
    was PROVEN to strand the desktop under live RDP test 2026-06-23→25 and is REMOVED.
    Regression guard that Path 1 is authored as designed. See ShellExt.wxs / Package.wxs
    and docs/goal/architecture/installers/windows.md § Shell Extension
    (tracked internally)."""

    PERMANENT = 0x10        # msidbComponentAttributesPermanent
    NEVER_OVERWRITE = 0x80  # msidbComponentAttributesNeverOverwrite

    def test_shell_dll_filename_is_version_stamped(self, msi_db):
        """The installed shell DLL @Name is fauna_shell_<ProductVersion>.dll. Coupled to
        ProductVersion so a Package Version bump that forgets to bump ShellExt.wxs's
        $(var.ShellDllName) is caught here (the two must move together)."""
        version = _product_version(msi_db)
        expected = f"fauna_shell_{version}.dll"
        rows = msi_db.query("SELECT FileName FROM File WHERE Component_ = 'FaunaShellDll'")
        assert len(rows) == 1, f"expected exactly one shell DLL file, got {rows}"
        name = rows[0][0].split("|")[-1] if "|" in rows[0][0] else rows[0][0]
        assert name == expected, (
            f"shell DLL @Name = {name!r}, want {expected!r} — ShellExt.wxs "
            f"$(var.ShellDllName) must track Package Version {version}"
        )

    def test_shell_dll_component_permanent_and_neveroverwrite(self, msi_db):
        """The shell DLL component is Permanent (MSI NEVER removes the in-use file — not on
        uninstall, not via RemoveExistingProducts during a major upgrade, so the genuine-
        change upgrade's old DLL is merely orphaned, never RM-removed) AND NeverOverwrite
        (a same-version reinstall never rewrites the in-use file). Together with the
        version-stamped filename, the in-use DLL is untouchable → RM never engaged →
        Explorer never closed."""
        rows = msi_db.query(
            "SELECT Attributes FROM Component WHERE Component = 'FaunaShellDll'"
        )
        assert rows, "FaunaShellDll component missing"
        attrs = int(rows[0][0])
        assert attrs & self.PERMANENT, (
            f"FaunaShellDll not Permanent (Attributes={attrs:#x}) — RemoveExistingProducts "
            "would remove the in-use DLL on a genuine-version upgrade and strand the desktop"
        )
        assert attrs & self.NEVER_OVERWRITE, (
            f"FaunaShellDll not NeverOverwrite (Attributes={attrs:#x}) — a same-version "
            "reinstall would rewrite the in-use DLL and strand the desktop"
        )

    def test_no_restart_resource_table(self, msi_db):
        """util:RestartResource is removed — the shell DLL is no longer registered with the
        Restart Manager (Path 1 never wants RM to close Explorer; the DLL was the only
        RestartResource, so no Wix*RestartResource table should exist)."""
        tables = [r[0] for r in msi_db.query("SELECT Name FROM _Tables")]
        leftover = [t for t in tables if "RestartResource" in t]
        assert not leftover, (
            f"a Wix*RestartResource table is still present ({leftover}) — Path 1 removes "
            "the shell-ext RM registration so RM is not engaged for the in-use DLL"
        )

    def test_no_explorer_restart_or_autorestartshell_cas(self, msi_db):
        """The in-place close-and-restart CAs are GONE: RestartExplorer (relaunched the
        shell after RM closed it) and the three AutoRestartShell-suppression CAs (B1) — all
        proven to strand the desktop, superseded by Path 1."""
        dead_cas = (
            "RestartExplorer", "SuppressAutoRestartShell",
            "RestoreAutoRestartShell", "Rollback_RestoreAutoRestartShell",
        )
        actions = {a for (a,) in msi_db.query("SELECT Action FROM CustomAction")}
        for dead in dead_cas:
            assert dead not in actions, (
                f"{dead} CA is still present — Path 1 removes the in-place "
                "close-and-restart machinery (it stranded the desktop)"
            )
        seq = {a for a, *_ in msi_db.query("SELECT Action FROM InstallExecuteSequence")}
        for dead in dead_cas:
            assert dead not in seq, f"{dead} still sequenced in InstallExecuteSequence"

    def test_kill_fauna_app_retained(self, msi_db):
        """KillFaunaApp is a SEPARATE concern (the FaunaApp.exe files-in-use lock, not the
        shell) and survives the Path 1 CA cleanup. Softened 2026-07-15: it now runs AFTER
        InstallValidate (the built-in Restart Manager gracefully closes cooperating
        current-version apps there — session-end persists drafts + relaunches), so this CA
        is the pure last-resort force-only net for the windowless / cross-session strays RM
        can't close. Running it BEFORE InstallValidate pre-empted RM; a WM_CLOSE/Start-Sleep
        step would re-introduce that weaker graceful path (RM owns graceful now)."""
        actions = {a for (a,) in msi_db.query("SELECT Action FROM CustomAction")}
        assert "KillFaunaApp" in actions, \
            "KillFaunaApp CA was dropped — it guards the FaunaApp.exe lock, unrelated to the shell"
        seq = {a: int(s) for a, s, *_ in
               msi_db.query("SELECT Action, Sequence FROM InstallExecuteSequence") if s}
        assert "KillFaunaApp" in seq, "KillFaunaApp not sequenced in InstallExecuteSequence"
        assert seq["KillFaunaApp"] > seq["InstallValidate"], (
            "KillFaunaApp must run AFTER InstallValidate — RM shuts cooperating apps down at "
            "InstallValidate; running before pre-empts RM's graceful, draft-preserving close"
        )
        assert seq["KillFaunaApp"] < seq["InstallFiles"], (
            "KillFaunaApp must run before InstallFiles — the force net must clear any surviving "
            "lock before file transfer"
        )
        target = msi_db.query("SELECT Target FROM CustomAction WHERE Action = 'KillFaunaApp'")[0][0]
        assert "Stop-Process" in target, "KillFaunaApp must still force-kill in-scope survivors"
        assert "CloseMainWindow" not in target and "Start-Sleep" not in target, (
            "KillFaunaApp body must be force-only — RM owns the graceful close; a WM_CLOSE / "
            "Start-Sleep step re-introduces the pre-empting graceful path"
        )

    def test_kill_fauna_sync_retained(self, msi_db):
        """KillFaunaSync (2026-07-17 live incident) is the sync-agent counterpart of
        KillFaunaApp: the agent is a per-user Run-key process with NO lifecycle window at
        all, so the built-in Restart Manager can never close it — unlike FaunaApp there is no
        cooperative path to try first, so this CA is not a last-resort net, it is the ONLY
        mechanism. Observed live: a stale pre-upgrade sync agent ran a full day past an
        install and, hitting the pre-persistence teardown path, destroyed un-hydrated
        placeholders out from under the new install."""
        actions = {a for (a,) in msi_db.query("SELECT Action FROM CustomAction")}
        assert "KillFaunaSync" in actions, \
            "KillFaunaSync CA missing — an upgrade leaves the old fauna-sync-agent.exe running indefinitely"
        seq = {a: int(s) for a, s, *_ in
               msi_db.query("SELECT Action, Sequence FROM InstallExecuteSequence") if s}
        assert "KillFaunaSync" in seq, "KillFaunaSync not sequenced in InstallExecuteSequence"
        assert seq["KillFaunaSync"] > seq["InstallValidate"], (
            "KillFaunaSync must run after InstallValidate, same window as KillFaunaApp"
        )
        assert seq["KillFaunaSync"] < seq["InstallFiles"], (
            "KillFaunaSync must run before InstallFiles — the lock on fauna-sync-agent.exe must "
            "clear before file transfer can replace it"
        )
        target = msi_db.query("SELECT Target FROM CustomAction WHERE Action = 'KillFaunaSync'")[0][0]
        assert "fauna-sync-agent.exe" in target, "KillFaunaSync must target fauna-sync-agent.exe"
        # Only the shipped image name. The pre-A5 `fauna-sync.exe` match served an upgrade
        # from a 0.1.1-or-earlier install; none exists, so the compat-remnant sweep removed it
        # (version-compatibility.md § Dimension 2, the fourth ratified exception).
        assert "fauna-sync.exe" not in target, (
            "KillFaunaSync must match only fauna-sync-agent.exe — the pre-rename image name "
            "was a pre-sweep upgrade remnant"
        )
        assert "Stop-Process" in target, "KillFaunaSync must force-kill in-scope survivors"
        assert "[INSTALLFOLDER]" in target, (
            "KillFaunaSync must be path-scoped to [INSTALLFOLDER] — never kill fauna-sync-agent.exe "
            "by image name alone (would hit a sibling dev checkout's own builds on this box)"
        )

    # ── Orphan-DLL cleanup (Path 1 follow-on, 2026-06-26) ──
    # The shell DLL is Permanent, so a version-bump upgrade leaves the prior fauna_shell_<oldver>.dll
    # and an uninstall leaves the current one. A single DEFERRED + no-impersonate (LocalSystem) CA,
    # CleanShellDlls, reclaims them via MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT) semantics — appending
    # to HKLM\SYSTEM PendingFileRenameOperations. It MUST be deferred/system (an immediate CA can't
    # write HKLM\SYSTEM — proven live 2026-06-26) and reads everything from the registry + filesystem
    # (NO MSI props) so it sidesteps the deferred-EXE [CustomActionData]-resolves-empty trap (also
    # proven live 2026-06-26).

    def test_orphan_cleanup_ca_present(self, msi_db):
        """The deferred cleanup CA exists."""
        actions = {a for (a,) in msi_db.query("SELECT Action FROM CustomAction")}
        assert "CleanShellDlls" in actions, "CleanShellDlls CA missing — orphans would never be reclaimed"

    def test_orphan_cleanup_is_deferred_no_impersonate(self, msi_db):
        """CleanShellDlls MUST be deferred (0x400) + no-impersonate (0x800) → runs as LocalSystem,
        the only context that can write HKLM\\SYSTEM PendingFileRenameOperations. An immediate CA
        silently failed to write it (proven live 2026-06-26); this guards against regressing."""
        rows = msi_db.query("SELECT Type FROM CustomAction WHERE Action = 'CleanShellDlls'")
        assert rows, "CleanShellDlls CA missing"
        t = int(rows[0][0])
        assert t & 0x400, f"CleanShellDlls not deferred (Type={t:#x}) — immediate can't write HKLM\\SYSTEM"
        assert t & 0x800, f"CleanShellDlls not no-impersonate/system (Type={t:#x}) — needed for HKLM\\SYSTEM"

    def test_orphan_cleanup_no_msi_props_and_not_removefile(self, msi_db):
        """The deferred CA writes PendingFileRenameOperations (MoveFileEx reboot-delete), reads the
        active DLL from the overlay CLSID's InprocServer32, and uses NO MSI bracket props — so
        [CustomActionData]/[..] can't resolve to EMPTY at runtime (the proven deferred-EXE trap).
        NOT WiX RemoveFile (RM-aware → re-strands)."""
        target = dict(msi_db.query("SELECT Action, Target FROM CustomAction")).get("CleanShellDlls") or ""
        assert "PendingFileRenameOperations" in target, \
            f"CleanShellDlls must write PendingFileRenameOperations, got {target[:120]!r}"
        assert "4a7b8c01" in target.lower(), \
            "CleanShellDlls must read the active DLL from the overlay CLSID InprocServer32"
        assert r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" in target, (
            "CleanShellDlls must invoke powershell by ABSOLUTE path — MSI's deferred-CA host does not "
            "search the working dir, so a relative path fails with error 1314 (proven live 2026-06-27)"
        )
        assert "[" not in target and "]" not in target, (
            "CleanShellDlls must use NO MSI bracket props — a deferred EXE CA resolves [CustomActionData]/[..] "
            f"to empty at runtime (proven 2026-06-26). Target={target[:120]!r}"
        )
        assert "{}" not in target, (
            "CleanShellDlls must contain NO empty {} — MSI's Formatted-field processor strips it, so "
            "catch{} becomes catch; (a PowerShell parse error → exit 1, proven 2026-06-27)"
        )
        tables = {r[0] for r in msi_db.query("SELECT Name FROM _Tables")}
        if "RemoveFile" in tables:
            shell_rf = [r for r in msi_db.query("SELECT FileName FROM RemoveFile")
                        if "fauna_shell" in (r[0] or "").lower()]
            assert not shell_rf, f"RemoveFile targets a shell DLL ({shell_rf}) — RM-aware, would re-strand"

    def test_orphan_cleanup_tries_immediate_delete_first(self, msi_db):
        """Option-(a) refinement (no-reboot when possible): the CA attempts a direct
        Remove-Item on each stale fauna_shell_*.dll and only reboot-defers
        (PendingFileRenameOperations) the ones still present afterwards (genuinely
        locked / Explorer-held). This reclaims a NOT-currently-loaded orphan
        (fresh-install-over-leftovers) immediately with NO reboot, while the in-use DLL
        still gets the strand-safe reboot-delete baseline. Strand-safe: a failed
        Remove-Item on a locked file is a no-op — it never force-closes Explorer."""
        target = dict(msi_db.query("SELECT Action, Target FROM CustomAction")).get("CleanShellDlls") or ""
        assert "Remove-Item" in target, (
            "CleanShellDlls must try an immediate Remove-Item before reboot-deferring, so a "
            "not-currently-loaded orphan is reclaimed without a reboot"
        )
        # Delete-first ordering: Remove-Item must precede the \??\ reboot-delete append, so
        # only files that SURVIVE the immediate delete (locked) are scheduled for reboot.
        # Search for \??\ STARTING FROM di, not the first occurrence in the whole script:
        # the 2026-07-17 prune-before-add fix (defect A) introduces an earlier, unrelated
        # \??\ occurrence — $keepEntry, built to RECOGNISE a stale pending-delete pair, not
        # to schedule one — so "first \??\ in the string" no longer means "the reboot-delete
        # append this test is pinning."
        di = target.index("Remove-Item")
        ai = target.index("\\??\\", di)
        assert di < ai, (
            "Remove-Item must run BEFORE the \\??\\ reboot-delete append — only a file that "
            f"survives the immediate delete (locked) gets reboot-deferred. Target={target[:200]!r}"
        )
        # The reboot-defer append must be gated by a re-check that the file still exists
        # after the delete attempt (Test-Path between the delete and the append).
        assert "Test-Path" in target[di:ai], (
            "the \\??\\ reboot-defer must be guarded by a Test-Path re-check AFTER Remove-Item — "
            "an unlocked orphan that was just deleted must not also be reboot-scheduled"
        )

    def test_orphan_cleanup_sequencing_and_condition(self, msi_db):
        """CleanShellDlls runs Before InstallFinalize (after registry is written on install / removed
        on uninstall) and is guarded NOT UPGRADINGPRODUCTCODE (so it doesn't over-schedule the new
        DLL during a major upgrade's RemoveExistingProducts of the old product)."""
        seq = {a: ((c or ""), int(s)) for a, c, s in
               msi_db.query("SELECT Action, Condition, Sequence FROM InstallExecuteSequence") if s}
        assert "CleanShellDlls" in seq, "CleanShellDlls not sequenced in InstallExecuteSequence"
        cond, s = seq["CleanShellDlls"]
        assert "UPGRADINGPRODUCTCODE" in cond.upper(), \
            f"CleanShellDlls must be guarded NOT UPGRADINGPRODUCTCODE, got {cond!r}"
        fin = seq.get("InstallFinalize")
        assert fin and s < fin[1], "CleanShellDlls must run before InstallFinalize"

    def test_orphan_cleanup_prunes_pending_delete_for_kept_dll(self, msi_db):
        """a prior uninstall (DLL locked) reboot-defers
        a delete-pair for it; a same-version reinstall never used to revisit that pending pair, so
        the DLL this run just decided to KEEP stayed armed for silent deletion at the next reboot
        (observed live: THREE stacked delete-pairs for the live DLL). The script must now walk the
        existing PendingFileRenameOperations entries and drop any pair whose source is the
        just-resolved keep path, before (or as well as) scheduling any new deletes."""
        target = dict(msi_db.query("SELECT Action, Target FROM CustomAction")).get("CleanShellDlls") or ""
        assert "keepEntry" in target or "keepentry" in target.lower(), (
            "CleanShellDlls must compute the active DLL's own PendingFileRenameOperations entry "
            "shape so it can be recognised and pruned from the existing list"
        )
        assert "foreach" in target.lower(), (
            "CleanShellDlls must walk the existing PendingFileRenameOperations pairs (foreach, no "
            "array indexing — see the no-'[..]' assertion below) to prune the kept DLL's stale entry"
        )
        # Same escaping-contract assertions the read-side test already pins, re-checked because the
        # prune logic is new code in the same Formatted-field-processed ExeCommand string.
        assert "[" not in target and "]" not in target, (
            "the prune logic must not introduce array-indexing '[..]' syntax — MSI's Formatted-field "
            f"processor mangles any bracket in ExeCommand. Target={target[:200]!r}"
        )
        assert "{}" not in target, (
            "the prune logic must not introduce an empty {} — MSI strips it into a parse error"
        )

    def test_orphan_cleanup_writeback_fires_on_prune_alone(self, msi_db):
        """The write-back (Set-ItemProperty) must fire whenever anything was PRUNED, not only when
        something was newly scheduled — a same-version reinstall with no orphans to reclaim (nothing
        added) must still persist the pruned list, or the stale delete-pair for the live DLL survives
        untouched."""
        target = dict(msi_db.query("SELECT Action, Target FROM CustomAction")).get("CleanShellDlls") or ""
        assert "cur.Count -ne $raw.Count" in target or "cur.Count-ne$raw.Count" in target.replace(" ", ""), (
            "the Set-ItemProperty write-back guard must also trigger when pruning shrank the list "
            "(cur.Count vs raw.Count), not only when $add.Count -gt 0"
        )


# ── Registry table tests (shell-ext CLSIDs, fauna:// protocol, data-dir) ──

OVERLAY_CLSIDS = [
    "{4a7b8c01-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",
    "{4a7b8c02-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",
    "{4a7b8c03-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",
    "{4a7b8c04-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",
]
CONTEXT_CLSIDS = [
    "{4a7b8c10-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",  # root FaunaContextMenu
    "{4a7b8c11-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",  # share
    "{4a7b8c12-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",  # info-devices
    "{4a7b8c13-f1e2-4d3a-b5c6-d7e8f9a0b1c2}",  # info-versions
]
ALL_CLSIDS = set(OVERLAY_CLSIDS + CONTEXT_CLSIDS)


class TestRegistryTable:
    """Shell-extension COM registration, fauna:// protocol handler, and the
    data-dir value the REMOVE_USER_DATA removal reads back at uninstall.
    These ride at the MSI-table level only — Explorer actually loading the DLL is
    covered separately (apps/windows.md § Shell Extension)."""

    def _registry(self, msi_db):
        # `Key` is a reserved word in MSI SQL — backtick-quote the identifiers.
        return msi_db.query("SELECT `Key`, `Name`, `Value` FROM `Registry`")

    @staticmethod
    def _clsid_of(key):
        return key.split("\\CLSID\\")[1].split("\\")[0].lower()

    def test_sync_logon_run_key(self, msi_db):
        """Sync is launched per-user via an HKLM \\Run value (replaces the FaunaSync service)."""
        by_key = {(key, name): value for key, name, value in self._registry(msi_db)}
        assert by_key.get(("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run", "Fauna Sync")) \
            == "[INSTALLFOLDER]fauna-sync-agent.exe", \
            f"Run-key launch missing/wrong: {by_key.get(('SOFTWARE\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Run', 'Fauna Sync'))!r}"

    def test_all_eight_clsids_have_inprocserver(self, msi_db):
        """All 8 CLSIDs register InprocServer32 → the version-stamped
        fauna_shell_<ProductVersion>.dll (Path 1). A dropped CLSID here is exactly the
        ShellExt.wxs ↔ shell-ext lockstep break; a wrong filename here means the COM
        registration points at a DLL the MSI doesn't install."""
        expected = f"[INSTALLFOLDER]fauna_shell_{_product_version(msi_db)}.dll"
        seen = set()
        for key, name, value in self._registry(msi_db):
            if key.endswith("\\InprocServer32") and name == "":
                assert value == expected, \
                    f"{key} InprocServer32 = {value}, want {expected}"
                seen.add(self._clsid_of(key))
        assert seen == ALL_CLSIDS, \
            f"InprocServer32 CLSID set mismatch:\n got {sorted(seen)}\nwant {sorted(ALL_CLSIDS)}"

    def test_all_clsids_apartment_threaded(self, msi_db):
        threaded = {
            self._clsid_of(key)
            for key, name, value in self._registry(msi_db)
            if name == "ThreadingModel" and value == "Apartment"
        }
        assert threaded == ALL_CLSIDS

    def test_overlay_keys_space_prefixed(self, msi_db):
        """The 4 ShellIconOverlayIdentifiers leaf keys are space-prefixed so Fauna's
        overlays sort ahead of competitors within Windows' 15-overlay limit."""
        leaves = [
            key.split("\\")[-1]
            for key, name, value in self._registry(msi_db)
            if "ShellIconOverlayIdentifiers\\" in key
        ]
        assert len(leaves) == 4, f"expected 4 overlay identifier keys, got {leaves}"
        for leaf in leaves:
            assert leaf.startswith(" "), f"overlay key not space-prefixed: {leaf!r}"

    def test_context_menu_handler_registered_as_explorer_command(self, msi_db):
        """The MSI registers the root submenu as an *IExplorerCommand* handler.

        `ExplorerCommandHandler` under a `shell\\<verb>` key is the only registry surface
        Explorer honours for IExplorerCommand. `FaunaContextMenu` implements *only*
        IExplorerCommand, so the legacy `shellex\\ContextMenuHandlers` key — whose
        contract is IShellExtInit + IContextMenu — makes Explorer QueryInterface, get
        E_NOINTERFACE, and silently drop the handler. That is what shipped until
        2026-07-14: the submenu never appeared in any menu, while every in-process test
        stayed green. This test pins the key, and its Rust twin
        (`registration_contract_matches_implemented_interface`) pins the interface.
        """
        by_key = {(key, name): value for key, name, value in self._registry(msi_db)}
        # Both verb keys: all files, and folders (same CLSID — the folder menu's
        # reduced leaf set lives in the handler; USER-decided 2026-07-16).
        for verb in ("SOFTWARE\\Classes\\*\\shell\\Fauna",
                     "SOFTWARE\\Classes\\Directory\\shell\\Fauna"):
            handler = by_key.get((verb, "ExplorerCommandHandler"))
            assert handler == CONTEXT_CLSIDS[0], \
                f"{verb} ExplorerCommandHandler → {handler}, want {CONTEXT_CLSIDS[0]}"

        legacy = [
            key for key, name, value in self._registry(msi_db)
            if "shellex\\ContextMenuHandlers" in key
        ]
        assert not legacy, \
            f"the wrong-contract legacy ContextMenuHandlers key is back: {legacy}"

    def test_fauna_protocol_handler(self, msi_db):
        """fauna:// is a URL Protocol that launches FaunaApp.exe."""
        by_key = {(key, name): value
                  for key, name, value in self._registry(msi_db)}
        assert by_key.get(("SOFTWARE\\Classes\\fauna", "URL Protocol")) == "", \
            "fauna scheme missing 'URL Protocol' marker"
        cmd = by_key.get(("SOFTWARE\\Classes\\fauna\\shell\\open\\command", ""))
        assert cmd == '"[AppFolder]FaunaApp.exe" "%1"', f"fauna:// command = {cmd!r}"

    def test_datadir_registry_value(self, msi_db):
        """The DataDir value the uninstall RemoveFolderEx reads back is written under
        the required Sync feature (DataDirRegistry component)."""
        data = {
            value for key, name, value in self._registry(msi_db)
            if key == "SOFTWARE\\Fauna\\Install" and name == "DataDir"
        }
        assert data == {"[FaunaDataFolder]"}, f"DataDir = {data}"


# ── Sparse-package registration (STEP E — the Win11 default context menu) ──

# The sparse package's Identity/@Name (AppxManifest.xml.in), by which the deregister
# CA finds the provisioned + per-user registrations to remove.
SPARSE_IDENTITY_NAME = "FaunaSocial.Fauna"
# The .msix the MSI ships into INSTALLFOLDER and the register CA hands to Add-AppxPackage.
SPARSE_MSIX_NAME = "Fauna-Sparse.msix"
# Packaging-with-external-location floor (Windows 10 2004); below it the register CA
# no-ops and the HKCR verb (ShellExt.wxs) remains the only, correct, surface.
SPARSE_OS_FLOOR = "19041"
# The overlay CLSID the deferred register CA reads InprocServer32 from to recover the
# install directory WITHOUT any MSI property (the deferred-EXE [CustomActionData]-empty
# trap — same trick CleanShellDlls uses).
OVERLAY_CLSID_SHORT = "4a7b8c01"
PS51_ABS = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"


class TestSparsePackageRegistration:
    """STEP E — the MSI must ship + register the sparse package that grants the shell
    extension PACKAGE IDENTITY (the only way handlers reach the Windows 11 DEFAULT
    context menu; the HKCR verb reaches only "Show more options").

    There is no MSI-native mechanism, so install runs
    `Add-AppxPackage -Stage <msix> -ExternalLocation <installdir>` +
    `Add-AppxProvisionedPackage -Online` (per-machine) and uninstall the matching
    `Remove-AppxProvisionedPackage` + `Remove-AppxPackage -AllUsers`
    (docs/goal/architecture/installers/windows.md § Menu placement / § Implementation
    status, and MS Learn "grant-identity-to-nonpackaged-apps" § Per-Machine).

    Both CAs obey the same four deferred-CA traps CleanShellDlls documents (proven live):
    per-machine provisioning needs LocalSystem → deferred + no-impersonate; a deferred
    EXE CA's [CustomActionData]/[PROPERTY] resolves EMPTY at runtime → read the install
    dir from the overlay CLSID InprocServer32 instead; powershell.exe by ABSOLUTE path
    (the deferred-CA host doesn't search the working dir); and the escaping contract —
    no double-quotes / no `&` / no `[..]` / no empty `{}`."""

    DEFERRED = 0x400
    NO_IMPERSONATE = 0x800

    @staticmethod
    def _ca_target(msi_db, action):
        return dict(msi_db.query("SELECT Action, Target FROM CustomAction")).get(action) or ""

    @staticmethod
    def _ca_type(msi_db, action):
        rows = msi_db.query(f"SELECT Type FROM CustomAction WHERE Action = '{action}'")
        return int(rows[0][0]) if rows else None

    # ── existence + privilege ──

    def test_register_ca_present(self, msi_db):
        actions = {a for (a,) in msi_db.query("SELECT Action FROM CustomAction")}
        assert "RegisterSparsePackage" in actions, (
            "RegisterSparsePackage CA missing — the MSI never registers the sparse "
            "package, so the Win11 default menu stays empty (only the HKCR verb ships)"
        )

    def test_deregister_ca_present(self, msi_db):
        actions = {a for (a,) in msi_db.query("SELECT Action FROM CustomAction")}
        assert "DeregisterSparsePackage" in actions, (
            "DeregisterSparsePackage CA missing — uninstall leaves a stale package "
            "registration pointing at a deleted external location (goal doc § Menu placement)"
        )

    def test_both_cas_deferred_no_impersonate(self, msi_db):
        """Per-machine provisioning (Add-/Remove-AppxProvisionedPackage -Online) needs
        LocalSystem → both CAs must be deferred (0x400) + no-impersonate (0x800). An
        immediate CA impersonates the installing user (not elevated) and can't provision
        machine-wide — the exact reason CleanShellDlls is deferred/system."""
        for action in ("RegisterSparsePackage", "DeregisterSparsePackage"):
            t = self._ca_type(msi_db, action)
            assert t is not None, f"{action} CA missing"
            assert t & self.DEFERRED, f"{action} not deferred (Type={t:#x})"
            assert t & self.NO_IMPERSONATE, f"{action} not no-impersonate/system (Type={t:#x})"

    # ── register CA contract ──

    def test_register_uses_stage_externallocation_and_provision(self, msi_db):
        """The per-machine register sequence (MS Learn § Per-Machine): stage the package
        bound to its external location, then provision it online for all users."""
        target = self._ca_target(msi_db, "RegisterSparsePackage")
        for token in ("Add-AppxPackage", "-Stage", "-ExternalLocation",
                      "Add-AppxProvisionedPackage", "-Online", SPARSE_MSIX_NAME):
            assert token in target, (
                f"RegisterSparsePackage must contain {token!r} (per-machine sparse "
                f"registration); Target={target[:200]!r}"
            )

    def test_register_reads_install_dir_from_overlay_clsid(self, msi_db):
        """A deferred EXE CA can't read [INSTALLFOLDER] (resolves EMPTY at runtime —
        proven live 2026-06-26). So it recovers the install dir from the overlay CLSID's
        InprocServer32 value and Split-Parents it — the CleanShellDlls trick — never an
        MSI bracket property."""
        target = self._ca_target(msi_db, "RegisterSparsePackage")
        assert OVERLAY_CLSID_SHORT in target.lower(), (
            "RegisterSparsePackage must recover the install dir from the overlay CLSID "
            f"InprocServer32 ({OVERLAY_CLSID_SHORT}), not an MSI property"
        )
        assert "Split-Path" in target, \
            "RegisterSparsePackage must Split-Path the CLSID DLL path to the install dir"

    def test_register_gates_on_os_build_floor(self, msi_db):
        """Below Windows 10 build 19041 there is no packaging-with-external-location at
        all, so the CA must no-op there and leave the HKCR verb as the only surface. The
        gate reads CurrentBuildNumber (no MSI property, no bracket cast)."""
        target = self._ca_target(msi_db, "RegisterSparsePackage")
        assert SPARSE_OS_FLOOR in target, \
            f"RegisterSparsePackage must gate on build {SPARSE_OS_FLOOR}; Target={target[:200]!r}"
        assert "CurrentBuildNumber" in target, \
            "RegisterSparsePackage must read CurrentBuildNumber to apply the 19041 floor"

    # ── deregister CA contract ──

    def test_deregister_removes_provisioned_and_all_users(self, msi_db):
        """Uninstall deprovisions (all users) then removes any per-user registration,
        matched by the package DisplayName == Identity/@Name (survives the dev→prod cert
        swap, which changes the PackageFamilyName hash but not the DisplayName)."""
        target = self._ca_target(msi_db, "DeregisterSparsePackage")
        for token in ("Remove-AppxProvisionedPackage", "Remove-AppxPackage",
                      "-AllUsers", SPARSE_IDENTITY_NAME):
            assert token in target, (
                f"DeregisterSparsePackage must contain {token!r}; Target={target[:200]!r}"
            )

    # ── the four deferred-CA traps, for BOTH CAs ──

    def test_both_cas_absolute_powershell_and_escaping_contract(self, msi_db):
        """Both CAs invoke powershell by ABSOLUTE path (deferred-CA host doesn't search
        the working dir → relative fails 1314, proven live 2026-06-27) and obey the
        escaping contract that MSI's Formatted-field processor + XML escaping impose: NO
        MSI bracket props (a deferred EXE CA resolves [..] to empty), NO `&`, and NO empty
        `{}` (stripped → `catch{}` becomes `catch;`, a parse error — proven 2026-06-27)."""
        for action in ("RegisterSparsePackage", "DeregisterSparsePackage"):
            target = self._ca_target(msi_db, action)
            assert PS51_ABS in target, (
                f"{action} must invoke powershell by absolute path {PS51_ABS!r} "
                f"(built-in Windows PowerShell 5.1 — pwsh isn't guaranteed present)"
            )
            assert "[" not in target and "]" not in target, (
                f"{action} must use NO MSI bracket props — a deferred EXE CA resolves them "
                f"to empty at runtime. Target={target[:200]!r}"
            )
            assert "&" not in target, (
                f"{action} must contain no `&` — it doesn't survive MSI formatting + XML "
                f"escaping. Target={target[:200]!r}"
            )
            assert "{}" not in target, (
                f"{action} must contain no empty {{}} — MSI's Formatted-field processor "
                f"strips it into a parse error. Target={target[:200]!r}"
            )

    # ── sequencing + conditions ──

    def _seq(self, msi_db):
        return {a: ((c or ""), int(s)) for a, c, s in
                msi_db.query("SELECT Action, Condition, Sequence FROM InstallExecuteSequence")
                if s}

    def test_register_sequenced_on_install_before_cleanup(self, msi_db):
        """RegisterSparsePackage runs on install (NOT REMOVE) after WriteRegistryValues
        (so the overlay CLSID it reads exists) and after InstallFiles (so the .msix is on
        disk) — anchored before CleanShellDlls, hence before InstallFinalize."""
        seq = self._seq(msi_db)
        assert "RegisterSparsePackage" in seq, "RegisterSparsePackage not sequenced"
        cond, s = seq["RegisterSparsePackage"]
        assert "NOT REMOVE" in cond.upper(), \
            f"RegisterSparsePackage must be conditioned NOT REMOVE (install only), got {cond!r}"
        clean = seq.get("CleanShellDlls")
        assert clean and s < clean[1], \
            "RegisterSparsePackage must run before CleanShellDlls (and thus InstallFinalize)"

    def test_deregister_sequenced_on_uninstall_before_cleanup(self, msi_db):
        """DeregisterSparsePackage runs when the product/feature is being removed (REMOVE)
        and BEFORE CleanShellDlls, so the COM surrogate's hold on the shell DLL is released
        before the orphan cleanup runs."""
        seq = self._seq(msi_db)
        assert "DeregisterSparsePackage" in seq, "DeregisterSparsePackage not sequenced"
        cond, s = seq["DeregisterSparsePackage"]
        assert "REMOVE" in cond.upper(), \
            f"DeregisterSparsePackage must be conditioned on REMOVE (uninstall), got {cond!r}"
        clean = seq.get("CleanShellDlls")
        assert clean and s < clean[1], \
            "DeregisterSparsePackage must run before CleanShellDlls (release the surrogate first)"

    # ── the .msix ships in the MSI under the ShellExt feature ──

    def test_msix_shipped_under_shellext(self, msi_db):
        """The sparse .msix ships into INSTALLFOLDER as a component of the ShellExt
        feature — so it is present for the register CA and removed on uninstall alongside
        the rest of the shell integration."""
        files = {
            (r[0].split("|")[-1] if "|" in r[0] else r[0]): r[1]
            for r in msi_db.query("SELECT FileName, Component_ FROM File")
        }
        assert SPARSE_MSIX_NAME in files, f"{SPARSE_MSIX_NAME} missing from the MSI File table"
        component = files[SPARSE_MSIX_NAME]
        shellext_components = {
            c for f, c in msi_db.query("SELECT Feature_, Component_ FROM FeatureComponents")
            if f == "ShellExt"
        }
        assert component in shellext_components, (
            f"{SPARSE_MSIX_NAME} rides component {component!r}, not a ShellExt component "
            f"{sorted(shellext_components)}"
        )


class TestCleanSyncRoots:
    """Uninstall must clean PERSISTENT cfapi sync-root registrations (2026-07-16 ask).

    Sync-root registrations became persistent (they belong to the folder-binding, not
    the service's lifetime), so a plain MSI uninstall strands every `Fauna!*`
    SyncRootManager entry — Explorer keeps rendering a dead "Fauna – <set>" cloud
    location the user can't remove without regedit. `CleanSyncRoots` runs the
    just-installed fauna-sync-agent.exe with `--cleanup-roots`
    (fauna-sync-agent::cfapi_host::unregister_all_shell_sync_roots), which reuses the
    product's own cfapi primitives (docs/goal/behavior/file-sync.md § Per-file
    sync-status display).

    Unlike CleanShellDlls / RegisterSparsePackage (which need LocalSystem to write
    HKLM\\SYSTEM / provision machine-wide), this only needs the SAME non-elevated
    per-user context fauna-sync-agent.exe itself always runs in — so it is IMMEDIATE, not
    deferred, and can reference [INSTALLFOLDER] directly (only deferred EXE CAs hit the
    resolves-to-empty-at-runtime trap CleanShellDlls's own comments document)."""

    @staticmethod
    def _ca_row(msi_db, action):
        rows = msi_db.query(
            f"SELECT Type, Target FROM CustomAction WHERE Action = '{action}'"
        )
        return (int(rows[0][0]), rows[0][1]) if rows else (None, None)

    def test_ca_present(self, msi_db):
        actions = {a for (a,) in msi_db.query("SELECT Action FROM CustomAction")}
        assert "CleanSyncRoots" in actions, (
            "CleanSyncRoots CA missing — uninstall would strand every Fauna! "
            "SyncRootManager entry (a ghost cloud location Explorer keeps rendering)"
        )

    def test_ca_is_immediate_not_deferred(self, msi_db):
        """Runs impersonated as the installing user — the same non-elevated per-user
        context fauna-sync-agent.exe itself always runs in — so it must NOT be deferred/
        no-impersonate (that would run it as LocalSystem/session 0 instead)."""
        t, _ = self._ca_row(msi_db, "CleanSyncRoots")
        assert t is not None, "CleanSyncRoots CA missing"
        assert not (t & 0x400), f"CleanSyncRoots must be immediate, not deferred (Type={t:#x})"
        assert not (t & 0x800), f"CleanSyncRoots must impersonate, not run as system (Type={t:#x})"

    def test_ca_invokes_cleanup_roots_flag(self, msi_db):
        """Targets the installed fauna-sync-agent.exe with the dedicated cleanup flag,
        quoted (INSTALLFOLDER contains a space — 'Program Files')."""
        _, target = self._ca_row(msi_db, "CleanSyncRoots")
        assert target, "CleanSyncRoots CA missing"
        assert "fauna-sync-agent.exe" in target, "CleanSyncRoots must target fauna-sync-agent.exe"
        assert "--cleanup-roots" in target, "CleanSyncRoots must pass --cleanup-roots"
        assert '"[INSTALLFOLDER]fauna-sync-agent.exe"' in target, (
            f"CleanSyncRoots must quote the [INSTALLFOLDER]-qualified exe path "
            f"(INSTALLFOLDER contains a space); Target={target!r}"
        )

    def test_ca_sequenced_after_kill_fauna_sync_uninstall_only(self, msi_db):
        """Must run AFTER KillFaunaSync (the old agent's live filter connection must be
        torn down first — a still-connected root refuses the shell unregister), on
        uninstall only (REMOVE), and never during a routine upgrade's old-product
        teardown (NOT UPGRADINGPRODUCTCODE) — an ordinary upgrade must not strip every
        binding and force the new agent to silently re-register everything."""
        seq = {a: ((c or ""), int(s)) for a, c, s in
               msi_db.query("SELECT Action, Condition, Sequence FROM InstallExecuteSequence")
               if s}
        assert "CleanSyncRoots" in seq, "CleanSyncRoots not sequenced in InstallExecuteSequence"
        cond, s = seq["CleanSyncRoots"]
        assert "REMOVE" in cond.upper(), f"CleanSyncRoots must be conditioned on REMOVE, got {cond!r}"
        assert "UPGRADINGPRODUCTCODE" in cond.upper(), (
            f"CleanSyncRoots must guard NOT UPGRADINGPRODUCTCODE (never during a routine "
            f"upgrade's old-product teardown), got {cond!r}"
        )
        kill_sync = seq.get("KillFaunaSync")
        assert kill_sync and s > kill_sync[1], (
            "CleanSyncRoots must run AFTER KillFaunaSync — the old agent's live filter "
            "connection must be torn down before the shell unregister can succeed"
        )

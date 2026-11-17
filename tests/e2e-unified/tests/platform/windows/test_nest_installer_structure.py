"""Structural validation of the Windows NEST MSI — no admin required.

The nest MSI (`apps/fauna-windows/installer/nest/Package.wxs`) is the hosting
installer of the direct download: the Nest service, the Bridge service and the
Terminal App, and nothing of the client — no desktop app, no sync agent, no
Explorer integration, no `fauna://` handler, no sparse package and none of the
custom actions that served them (docs/goal/architecture/installers/windows.md
§ Direct download (MSIX app + nest MSI), 1. The nest MSI's features). Nest is on
by default; the UpgradeCode is the full-feature package's, so it replaces one as
a major upgrade.

The MSI comes through its one door, `conftest.py::nest_msi_path`, which links it
when its inputs are newer than its stamp — no publish, since it has no app — and
the database is queried read-only through msi.dll, so this suite runs green
unelevated, the verify queue's on Windows included. The full-feature package's own
suite is `test_installer_structure.py`.
"""

import sys
import tomllib
from pathlib import Path

import pytest

from helpers.msi_database import MsiDatabase

pytestmark = [pytest.mark.skipif(sys.platform != "win32", reason="Windows-only"), pytest.mark.tier_3,
              pytest.mark.timeout(3600)]

_REPO = Path(__file__).resolve().parents[5]

#: The full-feature package's UpgradeCode, kept on purpose (§ Direct download, 1,
#: the upgrade bullet): the nest MSI replaces a full-feature install.
EXPECTED_UPGRADE_CODE = "{4A7B8C00-F1E2-4D3A-B5C6-000000000001}"

#: The nest MSI's components, every one: the two services and the mail bridge's
#: three files, the terminal app's two, the DataDir registry value and the three
#: licence files. A client component appearing here is the trim undone.
EXPECTED_COMPONENTS = {
    "FaunaNestSvcExe", "FaunaBridgeSvcExe",
    "FaunaMailBridgeExe", "FaunaMailBridgeFfiDll", "FaunaMailBridgeLibunwindDll",
    "FaunaTuiExe", "FaunaCliExe",
    "DataDirRegistry",
    "LicenseMitTxt", "LicenseApacheTxt", "ThirdPartyNoticesTxt",
}

NOTICE_FILES = {"LicenseMitTxt": "LICENSE-MIT.txt", "LicenseApacheTxt": "LICENSE-APACHE.txt",
                "ThirdPartyNoticesTxt": "THIRD-PARTY-NOTICES.txt"}


@pytest.fixture(scope="session")
def nest_msi_db(nest_msi_path):
    """Open the nest MSI, built or cached by its door, for structural queries."""
    with MsiDatabase(nest_msi_path) as db:
        yield db


def _features(db):
    return {f: (parent, level, int(attrs) if attrs else 0)
            for f, parent, level, attrs in db.query("SELECT Feature, Feature_Parent, Level, Attributes FROM Feature")}


def _files(db):
    """File name → component, from the File table (`short|long` names reduced to the long)."""
    return {raw.split("|")[-1]: comp for raw, comp in db.query("SELECT FileName, Component_ FROM File")}


def _property(db, name):
    rows = db.query(f"SELECT Value FROM Property WHERE Property = '{name}'")
    return rows[0][0] if rows else None


class TestFeatureTree:
    def test_the_three_features_and_no_other(self, nest_msi_db):
        """Nest with its Bridge child, and the Terminal App beside it — no
        DesktopApp, Sync or ShellExt (§ Direct download, 1)."""
        features = _features(nest_msi_db)
        assert set(features) == {"Nest", "Bridge", "TerminalApp"}, sorted(features)
        assert features["Nest"][0] == "", "Nest is a top-level feature"
        assert features["TerminalApp"][0] == "", (
            "TerminalApp is top-level: a box may want the terminal app without the nest")
        assert features["Bridge"][0] == "Nest", "Bridge requires Nest, by nesting alone"

    def test_nest_and_the_terminal_app_on_by_default_the_bridge_off(self, nest_msi_db):
        """Nest is on: the reason it was off belonged to an installer that also
        carried the client (§ Direct download, 1, the defaults bullet)."""
        features = _features(nest_msi_db)
        assert features["Nest"][1] == "1", f"Nest Level={features['Nest'][1]}, expected 1 (default on)"
        assert features["TerminalApp"][1] == "1", f"TerminalApp Level={features['TerminalApp'][1]}"
        assert int(features["Bridge"][1]) > 1, f"Bridge Level={features['Bridge'][1]}, expected default off"

    def test_every_feature_deselectable(self, nest_msi_db):
        UI_DISALLOW_ABSENT = 0x10
        for fid, (_, _, attrs) in _features(nest_msi_db).items():
            assert attrs & UI_DISALLOW_ABSENT == 0, f"Feature {fid} is UIDisallowAbsent (Attributes={attrs})"


class TestNoClient:
    """Nothing of the client is left in the package."""

    def test_exactly_the_nest_components(self, nest_msi_db):
        got = {c for (c,) in nest_msi_db.query("SELECT Component FROM Component")}
        assert got == EXPECTED_COMPONENTS, (
            f"extra: {sorted(got - EXPECTED_COMPONENTS)}, missing: {sorted(EXPECTED_COMPONENTS - got)}")

    def test_no_client_file(self, nest_msi_db):
        files = _files(nest_msi_db)
        for name in ("FaunaApp.exe", "fauna-sync-agent.exe", "Fauna-Sparse.msix"):
            assert name not in files, f"{name} is in the nest MSI"
        assert not [f for f in files if f.lower().startswith("fauna_shell")], "a shell extension DLL is in the nest MSI"
        assert not [f for f in files if f.lower().endswith(".ico")], "overlay icons are in the nest MSI"

    def test_no_shortcut_no_protocol_no_shell_registration_no_run_key(self, nest_msi_db):
        shortcuts = nest_msi_db.query("SELECT Shortcut FROM Shortcut") if self._has_table(nest_msi_db, "Shortcut") else []
        assert not shortcuts, f"Start menu shortcuts in the nest MSI: {shortcuts}"
        # `Key` is a reserved word of MSI SQL: quoted, or the view fails (1615).
        keys = [k for (k,) in nest_msi_db.query("SELECT `Key` FROM `Registry`")]
        for key in keys:
            low = key.lower()
            assert not low.startswith("clsid\\") and "\\clsid\\" not in low, f"a COM class is registered: {key}"
            assert "shellex" not in low and "shelliconoverlayidentifiers" not in low, f"a shell registration: {key}"
            assert not low.startswith("fauna"), f"the fauna:// handler is registered: {key}"
            assert "currentversion\\run" not in low, f"a logon Run key is written: {key}"
        # What remains: the DataDir value alone.
        assert keys == ["SOFTWARE\\Fauna\\Install"], keys

    def test_no_custom_action_of_its_own(self, nest_msi_db):
        """Every custom action the full-feature package authors served the client
        (KillFaunaApp, KillFaunaSync, CleanShellDlls, the two sparse-package
        actions, CleanSyncRoots); the nest MSI carries only WiX's own — the
        service-config, folder-removal and firewall actions its extensions add."""
        actions = [a for (a,) in nest_msi_db.query("SELECT Action FROM CustomAction")]
        own = [a for a in actions if not a.startswith(("Wix4", "Wix5", "Wix6"))]
        assert own == [], f"custom actions of the package's own: {own}"

    def test_no_client_directory(self, nest_msi_db):
        dirs = {d for (d,) in nest_msi_db.query("SELECT Directory FROM Directory")}
        for d in ("AppFolder", "FaunaStartMenu", "SyncDataFolder"):
            assert d not in dirs, f"the client directory {d} is in the nest MSI"

    @staticmethod
    def _has_table(db, table):
        return bool(db.query(f"SELECT Name FROM _Tables WHERE Name = '{table}'"))


class TestIdentity:
    def test_product_name(self, nest_msi_db):
        assert _property(nest_msi_db, "ProductName") == "Fauna Nest"

    def test_upgrade_code_is_the_full_feature_packages(self, nest_msi_db):
        assert _property(nest_msi_db, "UpgradeCode") == EXPECTED_UPGRADE_CODE
        rows = nest_msi_db.query("SELECT UpgradeCode, ActionProperty FROM Upgrade")
        assert rows and all(code == EXPECTED_UPGRADE_CODE for code, _ in rows), rows
        assert "WIX_DOWNGRADE_DETECTED" in {p for _, p in rows}, rows

    def test_a_major_upgrade_removes_the_previous_install_first(self, nest_msi_db):
        seq = {a: int(s) for a, s in nest_msi_db.query("SELECT Action, Sequence FROM InstallExecuteSequence") if s}
        assert seq["RemoveExistingProducts"] > seq["InstallInitialize"]

    def test_version_is_the_product_version(self, nest_msi_db):
        cargo = tomllib.loads((_REPO / "Cargo.toml").read_text(encoding="utf-8"))
        assert _property(nest_msi_db, "ProductVersion") == cargo["workspace"]["package"]["version"]


class TestServicesAndFirewall:
    EXPECTED_ACCOUNTS = {"FaunaNest": "NT SERVICE\\FaunaNest", "FaunaBridge": "NT SERVICE\\FaunaBridge"}

    def test_two_machine_services_under_virtual_accounts(self, nest_msi_db):
        rows = nest_msi_db.query("SELECT Name, StartName FROM ServiceInstall")
        assert dict(rows) == self.EXPECTED_ACCOUNTS

    def test_service_control_and_recovery(self, nest_msi_db):
        assert {n: e for n, e in nest_msi_db.query("SELECT Name, Event FROM ServiceControl")} == {
            "FaunaNest": "163", "FaunaBridge": "163"}
        rows = nest_msi_db.query(
            "SELECT ServiceName, FirstFailureActionType, SecondFailureActionType, "
            "ThirdFailureActionType, RestartServiceDelayInSeconds, ResetPeriodInDays FROM Wix4ServiceConfig")
        assert {r[0] for r in rows} == set(self.EXPECTED_ACCOUNTS)
        for name, a1, a2, a3, delay, reset in rows:
            assert (a1, a2, a3, delay, reset) == ("restart", "restart", "restart", "5", "1"), name

    def test_nest_inbound_443_exception(self, nest_msi_db):
        rows = nest_msi_db.query("SELECT Port, Program, Component_ FROM Wix5FirewallException")
        assert rows == [["443", "[#FaunaNestSvcExe]", "FaunaNestSvcExe"]], rows


class TestRemovePath:
    """The uninstall dialog's "Also permanently delete this data" (`UI.wxs`'s
    `RemoveDataDlg`) is reached only when the install recorded a data folder: the
    `DataDirRegistry` value rides the Nest feature alone, so a Terminal-App-only
    install has nothing for the checkbox to delete and its Remove path goes straight
    to WiX's confirm page (installers/windows.md § Uninstall → Full Removal)."""

    ROUTE = 'WixUI_InstallMode = "Remove" AND FAUNA_PROGRAMDATA_DIR'

    def test_every_route_into_the_dialog_needs_a_recorded_data_folder(self, nest_msi_db):
        rows = nest_msi_db.query(
            "SELECT `Dialog_`, `Control_`, `Event`, `Condition` FROM `ControlEvent` "
            "WHERE `Argument` = 'RemoveDataDlg'")
        routes = {(d, c, e): " ".join((cond or "").split()) for d, c, e, cond in rows}
        assert set(routes) == {("MaintenanceTypeDlg", "RemoveButton", "NewDialog"),
                               ("VerifyReadyDlg", "Back", "NewDialog")}, rows
        for route, condition in routes.items():
            assert condition == self.ROUTE, f"{route} opens RemoveDataDlg on {condition!r}"

    def test_the_data_folder_is_read_back_before_the_wizard_runs(self, nest_msi_db):
        """`FAUNA_PROGRAMDATA_DIR` comes from AppSearch's `DataDir` lookup, which the UI
        sequence runs, so the maintenance dialogs see it set or empty."""
        assert ["FAUNA_PROGRAMDATA_DIR", "FindProgramData"] in nest_msi_db.query(
            "SELECT Property, Signature_ FROM AppSearch")
        assert nest_msi_db.query("SELECT Root, `Key`, Name FROM RegLocator WHERE Signature_ = 'FindProgramData'") == [
            ["2", "SOFTWARE\\Fauna\\Install", "DataDir"]]
        ui_actions = {a for (a,) in nest_msi_db.query("SELECT Action FROM InstallUISequence")}
        assert "AppSearch" in ui_actions, sorted(ui_actions)


class TestFeatureContents:
    def _feature_components(self, db):
        out = {}
        for feature, comp in db.query("SELECT Feature_, Component_ FROM FeatureComponents"):
            out.setdefault(feature, set()).add(comp)
        return out

    def test_each_feature_carries_its_own(self, nest_msi_db):
        fc = self._feature_components(nest_msi_db)
        assert {"FaunaNestSvcExe", "DataDirRegistry"} <= fc["Nest"]
        assert {"FaunaBridgeSvcExe", "FaunaMailBridgeExe", "FaunaMailBridgeFfiDll",
                "FaunaMailBridgeLibunwindDll"} <= fc["Bridge"]
        assert {"FaunaTuiExe", "FaunaCliExe"} <= fc["TerminalApp"]

    def test_every_top_level_feature_carries_the_licence_files(self, nest_msi_db):
        fc = self._feature_components(nest_msi_db)
        for feature in ("Nest", "TerminalApp"):
            assert set(NOTICE_FILES) <= fc[feature], f"{feature} does not carry the licence files"

    def test_the_licence_files_sit_in_installfolder_and_are_not_empty(self, nest_msi_db):
        rows = nest_msi_db.query(
            "SELECT `File`.`FileName`, `File`.`FileSize`, `File`.`Component_`, `Component`.`Directory_` "
            "FROM `File`, `Component` WHERE `File`.`Component_` = `Component`.`Component`")
        by_component = {comp: (raw.split("|")[-1], size, d) for raw, size, comp, d in rows}
        for comp, name in NOTICE_FILES.items():
            got, size, directory = by_component[comp]
            assert (got, directory) == (name, "INSTALLFOLDER"), (comp, got, directory)
            assert int(size or 0) > 0, f"{name} is empty in the built MSI"

    def test_one_machine_path_entry_on_the_terminal_app(self, nest_msi_db):
        rows = nest_msi_db.query("SELECT Name, Value, Component_ FROM Environment")
        assert len(rows) == 1, rows
        name, value, component = rows[0]
        assert name.lstrip("=-*!+").upper() == "PATH" and "*" in name, name
        assert "[INSTALLFOLDER]" in value and component == "FaunaTuiExe", rows

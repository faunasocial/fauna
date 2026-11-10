"""tier_1: which architecture's staged Store layout the development-mode nodes of
``tests/artifact/test_windows_store_install.py`` register, with no ``pwsh`` call.

The x64 leg of the Store witness runs the x64 ``store-no-payments`` layout under
emulation on an Arm box (``docs/goal/architecture/packaging-witnesses.md``
§ Implementation status today, the bullet on how the Windows Store install is
driven). The run chooses it with ``--store-layout-arch``; without the option the
nodes read the host's own architecture, as before. These pin that choice, the
stage path and recipe it names, and the pick among installed packages of one name
(``Get-AppxPackage`` answers a list when both architectures are installed).
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import windows_store_install as store  # noqa: E402

pytestmark = pytest.mark.tier_1

ARM64_FULL = "FaunaSocial.FaunaSocial_0.1.6.0_arm64__hra91aebets7r"
X64_FULL = "FaunaSocial.FaunaSocial_0.1.6.0_x64__hra91aebets7r"


@pytest.fixture
def arm_host(monkeypatch):
    """An Arm64 host, as Windows reports one to a native process."""
    monkeypatch.delenv("PROCESSOR_ARCHITEW6432", raising=False)
    monkeypatch.setenv("PROCESSOR_ARCHITECTURE", "ARM64")


@pytest.mark.parametrize("option", [None, ""])
def test_no_option_selects_the_host_architecture(arm_host, option):
    assert store.selected_layout_arch(option) == "arm64"


def test_an_explicit_architecture_is_selected(arm_host):
    assert store.selected_layout_arch("x64") == "x64"
    assert store.selected_layout_arch("arm64") == "arm64"


def test_an_architecture_no_store_package_builds_is_refused(arm_host):
    with pytest.raises(ValueError, match="x86"):
        store.selected_layout_arch("x86")


def test_the_selected_architecture_names_its_stage_and_recipe(arm_host, tmp_path):
    arch = store.selected_layout_arch("x64")
    assert store.staged_layout_manifest(tmp_path, arch) == (
        tmp_path / "build" / "installer" / "store-stage" / "x64" / "AppxManifest.xml"
    )
    assert store.layout_recipe(arch).startswith("just windows-store-package x64 store-no-payments ")
    # The default is unchanged: the host's own stage.
    assert store.staged_layout_manifest(tmp_path) == (
        tmp_path / "build" / "installer" / "store-stage" / "arm64" / "AppxManifest.xml"
    )


def test_the_pick_prefers_the_selected_architecture():
    both = [{"PackageFullName": ARM64_FULL}, {"PackageFullName": X64_FULL}]
    assert store.pick_package(both, "x64")["PackageFullName"] == X64_FULL
    assert store.pick_package(both, "arm64")["PackageFullName"] == ARM64_FULL


def test_the_pick_takes_the_one_installed_when_the_selected_is_absent():
    """Only one architecture installed: that one is what the catalogue holds, and
    the fixture's registered-from check decides whether it is the module's."""
    assert store.pick_package([{"PackageFullName": ARM64_FULL}], "x64")["PackageFullName"] == ARM64_FULL
    assert store.pick_package({"PackageFullName": X64_FULL}, "arm64")["PackageFullName"] == X64_FULL


def test_the_pick_defaults_to_the_host_architecture(arm_host):
    both = [{"PackageFullName": X64_FULL}, {"PackageFullName": ARM64_FULL}]
    assert store.pick_package(both)["PackageFullName"] == ARM64_FULL

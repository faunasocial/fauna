"""Structural validation of the Win11 default-context-menu **sparse package**.

Pure XML parsing of the checked-in manifest template — no MSI, no driver, no nest,
no Windows API. Runs on every machine (the coupling it guards is fleet-wide), which
is why there is no `skipif(sys.platform != "win32")` here.

Background — why a sparse package exists at all
-----------------------------------------------
An `HKCR` verb + `ExplorerCommandHandler` registration surfaces **only** in the
legacy ("Show more options") menu. The Windows 11 **default** context menu renders
handlers only from a component carrying **package identity**, which a non-packaged
Win32 app obtains via a *sparse package* (a manifest-only MSIX with
`uap10:AllowExternalContent`, registered with `Add-AppxPackage -ExternalLocation`).
See `docs/goal/architecture/installers/windows.md` § Context-menu registration.

The load-bearing invariants pinned here
---------------------------------------
1. **No 9th CLSID.** The sparse package must point at the *same* root
   `IExplorerCommand` CLSID that `ShellExt.wxs` already registers — it is a second
   *registration surface* for one implementation, not a second implementation. The
   expected GUID is therefore **read out of `ShellExt.wxs`**, never restated here:
   drift between the two files is exactly the bug this test exists to catch.
2. **`Identity/@Publisher` stays a template token.** MSIX binds the manifest to the
   signing certificate by requiring `Publisher` to equal the cert's Subject DN
   *exactly*. Hard-coding a DN would weld the package to one certificate and turn
   the eventual dev-cert → production-cert swap into a code change. It must remain a
   build-time parameter. (This is the same reasoning that keeps the track unblocked
   by code-signing at all: the cert gates distribution, not authoring.)
3. **The DLL path is the version-stamped name.** `com:Class/@Path` resolves inside
   the `-ExternalLocation` directory and must name the same version-stamped shell DLL
   the MSI installs (`fauna_shell_<ver>.dll`, Path 1) — not the bare build-output name.
"""

import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

import pytest

pytestmark = pytest.mark.tier_1


NS = {
    "pkg": "http://schemas.microsoft.com/appx/manifest/foundation/windows10",
    "uap10": "http://schemas.microsoft.com/appx/manifest/uap/windows10/10",
    "desktop4": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/4",
    "desktop5": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/5",
    "com": "http://schemas.microsoft.com/appx/manifest/com/windows10",
}

# Sparse packaging ("packaging with external location") floor — Windows 10 2004.
# Below this the installer must skip registration and fall back to the HKCR verb.
MIN_OS_VERSION = "10.0.19041.0"


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    ).stdout.strip()
    if sys.platform == "win32" and out.startswith("/"):
        out = out[1].upper() + ":" + out[2:]
    return os.path.normpath(out)


def _installer_dir():
    return os.path.join(_repo_root(), "apps", "fauna-windows", "installer")


@pytest.fixture(scope="module")
def manifest():
    path = os.path.join(_installer_dir(), "sparse", "AppxManifest.xml.in")
    assert os.path.exists(path), f"sparse manifest template missing: {path}"
    return ET.parse(path).getroot()


@pytest.fixture(scope="module")
def wxs_root_clsid():
    """The root IExplorerCommand CLSID, read from ShellExt.wxs (never restated).

    This is the GUID the MSI writes as `*\\shell\\Fauna`'s `ExplorerCommandHandler`.
    The sparse package must reuse it verbatim.
    """
    wxs = os.path.join(_installer_dir(), "ShellExt.wxs")
    with open(wxs, encoding="utf-8") as f:
        text = f.read()
    m = re.search(
        r'Name="ExplorerCommandHandler"\s+Type="string"\s+Value="\{([0-9a-fA-F-]+)\}"',
        text,
    )
    assert m, "could not find the ExplorerCommandHandler CLSID in ShellExt.wxs"
    return m.group(1).lower()


class TestSparsePackageManifest:
    def test_reuses_the_existing_root_clsid_no_ninth_guid(self, manifest, wxs_root_clsid):
        """Every CLSID the manifest names is the *existing* root context-menu CLSID.

        The sparse package is a second registration surface for one COM class, so it
        must not mint a new GUID. Both the `com:Class/@Id` (which registers the class
        in the manifest instead of the registry) and every `desktop5:Verb/@Clsid`
        (which binds the File Explorer verb to it) must equal the CLSID ShellExt.wxs
        already registers.
        """
        com_ids = [
            e.get("Id") for e in manifest.iterfind(".//com:Class", NS)
        ]
        verb_clsids = [
            e.get("Clsid") for e in manifest.iterfind(".//desktop5:Verb", NS)
        ]
        assert com_ids, "manifest declares no com:Class — the COM server is unregistered"
        assert verb_clsids, "manifest declares no desktop5:Verb — no context menu"

        named = {g.strip("{}").lower() for g in com_ids + verb_clsids}
        assert named == {wxs_root_clsid}, (
            f"sparse package names CLSIDs {sorted(named)}, but must reuse ONLY the root "
            f"IExplorerCommand CLSID from ShellExt.wxs ({wxs_root_clsid}) — no 9th GUID"
        )

    def test_publisher_is_a_template_token_not_a_hardcoded_cert_dn(self, manifest):
        """`Identity/@Publisher` must stay substitutable.

        MSIX requires Publisher == the signing cert's Subject DN, exactly. A literal DN
        here would weld the package to one certificate, making the dev-cert →
        production-cert swap a code change instead of a build parameter (and silently
        breaking the build the day the real "Fauna Social" cert lands).
        """
        identity = manifest.find("pkg:Identity", NS)
        assert identity is not None, "manifest has no <Identity>"
        publisher = identity.get("Publisher")
        assert publisher == "@@Publisher@@", (
            f'Identity/@Publisher is "{publisher}" — it must remain the literal token '
            '"@@Publisher@@" so the signing cert stays a build-time parameter'
        )

    def test_allows_external_content(self, manifest):
        """Sparse packaging: the payload (the shell DLL) lives OUTSIDE the package."""
        el = manifest.find("pkg:Properties/uap10:AllowExternalContent", NS)
        assert el is not None and (el.text or "").strip().lower() == "true", (
            "uap10:AllowExternalContent must be true — without it the manifest cannot "
            "reference the shell DLL at its -ExternalLocation install directory"
        )

    def test_com_class_path_is_the_version_stamped_shell_dll(self, manifest):
        """`com:Class/@Path` must be the templated, version-stamped DLL name.

        Path 1 installs the shell DLL as `fauna_shell_<ProductVersion>.dll` so an
        upgrade never touches the in-use file. The sparse package resolves @Path inside
        the -ExternalLocation dir, so it must name that same stamped file — a bare
        `fauna_shell.dll` would resolve to nothing on an installed machine.
        """
        cls = manifest.find(".//com:Class", NS)
        assert cls is not None
        assert cls.get("Path") == "@@ShellDllName@@", (
            f'com:Class/@Path is "{cls.get("Path")}" — must be the token "@@ShellDllName@@", '
            "substituted with the same version-stamped name ShellExt.wxs installs"
        )

    def test_targets_the_sparse_package_os_floor(self, manifest):
        """Below Windows 10 2004 there is no sparse packaging at all."""
        tdf = manifest.find("pkg:Dependencies/pkg:TargetDeviceFamily", NS)
        assert tdf is not None, "manifest declares no TargetDeviceFamily"
        assert tdf.get("MinVersion") == MIN_OS_VERSION, (
            f'TargetDeviceFamily/@MinVersion is {tdf.get("MinVersion")}, want {MIN_OS_VERSION} '
            "(the packaging-with-external-location floor)"
        )

    def test_registers_the_verb_for_files_and_folders(self, manifest):
        """The menu must appear on files, folders, and folder backgrounds.

        The shell extension acts on tracked files *and* tracked directories, so all
        three shell item types carry the verb — a `*`-only registration would leave the
        menu absent on the folder surfaces where sharing a set is most useful.
        """
        types = {
            e.get("Type") for e in manifest.iterfind(".//desktop5:ItemType", NS)
        }
        assert {"*", "Directory", "Directory\\Background"} <= types, (
            f"sparse package registers item types {sorted(types)}; want at least "
            "'*', 'Directory' and 'Directory\\\\Background'"
        )

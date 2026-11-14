"""tier_1: the MSI's sources install the three licence files beside the binaries.

`docs/goal/architecture/shipped-notices.md` § The rule: every artifact Fauna ships
in binary form carries LICENSE-MIT, LICENSE-APACHE (the repository's files byte for
byte) and its own THIRD-PARTY-NOTICES; § Per channel, the Windows MSI row: in
`INSTALLFOLDER`, beside the binaries, `.txt`. This module pins the authoring side
by text, on every machine: `Notices.wxs` installs the three files from
`$(var.NoticesDir)`, every top-level feature of `Package.wxs` carries them (every
feature is deselectable, and a command-line ADDLOCAL installs only what it names),
`just windows-msi-notices` writes them, and the test fixture's MSI build runs that
recipe and hands WiX the directory. The release workflow's half is pinned by the
publish tooling's workflow-shape test; the built MSI's half (the files are in the
package and not empty) by `test_installer_structure.py`.

Nothing here reads a generated notices file: it is dependency-authored text.
"""

import re
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

from helpers.windows_installer import msi_notices_faults

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[5]
_INSTALLER = _REPO / "apps" / "fauna-windows" / "installer"
_WIX = "{http://wixtoolset.org/schemas/v4/wxs}"
FILES = ("LICENSE-MIT.txt", "LICENSE-APACHE.txt", "THIRD-PARTY-NOTICES.txt")


def _wxs(name: str) -> ET.Element:
    return ET.parse(_INSTALLER / name).getroot()


def _recipe_body(name: str) -> str:
    text = (_REPO / "justfile").read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    return "\n".join(ln for ln in m.group(1).splitlines() if ln.strip() and not ln.strip().startswith("#"))


def test_notices_wxs_installs_the_three_files_under_installfolder():
    groups = [g for g in _wxs("Notices.wxs").iter(f"{_WIX}ComponentGroup") if g.get("Id") == "NoticesComponents"]
    assert len(groups) == 1, "Notices.wxs defines no NoticesComponents group"
    group = groups[0]
    assert group.get("Directory") == "INSTALLFOLDER"
    files = [f for c in group.iter(f"{_WIX}Component") for f in c.iter(f"{_WIX}File")]
    assert sorted(f.get("Name") for f in files) == sorted(FILES)
    for f in files:
        assert f.get("Source") == f"$(var.NoticesDir){f.get('Name')}", f"{f.get('Name')} is not read from NoticesDir"
        assert f.get("KeyPath") == "yes"


def test_every_top_level_feature_carries_the_licence_files():
    package = _wxs("Package.wxs").find(f"{_WIX}Package")
    top = package.findall(f"{_WIX}Feature")
    assert {f.get("Id") for f in top} >= {"DesktopApp", "Sync", "TerminalApp", "Nest"}
    for feature in top:
        refs = {r.get("Id") for r in feature.findall(f"{_WIX}ComponentGroupRef")}
        assert "NoticesComponents" in refs, f"top-level feature {feature.get('Id')} does not carry the licence files"


def test_the_staging_recipe_writes_the_two_licences_and_the_msis_notices():
    body = _recipe_body("windows-msi-notices")
    assert re.search(r'just shipped-licences "\{\{dir\}\}" \.txt', body), body
    assert re.search(r'shipped-notices\.py windows-msi "\{\{dir\}\}/THIRD-PARTY-NOTICES\.txt"', body), body
    assert "go.binary={{bridge}}" in body
    # one MSI ships one arch: its notices are resolved for that arch's triples
    assert '--target "$CPU-pc-windows-msvc" --target "$CPU-pc-windows-gnullvm"' in body
    assert re.search(r"arm64\) CPU=aarch64 ;;", body) and re.search(r"x64\)\s+CPU=x86_64 ;;", body)
    shipped = _recipe_body("shipped-licences")
    assert 'cp LICENSE-MIT "{{dir}}/LICENSE-MIT{{ext}}"' in shipped
    assert 'cp LICENSE-APACHE "{{dir}}/LICENSE-APACHE{{ext}}"' in shipped


def test_the_windows_msi_artifact_names_the_msi_row():
    import tomllib

    art = tomllib.loads((_REPO / "installer" / "shipped-notices.toml").read_text(encoding="utf-8"))["artifact"]["windows-msi"]
    assert art["channel"] == "Windows MSI"
    assert set(art) >= {"rust", "go", "nuget"}
    # the windows build stages the bridge in target/ (`_windows-go-cgo-build`)
    assert art["go"]["binaries"] == ["target/fauna-mail-bridge.exe"]
    assert art["go"]["module"] == "bins/fauna-bridges"


def _manifest() -> dict:
    import tomllib

    return tomllib.loads((_REPO / "installer" / "shipped-notices.toml").read_text(encoding="utf-8"))


def _release_workflow() -> str:
    """release.yml: kept under the publish tree's injected files in
    development, at `.github/workflows/` in the public repository."""
    dev = _REPO / "scripts" / "publish" / "public-files" / ".github" / "workflows" / "release.yml"
    if dev.exists():
        return dev.read_text(encoding="utf-8")
    return (_REPO / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")


def _libunwind() -> dict:
    comps = _manifest()["artifact"]["windows-msi"]["bundled"]["components"]
    found = [c for c in comps if c["name"] == "libunwind"]
    assert len(found) == 1, "the windows-msi artifact names no bundled libunwind"
    return found[0]


def test_the_msis_notices_name_the_libunwind_dll_the_bridge_feature_installs():
    """shipped-notices.md § Per channel, the Windows MSI row: the closure covers
    every feature's binaries — the Bridge feature's libunwind.dll (the gnullvm
    FFI library's runtime, from the llvm-mingw toolchain) included, which no
    package resolver lists, so the manifest names it as a bundled component."""
    group = [g for g in _wxs("ServiceBridge.wxs").iter(f"{_WIX}Component") if g.get("Id") == "FaunaMailBridgeLibunwindDll"]
    assert len(group) == 1, "ServiceBridge.wxs installs no libunwind.dll"
    installed = group[0].find(f"{_WIX}File").get("Name")
    comp = _libunwind()
    assert comp["binary"] == installed
    assert comp["licence"] == "Apache-2.0 WITH LLVM-exception"
    # the toolchain's own licence file, staged beside the dll by the build
    assert comp["files"] == ["llvm-mingw/LICENSE.TXT"]
    assert _manifest()["artifact"]["windows-msi"]["bundled"]["dir"] == "target"


def test_the_libunwind_entry_names_the_llvm_mingw_release_the_builds_pin():
    """The entry's origin and source name the llvm-mingw release; the release
    workflow's mail-bridge job and the win setup guide each pin one. A pin moved
    without the entry would print the old release in every MSI's notices."""
    comp = _libunwind()
    rel = re.search(r"release (\d{8})$", comp["origin"])
    assert rel, comp["origin"]
    assert comp["source"] == f"https://github.com/mstorsjo/llvm-mingw/releases/tag/{rel.group(1)}"
    assert re.findall(r'\$ver = "(\d{8})"', _release_workflow()) == [rel.group(1)], "release.yml pins another llvm-mingw release"
    # the dev machines' setup guide is not published; where it is, it agrees
    guide = _REPO / "docs" / "dev-setup" / "win.md"
    if guide.exists():
        assert re.findall(r'\$ver = "(\d{8})"', guide.read_text(encoding="utf-8")) == [rel.group(1)], \
            "the win setup guide pins another llvm-mingw release"


def _mingw_w64() -> dict:
    comps = _manifest()["artifact"]["windows-msi"]["bundled"]["components"]
    found = [c for c in comps if c["name"] == "mingw-w64 runtime"]
    assert len(found) == 1, "the windows-msi artifact names no bundled mingw-w64 runtime"
    return found[0]


def test_the_msis_notices_name_the_mingw_w64_runtime_the_gnullvm_link_puts_into_the_bridge():
    """shipped-notices.md § How the notices file is produced, the toolchain
    bullet, Owed (d): the mingw-w64 runtime's startup objects and the static
    part of its C runtime are in the mail bridge and its gnullvm FFI library;
    no resolver lists them, so the manifest names the runtime as a bundled
    component carried by the bridge, with the licence files the toolchain
    carries for it, its id passing on an exception that records its reading."""
    comp = _mingw_w64()
    installed = [f.get("Name") for c in _wxs("ServiceBridge.wxs").iter(f"{_WIX}Component") for f in c.iter(f"{_WIX}File")]
    assert comp["binary"] == "fauna-mail-bridge.exe" and comp["binary"] in installed
    assert comp["files"] and all(f.startswith("llvm-mingw/mingw32/COPYING") for f in comp["files"]), comp["files"]
    assert "llvm-mingw/mingw32/COPYING.MinGW-w64-runtime.txt" in comp["files"]
    # the version and origin are the toolchain release the builds pin
    lib = _libunwind()
    assert comp["origin"] == lib["origin"] and comp["source"] == lib["source"]
    assert comp["version"] == re.search(r"release (\d{8})$", lib["origin"]).group(1)
    exc = [e for e in _manifest().get("exception", []) if e.get("ecosystem") == "bundled" and e.get("name") == comp["name"]]
    assert len(exc) == 1 and exc[0]["licence"] == comp["licence"] and exc[0]["reading"], "no exception records the runtime's reading"


def test_the_bridge_build_stages_the_toolchains_licence_and_the_part_carries_it():
    body = _recipe_body("_windows-go-cgo-build")
    assert 'LICENCE="$(dirname "$MINGW_BIN")/LICENSE.TXT"' in body
    assert 'cp "$LICENCE" "$STAGE/llvm-mingw/LICENSE.TXT"' in body
    assert "cp target/llvm-mingw/LICENSE.TXT part/llvm-mingw/" in _release_workflow()
    # the mingw-w64 runtime's files: the target triple's own, in llvm-mingw/mingw32/
    assert 'MINGW_LICENCES="$(dirname "$MINGW_BIN")/${CC_BIN%-clang}/share/mingw32"' in body
    assert 'cp "$MINGW_LICENCES"/COPYING* "$STAGE/llvm-mingw/mingw32/"' in body
    assert "cp -r target/llvm-mingw/mingw32 part/llvm-mingw/" in _release_workflow()
    # the assembly job's notices read the part's directory: the recipe points
    # the bundled leg at the bridge's own directory
    assert '--set "bundled.dir=$(dirname "{{bridge}}")"' in _recipe_body("windows-msi-notices")


def test_the_runtime_packs_are_read_from_the_publish_the_recipe_writes():
    """The self-contained publish copies the .NET runtime and the Windows SDK
    projection into the app from runtime packs, which project.assets.json lists
    only as downloads; the NuGet leg reads them off the published app's
    deps.json, at `windows-app-publish`'s PUBLISH_DIR with `{rid}` for $RID — on
    the MSI and on every Store flavour, which publish the same app."""
    m = re.search(r'PUBLISH_DIR="([^"]+)"', _recipe_body("windows-app-publish"))
    assert m, "windows-app-publish names no PUBLISH_DIR"
    want = m.group(1).replace("$APP", "apps/fauna-windows/FaunaApp/FaunaApp").replace("$RID", "{rid}") + "/FaunaApp.deps.json"
    arts = _manifest()["artifact"]
    for name in ("windows-msi", "windows-store", "windows-store-safe", "windows-store-full"):
        assert arts[name]["nuget"].get("deps") == want, name


# The verdict lines the arm64 MSI's notices run wrote over its real inputs
# (2026-11-13: a fresh mail-bridge build, a real self-contained publish).
_GOOD_VERDICTS = [
    "shipped-notices: windows-msi rust: components=728 files-copied=1147 fallbacks=68 left-out=0 refused=0",
    "shipped-notices: windows-msi go: components=24 files-copied=29 fallbacks=0 left-out=0 refused=0",
    "shipped-notices: windows-msi nuget: components=7 files-copied=11 fallbacks=1 left-out=0 refused=0",
    "shipped-notices: windows-msi bundled: components=2 files-copied=6 fallbacks=0 left-out=0 refused=0",
    "shipped-notices: windows-msi toolchain: components=2 files-copied=2 fallbacks=0 left-out=0 refused=0",
    "shipped-notices: not shipped nuget Microsoft.Windows.SDK.BuildTools@10.0.22621.756:"
    " no asset of it reaches the published output",
    "shipped-notices: nuget runtime packs from FaunaApp.deps.json (.NETCoreApp?Version?v10.0/win-arm64): 2",
    "shipped-notices: nuget app host Microsoft.NETCore.App.Host.win-arm64@10.0.8 (the .NET SDK's own copy,"
    " with the id and files of Microsoft.NETCore.App.Runtime.win-arm64 10.0.8)",
    "shipped-notices: no text nuget Microsoft.Windows.SDK.NET.Ref@10.0.26100.57:"
    " no licence file of its own and no canonical text for its licence",
    "shipped-notices: windows-msi OK — 763 components, 475 distinct texts",
]


def test_the_msi_notices_check_passes_a_complete_closure():
    assert msi_notices_faults(_GOOD_VERDICTS, "win-arm64") == []
    # the app host the restore downloaded passes as well
    downloaded = [ln.replace(ln.partition(" (")[2], "downloaded by the restore)") if "app host" in ln else ln for ln in _GOOD_VERDICTS]
    assert msi_notices_faults(downloaded, "win-arm64") == []


def test_the_msi_notices_check_counts_the_manifests_bundled_components():
    """The bundled verdict the fixture requires is the manifest's: libunwind.dll
    and the mingw-w64 runtime, each with every licence file it names copied."""
    from helpers.windows_installer import MSI_BUNDLED_COMPONENTS, MSI_BUNDLED_FILES

    comps = _manifest()["artifact"]["windows-msi"]["bundled"]["components"]
    assert sorted(c["name"] for c in comps) == ["libunwind", "mingw-w64 runtime"]
    assert MSI_BUNDLED_COMPONENTS == len(comps)
    assert MSI_BUNDLED_FILES == sum(len(c["files"]) for c in comps)


@pytest.mark.parametrize("drop,why", [
    ("runtime packs", "no runtime pack read"),
    ("app host", "app host for win-arm64"),
    ("bundled", "libunwind.dll and the mingw-w64 runtime are not in the notices"),
    ("OK —", "no OK verdict"),
])
def test_the_msi_notices_check_names_each_missing_part(drop, why):
    lines = [ln for ln in _GOOD_VERDICTS if drop not in ln]
    faults = msi_notices_faults(lines, "win-arm64")
    assert len(faults) == 1 and why in faults[0], faults
    # another runtime's publish is not this MSI's
    assert any("no runtime pack read" in f for f in msi_notices_faults(_GOOD_VERDICTS, "win-x64"))
    # a deps.json naming no runtime pack: a framework-dependent publish
    zero = [ln.replace("): 2", "): 0") for ln in _GOOD_VERDICTS]
    assert any("no runtime pack read" in f for f in msi_notices_faults(zero, "win-arm64"))


def test_the_fixture_msi_build_checks_the_notices_verdicts():
    src = (Path(__file__).resolve().parent / "test_installer.py").read_text(encoding="utf-8")
    body = src[src.index("def _build_msi("):src.index("\ndef ", src.index("def _build_msi(") + 1)]
    assert re.search(r'notices_out = run_build_step\(\s*\["just", "windows-msi-notices"', body)
    assert 'msi_notices_faults(notices_out, "win-arm64")' in body


def test_the_fixture_msi_build_runs_the_recipe_and_hands_wix_the_directory():
    src = (Path(__file__).resolve().parent / "test_installer.py").read_text(encoding="utf-8")
    body = src[src.index("def _build_msi("):src.index("\ndef ", src.index("def _build_msi(") + 1)]
    stage = body.index('"windows-msi-notices"')
    assert stage < body.index('"wix", "build"'), "the fixture stages the licence files after the WiX build"
    assert re.search(r'"NoticesDir=\{notices_dir\}\\\\"', body), "the fixture's wix build passes no NoticesDir"


def test_the_installer_readme_build_lists_the_notices_directory():
    """The hand build the README documents passes every variable the release does."""
    text = (_INSTALLER / "README.md").read_text(encoding="utf-8")
    assert "windows-msi-notices" in text and "NoticesDir=" in text

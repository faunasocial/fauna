"""tier_1: each binary channel's packaging source stages the three licence files.

shipped-notices.md § Per channel: every artifact Fauna ships in binary form
carries LICENSE-MIT, LICENSE-APACHE (the repository's files byte for byte) and
the artifact's own THIRD-PARTY-NOTICES, where its row of the table says. This
module pins the terminal archive's assembly and every Linux desktop packaging's
file list — the Debian package (with its `copyright` file), the Flatpak and Snap
manifests, `install.sh` — and the recipes that write the notices file before any
packaging copies it. The release assets are pinned by the publish tooling's
workflow-shape test; the AppImage's AppDir, which
has no recipe, by the tier_3 installer suite that stages it.

The scripts run here over stand-in binaries and a stand-in notices file: what
is tested is where the packaging puts the files, never the generated text (a
real notices file is dependency-authored text no session or test reads).
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

pytestmark = pytest.mark.tier_1

REPO = Path(__file__).resolve().parent.parent
ARCHIVE = REPO / "apps" / "fauna-tui" / "packaging" / "assemble-archive.sh"
LINUX = REPO / "apps" / "fauna-linux"
DOC = "share/doc/fauna-desktop"
NOTICES_PATH = "build/shipped-notices/linux-desktop/THIRD-PARTY-NOTICES"
FILES = ("LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES")


def _bins(d: Path, ext: str = "") -> Path:
    d.mkdir(parents=True, exist_ok=True)
    for name in ("fauna-tui", "fauna-sync-agent", "fauna-cli"):
        (d / f"{name}{ext}").write_text("#!/bin/sh\n")
    return d


def _notices(tmp: Path) -> Path:
    p = tmp / "notices.txt"
    p.write_text("stand-in notices\n")
    return p


def _assemble(tmp: Path, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", str(ARCHIVE), "--out", str(tmp / "out"), "--suffix", "t", "--no-tar", *extra],
        capture_output=True, text=True, timeout=60,
    )


def _same_as_repo(staged: Path, name: str) -> bool:
    return staged.read_bytes() == (REPO / name).read_bytes()


# --- the terminal archive -------------------------------------------------------


def test_the_archive_root_holds_both_licences_and_the_notices(tmp_path):
    rc = _assemble(tmp_path, "--bin-dir", str(_bins(tmp_path / "bin")), "--notices", str(_notices(tmp_path)))
    assert rc.returncode == 0, rc.stderr
    root = tmp_path / "out" / "fauna-tui-t"
    assert _same_as_repo(root / "LICENSE-MIT", "LICENSE-MIT")
    assert _same_as_repo(root / "LICENSE-APACHE", "LICENSE-APACHE")
    assert (root / "THIRD-PARTY-NOTICES").read_text() == "stand-in notices\n"


def test_the_windows_archive_names_them_txt(tmp_path):
    rc = _assemble(tmp_path, "--bin-dir", str(_bins(tmp_path / "bin", ".exe")), "--exe",
                   "--notices", str(_notices(tmp_path)))
    assert rc.returncode == 0, rc.stderr
    root = tmp_path / "out" / "fauna-tui-t"
    for name in FILES:
        assert (root / f"{name}.txt").is_file(), f"{name}.txt missing from the Windows archive"
        assert not (root / name).exists()
    assert _same_as_repo(root / "LICENSE-MIT.txt", "LICENSE-MIT")


def test_an_archive_without_its_notices_is_refused(tmp_path):
    bins = str(_bins(tmp_path / "bin"))
    assert _assemble(tmp_path, "--bin-dir", bins).returncode != 0
    rc = _assemble(tmp_path, "--bin-dir", bins, "--notices", str(tmp_path / "absent.txt"))
    assert rc.returncode != 0
    assert not (tmp_path / "out" / "fauna-tui-t").exists(), "a refused archive was staged anyway"


# --- the Linux desktop packagings -----------------------------------------------


# install.sh is the Linux desktop's own installer and uses GNU `install -D`,
# which the BSD `install` on macOS lacks: it runs where it ships.
@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="install.sh runs on Linux only (GNU install -D)")
def test_install_sh_installs_them_and_its_uninstall_removes_them(tmp_path):
    target = tmp_path / "target" / "release"
    target.mkdir(parents=True)
    for name in ("fauna-desktop", "fauna-sync-agent"):
        (target / name).write_text("#!/bin/sh\n")
    prefix = tmp_path / "prefix"
    env = {
        "PATH": os.environ["PATH"], "HOME": str(tmp_path / "home"), "PREFIX": str(prefix),
        "CARGO_TARGET_DIR": str(tmp_path / "target"), "SHIPPED_NOTICES": str(_notices(tmp_path)),
        "XDG_CONFIG_HOME": str(tmp_path / "xdg"), "XDG_DATA_HOME": str(tmp_path / "data"),
    }
    sh = str(LINUX / "install.sh")
    rc = subprocess.run(["bash", sh], capture_output=True, text=True, timeout=60, env=env)
    assert rc.returncode == 0, rc.stdout + rc.stderr
    doc = prefix / DOC
    assert _same_as_repo(doc / "LICENSE-MIT", "LICENSE-MIT")
    assert _same_as_repo(doc / "LICENSE-APACHE", "LICENSE-APACHE")
    assert (doc / "THIRD-PARTY-NOTICES").read_text() == "stand-in notices\n"
    rc = subprocess.run(["bash", sh, "--uninstall"], capture_output=True, text=True, timeout=60, env=env)
    assert rc.returncode == 0, rc.stdout + rc.stderr
    assert not doc.exists(), "the uninstall left the documentation directory"


def test_install_sh_refuses_without_the_notices_file(tmp_path):
    target = tmp_path / "target" / "release"
    target.mkdir(parents=True)
    for name in ("fauna-desktop", "fauna-sync-agent"):
        (target / name).write_text("#!/bin/sh\n")
    env = {"PATH": os.environ["PATH"], "HOME": str(tmp_path), "PREFIX": str(tmp_path / "prefix"),
           "CARGO_TARGET_DIR": str(tmp_path / "target"), "SHIPPED_NOTICES": str(tmp_path / "absent")}
    rc = subprocess.run(["bash", str(LINUX / "install.sh")], capture_output=True, text=True, timeout=60, env=env)
    assert rc.returncode != 0
    assert not (tmp_path / "prefix" / "bin" / "fauna-desktop").exists()


def _install_lines(text: str) -> dict[str, str]:
    """`install -Dm644 <src> <dst>` lines: destination → source."""
    return {dst: src for src, dst in re.findall(r"install -Dm644 (\S+) (\S+)", text)}


@pytest.mark.parametrize("name,doc_dir", [
    ("debian/rules", f"debian/fauna-desktop/usr/{DOC}"),
    ("snap/snapcraft.yaml", f"$CRAFT_PART_INSTALL/{DOC}"),
])
def test_the_packaging_installs_them_into_its_documentation_directory(name, doc_dir):
    lines = _install_lines((LINUX / "packaging" / name).read_text(encoding="utf-8"))
    assert lines.get(f"{doc_dir}/LICENSE-MIT") == "LICENSE-MIT"
    assert lines.get(f"{doc_dir}/LICENSE-APACHE") == "LICENSE-APACHE"
    assert lines.get(f"{doc_dir}/THIRD-PARTY-NOTICES") == NOTICES_PATH


def test_the_flatpak_installs_them_into_its_documentation_directory():
    manifest = yaml.safe_load((LINUX / "packaging" / "flatpak" / "social.fauna.fauna.yml").read_text(encoding="utf-8"))
    (module,) = [m for m in manifest["modules"] if m["name"] == "fauna-linux"]
    lines = _install_lines("\n".join(module["build-commands"]))
    doc = "/app/share/doc/fauna-desktop"
    assert lines.get(f"{doc}/LICENSE-MIT") == "LICENSE-MIT"
    assert lines.get(f"{doc}/LICENSE-APACHE") == "LICENSE-APACHE"
    assert lines.get(f"{doc}/THIRD-PARTY-NOTICES") == NOTICES_PATH
    # The generator runs outside the sandbox: the source is the repository dir.
    assert [s["type"] for s in module["sources"]] == ["dir"]


def test_the_debian_copyright_file_states_the_same_licence_facts():
    text = (LINUX / "packaging" / "debian" / "copyright").read_text(encoding="utf-8")
    assert text.startswith("Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n")
    paras = [p for p in text.split("\n\n") if p.strip()]
    files = next(p for p in paras if p.startswith("Files: *"))
    assert "License: MIT or Apache-2.0" in files
    mit_line = (REPO / "LICENSE-MIT").read_text().splitlines()[2]
    assert f"Copyright: {mit_line.removeprefix('Copyright (c) ')}" in files
    mit = next(p for p in paras if p.startswith("License: MIT\n"))
    stanza = [ln[1:] if ln != " ." else "" for ln in mit.splitlines()[1:]]
    notice = (REPO / "LICENSE-MIT").read_text().split(mit_line, 1)[1].strip().splitlines()
    assert stanza == notice, "debian/copyright's MIT text differs from LICENSE-MIT's"
    assert any(p.startswith("License: Apache-2.0\n") for p in paras)
    assert f"/usr/{DOC}/THIRD-PARTY-NOTICES" in text


def test_every_linux_desktop_recipe_writes_the_notices_file_first():
    just = shutil.which("just")
    if not just:
        pytest.skip("no just on PATH")
    for recipe in ("linux-release", "linux-deb", "linux-flatpak", "linux-flatpak-install", "linux-flatpak-bundle"):
        out = subprocess.run([just, "--show", recipe], cwd=REPO, capture_output=True, text=True, timeout=30)
        if out.returncode != 0 and recipe != "linux-release":
            continue  # a packaging recipe the public tree's justfile prunes
        assert out.returncode == 0, out.stderr
        header = next(ln for ln in out.stdout.splitlines() if ln.startswith(recipe))
        assert "linux-desktop-notices" in header.split(":", 1)[1], f"{recipe} does not depend on linux-desktop-notices"
    out = subprocess.run([just, "--show", "linux-desktop-notices"], cwd=REPO, capture_output=True, text=True, timeout=30)
    assert f"shipped-notices.py linux-desktop {NOTICES_PATH}" in out.stdout


def test_the_linux_desktop_notices_are_resolved_for_the_built_triple():
    # The file covers the binary the packaging ships: `linux-release` hands
    # its target down, and a native packaging takes the host's triple.
    body = _just_show("linux-desktop-notices")
    assert f'shipped-notices.py linux-desktop {NOTICES_PATH} --target "$triple"' in body
    assert "rustc -vV" in body
    header = next(ln for ln in _just_show("linux-release").splitlines() if ln.startswith("linux-release"))
    assert "(linux-desktop-notices target)" in header


def test_the_debian_package_keeps_the_licence_files_uncompressed():
    # debhelper gzips doc files over 4 KiB; the header names the texts
    # "beside this file" by their plain names, as every other packaging has them.
    rules = (LINUX / "packaging" / "debian" / "rules").read_text(encoding="utf-8")
    m = re.search(r"^override_dh_compress:\n((?:\t.*\n)+)", rules, re.M)
    assert m, "debian/rules has no dh_compress override"
    cmd = [ln.strip() for ln in m.group(1).splitlines() if not ln.strip().startswith("#")]
    assert cmd == ["dh_compress -X LICENSE -X THIRD-PARTY-NOTICES"]


# --- the Apple channels ---------------------------------------------------------
#
# shipped-notices.md § Per channel, the macOS `.app`, macOS `.pkg` and iOS rows.
# Each is staged by its channel's own script through `just shipped-notices-stage`
# (the licence pair plus the generator for the named artifact), after the
# bundle is built and before it is signed — a signature seals the bundle's
# resources, so a file added later would break it. The built bundles are read
# by the tier_3 `.pkg` suite (tests/e2e-unified/tests/platform/macos/test_installer.py).

MACOS = REPO / "installer" / "macos"


def _just_show(recipe: str) -> str:
    just = shutil.which("just")
    if not just:
        pytest.skip("no just on PATH")
    out = subprocess.run([just, "--show", recipe], cwd=REPO, capture_output=True, text=True, timeout=30)
    assert out.returncode == 0, out.stderr
    return out.stdout


def _in_order(text: str, *needles: str) -> None:
    at = -1
    for k, n in enumerate(needles):
        i = text.find(n, at + 1)
        assert i > at, f"{n!r} missing or out of order (after {needles[:k]})"
        at = i


def test_the_stage_recipe_writes_the_licence_pair_and_the_artifacts_notices():
    body = _just_show("shipped-notices-stage")
    _in_order(body, 'just shipped-licences "$dir"', 'scripts/shipped-notices.py "$artifact" "$dir/THIRD-PARTY-NOTICES"')


def test_the_macos_app_stages_them_in_its_resources_after_the_build_and_before_signing():
    body = _just_show("_mac-app-impl")
    _in_order(
        body,
        "xcodebuild",
        'just shipped-notices-stage macos-app "$APP/Contents/Resources"',
        "Contents/Resources/THIRD-PARTY-NOTICES",  # the assembled-bundle check
        "sign-app-bundle.sh",
    )


def test_the_pkg_installs_the_tools_set_from_its_own_component():
    build = (MACOS / "build.sh").read_text()
    _in_order(
        build,
        'just mail-bridge-build "$PROFILE"',  # the Go binary the generator reads
        'just shipped-notices-stage macos-pkg-tools "$BUILD_DIR/payloads/doc"',
        "--identifier social.fauna.doc",
        "--install-location /usr/local/share/doc/fauna",
        '"$BUILD_DIR/components/fauna-doc.pkg"',
        "productbuild",
    )
    assert "__SIZE_DOC__/$size_doc" in build
    assert "rm -rf /usr/local/share/doc/fauna" in (MACOS / "fauna-uninstall").read_text()


def test_every_tool_choice_and_not_the_app_choice_installs_the_doc_component():
    import xml.etree.ElementTree as ET

    root = ET.parse(MACOS / "distribution.xml").getroot()
    holds = {c.get("id"): {r.get("id") for r in c.iter("pkg-ref")} for c in root.iter("choice")}
    for cid in ("social.fauna.nest", "social.fauna.sync", "social.fauna.tui", "social.fauna.bridge"):
        assert "social.fauna.doc" in holds[cid], f"{cid} does not install the doc component"
    assert "social.fauna.doc" not in holds["social.fauna.app"]  # the app carries its own set
    defs = [r for r in root.findall("pkg-ref") if r.get("id") == "social.fauna.doc"]
    assert len(defs) == 1 and (defs[0].text or "").strip() == "fauna-doc.pkg"
    assert defs[0].get("installKBytes") == "__SIZE_DOC__"


def test_the_ios_app_is_staged_from_the_built_tree_before_the_signer():
    script = (REPO / "scripts" / "ios-store-upload.sh").read_text()
    build_half = script[script.index("build_half() {"):script.index("sign_half() {")]
    _in_order(
        build_half,
        "xcodebuild archive",
        'ditto "$app" "$UNSIGNED_APP"',
        'just --justfile "$CLONE/justfile" --working-directory "$CLONE"',
        'shipped-notices-stage ios-app "$UNSIGNED_APP"',
    )

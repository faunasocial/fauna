"""Tests for the downloads table's release check (installer/check-downloads.py).

The check runs in the release workflow over the files it is about to publish,
so it must red in both directions — a cell for a file the release lacks, a
published file the table neither links nor skips — and stay green on the
tree's own table against the files the v0.1.5 release actually carried.
"""

import copy
import importlib.util
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("check_downloads",
                                               ROOT / "installer" / "check-downloads.py")
mod = importlib.util.module_from_spec(_spec)
sys.modules["check_downloads"] = mod
_spec.loader.exec_module(mod)

MANIFEST = json.loads((ROOT / "installer" / "downloads.json").read_text(encoding="utf-8"))

# The files release.yml published into v0.1.5 (2026-10-22), read off the release.
_BINARIES = [f"{name}-{target}"
             for name in ("fauna-nest", "fauna-front-door", "fauna-push-relay", "fauna-cors-proxy")
             for target in ("aarch64-darwin", "aarch64-linux", "x86_64-darwin", "x86_64-linux")]
V015 = _BINARIES + [
    "fauna-linux-x86_64-linux",
    "Fauna-Setup-arm64.msi", "Fauna-Setup-x64.msi",
    "fauna-tui-aarch64-linux.tar.gz", "fauna-tui-x86_64-linux.tar.gz",
    "fauna-tui-arm64-windows.zip", "fauna-tui-x64-windows.zip",
    "SHA256SUMS",
]
V015 = V015 + [f"{name}.bundle" for name in V015]

# What release.yml publishes from the next release on: v0.1.5's files plus the
# linux desktop app for arm64, built natively since 2026-10-25.
NEXT = V015 + ["fauna-linux-aarch64-linux", "fauna-linux-aarch64-linux.bundle"]


def test_the_tree_table_agrees_with_the_next_release():
    assert mod.check(MANIFEST, "release.yml", NEXT) == []


def test_the_v015_file_set_lacks_the_arm64_desktop_app():
    errors = mod.check(MANIFEST, "release.yml", V015)
    assert len(errors) == 1 and "fauna-linux-aarch64-linux" in errors[0]


def test_a_cell_for_a_file_the_release_lacks_reds():
    files = [f for f in NEXT if f != "Fauna-Setup-arm64.msi"]
    errors = mod.check(MANIFEST, "release.yml", files)
    assert len(errors) == 1 and "Fauna-Setup-arm64.msi" in errors[0]
    assert "not publishing it" in errors[0]


def test_a_new_published_file_the_table_does_not_name_reds():
    errors = mod.check(MANIFEST, "release.yml", NEXT + ["fauna-linux-riscv64-linux"])
    assert len(errors) == 1 and "fauna-linux-riscv64-linux" in errors[0]
    assert "neither links it nor lists it" in errors[0]


# What release-macos.yml publishes: the signed .dmg and the two darwin tui
# archives, each with a `.sha256` beside it (the workflow's `build/`).
MACOS = ["Fauna-0.1.6.dmg", "Fauna-0.1.6.dmg.sha256",
         "fauna-tui-aarch64-darwin.tar.gz", "fauna-tui-aarch64-darwin.tar.gz.sha256",
         "fauna-tui-x86_64-darwin.tar.gz", "fauna-tui-x86_64-darwin.tar.gz.sha256"]


def test_another_workflows_cells_are_not_this_workflows_business():
    # release.yml's check ignores the macOS cells the tree table now carries.
    assert any(c["workflow"] == "release-macos.yml"
               for row in MANIFEST["rows"] for cells in row["cells"].values() for c in cells)
    assert mod.check(MANIFEST, "release.yml", NEXT) == []
    # ...and release-macos.yml's ignores release.yml's.
    assert mod.check(MANIFEST, "release-macos.yml", MACOS, "0.1.6") == []


def test_the_macos_release_agrees_with_the_tree_table():
    assert mod.check(MANIFEST, "release-macos.yml", MACOS, "0.1.6") == []


def test_a_versioned_cell_is_matched_at_the_version_being_published():
    errors = mod.check(MANIFEST, "release-macos.yml", MACOS, "0.1.7")
    assert any("Fauna-0.1.7.dmg" in e and "not publishing it" in e for e in errors)
    assert any("Fauna-0.1.6.dmg" in e and "neither links it" in e for e in errors)


def test_a_versioned_cell_without_a_version_reds():
    errors = mod.check(MANIFEST, "release-macos.yml", MACOS)
    assert any("{version}" in e and "no version" in e for e in errors)


def test_the_macos_release_refuses_a_missing_or_unnamed_file():
    errors = mod.check(MANIFEST, "release-macos.yml",
                       [f for f in MACOS if "x86_64" not in f] + ["Fauna-0.1.6.pkg"], "0.1.6")
    assert any("fauna-tui-x86_64-darwin.tar.gz" in e for e in errors)
    assert any("Fauna-0.1.6.pkg" in e for e in errors)


def test_a_cell_in_an_unknown_column_reds():
    m = copy.deepcopy(MANIFEST)
    m["rows"][2]["cells"]["beos"] = m["rows"][2]["cells"].pop("linux")
    errors = mod.check(m, "release.yml", NEXT)
    assert len(errors) == 1 and "'beos'" in errors[0]


def test_a_cell_with_both_or_neither_link_reds():
    m = copy.deepcopy(MANIFEST)
    m["rows"][2]["cells"]["linux"][0]["file"] = "x"
    errors = mod.check(m, "release.yml", NEXT)
    assert any("exactly one of" in e for e in errors)


def test_main_exits_nonzero_and_names_each_error(tmp_path, capsys):
    files = tmp_path / "all-artifacts"
    files.mkdir()
    for name in NEXT:
        if name != "Fauna-Setup-x64.msi":
            (files / name).write_text("x")
    (files / "surprise.bin").write_text("x")
    manifest = tmp_path / "downloads.json"
    manifest.write_text(json.dumps(MANIFEST))
    assert mod.main(["check-downloads.py", str(manifest), "release.yml", str(files)]) == 1
    out = capsys.readouterr().out
    assert "surprise.bin" in out and "Fauna-Setup-x64.msi" in out and "2 error(s)" in out
    (files / "surprise.bin").unlink()
    (files / "Fauna-Setup-x64.msi").write_text("x")
    assert mod.main(["check-downloads.py", str(manifest), "release.yml", str(files)]) == 0

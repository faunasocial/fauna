"""The Flatpak build packages, it never compiles (`installers/linux-desktop.md`
§ 3 Flatpak → *The Flatpak build packages, it never compiles*).

The shipped manifest's one module installs what `scripts/linux-flatpak-stage.py`
staged — the two binaries `just linux-release <target>` built, the desktop
entry, the metainfo, the icon, the licence texts and the notices — and nothing
else: no `build-args` (so the build sandbox has no network), no toolchain
install, no compile. These pins are the regression guard for that decision:
a manifest that grows a `cargo`, `curl` or `rustup` line again, or a network
grant for its build, reds here in every default loop, long before anyone builds
a bundle.

The staging script is the payload's single definition: the manifest's install
lines read exactly the names it stages, and the real-session seam test stages
its automation-feature binaries through the same script into the same manifest.
"""

import importlib.util
import os
import shutil
import stat
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

from common import get_repo_root

pytestmark = [pytest.mark.tier_1]

REPO = Path(get_repo_root())
MANIFEST = REPO / "apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml"
STAGE_SCRIPT = REPO / "scripts/linux-flatpak-stage.py"
FLATPAK_RECIPES = ("linux-flatpak", "linux-flatpak-install", "linux-flatpak-bundle")
# Words that mean the build fetches or compiles something.
FORBIDDEN = ("cargo", "rustup", "rustc", "curl", "wget", "sh.rustup.rs")


def _module():
    manifest = yaml.safe_load(MANIFEST.read_text(encoding="utf-8"))
    (module,) = manifest["modules"]
    return manifest, module


def _stage():
    spec = importlib.util.spec_from_file_location("linux_flatpak_stage_row_6430", STAGE_SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _install_sources(commands):
    """The source operand of every `install … <src> <dest>` line."""
    out = []
    for line in commands:
        words = line.split()
        if words and words[0] == "install":
            out.append(words[-2])
    return out


def test_the_shipped_manifest_carries_no_build_network_toolchain_or_compile():
    manifest, module = _module()
    assert "build-options" not in module, (
        "the Flatpak module carries build-options again — its build packages "
        "staged binaries and needs no environment, path or network grant "
        "(linux-desktop.md § 3)"
    )
    assert "build-options" not in manifest
    def keys(node):
        if isinstance(node, dict):
            for k, v in node.items():
                yield k
                yield from keys(v)
        elif isinstance(node, list):
            for v in node:
                yield from keys(v)

    assert "build-args" not in set(keys(manifest)), "a build-args grant gives the build sandbox the network"
    for line in module["build-commands"]:
        for word in FORBIDDEN:
            assert word not in line, (
                f"build command {line!r} names {word!r}: the Flatpak build "
                f"never compiles or fetches — the binaries come from "
                f"`just linux-release` (linux-desktop.md § 3)"
            )
    assert "sdk-extensions" not in manifest


def test_the_module_installs_exactly_what_the_staging_script_stages():
    _, module = _module()
    (source,) = module["sources"]
    assert source["type"] == "dir"
    staged_at = (MANIFEST.parent / source["path"]).resolve()
    assert staged_at == (REPO / _stage().DEFAULT_OUT).resolve(), (
        "the manifest's dir source must name the directory the staging script fills"
    )
    assert sorted(_install_sources(module["build-commands"])) == sorted(_stage().PAYLOAD), (
        "the manifest's install lines and the staging script's payload drifted "
        "apart — the script is the payload's one definition"
    )


def test_the_staging_script_stages_the_payload_flat(tmp_path):
    bins = tmp_path / "release"
    bins.mkdir()
    for name in ("fauna-desktop", "fauna-sync-agent"):
        (bins / name).write_bytes(b"\x7fELF probe row 6430")
    notices = tmp_path / "THIRD-PARTY-NOTICES"
    notices.write_text("notices\n")
    out = tmp_path / "staging"
    (out).mkdir()
    (out / "stale-leftover").write_text("x")

    subprocess.run(
        [sys.executable, str(STAGE_SCRIPT), "--bin-dir", str(bins),
         "--notices", str(notices), "--out", str(out)],
        cwd=REPO, check=True, capture_output=True, text=True,
    )

    assert sorted(os.listdir(out)) == sorted(_stage().PAYLOAD), "a stale file survived, or one is missing"
    for name in ("fauna-desktop", "fauna-sync-agent"):
        assert (out / name).stat().st_mode & stat.S_IXUSR
    assert (out / "THIRD-PARTY-NOTICES").read_text() == "notices\n"
    assert (out / "LICENSE-MIT").read_bytes() == (REPO / "LICENSE-MIT").read_bytes()


def test_the_staging_script_refuses_a_missing_binary(tmp_path):
    bins = tmp_path / "release"
    bins.mkdir()
    (bins / "fauna-desktop").write_bytes(b"probe")
    notices = tmp_path / "THIRD-PARTY-NOTICES"
    notices.write_text("n\n")
    result = subprocess.run(
        [sys.executable, str(STAGE_SCRIPT), "--bin-dir", str(bins),
         "--notices", str(notices), "--out", str(tmp_path / "staging")],
        cwd=REPO, capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "fauna-sync-agent" in result.stderr


def _just_show(recipe):
    just = shutil.which("just")
    if not just:
        pytest.skip("no just on PATH")
    out = subprocess.run([just, "--show", recipe], cwd=REPO, capture_output=True, text=True, timeout=30)
    if out.returncode != 0:
        pytest.skip(f"{recipe} is pruned from this tree's justfile")
    return out.stdout


@pytest.mark.parametrize("recipe", FLATPAK_RECIPES)
def test_every_flatpak_recipe_takes_the_target_and_packages_the_release_build(recipe):
    text = _just_show(recipe)
    header = next(ln for ln in text.splitlines() if ln.startswith(recipe))
    assert header.startswith(f'{recipe} target=""'), header
    deps = header.split(":", 1)[1]
    assert "(linux-release target)" in deps
    assert "(linux-flatpak-stage target)" in deps
    assert deps.index("(linux-release target)") < deps.index("(linux-flatpak-stage target)")
    body = text.split("\n", text.splitlines().index(header) + 1)[-1]
    assert "cargo build" not in body and "rustup" not in body
    assert "flatpak_builder" in body.replace(" ", "")
    builder = subprocess.run(
        [shutil.which("just"), "--evaluate", "flatpak_builder"],
        cwd=REPO, capture_output=True, text=True, timeout=30, check=True,
    ).stdout
    assert builder.startswith("flatpak-builder ")
    assert "--default-branch=stable" in builder
    assert "--disable-rofiles-fuse" in builder


def test_the_bundle_is_named_by_suffix_and_names_flathub_as_its_runtime_repo():
    body = _just_show("linux-flatpak-bundle")
    assert "--runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo" in body
    assert "fauna-desktop-${suffix}.flatpak" in body
    assert 'x86_64-unknown-linux-gnu) suffix="x86_64-linux"' in body
    assert 'aarch64-unknown-linux-gnu) suffix="aarch64-linux"' in body
    assert "social.fauna.fauna stable" in body

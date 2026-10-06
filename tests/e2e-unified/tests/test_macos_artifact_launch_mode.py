"""`launch_mode` is ADDITIVE: absent means the bare binary, exactly as before.

The artifact mode (`--macos-artifact`, `tests/artifact/`,
`apple-e2e-automation.md` § Artifact launch mode) added a second way for the
macOS driver to launch an app. Every apple e2e test in the fleet rides the
first one, so the contract that matters most is the one no artifact test can
check: **a config that says nothing about `launch_mode` still resolves the bare
swift-build executable.**

These are tier_1 on purpose. The artifact suite is opt-in, costs minutes, and
runs rarely — so if the only proof that the default path survived lived there, a
regression would sit on `origin/main` until someone next opted in. This file runs
in every ordinary collection, needs no build, no bundle, and no macOS, and it is
what turns "the change is additive" from a claim in a commit message into an
assertion.
"""

import plistlib
from pathlib import Path

import pytest

from drivers.macos import (
    MacosInProcessDriver,
    bundle_executable,
    read_bundle_id,
)

pytestmark = pytest.mark.tier_1


def _driver() -> MacosInProcessDriver:
    """A driver instance with no launch — only `_executable` is exercised."""
    return MacosInProcessDriver.__new__(MacosInProcessDriver)


def test_a_config_without_launch_mode_resolves_the_bare_executable(tmp_path):
    """The default every apple test depends on."""
    bare = tmp_path / ".build" / "arm64-apple-macosx" / "debug" / "FaunaMacOS"
    bare.parent.mkdir(parents=True)
    bare.write_text("#!/bin/sh\n")

    assert _driver()._executable({"app_path": str(bare)}) == bare


def test_an_explicit_binary_mode_is_the_same_as_saying_nothing(tmp_path):
    bare = tmp_path / "FaunaMacOS"
    bare.write_text("#!/bin/sh\n")

    d = _driver()
    assert (d._executable({"app_path": str(bare)})
            == d._executable({"app_path": str(bare), "launch_mode": "binary"}))


def _fake_bundle(root: Path, *, executable: str = "Fauna",
                 identifier: str = "social.fauna.fauna") -> Path:
    """A minimal, real-shaped `.app` — enough for the plist readers, no signing."""
    bundle = root / "Fauna.app"
    (bundle / "Contents" / "MacOS").mkdir(parents=True)
    (bundle / "Contents" / "MacOS" / executable).write_text("#!/bin/sh\n")
    (bundle / "Contents" / "Info.plist").write_bytes(
        plistlib.dumps({"CFBundleExecutable": executable,
                        "CFBundleIdentifier": identifier})
    )
    return bundle


def test_a_bundle_path_resolves_through_cfbundleexecutable(tmp_path):
    """The regression this replaced.

    The driver's old back-compat bundle branch hardcoded `Contents/MacOS/FaunaMacOS`
    — the SwiftPM *product* name. `just mac-app` copies that product in as
    **`Fauna`**, so the hardcoded path does not exist in any real bundle; the branch
    only survived because nothing called it. Naming the executable something other
    than the product name here is exactly the case that used to break.
    """
    bundle = _fake_bundle(tmp_path, executable="Fauna")
    resolved = _driver()._executable({"app_path": str(bundle),
                                      "launch_mode": "bundle"})
    assert resolved == bundle / "Contents" / "MacOS" / "Fauna"
    assert resolved.exists()
    assert bundle_executable(bundle) == resolved


def test_a_bundle_without_cfbundleexecutable_is_named_as_the_problem(tmp_path):
    """Fail with the cause, not with a FileNotFoundError three frames later."""
    bundle = tmp_path / "Broken.app"
    (bundle / "Contents" / "MacOS").mkdir(parents=True)
    (bundle / "Contents" / "Info.plist").write_bytes(plistlib.dumps({}))

    with pytest.raises(RuntimeError, match="CFBundleExecutable"):
        bundle_executable(bundle)


def test_read_bundle_id_reads_the_plist(tmp_path):
    assert read_bundle_id(_fake_bundle(tmp_path)) == "social.fauna.fauna"
    assert read_bundle_id(tmp_path / "absent.app") == ""


@pytest.mark.parametrize("mode", ["bundle ", "BUNDLE", "app", "dmg", ""])
def test_an_unknown_launch_mode_is_refused(mode):
    """A typo must not silently fall through to the bare-binary default.

    Silently launching the wrong subject is the worst outcome available here: a
    misspelled `--macos-artifact` wiring would produce a fully green artifact
    suite that never touched an artifact. The suite has its own guard against
    that (`test_the_launched_app_is_the_bundle_under_a_per_instance_identity`);
    this is the same rule one layer down, where it costs nothing.
    """
    from drivers.macos import MacosInProcessDriver as D

    d = D.__new__(D)
    d._launch_config = {}
    with pytest.raises(RuntimeError, match="unknown launch_mode"):
        d.launch({"app_path": "/nonexistent", "launch_mode": mode})

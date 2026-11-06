"""tier_1: an upgrade over a pre-fix macOS install drops the sync agent's log redirect.

The incident (found 2026-10-27 on macOS). A 0.1.2 `.pkg`, installed by hand
for a supervised File Provider round on 2026-08-28 and never removed, kept a
per-user sync agent running at login for two months. Its LaunchAgent still
carried the pre-fix `StandardOutPath`/`StandardErrorPath` redirect, launchd
never rotates a redirect, and a 401 retry loop wrote 7 GB. The bound
(observability.md § Persistence & privacy, *The file is bounded by
construction*) holds only for a box that runs a current agent under a current
plist — so the product question is whether the NEXT install over such a box
rewrites the old plist.

It does, through `installer/macos/scripts/common.sh::install_launchagent`
(bytes differ → bootout, rewrite, bootstrap); this file pins that against the
real pre-fix bytes, headlessly (`helpers/postinstall_sandbox.py` says how). The
same harness runs on the shipped `.pkg` scripts in
`platform/macos/test_installer.py::TestDryRun`, and the real sudo install in
`TestFullInstall::test_upgrade_rewrites_a_pre_fix_redirecting_plist`. The
app's own self-install heals the same plist from the other side
(`libs/fauna-client-sync/src/agent_spawner.rs::ensure_launch_agent`, pinned by
its `ensure_writes_then_keeps_then_heals_the_plist`).
"""

import os
import shutil
import sys

import pytest

_HERE = os.path.dirname(__file__)
sys.path.insert(0, os.path.normpath(os.path.join(_HERE, "..")))

from helpers.postinstall_sandbox import Sandbox, SYNC_LABEL, pre_fix_plist  # noqa: E402

pytestmark = [
    pytest.mark.tier_1,
    pytest.mark.skipif(shutil.which("bash") is None, reason="the postinstall is a bash script"),
]

_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_SCRIPTS = os.path.join(_REPO, "installer", "macos", "scripts")


def _scripts():
    with open(os.path.join(_SCRIPTS, "sync", "postinstall")) as f:
        post = f.read()
    with open(os.path.join(_SCRIPTS, "common.sh")) as f:
        common = f.read()
    return post, common


def _fresh_install_bytes(tmp_path):
    box = Sandbox(tmp_path / "fresh")
    r = box.run_postinstall(*_scripts())
    assert r.returncode == 0, f"fresh install failed:\n{r.stdout}\n{r.stderr}"
    return box.plist_text()


def test_an_upgrade_rewrites_the_pre_fix_redirecting_plist(tmp_path):
    fresh = _fresh_install_bytes(tmp_path)

    box = Sandbox(tmp_path / "upgrade")
    old = pre_fix_plist(box.home)
    box.plant(old)
    assert "StandardOutPath" in old  # the fixture IS the pre-fix shape

    r = box.run_postinstall(*_scripts())
    assert r.returncode == 0, f"upgrade failed:\n{r.stdout}\n{r.stderr}"

    after = box.plist_text()
    assert "StandardOutPath" not in after and "StandardErrorPath" not in after, (
        "an upgrade left the pre-fix launchd redirect in place — every box "
        f"installed before the log fix keeps an unrotated log for ever:\n{after}"
    )
    assert after == fresh, "the upgraded plist must be byte-identical to a fresh install's"
    calls = box.calls()
    assert f"launchctl bootout gui/4242/{SYNC_LABEL}" in calls, (
        f"the running pre-fix agent must be booted out so the new plist takes effect: {calls}"
    )
    assert any(c.startswith("launchctl bootstrap gui/4242 ") for c in calls), calls
    assert not any("kickstart" in c for c in calls), (
        f"a kickstart alone keeps launchd on the OLD plist: {calls}"
    )


def test_an_upgrade_over_a_current_plist_only_restarts_the_agent(tmp_path):
    # The other arm of the same decision: unchanged bytes are left alone (the
    # sudo leg `test_upgrade_preserves_plists` asserts it by mtime on a real box).
    fresh = _fresh_install_bytes(tmp_path)
    box = Sandbox(tmp_path / "current")
    box.plant(fresh)
    r = box.run_postinstall(*_scripts())
    assert r.returncode == 0, f"upgrade failed:\n{r.stdout}\n{r.stderr}"
    assert box.plist_text() == fresh
    calls = box.calls()
    assert f"launchctl kickstart -k gui/4242/{SYNC_LABEL}" in calls, calls
    assert not any("bootout" in c for c in calls), calls

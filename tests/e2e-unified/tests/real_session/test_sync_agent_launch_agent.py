"""The shared launchd writer against the REAL launchd — the macOS twin of
`test_sync_agent_unit_flap.py`.

`libs/fauna-client-sync/src/agent_spawner.rs::ensure_launch_agent` is the one
writer of the per-user `social.fauna.sync-agent` LaunchAgent for both apps that
self-install it — the macOS desktop app (over UniFFI) and fauna-tui on a mac
installed from the archive alone (`sync-agent.md` § Packaging + lifecycle,
*A tui-only mac*). Every e2e launch skips it by construction (both spawners
direct-spawn a child under e2e, so no test touches the machine-global job), so
this is the one place the mechanism meets launchd itself: the hand-written plist
is accepted, `bootstrap` loads it into `gui/<uid>`, a heal (a different target)
boots the loaded job out so the reloaded definition follows the file, and
`bootout` removes it.

**What runs.** The Rust test
`agent_spawner::tests::real_launchd_loads_heals_and_boots_out_the_probe_job`
(`#[ignore]`d in an ordinary `cargo test`), so the bytes launchd reads are the
production renderer's own — never a Python copy of them. It drives a THROWAWAY
label (`social.fauna.sync-agent-probe-<pid>`) whose plist and stub agent
(`exec sleep 300`) live in this test's own temp dir, never the real job or
`~/Library/LaunchAgents`; it boots the probe out and removes its plist on every
exit path. The two other halves of the coverage:
  - the emitted string, the resolution order and the heal/keep decisions are
    pinned by the ordinary unit tests beside it;
  - the install-channel half (that `install.sh --uninstall` boots the job out
    when, and only when, the plist targets the prefix's own agent) is
    `tests/artifact/test_tui_installed_product.py`.

Borrow and give back (the category contract, conftest.py): nothing of the real
session's is touched, and the probe job is removed whatever the outcome.
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

from helpers.helper_bins import helper_bin

# tier_3: a real, unmocked integration against a real OS subsystem (the user's
# launchd), with no fauna app, driver or image involved — the flap twin's tier.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_session,
    pytest.mark.macos,
    pytest.mark.timeout(1200),
    pytest.mark.skipif(sys.platform != "darwin", reason="launchd is macOS-only"),
]

_REPO = Path(__file__).resolve().parents[4]
_RUST_TEST = "agent_spawner::tests::real_launchd_loads_heals_and_boots_out_the_probe_job"


def test_the_shared_writer_loads_heals_and_boots_out_a_real_launch_agent(tmp_path):
    env = dict(os.environ, FAUNA_REAL_LAUNCHD_PROBE_DIR=str(tmp_path))
    # The crate's lib unit-test binary is compiled at COLLECTION by the
    # helper-binary prebuild (`helpers/helper_bins.py`, convention 9's 2026-10-30
    # ruling), never in this test's clock; the round trip through launchd is
    # the subject. A miss is a loud `HelperBinNotPrebuilt`, not a build.
    test_binary = helper_bin("fauna-client-sync-lib-tests")
    proc = subprocess.run(
        [str(test_binary), _RUST_TEST, "--ignored", "--exact"],
        # cargo runs a package's tests from the package dir.
        cwd=_REPO / "libs" / "fauna-client-sync",
        env=env,
        capture_output=True,
        text=True,
    )
    output = proc.stdout + proc.stderr
    assert proc.returncode == 0, f"the real-launchd probe failed:\n{output[-6000:]}"
    # A filter that matched nothing also exits 0 — the witness is the PASS line.
    assert f"test {_RUST_TEST} ... ok" in output, (
        f"the real-launchd probe did not run (filter matched nothing?):\n{output[-4000:]}"
    )
    leftover = subprocess.run(
        ["/bin/launchctl", "list"], capture_output=True, text=True
    ).stdout
    assert "social.fauna.sync-agent-probe-" not in leftover, (
        "a probe job is still loaded in gui/<uid> — the give-back failed"
    )

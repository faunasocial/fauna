"""tier_1: which running sync agent belongs to the installed product.

The journey test (``test_installer.py::TestFullJourneyInstalledApp``) rests on a
box precondition it never stated: **no foreign agent may hold**
``\\\\.\\pipe\\fauna-sync.<SID>``. The app's ``SpawnSyncAgentDetached`` probes
that pipe and, finding it *reachable*, deliberately does **not** spawn its own
agent (``App.xaml.cs`` ``EnsureRunningAsync``) — then ``CapabilityProvisioner``
provisions over whatever is already there.

So a sibling dev checkout's agent, running out of its own build tree as the same
user, silently becomes the agent under test. Two failures follow, and the second
is the dangerous one — note the vocabulary here is deliberately generic: this
file ships, and the publish leak gate rejects internal session vocabulary
(``leak-check.sh``'s ``\\bworktree`` HARD hit).

1. ``test_installed_app_spawns_the_installers_own_sync_agent`` reds with "no
   agent under %ProgramFiles%\\Fauna" — which reads exactly like a product
   defect in the installer→agent chain, and would send the next session hunting
   one that does not exist.
2. The run **writes into the sibling's agent** — pushing a test nest's
   capability into it and binding a test folder into its sync roots.

Measured on Windows 2026-07-17: a sibling dev checkout's agent (started 12:48,
outliving its own session because the agent is spawned detached) held the pipe
while this suite was staged to run. That is the *precise* confusion the journey
docstring says the 2026-07-17 incident hid behind — "a stale build-tree agent
kept serving while the freshly installed one was never provisioned" — so the
suite must diagnose it rather than reproduce it.

Split out as tier_1 for the same reason as ``test_placeholder_helper``: the
predicate needs no elevation, no MSI and no exclusive box, so it is provable in
milliseconds by any session, while the journey it guards needs all three. A
guard whose own logic is only exercised inside a 20-minute elevated run is a
guard nobody can trust.

What this canNOT prove: that the OS pipe enumeration sees a *live* agent's pipe.
That needs a running agent — ``test_per_user_sync_agent.py`` has one.
"""

from __future__ import annotations

import sys

import pytest

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="Windows paths / named pipes"),
    pytest.mark.tier_1,
]

from helpers import windows_sync_agent as wsa  # noqa: E402

_INSTALL_DIR = r"C:\Program Files\Fauna"
_INSTALLED = r"C:\Program Files\Fauna\fauna-sync-agent.exe"
#: A developer's own cargo build output — the shape that actually collided on
#: Windows 2026-07-17. Kept realistic (same `target\<triple>\dist\` tail a real dev
#: build produces) but generic: this file ships publicly, and the publish leak
#: gate hard-fails on the development repo's name as well as on session vocabulary.
_BUILD_TREE = (
    r"C:\src\fauna\target"
    r"\aarch64-pc-windows-msvc\dist\fauna-sync-agent.exe"
)


def test_the_installed_agent_is_not_foreign():
    """The agent the MSI put under %ProgramFiles%\\Fauna is the one under test."""
    assert wsa.foreign_agents([_INSTALLED], _INSTALL_DIR) == []


def test_a_sibling_build_tree_agent_is_foreign():
    """The 2026-07-17 blocker, verbatim: a sibling dev checkout's own agent is a
    sync agent like any other, and by image name alone indistinguishable
    from ours. Only the path separates them."""
    assert wsa.foreign_agents([_BUILD_TREE], _INSTALL_DIR) == [_BUILD_TREE]


def test_foreign_detection_survives_case_and_separator_drift():
    """Win32 hands back paths in whatever case/separator the creator used;
    a case-sensitive compare would call the installed agent foreign and abort
    a perfectly good run."""
    assert wsa.foreign_agents([r"c:\program files\fauna\FAUNA-SYNC-AGENT.EXE"], _INSTALL_DIR) == []
    assert wsa.foreign_agents([r"C:/Program Files/Fauna/fauna-sync-agent.exe"], _INSTALL_DIR) == []


def test_a_quiet_box_has_no_foreign_agents():
    """The happy path must be silent — no agents means nothing to report."""
    assert wsa.foreign_agents([], _INSTALL_DIR) == []


def test_a_sibling_named_like_the_install_dir_is_still_foreign():
    """A prefix compare on the raw string would call `C:\\Program Files\\Fauna-old\\`
    ours. The boundary must be a path component, not a substring.

    Deliberately spelled with the PRE-rename image name: an older product parked
    beside the current one is the most likely real-world impostor, and the guard
    must not care what the intruder's binary is called."""
    impostor = r"C:\Program Files\Fauna-old\fauna-sync.exe"
    assert wsa.foreign_agents([impostor], _INSTALL_DIR) == [impostor]


def test_the_process_scan_matches_only_the_shipped_image_name():
    """The shipped artifact is ``fauna-sync-agent.exe`` and the scan matches only it.

    The pre-A5 name ``fauna-sync.exe`` was removed from the MSI by the compat-remnant
    sweep (no pre-rename install exists), and it was also the unrelated
    ``bins/fauna-sync`` CLI daemon, so matching it would flag a process that is not
    a sync agent. The pipe-name prefix is a separate axis and is unchanged."""
    assert "fauna-sync-agent.exe" in wsa._AGENT_IMAGE_NAMES
    assert "fauna-sync.exe" not in wsa._AGENT_IMAGE_NAMES


def test_mixed_box_reports_only_the_foreign_one():
    """Both running at once: report the intruder, never the installed agent."""
    assert wsa.foreign_agents([_INSTALLED, _BUILD_TREE], _INSTALL_DIR) == [_BUILD_TREE]


def test_reading_the_pipe_filesystem_never_raises():
    """The guard runs before a destructive install; it must never be the thing
    that explodes. Whatever the box's state, this is a bool."""
    assert wsa.sync_pipe_is_served() in (True, False)


def test_the_blocker_diagnosis_names_the_intruder_and_stays_quiet_when_clear():
    """A guard that fires without naming the process is a guard that costs a
    session. The message must carry the path to stop."""
    msg = wsa.describe_blocker([_BUILD_TREE], _INSTALL_DIR)
    assert msg is not None
    assert _BUILD_TREE in msg

    assert wsa.describe_blocker([_INSTALLED], _INSTALL_DIR) is None
    assert wsa.describe_blocker([], _INSTALL_DIR) is None

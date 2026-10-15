"""tier_3 e2e: the windows sync agent's `ws-device` toast sink, over its real pipe.

`common.md` § Push Notifications → *Transports* (windows: the WinRT toast,
attributed to the app) and `windows.md` § Notifications (the agent's half): the
agent has no toast identity of its own — the app names the AUMID its own toasts
post under when it attaches (`AttachApp`'s `notification_identity`), the agent
keeps it across its own restarts, and the health reply's `notification_sink`
is what the app's push control reads (`sync-agent.md` § Local agent health).

What is real: the shipped agent binary on a private pipe and data dir, its IPC
codec, and the notification platform it asks. The identity is registered per
run the way an unpackaged app's toast registration is
(`HKCU\\Software\\Classes\\AppUserModelId\\<aumid>`) and removed afterwards. The
toast itself — posting under the identity and reading it back from the
notification centre — is pinned against the same platform by
`bins/fauna-sync-agent/src/push_arm.rs`'s `toast_tests`.

Process safety: the agent processes are this test's own children
(`running_agent`); no nest, bridge or app is involved.
"""

from __future__ import annotations

import os
import sys

import pytest

from helpers import sync_agent_ipc as ipc
from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until
from helpers.windows_sync_agent import attach_once, request, running_agent
from helpers.windows_toast import registered_identity

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.skipif(sys.platform != "win32", reason="the windows agent's WinRT toast sink"),
]


def _pipe(tag: str) -> str:
    return r"\\.\pipe\fauna-sync-test-toast-{}-{}".format(tag, os.getpid())


def _sink(pipe: str):
    return request(pipe, ipc.get_service_status_request(1))["ServiceStatus"].get(
        "notification_sink"
    )


def _probed_sink(pipe: str):
    """The sink once the agent's boot probe has answered (it runs off the
    runtime after the pipe comes up, so the first replies may say nothing)."""
    seen = []

    def answered():
        seen.append(_sink(pipe))
        return seen[-1] is not None

    wait_until(answered, UI_SETTLE_S, diagnose=lambda: f"notification_sink={seen[-1:]}")
    return seen[-1]


def test_the_identity_an_app_attaches_with_is_the_sink_and_outlives_an_agent_restart(
    sync_agent_binary, tmp_path
):
    pipe = _pipe("restart")
    data_dir = tmp_path / "agent"
    data_dir.mkdir()
    with registered_identity("restart") as aumid:
        with running_agent(sync_agent_binary, pipe, data_dir, tmp_path / "first.log"):
            assert _probed_sink(pipe) is False, "no app has named an identity to post under"
            # The attach adopts the identity before it replies.
            attach_once(pipe, aumid)
            assert _sink(pipe) is True, "the app's registered identity is a sink"

        # A restarted agent — the app closed — still posts under the app's identity.
        with running_agent(sync_agent_binary, pipe, data_dir, tmp_path / "second.log"):
            assert _probed_sink(pipe) is True, "the identity was not kept across the restart"


def test_an_identity_with_no_toast_registration_is_no_sink(sync_agent_binary, tmp_path):
    pipe = _pipe("unregistered")
    data_dir = tmp_path / "agent"
    data_dir.mkdir()
    with running_agent(sync_agent_binary, pipe, data_dir, tmp_path / "agent.log"):
        _probed_sink(pipe)
        attach_once(pipe, f"Fauna.AgentSinkE2e.unregistered.{os.getpid()}")
        assert _sink(pipe) is False

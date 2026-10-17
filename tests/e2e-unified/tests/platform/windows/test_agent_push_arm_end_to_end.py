"""tier_3 e2e: the windows agent's `ws-device` push arm, end to end — a live
nest's frame through the real agent process to a toast under the app's
identity, withheld while an app holds the attachment lease.

`common.md` § Push Notifications → *Transports* (the `ws-device` ruling: the
nest delivers a present row as a `fauna.push.notification` frame over every
connection that announced the row's device id, and the agent posts it only
while no app is attached) and `windows.md` § Notifications (the agent's half).
Each half is pinned on its own — the attach logic over a fake sink and the
WinRT sink against the real platform (`push_arm.rs`), the identity the agent
adopts and keeps (`test_agent_toast_sink.py`); this is the run that joins them.

What is real: a nest from the locally-built test-hooks binary; a registered
user whose app subscribed the machine's `ws-device` row; a stranger's mail over
`fauna.inbox.send`, which the nest holds as a knock under the default
`allow_knock` mode and pushes (the production trigger); the shipped agent
binary on a private pipe, data dir and `%LOCALAPPDATA%`, provisioned with the
user's own bearer, so its account connection dials the nest as that user and
announces the device; and the notification platform the toast lands on, read
back from the identity's history by a second process (`helpers/windows_toast`).

Convention 14 — every wait is on a fact, never a clock:

* *the agent is present* — the nest's own presence snapshot, the very input the
  dispatch decides on (`helpers.push_dispatch.await_present`);
* *the frame reached the agent* — `delivered` once `settled` has caught up
  with `initiated` (`helpers.push_dispatch`, the dispatch's anchors);
* *the agent decided it* — the agent's test-only push-arm tally. A withheld
  frame leaves nothing else behind, so the silence is asserted only after the
  tally says the frame was withheld; a late post can then no longer come;
* *the attachment ended* — the same tally's `attached_apps`, since the lease
  ends asynchronously as the app's connection closes;
* the toast is awaited in the history under a budget a green run never spends.

The order is an app's life: open (attached — the frame is withheld), then
closed (the lease is gone — the next frame is posted). The posted toast is also
what makes the silence mean something: the same sink that stayed quiet could
post.

The agent posts with its on-screen banner (it has no test seam to suppress
it), so the run paints one toast; `registered_identity` clears it from the
notification centre on exit.

Process safety: the agent is this test's own child (`running_agent`); the nest
is a dedicated one started through the harness's own seam.
"""

from __future__ import annotations

import os
import sys
import uuid

import pytest

from common import create_actor_and_register
from helpers import push_dispatch
from helpers.budgets import ACCOUNT_RUNTIME_ASSEMBLY_S, PUSH_DIAL_S, UI_SETTLE_S
from helpers.waiting import wait_until
from helpers.windows_sync_agent import (
    attached_app,
    provision,
    push_arm_tally,
    running_agent,
)
from helpers.windows_toast import registered_identity, toast_history

pytestmark = [
    pytest.mark.tier_3,
    # The nest's presence and dispatch reads are test-hooks surfaces, and the
    # agent dials the nest from this machine: a nest this harness built and
    # started here.
    pytest.mark.standalone_only,
    pytest.mark.skipif(sys.platform != "win32", reason="the windows agent's WinRT toast sink"),
]

#: The strings a `ws-device` frame carries — generic on purpose (`push.rs`
#: `GENERIC_TITLE` / `GENERIC_BODY`: the agent links no MLS, so it can render no
#: more). The toast's two text lines are the frame's title and body.
_FRAME_TEXT = ["Fauna", "New message"]


@pytest.fixture
def push_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest: the dispatch counters are nest-wide, and exact only
    where every dispatch is this test's own."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "agent-push-arm")
    yield nest
    cleanup()


@pytest.mark.feature("notifications")
def test_a_frame_is_withheld_while_the_app_is_open_and_toasted_once_it_closes(
    push_nest, sync_agent_binary, tmp_path
):
    url, port = push_nest["url"], push_nest["port"]
    admin_sk = push_nest["admin"]["signing_key"]
    user = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
    device_id = f"desk-{uuid.uuid4().hex[:8]}"

    # The app's push toggle, before the app closes: the machine's `ws-device`
    # row, and the bearer the app hands its agent.
    with push_dispatch.actor_client(url, user) as app:
        app.call(
            "fauna.push.subscribe",
            {"device_id": device_id, "endpoint": device_id, "transport": "ws-device"},
        )
        bearer = app.mint_bearer()

    pipe = r"\\.\pipe\fauna-sync-test-push-arm-{}".format(os.getpid())
    data_dir = tmp_path / "agent"
    data_dir.mkdir()
    local_appdata = tmp_path / "localappdata"
    local_appdata.mkdir()
    with (
        registered_identity("pusharm") as aumid,
        running_agent(
            sync_agent_binary, pipe, data_dir, tmp_path / "agent.log",
            extra_env={"LOCALAPPDATA": str(local_appdata)},
        ),
    ):
        provision(
            pipe,
            backup_key=os.urandom(32),
            actor_id=user["actor_id_bytes"],
            nest_url=url,
            device_id=device_id,
            bearer_token=bearer.token,
            bearer_expires_at=int(bearer.expires_at),
        )
        # The agent's capability connection announces the device — provisioned
        # is enough; no enrollment, no account-store mount (common.md
        # § Transports, ruled 2026-10-09).
        push_dispatch.await_present(url, user["actor_id_hex"], device_id, ACCOUNT_RUNTIME_ASSEMBLY_S)
        presence = push_dispatch.presence(url, user["actor_id_hex"])
        assert presence["any_unannounced"] is False, (
            "every live connection of the user must have announced — an "
            "unannounced agent connection is the compatibility rider's trigger "
            f"and silences the user's web-push/apns rows: {presence}"
        )

        # ── The app is open: it holds the lease and names its toast identity.
        with attached_app(pipe, aumid):
            assert push_arm_tally(pipe)["attached_apps"] == 1, "the attach replies holding the lease"
            delivered = _knock(url, port, admin_sk, user)
            assert delivered == 1, (
                "the knock's frame must be delivered over the agent's announcing "
                f"connection (common.md § Dispatch Logic); delivered {delivered}"
            )
            tally = _await_decided(pipe, 1)
            assert tally["withheld"] == 1, (
                f"with an app attached the agent must leave the banner to it: {tally}"
            )
            assert toast_history(aumid) == [], "a withheld frame must post nothing"

        # ── The app closes: its lease ends with its connection.
        wait_until(
            lambda: push_arm_tally(pipe)["attached_apps"] == 0,
            UI_SETTLE_S,
            diagnose=lambda: f"the app's lease outlived its connection: {push_arm_tally(pipe)}",
        )
        delivered = _knock(url, port, admin_sk, user)
        assert delivered == 1, f"the second knock's frame was delivered {delivered} times"
        tally = _await_decided(pipe, 2)
        assert tally == {
            "attached_apps": 0, "withheld": 1, "no_sink": 0, "posted": 1, "failed": 0,
        }, f"with no app attached the agent must post the frame as a toast: {tally}"

        toasts: list = []

        def landed() -> bool:
            toasts[:] = toast_history(aumid)
            return bool(toasts)

        wait_until(
            landed,
            PUSH_DIAL_S,
            diagnose=lambda: f"the agent reported a post but {aumid}'s history is empty",
        )
        assert toasts == [_FRAME_TEXT], (
            f"exactly the one posted frame must be under the app's identity: {toasts}"
        )


def _knock(url: str, port: int, admin_sk, user: dict) -> int:
    """A stranger's first mail to ``user``: held as a knock, and pushed. Returns
    how many `ws-device` deliveries its settled dispatch made."""
    before = push_dispatch.await_settled(url)
    stranger = create_actor_and_register(port, base_url=url, admin_signing_key=admin_sk)
    reply = push_dispatch.send_inbox_mail(url, stranger, user, "closed")
    assert reply.get("inbox_id") is None, (
        f"a stranger's first mail under allow_knock must be held as a knock: {reply!r}"
    )
    after = push_dispatch.await_settled(url, initiated=before["initiated"] + 1)
    return after["delivered"] - before["delivered"]


def _await_decided(pipe: str, frames: int) -> dict:
    """Wait until the agent's push arm has decided ``frames`` frames in all;
    returns its tally."""
    seen: list = []

    def decided() -> bool:
        seen[:] = [push_arm_tally(pipe)]
        t = seen[0]
        return t["withheld"] + t["no_sink"] + t["posted"] + t["failed"] >= frames

    wait_until(
        decided,
        PUSH_DIAL_S,
        diagnose=lambda: f"the agent heard the frame but never decided it: {seen[-1:]}",
    )
    return seen[0]

"""tier_3 e2e: the linux agent's `ws-device` push arm, end to end — a live
nest's frame through the real agent process to a desktop notification,
withheld while an app holds the attachment lease.

`common.md` § Push Notifications → *Transports* (the `ws-device` ruling: the
nest delivers a present row as a `fauna.push.notification` frame over every
connection that announced the row's device id, and the agent posts it only
while no app is attached; ruled 2026-10-09, the announce and the arm ride the
capability connection from provision on) and `linux.md` § Desktop
Notifications (the agent's half). The attach logic is pinned in-process over a
fake sink (`push_arm.rs`); this is the run that joins it to a nest and to the
freedesktop notification call. The windows twin is
`tests/platform/windows/test_agent_push_arm_end_to_end.py`.

What is real: a nest from the locally-built test-hooks binary; a registered
user whose app subscribed the machine's `ws-device` row; a stranger's mail over
`fauna.inbox.send`, which the nest holds as a knock under the default
`allow_knock` mode and pushes (the production trigger); the shipped agent
binary in a private XDG world, provisioned with the user's own bearer — no
enrollment — so its capability connection dials the nest as that user and
announces the device; a real `gnome-keyring-daemon` on the agent's private
session bus, unlocked, which is what makes the agent's banner sink present
(`keyring_probe`); and the `notify-rust` `Notify` call over real D-Bus.

What is a stand-in: the notification daemon. A test-owned
`org.freedesktop.Notifications` on the same private bus
(`helpers/notification_bus.py`) records each `Notify` — the read-back the
windows twin gets from the toast history. A banner here never reaches a
screen, and the box's own notification daemon is never on the bus.

Convention 14 — every wait is on a fact, never a clock:

* *the agent is present* — the nest's own presence snapshot, the very input the
  dispatch decides on (`helpers.push_dispatch.await_present`);
* *the frame reached the agent* — `delivered` once `settled` has caught up
  with `initiated` (`helpers.push_dispatch`, the dispatch's anchors);
* *the agent decided it* — the agent's test-only push-arm tally, which moves
  only after the decision (and, for a post, after `Notify` answered). A
  withheld frame leaves nothing else behind, so the silence is asserted only
  after the tally says the frame was withheld; a late post can then no longer
  come;
* *the attachment ended* — the same tally's `attached_apps`, since the lease
  ends asynchronously as the app's connection closes;
* *the banner was posted* — the daemon prints each `Notify` after replying, so
  a post the tally counted is awaited in the record under a budget a green run
  never spends.

Process safety: the agent, the bus, the keyring and the notification daemon are
this test's own children, stopped by handle; the nest is a dedicated one
started through the harness's own seam.
"""

from __future__ import annotations

import os
import sys
import uuid
from pathlib import Path

import pytest

from common import create_actor_and_register
from helpers import push_dispatch
from helpers.app_surface import skip_environment
from helpers.budgets import ACCOUNT_RUNTIME_ASSEMBLY_S, PUSH_DIAL_S, UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # The nest's presence and dispatch reads are test-hooks surfaces, and the
    # agent dials the nest from this machine: a nest this harness built and
    # started here.
    pytest.mark.standalone_only,
    pytest.mark.skipif(sys.platform != "linux", reason="the linux agent's freedesktop sink"),
]

#: The strings a `ws-device` frame carries — generic on purpose (`push.rs`
#: `GENERIC_TITLE` / `GENERIC_BODY`: the agent links no MLS, so it can render no
#: more). The banner's summary and body are the frame's title and body.
_FRAME_TITLE, _FRAME_BODY = "Fauna", "New message"


@pytest.fixture
def push_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest: the dispatch counters are nest-wide, and exact only
    where every dispatch is this test's own."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "agent-push-arm")
    yield nest
    cleanup()


@pytest.fixture
def notification_bus():
    """The agent's private session bus: an unlocked keyring (the sink's
    presence) and a recording notification daemon (its read-back)."""
    from common.keyring import secret_service_available

    if not secret_service_available():
        skip_environment("gnome-keyring-daemon + dbus-daemon are not installed")
    from helpers.notification_bus import NotificationBus

    bus = NotificationBus().start()
    try:
        yield bus
    finally:
        bus.stop()


@pytest.fixture
def linux_agent_binary(tui_app_path):
    """The `fauna-sync-agent` debug binary `just tui-debug` builds beside
    `fauna-tui` (the fixture builds it if stale)."""
    agent = Path(tui_app_path).parent / "fauna-sync-agent"
    assert agent.exists(), f"`just tui-debug` left no agent beside {tui_app_path}"
    return agent


@pytest.mark.feature("notifications")
def test_a_frame_is_withheld_while_the_app_is_open_and_posted_once_it_closes(
    push_nest, notification_bus, linux_agent_binary, tmp_path
):
    from helpers.linux_sync_agent import running_agent
    from helpers.sync_agent_ipc_unix import attached_app, provision, push_arm_tally

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

    data_dir = tmp_path / "agent"
    data_dir.mkdir()
    log_path = tmp_path / "agent.log"
    with running_agent(
        linux_agent_binary, data_dir, log_path, bus_address=notification_bus.address
    ) as sock:

        def diagnosis(what: str) -> str:
            log = log_path.read_text(errors="replace").strip().splitlines()[-15:]
            return (
                f"{what}\n  tally: {push_arm_tally(sock)}\n"
                f"  notifications: {notification_bus.notifications()}\n"
                f"  daemon output: {notification_bus.daemon_output()!r}\n"
                "  agent log tail:\n    " + "\n    ".join(log)
            )

        provision(
            sock,
            1,
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

        # ── The app is open: it holds the lease.
        with attached_app(sock, "linux"):
            assert push_arm_tally(sock)["attached_apps"] == 1, "the attach replies holding the lease"
            delivered = _knock(url, port, admin_sk, user)
            assert delivered == 1, (
                "the knock's frame must be delivered over the agent's announcing "
                f"connection (common.md § Dispatch Logic); delivered {delivered}"
            )
            tally = _await_decided(sock, 1, diagnosis)
            assert tally["withheld"] == 1, diagnosis(
                "with an app attached the agent must leave the banner to it"
            )
            assert notification_bus.notifications() == [], "a withheld frame must post nothing"

        # ── The app closes: its lease ends with its connection.
        wait_until(
            lambda: push_arm_tally(sock)["attached_apps"] == 0,
            UI_SETTLE_S,
            diagnose=lambda: f"the app's lease outlived its connection: {push_arm_tally(sock)}",
        )
        delivered = _knock(url, port, admin_sk, user)
        assert delivered == 1, f"the second knock's frame was delivered {delivered} times"
        tally = _await_decided(sock, 2, diagnosis)
        assert tally == {
            "attached_apps": 0, "withheld": 1, "no_sink": 0, "posted": 1, "failed": 0,
        }, diagnosis("with no app attached the agent must post the frame as a banner")

        wait_until(
            lambda: bool(notification_bus.notifications()),
            PUSH_DIAL_S,
            diagnose=lambda: diagnosis("the agent reported a post but the daemon recorded none"),
        )
        posted = [
            (n["app_name"], n["summary"], n["body"]) for n in notification_bus.notifications()
        ]
        assert posted == [("Fauna", _FRAME_TITLE, _FRAME_BODY)], (
            f"exactly the one posted frame must reach the notification daemon: {posted}"
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


def _await_decided(sock: str, frames: int, diagnosis) -> dict:
    """Wait until the agent's push arm has decided ``frames`` frames in all;
    returns its tally."""
    from helpers.sync_agent_ipc_unix import push_arm_tally

    seen: list = []

    def decided() -> bool:
        seen[:] = [push_arm_tally(sock)]
        t = seen[0]
        return t["withheld"] + t["no_sink"] + t["posted"] + t["failed"] >= frames

    wait_until(
        decided,
        PUSH_DIAL_S,
        diagnose=lambda: diagnosis("the agent heard the frame but never decided it"),
    )
    return seen[0]

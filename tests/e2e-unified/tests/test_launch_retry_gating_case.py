"""A launch that entered NO session lands on ``launch_retry`` when the nest is
unreachable — never on the wizard's ``identity_choice``.

``docs/goal/behavior/onboarding.md`` § App-launch routing: the *Network failure
or nest unreachable* row shows the ``launch_retry`` surface
(``launch-transient-error`` / ``launch-retry-button`` /
``launch-fallthrough-button``, plus ``launch-retire-button`` —
``nest-retirement.md`` § Layout & flow, always shown there); and the iOS
entry-posture paragraph: when there is nothing to enter optimistically — entry
withheld for a pending-factory-reset slot — iOS GATES like the others.

The seed (``helpers/launch_retry_seed.py``): an identity whose saved nest
refuses the connection, plus a STALE pending-factory-reset slot naming the
session's live, claimed nest. The slot is what makes iOS withhold its optimistic
entry; the boot reconcile clears it (stale, box claimed), and the silent
challenge against the dead saved nest is transient. On iOS this frame used to
paint ``identity_choice`` over the held identity — the transient arm was written
for an entered session and only logged. The other apps gate on every launch, so
for them this is the ordinary retry row reached through the reconcile.

Native apps only: the seed rides the launch harness's file-backed store
(``raw_fields``); web's store is the browser's and android's seed is sent with
the launch — neither is a different behavior, only a seam this case does not
need (the retry surface itself is pinned for them by
``test_onboarding_launch_routing_smoke.py`` cases C/D/F).
"""

from __future__ import annotations

import secrets
import socket

import pytest

from common.launch_harness import make_launch_harness
from conftest import _trust_seeder, get_available_apps
from helpers.app_launch import app_launch_config
from helpers.launch_retry_seed import stale_reset_slot_fields
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.macos,
    pytest.mark.ios, pytest.mark.windows,
]

GATING_APPS = ("tui", "linux", "macos", "ios", "windows")

#: ``ui.yaml`` ``onboarding.launch_retry``'s elements, every one required here.
LAUNCH_RETRY_IDS = (
    "launch-transient-error",
    "launch-retry-button",
    "launch-fallthrough-button",
    "launch-retire-button",
)
#: ``identity_choice``'s entry controls — the frame this case exists to forbid.
IDENTITY_CHOICE_IDS = ("create-identity-button", "import-identity-button")


def _clients() -> list[str]:
    available = get_available_apps()
    return [c for c in GATING_APPS if c in available]


def _closed_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@pytest.mark.parametrize("client", _clients())
@pytest.mark.feature("connect-and-sign-in")
def test_no_session_entered_and_nest_unreachable_lands_launch_retry(
    request, tmp_path, nest_instance, client
):
    harness = make_launch_harness(
        client, tmp_path=tmp_path, **app_launch_config(client, request),
        file_backed=True, seed_trust=_trust_seeder(request),
    )
    try:
        secret_hex = secrets.token_hex(32)
        driver = harness.launch(
            secret_hex=secret_hex,
            node_url=f"http://127.0.0.1:{_closed_port()}",
            trust=None,
            raw_fields=stale_reset_slot_fields(secret_hex, nest_instance["url"]),
        )
        wait_until(
            lambda: driver.is_visible("launch-retry-button")
            or any(driver.is_visible(i) for i in IDENTITY_CHOICE_IDS),
            90,
            diagnose=lambda: "the launch settled on neither launch_retry nor "
            "identity_choice: " + driver.diagnose("launch-retry-button"),
        )
        for eid in IDENTITY_CHOICE_IDS:
            assert driver.is_absent(eid), (
                f"{client}: a launch that entered no session painted the wizard's "
                f"identity_choice ({eid}) over the held identity instead of "
                f"launch_retry: {driver.diagnose(eid)}"
            )
        for eid in LAUNCH_RETRY_IDS:
            assert driver.is_visible(eid), (
                f"{client}: launch_retry is missing {eid}: {driver.diagnose(eid)}"
            )
    finally:
        harness.teardown()

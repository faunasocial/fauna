"""tier_3 e2e: on web, asking a nest that already holds your account for an
invite says so plainly and never tells you to try again.

`docs/features/connect-and-sign-in.md` outcome 11;
`docs/goal/behavior/onboarding.md` § 3. Invite request (`invite_request`).
The native apps' witness is
``test_onboarding_launch_routing_smoke.py::test_smoke_n_suspended_user_fallthrough_invite_submit_is_terminal``,
which reaches the invite page through the refused launch's fallthrough and a
real handle check against a TLS nest dialled at the typed loopback handle — a
dial a browser cannot make (web reaches nests through the CORS ``spa_url``
proxy; that module's case K). The refused launch itself is witnessed on web by
case E's sibling, ``test_smoke_e2_…``.

So web reaches the same page the way ``test_invite_request_submit_roundtrip.py``
does — the paste-secret import, then the wizard's
``navigate_to_invite_request_for_known_nest`` for a claimed nest — and the
subject under test is all real: a registered-then-suspended actor's key (a
random key's submit SUCCEEDS as a pending row, so only a key the nest holds
reaches the refusal), a real ``fauna.account.invite_request.submit`` over the
anonymous WS, the nest's ``fauna.account.actor_exists`` refusal, and the shared
onboarding machine's dedicated ``already_registered`` sentence on
``invite-request-status``.
"""

from __future__ import annotations

import contextlib
import json

import pytest

from actions import ActionLayer
from common.auth import _authed_call, create_actor_and_register
from drivers import create_driver
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

INVITE_SUBMIT = "invite-request-submit-button"
INVITE_STATUS = "invite-request-status"
BOOT_BUDGET_S = 90.0
ROUTE_BUDGET_S = 45.0


@pytest.mark.feature("connect-and-sign-in")
def test_web_suspended_user_invite_submit_is_terminal(nest_instance, spa_url):
    admin_sk = nest_instance["admin"]["signing_key"]
    actor = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    _authed_call(nest_instance["url"], admin_sk, "fauna.admin.users.suspend", {
        "actor_id": actor["actor_id_bytes"],
        "category": "other",
        "reason": "e2e: suspended user's invite request on web",
    })

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        app = ActionLayer(driver)
        driver.wait_for("import-identity-button", timeout=BOOT_BUDGET_S)
        app.onboarding.import_key(bytes(actor["signing_key"]).hex())
        driver.wait_for("handle-input", timeout=ROUTE_BUDGET_S)

        driver.call_machine_method(
            "navigate_to_invite_request_for_known_nest",
            json.dumps([nest_instance["url"], actor["handle"] or "suspended"]),
        )
        driver.wait_for(INVITE_SUBMIT, timeout=ROUTE_BUDGET_S)
        driver.click(INVITE_SUBMIT)

        # A condition wait on the painted sentence (convention 14).
        wait_until(
            lambda: S.onboarding.invite.error.already_registered
            in (driver.get_text(INVITE_STATUS) or ""),
            ROUTE_BUDGET_S,
            diagnose=lambda: f"{INVITE_STATUS}={driver.get_text(INVITE_STATUS)!r}, "
            f"app error: {app.error_text()!r}",
        )
        status = driver.get_text(INVITE_STATUS) or ""
        assert "Try again" not in status, (
            f"an already-registered refusal never reads as a retry, got {status!r}"
        )
        # The refusal is permanent until the admin restores: no pending row
        # and nothing to recheck.
        assert driver.is_absent("invite-request-recheck-button"), (
            "an already-registered refusal must not reach PendingReview"
        )
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()

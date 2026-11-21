"""The admin's main use cases, walked by gestures alone — `invite-someone` and
`turn-mail-on` (`docs/goal/behavior/main-use-cases.md` § The journeys;
the steps are catalogued in `use-cases.yaml`).

Each journey starts where an admin is after signing in: on the feed of the
primary view (`admin_main_app`). It reaches the admin page by its tab and its
rail row, acts with the page's own control, and reads the outcome on the page.
Then `assert_use_case_walked` checks that the driver's gesture record walked the
catalogued steps (convention 8: the mutation under test is a UI gesture; an API
call only arranges the world).

* `invite-someone` — the world holds one pending invite request, submitted the
  way a requester's onboarding submits it. The admin approves it with
  `invite-request-row-approve-button`. Picking a tier is not a step: the row's
  approve-at picker starts on the nest's `free` tier (`admin.md` § 2, Section
  1), so approving it as offered is what a person does. The outcome is that the
  request leaves the pending list and the requester gets a user row.
* `turn-mail-on` — the world has deployment mail off, as on a nest claimed
  with a loopback or IP handle (`mail-bridge-lifecycle.md` § Default-off on
  first claim). The arrangement turns mail off over the admin socket and
  restores it afterwards, because the session nest is shared. A fresh box's
  mail bridge needs no approval step: the box's own MTA is auto-approved once
  `mail_enabled` holds (§ Onboarding auto-approval). A standalone test nest
  declares no content scanners `absent`, so the toggle is live and the page has
  no unblocking step to offer (§ *A box with no content scanners cannot enable
  mail*). The outcome is the toggle reading on, and the nest agreeing.

tier_3: a real nest and a real app, nothing stubbed. tui is the lead app; the
other apps run it through the background tester.
"""

from __future__ import annotations

import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from helpers import budgets
from helpers.use_case_walk import assert_use_case_walked
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
]


def _admin_client(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _nest(nest_instance, kind: str, params: dict | None = None):
    with _admin_client(nest_instance) as client:
        return client.call(kind, params or {})


def _submit_invite_request(nest_url: str, handle: str) -> str:
    """Submit a real signed invite request, the way a requester's onboarding
    does (the anonymous `fauna.account.invite_request.submit`). Returns the
    requester's actor hex, so the outcome is read by identity."""
    from common.sig_domain import invite_submit_signed_message

    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    message = "let me in"
    ts = int(time.time() * 1000)
    msg = invite_submit_signed_message(bytes.fromhex(actor_hex), handle, message, ts)
    with WsRpcAnonClient(nest_url) as anon:
        anon.call(
            "fauna.account.invite_request.submit",
            {
                "actor_id": actor_hex,
                "handle": handle,
                "message": message,
                "timestamp": ts,
                "signature": sk.sign(msg).signature.hex(),
            },
        )
    return actor_hex


@pytest.mark.use_case("invite-someone")
@pytest.mark.feature("admin-users")
def test_admin_approves_an_invite_request_by_gestures(admin_main_app, nest_instance):
    app = admin_main_app
    # The session nest is shared by every app parametrization and refuses a
    # handle already taken, so the handle is fresh per run.
    handle = f"usecase{secrets.token_hex(3)}"
    actor_hex = _submit_invite_request(nest_instance["url"], handle)

    app.admin.open_page_by_click("admin-users", "admin-users-requests-section")
    # The pending list is fetched when the page opens. If the request has not
    # surfaced yet, the admin opens the page again from the same rail row: a
    # repeat of the step just walked, never a state write.
    deadline = time.monotonic() + budgets.RPC_ROUNDTRIP_S
    while (handle not in app.admin.invite_request_handles()
           and time.monotonic() < deadline):
        time.sleep(0.5)
        app.driver.click("admin-nav-row[admin-users]")
        app.driver.wait_for("admin-users-requests-section", timeout=15.0)
    assert handle in app.admin.invite_request_handles(), (
        f"the pending request {handle!r} never surfaced on admin-users. "
        f"{app.admin.pending_requests_diagnosis()}"
    )

    app.admin.approve_request(index=app.admin.invite_request_row_index(handle))

    # The outcome where the admin sees it: the request leaves the pending list
    # and its requester appears among the users.
    try:
        wait_until(lambda: handle not in app.admin.invite_request_handles(),
                   budgets.RPC_ROUNDTRIP_S)
    except AssertionError:
        pass
    assert handle not in app.admin.invite_request_handles(), (
        f"approving {handle!r} left it on the pending list. "
        f"{app.admin.pending_requests_diagnosis()}"
    )
    try:
        wait_until(lambda: app.admin.has_user_row(actor_hex), budgets.RPC_ROUNDTRIP_S)
    except AssertionError:
        pass
    assert app.admin.has_user_row(actor_hex), (
        f"the approved requester {actor_hex[:12]}… has no user row. "
        f"{app.admin.pending_requests_diagnosis()}"
    )
    assert_use_case_walked(app.driver, "invite-someone")


@pytest.fixture
def mail_off_restored(nest_instance):
    """Arrange deployment mail off, and put it back the way the test found it:
    the session nest is shared by every later module."""
    before = _nest(nest_instance, "fauna.bridges.get_mail_config")["mail_enabled"]
    _nest(nest_instance, "fauna.bridges.set_mail_enabled", {"enabled": False})
    yield
    _nest(nest_instance, "fauna.bridges.set_mail_enabled", {"enabled": before})


@pytest.mark.use_case("turn-mail-on")
@pytest.mark.feature("admin-mail-policy")
def test_admin_turns_mail_on_by_gestures(admin_main_app, nest_instance, mail_off_restored):
    app = admin_main_app
    toggle = "admin-mail-enabled-toggle"

    app.admin.open_page_by_click("admin-mail", toggle)
    # The toggle reads the persisted state once the page has hydrated: wait for
    # it to read off before acting, so the click turns mail on and not off.
    try:
        wait_until(lambda: app.driver.get_attr(toggle, "checked") == "false",
                   budgets.RPC_ROUNDTRIP_S)
    except AssertionError:
        pass
    assert app.driver.get_attr(toggle, "checked") == "false", (
        f"admin-mail never read mail off before the click: "
        f"{app.driver.diagnose(toggle, attrs=('checked', 'state', 'disabled'))}; "
        f"error: {app.error_text()!r}"
    )

    app.admin.toggle_mail_enabled()

    try:
        wait_until(lambda: app.driver.get_attr(toggle, "checked") == "true",
                   budgets.RPC_ROUNDTRIP_S)
    except AssertionError:
        pass
    assert app.driver.get_attr(toggle, "checked") == "true", (
        f"turning mail on did not read back on the page: "
        f"{app.driver.diagnose(toggle, attrs=('checked', 'state', 'disabled'))}; "
        f"error: {app.error_text()!r}"
    )
    assert _nest(nest_instance, "fauna.bridges.get_mail_config")["mail_enabled"], (
        "the page reads mail on but the nest does not"
    )
    assert_use_case_walked(app.driver, "turn-mail-on")

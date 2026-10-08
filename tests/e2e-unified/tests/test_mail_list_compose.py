"""E2E journeys for SENDING to one of your own mailing lists from the app.

Target state: docs/goal/behavior/mail-mass-mailing.md § Composing a list
message (the list is addressed by typing its address into the ordinary
recipient picker; the compose form warns how many subscribers the send reaches
and today's quota before Send; the send goes out through the nest's list
fan-out, one copy per subscribed member; it shows as ONE entry in your sent
mail; its delivery progress is one figure for the whole send) and § The per-day
per-account cap (approaching-limit warning within 10% of the cap; an over-limit
send is explained where it was composed). UX/IDs: tests/e2e-unified/ui.yaml
`dm-compose-form` `optional_elements` — `dm-compose-list-send-warning`,
`dm-compose-list-send-progress`, `dm-compose-list-quota-warning`.

tier_3: every binary real — the app's own compose drives
`fauna.bridges.send_list_message`, the real MTA bridge relays each member's
copy to the stub external MX, and the nest files the one Sent copy. The list,
its members and (outcome 14) the lowered daily cap are arranges over the
user's WS-RPC surface and the nest's test-hooks policy endpoint; the act under
test is always the compose form.

tui leads; the other six apps join once their compose renders the three ids
(the per-app trickle-down rows).
"""

import json
import secrets
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.e2e_session import E2E_LOGIN_DEVICE_ID
from helpers.mail_aliases import add_exact_alias
from helpers.mail_wire import header_value
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

# Routed to the stub external MX by the fixture's `mta_mx_override`.
MEMBER_DOMAIN = "external.test"
HANDLE_LOCAL = "admin"
SETTLE_S = 30.0


def _sign_in_with_mail(app, handle):
    """Log the app in as the dedicated nest's admin, turn mail on through the
    real mail-settings page and make `<handle>@<domain>` routable — the
    arrange `test_tui_mail_outbound_from.py` uses — and return the admin's
    user-surface WS-RPC client."""
    handle.assert_mta_running()
    nest = handle.nest
    admin_sk = nest["admin"]["signing_key"]
    admin_actor_id = bytes(admin_sk.verify_key)

    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": admin_sk.encode().hex(),
            "handle": HANDLE_LOCAL,
            "actor_id": admin_actor_id.hex(),
            "device_id": E2E_LOGIN_DEVICE_ID,
        },
        "nav": {"stack": [{"view": "conversations"}]},
    })
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    handle.rebind_after_enable()
    add_exact_alias(nest["url"], admin_sk, handle.domain, HANDLE_LOCAL)
    return WsRpcAdminClient(nest["url"], actor_id=admin_actor_id, signing_key=bytes(admin_sk))


def _create_list(api, domain, members):
    """A list on the mail domain with `members` external subscribers; returns
    (list_id, address, member addresses)."""
    local_part = "news" + secrets.token_hex(3)
    list_id = api.call(
        "fauna.bridges.create_account_list",
        {"local_part": local_part, "local_domain": domain, "friendly_name": "Weekly news"},
    )["list_id"]
    addresses = []
    for _ in range(members):
        address = f"reader-{secrets.token_hex(3)}@{MEMBER_DOMAIN}"
        api.call(
            "fauna.bridges.add_list_member",
            {"list_id": list_id, "recipient_address": address},
        )
        addresses.append(address)
    return list_id, f"{local_part}@{domain}", addresses


def _sent_count(api):
    return len(
        api.call("fauna.email.sent.fetch", {"after_uid": 0, "limit": 500})["messages"]
    )


def _set_daily_cap(nest_url, cap):
    """Lower the per-account daily list-recipient ceiling (the nest's
    `--features test-hooks` policy endpoint; no admin UI exists for it yet)."""
    body = json.dumps({"list_recipients_per_account_per_day_ceiling": cap}).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/mass-mailing/policy",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"mass-mailing policy hook returned {resp.status}"


def _account_meter(api):
    reply = api.call("fauna.bridges.list_account_lists", {})
    return reply.get("account_recipients_today", 0)


def _relayed_to(stub_mx, token):
    """The member addresses the stub external MX received a copy carrying
    `token` for (one entry per relayed copy)."""
    got = []
    for raw in stub_mx.messages():
        if token.encode() in raw:
            got.append(raw)
    return got


def _compose_to_list(app, address):
    """Open the new-conversation composer and commit the list's address as the
    recipient — the user's whole gesture for addressing a list."""
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.conversations.add_recipient(address)


def _text(app, element_id):
    wait_until(
        lambda: app.driver.count(element_id) > 0,
        SETTLE_S,
        diagnose=lambda: f"{element_id} never rendered: {app.driver.diagnose(element_id)}; "
        f"error: {app.error_text()!r}",
    )
    return app.driver.get_text(element_id)


@pytest.mark.feature("mailing-lists")
def test_sending_to_a_list_from_the_app_warns_reaches_every_member_once_and_shows_one_sent_entry(
    app, dedicated_mail_nest_handle_domain
):
    """Outcomes 10–13: addressing a list in the compose form warns how many
    subscribers it reaches and today's quota; Send reaches every member; the
    send is one entry in your sent mail; its progress is one whole-send figure."""
    handle = dedicated_mail_nest_handle_domain
    with _sign_in_with_mail(app, handle) as api:
        _list_id, address, members = _create_list(api, handle.domain, members=2)
        sent_before = _sent_count(api)

        # Outcome 11 — before sending: the reach and today's allowance.
        _compose_to_list(app, address)
        warning = _text(app, "dm-compose-list-send-warning")
        assert "2 subscribed recipients" in warning, warning
        assert "Weekly news" in warning, warning
        assert "Today's quota: 0 / " in warning, warning
        assert app.driver.count("dm-compose-list-quota-warning") == 0, (
            "nowhere near the cap: no quota warning"
        )

        # Outcome 10 — Send from the app.
        token = f"list-issue-{secrets.token_hex(4)}"
        app.conversations.add_topic_to_compose(f"Issue {token}")
        app.driver.type_text("dm-text-field", f"Hello readers {token}\n")
        app.driver.click("dm-send-button")

        wait_until(
            lambda: len(_relayed_to(handle.stub_mx, token)) >= 2,
            SETTLE_S,
            diagnose=lambda: f"the stub MX holds {len(_relayed_to(handle.stub_mx, token))} "
            f"copies of {token!r}; error: {app.error_text()!r}; {handle.bridge_log_hint()}",
        )
        relayed = _relayed_to(handle.stub_mx, token)
        assert len(relayed) == len(members), "exactly one copy per subscribed member"
        # Each copy went out through the list fan-out: it carries its member's
        # own one-click unsubscribe link, so two copies with two distinct links
        # are two different members (the member address rides only the envelope).
        links = {header_value(raw, "List-Unsubscribe") for raw in relayed}
        assert None not in links and len(links) == len(members), links

        # Outcome 13 — the send's progress, as a whole.
        progress = _text(app, "dm-compose-list-send-progress")
        assert "2 of 2 recipients" in progress, progress

        # Outcome 12 — one entry in your sent mail, not one per recipient.
        assert app.driver.count("dm-message-text") == 1, (
            f"the thread should hold the send once; {app.driver.diagnose('dm-message-text')}"
        )
        wait_until(
            lambda: _sent_count(api) == sent_before + 1,
            SETTLE_S,
            diagnose=lambda: f"the Sent mailbox should gain exactly one record, gained "
            f"{_sent_count(api) - sent_before}",
        )


@pytest.mark.feature("mailing-lists")
def test_the_compose_form_warns_near_todays_list_limit_and_explains_an_over_limit_send(
    app, dedicated_mail_nest_handle_domain
):
    """Outcome 14: close to today's per-account list limit the compose form
    says how many recipients are left; a send that would pass the limit is
    explained where it was composed, and nothing goes out."""
    handle = dedicated_mail_nest_handle_domain
    nest_url = handle.nest["url"]
    with _sign_in_with_mail(app, handle) as api:
        list_id, address, _members = _create_list(api, handle.domain, members=2)
        # Arrange: a daily cap of 20 with 18 already used (nine earlier sends
        # to the 2 members) — 2 left, which is within 10% of the cap.
        _set_daily_cap(nest_url, 20)
        try:
            raw = (
                f"From: {HANDLE_LOCAL}@{handle.domain}\r\nSubject: earlier\r\n\r\nearlier\r\n"
            ).encode()
            for _ in range(9):
                api.call("fauna.bridges.send_list_message", {"list_id": list_id, "message": raw})

            # 18 of 20 used: approaching — 2 more recipients today.
            _compose_to_list(app, address)
            approaching = _text(app, "dm-compose-list-quota-warning")
            assert "Approaching daily limit" in approaching, approaching
            assert "2 more recipients today" in approaching, approaching

            # The last send that fits is accepted (the nest's meter reaches the
            # cap); the next one would pass it. Delivery is outcome 10's.
            app.driver.type_text("dm-text-field", "Last one\n")
            app.driver.click("dm-send-button")
            wait_until(
                lambda: _account_meter(api) == 20,
                SETTLE_S,
                diagnose=lambda: f"the last fitting send was not accepted: meter "
                f"{_account_meter(api)}; error: {app.error_text()!r}",
            )

            over = None

            def _over():
                nonlocal over
                if app.driver.count("dm-compose-list-quota-warning") == 0:
                    return False
                over = app.driver.get_text("dm-compose-list-quota-warning")
                return "would pass today's limit of 20" in over

            wait_until(_over, SETTLE_S, diagnose=lambda: f"no over-limit explanation; read {over!r}")
            assert "0 left" in over, over

            # Sending anyway is refused by the nest; the explanation stays put
            # and nothing is reserved or sent.
            refused = f"list-refused-{secrets.token_hex(4)}"
            app.driver.type_text("dm-text-field", f"Too many {refused}\n")
            app.driver.click("dm-send-button")
            wait_until(
                lambda: bool(app.error_text()),
                SETTLE_S,
                diagnose=lambda: "the refused send must say why on the compose page",
            )
            assert "would pass today's limit" in app.driver.get_text(
                "dm-compose-list-quota-warning"
            )
            # The nest refuses before reserving or enqueueing anything, so the
            # send's failure is already final: no copy can follow it.
            assert _account_meter(api) == 20
            assert _relayed_to(handle.stub_mx, refused) == []
        finally:
            _set_daily_cap(nest_url, 50_000)

"""E2E journeys for the per-account mail switches and the automatic reply on
the mail settings page.

Target state: `docs/goal/behavior/mail-policy-config.md` § Tier 3 — per-account
(user): "Receive mail for this account" and "Send mail from this account", both
on by default, and the auto-respond toggle + message, which keep no state of
their own — they are one match-all `AutoReply` filter rule
(`docs/goal/behavior/email-filters.md` § The auto-respond setting). UX/IDs:
tests/e2e-unified/ui.yaml `mail-settings` page — `mail-settings-receive-toggle`,
`mail-settings-send-toggle`, `mail-settings-autorespond-toggle`,
`mail-settings-autorespond-message-input`, `error-message`.

What these witness is the app half: the gesture on the page reaches the account
(read back over the same User-class doors the page uses), each toggle's `state`
repaints from what the nest kept, and a refused draft names its reason with
nothing stored. That a switched-off account refuses mail, and that the
auto-reply is sent at delivery, are the nest's outcomes, witnessed by the nest
tests (`test_mail_bridge_filter_delivery.py` for the reply).

tier_3: a real app driver against a real fauna-nest.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.waiting import wait_until
from i18n.strings import S

# tui leads (the lead app); the other six apps join by adding their marker once
# their run is green.
pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.web]

_UI_S = 20.0


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )


def _wait(pred, budget_s: float = _UI_S) -> bool:
    """A deadline poll (convention 14) that answers instead of raising, so the
    caller's assert carries its own diagnosis."""
    try:
        return bool(wait_until(pred, budget_s))
    except AssertionError:
        return False


def _autorespond_rules(api) -> list[dict]:
    """The caller's match-all AutoReply rules — what the setting is."""
    filters = api.call("fauna.email.filters.list", {}).get("filters") or []
    return [
        f for f in filters
        if not f.get("rules") and f.get("combination") == "all"
        and isinstance(f.get("action"), dict) and "AutoReply" in f["action"]
    ]


@pytest.fixture
def _account_reset(nest_instance, test_user):
    """The settings are the session user's: start from, and put back, both
    switches on and no automatic reply, so test order cannot leak a refused
    mailbox or a reply into a later test's deliveries."""

    def reset():
        with _user_client(nest_instance, test_user) as api:
            api.call("fauna.bridges.set_mail_switches", {
                "receive_enabled": True,
                "send_enabled": True,
            })
            for rule in _autorespond_rules(api):
                api.call("fauna.email.filters.delete", {"id": rule["id"]})

    reset()
    yield
    reset()


@pytest.mark.feature("turn-on-mail")
def test_receiving_and_sending_are_switched_off_and_on_separately(
    logged_in_app, nest_instance, test_user, _account_reset
):
    """Outcome 22: both switches start on. Turning receiving off leaves sending
    on, and the reverse; each toggle shows what the account kept, on this visit
    and the next."""
    app = logged_in_app
    ms = app.mail_settings
    ms.navigate()
    ms.ensure_mail_enabled()

    def stored():
        with _user_client(nest_instance, test_user) as api:
            return api.call("fauna.bridges.get_mail_switches", {})

    assert _wait(lambda: ms.toggle_state(ms.RECEIVE_ID) == "on"), (
        f"receiving must start on; the toggle reads {ms.toggle_state(ms.RECEIVE_ID)!r}"
    )
    assert ms.toggle_state(ms.SEND_ID) == "on", "sending must start on"

    ms.click_toggle(ms.RECEIVE_ID)
    assert _wait(lambda: stored()["receive_enabled"] is False), (
        f"turning receiving off never reached the account (reads {stored()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )
    assert stored()["send_enabled"] is True, "turning receiving off must leave sending on"
    assert _wait(lambda: ms.toggle_state(ms.RECEIVE_ID) == "off")
    assert ms.toggle_state(ms.SEND_ID) == "on"

    ms.click_toggle(ms.SEND_ID)
    assert _wait(lambda: stored()["send_enabled"] is False), (
        f"turning sending off never reached the account (reads {stored()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )
    assert _wait(lambda: ms.toggle_state(ms.SEND_ID) == "off")

    # A later visit shows what the account kept.
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    ms.navigate()
    assert _wait(lambda: ms.toggle_state(ms.RECEIVE_ID) == "off")
    assert ms.toggle_state(ms.SEND_ID) == "off"

    ms.click_toggle(ms.RECEIVE_ID)
    assert _wait(lambda: stored()["receive_enabled"] is True), (
        f"turning receiving back on never reached the account (reads {stored()!r})"
    )
    assert stored()["send_enabled"] is False, "turning receiving on must leave sending off"
    assert _wait(lambda: ms.toggle_state(ms.RECEIVE_ID) == "on")


@pytest.mark.feature("mail-filter-rules")
def test_an_automatic_reply_is_turned_on_with_its_message_and_off_again(
    logged_in_app, nest_instance, test_user, _account_reset
):
    """Outcome 11: the person writes an automatic reply and it is on, with
    exactly that message; a blank message is refused with its reason; turning
    the toggle off removes the reply, and turning it on again sends the same
    words."""
    app = logged_in_app
    ms = app.mail_settings
    ms.navigate()
    ms.ensure_mail_enabled()

    def bodies():
        with _user_client(nest_instance, test_user) as api:
            return [r["action"]["AutoReply"]["body"] for r in _autorespond_rules(api)]

    assert _wait(lambda: ms.toggle_state(ms.AUTORESPOND_ID) == "off"), (
        f"the automatic reply must start off; reads {ms.toggle_state(ms.AUTORESPOND_ID)!r}"
    )

    # A blank message is refused on the page, before anything is stored.
    ms.set_autorespond_message("   ")
    assert _wait(
        lambda: ms.page_error_text(timeout=1.0) == S.mail_settings.autorespond_message_empty
    ), f"a blank reply must say so; error-message reads {ms.page_error_text(timeout=1.0)!r}"
    assert bodies() == [], "a refused draft must store no reply"

    message = "Away until Monday."
    ms.set_autorespond_message(message)
    assert _wait(lambda: bodies() == [message]), (
        f"the page's reply never reached the account (rules {bodies()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )
    assert _wait(lambda: ms.toggle_state(ms.AUTORESPOND_ID) == "on")

    # A later visit shows the stored reply.
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    ms.navigate()
    assert _wait(lambda: ms.autorespond_message() == message), (
        f"a fresh visit must show the stored reply; shows {ms.autorespond_message()!r}"
    )

    ms.click_toggle(ms.AUTORESPOND_ID)
    assert _wait(lambda: bodies() == []), (
        f"turning the reply off must remove it (rules {bodies()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )
    assert _wait(lambda: ms.toggle_state(ms.AUTORESPOND_ID) == "off")

    # The words stay on the page, so turning it back on sends them again.
    ms.click_toggle(ms.AUTORESPOND_ID)
    assert _wait(lambda: bodies() == [message]), (
        f"turning the reply back on must send the same words (rules {bodies()!r})"
    )

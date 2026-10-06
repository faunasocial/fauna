"""E2E journeys for the Forwarding section of the mail settings page: forwarding
all incoming mail to another address, and the person's own hourly forwarding
limit.

Target state: `docs/goal/behavior/mail-forwarding.md` § Per-account "forward all"
(set an address to forward, clear it to stop; the app checks the address, the
nest re-checks it and refuses one on a domain it hosts — that is an alias, not a
forward) and § Per-account forward rate-limit (the person's own limit, default
100, never above the admin ceiling the nest reports). UX/IDs:
tests/e2e-unified/ui.yaml `mail-settings` page —
`mail-settings-forward-all-to-input`, `mail-settings-forward-per-hour-input`,
`error-message`.

What these witness is the app half: the gesture on the page reaches the account
(read back over the same User-class doors the page uses), a refused value names
its reason on the page and leaves the stored value alone, and the field repaints
from what the nest kept. That forwarding then actually happens at delivery is
the nest's outcome, witnessed by `test_mail_bridge_mta.py`'s forward-all tests.

tier_3: a real app driver against a real fauna-nest.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.waiting import wait_until
from i18n.strings import S

# tui leads (the lead app); the other six apps join by adding their marker once
# their run is green.
pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

# A domain this nest hosts, so the hosted-address refusal has one to refuse.
_HOSTED_DOMAIN = "forwarding-settings-e2e.test"
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


@pytest.fixture
def _forwarding_reset(nest_instance, test_user):
    """The settings are the session user's: start from, and put back, the
    defaults — no forwarding, the default limit — so test order cannot leak
    a forward into a later test's deliveries."""

    def reset():
        with _user_client(nest_instance, test_user) as api:
            api.call("fauna.bridges.set_forward_all_to", {"forward_all_to": None})
            api.call("fauna.bridges.set_forward_per_hour", {"forward_per_hour": 100})

    reset()
    yield
    reset()


@pytest.mark.feature("mail-filter-rules")
def test_forward_all_mail_is_set_and_stopped_from_settings(
    logged_in_app, nest_instance, test_user, _forwarding_reset
):
    """Outcome 9: the person enters an address to forward all their incoming
    mail and clears it to stop. A mistyped address and one on a domain this
    nest hosts are refused on the page with their reason, and forwarding keeps
    going to the address already set."""
    app = logged_in_app
    ms = app.mail_settings
    ms.navigate()
    ms.ensure_mail_enabled()

    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as admin_ws:
        admin_ws.call("fauna.bridges.add_local_domain", {
            "domain": _HOSTED_DOMAIN,
            "mta_sts_cert_mode": "expand_primary",
        })

    def stored():
        with _user_client(nest_instance, test_user) as api:
            return api.call("fauna.bridges.get_forward_all_to", {}).get("forward_all_to")

    target = "me@elsewhere.example"
    ms.set_forward_all_to(target)
    assert _wait(lambda: stored() == target), (
        f"the page's forward-all address never reached the account (reads {stored()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )
    assert _wait(lambda: ms.forward_all_to() == target), (
        f"the field must show the address the account kept; shows {ms.forward_all_to()!r}"
    )

    # A typo is refused on the page, before anything is sent.
    ms.set_forward_all_to("me@nodot")
    assert _wait(lambda: ms.page_error_text(timeout=1.0) == S.mail_settings.forward_all_to_invalid), (
        f"a malformed address must say so; error-message reads {ms.page_error_text(timeout=1.0)!r}"
    )
    assert stored() == target, "a refused address must not replace the one already set"

    # An address this nest hosts is an alias, not a forward — the nest refuses
    # it, and the page says what to do instead.
    ms.set_forward_all_to(f"me@{_HOSTED_DOMAIN}")
    assert _wait(
        lambda: ms.page_error_text(timeout=1.0) == S.error.bridges.forward_target_on_local_domain
    ), (
        f"a hosted address must point at aliases; error-message reads "
        f"{ms.page_error_text(timeout=1.0)!r}"
    )
    assert stored() == target, "a refused address must not replace the one already set"

    # A later visit shows the address the account forwards to.
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    ms.navigate()
    assert _wait(lambda: ms.forward_all_to() == target), (
        f"a fresh visit must show the stored address; shows {ms.forward_all_to()!r}"
    )

    # Clearing the field stops forwarding.
    ms.set_forward_all_to("")
    assert _wait(lambda: stored() is None), (
        f"clearing the field must stop forwarding (reads {stored()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )
    assert ms.forward_all_to() == ""


@pytest.mark.feature("mail-filter-rules")
def test_your_own_hourly_forwarding_limit_is_set_from_settings(
    logged_in_app, nest_instance, test_user, _forwarding_reset
):
    """Outcome 10: the person sets their own hourly forwarding limit. It starts
    at the default, takes any whole number from 1 up to the ceiling the nest
    reports, and a value outside that range is refused on the page with the
    reason, the stored limit unchanged."""
    app = logged_in_app
    ms = app.mail_settings
    ms.navigate()
    ms.ensure_mail_enabled()

    def stored():
        with _user_client(nest_instance, test_user) as api:
            return api.call("fauna.bridges.get_forward_per_hour", {})

    ceiling = stored()["forward_per_hour_ceiling"]
    assert _wait(lambda: ms.forward_per_hour() == "100"), (
        f"the field must start at the default limit; shows {ms.forward_per_hour()!r}"
    )

    ms.set_forward_per_hour("40")
    assert _wait(lambda: stored()["forward_per_hour"] == 40), (
        f"the page's limit never reached the account (reads {stored()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )
    assert _wait(lambda: ms.forward_per_hour() == "40")

    for refused, reason in [
        ("0", S.mail_settings.forward_per_hour_zero),
        (str(ceiling + 1), S.mail_settings.forward_per_hour_above_ceiling(ceiling=str(ceiling))),
        ("lots", S.mail_settings.forward_per_hour_not_a_number),
    ]:
        ms.set_forward_per_hour(refused)
        assert _wait(lambda: ms.page_error_text(timeout=1.0) == reason), (
            f"{refused!r} must be refused with {reason!r}; error-message reads "
            f"{ms.page_error_text(timeout=1.0)!r}"
        )
        assert stored()["forward_per_hour"] == 40, (
            f"a refused limit ({refused!r}) must leave the stored one alone"
        )

    # The ceiling itself is allowed.
    ms.set_forward_per_hour(str(ceiling))
    assert _wait(lambda: stored()["forward_per_hour"] == ceiling), (
        f"the ceiling itself is a valid limit (reads {stored()!r}; "
        f"error: {ms.page_error_text(timeout=1.0)!r})"
    )

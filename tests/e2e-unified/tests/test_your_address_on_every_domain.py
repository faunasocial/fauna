"""tier_3 app e2e: one handle, found and joined under every domain the nest hosts.

`docs/features/your-address-on-every-domain.md`, the two ``[app]`` outcomes
(its ``[nest]`` halves, mail in and mail out, are
``tests/api/test_mail_handle_every_domain.py``):

* **2** — someone who looks you up at another of your nest's domains finds you
  (`mail-multidomain.md` § The model: one handle, addressable on every active
  domain). Contacts → Find User with ``handle@<secondary domain>``: the shared
  ``find_user_across_nests`` probes the home nest with the domain as the
  multi-domain qualifier, the nest echoes it because it hosts it, and the
  lookup resolves on the home nest — no peer dial.
* **3** — you can join having reached your nest by any of its domains
  (§ Registration). The join wizard signs the registration over the typed
  handle's ``@domain`` (``fauna-onboarding-machine``'s redeem path), and the
  nest accepts a signature over any active local domain, not only its primary.

The secondary domain is fixture state: ``seed_local_domain`` is the admin's
``fauna.bridges.add_local_domain``, added after the primary is pinned so it
lands as a secondary (the first active domain becomes the primary).
"""

from __future__ import annotations

import json as _json
import secrets
import time

import pytest
from nacl.signing import SigningKey

from common.auth import create_actor_and_register
from conftest import MAIL_PRIMARY_DOMAIN, MAIL_SECOND_DOMAIN
from helpers import budgets
from helpers.admin_wire import admin_client, seed_local_domain
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until
from i18n.strings import S

# The join route's dedicated open-posture nest and its admin's invite mint, as
# the join-a-nest journeys use them.
from tests.test_trust_prompt import (  # noqa: F401 — `joiner_nest` is a fixture
    _mint_invite_code,
    _require_the_prompt,
    _wait_for_the_app,
    joiner_nest,
)

pytestmark = pytest.mark.tier_3


def _host_a_second_domain(nest) -> None:
    """Make ``MAIL_SECOND_DOMAIN`` an active SECONDARY domain of ``nest``.
    Both adds are idempotent; the primary goes first so the second can never
    take the primary slot."""
    seed_local_domain(nest, MAIL_PRIMARY_DOMAIN)
    seed_local_domain(nest, MAIL_SECOND_DOMAIN)
    with admin_client(nest) as client:
        rows = client.call("fauna.bridges.list_local_domains", {}).get("active", [])
    by_name = {row.get("domain_name"): row for row in rows}
    assert MAIL_SECOND_DOMAIN in by_name and not by_name[MAIL_SECOND_DOMAIN].get("is_primary"), (
        f"control: {MAIL_SECOND_DOMAIN} must be an active secondary domain: {rows!r}"
    )


@pytest.mark.feature("your-address-on-every-domain")
def test_a_lookup_at_another_of_the_nests_domains_finds_you(logged_in_app, nest_instance):
    app = logged_in_app
    _host_a_second_domain(nest_instance)
    target = create_actor_and_register(
        nest_instance["port"], base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    assert target["handle"], "control: the looked-up actor needs a handle"

    app.contacts.navigate()
    app.contacts.find_by_handle(f"{target['handle']}@{MAIL_SECOND_DOMAIN}")
    resolved = wait_until(
        lambda: app.contacts.actor_id_result_text() or None,
        budgets.CROSS_NEST_S,
        diagnose=lambda: (
            f"a lookup of {target['handle']}@{MAIL_SECOND_DOMAIN}, a domain this "
            "nest hosts, found nobody: find-error="
            f"{app.contacts.find_error_text()!r} "
            f"{app.driver.diagnose('contact-actor-id-result')}"
        ),
    )
    assert resolved == target["actor_id_hex"], (
        f"{target['handle']}@{MAIL_SECOND_DOMAIN} must resolve to the one actor that "
        f"holds the handle, {target['actor_id_hex']}; got {resolved!r}"
    )


def _redeem_an_invite_as(app, nest, handle: str) -> None:
    """Join ``nest`` through the wizard's invite page with a freshly minted
    code, the handle typed as ``handle``, and press Continue."""
    from conftest import _relaunch_trusting_nest

    code = _mint_invite_code(nest)
    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(bytes(SigningKey.generate()).hex())
    app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)
    # Handle discovery is skipped as in the join-a-nest journeys (a test box's
    # domains resolve nowhere); the handle the wizard signs over is the one
    # typed by the caller.
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        _json.dumps([nest["dial_url"], handle]),
    )
    app.driver.wait_for("invite-code-input", timeout=RPC_ROUNDTRIP_S)
    app.driver.clear_and_type("invite-code-input", code)
    app.driver.click("invite-code-check-button")
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline and not app.driver.is_enabled(
        "invite-request-continue-button"
    ):
        time.sleep(0.5)
    assert app.driver.is_enabled("invite-request-continue-button"), (
        "the invite code never validated: "
        f"{app.driver.get_text('invite-code-status')!r}; error: {app.error_text()!r}"
    )
    app.driver.click("invite-request-continue-button")


@pytest.mark.feature("your-address-on-every-domain")
def test_you_can_join_having_reached_the_nest_by_another_of_its_domains(app, joiner_nest):
    _host_a_second_domain(joiner_nest)
    localpart = "joiner" + secrets.token_hex(3)
    _redeem_an_invite_as(app, joiner_nest, f"{localpart}@{MAIL_SECOND_DOMAIN}")

    # The trust offer follows a register the nest accepted; a signature over a
    # domain it does not accept would stop here with the refusal on screen.
    _require_the_prompt(app, leaving="the invite redemption under a secondary domain")
    assert app.onboarding.finish_trust_prompt(grant=False)
    _wait_for_the_app(app)
    assert not app.has_error(), f"joining surfaced an error: {app.error_text()!r}"

    with admin_client(joiner_nest) as client:
        handles = [u.get("handle") for u in client.users_list_all()]
    assert localpart in handles, (
        f"the nest holds no account under the bare handle {localpart!r}: {handles!r}"
    )


def test_joining_under_a_domain_the_nest_does_not_host_says_why(app, joiner_nest):
    """The red check of outcome 3: a handle under a domain the nest does not
    host is refused (the nest verifies the registration's signature only over
    its own domains, so it answers ``signature_failed``), and the invite page
    says so on ``error-message`` (convention 2) — in words, not the wire code.
    Before the fix the refusal reached only the status line, under a fixed
    "redeem failed", and ``error-message`` stayed empty."""
    localpart = "joiner" + secrets.token_hex(3)
    _redeem_an_invite_as(app, joiner_nest, f"{localpart}@unhosted.test")

    expected = S.onboarding.invite.error.domain_not_hosted
    shown = wait_until(
        lambda: app.error_text() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the refused redeem left error-message empty; status line: "
            f"{app.driver.get_text('invite-request-status')!r}"
        ),
    )
    assert shown == expected, (
        f"error-message must name the refusal: expected {expected!r}, got {shown!r}"
    )
    assert "fauna.account" not in shown, f"a wire code reached the screen: {shown!r}"

    with admin_client(joiner_nest) as client:
        handles = [u.get("handle") for u in client.users_list_all()]
    assert localpart not in handles, (
        f"control: the refused join must create no account: {handles!r}"
    )

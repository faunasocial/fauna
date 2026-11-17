"""The choose-providers page — `docs/goal/behavior/onboarding-provisioning.md`
§ 3d (ratified 2026-11-15, second and third rounds 2026-11-16).

One page between the handle check and the setting-things-up page: the goals of
the set-up at the top, the providers below, each opened in its own dialog whose
*Connect* runs every check of the provider's capabilities with one credential
set; Continue goes straight to ``nest_provisioning``, whose choices region
carries the server's location and type (each with a default), the mail mode,
the update channel and the SSH door.

Journeys (every mutation a driver UI action, convention 8; the handle-check
OUTCOME is seeded through the sanctioned test setter so no journey depends on
outbound DNS):

* one provider covering DNS and the server — reaches ``nest_provisioning``
  with a location and a server type already chosen, *Buy and set up* live with
  no further input;
* the manual DNS cover — a provider that does not hold the domain covers the
  server only, and the manual row covers DNS;
* two providers and the goal choice — both cover both goals, the page offers
  the choice, and the pick moves the cover.

The domain-buying journey (registration, DNS and a server through the bundled
provider, the price confirm on ``nest_provisioning`` gating the start, the real
orchestrator end to end) is ``test_bundled_provider.py``, which walks this page
on an app that renders it. Whether the manual row is refused while a domain is
bought is today's model and round 3 retires it, so no journey here pins it.

tui leads (`testing.md` § Default app and nest mode); the other six reach the
page in their trickle-down, gated by the ``provider-choice`` surface.
"""
from __future__ import annotations

import json

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import require_surface
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_2

# The zone both fake DNS legs (Hetzner, and the bundled intermediary once
# seeded) report.
HELD_DOMAIN = "example.test"


def _land_on_the_page(app, handle: str) -> None:
    """Seed a RegisteredNoNest outcome with the control box ticked — a domain
    the person brings — and press the handle check's Continue."""
    domain = handle.split("@", 1)[1]
    app.onboarding.go_to_handle_entry()
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": "RegisteredNoNest",
        "message": {
            "key": "onboarding.handle_check.outcome.registered_no_nest",
            "args": {"domain": domain},
        },
        "continue_enabled": True,
        "control_checkbox_visible": True,
        "control_checkbox_checked": True,
    }, handle=handle)
    wait_until(lambda: app.is_enabled("handle-entry-continue-button"), 10.0,
               diagnose=lambda: app.driver.diagnose("handle-entry-continue-button"))
    app.driver.click("handle-entry-continue-button")
    app.wait_for("provider-choice-continue-button")


def _connect_hetzner(app) -> None:
    drv = app.driver
    drv.click("provider-row[hetzner]")
    app.wait_for("provider-dialog")
    drv.type_text("provider-credentials-form-api-token", "MOCK")
    drv.wait_until_enabled("provider-connect-button", timeout=10)
    drv.click("provider-connect-button")
    wait_until(
        lambda: drv.get_attr("provider-row[hetzner]", "state") == "connected",
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            "hetzner never read connected: dialog status="
            f"{drv.get_text('provider-dialog-status-text')!r} error={app.error_text()!r} "
            + drv.diagnose("provider-row[hetzner]")
        ),
    )
    drv.click("provider-dialog-close-button")


def _cover(app, goal: str) -> str | None:
    return app.driver.get_attr(f"provider-goal-row[{goal}]", "provider")


def _hetzner_urls(fake_cloud) -> dict[str, str]:
    return {
        "dns": fake_cloud.dns_base_for("hetzner"),
        "vps": fake_cloud.url_map()["vps"],
    }


def test_one_provider_covers_dns_and_the_server_and_starts_with_no_further_input(
    app, fake_cloud
):
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    drv.set_provider_base_urls(_hetzner_urls(fake_cloud))
    _land_on_the_page(app, f"alice@{HELD_DOMAIN}")

    # At entry nothing is connected: the goals are open, Continue is dead and
    # the status line names what is still open (rule 5).
    assert drv.get_attr("provider-goal-row[dns]", "state") == "open"
    assert not app.is_enabled("provider-choice-continue-button")
    assert drv.get_text("provider-choice-status-text").strip(), (
        "a dead Continue owes a reason: " + drv.diagnose("provider-choice-status-text")
    )
    # A registered domain cannot be bought: no buy box, no registration goal.
    assert drv.is_absent("dns-buy-domain-checkbox")
    assert drv.is_absent("provider-goal-row[domain]")

    _connect_hetzner(app)
    assert _cover(app, "dns") == "hetzner", drv.diagnose("provider-goal-row[dns]")
    assert _cover(app, "vps") == "hetzner", drv.diagnose("provider-goal-row[vps]")
    assert drv.is_absent("provider-goal-select[dns]"), "one candidate, no choice"
    drv.wait_until_enabled("provider-choice-continue-button", timeout=10)
    drv.click("provider-choice-continue-button")

    # nest_provisioning: the choices region with every default already set.
    app.wait_for("provisioning-start-button")
    assert drv.get_text("vps-location-picker").strip(), drv.diagnose("vps-location-picker")
    chosen = [
        i for i in range(6)
        if drv.is_visible(f"vps-server-type-radio[{i}]")
        and drv.get_attr(f"vps-server-type-radio[{i}]", "state") == "on"
    ]
    assert len(chosen) == 1, f"exactly one server type is pre-selected; got {chosen}"
    assert drv.is_visible("vps-config-mail-mode-toggle")
    assert drv.is_visible("vps-config-ssh-door-toggle")
    assert drv.is_absent("dns-price-confirm-checkbox"), "nothing is bought"
    assert app.is_enabled("provisioning-start-button"), (
        "connect, Continue, Buy and set up: the start needs no further input; "
        f"error={app.error_text()!r} " + drv.diagnose("provisioning-start-button")
    )

    # Back returns to the page on this path, the provider still connected.
    drv.click("provisioning-back-button")
    app.wait_for("provider-choice-continue-button")
    assert drv.get_attr("provider-row[hetzner]", "state") == "connected"


def test_the_manual_row_covers_dns_beside_a_server_provider(app, fake_cloud):
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    drv.set_provider_base_urls(_hetzner_urls(fake_cloud))
    # The Hetzner fake holds only example.test: for another domain its row
    # covers the server and says the account does not hold the domain.
    _land_on_the_page(app, "alice@elsewhere.test")
    _connect_hetzner(app)
    assert _cover(app, "vps") == "hetzner"
    assert drv.get_attr("provider-goal-row[dns]", "state") == "open"
    assert not app.is_enabled("provider-choice-continue-button")

    drv.click("provider-row[manual]")
    wait_until(
        lambda: _cover(app, "dns") == "manual",
        10.0,
        diagnose=lambda: drv.diagnose("provider-goal-row[dns]"),
    )
    drv.wait_until_enabled("provider-choice-continue-button", timeout=10)
    drv.click("provider-choice-continue-button")
    app.wait_for("provisioning-start-button")
    assert drv.is_absent("provisioning-bom-domain-line")
    assert app.is_enabled("provisioning-start-button"), (
        f"error={app.error_text()!r} " + drv.diagnose("provisioning-start-button")
    )


def test_two_providers_offer_the_goal_choice(app, fake_cloud):
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    drv.set_provider_base_urls(_hetzner_urls(fake_cloud))
    _land_on_the_page(app, f"alice@{HELD_DOMAIN}")
    _connect_hetzner(app)

    # Fixture setup (convention 8 — the choice is the journey, not standing up
    # a second provider's API): the harness's `dns` / `vps` overrides are one
    # per kind, so a second faked provider's checks would land on Hetzner's
    # fake. The bundled provider is put into the connected set as its checks
    # would have left it — holding the zone and listing one offer.
    drv.call_machine_method("set_provider_connection_for_test", json.dumps([
        "bundled",
        {
            "creds": {"base-url": "https://bundle.invalid", "api-token": "t"},
            "connected": True,
            "held": False,
            "zones": [{"id": "z-held", "name": HELD_DOMAIN}],
            "server_checked": True,
            "locations": [{"id": "eu-1", "name": "Europe 1", "city": "Europe", "country": "DE"}],
            "server_types": [{
                "id": "small", "vcpu": 1, "mem_gb": 2.0, "disk_gb": 20,
                "price_monthly_cents": 599, "currency": "EUR",
            }],
            "listed_for_location": "eu-1",
        },
    ]))

    # Both cover both goals: each goal row offers the choice, and the
    # assignment keeps one provider for both.
    for goal in ("dns", "vps"):
        app.wait_for(f"provider-goal-select[{goal}]")
    first = _cover(app, "dns")
    assert first in {"hetzner", "bundled"} and _cover(app, "vps") == first, (
        "the fewest providers: one covers both goals; "
        + drv.diagnose("provider-goal-row[dns]") + drv.diagnose("provider-goal-row[vps]")
    )
    other = "bundled" if first == "hetzner" else "hetzner"
    drv.select("provider-goal-select[dns]", other)
    wait_until(
        lambda: _cover(app, "dns") == other,
        10.0,
        diagnose=lambda: drv.diagnose("provider-goal-row[dns]"),
    )
    # The server follows the DNS cover while the person has not chosen it.
    assert _cover(app, "vps") == other, drv.diagnose("provider-goal-row[vps]")
    assert app.is_enabled("provider-choice-continue-button")

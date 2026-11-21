"""A domain bought at one provider with its DNS at another —
`docs/goal/behavior/onboarding-provisioning.md` § 3d *Which provider covers
which goal* (cover (2): another connected DNS provider with the zone door,
beside a registration cover with the nameserver door) and § 6 *The Domain step
when DNS runs at another provider*.

The registrar is Porkbun (the nameserver door), the DNS provider and the
server Hetzner (the zone door) — `registry.md` § Delegation doors. On the
choose-providers page the DNS goal starts on the registrar and offers Hetzner;
the person picks Hetzner, and the run registers the domain at Porkbun, creates
its zone at Hetzner, points the domain at the zone's assigned nameservers,
publishes the records into the new zone, builds the box and claims it.

Journeys:

* the whole run — the zone exists at Hetzner, Porkbun holds exactly the zone's
  nameservers, the records are in the new zone, the run ends claimed;
* the zone create refused once — the Domain step fails with Hetzner's own
  error, Retry runs the step from the top, and the domain is not registered a
  second time (Porkbun's fake refuses a second create of a domain it sold).

**Fixture setup, not journey (convention 8).** Porkbun is put into the
connected set as its *Connect* would have left it — its quote read off the
Porkbun fake's own price — through the sanctioned test setter, as
``test_provider_choice.py``'s goal-choice journey does for its second provider:
the harness's ``dns`` override is one per kind, so Porkbun's DNS check would
land on Hetzner's fake. Its registrar calls reach the Porkbun fake through the
registrar's ``api-base`` credential, which the seeded credential carries.
Hetzner is connected through the dialog, and every mutation of the journey —
the goal pick, Continue, the price box, *Buy and set up*, Retry — goes
through the UI.

tier_3: the run's nest leg is a REAL never-claimed nest
(``provision_target_nest``) — on the standard path the ``Online`` step ends by
claiming the box over WS-RPC, which no fake serves (see
``test_bundled_provider.py``'s override comment).

tui leads (`testing.md` § Default app and nest mode); the other six reach the
page in their trickle-down, gated by the ``provider-choice`` surface.
"""
from __future__ import annotations

import json

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import require_surface
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.crash_recovery import setup_status
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

# Domains the fakes sell (never real names). One per journey: the app is
# session-cached and the wizard remembers what it registered, so a second
# journey on the first one's domain would start already registered.
WHOLE_RUN_DOMAIN = "not-a-real-fauna-e2e-split.test"
RETRY_DOMAIN = "not-a-real-fauna-e2e-split-retry.test"


def _cover(app, goal: str) -> str | None:
    return app.driver.get_attr(f"provider-goal-row[{goal}]", "provider")


def _to_the_start(app, fake_cloud, nest, domain: str) -> None:
    """Land on ``nest_provisioning`` with Porkbun registering ``domain``,
    Hetzner covering DNS (picked on the goal's select) and the server, and the
    price agreed — *Buy and set up* live."""
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    porkbun = fake_cloud.porkbun

    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(drv, nest)
    urls = {
        "dns": fake_cloud.dns_base_for("hetzner"),
        "vps": fake_cloud.url_map()["vps"],
        "nest": nest["url"],
    }
    drv.set_provider_base_urls(urls)
    applied = drv.call_machine_method("provider_base_url", json.dumps("dns"))
    assert applied == urls["dns"], (
        f"the dns override never reached the machine: {applied!r} != {urls['dns']!r}"
    )
    # The stand-in box already knows its claim code; the run presents that one.
    drv.call_machine_method(
        "set_provision_claim_code_for_test", json.dumps(nest["claim_code"])
    )

    app.onboarding.go_to_handle_entry()
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"DomainAvailable": {"buyable_via_provider": True, "price": None}},
        "message": {
            "key": "onboarding.handle_check.outcome.domain_available_unpriced",
            "args": {"domain": domain},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    }, handle=f"alice@{domain}")
    wait_until(lambda: app.is_enabled("handle-entry-continue-button"), 10.0,
               diagnose=lambda: drv.diagnose("handle-entry-continue-button"))
    drv.click("handle-entry-continue-button")
    app.wait_for("provider-choice-continue-button")

    # Fixture setup: Porkbun connected as its checks would have left it, its
    # quote the Porkbun fake's own (module docstring).
    drv.call_machine_method("set_provider_connection_for_test", json.dumps([
        "porkbun",
        {
            "creds": {
                "api-key": "pk1_e2e",
                "secret-api-key": "sk1_e2e",
                "api-base": porkbun.base_url(),
            },
            "connected": True,
            "held": False,
            "error": None,
            "zones": [],
            "availability": {"Buyable": {
                "price_cents": porkbun.registration_cents,
                "currency": "USD",
                "renewal_cents": porkbun.renewal_cents,
            }},
            "contact": None,
            "server_checked": False,
            "locations": [],
            "server_types": [],
            "listed_for_location": None,
        },
    ]))

    # Hetzner through its dialog: the DNS and server checks against the fake.
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

    # § 3d: the registrar registers; the DNS goal starts on the registrar
    # (fewest providers) and offers Hetzner, which holds the zone door.
    wait_until(
        lambda: _cover(app, "domain") == "porkbun",
        10.0,
        diagnose=lambda: drv.diagnose("provider-goal-row[domain]"),
    )
    assert _cover(app, "dns") == "porkbun", drv.diagnose("provider-goal-row[dns]")
    assert _cover(app, "vps") == "hetzner", drv.diagnose("provider-goal-row[vps]")
    app.wait_for("provider-goal-select[dns]")
    drv.select("provider-goal-select[dns]", "hetzner")
    wait_until(
        lambda: _cover(app, "dns") == "hetzner",
        10.0,
        diagnose=lambda: drv.diagnose("provider-goal-row[dns]"),
    )
    assert _cover(app, "domain") == "porkbun", (
        "the registration stays at the registrar when DNS moves: "
        + drv.diagnose("provider-goal-row[domain]")
    )
    wait_until(lambda: app.is_enabled("provider-choice-continue-button"), 10.0,
               diagnose=lambda: "Continue stayed disabled: "
               + drv.get_text("provider-choice-status-text"))
    drv.click("provider-choice-continue-button")

    # nest_provisioning: the run registers, so its price is agreed here.
    app.wait_for("provisioning-start-button")
    assert porkbun.REGISTRATION in drv.get_text("provisioning-bom-domain-line"), (
        drv.diagnose("provisioning-bom-domain-line")
    )
    assert not app.is_enabled("provisioning-start-button"), (
        "Buy and set up waits for the price to be agreed"
    )
    drv.click("dns-price-confirm-checkbox")
    wait_until(lambda: app.is_enabled("provisioning-start-button"), 10.0,
               diagnose=lambda: drv.diagnose("provisioning-start-button")
               + f" | error={app.error_text()!r}")


def _wait_overall(app, want: str) -> dict:
    last = {"snap": None}

    def reached():
        last["snap"] = app.driver.call_machine_method("provisioning_snapshot")
        return isinstance(last["snap"], dict) and last["snap"].get("overall") == want

    wait_until(
        reached,
        ORCHESTRATION_STEP_S,
        diagnose=lambda: f"the run never reached {want}. snapshot="
        + json.dumps(last["snap"]) + f" | error={app.error_text()!r}",
    )
    return last["snap"]


def _assert_delegated_and_claimed(fake_cloud, nest, domain: str) -> None:
    porkbun, hetzner = fake_cloud.porkbun, fake_cloud.hetzner_dns
    zone = hetzner.zone_named(domain)
    assert zone is not None, f"no zone for {domain} at Hetzner: {hetzner.zones!r}"
    assert [z["name"] for z in hetzner.created_zones] == [domain]
    assert porkbun.nameservers_of(domain) == hetzner.ASSIGNED_NAMESERVERS, (
        "the registrar holds exactly the zone's assigned nameservers: "
        f"{porkbun.nameserver_updates!r}"
    )
    published = hetzner.created_rrsets(zone["id"])
    assert any(r["type"] == "A" and r["name"] == "@" for r in published), (
        f"the records go into the new zone: {published!r}"
    )
    st = setup_status(nest["url"])
    assert st.get("claimed") is True and st.get("admin_exists") is True, (
        f"Succeeded on the standard path means built AND claimed: {st!r}"
    )


def test_buying_at_one_provider_with_dns_at_another_delegates_and_claims(
    app, fake_cloud, provision_target_nest
):
    nest = provision_target_nest
    porkbun = fake_cloud.porkbun
    _to_the_start(app, fake_cloud, nest, WHOLE_RUN_DOMAIN)

    app.driver.click("provisioning-start-button")
    _wait_overall(app, "Succeeded")

    assert porkbun.registered == [
        {"domain": WHOLE_RUN_DOMAIN, "cost": porkbun.registration_cents}
    ], "registered once, at the agreed price"
    _assert_delegated_and_claimed(fake_cloud, nest, WHOLE_RUN_DOMAIN)


def test_a_refused_zone_create_fails_the_domain_step_and_retry_does_not_register_twice(
    app, fake_cloud, provision_target_nest
):
    nest = provision_target_nest
    drv = app.driver
    porkbun, hetzner = fake_cloud.porkbun, fake_cloud.hetzner_dns
    _to_the_start(app, fake_cloud, nest, RETRY_DOMAIN)
    hetzner.refuse_zone_creates = 1

    drv.click("provisioning-start-button")
    _wait_overall(app, "Failed")

    # The Domain step failed with the provider's own words, after the purchase.
    err = drv.get_text("provisioning-step-error", scope="provisioning-step-row[0]")
    assert hetzner.ZONE_CREATE_REFUSAL in err, (
        "the step shows the provider's error: "
        + drv.diagnose("provisioning-step-error", scope="provisioning-step-row[0]")
    )
    assert [r["domain"] for r in porkbun.registered] == [RETRY_DOMAIN]
    assert hetzner.zone_named(RETRY_DOMAIN) is None
    assert porkbun.nameservers_of(RETRY_DOMAIN) is None, "never pointed at a zone that is not there"

    app.wait_for("provisioning-retry-button")
    drv.click("provisioning-retry-button")
    _wait_overall(app, "Succeeded")

    assert [r["domain"] for r in porkbun.registered] == [RETRY_DOMAIN], (
        f"Retry must not register the domain a second time: {porkbun.registered!r} "
        f"calls={porkbun.calls!r}"
    )
    _assert_delegated_and_claimed(fake_cloud, nest, RETRY_DOMAIN)

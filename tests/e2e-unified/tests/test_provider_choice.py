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
on an app that renders it. Buying with the manual row covering DNS (§ 3d third
round, § 6 *Deferred-DNS path*) is journeyed here: the bundled provider
registers and serves, the run registers the domain and publishes nothing, and
the price box gates the start as on any buying run. Buying at one provider
with DNS at another (§ 6 *The Domain step when DNS runs at another provider*)
is ``test_provider_choice_split_domain.py`` (tier_3: its run ends claimed
against a real nest).

tui leads (`testing.md` § Default app and nest mode); the other six reach the
page in their trickle-down, gated by the ``provider-choice`` surface.
"""
from __future__ import annotations

import json

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import require_surface
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.provisioning_drive import (
    HANDLE_DOMAIN,
    point_providers_at,
    seed_standard_path_run,
)
from helpers.waiting import wait_until
from i18n.strings import S

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



# A domain the bundled fake can register (never a real name).
BUY_DOMAIN = "not-a-real-fauna-e2e-domain.test"

BUY_CONTACT = {
    "first_name": "Test",
    "last_name": "User",
    "email": "hello@example.test",
    "phone": "+1.5555550100",
    "address1": "1 Example Way",
    "city": "Exampleton",
    "state": "EX",
    "postal_code": "00000",
    "country": "US",
}


def _buy_through_the_bundled_provider(
    app, fake_cloud, domain: str, *, dns_cover: str, on_handle_entry: bool = False
) -> None:
    """Walk the choose-providers page for a domain the person is buying: the
    bundled provider registers it and serves the server, ``dns_cover`` covers
    its DNS (``"manual"`` — the manual row, clicked here — or ``"bundled"``,
    the registrar's own DNS, which covers it with nothing clicked); Continue
    lands on ``nest_provisioning``, where the caller takes over.
    ``on_handle_entry``: the caller already stands on ``handle_entry`` (walked
    back to it), so the walk starts there instead of at identity choice."""
    drv = app.driver
    bundled = fake_cloud.bundled
    # Every call goes to the address the user TYPES: an earlier journey's
    # `dns`/`vps` override (the machine keeps it) would send the bundled
    # provider's checks to another provider's fake.
    drv.set_provider_base_urls({})

    if not on_handle_entry:
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

    # ── the choose-providers page: bundled registers and serves ──
    app.wait_for("dns-buy-domain-checkbox")
    drv.click("provider-row[bundled]")
    app.wait_for("provider-credentials-form-base-url")
    drv.fill("provider-credentials-form-base-url", bundled.base_url())
    app.wait_for("provider-credentials-form-api-token")
    drv.click("provider-credentials-form-api-token")
    wait_until(
        lambda: drv.get_text("provider-credentials-form-api-token").strip() == "Connected",
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "hosted-auth never reached Connected: "
        + drv.get_text("provider-credentials-form-api-token"),
    )
    drv.wait_until_enabled("provider-connect-button", timeout=10)
    drv.click("provider-connect-button")
    wait_until(
        lambda: drv.get_attr("provider-row[bundled]", "state") == "connected",
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            f"bundled never connected: {drv.get_text('provider-dialog-status-text')!r}"
            f" | error={app.error_text()!r}"
        ),
    )
    app.wait_for("dns-contact-form")
    app.onboarding.fill_dns_contact(BUY_CONTACT)
    drv.click("provider-dialog-close-button")
    if dns_cover == "manual":
        drv.click("provider-row[manual]")
    wait_until(
        lambda: _cover(app, "dns") == dns_cover,
        10.0,
        diagnose=lambda: drv.diagnose("provider-goal-row[dns]"),
    )
    assert _cover(app, "domain") == "bundled", drv.diagnose("provider-goal-row[domain]")
    assert _cover(app, "vps") == "bundled", drv.diagnose("provider-goal-row[vps]")
    wait_until(lambda: app.is_enabled("provider-choice-continue-button"), 10.0,
               diagnose=lambda: "Continue stayed disabled: "
               + drv.get_text("provider-choice-status-text"))
    drv.click("provider-choice-continue-button")
    app.wait_for("provisioning-start-button")


def test_buying_with_dns_left_for_later_registers_and_publishes_nothing(app, fake_cloud):
    """§ 3d's manual row covers DNS while a domain is bought, and § 6
    *Deferred-DNS path*: the run registers the domain and publishes nothing;
    ``nest_provisioning`` carries the domain line and the price box as on any
    buying run, and *Buy and set up* waits on the box. The run needs no nest
    leg: the deferred path has no ``Online`` step, so nothing is claimed."""
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    bundled = fake_cloud.bundled
    _buy_through_the_bundled_provider(app, fake_cloud, BUY_DOMAIN, dns_cover="manual")

    # ── nest_provisioning: the line, the box, and the start waiting on it ──
    domain_line = drv.get_text("provisioning-bom-domain-line")
    assert "10.99" in domain_line and "14.99" in domain_line, domain_line
    assert drv.is_visible_scrolled("dns-price-confirm-checkbox"), (
        "the run registers, so its price is agreed here: "
        + drv.diagnose("dns-price-confirm-checkbox")
    )
    assert not app.is_enabled("provisioning-start-button"), (
        "Buy and set up waits for the price to be agreed"
    )
    drv.click("dns-price-confirm-checkbox")
    wait_until(lambda: app.is_enabled("provisioning-start-button"), 10.0,
               diagnose=lambda: drv.diagnose("provisioning-start-button")
               + f" | error={app.error_text()!r}")

    drv.click("provisioning-start-button")
    last = {"snap": None}

    def succeeded():
        last["snap"] = drv.call_machine_method("provisioning_snapshot")
        return isinstance(last["snap"], dict) and last["snap"].get("overall") == "Succeeded"

    wait_until(
        succeeded,
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "the deferred run never reached Succeeded. snapshot="
        + json.dumps(last["snap"]) + f" | error={app.error_text()!r}",
    )

    # ── what the intermediary saw: a registration and a server, no records ──
    assert [r["name"] for r in bundled.registered] == [BUY_DOMAIN]
    assert bundled.registered[0]["agreed_price_cents"] == bundled.registration_cents
    assert len(bundled.servers) == 1
    assert not any(bundled.records.values()), (
        f"the manual cover publishes nothing: {bundled.records!r}"
    )


# The DNS plan (§ 6 *The DNS plan*). The records an earlier box left in the
# Hetzner fake's `example.test` zone: its apex address and IPv6 address, and an
# MX that already names the run's own mail host.
OLD_BOX_V4 = "198.51.100.7"
OLD_BOX_V6 = "2001:db8::7"
# A domain the bundled fake can register, for the plan's bought-domain half —
# never the buying journey's, so neither journey finds the other's registration.
PLAN_BUY_DOMAIN = "not-a-real-fauna-e2e-dns-plan.test"


def _plan_row(app, key: str) -> str:
    """The text of ``provisioning-dns-plan-row[<key>]`` once it has painted."""
    row = f"provisioning-dns-plan-row[{key}]"
    wait_until(
        lambda: app.is_visible(row),
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            f"{row} never painted. plan="
            + json.dumps(app.driver.call_machine_method("dns_plan"))
            + " | " + app.driver.diagnose("provisioning-dns-plan")
        ),
    )
    return app.driver.get_text(row)


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_the_dns_plan_names_what_the_run_keeps_changes_and_removes_and_a_bought_domain_has_none(
    app, fake_cloud
):
    """§ 6 *The DNS plan*: on the provider path with an existing zone the page
    lists, before *Buy and set up*, every record the run will publish and what
    it does to each — read from the zone by the machine itself as it enters
    the page (no app hook): the earlier box's apex ``A`` is changed to the new
    server's address, its ``AAAA`` removed, the MX already right is kept, and a
    record the zone lacks is added. A bought domain's fresh zone has nothing to
    keep or change, so the region is absent there — asserted after the plan
    above painted, so the absence is the machine hiding it on entry, not a page
    that never had one."""
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    zone = fake_cloud.hetzner_dns
    zone.seed_record("@", "A", OLD_BOX_V4)
    zone.seed_record("@", "AAAA", OLD_BOX_V6)
    zone.seed_record("@", "MX", f"10 mail.{HELD_DOMAIN}.")

    # ── an existing zone: Hetzner holds example.test and covers both goals ──
    drv.set_provider_base_urls(_hetzner_urls(fake_cloud))
    _land_on_the_page(app, f"alice@{HELD_DOMAIN}")
    _connect_hetzner(app)
    drv.wait_until_enabled("provider-choice-continue-button", timeout=10)
    drv.click("provider-choice-continue-button")
    app.wait_for("provisioning-start-button")

    strings = S.onboarding.nest_provisioning
    changed = _plan_row(app, "@|A")
    assert changed.strip() == strings.dns_plan_row_change(
        name="@", record_type="A", old=OLD_BOX_V4, value=strings.dns_plan_new_server,
    ), f"the earlier box's address is changed to the new server's: {changed!r}"
    removed = _plan_row(app, "@|AAAA")
    assert removed.strip() == strings.dns_plan_row_remove(
        name="@", record_type="AAAA", old=OLD_BOX_V6,
    ), f"the earlier box's IPv6 address is removed: {removed!r}"
    kept = _plan_row(app, "@|MX")
    keep_tail = strings.dns_plan_row_keep(name="@", record_type="MX", value="<v>").split("<v>", 1)[1]
    assert kept.strip().startswith("@ MX ") and kept.strip().endswith(keep_tail), (
        f"the MX already naming the run's mail host is kept: {kept!r}"
    )
    added = _plan_row(app, "mail|A")
    assert added.strip() == strings.dns_plan_row_add(
        name="mail", record_type="A", value=strings.dns_plan_new_server,
    ), f"a record the zone lacks is added: {added!r}"
    heading = drv.get_text("provisioning-dns-plan")
    assert strings.dns_plan_heading(domain=HELD_DOMAIN) in heading, (
        f"the region names the domain it read: {heading!r}"
    )
    assert zone.removed_records == [] and zone.added_records == [], (
        "the plan is shown before the run: nothing in the zone is touched yet"
    )

    # ── a bought domain: the registrar's own DNS covers it, no region ──
    # Back through the pages, not a reset: the machine keeps the plan painted
    # above, so the region's absence below is the entry hiding it.
    drv.click("provisioning-back-button")
    app.wait_for("provider-choice-back-button")
    drv.click("provider-choice-back-button")
    app.onboarding.wait_for_handle_entry()
    _buy_through_the_bundled_provider(
        app, fake_cloud, PLAN_BUY_DOMAIN, dns_cover="bundled", on_handle_entry=True
    )
    wait_until(
        lambda: drv.is_absent("provisioning-dns-plan"),
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            "a bought domain's fresh zone has nothing to keep or change, so the "
            "plan must be absent; plan="
            + json.dumps(drv.call_machine_method("dns_plan"))
            + " | " + drv.diagnose("provisioning-dns-plan")
        ),
    )
    assert drv.is_absent("provisioning-dns-plan-row[@|A]"), (
        drv.diagnose("provisioning-dns-plan-row[@|A]")
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_the_dns_plan_paints_after_the_server_choices_continue(app, fake_cloud):
    """§ 6 *The DNS plan* on the server-choices path: ``vps_config``'s Continue
    (``continue_from_vps``) is the other transition into ``nest_provisioning``,
    and the machine reads the zone itself there too, so the page paints the
    region and its keyed rows with no app hook. The windows wizard walks this
    path rather than the choose-providers page, so this is the region's witness
    there; every app takes the same transition. The run's ``A`` at the apex
    points at a server that does not exist yet, so its row names *the new
    server's address*, whether the zone held an older value or none.

    The plan reads the zone the DNS verify picked (``refresh_dns_plan`` hides
    the region with no ``zone_id``), and the seed stores the DNS credential
    without verifying it, so the journey verifies it as the DNS page's Verify
    does: the fake's ``GET /zones`` answers the zone holding the handle's
    domain.

    The seeded DNS provider is Cloudflare, whose records are written fully
    qualified (``record_names_relative_to_zone`` is false), yet the plan names
    them zone-relative whatever the provider (onboarding-provisioning.md § 6
    *The DNS plan*), so the apex row keys ``@|A`` here as under the Hetzner of
    the choose-providers witness."""
    drv = app.driver
    point_providers_at(app, fake_cloud)
    seed_standard_path_run(app)
    drv.call_machine_method("verify_dns")
    drv.call_machine_method("set_step_for_test", json.dumps("VpsConfig"))
    app.wait_for("vps-config-continue-button")
    wait_until(lambda: app.is_enabled("vps-config-continue-button"), ORCHESTRATION_STEP_S,
               diagnose=lambda: drv.diagnose("vps-config-continue-button"))
    drv.click("vps-config-continue-button")
    app.wait_for("provisioning-start-button")

    strings = S.onboarding.nest_provisioning
    row = "provisioning-dns-plan-row[@|A]"
    wait_until(
        lambda: drv.is_visible_scrolled(row),
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            f"{row} never painted after the server choices' Continue. plan="
            + json.dumps(drv.call_machine_method("dns_plan"))
            + f" | page error: {app.error_text()!r}"
            + " | " + drv.diagnose("provisioning-dns-plan")
        ),
    )
    apex = drv.get_text(row).strip()
    assert apex.startswith("@ A ") and strings.dns_plan_new_server in apex, (
        f"the apex A row names the new server's address: {apex!r}"
    )
    heading = drv.get_text("provisioning-dns-plan")
    assert strings.dns_plan_heading(domain=HANDLE_DOMAIN) in heading, (
        f"the region names the domain it read: {heading!r}"
    )

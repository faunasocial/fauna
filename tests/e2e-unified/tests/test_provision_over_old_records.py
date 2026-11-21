"""Setting up a nest on a domain whose zone still holds a deleted box's
records — `docs/goal/behavior/onboarding.md` § 2 *A public domain whose
address answers nothing* and `docs/goal/behavior/onboarding-provisioning.md`
§ 6 *The DNS plan*.

The handle check finds the domain's address silent, completes as
``RegisteredNoNest`` and lets the person go on straight from handle entry (no
checkbox). Hetzner holds the zone and covers DNS and the server. The zone still
holds what an earlier box left: its apex and ``mail`` ``A`` at the old address,
an apex ``AAAA``, the right ``MX``, and a site-verification ``TXT`` that is
nobody's business but the domain owner's. *Buy and set up* runs to Succeeded,
and the zone it leaves behind is the witness of the converge: each ``A`` holds
exactly the new server's address, no ``AAAA`` is left, the verification ``TXT``
is untouched and one ``MX`` remains.

The preview of what the run will do (each plan row's text) is pinned before
the run by ``test_provider_choice.py``'s DNS-plan test; here one row is read
only to show the run started from a page that planned it.

**Fixture setup, not journey (convention 8).** The handle check's OUTCOME is
seeded through the sanctioned test setter, so the journey needs no outbound
DNS; the old box's records are seeded into the Hetzner fake's zone. Every
mutation of the journey — Continue, Connect, *Buy and set up* — goes through
the UI.

tier_3: the run's nest leg is a REAL never-claimed nest
(``provision_target_nest``) — on the standard path the ``Online`` step ends by
claiming the box over WS-RPC, which no fake serves.
"""
from __future__ import annotations

import json

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import require_surface
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.crash_recovery import setup_status
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# The zone the Hetzner DNS fake holds.
HELD_DOMAIN = "example.test"
# The deleted box's addresses (documentation ranges, never the new server's).
OLD_BOX_V4 = "198.51.100.7"
OLD_BOX_V6 = "2001:db8::7"
# A site-verification record the run must leave alone.
VERIFICATION_TXT = "site-verification=not-a-real-fauna-e2e-token"


def _a_values(records: list[tuple[str, str, str]], name: str) -> list[str]:
    return sorted(v for n, t, v in records if n == name and t == "A")


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_setup_over_a_deleted_boxs_records(app, fake_cloud, provision_target_nest):
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    nest = provision_target_nest
    zone = fake_cloud.hetzner_dns

    # ── the zone an earlier, deleted box left behind ──
    zone.seed_record("@", "A", OLD_BOX_V4)
    zone.seed_record("mail", "A", OLD_BOX_V4)
    zone.seed_record("@", "AAAA", OLD_BOX_V6)
    zone.seed_record("@", "MX", f"10 mail.{HELD_DOMAIN}.")
    stored_txt = zone.seed_txt("@", VERIFICATION_TXT)

    # ── providers and the real nest standing in for the new box ──
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
    drv.call_machine_method(
        "set_provision_claim_code_for_test", json.dumps(nest["claim_code"])
    )

    # ── § 2: a silent address completes RegisteredNoNest, Continue live ──
    app.onboarding.go_to_handle_entry()
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": "RegisteredNoNest",
        "message": {
            "key": "onboarding.handle_check.outcome.registered_no_nest",
            "args": {"domain": HELD_DOMAIN},
        },
        "continue_enabled": True,
    }, handle=f"alice@{HELD_DOMAIN}")
    wait_until(lambda: app.is_enabled("handle-entry-continue-button"), 10.0,
               diagnose=lambda: drv.diagnose("handle-entry-continue-button"))
    drv.click("handle-entry-continue-button")
    app.wait_for("provider-choice-continue-button")

    # ── Hetzner holds the zone and covers DNS and the server ──
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
    wait_until(lambda: app.is_enabled("provider-choice-continue-button"), 10.0,
               diagnose=lambda: "Continue stayed disabled: "
               + drv.get_text("provider-choice-status-text"))
    drv.click("provider-choice-continue-button")
    app.wait_for("provisioning-start-button")

    # One sanity read: the page planned to change the old apex address.
    row = "provisioning-dns-plan-row[@|A]"
    wait_until(
        lambda: app.is_visible(row),
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            f"{row} never painted. plan="
            + json.dumps(drv.call_machine_method("dns_plan"))
            + " | " + drv.diagnose("provisioning-dns-plan")
        ),
    )
    strings = S.onboarding.nest_provisioning
    assert drv.get_text(row).strip() == strings.dns_plan_row_change(
        name="@", record_type="A", old=OLD_BOX_V4, value=strings.dns_plan_new_server,
    ), drv.diagnose(row)

    # ── Buy and set up, to Succeeded ──
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
        diagnose=lambda: "the run never reached Succeeded. snapshot="
        + json.dumps(last["snap"]) + f" | error={app.error_text()!r}",
    )

    # ── the zone the run left: converged, nothing else touched ──
    new_v4 = fake_cloud.hetzner_cloud._ORDERED_SERVER_IPV4
    left = zone.records()
    assert new_v4 != OLD_BOX_V4
    assert _a_values(left, "@") == [new_v4], (
        f"the apex A holds exactly the new server's address: {left!r}"
    )
    assert _a_values(left, "mail") == [new_v4], (
        f"the mail A holds exactly the new server's address: {left!r}"
    )
    assert not [r for r in left if r[1] == "AAAA"], (
        f"the old box's AAAA is removed: {left!r}"
    )
    assert ("@", "TXT", stored_txt) in left, (
        f"the site-verification TXT is untouched: {left!r}"
    )
    assert ("@", "TXT", stored_txt) not in zone.removed_records
    assert [r for r in left if r[1] == "MX"] == [("@", "MX", f"10 mail.{HELD_DOMAIN}.")], (
        f"one MX, the one already right: {left!r}"
    )
    st = setup_status(nest["url"])
    assert st.get("claimed") is True, (
        f"Succeeded on the standard path means built AND claimed: {st!r}"
    )

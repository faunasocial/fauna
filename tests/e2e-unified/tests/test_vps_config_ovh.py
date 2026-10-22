"""tier_2: OVH through the vps_config wizard, against the fake cloud's OVH leg.

`docs/goal/architecture/provisioning/registry.md` § Server offers follow the
location → *OVH: a location is a project and a region*: verify lists every UP
`region` of every project the consumer key reads as one ``<project>/<region>``
location, a one-project account getting a picker indistinguishable from
Hetzner's (the wizard never parses the id); the offers are the curated flavors
the chosen region has, priced gross from the public catalogue; create resolves
the flavor and the Ubuntu 26.04 image by id in that region and bills monthly.
`docs/goal/behavior/onboarding-provisioning.md` § 5: the same two controls on
every provider — ``vps-location-picker`` and ``vps-server-type-radio`` (at most
5 offers, labelled by ``server_type_label``), re-listed on a location change.

The fake (``fakes/fake_cloud.py::OvhHandlers``) holds GRA11 with d2-2 and d2-4,
SBG5 with d2-2 only, and a Brussels localzone verify must leave out; the
catalogue prices d2-2 at 5.71 + 1.3133 tax (gross 7.02) and d2-4 at 14.04.

Drives the real ``verify_vps`` and the real orchestrator (the verified helper:
web + linux + tui); web omits the location picker, so it runs on linux + tui.
"""

from __future__ import annotations

import json

import pytest

from helpers import budgets
from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_2

PROJECT = "proj-ovh-e2e"
CREDS = {"app-key": "AK", "app-secret": "AS", "consumer-key": "CK"}


def _skip_without_the_picker(drv) -> None:
    if not (drv.is_linux() or drv.is_tui()):
        skip_unbuilt(
            drv,
            surface="the vps-location-picker over a real verify",
            detail="the verified helper runs on web + linux + tui and web omits "
            "the picker; the other apps lack set_provider_base_urls",
            tracked="installers/vps.md",
        )


def test_ovh_project_regions_priced_offers_and_the_resolved_create(app, fake_cloud):
    drv = app.driver
    _skip_without_the_picker(drv)
    ovh = fake_cloud.ovh
    ovh.seed_project(PROJECT, "e2e project")

    app.onboarding.go_to_vps_config_with_verified(
        "ovh", CREDS, fake_cloud, vps_base_url=ovh.base_url()
    )

    # One project → plain region names, the project never shown; the first
    # UP region is the default.
    picker = "vps-location-picker"
    assert drv.get_text(picker) == "Gravelines (GRA11)", (
        f"the picker should default to the first UP region; read "
        f"{drv.get_text(picker)!r}; {drv.diagnose(picker)}"
    )

    # GRA11 offers both curated flavors, priced gross from the catalogue.
    first, second = "vps-server-type-radio[0]", "vps-server-type-radio[1]"
    assert "d2-2" in drv.get_text(first) and "7.02" in drv.get_text(first), (
        f"radio 0 should be d2-2 at the gross 7.02; read {drv.get_text(first)!r}"
    )
    assert "d2-4" in drv.get_text(second) and "14.04" in drv.get_text(second), (
        f"radio 1 should be d2-4 at the gross 14.04; read {drv.get_text(second)!r}"
    )

    # The second region lacks d2-4: the radio re-lists without it.
    drv.select(picker, "Strasbourg (SBG5)")
    wait_until(
        lambda: drv.is_absent(second),
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"after selecting Strasbourg the radio still lists "
            f"{drv.get_text(second)!r}; error={app.error_text()!r}; "
            f"{drv.diagnose(second)}"
        ),
    )
    assert drv.get_text(picker) == "Strasbourg (SBG5)", drv.diagnose(picker)
    assert "d2-2" in drv.get_text(first), drv.diagnose(first)
    drv.click(first)

    # Continue to the run and start it: the Server step creates in SBG5 with
    # the flavor and image the adapter resolved by id.
    wait_until(lambda: app.is_enabled("vps-config-continue-button"), budgets.UI_SETTLE_S)
    app.click("vps-config-continue-button")
    app.wait_for("provisioning-start-button", timeout=budgets.UI_SETTLE_S)
    app.click("provisioning-start-button")

    last = {"snap": None}

    def created():
        last["snap"] = drv.call_machine_method("provisioning_snapshot")
        return bool(ovh.created)

    wait_until(
        created,
        budgets.ORCHESTRATION_STEP_S,
        diagnose=lambda: "the run never reached the OVH instance create. snapshot="
        + json.dumps(last["snap"])
        + f" | error={app.error_text()!r}",
    )
    project, body = ovh.created[0]
    assert len(ovh.created) == 1, f"expected one create, saw {ovh.created!r}"
    assert project == PROJECT, body
    assert body["region"] == "SBG5", body
    assert body["flavorId"] == "flavor-d2-2", body
    assert body["imageId"] == "img-2604", body
    assert body["monthlyBilling"] is True, body
    assert body.get("userData"), "the create carries the box's cloud-init"

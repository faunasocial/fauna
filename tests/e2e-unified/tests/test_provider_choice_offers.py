"""The choose-providers path's server offers, update channel and Namecheap's
refusal — set-up-a-nest-from-the-app outcomes 10, 12 and 13 on the page every
app but windows now walks.

* Outcome 10, `docs/goal/behavior/onboarding-provisioning.md` § 5 → *Update
  channel*: the three channel rows are offered, Stable is the default, and a
  pick moves the selection — on this path in the choices region of
  ``nest_provisioning`` (§ 3d → *The choices on the setting-things-up page*).
* Outcome 12, `docs/goal/architecture/provisioning/registry.md` § Server
  offers follow the location: the offers listed are the chosen location's;
  changing the location re-lists the server types as that location sells them,
  and a type it does not offer drops out of the choice. On this path the
  location and type are choices on ``nest_provisioning``, each with a default
  (`docs/goal/behavior/onboarding-provisioning.md` § 3d → *What becomes of
  §§ 4, 5 and 6*), so a dropped type is re-chosen by the default rule rather
  than left empty. Two journeys: Hetzner (the price per location is the
  listing's witness) and OVH (a location is a project and a region; the
  create resolves the flavor and image by id in the chosen region).
* Outcome 13, `registry.md` § Proxy boundary (*Namecheap's IP allowlist*):
  Namecheap's own rejection is the reflector — when it refuses the request's
  source address, the error the dialog shows names the address to allow.

The old set-up path's journeys (``test_vps_config.py``,
``test_vps_config_ovh.py``, ``test_dns_config.py``,
``test_provider_registry_parity.py``) are marked for windows alone, the one app
that still walks ``dns_config`` → ``vps_config``; these are their successors.
"""
from __future__ import annotations

import json

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from helpers import budgets
from helpers.app_surface import require_surface
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_2

# The zone the Hetzner fake holds; a domain no fake DNS leg holds, for the
# journeys whose DNS is the manual row's.
HELD_DOMAIN = "example.test"
OTHER_DOMAIN = "elsewhere.test"

OVH_PROJECT = "proj-ovh-e2e"
OVH_CREDS = {"app-key": "AK", "app-secret": "AS", "consumer-key": "CK"}


def _land_on_the_page(app, handle: str) -> None:
    """Seed a RegisteredNoNest outcome — a domain the person brings — and press
    the handle check's Continue (the outcome is seeded through the sanctioned
    test setter so no journey depends on outbound DNS)."""
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


def _open_and_fill(app, provider: str, fields: dict[str, str]) -> None:
    drv = app.driver
    drv.click(f"provider-row[{provider}]")
    app.wait_for("provider-dialog")
    for field, value in fields.items():
        drv.type_text(f"provider-credentials-form-{field}", value)
    drv.wait_until_enabled("provider-connect-button", timeout=10)
    drv.click("provider-connect-button")


def _connect(app, provider: str, fields: dict[str, str]) -> None:
    drv = app.driver
    _open_and_fill(app, provider, fields)
    wait_until(
        lambda: drv.get_attr(f"provider-row[{provider}]", "state") == "connected",
        budgets.ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            f"{provider} never read connected: dialog status="
            f"{drv.get_text('provider-dialog-status-text')!r} error={app.error_text()!r} "
            + drv.diagnose(f"provider-row[{provider}]")
        ),
    )
    drv.click("provider-dialog-close-button")


def _continue_to_provisioning(app) -> None:
    drv = app.driver
    drv.wait_until_enabled("provider-choice-continue-button", timeout=10)
    drv.click("provider-choice-continue-button")
    app.wait_for("provisioning-start-button")


def _chosen_radios(drv) -> list[int]:
    return [
        i for i in range(6)
        if drv.is_visible(f"vps-server-type-radio[{i}]")
        and drv.get_attr(f"vps-server-type-radio[{i}]", "state") == "on"
    ]


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_the_server_sizes_on_the_setting_up_page_follow_the_location(app, fake_cloud):
    """Hetzner covers DNS and the server; on ``nest_provisioning`` the first
    radio's price is the listing's witness — the fake prices ``cx23`` at 4.51
    in Falkenstein (the default location) and 5.17 in Helsinki."""
    require_surface(app.driver, "provider-choice")
    app.onboarding.require_offers_follow_the_location()
    drv = app.driver
    drv.set_provider_base_urls({
        "dns": fake_cloud.dns_base_for("hetzner"),
        "vps": fake_cloud.url_map()["vps"],
    })
    _land_on_the_page(app, f"alice@{HELD_DOMAIN}")
    _connect(app, "hetzner", {"api-token": "MOCK"})
    _continue_to_provisioning(app)

    radio = "vps-server-type-radio[0]"
    first = drv.get_text(radio)
    assert "4.51" in first, (
        f"the default location (Falkenstein) should price cx23 at 4.51; "
        f"radio read {first!r}; {drv.diagnose(radio)}"
    )

    drv.select("vps-location-picker", "Helsinki DC Park 1")
    wait_until(
        lambda: "5.17" in (drv.get_text(radio) or ""),
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"after selecting Helsinki the radio still reads "
            f"{drv.get_text(radio)!r}; error={app.error_text()!r}; "
            f"{drv.diagnose(radio)}"
        ),
    )
    assert len(_chosen_radios(drv)) == 1, (
        "a server type stays chosen across the re-list: "
        f"{_chosen_radios(drv)}; {drv.diagnose(radio)}"
    )
    assert app.is_enabled("provisioning-start-button"), (
        f"error={app.error_text()!r} " + drv.diagnose("provisioning-start-button")
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_ovh_regions_re_list_the_offers_and_the_create_lands_in_the_chosen_region(
    app, fake_cloud
):
    """OVH covers the server and the manual row DNS. The fake
    (``fakes/fake_cloud.py::OvhHandlers``) holds GRA11 with d2-2 and d2-4 and
    SBG5 with d2-2 only, priced gross from the catalogue (d2-2 7.02, d2-4
    14.04): one project reads as plain region names; choosing d2-4 and then
    SBG5 drops d2-4, and the default rule re-chooses d2-2; the run creates in
    SBG5 with the flavor and the Ubuntu image resolved by id, billed monthly."""
    require_surface(app.driver, "provider-choice")
    app.onboarding.require_offers_follow_the_location()
    drv = app.driver
    ovh = fake_cloud.ovh
    ovh.seed_project(OVH_PROJECT, "e2e project")
    drv.set_provider_base_urls({"vps": ovh.base_url()})
    _land_on_the_page(app, f"alice@{OTHER_DOMAIN}")
    _connect(app, "ovh", OVH_CREDS)
    drv.click("provider-row[manual]")
    wait_until(
        lambda: drv.get_attr("provider-goal-row[dns]", "provider") == "manual",
        10.0,
        diagnose=lambda: drv.diagnose("provider-goal-row[dns]"),
    )
    assert drv.get_attr("provider-goal-row[vps]", "provider") == "ovh", (
        drv.diagnose("provider-goal-row[vps]")
    )
    _continue_to_provisioning(app)

    picker = "vps-location-picker"
    assert drv.get_text(picker) == "Gravelines (GRA11)", (
        f"the picker should default to the first UP region; read "
        f"{drv.get_text(picker)!r}; {drv.diagnose(picker)}"
    )
    first, second = "vps-server-type-radio[0]", "vps-server-type-radio[1]"
    assert "d2-2" in drv.get_text(first) and "7.02" in drv.get_text(first), (
        f"radio 0 should be d2-2 at the gross 7.02; read {drv.get_text(first)!r}"
    )
    assert "d2-4" in drv.get_text(second) and "14.04" in drv.get_text(second), (
        f"radio 1 should be d2-4 at the gross 14.04; read {drv.get_text(second)!r}"
    )

    drv.click(second)
    wait_until(
        lambda: drv.get_attr(second, "state") == "on",
        budgets.UI_SETTLE_S,
        diagnose=lambda: "d2-4 never read chosen: " + drv.diagnose(second),
    )

    # SBG5 lacks d2-4: the radio re-lists without it, and the default rule
    # re-chooses among what the region sells.
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
    wait_until(
        lambda: drv.get_attr(first, "state") == "on",
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the dropped d2-4 was not replaced by d2-2: "
        + drv.diagnose(first),
    )

    wait_until(lambda: app.is_enabled("provisioning-start-button"), budgets.UI_SETTLE_S,
               diagnose=lambda: f"error={app.error_text()!r} "
               + drv.diagnose("provisioning-start-button"))
    drv.click("provisioning-start-button")

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
    assert project == OVH_PROJECT, body
    assert body["region"] == "SBG5", body
    assert body["flavorId"] == "flavor-d2-2", body
    assert body["imageId"] == "img-2604", body
    assert body["monthlyBilling"] is True, body
    assert body.get("userData"), "the create carries the box's cloud-init"


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_namecheaps_refusal_in_the_provider_dialog_names_the_address_to_allow(
    app, fake_cloud
):
    """The fake's Namecheap leg (``fakes/fake_cloud.py::NamecheapHandlers``)
    refuses every call from ``observed_ip`` with HTTP 200 and error 1011150
    echoing the address — the adapter's one self-heal retry included — so the
    refusal reaches the dialog, whose status names the address and says to
    allow it; the row stays unconnected."""
    require_surface(app.driver, "provider-choice")
    require_surface(app.driver, "set-provider-base-urls")
    drv = app.driver
    nc = fake_cloud.namecheap
    drv.set_provider_base_urls({"dns": nc.base_url()})
    _land_on_the_page(app, f"alice@{HELD_DOMAIN}")
    _open_and_fill(app, "namecheap", {"api-user": "e2e-user", "api-key": "e2e-key"})

    def shown() -> str:
        return f"{drv.get_text('provider-dialog-status-text') or ''} {app.error_text() or ''}"

    wait_until(
        lambda: nc.observed_ip in shown(),
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"no error naming {nc.observed_ip}: shown={shown()!r} "
            f"| namecheap calls={nc.requests!r}"
        ),
    )
    assert "allowlist" in shown().lower(), shown()
    assert drv.get_attr("provider-row[namecheap]", "state") != "connected", (
        drv.diagnose("provider-row[namecheap]")
    )
    # The fake saw the placeholder, then the self-heal retry with the echo.
    assert [r.get("ClientIp") for r in nc.requests[:2]] == ["192.0.2.1", nc.observed_ip], (
        nc.requests
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_the_update_channel_rows_on_the_setting_up_page_select_one_of_three(
    app, fake_cloud
):
    """Outcome 10 on this path: the ``vps-config-update-channel-row`` radios in
    the choices region of ``nest_provisioning`` offer all three channels,
    ``stable`` is selected until another is picked, and picking one moves the
    selection.

    The page reads which channels have a build on entry; a fixed reading is
    rested on the machine through its test-only setter (which a late live probe
    never overwrites) so the default is the status quo, Stable, as in the old
    path's journey. What the selection does to the box is pinned in Rust
    (``update_channel_ids_and_tags``,
    ``provision_update_channel_defaults_to_stable_then_follows_the_choice``)."""
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    drv.set_provider_base_urls({
        "dns": fake_cloud.dns_base_for("hetzner"),
        "vps": fake_cloud.url_map()["vps"],
    })
    _land_on_the_page(app, f"alice@{HELD_DOMAIN}")
    _connect(app, "hetzner", {"api-token": "MOCK"})
    _continue_to_provisioning(app)
    channels = ("stable", "test", "dev")
    drv.call_machine_method(
        "set_channel_availability_for_test",
        json.dumps([{"channel": c, "build": {"build": "unknown"}} for c in channels]),
    )

    row = "vps-config-update-channel-row[{}]".format
    app.wait_for(row("stable"))

    def states() -> dict[str, str | None]:
        return {c: drv.get_attr(row(c), "state") for c in channels}

    def await_only(selected: str) -> None:
        want = {c: ("on" if c == selected else "off") for c in channels}
        wait_until(
            lambda: states() == want,
            budgets.UI_SETTLE_S,
            diagnose=lambda: (
                f"expected only the {selected} row selected, rows read {states()}; "
                f"error={app.error_text()!r}; {drv.diagnose(row(selected))}"
            ),
        )

    await_only("stable")
    drv.click(row("dev"))
    await_only("dev")
    drv.click(row("test"))
    await_only("test")


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_the_mail_gate_on_the_setting_up_page_keeps_a_mail_box_to_2_gb_and_up(
    app, fake_cloud
):
    """Outcome 2's mail half on this path: in the choices region of
    ``nest_provisioning`` a mail box (``vps-config-mail-mode-toggle`` on) is
    offered only the server types with at least 2 GB — the scanners need the
    RAM (`docs/goal/behavior/onboarding-provisioning.md` § 5) — and turning
    mail off offers the 1 GB tier too, a type staying chosen across the flip.

    The server types are rested on the machine through its test-only setter, a
    1 GB and a 4 GB plan, so the gate is exercised against known sizes rather
    than whatever the fake lists (it sells no 1 GB plan); successor to
    ``test_vps_config.py::test_vps_config_mail_mode_toggle_gates_server_types``."""
    require_surface(app.driver, "provider-choice")
    drv = app.driver
    drv.set_provider_base_urls({
        "dns": fake_cloud.dns_base_for("hetzner"),
        "vps": fake_cloud.url_map()["vps"],
    })
    _land_on_the_page(app, f"alice@{HELD_DOMAIN}")
    _connect(app, "hetzner", {"api-token": "MOCK"})
    _continue_to_provisioning(app)

    drv.call_machine_method("set_provision_mail_mode", json.dumps(True))
    drv.call_machine_method("set_vps_state_for_test", json.dumps({
        "provider_id": "hetzner",
        "server_types": [
            {"id": "cx11", "vcpu": 1, "mem_gb": 1.0, "disk_gb": 25,
             "price_monthly_cents": 400, "currency": "EUR"},
            {"id": "cx22", "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
             "price_monthly_cents": 900, "currency": "EUR"},
        ],
    }))
    app.wait_for("vps-config-mail-mode-toggle")

    def offered() -> int:
        return sum(
            1 for i in range(6) if drv.is_visible(f"vps-server-type-radio[{i}]")
        )

    wait_until(
        lambda: offered() == 1,
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"a mail box should offer only the >=2 GB plan, saw {offered()} "
            f"radios; error={app.error_text()!r}; "
            + drv.diagnose("vps-server-type-radio[0]")
        ),
    )
    # The setter rests the offers without running the default rule, so the one
    # offered plan is chosen by hand; the flip must keep it chosen.
    drv.click("vps-server-type-radio[0]")
    wait_until(
        lambda: _chosen_radios(drv) == [0],
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the 4 GB plan never read chosen: "
        + drv.diagnose("vps-server-type-radio[0]"),
    )

    drv.click("vps-config-mail-mode-toggle")
    wait_until(
        lambda: offered() == 2,
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"mail off should offer both plans, saw {offered()} radios; "
            f"error={app.error_text()!r}; "
            + drv.diagnose("vps-config-mail-mode-toggle")
        ),
    )
    assert len(_chosen_radios(drv)) == 1, (
        f"a server type stays chosen after the flip: {_chosen_radios(drv)}"
    )

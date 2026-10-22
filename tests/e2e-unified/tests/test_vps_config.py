"""Handle-first onboarding: vps_config stage.

Element IDs from ``tests/e2e-unified/ui.yaml`` (lines ~886-905):
  vps-provider-row (indexed: ``vps-provider-row[<provider_id>]``),
  vps-provider-link, vps-provider-open-browser-button,
  vps-provider-help-text, vps-credentials-form
  (child ``vps-credentials-form-<field_id>``),
  vps-verify-button,
  vps-server-type-radio (indexed by position),
  vps-config-back-button, vps-config-continue-button,
  error-message.

The "verified" variants below depend on a wiremock-backed provider that
isn't wired yet; those tests skip with a clear pointer.
"""

from __future__ import annotations

import json

import pytest

from helpers import budgets
from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_2


def test_vps_config_renders_provider_row(app):
    """vps_config stage (reached via ``dns-set-up-later-button``) shows
    the provider row + back/continue buttons."""
    app.onboarding.go_to_vps_config()
    assert app.driver.count("vps-provider-row") >= 1, (
        "vps_config stage should render at least one provider row: "
        f"{app.driver.diagnose('vps-provider-row')}"
    )
    assert app.driver.is_visible("vps-config-back-button"), (
        "vps_config should show the back button: "
        f"{app.driver.diagnose('vps-config-back-button')}"
    )
    assert app.driver.is_visible("vps-config-continue-button"), (
        "vps_config should show the continue button: "
        f"{app.driver.diagnose('vps-config-continue-button')}"
    )


def test_vps_config_back_returns_to_dns_config(app):
    """``vps-config-back-button`` returns to dns_config."""
    # macos + ios: the dns_config canary `dns-provider-row` (bare container) reads
    # via apple's shared ProviderRow countable-container `.automationValue` fix; both verified GREEN (macos N+31, ios N+33).
    app.onboarding.go_to_vps_config()
    app.driver.click("vps-config-back-button")
    app.driver.wait_for("dns-provider-row", timeout=10)
    assert app.driver.is_visible("dns-provider-row"), (
        "Back from vps_config should return to dns_config (dns-provider-row canary): "
        f"{app.driver.diagnose('dns-provider-row')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_vps_config_select_provider_shows_credentials_form(app):
    """Selecting a VPS provider reveals the credentials form."""
    app.onboarding.go_to_vps_config()
    if not app.driver.is_visible("vps-provider-row[hetzner]"):
        pytest.skip(
            "Hetzner not visible in vps-provider-row — providers.ts "
            "regression or per-app filter issue."
        )
    app.driver.click("vps-provider-row[hetzner]")
    app.driver.wait_for("vps-credentials-form", timeout=10)
    assert app.driver.is_visible("vps-provider-link"), (
        "selecting a VPS provider should reveal the provider link: "
        f"{app.driver.diagnose('vps-provider-link')}"
    )
    assert app.driver.is_visible("vps-verify-button"), (
        "selecting a VPS provider should reveal the verify button: "
        f"{app.driver.diagnose('vps-verify-button')}"
    )


def test_vps_config_preselects_dns_provider_when_capable(app, fake_cloud):
    """When a DNS provider with VPS capability was selected on dns_config
    AND ``same_provider_for_vps`` is on, vps_config preselects it.

    Drives the real verify_dns probe against the ``fake_cloud`` stub
    (web-only; the helper skips on the other apps until they implement
    ``set_provider_base_urls`` — tracked internally, Track 2)."""
    app.onboarding.go_to_vps_config_with_dns_provider("hetzner", fake_cloud)
    assert app.driver.is_visible("vps-provider-row[hetzner]"), (
        "vps_config should preselect the VPS-capable DNS provider (hetzner): "
        f"{app.driver.diagnose('vps-provider-row[hetzner]')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_vps_config_server_types_after_verify(app, fake_cloud):
    """After ``vps-verify-button`` succeeds, server types render as radios
    (≤5). The verify probe hits the ``fake_cloud`` stub's datacenters +
    server_types endpoints (web-only — see the helper's skip)."""
    app.onboarding.go_to_vps_config_with_verified("hetzner",
                                                  {"api-token": "MOCK"},
                                                  fake_cloud)
    # vps-server-type-radio is indexed-by-position (`vps-server-type-radio[i]`)
    # on every app, so probe indices — the bridge's count() does an exact
    # data-testid match and can't count the indexed variants from the bare id.
    radios = sum(
        1 for i in range(6)
        if app.driver.is_visible(f"vps-server-type-radio[{i}]")
    )
    assert 1 <= radios <= 5, (
        f"vps-server-type-radio count {radios} outside 1..5 — curated "
        "list slice broke or stub returned wrong shape"
    )


def test_vps_server_types_follow_the_location(app, fake_cloud):
    """A location change re-lists the ``vps-server-type-radio`` for the new
    location (``registry.md`` § Server offers follow the location;
    ``onboarding-provisioning.md`` § 5). The ``fake_cloud`` Hetzner leg prices
    each type differently per location — ``cx23`` is 4.51 in Falkenstein and
    5.17 in Helsinki — so the first radio's price is the listing's witness.

    Drives the real ``verify_vps`` (the verified helper: web + linux + tui);
    web omits the location picker, so it runs on linux + tui."""
    if not (app.driver.is_linux() or app.driver.is_tui()):
        skip_unbuilt(
            app.driver,
            surface="the vps-location-picker over a real verify",
            detail="the verified helper runs on web + linux + tui and web omits "
            "the picker; the other apps lack set_provider_base_urls",
            tracked="installers/vps.md",
        )
    app.onboarding.go_to_vps_config_with_verified("hetzner",
                                                  {"api-token": "MOCK"},
                                                  fake_cloud)
    radio = "vps-server-type-radio[0]"
    first = app.driver.get_text(radio)
    assert "4.51" in first, (
        f"the default location (Falkenstein) should price cx23 at 4.51; "
        f"radio read {first!r}; {app.driver.diagnose(radio)}"
    )

    app.driver.select("vps-location-picker", "Helsinki DC Park 1")

    wait_until(
        lambda: "5.17" in (app.driver.get_text(radio) or ""),
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"after selecting Helsinki the radio still reads "
            f"{app.driver.get_text(radio)!r}; error={app.error_text()!r}; "
            f"{app.driver.diagnose(radio)}"
        ),
    )


def test_vps_location_picker_renders_and_selects(app):
    """The ``vps-location-picker`` dropdown renders the verified locations and
    ``select`` actuates it, persisting the choice in machine state.

    Runs on linux today; web omits the picker. The locations are injected via
    the cross-app ``set_vps_locations_for_test`` bridge (snapshot fixturing,
    like every other onboarding stage test) rather than the web-only fake-cloud
    verify probe — so this directly exercises the Linux ``gtk::DropDown``
    conversion without depending on ``set_provider_base_urls``
    (tracked internally).

    macOS verified green: apple's ``.automationSelect`` now
    reads/writes the location **display name** (not the id) — matching the
    cross-app ``vps-location-picker`` contract (the Linux gtk::DropDown this
    test asserts). The N+31 datum was ``get_text`` returning ``'fsn1'`` (the id)
    not ``'fsn1-dc14'`` (the name); the seam (``MacVpsConfigView``/``VpsConfigView``
    ``locationPicker``) now maps id<->name both directions. iOS shares the fix and
    is verified green here via a harness ``--client ios`` run."""
    if not (
        app.driver.is_linux()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="the vps-location-picker",
            detail="runs on linux + macos + ios + tui (apps/fauna-tui/src/"
            "wizard/vps_config.rs paints the full onboarding.md § 5 VPS "
            "wizard); web/windows/android are the remaining cross-app "
            "follow-on",
            tracked="installers/vps.md",
        )
    locations = [
        {"id": "fsn1", "name": "fsn1-dc14", "city": "Falkenstein", "country": "DE"},
        {"id": "nbg1", "name": "nbg1-dc3", "city": "Nuremberg", "country": "DE"},
    ]
    app.onboarding.go_to_vps_config_with_locations("hetzner", locations)
    assert app.driver.is_visible("vps-location-picker"), (
        "vps_config should render the location picker after verify: "
        f"{app.driver.diagnose('vps-location-picker')}"
    )
    # Defaults to the first location (verify_vps post-state).
    assert app.driver.get_text("vps-location-picker") == "fsn1-dc14", (
        "location picker should default to the first verified location 'fsn1-dc14'; "
        f"got {app.driver.get_text('vps-location-picker')!r}"
    )
    # Select the second by its display label; the choice must stick.
    app.driver.select("vps-location-picker", "nbg1-dc3")
    assert app.driver.get_text("vps-location-picker") == "nbg1-dc3", (
        "selecting 'nbg1-dc3' should stick in the location picker; "
        f"got {app.driver.get_text('vps-location-picker')!r}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_vps_config_mail_mode_toggle_gates_server_types(app):
    """The ``vps-config-mail-mode-toggle`` RAM-gates the server-type radio:
    a mail box offers only plans with ``mem_gb >= 2`` (the clamd/rspamd
    scanners need the RAM), and turning mail OFF unlocks the cheap 1 GB tier.

    Runs end-to-end on web + linux + windows + macos + ios + tui. mail-ON
    renders one radio (the >=2 GB plan); the toggle click flips mail OFF and
    re-renders the 1 GB plan as a SECOND radio. The apple in-process path
    required a registry re-registration fix: the enumerated
    radio rows keyed by the server-type-stable ``element.id`` while baking the
    position into the literal registered id, so the mail→social flip — which
    prepends the 1 GB tier — left the pre-existing row mounted under its stale
    ``[0]`` id and ``[1]`` never fired ``_AutomationRegister``; the fix keys by
    ``\\.offset`` and pins each row ``.id`` to ``(index, stable-content)`` so a
    reposition re-registers with the correct positional id. android widens
    once host-emulator e2e lands. tui had its own gap (2026-07-30): it called
    the shared ``server_type_allowed_for_mail`` gate but only used the result
    to set ``radio.enabled = allowed``, rendering the disallowed radio present
    but disabled instead of omitting it — fixed to filter before indexing,
    matching linux's ``rebuild_radio_group`` call site.
    Server types are injected via the cross-app ``set_vps_state_for_test``
    bridge so the gate is exercised against known RAM sizes (a 1 GB + a 4 GB
    plan) rather than whatever the live provider returns.
    """
    if not (
        app.driver.is_linux()
        or app.driver.is_web()
        or app.driver.is_windows()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="the vps-config-mail-mode-toggle RAM gate",
            detail="runs on web + linux + windows + macos + ios + tui; "
            "android widens once host-emulator e2e lands.",
            tracked="installers/vps.md",
        )
    app.onboarding.go_to_vps_config()
    # Seed mail-ON first, then the provider + two plans, so the per-provider
    # section renders with the toggle checked (in sync with machine state).
    app.driver.call_machine_method("set_provision_mail_mode", json.dumps(True))
    app.driver.call_machine_method("set_vps_state_for_test", json.dumps({
        "provider_id": "hetzner",
        "server_types": [
            {"id": "cx11", "vcpu": 1, "mem_gb": 1.0, "disk_gb": 25,
             "price_monthly_cents": 400, "currency": "EUR"},
            {"id": "cx22", "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
             "price_monthly_cents": 900, "currency": "EUR"},
        ],
    }))
    app.driver.wait_for("vps-config-mail-mode-toggle", timeout=10)
    app.driver.wait_for("vps-server-type-radio[0]", timeout=10)
    assert app.driver.is_visible("vps-config-mail-mode-toggle"), (
        "vps_config should render the mail-mode toggle: "
        f"{app.driver.diagnose('vps-config-mail-mode-toggle')}"
    )

    # Mail ON: only the 4 GB plan is offered (the 1 GB plan is filtered out).
    mail_on = sum(
        1 for i in range(5)
        if app.driver.is_visible(f"vps-server-type-radio[{i}]")
    )
    assert mail_on == 1, (
        f"a mail box should offer only the >=2 GB plan, got {mail_on} radios"
    )

    # Click the toggle → mail OFF → the 1 GB plan reappears as a second radio.
    app.driver.click("vps-config-mail-mode-toggle")
    app.driver.wait_for("vps-server-type-radio[1]", timeout=10)
    mail_off = sum(
        1 for i in range(5)
        if app.driver.is_visible(f"vps-server-type-radio[{i}]")
    )
    assert mail_off == 2, (
        f"a social-only box should offer both plans, got {mail_off} radios"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_vps_config_update_channel_rows_select_one_of_three(app):
    """The ``vps-config-update-channel-row`` radios: all three channels are
    offered to every user, ``stable`` is selected until another is picked, and
    picking one moves the selection (``onboarding-provisioning.md`` § 5).

    Driven by clicking the rows, and before any provider is chosen — the
    channel does not depend on the provider. What the selection then does to
    the box (the image tag written into cloud-init) is pinned in Rust:
    ``update_channel_ids_and_tags`` (fauna-provisioning) and
    ``provision_update_channel_defaults_to_stable_then_follows_the_choice``
    (fauna-onboarding-machine).
    """
    app.onboarding.go_to_vps_config()
    # A fixed registry, not the live one: entering the page reads which
    # channels have a build, and that reading moves the default (the next
    # test). Unknown everywhere is the status quo — Stable.
    seed_channel_availability(app, {"stable": UNKNOWN, "test": UNKNOWN, "dev": UNKNOWN})
    row = "vps-config-update-channel-row[{}]".format
    channels = ("stable", "test", "dev")
    app.driver.wait_for(row("stable"), timeout=10)

    def states() -> dict[str, str | None]:
        return {c: app.driver.get_attr(row(c), "state") for c in channels}

    def await_only(selected: str) -> None:
        want = {c: ("on" if c == selected else "off") for c in channels}
        wait_until(
            lambda: states() == want,
            budgets.UI_SETTLE_S,
            diagnose=lambda: (
                f"expected only the {selected} row selected, rows read {states()}; "
                f"error={app.error_text()!r}; {app.driver.diagnose(row(selected))}"
            ),
        )

    await_only("stable")
    app.driver.click(row("dev"))
    await_only("dev")
    app.driver.click(row("test"))
    await_only("test")


UNKNOWN = {"build": "unknown"}
NO_BUILD = {"build": "no_build"}


def seed_channel_availability(app, builds: dict[str, dict]) -> None:
    """Stand in for the registry read the page makes on entry
    (``OnboardingMachine::probe_channel_availability``): rest a fixed reading
    on the machine through its test-only setter, which a late live probe can
    never overwrite."""
    app.driver.call_machine_method(
        "set_channel_availability_for_test",
        json.dumps([{"channel": c, "build": b} for c, b in builds.items()]),
    )


def test_vps_config_default_channel_follows_what_the_registry_has(app):
    """With no Stable build published, an unchosen page selects the first
    channel that may have a usable one — here Test, whose state is unknown —
    and an explicit choice still wins (``onboarding-provisioning.md`` § 5 →
    *What exists decides the default*). The default is shared Rust
    (``UpdateChannel::default_for``), so every app that renders the rows
    follows it with no app code."""
    app.onboarding.go_to_vps_config()
    seed_channel_availability(app, {"stable": NO_BUILD, "test": UNKNOWN, "dev": UNKNOWN})
    row = "vps-config-update-channel-row[{}]".format
    channels = ("stable", "test", "dev")
    app.driver.wait_for(row("stable"), timeout=10)

    def states() -> dict[str, str | None]:
        return {c: app.driver.get_attr(row(c), "state") for c in channels}

    want = {"stable": "off", "test": "on", "dev": "off"}
    wait_until(
        lambda: states() == want,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"expected only the test row selected, rows read {states()}",
    )
    app.driver.click(row("stable"))
    want = {"stable": "on", "test": "off", "dev": "off"}
    wait_until(
        lambda: states() == want,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"an explicit Stable choice must stick, rows read {states()}",
    )


def test_vps_config_channel_with_no_build_says_so(app):
    """A channel the registry has no build for carries the no-build
    description in its own row, no new element; the shared getter picks the
    key (``update_channel_description_key_for_row``) and the app renders it."""
    app.onboarding.go_to_vps_config()
    seed_channel_availability(app, {"stable": NO_BUILD, "test": UNKNOWN, "dev": UNKNOWN})
    row = "vps-config-update-channel-row[{}]".format
    no_build = S.onboarding.vps_config.update_channel_stable_desc_no_build
    wait_until(
        lambda: no_build in (app.driver.get_text(row("stable")) or ""),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"the stable row should say it has no build, reads "
            f"{app.driver.get_text(row('stable'))!r}; {app.driver.diagnose(row('stable'))}"
        ),
    )
    assert no_build not in (app.driver.get_text(row("test")) or ""), (
        "a channel whose state is unknown keeps its own description"
    )

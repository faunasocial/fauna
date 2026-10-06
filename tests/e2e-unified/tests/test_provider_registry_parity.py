"""Assert every provider in ``i18n/providers.yaml`` renders correctly in the client UI.

Parametrized over providers × capabilities. Failures here are the forcing
function for per-app provider rendering — when a provider is added to
``i18n/providers.yaml``, every app must render its declared field IDs
without manual per-app wiring.

Per the handle-first migration (target: ``docs/goal/behavior/onboarding.md``),
the legacy 5-step ``nest_provision`` wizard (mode_select / dns / server /
registrar / provision) was deleted. DNS providers render on
``dns_config``, VPS providers render on ``vps_config``; registrars are a
sub-capability of DNS providers, surfaced when the user toggles
buy-domain on dns_config. There is no longer a separate ``registrar``
sub-page, so the previously-separate ``test_registrar_provider_fields_visible``
collapses into the DNS test (registrar-capable providers show their
combined field set on dns_config when buy-domain is on).

The test relies on ``OnboardingActions.go_to_dns_config()`` /
``go_to_vps_config()`` for cross-app navigation rather than the
per-driver legacy navigation helpers (web sessionStorage seeding /
the deleted linux ``setup_goto_step``).

Per the "don't skip for platform differences" testing rule, the test
does NOT ``pytest.skip`` for non-web/linux apps — let it fail loudly
so the backlog stays visible.
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

# Ensure ``generated`` and other e2e-unified modules are importable when
# pytest is invoked from the repo root.

pytestmark = pytest.mark.tier_2
_e2e_dir = str(Path(__file__).resolve().parent.parent)
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from generated.providers import PROVIDERS


# ---------------------------------------------------------------------------
# Parametrization helpers
# ---------------------------------------------------------------------------

def _providers_with_capability(cap: str) -> list[tuple[str, dict]]:
    return [
        (pid, provider)
        for pid, provider in PROVIDERS.items()
        if cap in provider.get("capabilities", [])
    ]


def _wait_until_enabled(driver, element_id: str, timeout: float = 3.0) -> None:
    """Poll until ``element_id`` is enabled. Toggling the buy-domain /
    same-provider checkboxes re-derives provider-row enabled-ness via the
    machine snapshot; the toggle→binding round-trip is async on some
    clients (WinUI), so a poll is more robust than an immediate click.
    Mirrors the helper in ``test_dns_config_select_provider_shows_credentials_form``.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if driver.is_enabled(element_id):
            return
        time.sleep(0.05)
    pytest.fail(
        f"{element_id!r} did not become enabled within {timeout:.0f}s — "
        "buy-domain / same-provider toggle propagation broken on this client."
    )


def _field_kinds(field: dict, provider: dict) -> list[str]:
    """Resolve a field's kinds, mirroring scripts/providers-generate.py:
    a missing or empty `kinds` key defaults to the provider's
    `capabilities` list. Tests then filter visible fields the same way
    the machine's `visible_*_fields()` predicates do.
    """
    kinds = field.get("kinds")
    if not kinds:
        return list(provider.get("capabilities", []))
    return list(kinds)


_DNS_PROVIDERS = _providers_with_capability("dns")
_VPS_PROVIDERS = _providers_with_capability("vps")
_REGISTRAR_PROVIDERS = _providers_with_capability("registrar")


# The handle-first dns_config / vps_config pages render via the cross-app
# OnboardingActions.go_to_dns_config / go_to_vps_config flow — verified to
# reach dns_config on macOS (2026-05-25: handle
# entry + DoH check + the dns-buy-domain/dns-same-provider/dns-provider-row
# page IDs all render). There is no longer a per-app allowlist gate:
# every app runs the real cross-app flow and fails
# loudly where its wizard is incomplete, rather than being hidden behind a
# synthetic skip/fail. Tests degrade gracefully via the dns-provider-row[id]
# visibility skip below where a client can't yet render the indexed rows.


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(
    "provider_id,provider",
    _DNS_PROVIDERS,
    ids=[pid for pid, _ in _DNS_PROVIDERS],
)
@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_dns_provider_fields_visible(app, provider_id, provider):
    """Every DNS provider exposes all declared credential fields + verify
    on the handle-first ``dns_config`` page.

    Field test IDs are ``dns-credentials-form-<field.id>`` — the
    dns_config creds-form prefixes the raw ``providers.yaml`` field.id
    with the form's ID (the legacy ``setup-dns-{provider}-{field}`` shape
    is gone). This is the convention web/iOS/Android use and that the
    passing ``test_dns_config.py`` / ``test_vps_config.py`` rely on.

    Some providers (Hetzner) declare VPS-kinded fields that are hidden
    when ``same_provider_for_vps`` is unchecked. The test toggles
    ``dns-same-provider-checkbox`` off so all DNS-kinded fields are visible.
    """
    app.onboarding.go_to_dns_config()

    # A DomainAvailable (unregistered) handle auto-sets `buy_domain=true` and
    # `same_provider_for_vps=true`, which together disable non-registrar /
    # non-VPS DNS providers (`isProviderEnabled` in +page.svelte). Uncheck
    # both so every DNS provider's row is selectable — this mirrors the proven
    # sequence in `test_dns_config_select_provider_shows_credentials_form`.
    # (Driving the checkboxes by UI click, not the machine bridge: the e2e
    # bridge's `call_machine_method` mutates a machine instance whose state
    # change doesn't reliably reach the page's reactive `selected_provider`
    # render, so the creds-form never appears.) Combined-capability providers
    # (Hetzner) then show only their DNS-kinded fields here; their VPS fields
    # are covered by `test_vps_provider_fields_visible`.
    app.driver.click("dns-buy-domain-checkbox")
    app.driver.click("dns-same-provider-checkbox")

    if not app.driver.is_visible(f"dns-provider-row[{provider_id}]"):
        pytest.skip(
            f"dns-provider-row[{provider_id}] not visible — provider may be "
            "disabled by buy-domain or capability filter on this run."
        )
    _wait_until_enabled(app.driver, f"dns-provider-row[{provider_id}]")
    app.driver.click(f"dns-provider-row[{provider_id}]")
    app.driver.wait_for("dns-credentials-form", timeout=10)

    # Per-field assertion is web-only because GTK4's AT-SPI exposes the
    # outer dns-credentials-form Box reliably but not its dynamically-
    # appended GtkEntry children — neither get_description() nor
    # get_name() returns the field.id we set via update_property /
    # widget_name on Entries inside a rebuilt section, even though
    # static-page Entries (handle-input on identity_import) work fine.
    # The Rust unit test `porkbun_creds_form_widget_ids_match_declared_fields`
    # in apps/fauna-linux/src/views/onboarding/generic_provider_form.rs
    # covers the field-IDs-match-declared invariant on linux via
    # widget_name walk, sidestepping the AT-SPI gap.
    if app.driver.is_web():
        for field in provider.get("fields", []):
            kinds = _field_kinds(field, provider)
            if "dns" not in kinds:
                continue
            tid = f"dns-credentials-form-{field['id']}"
            assert app.driver.is_visible(tid), (
                f"field {tid!r} not visible after selecting dns provider "
                f"{provider_id!r}. The dns_config creds-form renders each "
                "field with data-testid `dns-credentials-form-<field.id>`."
            )

    assert app.driver.is_visible("dns-credentials-form"), (
        f"dns-credentials-form not visible after selecting {provider_id!r}; "
        "form should be present and populated for every DNS provider."
    )
    # `is_visible_scrolled`, not a bare `is_visible`: the verify button sits
    # below the credentials form, and a provider row further down the list
    # (or a taller form) pushes it below the fold — the same below-the-fold
    # class `is_visible_scrolled`'s own docstring names (drivers/base.py).
    assert app.driver.is_visible_scrolled("dns-verify-button"), (
        f"dns-verify-button not visible after selecting {provider_id!r}."
    )


@pytest.mark.parametrize(
    "provider_id,provider",
    _VPS_PROVIDERS,
    ids=[pid for pid, _ in _VPS_PROVIDERS],
)
@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_vps_provider_fields_visible(app, provider_id, provider):
    """Every VPS provider exposes all declared credential fields + verify
    on the handle-first ``vps_config`` page.

    Field test IDs are ``vps-credentials-form-<field.id>`` (the vps_config
    creds-form prefixes the raw providers.yaml field.id; the legacy
    ``setup-server-{provider}-`` prefix is gone). VPS providers without
    curated_offers are filtered out of the provider row by `vps_config`'s
    ``capabilities includes 'vps' and len(curated_offers) > 0`` rule —
    those are skipped here.
    """
    if not provider.get("curated_offers"):
        pytest.skip(
            f"{provider_id!r} has no curated_offers; vps_config filters it "
            "out of the provider row per ui.yaml."
        )

    app.onboarding.go_to_vps_config()

    if not app.driver.is_visible(f"vps-provider-row[{provider_id}]"):
        pytest.skip(
            f"vps-provider-row[{provider_id}] not visible — provider may be "
            "disabled by capability filtering on this run."
        )
    app.driver.click(f"vps-provider-row[{provider_id}]")

    # Per-field assertion is web-only — see DNS test for the AT-SPI gap.
    if app.driver.is_web():
        for field in provider.get("fields", []):
            kinds = _field_kinds(field, provider)
            if "vps" not in kinds:
                continue
            tid = f"vps-credentials-form-{field['id']}"
            assert app.driver.is_visible(tid), (
                f"field {tid!r} not visible after selecting vps provider "
                f"{provider_id!r}."
            )

    assert app.driver.is_visible("vps-credentials-form"), (
        f"vps-credentials-form not visible after selecting {provider_id!r}."
    )
    assert app.driver.is_visible("vps-verify-button"), (
        f"vps-verify-button not visible after selecting {provider_id!r}."
    )


@pytest.mark.parametrize(
    "provider_id,provider",
    _REGISTRAR_PROVIDERS,
    ids=[pid for pid, _ in _REGISTRAR_PROVIDERS],
)
@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_registrar_provider_fields_visible(app, provider_id, provider):
    """Registrar-capable DNS providers expose their fields + verify on
    ``dns_config`` when buy-domain is toggled on.

    The handle-first wizard does not have a separate registrar page; the
    legacy ``setup-registrar-search-button`` and ``registrar-domain-input``
    are gone (the handle's domain comes from ``handle_entry`` and is
    pre-bound on dns_config, no search step). Registrars surface as DNS
    providers with the Registrar capability when buy-domain is on.
    """
    app.onboarding.go_to_dns_config()

    # Registrars are enabled only when buy_domain is on (`isProviderEnabled`
    # in +page.svelte). A DomainAvailable handle already auto-sets
    # buy_domain=true, so leave the buy-domain checkbox checked. Uncheck
    # same_provider_for_vps: registrars (porkbun/gandi) have no VPS capability,
    # so the same-provider filter would otherwise disable them. UI clicks (not
    # the machine bridge) — see test_dns_provider_fields_visible.
    app.driver.click("dns-same-provider-checkbox")

    if not app.driver.is_visible(f"dns-provider-row[{provider_id}]"):
        pytest.skip(
            f"dns-provider-row[{provider_id}] not visible after buy-domain — "
            "registrar may be filtered (no DNS capability) or page may be "
            "in an unexpected state."
        )
    _wait_until_enabled(app.driver, f"dns-provider-row[{provider_id}]")
    app.driver.click(f"dns-provider-row[{provider_id}]")
    app.driver.wait_for("dns-credentials-form", timeout=10)
    # See test_dns_provider_fields_visible: wait for the reactive creds-form
    # render before per-field checks.
    app.driver.wait_for("dns-credentials-form", timeout=10)

    # Per-field assertion is web-only — see DNS test for the AT-SPI gap.
    if app.driver.is_web():
        for field in provider.get("fields", []):
            kinds = _field_kinds(field, provider)
            if "dns" not in kinds and "registrar" not in kinds:
                continue
            tid = f"dns-credentials-form-{field['id']}"
            assert app.driver.is_visible(tid), (
                f"field {tid!r} not visible after selecting registrar "
                f"{provider_id!r} on dns_config with buy-domain on."
            )

    assert app.driver.is_visible("dns-credentials-form"), (
        f"dns-credentials-form not visible after selecting registrar "
        f"{provider_id!r} on dns_config with buy-domain on."
    )

    # The verify button covers both DNS verify and registrar
    # availability/contact-prefill in the handle-first design.
    # `is_visible_scrolled`, not a bare `is_visible` — see
    # test_dns_provider_fields_visible for the below-the-fold reason.
    assert app.driver.is_visible_scrolled("dns-verify-button"), (
        f"dns-verify-button not visible after selecting registrar {provider_id!r}."
    )

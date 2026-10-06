"""Handle-first onboarding: dns_config stage.

Element IDs from ``tests/e2e-unified/ui.yaml`` (lines ~857-880):
  dns-buy-domain-checkbox, dns-same-provider-checkbox,
  dns-provider-row (indexed: ``dns-provider-row[<provider_id>]``),
  dns-set-up-later-button,
  dns-provider-link, dns-provider-open-browser-button,
  dns-provider-help-text,
  dns-credentials-form (with child ``dns-credentials-form-<field_id>``),
  dns-verify-button, dns-tld-price-display, dns-status-text,
  dns-config-back-button, dns-config-continue-button,
  error-message.

Provider visibility / enablement is driven by ``m.dnsConfig()`` plus the
generated ``providers.ts``: only providers with ``capabilities`` including
``'dns'`` show up; ``buy-domain`` filters to those with ``'registrar'``;
``same-provider`` filters to those with ``'vps'``. ``visibleDnsFields``
hides ``kinds: ['vps']`` fields unless the same-provider checkbox is on
(Hetzner is the canonical case).
"""

from __future__ import annotations

import pytest

pytestmark = pytest.mark.tier_2


def test_dns_config_renders_provider_row(app):
    """dns_config stage shows the provider row + the two checkboxes
    + the set-up-later escape hatch."""
    app.onboarding.go_to_dns_config()
    assert app.driver.count("dns-provider-row") >= 1, (
        "dns_config stage should render at least one provider row: "
        f"{app.driver.diagnose('dns-provider-row')}"
    )
    assert app.driver.is_visible("dns-buy-domain-checkbox"), (
        "dns_config should show the buy-domain checkbox: "
        f"{app.driver.diagnose('dns-buy-domain-checkbox')}"
    )
    assert app.driver.is_visible("dns-same-provider-checkbox"), (
        "dns_config should show the same-provider checkbox: "
        f"{app.driver.diagnose('dns-same-provider-checkbox')}"
    )
    assert app.driver.is_visible("dns-set-up-later-button"), (
        "dns_config should show the set-up-later escape hatch: "
        f"{app.driver.diagnose('dns-set-up-later-button')}"
    )
    # Back/Continue sit at the foot of a scrolling page: `is_visible_scrolled`,
    # not a bare `is_visible`, so a below-the-fold read (windows IsOffscreen)
    # is told apart from "never rendered". Both are counted first by the
    # `diagnose` line on failure, so an absent element still fails honestly.
    assert app.driver.count("dns-config-back-button") >= 1 and (
        app.driver.is_visible_scrolled("dns-config-back-button")
    ), (
        "dns_config should show the back button: "
        f"{app.driver.diagnose('dns-config-back-button')}"
    )
    assert app.driver.count("dns-config-continue-button") >= 1 and (
        app.driver.is_visible_scrolled("dns-config-continue-button")
    ), (
        "dns_config should show the continue button: "
        f"{app.driver.diagnose('dns-config-continue-button')}"
    )


def test_dns_config_back_returns_to_handle_entry(app):
    """``dns-config-back-button`` returns to handle_entry.

    Post-target-state: nest_select is removed; Back from dns_config
    returns to handle_entry (``handle-input`` is the canary).
    """
    app.onboarding.go_to_dns_config()
    app.driver.click("dns-config-back-button")
    app.driver.wait_for("handle-input", timeout=10)
    assert app.driver.is_visible("handle-input"), (
        "Back from dns_config should return to handle_entry (handle-input canary): "
        f"{app.driver.diagnose('handle-input')}"
    )


def test_dns_config_set_up_later_skips_to_vps(app):
    """``dns-set-up-later-button`` jumps to vps_config (machine sets
    ``dns.setUpLater = true`` and advances)."""
    app.onboarding.go_to_dns_config()
    app.driver.click("dns-set-up-later-button")
    app.driver.wait_for("vps-provider-row", timeout=10)
    assert app.driver.is_visible("vps-provider-row"), (
        "set-up-later should jump to vps_config (vps-provider-row canary): "
        f"{app.driver.diagnose('vps-provider-row')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_dns_config_buy_domain_filters_providers(app):
    """``dns-buy-domain-checkbox``: with buy-domain on, only ``registrar``-
    capable providers (Cloudflare, Porkbun, Namecheap, Gandi) stay enabled;
    non-registrar DNS providers (e.g. Hetzner, DNS+VPS but no in-wizard
    registration) are disabled. Runs on every app via the uniform
    ``is_disabled`` read (tracked internally)."""
    import time

    app.onboarding.go_to_dns_config()
    # Landing state on the DomainAvailable path: buy_domain=true AND
    # same_provider_for_vps=true (machine defaults), which together disable
    # every DNS provider (none are both registrar AND vps). Uncheck
    # same-provider so only the buy-domain (registrar) filter remains active.
    app.driver.click("dns-same-provider-checkbox")
    if not app.driver.is_visible("dns-provider-row[porkbun]"):
        pytest.skip(
            "Porkbun not in the rendered provider row — providers registry may "
            "have regressed, or the registry filter excluded it."
        )

    # Porkbun is registrar-capable → becomes enabled once same-provider is off
    # (the eligibility refresh runs on an observer tick, so poll).
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline and app.driver.is_disabled("dns-provider-row[porkbun]"):
        time.sleep(0.05)
    assert not app.driver.is_disabled("dns-provider-row[porkbun]"), (
        "registrar-capable Porkbun must stay enabled with buy-domain on"
    )
    # Hetzner is DNS+VPS but no registrar → disabled by the buy-domain filter.
    if app.driver.is_visible("dns-provider-row[hetzner]"):
        assert app.driver.is_disabled("dns-provider-row[hetzner]"), (
            "non-registrar Hetzner must be disabled with buy-domain on"
        )


def test_dns_config_subdomain_of_a_held_zone_lands_with_buy_domain_off(app):
    """A handle on a name inside a held zone lands on dns_config with
    ``dns-buy-domain-checkbox`` off, so a DNS host that is no registrar
    (Hetzner, DNS+VPS) is selectable with nothing to untick
    (``onboarding-provisioning.md`` § 4). ``example.com`` is IANA's held
    zone: a name under it has no delegation of its own, and the resolver's
    SOA names ``example.com`` as the zone cut."""
    import time

    app.onboarding.go_to_dns_config(handle="alice@not-a-real-fauna-e2e.example.com")
    assert app.driver.get_attr("dns-buy-domain-checkbox", "checked") == "false", (
        "a subdomain of a held zone must land with buy-domain off: "
        f"{app.driver.diagnose('dns-buy-domain-checkbox')}"
    )
    assert app.driver.is_visible("dns-provider-row[hetzner]"), (
        f"Hetzner must render as a DNS provider row: {app.driver.diagnose('dns-provider-row[hetzner]')}"
    )
    # Eligibility lands on an observer tick: wait for the state, bounded.
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and app.driver.is_disabled("dns-provider-row[hetzner]"):
        time.sleep(0.05)
    assert not app.driver.is_disabled("dns-provider-row[hetzner]"), (
        "the non-registrar DNS host must be selectable on landing: "
        f"{app.driver.diagnose('dns-provider-row[hetzner]')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_dns_config_select_provider_shows_credentials_form(app):
    """Selecting a DNS provider reveals the credentials form.

    Cloudflare has DNS + Registrar (no VPS). When the user lands on
    dns_config via the DomainAvailable handle-check path, the machine
    auto-sets ``buy_domain=true`` (see
    ``OnboardingMachine::submit_handle_check_continue``). The defaults
    therefore become ``buy_domain=true`` and ``same_provider_for_vps=true``,
    which together disable Cloudflare (it has Registrar, satisfying the
    first filter, but not Vps, so the second filter alone still disables it):

      - ``buy_domain && !registrar`` disables non-registrar providers
      - ``same_provider_for_vps && !vps`` disables non-VPS providers

    To click Cloudflare we need both checkboxes unchecked — the user has
    a domain already (not buying) and isn't using the same provider for
    VPS. Polling between the unchecks tolerates platforms (e.g. WinUI)
    where the toggle/binding round-trip is asynchronous."""
    import time

    app.onboarding.go_to_dns_config()
    if not app.driver.is_visible("dns-provider-row[cloudflare]"):
        pytest.skip(
            "Cloudflare not visible in the provider row — registry change "
            "or per-app filter issue."
        )

    def wait_until_enabled(element_id: str, timeout: float = 3.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if app.driver.is_enabled(element_id):
                return
            time.sleep(0.05)
        pytest.fail(
            f"{element_id!r} did not become enabled within {timeout:.0f}s — "
            "toggle/binding propagation broken on this client."
        )

    app.driver.click("dns-buy-domain-checkbox")
    app.driver.click("dns-same-provider-checkbox")
    wait_until_enabled("dns-provider-row[cloudflare]")
    app.driver.click("dns-provider-row[cloudflare]")
    app.driver.wait_for("dns-credentials-form", timeout=10)
    assert app.driver.is_visible("dns-provider-link"), (
        "selecting a DNS provider should reveal the provider link: "
        f"{app.driver.diagnose('dns-provider-link')}"
    )
    assert app.driver.is_visible("dns-verify-button"), (
        "selecting a DNS provider should reveal the verify button: "
        f"{app.driver.diagnose('dns-verify-button')}"
    )


def test_dns_config_contact_form_hidden_initially(app):
    """Contact form is in the AT-SPI tree but hidden until the user
    selects a registrar-with-requires-contact provider AND verify_dns
    reaches UnregisteredBuyable. On a fresh dns_config landing the
    provider isn't selected — form must not be visible."""
    app.onboarding.go_to_dns_config()
    assert app.driver.is_absent("dns-contact-form"), (
        "dns-contact-form should be hidden in NotReady state"
    )


def test_dns_config_contact_form_hidden_for_porkbun(app):
    """Porkbun has registrar_requires_contact: false (uses account-level
    contacts). Even if the user selects Porkbun, the form must stay
    hidden — the i18n text on dns-registrar-notes-text points to
    porkbun.com/account/settings instead. Asserts the static-frame
    state (NotReady after click, no verify); the post-verify
    UnregisteredBuyable variant of this assertion lands when the
    cross-app wiremock fixture exists."""
    app.onboarding.go_to_dns_config()
    if not app.driver.is_visible("dns-provider-row[porkbun]"):
        pytest.skip("Porkbun not visible in dns-provider-row.")
    # Porkbun is registrar-only (no VPS capability). The default machine
    # state has same_provider_for_vps checked, which disables non-VPS
    # providers (clicking throws ElementNotEnabledException on Windows;
    # other platforms handle it less strictly but the user-flow is the
    # same). Uncheck same-provider so Porkbun becomes selectable —
    # mirrors test_dns_config_select_provider_shows_credentials_form.
    app.driver.click("dns-same-provider-checkbox")
    app.driver.click("dns-provider-row[porkbun]")
    # No verify_dns call here, so provider_status() == NotReady.
    # The form must still be hidden — even after selection, NotReady
    # gates visibility.
    assert app.driver.is_absent("dns-contact-form"), (
        "dns-contact-form should be hidden in NotReady (no verify yet)"
    )

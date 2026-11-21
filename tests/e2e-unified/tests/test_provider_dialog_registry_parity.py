"""Every provider in ``i18n/providers.yaml`` shows its registry fields in its
dialog on the choose-providers page — `docs/goal/behavior/onboarding-provisioning.md`
§ 3d *Connecting a provider*.

The page lists one row per registry entry; pressing a row opens that
provider's own dialog, which "shows every field the provider's wanted
capabilities need — DNS and server fields always, registrar fields when a
domain is being bought" (generated from the registry, a ``hosted-auth`` field
as its button), and one *Connect* button. This file is that sentence walked
over the whole registry, the expected fields derived HERE from
``providers.yaml`` (never from the machine's ``visible_provider_fields``, the
code under test): a provider added to the registry joins the walk with no
edit, and a dialog missing a declared field fails it by name.

It replaces, on the page that replaced them, the registry-parity witnesses of
``test_provider_registry_parity.py``, which drive the retired ``dns_config`` /
``vps_config`` set-up pages and skip on an app that renders this one.

Two walks, one landing each (convention 17 — walk the UI systematically and
assert the general invariant, every provider in one pass, every miss reported
together):

* a domain the person brings (the handle check found it registered): every
  provider's dialog shows its DNS and server fields and Connect, and no field
  the registry declares for registration alone;
* a domain being bought: every provider that registers domains additionally
  shows its registrar fields.

Every mutation is a driver UI action (convention 8); the handle-check OUTCOME
is seeded through the sanctioned test setter, as ``test_provider_choice.py``
does, so neither walk depends on outbound DNS. Nothing is connected and no
provider is called: the dialog's fields are the subject, Connect only its
presence.
"""
from __future__ import annotations

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from generated.providers import PROVIDERS
from helpers.app_surface import require_surface
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_2

# Never a real name: neither walk calls a provider.
BROUGHT_DOMAIN = "example.test"
BOUGHT_DOMAIN = "not-a-real-fauna-e2e-dialog-parity.test"


def _kinds(field: dict, provider: dict) -> set[str]:
    """A field's kinds, defaulting to the provider's capabilities when the key
    is absent — the registry's own rule (``providers.yaml``'s header,
    ``scripts/providers-generate.py::_resolved_field_kinds``)."""
    return set(field.get("kinds") or provider.get("capabilities", []))


def _expected_fields(provider: dict, *, buying: bool) -> tuple[list[str], list[str]]:
    """(shown, hidden) field ids by § 3d's rule: DNS and server fields always,
    registrar fields when a domain is bought by a provider that registers."""
    registers = buying and "registrar" in provider.get("capabilities", [])
    shown, hidden = [], []
    for field in provider.get("fields", []):
        kinds = _kinds(field, provider)
        wanted = bool(kinds & {"dns", "vps"}) or (registers and "registrar" in kinds)
        (shown if wanted else hidden).append(field["id"])
    return shown, hidden


def _land(app, *, handle: str, outcome, message_key: str, buying: bool) -> None:
    domain = handle.split("@", 1)[1]
    app.onboarding.go_to_handle_entry()
    snapshot = {
        "phase": "Complete",
        "outcome": outcome,
        "message": {"key": message_key, "args": {"domain": domain}},
        "continue_enabled": True,
    }
    if buying:
        snapshot["control_checkbox_visible"] = False
        snapshot["control_checkbox_checked"] = False
    set_handle_check_snapshot(app, snapshot, handle=handle)
    wait_until(lambda: app.is_enabled("handle-entry-continue-button"), 10.0,
               diagnose=lambda: app.driver.diagnose("handle-entry-continue-button"))
    app.driver.click("handle-entry-continue-button")
    app.wait_for("provider-choice-continue-button")


def _walk(app, *, buying: bool) -> None:
    """Open every registry provider's dialog in turn and collect every miss."""
    drv = app.driver
    misses: list[str] = []
    walked = 0
    for pid, provider in PROVIDERS.items():
        if buying and "registrar" not in provider.get("capabilities", []):
            continue
        row = f"provider-row[{pid}]"
        if not drv.is_visible_scrolled(row):
            misses.append(f"{pid}: no {row} on the page — {drv.diagnose(row)}")
            continue
        # The page offers every provider: a row is never disabled (its
        # cannot-cover reasons are lines beneath it, not a dead button).
        if not drv.is_enabled(row):
            misses.append(f"{pid}: {row} is disabled — {drv.diagnose(row, attrs=('reason',))}")
            continue
        drv.click(row)
        app.wait_for("provider-dialog")
        walked += 1
        shown, hidden = _expected_fields(provider, buying=buying)
        for fid in shown:
            tid = f"provider-credentials-form-{fid}"
            if not drv.is_visible_scrolled(tid):
                misses.append(
                    f"{pid}: registry field {fid!r} ({sorted(_kinds_of(provider, fid))}) "
                    f"missing from its dialog — {drv.diagnose(tid)}"
                )
        for fid in hidden:
            tid = f"provider-credentials-form-{fid}"
            if not drv.is_absent(tid):
                misses.append(
                    f"{pid}: field {fid!r} is registration-only and nothing is "
                    f"bought, yet the dialog shows it — {drv.diagnose(tid)}"
                )
        if not drv.is_visible_scrolled("provider-connect-button"):
            misses.append(f"{pid}: no Connect — {drv.diagnose('provider-connect-button')}")
        drv.click("provider-dialog-close-button")
        wait_until(lambda: drv.is_absent("provider-dialog"), 10.0,
                   diagnose=lambda: drv.diagnose("provider-dialog"))
    assert walked, "no provider dialog was walked: the registry slice is empty"
    assert not misses, (
        f"{len(misses)} dialog miss(es) against i18n/providers.yaml "
        f"({'buying' if buying else 'a domain brought'}):\n  " + "\n  ".join(misses)
    )


def _kinds_of(provider: dict, fid: str) -> set[str]:
    for field in provider.get("fields", []):
        if field["id"] == fid:
            return _kinds(field, provider)
    return set()


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_every_provider_dialog_shows_its_dns_and_server_fields_for_a_domain_brought(app):
    """A registered domain: nothing is bought, so every provider's dialog shows
    exactly its DNS and server fields — a field the registry declares for
    registration alone stays out — and Connect."""
    require_surface(app.driver, "provider-choice")
    _land(
        app,
        handle=f"alice@{BROUGHT_DOMAIN}",
        outcome="RegisteredNoNest",
        message_key="onboarding.handle_check.outcome.registered_no_nest",
        buying=False,
    )
    assert app.driver.is_absent("dns-buy-domain-checkbox"), (
        "a registered domain cannot be bought: "
        + app.driver.diagnose("dns-buy-domain-checkbox")
    )
    _walk(app, buying=False)


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_every_registrar_dialog_adds_its_registrar_fields_when_a_domain_is_bought(app):
    """An unregistered domain: the machine ticks the buy box, and every provider
    that registers domains shows its registrar fields beside its DNS and server
    fields, so it is connected once, whole."""
    require_surface(app.driver, "provider-choice")
    _land(
        app,
        handle=f"alice@{BOUGHT_DOMAIN}",
        outcome={"DomainAvailable": {"buyable_via_provider": True, "price": None}},
        message_key="onboarding.handle_check.outcome.domain_available_unpriced",
        buying=True,
    )
    drv = app.driver
    app.wait_for("dns-buy-domain-checkbox")
    assert drv.get_attr("dns-buy-domain-checkbox", "checked") == "true", (
        "an unregistered domain starts bought: "
        + drv.diagnose("dns-buy-domain-checkbox", attrs=("checked",))
    )
    assert drv.is_visible("provider-goal-row[domain]"), drv.diagnose("provider-goal-row[domain]")
    _walk(app, buying=True)

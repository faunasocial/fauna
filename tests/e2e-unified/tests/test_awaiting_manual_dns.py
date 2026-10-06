"""Onboarding: the "Almost ready" (awaiting-manual-DNS) surface.

Reached after the deferred-DNS provisioning path — the user provisioned their
own nest but chose "Set up later" for DNS — so the wizard exits to ``Done`` with
``wizard_outcome() == AwaitingManualDns`` and the client renders this polling
surface. Per ``docs/goal/behavior/onboarding.md`` § "Almost ready" surface, it is
**not** an ``OnboardingStep``: the client renders it off ``wizard_outcome()``,
which is why the same-session exit and the relaunch-hydration path look identical.

Seeded straight to the exit via the ``seed_awaiting_manual_dns`` E2E bridge (the
same call the relaunch-hydration path makes), so no real cloud provisioning is
needed. This is a render/actionability test; the recheck's claim ceremony needs a
reachable nest and is covered by the launch-routing / persistence suites.

Element IDs (``ui.yaml`` ``onboarding.awaiting_manual_dns``):
  awaiting-dns-records, awaiting-dns-status, awaiting-dns-recheck-button,
  awaiting-dns-copy-button, awaiting-dns-fallthrough-button.

The exit ("Use a different nest", ``onboarding-provisioning.md`` § "Almost ready"
surface → *Exit*) is landed here; its DURABLE half — the slot really is gone, so a
cold relaunch no longer resumes the box — needs the fake cloud and a durable
client store, so it is the tier_3 journey in ``test_provisioning_slot_recovery.py``.
"""

from __future__ import annotations

import json

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.budgets import UI_SETTLE_S

pytestmark = pytest.mark.tier_2

# `awaiting-dns-fallthrough-button` ("Use a different nest") landed on tui — the
# lead app — 2026-09-25 (rule-A approved), then windows; every other app still
# carries only the generated UiIds constant, unused.
_FALLTHROUGH_BUTTON_APPS = ("tui", "windows")


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_awaiting_manual_dns_surface_renders(app):
    """The four surface elements render, and the seeded DNS record value appears
    in the records block (the shared ``awaiting_dns_records_text()`` output that
    both the label and the copy button read)."""
    app.onboarding.go_to_awaiting_manual_dns(["A nest.example.com 203.0.113.7"])
    for eid in (
        "awaiting-dns-records",
        "awaiting-dns-status",
        "awaiting-dns-recheck-button",
        "awaiting-dns-copy-button",
    ):
        assert app.driver.is_visible(eid), (
            f"'Almost ready' surface should render {eid}: "
            f"{app.driver.diagnose(eid)} error={app.error_text()!r}"
        )
    if app_name(app.driver) not in _FALLTHROUGH_BUTTON_APPS:
        skip_unbuilt(
            app.driver,
            surface="awaiting-dns-fallthrough-button",
            detail="the Almost-ready exit landed on tui (lead app) 2026-09-25",
            tracked="onboarding-provisioning.md § \"Almost ready\" surface → Exit",
        )
    assert app.driver.is_visible("awaiting-dns-fallthrough-button"), (
        "'Almost ready' surface should render awaiting-dns-fallthrough-button: "
        f"{app.driver.diagnose('awaiting-dns-fallthrough-button')} error={app.error_text()!r}"
    )
    records = app.driver.get_text("awaiting-dns-records")
    assert "203.0.113.7" in records, (
        "seeded DNS record value should appear in awaiting-dns-records "
        f"(awaiting_dns_records_text() empty — seed_awaiting_manual_dns wiring?): "
        f"{records!r}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_awaiting_manual_dns_recheck_and_copy_actionable(app):
    """``awaiting-dns-recheck-button`` is enabled at rest (a single-shot probe;
    disabled only while a probe/claim is in flight) and the copy button is
    present."""
    app.onboarding.go_to_awaiting_manual_dns()
    assert app.driver.is_enabled("awaiting-dns-recheck-button"), (
        "recheck button should be enabled at rest: "
        f"{app.driver.diagnose('awaiting-dns-recheck-button')} error={app.error_text()!r}"
    )
    assert app.driver.is_visible("awaiting-dns-copy-button"), (
        "copy button should be present: "
        f"{app.driver.diagnose('awaiting-dns-copy-button')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_awaiting_manual_dns_fallthrough_lands_on_handle_entry(app):
    """"Use a different nest" is the way off a box that will never answer: the
    click leaves the surface and lands the wizard on ``handle_entry``.

    The identity is seeded first, exactly as the relaunch-hydration glue does
    (``seed_identity`` then the awaiting seeder): the landing is "a different nest,
    same self", so an identity-less machine has nowhere to land but the start.
    """
    if app_name(app.driver) not in _FALLTHROUGH_BUTTON_APPS:
        skip_unbuilt(
            app.driver,
            surface="awaiting-dns-fallthrough-button",
            detail="the Almost-ready exit landed on tui (lead app) 2026-09-25",
            tracked="onboarding-provisioning.md § \"Almost ready\" surface → Exit",
        )
    app.driver.call_machine_method(
        "seed_identity", json.dumps(app.onboarding._IMPORT_KEY_FOR_HANDLE_TESTS)
    )
    app.onboarding.go_to_awaiting_manual_dns()
    assert app.driver.is_enabled("awaiting-dns-fallthrough-button"), (
        "the exit must be live at rest — it is the only way off a box that never "
        f"answers: {app.driver.diagnose('awaiting-dns-fallthrough-button')}"
    )

    app.click("awaiting-dns-fallthrough-button")

    try:
        app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)
    except Exception as exc:
        raise AssertionError(
            "clicking 'Use a different nest' did not land on handle_entry "
            f"(handle-input never rendered): error={app.error_text()!r}"
        ) from exc
    assert app.driver.is_absent("awaiting-dns-status"), (
        "the exit must retire the surface, not layer handle_entry over it — the "
        "wizard outcome the surface renders off is what the exit clears"
    )

"""Handle-first onboarding: dns_post_instructions stage.

Element IDs from ``tests/e2e-unified/ui.yaml`` (lines ~907-915):
  dns-post-instructions-text, dns-post-instructions-copy-button,
  dns-post-instructions-continue-button.

Reached only when ``dns.setUpLater == true`` and VPS provisioning
returned a ``DeferredDnsResult``; the markdown comes from
``m.dns_post_instructions()`` (derived from the captured DNS records +
the successful ``ProvisioningSnapshot.result``).

``OnboardingActions.go_to_dns_post_instructions_with_records`` reaches
this stage by seeding both via the ``call_machine_method`` E2E bridge
(``set_dns_records_for_test`` + ``set_provisioning_snapshot_for_test`` +
``set_step_for_test``) — no real cloud provisioning needed.
"""

from __future__ import annotations

import time

import pytest

pytestmark = pytest.mark.tier_2


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_dns_post_instructions_shows_seeded_records(app):
    """After deferred-DNS provisioning, the markdown text + copy/continue
    buttons are visible and the seeded record value appears in the text.

    The text is populated by the dns_post_instructions page's refresh
    closure reading ``m.dns_post_instructions()`` after the bridge
    setters land; that derivation can trail the page mount by a tick on
    some toolkits, so poll the text rather than reading it once.
    """
    app.onboarding.go_to_dns_post_instructions_with_records(
        ["A example.com 1.2.3.4"]
    )
    assert app.driver.is_visible("dns-post-instructions-text"), (
        "dns_post_instructions page should render the records text: "
        f"{app.driver.diagnose('dns-post-instructions-text')} error={app.error_text()!r}"
    )
    assert app.driver.is_visible("dns-post-instructions-copy-button"), (
        "dns_post_instructions page should render the copy button: "
        f"{app.driver.diagnose('dns-post-instructions-copy-button')} error={app.error_text()!r}"
    )
    assert app.driver.is_visible("dns-post-instructions-continue-button"), (
        "dns_post_instructions page should render the continue button: "
        f"{app.driver.diagnose('dns-post-instructions-continue-button')} error={app.error_text()!r}"
    )
    text = ""
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        text = app.driver.get_text("dns-post-instructions-text")
        if "1.2.3.4" in text:
            return
        time.sleep(0.3)
    assert "1.2.3.4" in text, (
        "seeded DNS record value never appeared in dns-post-instructions-text "
        f"(m.dns_post_instructions() empty — set_dns_records_for_test / "
        f"set_provisioning_snapshot_for_test bridge wiring?): {text!r}"
    )


def test_dns_post_instructions_continue_button_ready(app):
    """``dns-post-instructions-continue-button`` is present and enabled.

    Per ``docs/goal/behavior/onboarding.md`` §7 / §"Wizard exit handling",
    clicking it calls ``continue_from_dns_post_instructions()`` →
    ``OnboardingStep::Done`` with ``wizard_outcome() == AwaitingManualDns``;
    the client saves the awaiting-manual-DNS slot and *leaves the wizard*
    (Linux/macOS close the window — there's no in-app surface to land on).
    The "Almost ready" polling surface that the wizard should hand off to
    isn't built on any client yet (tracked in
    tracked internally), so this test stops at the
    button being actionable rather than clicking it (which would terminate
    the app and poison the shared driver for later tests). Extend to click
    Continue + assert the surface once it lands.
    """
    app.onboarding.go_to_dns_post_instructions_with_records(
        ["A example.com 1.2.3.4"]
    )
    assert app.driver.is_visible("dns-post-instructions-continue-button"), (
        "dns_post_instructions continue button should be present: "
        f"{app.driver.diagnose('dns-post-instructions-continue-button')} error={app.error_text()!r}"
    )
    assert app.driver.is_enabled("dns-post-instructions-continue-button"), (
        "dns_post_instructions continue button should be enabled: "
        f"{app.driver.diagnose('dns-post-instructions-continue-button')} "
        f"error={app.error_text()!r}"
    )

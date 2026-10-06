"""tier_3 e2e: the Settings → Account **Recovery kit** section.

Goal docs: ``docs/goal/ui/settings.md`` § Recovery kit (the four states, the
placement, which action each state enables) and
``docs/goal/behavior/identity-succession.md`` § The RecoveryKey (the ceremony
behind the button, and the offline-only custody rule the display obeys).

What this pins, and why each is worth an e2e rather than a unit test:

- **The create ceremony reaches the nest and the status flips.** The unit tests
  cover the projection and the render; only a full-stack run proves the button
  is wired to a ceremony that actually lands a registration on a real
  ``fauna-nest`` — the gap between "the state machine is right" and "the app
  talks to the server".
- **The status is read from the account, not from a local flag.**
  ``settings.md`` § Recovery kit requires this explicitly ("never a local
  flag, so a kit created on another device is reflected here"). Re-entering
  the page re-reads it, so a state that survives navigation is evidence the
  answer came from the nest.
- **The minted secret is shown once and is not recoverable from the UI.**
  ``identity-succession.md`` § The RecoveryKey — *Custody* makes this
  iron-clad: nothing on the device keeps a copy, so leaving the page must not
  be able to bring it back. That is a property no unit test of the renderer
  can establish, because the renderer is exactly what would be wrong.

Every assertion is latency-independent (convention 14): the ceremony's status
re-read rides back on the same outcome as the mint, so there is no second hop
to wait on, and the waits below are generous deadline polls rather than
settle-sleeps.

tier_3: needs a real ``fauna-nest`` binary. Runs on any app whose UI has the
section — today tui, the lead app; the others join as their legs land.
"""
from __future__ import annotations

import pytest

from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

# The secret is 32 bytes as lowercase hex.
_SECRET_HEX_LEN = 64


@pytest.mark.feature("recovery-kit")
def test_creating_a_recovery_kit_flips_the_status_and_shows_the_secret_once(
    ungranted_app, nest_instance
):
    """create → the secret is shown once, the status leaves never-created, and
    leaving the page does not bring the secret back.

    Uses ``ungranted_app`` — a dedicated fresh actor — deliberately: the
    ceremony writes to the account's registration chain, so running it against
    a session-scoped shared user would make test ORDER load-bearing for
    everyone else reading that account.
    """
    app = ungranted_app
    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()

    # The never-created state: create is the offered action, and replacing
    # something that does not exist is not.
    before = app.settings.recovery_kit_status()
    assert before, (
        "the section must render a status line once the read resolves; "
        f"error surface says: {app.error_text()!r}"
    )
    assert app.is_enabled("recovery-kit-create-button"), (
        "create is the never-created state's action, but it is disabled — "
        f"status reads {before!r}, error surface: {app.error_text()!r}"
    )

    app.settings.create_recovery_kit()

    # The secret rides back with the ceremony, so it is on screen by the time
    # the click returns; the wait is a generous ceiling, not a settle.
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    secret = app.settings.recovery_kit_secret()
    assert len(secret) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(secret)} chars; "
        f"error surface: {app.error_text()!r}"
    )
    assert secret == secret.lower().strip(), "the secret renders as bare lowercase hex"

    # The status re-read rides the same outcome, so it is already current.
    after = app.settings.recovery_kit_status()
    assert after != before, (
        "a landed registration must move the status off never-created; "
        f"still reads {before!r}"
    )
    assert not app.is_enabled("recovery-kit-create-button"), (
        "a kit IS registered now; offering create again would open the wrong ceremony"
    )

    # Custody: leaving and returning must not re-show the secret. This is the
    # assertion that would catch a well-meaning future "let them see it again"
    # cache — the one feature this design can never have.
    app.settings.navigate()
    app.settings.open_recovery_kit()
    assert app.settings.recovery_kit_secret() == "", (
        "the secret is shown ONCE — nothing on the device keeps a copy, so "
        "re-entering the page must not be able to display it again"
    )

    # …but the *status* survives navigation, which is the evidence it came
    # from the account rather than from a flag the ceremony set locally.
    assert app.settings.recovery_kit_status() == after, (
        "the status is read from the registration chain, so it must survive a "
        "fresh navigation unchanged"
    )


@pytest.mark.feature("recovery-kit")
def test_replacing_the_kit_with_the_one_you_hold_mints_a_different_secret(
    ungranted_app, nest_instance
):
    """create → replace-with-the-held-kit → a NEW secret, and the old one is dead.

    This is the kit-in-hand half (`settings.md` § Recovery kit → *Kit-in-hand
    entry*, user-approved 2026-08-01): the ceremony reads the phrase the user
    pastes into ``recovery-entry-phrase-field`` and drives ``create_kit``'s
    ``prior`` arm. Worth a full-stack run rather than a unit test for two
    reasons the renderer cannot answer:

    - **The pasted kit actually authorizes the replacement.** The registration
      is co-signed by the prior root, and the nest verifies it against the
      chain it already holds. Only a real ``fauna-nest`` proves the app sent a
      chain-valid record rather than a well-formed one.
    - **Replaying the OLD phrase must now fail.** The chain advanced, so the
      superseded root can no longer author the next registration — the property
      that makes "replace" meaningful rather than cosmetic.

    Latency-independent throughout (convention 14): each ceremony's status
    re-read rides back on the same outcome as its mint, so there is no second
    hop, and the waits are generous deadline polls.
    """
    app = ungranted_app
    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()

    # Register the first kit, and keep its phrase — it is the "kit in hand".
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN

    # The field renders only now that a ceremony can consume it — that is what
    # makes it `optional_elements` rather than a permanent fixture of the page.
    app.settings.navigate()
    app.settings.open_recovery_kit()
    assert app.is_visible("recovery-entry-phrase-field"), (
        "a registered account can be replaced, so the kit-in-hand field must "
        f"be there to hold the phrase; error surface: {app.error_text()!r}"
    )
    assert app.is_enabled("recovery-kit-replace-button"), (
        "replace is the registered state's kit-in-hand action, but it is "
        f"disabled; status reads {app.settings.recovery_kit_status()!r}, "
        f"error surface: {app.error_text()!r}"
    )

    app.settings.replace_kit_with_held(held)

    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    replacement = app.settings.recovery_kit_secret()
    assert len(replacement) == _SECRET_HEX_LEN, (
        f"the replacement kit is 64-hex, got {len(replacement)}; "
        f"error surface: {app.error_text()!r}"
    )
    assert replacement != held, (
        "a replacement mints a NEW root — returning the same secret would mean "
        "the ceremony did not actually rotate anything"
    )

    # The account still reads as registered: replace does not open a window
    # (that is the seed-alone `lost` path), it takes effect immediately.
    assert not app.is_enabled("recovery-kit-create-button"), (
        "a kit is still registered after a replacement"
    )

    # The OLD phrase is now superseded — the chain moved past it, so re-using
    # it must be refused rather than quietly minting a third kit.
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.replace_kit_with_held(held)
    # Anchor on the ceremony having ANSWERED — either way — rather than on a
    # settle-sleep, then assert WHICH answer it gave (convention 14).
    wait_until(
        lambda: bool(app.error_text()) or bool(app.settings.recovery_kit_secret()),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"status={app.settings.recovery_kit_status()!r} "
            f"error={app.error_text()!r}"
        ),
    )
    assert not app.settings.recovery_kit_secret(), (
        "the superseded root must not be able to author the next registration; "
        "a kit came back, which means the replay was accepted"
    )
    assert app.error_text(), (
        "the refusal must reach error-message rather than failing silently"
    )

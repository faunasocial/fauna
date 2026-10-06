"""An identity code that carries a handle fills the handle step in for you.

`docs/goal/behavior/onboarding.md` § 1. Identity: "`identity_import` accepts a
pasted secret or a scanned QR. QR payload is `(identity, handle)`. When a handle
is present in the payload, the app calls `set_current_handle(handle)` before
navigating to step 2 so the input pre-fills; **if absent, step 2 starts empty**."

This is the second-device journey: you export your identity from the app you
already use, and the code carries who you are as well as your key, so the new
device does not ask you to retype a handle it was just told. Pasting the URI
form is enough — no camera, which is the only reason a terminal can witness it
at all.

**Both halves are here on purpose.** The pre-fill assertion alone would pass on
an app that pre-filled the handle step from anything — a cached value, a
default, a previous test's leftover. The bare-secret case is the control that
says the handle came out of the URI and nowhere else, and it is also its own
clause of the contract ("if absent, step 2 starts empty").

The parse itself is shared Rust (`fauna_core::identity_qr::parse_import_input`,
the union of every app's accepted forms) and is unit-pinned there. What no unit
test can say is that the app routes its paste field through that parser and
carries the handle across the step transition — tui does it at
`apps/fauna-tui/src/wizard/mod.rs`'s `ConfirmImportedIdentity`, and the value
has to survive `confirm_imported_identity`'s `reset_handle_check` to show up.
The suite's only other `fauna://identity?secret=` is a NEGATIVE test on the
recovery-phrase field (`test_recovery_kit_restore.py`), and no test anywhere
passes a `&handle=`.
"""

from __future__ import annotations

import pytest

pytestmark = pytest.mark.tier_2

# A deterministic 32-byte test key. Which key is irrelevant to the handle
# branch; a stable one keeps an identity-stage flake from reading as a
# pre-fill failure.
_SECRET_HEX = "00000000000000000000000000000000000000000000000000000000000000b2"
_HANDLE = "alice@fauna.social"


def _handle_step_text(app) -> str:
    """Wait for the handle step, then read `handle-input` — or fail naming why.

    An import the app rejects leaves the wizard on `identity_import` with its
    localized error, so a bare `wait_for("handle-input")` times out with nothing
    pointing at the cause (`OnboardingActions.import_key` records the same trap).
    Convention 6: fold the wizard's own error into the failure.
    """
    try:
        app.driver.wait_for("handle-input", timeout=15)
    except TimeoutError as exc:
        raise AssertionError(
            "the import never reached the handle step — handle-input did not "
            "appear (a paste the app rejects leaves the wizard on identity_import "
            f"with its localized error). error={app.error_text()!r}"
        ) from exc
    return app.driver.get_text("handle-input")


@pytest.mark.feature("identity-on-another-device")
def test_an_identity_code_carrying_a_handle_prefills_the_handle_step(app):
    """Paste `fauna://identity?secret=…&handle=…` → the handle step opens filled in."""
    ob = app.onboarding
    ob.navigate_to_status()
    # `import_key` types its argument verbatim into `paste-secret-field`, so the
    # URI reaches the shared parser exactly as a scanned QR payload would.
    ob.import_key(f"fauna://identity?secret={_SECRET_HEX}&handle={_HANDLE}")

    prefilled = _handle_step_text(app)
    assert prefilled == _HANDLE, (
        "an identity code carrying a handle must pre-fill the handle step "
        f"(onboarding.md § 1. Identity), but handle-input reads {prefilled!r} "
        f"instead of {_HANDLE!r}. Either the paste did not reach "
        "fauna_core::identity_qr::parse_import_input, or the parsed handle was "
        "not set on the machine before the step transition. "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("identity-on-another-device")
def test_an_identity_code_without_a_handle_leaves_the_handle_step_empty(app):
    """The control for the test above, and its own half of the contract: a bare
    secret carries no handle, so step 2 starts empty rather than guessing one."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(_SECRET_HEX)

    prefilled = _handle_step_text(app)
    assert not prefilled, (
        "a bare secret carries no handle, so the handle step must start empty "
        f"(onboarding.md § 1. Identity), but handle-input reads {prefilled!r}. "
        "A non-empty value here means the pre-fill in the sibling test proves "
        "nothing about the URI — something else is filling this field. "
        f"error={app.error_text()!r}"
    )

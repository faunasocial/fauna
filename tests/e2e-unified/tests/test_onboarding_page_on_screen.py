"""An onboarding wait that times out names the page the app is on.

`docs/goal/architecture/e2e-conventions.md` § point 6 (*failures must diagnose
themselves*). tier_1: `OnboardingActions.page_on_screen` and `wait_for_page` are
pure functions over a duck-typed driver, so they pin without a nest or an app.

**The failure this closes.** `test_onboarding_dns_glue_windows.py` waited on
`handle-input` straight after `identity-continue-button`, and failed with
`Element 'handle-input' not visible after 20s … count=0` for days. That message
cannot tell a skipped wizard step (since 2026-08-01 Continue lands on
`recovery_kit`, `onboarding.md` § 1 Identity) from a leftover signed-in session
from a blank frame, so the first reading of it chased the wrong commits.
"""

import sys
from pathlib import Path

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_1]

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from actions.onboarding import OnboardingActions  # noqa: E402


class _Driver:
    """Paints exactly `visible`; a `wait_for` on anything else times out at once."""

    def __init__(self, visible=(), authenticated=False):
        self._visible = set(visible)
        self._authenticated = authenticated

    def is_visible(self, element_id, *, scope=None):
        return element_id in self._visible

    def wait_for(self, element_id, timeout=10.0, *, scope=None):
        if element_id not in self._visible:
            raise TimeoutError(f"Element '{element_id}' not visible after {timeout}s")

    def get_state(self, path=None, **_kw):
        assert path == "session.authenticated"
        return self._authenticated


def test_a_handle_wait_parked_on_the_recovery_kit_names_the_recovery_kit():
    onboarding = OnboardingActions(_Driver(visible={"recovery-kit-skip-button"}))
    with pytest.raises(TimeoutError) as caught:
        onboarding.wait_for_handle_entry(timeout=0)
    message = str(caught.value)
    assert "handle_entry never appeared" in message
    assert "recovery_kit (recovery-kit-skip-button)" in message
    # The driver's own timeout text still rides along.
    assert "Element 'handle-input' not visible" in message


def test_a_signed_in_shell_is_named_as_such():
    onboarding = OnboardingActions(_Driver(visible={"feed-view"}, authenticated=True))
    where = onboarding.page_on_screen()
    assert "the signed-in shell (feed-view)" in where
    assert "session.authenticated=True" in where


def test_a_blank_frame_says_no_wizard_page():
    assert OnboardingActions(_Driver()).page_on_screen().startswith("no wizard page")


def test_a_shared_marker_names_both_pages():
    where = OnboardingActions(_Driver(visible={"paste-secret-field"})).page_on_screen()
    assert "identity_import or emergency_lockout_entry (paste-secret-field)" in where


def test_a_page_that_appears_returns_without_a_reading():
    OnboardingActions(_Driver(visible={"handle-input"})).wait_for_page("handle_entry", timeout=0)


def test_every_marker_is_a_ui_yaml_id():
    """A marker that is not a ui.yaml ID would read False on every app, so the
    page it stands for could never be named."""
    from generated import ui_ids

    known = {v for k, v in vars(ui_ids).items() if k.isupper() and isinstance(v, str)}
    markers = set(OnboardingActions._PAGE_MARKERS.values())
    assert markers <= known, f"not ui.yaml ids: {sorted(markers - known)}"

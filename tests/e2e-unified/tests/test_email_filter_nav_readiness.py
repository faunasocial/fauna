"""Windows nav-readiness regression: a filter row's edit affordance must read
correctly on a check issued IMMEDIATELY after navigating, even when the panel's
async load is slow.

Root cause: the windows ``TestAgent`` flips
``ready=true`` right after the synchronous nav frame-kickoff
(``SettingsShellPage.NavigateInner``), because ``SettingsPrivacyPage`` did not
implement ``IAsyncLoadedPage`` — so ``App.ArmNavLoad`` had nothing to arm from,
and ``set_state({nav -> privacy})`` returned before ``EmailFilterControl``'s own
``Panel_Loaded`` -> ``LoadFiltersAsync`` (a real ``EmailFiltersListAsync`` round
trip) finished repopulating the filter list and recomputing each row's
``EditVisibility``. Any single-shot read right after a fresh navigation to
Settings → Privacy (no scroll or fold involved) could observe a stale/empty
list; ``filter_edit_visible()`` is the one caller with no retry loop, which is
why it — and not ``filter_count()``/``filter_names()``, both used inside
polling loops elsewhere — was the one to surface it.

The flake itself is timing-dependent (it needs a slow-enough load), so this
reproduces it DETERMINISTICALLY via the ``email_filter_load_delay_ms`` e2e
command, which injects a one-shot delay into
``EmailFilterPanel.LoadFiltersAsync`` so the list stays stale well past an
immediate post-navigate read. Pre-fix the read observes the stale list;
post-fix the agent holds ``ready=false`` until both ``SettingsPrivacyPage``'s
own load AND the hosted panel's load complete (the combined
``IAsyncLoadedPage`` barrier), so ``filter_edit_visible()`` returns only once
the row is genuinely there.

Windows-only: the nav-readiness barrier + the ``email_filter_load_delay_ms``
command are the windows ``TestAgent`` mechanism; the other apps' nav
readiness is a different path.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# Comfortably longer than the Python round-trip so a pre-fix immediate read
# reliably observes the stale list, yet well under the nav-readiness budget
# (App.TestAgent ~8s) and the set_state timeout (10s) so a post-fix nav still
# settles.
_LOAD_DELAY_MS = 1500


@pytest.mark.feature("mail-filter-rules")
def test_filter_edit_visible_correct_under_slow_load(logged_in_app):
    """create -> arm a slow reload -> immediate filter_edit_visible() must see
    the real (post-load) state, not whatever was in the tree at frame-nav time."""
    app = logged_in_app
    settings = app.settings
    settings.navigate()

    name = "Nav readiness probe"
    settings.create_email_filter(
        name=name, rule_type="SenderIs",
        rule_value="probe@example.com", action="Discard",
    )
    names = settings.filter_names()
    assert name in names, f"create didn't land; have {names}"
    index = names.index(name)

    try:
        # Arm a one-shot slow load for the NEXT EmailFilterPanel load.
        app.driver.set_state({"email_filter_load_delay_ms": _LOAD_DELAY_MS})

        # Post-fix, this returns only after SettingsPrivacyPage's combined
        # IAsyncLoadedPage barrier completes (both the page's own load and the
        # panel's delayed reload); pre-fix it returns after the synchronous
        # frame-nav kickoff, while the panel is still mid-delay.
        visible = settings.filter_edit_visible(index)

        assert visible, (
            "a filter the create dialog itself produced must offer an edit "
            "affordance, even when read immediately after a navigate that raced "
            "a slow EmailFilterPanel reload (nav readiness must await the "
            "panel's own load, not just the frame-nav kickoff)"
        )
    finally:
        # One-shot already consumes the delay on load, but clear it defensively
        # so a failed navigate can't leak a slow load into a later
        # session-scoped test.
        app.driver.set_state({"email_filter_load_delay_ms": 0})

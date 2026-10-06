"""Windows nav-readiness regression: the folder wizard must open on an add-click
issued IMMEDIATELY after navigating, even when the page's async load is slow.

This is the deterministic guard for the whole-file ``test_folders.py --client windows``
wizard-render flake (2-4/11 RED batched, green in isolation). Root cause: the windows
``TestAgent`` flips ``ready=true`` right after the
synchronous nav frame-kickoff (``NavigateToSettingsSubPage``), NOT after
``FoldersPage.Page_Loaded`` finishes its async ``_machine`` build. So ``set_state({nav ->
folders})`` returned while ``_machine`` was still null, and the shared
``create_folder_via_wizard`` action's immediate ``folder-add-button`` click hit
``FolderAdd_Click``'s ``if (_machine is null) return`` and was silently dropped ->
``wizard-name-input`` 8s timeout. The window widens with the session-accumulated folder
count (``nest_instance``/``test_user`` are session-scoped), which is why it fails batched
but passes in isolation.

The flake itself is timing-dependent (it needs a slow-enough load), so this reproduces it
DETERMINISTICALLY via the ``folder_load_delay_ms`` e2e command, which injects a one-shot
delay into ``FoldersPage.Page_Loaded`` so ``_machine`` stays null well past the immediate
add-click. Pre-fix the click is dropped and the wizard never opens; post-fix the agent
holds ``ready=false`` until ``Page_Loaded`` completes (the ``IAsyncLoadedPage`` barrier),
so ``navigate_folders`` returns only once the page is loaded and the add-click lands.

Windows-only: the nav-readiness barrier + the ``folder_load_delay_ms`` command are the
windows ``TestAgent`` mechanism; the other apps' nav readiness is a different path.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# Comfortably longer than the Python click round-trip so a pre-fix immediate add-click
# reliably lands while _machine is still null, yet well under the nav-readiness budget
# (App.TestAgent ~8s) and the set_state timeout (10s) so a post-fix nav still settles.
_LOAD_DELAY_MS = 1500


def test_wizard_opens_on_immediate_add_click_under_slow_load(logged_in_app):
    """navigate -> immediate add-click must open the wizard even with a slow page load."""
    app = logged_in_app
    b = app.backups
    try:
        # Arm a one-shot slow load for the NEXT Folders page-load.
        app.driver.set_state({"folder_load_delay_ms": _LOAD_DELAY_MS})

        # Post-fix, this returns only after Page_Loaded completes (the barrier);
        # pre-fix it returns after the synchronous frame-nav kickoff, while _machine
        # is still null in the injected delay.
        b.navigate_folders()

        # The immediate add-click the shared create-folder action does — no wait
        # between navigate and add. Pre-fix this is dropped (_machine null); post-fix
        # the page is loaded so it opens the wizard.
        b.add_folder()

        app.driver.wait_for("wizard-name-input", timeout=5)
        assert app.driver.is_visible("wizard-name-input"), (
            "the wizard must open on an add-click issued immediately after navigating, "
            "even under a slow page load (nav readiness must await Page_Loaded, not just "
            f"the frame-nav kickoff): {app.driver.diagnose('wizard-name-input')} "
            f"error={app.error_text()!r}"
        )
    finally:
        # One-shot already consumes the delay on load, but clear it defensively so a
        # failed navigate can't leak a slow load into a later session-scoped test.
        app.driver.set_state({"folder_load_delay_ms": 0})

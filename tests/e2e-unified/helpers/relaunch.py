"""Closing the app and opening it again, as the same device — what the mail
wizards' "after a restart" journeys need (`mail-export.md` § Resume,
`mailbox-migration.md` § Resume protocol).

A restart of the same app is not a new device: the relaunched process must come
back signed in, on the store the closed one wrote. `require_relaunchable` skips
(as unbuilt) on a driver that cannot promise that, and `relaunch` performs it —
an unclean kill first where the driver owns its app child, so the journey sees
what a crash leaves behind, then the driver's own cold relaunch.
"""

from __future__ import annotations

from helpers.app_surface import skip_unbuilt


def require_relaunchable(driver) -> None:
    """The relaunch journeys need the app back, signed in, on the same store —
    a restart of the same app, not a new device (the
    `test_settings.py::test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch`
    gates)."""
    if not driver.preserve_state_across_relaunch():
        skip_unbuilt(
            driver,
            surface="a client store that can be pinned across a relaunch",
            detail="the driver hands the relaunched process a fresh store, so the "
                   "app returns as a new device rather than the one that was running "
                   "the job",
            tracked="drivers/http_bridge.py::preserve_state_across_relaunch",
        )
    if not driver.relaunch_preserves_injected_identity():
        skip_unbuilt(
            driver,
            surface="a login that survives a relaunch",
            detail="logged_in_app injects the session with set_state, and on this app "
                   "the injection does not reach a store the relaunch keeps",
            tracked="drivers/http_bridge.py::relaunch_preserves_injected_identity",
        )


def relaunch(driver, *, crash: bool = False) -> None:
    """Close the app and open it again, signed back in. With ``crash`` the app
    is SIGKILLed first where the driver owns its child (`kill_uncleanly`), so
    nothing it would do on a clean exit — pausing a job, flushing a write — gets
    to happen; elsewhere the relaunch's own teardown closes it."""
    from common.launch_harness import reached_authenticated_app

    if crash and driver.supports_unclean_kill():
        driver.kill_uncleanly()
    assert driver.recover(), "the app did not come back up after the relaunch"
    reached_authenticated_app(driver, timeout=90)

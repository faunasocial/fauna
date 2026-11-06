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

from helpers.app_surface import require_surface


def require_relaunchable(driver) -> None:
    """The relaunch journeys need the app back, signed in, on the same store —
    a restart of the same app, not a new device (the
    `test_settings.py::test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch`
    gates).

    Which drivers can promise that is the surface registry's two harness
    entries, `relaunch-store-pin` and `relaunch-injected-identity`, each pinned
    against the drivers' own predicates by `test_app_surface_declarations.py`.
    The store pin is still CALLED, because it is what pins the store: on a
    driver the registry declares able, a refusal is this launch's failure."""
    pin_store_across_relaunch(driver)
    require_surface(driver, "relaunch-injected-identity")


def pin_store_across_relaunch(driver) -> None:
    """Pin this launch's client-local store so the next relaunch reads back what
    this process wrote — the gate a client-side durability journey takes first.

    Which drivers can pin is the registry's `relaunch-store-pin` (pinned against
    every leaf driver's override); the pin itself is still CALLED, because the
    call is what pins, and on a driver the registry declares able a refusal is
    this launch's failure, never a skip — a fresh store would make "the slot did
    not survive" indistinguishable from "the client never wrote it"."""
    require_surface(driver, "relaunch-store-pin")
    assert driver.preserve_state_across_relaunch(), (
        "the driver could not pin this launch's client-local store, so the "
        "relaunched app would return as a new device rather than the one that "
        "wrote it (drivers/http_bridge.py::preserve_state_across_relaunch)"
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

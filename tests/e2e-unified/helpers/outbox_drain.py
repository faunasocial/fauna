"""The e2e outbox drain hold: keep a seat's queued intents queued while the nest
is reachable.

A queued ``OfflineQueued`` intent (a like given with no nest, say) drains in the
first pump pass that reaches the nest, so no window exists in which the nest is
up and the intent still queued. A journey that needs that window holds the
drain with :func:`hold_outbox_drain` and lifts it with
:func:`release_outbox_drain`. The hold is ``fauna_account_plane::outbox``'s
``outbox_drain_held``, compiled into debug and ``e2e-agent`` builds only
(convention 15), and it reaches an app one of two ways:

* **The hold file** (tui, linux, macos, ios): the file the launch's
  ``FAUNA_E2E_OUTBOX_DRAIN_HOLD_FILE`` names, created to hold and deleted to
  release. It is a file because the process that pumps inherits the
  launch's environment (on linux and tui the sync agent the app spawns).
* **The in-process hold** (web, android): a web tab has no environment or
  filesystem and an android app inherits nothing from the harness, but there
  the pump runs in the process the test drives, so the driver's
  ``set_outbox_drain_hold`` sets a flag in it through the e2e agent
  (``fauna_e2e_agent::OUTBOX_DRAIN_HOLD``).

The hold covers the generic drain and the app's MLS and identity drains alike;
the succession outbox carry keeps running. A held pass ends the app's drain
loop, so a journey releasing a queued MLS or identity intent follows
:func:`release_outbox_drain` with :func:`ring_app_outbox_drain`
(``fauna_e2e_agent::APP_OUTBOX_DRAIN_NOW``).
"""
from __future__ import annotations

from pathlib import Path


def _hold_file(app) -> Path | None:
    path = getattr(app.driver, "outbox_drain_hold_file", lambda: None)()
    return Path(path) if path else None


def _set_in_process(app, held: bool) -> bool:
    """Set the in-process hold when the driver has one; whether it had one."""
    setter = getattr(app.driver, "set_outbox_drain_hold", None)
    if setter is None:
        return False
    setter(held)
    return True


def hold_outbox_drain(app) -> None:
    """Hold this seat's outbox drain until :func:`release_outbox_drain`. Loud,
    never a skip, on a driver that reaches neither hold."""
    hold = _hold_file(app)
    if hold is not None:
        hold.parent.mkdir(parents=True, exist_ok=True)
        hold.touch()
        return
    if _set_in_process(app, True):
        return
    raise RuntimeError(
        f"{type(app.driver).__name__} reaches no outbox drain hold: it sets no "
        "FAUNA_E2E_OUTBOX_DRAIN_HOLD_FILE (the tui, linux, macos and ios "
        "drivers do, at launch) and has no set_outbox_drain_hold (the web and android drivers "
        "do); without one the hold would be a no-op and the journey would pass "
        "or fail for the wrong reason"
    )


def release_outbox_drain(app) -> None:
    """Release :func:`hold_outbox_drain`. The next pump pass drains."""
    hold = _hold_file(app)
    if hold is not None:
        hold.unlink(missing_ok=True)
        return
    _set_in_process(app, False)


#: ``fauna_e2e_agent::APP_OUTBOX_DRAIN_NOW`` owns the contract.
APP_OUTBOX_DRAIN_NOW = "app_outbox_drain_now"

#: ``fauna_e2e_agent::OUTBOX_DRAIN_HOLD`` owns the contract.
OUTBOX_DRAIN_HOLD = "outbox_drain_hold"


def ring_app_outbox_drain(app, *, timeout: float = 20.0) -> None:
    """Run the app's app-only drain loop now (queued MLS sends, identity-bound
    intents). Fire-and-forget: the caller polls the effect. The hold's held
    pass ended the loop, so after :func:`release_outbox_drain` nothing else
    would run it before the next reconnect."""
    app.driver.call_command(APP_OUTBOX_DRAIN_NOW, timeout=timeout)

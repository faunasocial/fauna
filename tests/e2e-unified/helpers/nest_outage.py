"""Take a seat's nest down and wait until the seat has noticed.

The offline journeys (a like, a preference edit, a conversation send, the
succession outbox carry) each act "with no nest". That act is offline only once
the seat's connection indicator has left ``Connected``. Stopping the nest and
acting at once would race the seat's own disconnect detection, so the wait is a
deadline poll on the painted indicator (convention 14), never a settle.
"""
from __future__ import annotations

from common.nest import start_nest_in_place, stop_nest
from helpers.waiting import wait_until
from i18n.strings import S

#: The connection indicator leaving ``Connected`` once the nest is held down.
DISCONNECT_S = 90.0


def hold_nest_down(app, nest_instance, *, act: str) -> None:
    """Stop ``nest_instance`` gracefully, then wait until ``app``'s
    ``connection-status`` reads anything but ``Connected``. ``act`` names what
    the caller does next offline ("the like below"), for the failure message.
    The caller brings the nest back with ``common.nest.start_nest_in_place``
    once this returns; if the wait fails, this brings it back itself, or the
    shared nest would stay down for every later test in the module, each failing
    for a reason that is not its own."""
    stop_nest(nest_instance, graceful=True)
    status: dict = {}

    def disconnected() -> bool:
        if not app.is_visible("connection-status"):
            return False
        status["text"] = app.driver.get_text("connection-status")
        return status["text"] != S.common.connected

    try:
        wait_until(
            disconnected, DISCONNECT_S,
            diagnose=lambda: (
                f"connection-status still read {status.get('text')!r} with the "
                f"nest down — the seat never noticed the outage, so {act} would "
                "not be offline at all"
            ),
        )
    except BaseException:
        start_nest_in_place(nest_instance)
        raise

"""Speak as a nest helper on the sidecar log plane, over the product's own kind.

A helper reports its events with `fauna.bridges.report_log_events`
(`docs/goal/architecture/apps/observability.md` § The sidecar log plane), and
the nest derives the source from the caller's authenticated class — never from
the payload — so the only way to plant a helper's line is to BE an enrolled
helper. This enrolls a fresh `mta`-role key exactly as a bridge's cold boot does
(`helpers/bridge_enrollment.py`), approves it as the admin, and reports through
it. No bridge process runs: the test plants the line a helper would, then reads
it back through the app's admin Logs page.

A planted event's ring target is `mta:<event>` (§ Admission, attribution).
"""

from __future__ import annotations

import time
import uuid
from dataclasses import dataclass

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.bridge_enrollment import PlantedBridges

#: The kind every enrolled bridge reports its events on (`fauna-protocol`
#: `log_plane::KIND_BRIDGES_REPORT_LOG_EVENTS`).
REPORT_LOG_EVENTS = "fauna.bridges.report_log_events"

#: The source id the nest stamps on an `mta`-class caller's events.
HELPER_SOURCE = "mta"


@dataclass(frozen=True)
class HelperEvent:
    """One event as a helper reports it. `event` is `[a-z0-9_.-]`, `level` one
    of `error` / `warn` / `info` (§ The allowlist contract)."""

    event: str
    level: str
    message: str
    timestamp_ms: int

    @property
    def target(self) -> str:
        """The ring target the nest attributes this event to."""
        return f"{HELPER_SOURCE}:{self.event}"


def helper_event(level: str, *, message: str = "", at_ms: int | None = None) -> HelperEvent:
    """A uniquely named event at `level`, stamped `at_ms` (default: now). The
    unique `event` name is what lets a test find its own row among the nest's."""
    tag = uuid.uuid4().hex[:10]
    return HelperEvent(
        event=f"e2e.probe_{tag}",
        level=level,
        message=message or f"probe {tag}",
        timestamp_ms=int(time.time() * 1000) if at_ms is None else at_ms,
    )


class LogPlaneHelper:
    """An approved `mta`-role helper, planted through `planted` (the
    `planted_bridges` fixture), ready to report events. The row is revoked at
    the fixture's teardown: an approved `mta` that never connects would read as
    a down mail helper to every later test's mail health.

    Enrolling writes the nest's own `bridge self-enrolled` line (target
    `bridge_rpc`, carrying `bridge_id=<id>`), which a test can use as a
    nest-own landmark at a known point in time — see :attr:`bridge_id`.
    """

    def __init__(self, planted: PlantedBridges):
        from nacl.signing import SigningKey

        self._nest_url = planted.nest_url
        self._key = SigningKey.generate()
        self.bridge_id = f"e2e-logplane-{uuid.uuid4().hex[:8]}"
        planted.plant(bytes(self._key.verify_key), HELPER_SOURCE, self.bridge_id)

    def plant_events(self, *events: HelperEvent, dropped: int = 0) -> None:
        """Report `events` in one batch. (Not named `report`: `kind_reach` merges
        by bare name, and every `.report(` in the tree would reach this kind.) The kind always answers success (the
        plane is best-effort), so a test reads the outcome off the Logs page."""
        ws = WsRpcAdminClient(
            self._nest_url,
            actor_id=bytes(self._key.verify_key),
            signing_key=bytes(self._key),
        )
        with ws:
            ws.call(
                REPORT_LOG_EVENTS,
                {
                    "events": [
                        {
                            "timestamp_ms": e.timestamp_ms,
                            "level": e.level,
                            "event": e.event,
                            "message": e.message,
                        }
                        for e in events
                    ],
                    "dropped": dropped,
                },
            )

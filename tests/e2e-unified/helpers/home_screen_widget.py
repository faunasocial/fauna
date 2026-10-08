"""Read a client's home-screen widget snapshot from OUTSIDE the app.

A widget lives outside the app's automation surface (`apps/common.md`
§ Home-screen widget → *Witnessing it*), so its witness never asks the app what
the widget shows: it reads the file the app publishes for the widget to render,
from `driver.widget_dir()` — on apple the host directory the driver told the app
to write into (`FAUNA_E2E_WIDGET_DIR`), on android a host mirror of the app's
private `files/widget/` that the driver refreshes through adb on each call, which
is why every read below calls it afresh. The file format is
`UnreadSnapshotStore`'s on both
(`apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/Widget/UnreadSnapshot.swift`,
`apps/fauna-android/app/src/main/java/com/fauna/app/widget/WidgetUnreadPublisher.kt`):
`{"count": <int>, "updatedAt": "<iso8601>"}` in `unread.json`.
"""

from __future__ import annotations

import json
import os
import uuid

from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

SNAPSHOT_FILE = "unread.json"

# `fauna_e2e_agent::WIDGET_REFRESH_KEY` / `WIDGET_REFRESH_SCHEDULED_PASS_NOW`.
WIDGET_REFRESH_KEY = "widget_refresh"
WIDGET_REFRESH_SCHEDULED_PASS_NOW = "widget_refresh_scheduled_pass_now"


def widget_snapshot(driver) -> dict | None:
    """The snapshot the app last wrote for its widget, or None if none yet.

    Fails loudly when the driver has no widget seam: a test marked for an app
    whose driver never wired one would otherwise poll a None forever and read
    as "the app never refreshed".
    """
    widget_dir = driver.widget_dir()
    assert widget_dir, (
        f"{type(driver).__name__}.widget_dir() is None — this driver never told "
        f"its app where to write the widget snapshot (FAUNA_E2E_WIDGET_DIR)"
    )
    path = os.path.join(widget_dir, SNAPSHOT_FILE)
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        return None
    except json.JSONDecodeError:
        # The app writes atomically, so a torn read means a foreign writer;
        # report it as absent and let the caller's wait diagnose.
        return None


def app_unread_total(conv) -> int:
    """The number the app itself shows: its thread list's unread, summed — the
    oracle every widget assertion compares against, never a constant (the
    session-scoped account may already hold unread threads from earlier tests)."""
    return sum(t.unread_count for t in conv.list_threads())


def plant_unread(conv, label: str) -> None:
    """An inbound the user has not read — one more unread message in the list."""
    conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"widget-{label}-{uuid.uuid4().hex[:8]}@self-nest.test",
        subject=None,
        body="a message waiting on the home screen",
    )


def await_widget_converged(driver, conv, *, above: int) -> int:
    """Wait until the snapshot equals the app's own total and that total is above
    `above`; return the total."""
    def check():
        total = app_unread_total(conv)
        snap = widget_snapshot(driver)
        if total > above and snap is not None and snap.get("count") == total:
            return total
        return None

    return wait_until(
        check,
        RPC_ROUNDTRIP_S,
        interval=0.3,
        diagnose=lambda: (
            f"widget snapshot {widget_snapshot(driver)!r} vs the app's own unread "
            f"total {app_unread_total(conv)} (wanted > {above})"
        ),
    )


def widget_refresh_passes(driver) -> dict:
    """The iOS / android widget-refresh pass counters (`fauna_e2e_agent::WIDGET_REFRESH_KEY`),
    asserted PRESENT: an app publishing none is refused, never read as "no pass"
    (convention 11) — every barrier on these counters would otherwise pass or
    fail vacuously."""
    value = driver.get_state(WIDGET_REFRESH_KEY)
    assert isinstance(value, dict), (
        f"the app publishes no `{WIDGET_REFRESH_KEY}` counters (got {value!r}), so "
        "the widget's scheduled refresh cannot be told from no pass at all"
    )
    for field in ("passes_started", "passes_completed"):
        assert isinstance(value.get(field), int), (
            f"`{WIDGET_REFRESH_KEY}` carries no integer `{field}`: {value!r}"
        )
    return value

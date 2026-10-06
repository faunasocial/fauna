"""Read the linux launcher badge from outside the app.

The linux home-screen widget is the launcher badge: the app broadcasts
`com.canonical.Unity.LauncherEntry.Update(s app_uri, a{sv} props)` on its
session bus and the desktop's dock paints `props["count"]` on the launcher
icon (`docs/goal/architecture/apps/linux.md` § Home-screen widget). A dock is
the last inch; the mechanism is the broadcast, and on a private session bus
(`drivers/session_bus.py`, whose policy allows eavesdropping) this listener IS
the dock: it subscribes to the interface and hands back every update as
`(app_uri, count, count_visible)`.

Pure `jeepney`, like `helpers/fake_status_notifier_watcher.py`; the listener
owns its own bus connection, so it sees broadcasts whether it attached before
or after the app launched. `jeepney` is imported lazily inside each method (as
`tests/common/keyring.py` does) so importing this module — which the linux
witness does at module scope — never requires it on a box that only collects
the suite, e.g. the mac dev VM's venv or a fresh public clone.
"""

from __future__ import annotations

import os
import time

LAUNCHER_ENTRY_IFACE = "com.canonical.Unity.LauncherEntry"


class LauncherBadgeUpdate:
    """One `Update` broadcast, unpacked."""

    def __init__(self, app_uri: str, props: dict):
        self.app_uri = app_uri
        # a{sv} arrives as {key: (signature, value)}.
        self.count = int(props.get("count", ("x", 0))[1])
        self.count_visible = bool(props.get("count-visible", ("b", False))[1])

    def __repr__(self) -> str:  # pragma: no cover — failure messages only
        return (
            f"LauncherBadgeUpdate(app_uri={self.app_uri!r}, count={self.count}, "
            f"count_visible={self.count_visible})"
        )


class LauncherEntryListener:
    """Subscribe to LauncherEntry updates on the bus at ``address``."""

    def __init__(self, address: str):
        self.address = address
        self._conn = None
        self.seen: list[LauncherBadgeUpdate] = []

    def start(self) -> "LauncherEntryListener":
        from jeepney import MatchRule, MessageType
        from jeepney.bus_messages import message_bus
        from jeepney.io.blocking import open_dbus_connection

        env_before = os.environ.get("DBUS_SESSION_BUS_ADDRESS")
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = self.address
        try:
            self._conn = open_dbus_connection(bus="SESSION")
        finally:
            if env_before is None:
                os.environ.pop("DBUS_SESSION_BUS_ADDRESS", None)
            else:
                os.environ["DBUS_SESSION_BUS_ADDRESS"] = env_before
        rule = MatchRule(type="signal", interface=LAUNCHER_ENTRY_IFACE, member="Update")
        reply = self._conn.send_and_get_reply(message_bus.AddMatch(rule))
        if reply.header.message_type == MessageType.error:
            raise RuntimeError(f"AddMatch refused: {reply.body}")
        return self

    def stop(self) -> None:
        if self._conn is not None:
            try:
                self._conn.close()
            except Exception:
                pass
            self._conn = None

    def _drain(self, timeout: float) -> LauncherBadgeUpdate | None:
        """Block up to ``timeout`` for the next update; None on silence."""
        from jeepney import HeaderFields, MessageType

        assert self._conn is not None, "start() the listener first"
        try:
            msg = self._conn.receive(timeout=timeout)
        except TimeoutError:
            return None
        fields = msg.header.fields
        if (
            msg.header.message_type != MessageType.signal
            or fields.get(HeaderFields.interface) != LAUNCHER_ENTRY_IFACE
            or fields.get(HeaderFields.member) != "Update"
        ):
            return None
        app_uri, props = msg.body
        update = LauncherBadgeUpdate(app_uri, props)
        self.seen.append(update)
        return update

    def await_count(self, expected: int, timeout: float) -> LauncherBadgeUpdate:
        """The first update whose count is ``expected``, within ``timeout``.

        Every update read on the way is kept in ``seen`` so a failure can show
        what the badge DID say. Raises ``AssertionError`` on silence or on a
        stream that never reaches ``expected``.
        """
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            update = self._drain(remaining)
            if update is not None and update.count == expected:
                return update
        raise AssertionError(
            f"no LauncherEntry.Update with count={expected} within {timeout:.0f}s; "
            f"updates seen on the bus: {self.seen}"
        )

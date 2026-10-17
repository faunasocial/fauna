#!/usr/bin/env python3
"""A minimal fake `org.freedesktop.Notifications` daemon for e2e tests.

The linux sync agent posts its push banners through `notify-rust`, which calls
`org.freedesktop.Notifications.Notify` on the **session bus**
(`bins/fauna-sync-agent/src/push_arm.rs`, `DesktopSink::post`). A private
session bus (`drivers/session_bus.py`) owns no notification daemon, so there
`Notify` fails and the agent's push arm counts the frame `failed`. This process
owns the name, answers the calls a client makes, and records every `Notify` —
the read-back a linux test has where windows reads the toast history.

Each `Notify` is printed to stdout as one `NOTIFY <json>` line (`app_name`,
`summary`, `body`, `actions`), after the reply is sent; the spawning harness
(`helpers/notification_bus.py`) collects them. It never emits `ActionInvoked`,
so a banner posted here is never "tapped" and launches nothing.

Run with `DBUS_SESSION_BUS_ADDRESS` pointing at the private bus. Pure `jeepney`
(no C extensions), so it runs in the e2e venv — the
`fake_status_notifier_watcher.py` shape.
"""

import json
import sys

from jeepney import HeaderFields, MessageType, new_error, new_method_return
from jeepney.bus_messages import message_bus
from jeepney.io.blocking import open_dbus_connection

NOTIFICATIONS_NAME = "org.freedesktop.Notifications"
NOTIFICATIONS_IFACE = "org.freedesktop.Notifications"

# DBUS_REQUEST_NAME_REPLY_PRIMARY_OWNER / _ALREADY_OWNER are both "we hold it".
_OWNED = (1, 4)

_CAPABILITIES = ["body", "actions"]
_SERVER_INFO = ("fauna-e2e-fake-notifications", "fauna", "1", "1.2")

_INTROSPECT_XML = (
    '<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"'
    ' "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">'
    '<node><interface name="org.freedesktop.Notifications">'
    '<method name="Notify">'
    '<arg type="s" direction="in"/><arg type="u" direction="in"/>'
    '<arg type="s" direction="in"/><arg type="s" direction="in"/>'
    '<arg type="s" direction="in"/><arg type="as" direction="in"/>'
    '<arg type="a{sv}" direction="in"/><arg type="i" direction="in"/>'
    '<arg type="u" direction="out"/></method>'
    '<method name="CloseNotification"><arg type="u" direction="in"/></method>'
    '<method name="GetCapabilities"><arg type="as" direction="out"/></method>'
    '<method name="GetServerInformation">'
    '<arg type="s" direction="out"/><arg type="s" direction="out"/>'
    '<arg type="s" direction="out"/><arg type="s" direction="out"/></method>'
    "</interface></node>"
)


class _Daemon:
    def __init__(self):
        self._next_id = 1

    def handle(self, msg):
        """Return ``(reply, record)`` for an incoming method call: the Message to
        send back (or None to ignore) and the `Notify` record to print (or None)."""
        if msg.header.message_type != MessageType.method_call:
            return None, None
        fields = msg.header.fields
        iface = fields.get(HeaderFields.interface)
        member = fields.get(HeaderFields.member)

        if iface in (NOTIFICATIONS_IFACE, None) and member == "Notify":
            app_name, replaces_id, _icon, summary, body, actions, _hints, _expire = msg.body
            nid = replaces_id or self._next_id
            if not replaces_id:
                self._next_id += 1
            record = {
                "id": nid,
                "app_name": app_name,
                "summary": summary,
                "body": body,
                "actions": list(actions),
            }
            return new_method_return(msg, "u", (nid,)), record
        if iface in (NOTIFICATIONS_IFACE, None) and member == "GetCapabilities":
            return new_method_return(msg, "as", (_CAPABILITIES,)), None
        if iface in (NOTIFICATIONS_IFACE, None) and member == "GetServerInformation":
            return new_method_return(msg, "ssss", _SERVER_INFO), None
        if iface in (NOTIFICATIONS_IFACE, None) and member == "CloseNotification":
            return new_method_return(msg), None

        if iface == "org.freedesktop.DBus.Introspectable" and member == "Introspect":
            return new_method_return(msg, "s", (_INTROSPECT_XML,)), None
        if iface == "org.freedesktop.DBus.Peer" and member == "Ping":
            return new_method_return(msg), None
        if iface == "org.freedesktop.DBus.Properties" and member == "GetAll":
            return new_method_return(msg, "a{sv}", ({},)), None

        # Anything else: a well-formed error keeps callers from hanging on a timeout.
        return new_error(msg, "org.freedesktop.DBus.Error.UnknownMethod"), None


def main() -> int:
    conn = open_dbus_connection(bus="SESSION")
    reply = conn.send_and_get_reply(message_bus.RequestName(NOTIFICATIONS_NAME, flags=0))
    if reply.body[0] not in _OWNED:
        print(
            f"fake notifications: could not own {NOTIFICATIONS_NAME} "
            f"(RequestName -> {reply.body[0]})",
            file=sys.stderr,
        )
        return 1
    # Signal readiness so the spawning harness can wait for ownership deterministically.
    print(f"OWNED {NOTIFICATIONS_NAME}", flush=True)

    daemon = _Daemon()
    while True:
        msg = conn.receive()
        out, record = daemon.handle(msg)
        if out is not None:
            conn.send(out)
        if record is not None:
            # After the reply: a record read back means the client got its id.
            print("NOTIFY " + json.dumps(record), flush=True)


if __name__ == "__main__":
    sys.exit(main())

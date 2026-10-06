#!/usr/bin/env python3
"""A minimal fake `org.kde.StatusNotifierWatcher` for e2e tray tests.

The Linux app's tray (vendored `libs/ksni`) decides whether close-to-tray has
somewhere to restore the window from by calling
`org.kde.StatusNotifierWatcher.RegisterStatusNotifierItem` at startup and by
watching `NameOwnerChanged` for that well-known name (see
`libs/ksni/src/service.rs`). A successful register call (or the name simply being
*owned*) fires `Tray::watcher_online`; the name vanishing fires
`Tray::watcher_offine`. So to put the client into the *host-present* state on an
isolated private session bus, all we need is a process that **owns that name** and
replies OK to `RegisterStatusNotifierItem` — we do not need a real tray at all.

Run with `DBUS_SESSION_BUS_ADDRESS` pointing at the private bus. Owns the name
until the process is terminated; terminating it (SIGTERM) releases the name, which
drives the client's `watcher_offine` (the dynamic host-disappears transition).

Pure `jeepney` (no C extensions), so it runs in the e2e venv and in CI without
system D-Bus Python bindings.
"""

import sys

from jeepney import HeaderFields, MessageType, new_error, new_method_return
from jeepney.bus_messages import message_bus
from jeepney.io.blocking import open_dbus_connection

WATCHER_NAME = "org.kde.StatusNotifierWatcher"
WATCHER_PATH = "/StatusNotifierWatcher"
WATCHER_IFACE = "org.kde.StatusNotifierWatcher"

# DBUS_REQUEST_NAME_REPLY_PRIMARY_OWNER / _ALREADY_OWNER are both "we hold it".
_OWNED = (1, 4)

_INTROSPECT_XML = (
    '<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"'
    ' "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">'
    "<node><interface name=\"org.kde.StatusNotifierWatcher\">"
    '<method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>'
    '<method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>'
    '<property name="IsStatusNotifierHostRegistered" type="b" access="read"/>'
    '<property name="RegisteredStatusNotifierItems" type="as" access="read"/>'
    '<property name="ProtocolVersion" type="i" access="read"/>'
    "</interface></node>"
)


def _reply_for(msg):
    """Return the Message to send back for an incoming method call, or None to ignore."""
    if msg.header.message_type != MessageType.method_call:
        return None
    fields = msg.header.fields
    iface = fields.get(HeaderFields.interface)
    member = fields.get(HeaderFields.member)

    if iface == WATCHER_IFACE and member in (
        "RegisterStatusNotifierItem",
        "RegisterStatusNotifierHost",
    ):
        # The load-bearing call: ksni registers its item here at startup and
        # treats an Ok reply as "watcher online".
        return new_method_return(msg)

    if iface == "org.freedesktop.DBus.Introspectable" and member == "Introspect":
        return new_method_return(msg, "s", (_INTROSPECT_XML,))

    if iface == "org.freedesktop.DBus.Properties":
        if member == "Get":
            _, prop = msg.body
            value = {
                "IsStatusNotifierHostRegistered": ("b", True),
                "RegisteredStatusNotifierItems": ("as", []),
                "ProtocolVersion": ("i", 0),
            }.get(prop)
            if value is None:
                return new_error(msg, "org.freedesktop.DBus.Error.UnknownProperty")
            return new_method_return(msg, "v", (value,))
        if member == "GetAll":
            return new_method_return(
                msg,
                "a{sv}",
                (
                    {
                        "IsStatusNotifierHostRegistered": ("b", True),
                        "RegisteredStatusNotifierItems": ("as", []),
                        "ProtocolVersion": ("i", 0),
                    },
                ),
            )

    # Anything else: a well-formed error keeps callers from hanging on a timeout.
    return new_error(msg, "org.freedesktop.DBus.Error.UnknownMethod")


def main() -> int:
    conn = open_dbus_connection(bus="SESSION")
    reply = conn.send_and_get_reply(message_bus.RequestName(WATCHER_NAME, flags=0))
    if reply.body[0] not in _OWNED:
        print(
            f"fake watcher: could not own {WATCHER_NAME} (RequestName -> {reply.body[0]})",
            file=sys.stderr,
        )
        return 1
    # Signal readiness so the spawning fixture can wait for ownership deterministically.
    print(f"OWNED {WATCHER_NAME}", flush=True)

    while True:
        msg = conn.receive()
        out = _reply_for(msg)
        if out is not None:
            conn.send(out)


if __name__ == "__main__":
    sys.exit(main())

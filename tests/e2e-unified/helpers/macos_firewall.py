"""What the macOS application firewall says about a binary — for failure messages.

The peer leg is an INBOUND connection: a sibling dials this device's listener.
With the application firewall on, macOS holds an inbound flow to a binary that
is not on its allow list while it waits for a GUI "Allow incoming connections?"
answer that no e2e run ever gives, so the dialer sees nothing but a connect
timeout (`peer dial: … no answer within 10s`). The firewall lists binaries by
PATH, and every checkout builds its own `target/debug/fauna-sync-agent`, so one
checkout's seats can admit siblings while another's, on the same box and the
same commit, never do.

Measured 2026-10-29 on a macOS dev VM: the native pair's agents (the agent hosts
the account runtime there) published their endpoints and dialed each other
every pass, and `socketfilterfw` logged `Enqueuing new inbound flow for
fauna-sync-agent` and never delivered it; the tui pair on the same box passed
admission because its peer leg rides `fauna-tui`, which was on the list.

Read-only: `--getglobalstate` and `--listapps` need no privilege. Adding a
binary to the list does (`socketfilterfw --add` runs as root), so this module
only says what it sees and never tries to change it. `--getappblocked` is NOT
used: it answers "permitted" for an unlisted binary whose flows are being held.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

SOCKETFILTERFW = "/usr/libexec/ApplicationFirewall/socketfilterfw"


def _run(*args: str) -> str | None:
    try:
        return subprocess.run(
            [SOCKETFILTERFW, *args], capture_output=True, text=True, timeout=10
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None


def listed_apps(listing: str) -> dict[str, str]:
    """``{path: verdict}`` from ``socketfilterfw --listapps`` output, the verdict
    being the parenthesised line under each numbered path (``Allow incoming
    connections`` / ``Block incoming connections``)."""
    apps: dict[str, str] = {}
    path = None
    for raw in listing.splitlines():
        line = raw.strip()
        head, sep, rest = line.partition(" : ")
        if sep and head.isdigit():
            path = rest.strip()
            apps[path] = ""
        elif path is not None and line.startswith("(") and line.endswith(")"):
            apps[path] = line[1:-1]
            path = None
    return apps


def firewall_note(paths) -> str | None:
    """One line per binary in ``paths`` that the firewall will not let a sibling
    reach, or None when it is off, unreadable, or every binary is allowed. Off
    darwin always None."""
    if sys.platform != "darwin":
        return None
    state = _run("--getglobalstate")
    if not state or "enabled" not in state.lower():
        return None
    listing = _run("--listapps")
    if listing is None:
        return None
    apps = listed_apps(listing)
    flows: str | None = None
    lines = []
    for path in paths:
        # The list holds the path the binary RAN from, symlinks resolved: the
        # macOS seat launches `.build/debug/FaunaMacOS`, a symlink whose target
        # moved with Xcode 27 (`arm64-apple-macosx/debug` → `out/Products/Debug`),
        # so one checkout's old entry no longer covers the binary it now runs.
        verdict = apps.get(str(path))
        if verdict is None:
            verdict = apps.get(os.path.realpath(str(path)))
        if verdict == "Allow incoming connections":
            continue
        if verdict is None:
            # Unlisted by path is not proof of a held flow: an entry made by
            # answering the prompt matches the binary's signing identifier
            # first, the same in every checkout.
            # The log's own verdict decides.
            if flows is None:
                flows = _flow_log("30m") or ""
            if last_flow_verdict(flows, os.path.basename(str(path))) == "allowed":
                continue
        why = (
            "is NOT on its list, so a sibling's dial to it waits on an Allow "
            "prompt nobody answers and never connects"
            if verdict is None
            else f"is listed as {verdict!r}"
        )
        lines.append(
            f"  macOS application firewall is ON and {os.path.realpath(str(path))} {why}"
        )
    if not lines:
        return None
    return "\n".join(
        lines
        + [
            "  (adding a binary needs root: `sudo /usr/libexec/ApplicationFirewall/"
            "socketfilterfw --add <path> --unblockapp <path>` — a user step)"
        ]
    )


#: `socketfilterfw`'s verdict numbers, as measured on macOS: 1 let a known app's
#: flow through, 2 refused it. Any other number is not guessed at.
_KNOWN_VERDICTS = {"1": "allowed", "2": "blocked"}
_FLOW_LINE = re.compile(
    r"KNOWN APP FLOW: \S*/(?P<known>[^/,\s]+), return known verdict: (?P<verdict>\d+)"
    r"|Enqueuing new inbound flow for (?P<held>\S+) with identifier"
)


def last_flow_verdict(log_text: str, binary_prefix: str) -> str | None:
    """What the firewall did with the newest inbound flow to a binary whose name
    starts with ``binary_prefix``: ``allowed``, ``blocked``, ``held`` (waiting on
    an Allow prompt nobody answers), or None when the log shows no such flow or a
    verdict number this module does not know.

    This reads the decision itself, because ``--listapps`` cannot: a dialog-made
    entry carries the binary's signing identifier and decides the flow of every
    binary signed with that identifier, whatever entry their own path has."""
    verdict = None
    for line in log_text.splitlines():
        match = _FLOW_LINE.search(line)
        if match is None:
            continue
        name = match["known"] or match["held"]
        if name.startswith(binary_prefix):
            verdict = "held" if match["held"] else _KNOWN_VERDICTS.get(match["verdict"])
    return verdict


def _flow_log(window: str) -> str | None:
    """`socketfilterfw`'s unified-log lines over the last ``window`` (read-only,
    no privilege), or None when the log cannot be read."""
    try:
        return subprocess.run(
            [
                "/usr/bin/log", "show", "--last", window, "--style", "compact",
                "--predicate", 'process == "socketfilterfw"',
            ],
            capture_output=True, text=True, timeout=30,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None


def inbound_flow_note(binary_prefix: str, window: str = "2m") -> str | None:
    """One sentence on the firewall having refused or held a flow to the binary in
    the last ``window``, for a failure or skip message; None off darwin, when the
    firewall let it through, or when the log says nothing about it."""
    if sys.platform != "darwin":
        return None
    log_text = _flow_log(window)
    if log_text is None:
        return None
    verdict = last_flow_verdict(log_text, binary_prefix)
    if verdict == "blocked":
        return (
            f"the macOS application firewall BLOCKED the inbound flow to "
            f"{binary_prefix}: an entry set to block its signing identifier decides "
            f"every checkout's binary"
        )
    if verdict == "held":
        return (
            f"the macOS application firewall is HOLDING the inbound flow to "
            f"{binary_prefix} on an Allow prompt nobody answers"
        )
    return None


def seat_binaries(driver) -> list[str]:
    """The binaries a sibling's dial reaches on this seat: the app it launched
    and, when the app spawns one, its private agent (`FAUNA_E2E_SYNC_AGENT_BIN`).
    Read off the driver's own launch config — every driver keeps it as
    ``_launch_config`` — so nothing here guesses a layout path."""
    config = getattr(driver, "_launch_config", None) or {}
    env = config.get("environment") or {}
    return [p for p in (config.get("app_path"), env.get("FAUNA_E2E_SYNC_AGENT_BIN")) if p]


def seat_firewall_note(driver, seat: str) -> str:
    """:func:`firewall_note` over one seat's :func:`seat_binaries`, labelled for a
    failure message; empty when nothing on the seat is held."""
    note = firewall_note(seat_binaries(driver))
    return f"  [{seat}] application firewall:\n{note}\n" if note else ""

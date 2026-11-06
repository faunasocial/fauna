"""Where THIS launch's own sync agent answers, and how to stop it — one seam
over both transports.

A unix launch's agent serves a per-launch socket (``sync_agent_socket``: linux
and tui on linux or macOS for every launch, the macos app for a launch that
spawns a real agent); a windows launch's — the windows app, and tui running on
windows — serves a per-launch named pipe (``sync_agent_pipe``, pinned by the
``isolated_sync_agent`` posture). Both speak the same verbs, so a witness that
addresses "this launch's agent" asks here instead of choosing a transport
itself: every reader agrees on which agent is the launch's, and a windows run
reaches the same assertion a unix run does.

The transport-specific halves stay where they are (``sync_agent_ipc_unix``,
``sync_agent_ipc`` / ``windows_sync_agent``); this module only routes.

Two older readers still make the same socket-before-pipe choice inline —
``helpers/sync_seats.py::SyncSeat._agent_endpoint`` and
``helpers/fleet.py::poke_co_located_agent`` — and route through
:func:`agent_endpoint` the next time either is edited for its own reasons (each
is imported by enough multi-seat journeys that a routing-only edit would put
them all in a landing's touched set).
"""

from __future__ import annotations

#: ``("unix", <socket path>)`` or ``("pipe", <Win32 pipe path>)``.
Endpoint = tuple[str, str]


def agent_endpoint(driver) -> Endpoint | None:
    """Where ``driver``'s own agent answers, whichever transport the driver
    publishes — or ``None`` (not launched yet, or a launch that pinned no agent
    endpoint: the agent, if any, is the box's own, never ours to address)."""
    socket_path = getattr(driver, "sync_agent_socket", None)
    if socket_path:
        return "unix", socket_path
    pipe = getattr(driver, "sync_agent_pipe", None)
    if pipe:
        return "pipe", pipe
    return None


def shutdown_agent(endpoint: Endpoint, *, timeout: float = 60.0) -> int:
    """Stop the agent answering at ``endpoint`` through its own ``Shutdown``
    verb and wait until its PROCESS has exited — so every kernel lock it held
    (``agent.lock``, ``engine.lock``) is released; the pid that exited.

    Never a kill (the harness's process-safety rule). Nothing here keeps the
    agent stopped: an app whose provisioning loop finds it gone spawns a fresh
    one on its next convergence tick."""
    kind, address = endpoint
    if kind == "unix":
        from helpers.sync_agent_ipc_unix import shutdown_agent as stop
    elif kind == "pipe":
        from helpers.windows_sync_agent import shutdown_agent as stop
    else:
        raise ValueError(f"unknown sync-agent transport {kind!r} in {endpoint!r}")
    return stop(address, timeout=timeout)

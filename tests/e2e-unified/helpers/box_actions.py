"""The live harness's hook for the organisation's own box record.

The project keeps an action record for each of the organisation's own
deployed nests, written by the tool that acts on one. That record and its
writer are not part of this tree, and a contributor's run never drives one of
those nests, so in this tree every call here writes nothing: ``record`` returns
``None`` and ``target`` names no box. The shared live steps call it so that
they read the same in every tree.
"""
from __future__ import annotations

from urllib.parse import urlsplit


def host_of(target: str) -> str:
    """The host a URL or bare host names, lower-cased."""
    t = (target or "").strip()
    if "://" not in t:
        t = "https://" + t
    return (urlsplit(t).hostname or "").lower().rstrip(".")


def run_box() -> str | None:
    """The run's own box when the run is a live one, else None."""
    from helpers import nest_mode

    mode = nest_mode.run_mode()
    if not mode.is_live:
        return None
    try:
        return nest_mode.live_url(mode)
    except nest_mode.NestModeError:
        return None


def target(box: str | None) -> str | None:
    """The organisation box ``box`` names: none in this tree."""
    return None


def record(box: str | None, what: str, *, detail: dict | None = None) -> dict | None:
    """Record an action on an organisation box: nothing to record in this tree."""
    return None

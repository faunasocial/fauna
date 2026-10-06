"""Scope path parser for scoped element queries.

Scope strings use a simplified XPath-inspired syntax to narrow element
lookups to a subtree of the accessibility/DOM tree:

    post-card[2]                  # 3rd post-card below the app root
    conversation-view/msg[1]      # 2nd msg inside the 1st conversation-view
    event-card                    # 1st event-card (index defaults to 0)

Every step resolves as a **descendant** match (e2e-conventions.md
§ convention 1, ruled 2026-08-14): a step names a container found anywhere
below the previous step's subtree, and ``[n]`` indexes those instances in
document order below it.  Containers between the steps may be left unnamed,
so a scope names only what it cares about — ``post-card[1]/quoted-post`` and
a bare ``quoted-post`` are both legal.

Parsed once in the Python driver layer and sent as structured JSON to
bridge servers.  Bridges never parse the string syntax themselves.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

_SEGMENT_RE = re.compile(r"^([a-zA-Z0-9_-]+)(?:\[(\d+)\])?$")


@dataclass(frozen=True, slots=True)
class ScopeStep:
    element_id: str
    index: int = 0


def parse_scope(scope: str | None) -> list[ScopeStep]:
    """Parse a scope path string into a list of steps.

    Returns an empty list for ``None`` or empty string (meaning global
    search, backward compatible with the no-scope behavior).

    Raises ``ValueError`` on malformed input.
    """
    if not scope:
        return []
    steps: list[ScopeStep] = []
    for segment in scope.split("/"):
        m = _SEGMENT_RE.match(segment)
        if not m:
            raise ValueError(
                f"Invalid scope segment {segment!r} in {scope!r}. "
                "Expected format: element-id or element-id[index]"
            )
        steps.append(ScopeStep(m.group(1), int(m.group(2) or 0)))
    return steps


def scope_to_wire(steps: list[ScopeStep]) -> list[dict] | None:
    """Serialize parsed scope steps for the HTTP wire protocol.

    Returns ``None`` when the list is empty (no scope), so bridges see
    the absence of a ``scope`` field rather than an empty array.
    """
    if not steps:
        return None
    return [{"id": s.element_id, "index": s.index} for s in steps]


def scope_from_wire(data: list[dict] | None) -> list[ScopeStep]:
    """Deserialize scope steps received over the wire in a bridge server."""
    if not data:
        return []
    return [ScopeStep(d["id"], d.get("index", 0)) for d in data]

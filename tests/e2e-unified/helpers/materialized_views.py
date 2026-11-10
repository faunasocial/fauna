"""The pieces the cross-process witnesses of the materialization grant share.

Owner doc: `docs/goal/architecture/encryption-at-rest.md` § Capability
tiering → *The materialization grant* (parts (c) the per-actor, per-kind
area and (d) what search serves through it). The mail witness keeps its own
copies (it predates this module); the calendar and card witnesses
(`tests/api/test_search_materialized_calendar.py`, `..._card.py`) read
these.
"""

from __future__ import annotations

import sqlite3
import time
from pathlib import Path

import fauna_ffi

#: Ceiling for the materializer's trickle to index a stored record — a poll
#: ceiling, never a settle-sleep (convention 14).
INDEXED_S = 30.0


def grant_ids(db_path: str, owner: bytes) -> set[bytes]:
    """Every capability grant the owner holds on the nest, by id."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT grant_id FROM capability_grants WHERE owner_actor_id = ?",
            (owner,),
        ).fetchall()
    finally:
        conn.close()
    return {bytes(r[0]) for r in rows}


def mint_kind(url: str, db_path: str, owner_seed: bytes, owner_id: bytes,
              kind: str) -> bytes:
    """Mint ``materialize_<kind>`` through the Nests page's own machine and
    return the one grant id the mint deposited."""
    before = grant_ids(db_path, owner_id)
    fauna_ffi.harness_mint_trust(url, owner_seed, f"materialize_{kind}")
    new = grant_ids(db_path, owner_id) - before
    assert len(new) == 1, (
        f"the materialize_{kind} mint must deposit exactly one grant, found {len(new)}"
    )
    return new.pop()


def kind_dir(db_path: str, owner_hex: str, kind: str) -> Path:
    """``__materialized/<owner>/<kind>/`` beside the nest's database."""
    return Path(db_path).parent / "__materialized" / owner_hex / kind


def hits(client, phrase: str, content_type: str) -> list[dict]:
    """The caller's search hits of one content type for ``phrase``."""
    reply = client.call(
        "fauna.search.query",
        {"query": phrase, "limit": 20, "content_type": content_type},
    )
    return [r for r in reply.get("results", []) if r.get("content_type") == content_type]


def wait_hits(client, phrase: str, content_type: str) -> list[dict]:
    """Poll until the caller's search finds ``phrase`` or the ceiling passes."""
    deadline = time.monotonic() + INDEXED_S
    found = hits(client, phrase, content_type)
    while not found and time.monotonic() < deadline:
        time.sleep(0.25)
        found = hits(client, phrase, content_type)
    return found


def uid_hash_hex(href: str, suffix: str) -> str:
    """The hex ``uid_hash`` a DAV resource's server-canonical href carries:
    the MDA names every resource ``blake3(UID).<suffix>`` (caldav/carddav
    ``put.go`` step 3), the same hash the nest keys the record by."""
    name = href.rstrip("/").rsplit("/", 1)[1]
    assert name.endswith(suffix), f"a canonical DAV href ends in {suffix}: {href!r}"
    stem = name[: -len(suffix)]
    assert len(bytes.fromhex(stem)) == 32, f"the canonical slug is a 32-byte hash: {href!r}"
    return stem

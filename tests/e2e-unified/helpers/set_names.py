"""Find a folder on the nest by the name a test gave it.

Since schema 114 a sealed set's ``folders`` row rests **no plaintext name**:
``fauna.folders.list`` reports ``name == ""`` beside the set's ``name_hash`` and
``name_sealed`` (``docs/goal/behavior/path-sealing.md`` § the set-name plane).
A ground-truth read keyed by ``fs["name"]`` therefore finds no app-created set.
Key it by :func:`set_name_hash` instead — what every app and the nest address a
set by.
"""

from __future__ import annotations

import blake3

# MUST match `fauna_core::path_crypto::set_name_hash` — pinned there by
# `set_name_hash_is_the_pinned_derivation`; the module-level assert below pins
# the same known answer, so a drift fails at import, never as a silent miss.
_SET_NAME_CONTEXT = "fauna.set-name.v1"


def set_name_hash(name: str) -> bytes:
    """The 32-byte address of the set called ``name``."""
    return blake3.blake3(name.encode("utf-8"), derive_key_context=_SET_NAME_CONTEXT).digest()


assert (
    set_name_hash("Family photos").hex()
    == "5245f3343a81010fb7a914a290461030408fd8e90b45eb6799353860718a8331"
), "set_name_hash drifted from fauna_core::path_crypto::set_name_hash"


def row_name_hash(row: dict) -> bytes | None:
    """A list row's ``name_hash`` as bytes (the wire carries it as bytes, or as
    a list of ints when decoded loosely), else ``None``."""
    raw = row.get("name_hash")
    if raw is None:
        return None
    return bytes(raw)


def find_set(rows: list[dict], name: str) -> dict | None:
    """The row of ``rows`` that is the set called ``name`` — by hash, or by the
    plaintext a hash-less row (a reserved ``__`` set) still carries."""
    want = set_name_hash(name)
    for row in rows:
        if row_name_hash(row) == want or (row.get("name") and row.get("name") == name):
            return row
    return None


def addressed(name: str, **fields) -> dict:
    """A request payload addressed to the set called ``name`` by its hash — the
    harness twin of ``fauna_protocol::folders::addressed``. A sealed set rests
    no plaintext name, so a ``{"name": ...}`` request finds nothing
    (``fauna.folders.not_found``)."""
    return {"name_hash": set_name_hash(name), **fields}

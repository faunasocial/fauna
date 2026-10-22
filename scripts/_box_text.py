"""Bounded, escaped rendering of text a deployed box sent over the wire.

WHY THIS EXISTS: a deploy-verify gate runs in CI and its output lands in the
Actions log, which a person (and a session) reads. Anything the box answered —
a health field, a DKIM value, an error reason — is untrusted data
(docs/goal/architecture/deployment-threat-model.md § Concern 2, path 6: the
log must never carry a box-sourced line verbatim). A gate that must show what
the box said shows `summarise(...)` of it: the repr (every control character
escaped, the whole thing on one line) cut to a fixed length.

Dependencies: Python stdlib only. Loaded by path from the gates
(`importlib.util.spec_from_file_location`), like scripts/_cloud_project.py.
"""

from __future__ import annotations

#: The longest rendering `summarise` returns, ellipsis included.
MAX_LEN = 200


def summarise(value: object, limit: int = MAX_LEN) -> str:
    """`repr(value)`, at most `limit` characters; a cut one ends in `…`."""
    text = repr(value)
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "…"

"""The canonical-envelope dedup key of a raw RFC 5322 message, test-side.

A mirror of ``fauna_mail::mail_dedup_keys_from_slice``'s ``envelope_key``
(``libs/fauna-mail/src/dedup_key.rs``; ``mailbox-migration.md`` § Key format)
for the simple, unfolded header blocks tests build: sha256 over the trimmed
``From``, ``To``, ``Cc``, ``Date`` and ``Subject`` values, each followed by a
NUL, then the raw sha256 of the CRLF-normalized body. Every producer sends it
beside the ``dedup_key`` on ``fauna.bridges.import_message``, and a
``dedup_key`` hit skips only when the two envelope keys agree.

It is not a parser: a folded or encoded header block is out of scope, and a
test that needs one must take the key from the shared Rust function instead.
``_GOLDEN`` pins it to the Rust suite's golden vector.
"""

from __future__ import annotations

import hashlib

_FIELDS = ("from", "to", "cc", "date", "subject")

#: The spec's golden vector (`dedup_key.rs::envelope_form_matches_the_specs_golden_vector`).
_GOLDEN_MESSAGE = (
    b"From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\n"
    b"Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n"
)
_GOLDEN = "env:v1:ae6d5a8b9de5d0e09b958ea8b6ab5d023262dbf015d996cc247c004e4c5ea576"


def _split(raw: bytes) -> tuple[bytes, bytes]:
    crlf = raw.find(b"\r\n\r\n")
    lf = raw.find(b"\n\n")
    ends = [i + 4 for i in [crlf] if i >= 0] + [i + 2 for i in [lf] if i >= 0]
    if not ends:
        return raw, b""
    cut = min(ends)
    return raw[:cut], raw[cut:]


def _crlf_normalize(body: bytes) -> bytes:
    return body.replace(b"\r\n", b"\n").replace(b"\r", b"\n").replace(b"\n", b"\r\n")


def envelope_key(raw: bytes) -> str:
    """``env:v1:<hex>`` for ``raw`` (simple unfolded headers only)."""
    headers, body = _split(raw)
    values: dict[str, str] = {}
    for line in headers.replace(b"\r\n", b"\n").split(b"\n"):
        name, sep, value = line.partition(b":")
        key = name.strip().lower().decode("ascii", "replace")
        if sep and key in _FIELDS and key not in values:
            values[key] = value.strip().decode("utf-8", "replace")
    h = hashlib.sha256()
    for field in _FIELDS:
        h.update(values.get(field, "").encode())
        h.update(b"\0")
    h.update(hashlib.sha256(_crlf_normalize(body)).digest())
    return "env:v1:" + h.hexdigest()


assert envelope_key(_GOLDEN_MESSAGE) == _GOLDEN, "the mirror drifted from the Rust golden vector"

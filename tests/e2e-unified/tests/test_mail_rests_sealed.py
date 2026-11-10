"""tier_3: received mail rests only sealed, and the nest refuses an unsealed
record (mail-server outcome 42).

Goal doc: ``docs/goal/architecture/encryption-at-rest.md`` § Implementation
status today (S1, the uniform seal at ingest): the MTA seals every inbound
message to its recipient before it reaches the nest, and the ingest RPC proves
both payload halves are sealed recipient envelopes at the wire edge, refusing
an unsealed body as ``malformed`` — nothing unsealed reaches the ``__mail``
segment store (``bridge_routing_handlers.rs``, the S6.12b structural seal gate).

Two halves, on the real nest + the real Go MTA (``mail_bridge_mta``):

* **rest** — a message with a distinctive phrase in its subject and body is
  delivered over SMTP; the 250 on end-of-DATA follows the synchronous ingest,
  so the message has arrived, and the phrase is then in no file the nest holds,
  its database and write-ahead log included (the byte scan
  ``test_mail_import_nest_api.py::test_imported_mail_rests_sealed_on_the_nest``
  uses for mailbox-import 45). Only the nest's own directory is scanned, never
  the bridge's spool.
* **refusal** — the MTA's own principal sends ``ingest_inbound_mail`` with a
  plaintext ``encrypted_body``; the nest answers ``malformed`` naming the
  unsealed body, and the recipient's mailbox gains no row and the plaintext
  phrase reaches no nest file.

Latency-independent (convention 14): every step is a request with a reply.
"""

from __future__ import annotations

import secrets
import sqlite3
import time
from pathlib import Path

import cbor2
import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from helpers.mail_wire import _connect_smtp_starttls

pytestmark = pytest.mark.tier_3


def _files_holding(root: str, phrase: str) -> list[str]:
    """Every file under ``root`` whose bytes contain ``phrase``."""
    held = []
    for path in Path(root).rglob("*"):
        if not path.is_file():
            continue
        try:
            if phrase.encode() in path.read_bytes():
                held.append(str(path))
        except FileNotFoundError:
            continue  # a `-shm`/`-wal` side file SQLite removed mid-scan
    return held


def _mail_rows(db_path: str, actor_id: bytes) -> int:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?", (actor_id,)
        ).fetchone()[0]
    finally:
        conn.close()


@pytest.mark.feature("mail-server")
def test_received_mail_rests_only_sealed_on_the_nest(mail_bridge_mta, nest_instance):
    handle = mail_bridge_mta
    phrase = f"marmalade-pangolin-{secrets.token_hex(6)}"
    rcpt = f"{handle.recipient_local_part}@{handle.domain}"
    actor_id = bytes(handle.recipient_actor["actor_id_bytes"])
    before = _mail_rows(nest_instance["db_path"], actor_id)

    lines = [
        "From: External Sender <sender@external.test>",
        f"To: {rcpt}",
        f"Subject: plans for {phrase}",
        f"Message-ID: <rest-{secrets.token_hex(6)}@external.test>",
        "Date: Sat, 07 Nov 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The words that must never rest readable: {phrase}.",
    ]
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(("\r\n".join(lines) + "\r\n").encode())
        # 250 on `.` only once the nest's ingest committed the record.
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    assert _mail_rows(nest_instance["db_path"], actor_id) == before + 1, (
        "the message must really have arrived, or its absence from disk proves nothing"
    )
    held = _files_holding(nest_instance["tmp_dir"], phrase)
    assert not held, f"received mail's plaintext is readable on the nest's disk: {held}"


@pytest.mark.feature("mail-server")
def test_the_nest_refuses_an_unsealed_mail_record(mail_bridge_mta, nest_instance):
    handle = mail_bridge_mta
    phrase = f"unsealed-ocelot-{secrets.token_hex(6)}"
    actor_id = bytes(handle.recipient_actor["actor_id_bytes"])
    before = _mail_rows(nest_instance["db_path"], actor_id)

    # The MTA's own enrolled principal — the only class the ingest door admits —
    # so the refusal is the seal gate's, not a class check's.
    with open(handle.keypair_file, "rb") as f:
        seed = bytes(cbor2.loads(f.read())["ed25519_seed"])
    raw = f"From: sender@external.test\r\nSubject: {phrase}\r\n\r\n{phrase}\r\n".encode()
    none = {"kind": "none"}
    request = {
        "actor_id": actor_id,
        "encrypted_body": raw,
        "encrypted_index_hint": raw,
        "public_metadata": {
            "timestamp": int(time.time()),
            "ciphertext_size": len(raw),
            "sender_domain": "external.test",
        },
        "verdicts": {"dkim": none, "spf": none, "dmarc": none, "arc": none},
        "spam_score": 0,
        "spam_disposition": "accept",
        # Required by the wire; the seal gate fires before either is read.
        "dedup_key": f"probe-dedup-{phrase}",
        "envelope_key": f"probe-envelope-{phrase}",
    }
    with WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(SigningKey(seed).verify_key), signing_key=seed
    ) as mta:
        with pytest.raises(RpcCallError) as refused:
            mta.call("fauna.bridges.ingest_inbound_mail", request)
    assert refused.value.code == "fauna.protocol.malformed", (
        f"an unsealed body is refused as malformed: {refused.value.code!r} {refused.value.details!r}"
    )
    assert "not a sealed mail record" in str(refused.value), (
        f"the refusal names the unsealed body, not some other defect: {refused.value!r}"
    )

    assert _mail_rows(nest_instance["db_path"], actor_id) == before, (
        "the refused record stored no mailbox row"
    )
    held = _files_holding(nest_instance["tmp_dir"], phrase)
    assert not held, f"the refused plaintext reached the nest's disk: {held}"

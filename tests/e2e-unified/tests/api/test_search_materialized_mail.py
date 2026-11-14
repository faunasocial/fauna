"""tier_3 — with your mail kept searchable on your nest, your search finds a
phrase from your mail, nobody else's search does, and turning it off deletes
the nest's copy.

`[nest]` outcome 9 of `docs/features/search.md`. Owner doc:
`docs/goal/architecture/encryption-at-rest.md` § Capability tiering → *The
materialization grant* (parts (b) the materializer holder, (c) the per-actor
area, (d) what search serves through it) and § Readable classes, class 4
(own-actor scopes only; revoke = keys dropped + views deleted).

Every step is the production path, app-less:

- mail is enabled by the shared Mail settings machine (``harness_enable_mail``),
  which leaves the MSEK in the owner's sealed mail custody and registers the
  recipient seal key, then runs the app's connect-time refresh, publishing the
  content-sealing epoch schedule the MTA seals to (so the materializer must
  open an epoch-sealed record, as for every app user);
- the grant is minted by the Nests page's own ``LinkedNestsMachine`` from its
  derived "Keep my mail searchable on this nest" option, to the holder that
  option names — the nest's own materializer (``harness_mint_trust``);
- the mail arrives over real SMTP through the Go MTA, sealed to the owner;
- the owner's ``fauna.search.query`` finds it by a body phrase once the
  materializer's trickle has indexed it (a deadline poll — the worker indexes
  on a debounce after the append, convention 14);
- another member's query over the same phrase finds nothing (the connection
  actor is the ACL);
- ``fauna.capabilities.revoke`` deletes ``__materialized/<owner>/`` before it
  replies, and the owner's query no longer finds the mail.

The restart, the retroactive arm at boot and the lapsed window are pinned at
tier_3 in Rust (``bins/fauna-nest/tests/conformance_materialization_grant.rs``);
this test is the cross-process witness of the journey.
"""

import sqlite3
import time
from pathlib import Path

import pytest

import fauna_ffi
from helpers.mail_wire import _connect_smtp_starttls

pytestmark = pytest.mark.tier_3

SENDER_DOMAIN = "external.test"

#: Ceiling for the materializer's trickle to index the delivered mail — a
#: poll ceiling, never a settle-sleep (convention 14).
_INDEXED_S = 30.0


def _deliver(mx_port: int, server_name: str, recipient_addr: str, raw: bytes,
             deadline: float) -> None:
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _mail_hits(client, phrase: str) -> list[dict]:
    reply = client.call("fauna.search.query", {"query": phrase, "limit": 20})
    return [r for r in reply.get("results", []) if r.get("content_type") == "mail"]


def _materialization_grant_ids(db_path: str, owner: bytes) -> list[bytes]:
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT grant_id FROM capability_grants WHERE owner_actor_id = ?",
            (owner,),
        ).fetchall()
    finally:
        conn.close()
    return [bytes(r[0]) for r in rows]


def _epoch_seal_key_count(db_path: str, owner: bytes) -> int:
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        (n,) = conn.execute(
            "SELECT COUNT(*) FROM actor_epoch_seal_keys WHERE actor_id = ?",
            (owner,),
        ).fetchone()
    finally:
        conn.close()
    return int(n)


@pytest.mark.feature("search")
def test_your_mail_kept_searchable_on_your_nest_is_found_by_you_alone_and_turning_it_off_deletes_it(
    nest_instance, mail_bridge_mta
):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register
    from helpers.mail_aliases import add_exact_alias

    url = nest_instance["url"]
    admin_key = nest_instance["admin"]["signing_key"]
    owner = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_key)
    other = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_key)
    owner_seed = bytes(owner["signing_key"])

    # ── Arrange: mail on with the epoch schedule an app publishes, so the MTA
    #    seals the mail under a per-epoch key — the case every app produces —
    #    and an address the MTA routes to the owner. ──
    fauna_ffi.harness_enable_mail(url, owner_seed, epoch_schedule=True)
    assert _epoch_seal_key_count(nest_instance["db_path"], owner["actor_id_bytes"]) > 0, (
        "the enable must publish the owner's content-sealing epoch schedule")
    address = add_exact_alias(url, owner["signing_key"], mail_bridge_mta.domain,
                              f"keep-{owner['actor_id_hex'][:10]}")

    # ── Act: the "Keep my mail searchable" grant, from the Nests page's option. ──
    fauna_ffi.harness_mint_trust(url, owner_seed, "materialize_mail")
    grants = _materialization_grant_ids(nest_instance["db_path"], owner["actor_id_bytes"])
    assert len(grants) == 1, f"the mint must deposit one grant, found {len(grants)}"

    nonce = f"heronfield{time.time_ns()}"
    raw = (
        f"From: External Sender <sender@{SENDER_DOMAIN}>\r\n"
        f"To: {address}\r\n"
        "Subject: Field notes\r\n"
        f"Message-ID: <{nonce}@{SENDER_DOMAIN}>\r\n"
        "Date: Sun, 08 Nov 2026 12:00:00 +0000\r\n"
        "\r\n"
        f"The {nonce} nested by the reeds this spring.\r\n"
    ).encode()
    _deliver(mail_bridge_mta.mx_port, mail_bridge_mta.domain, address, raw,
             time.monotonic() + 60.0)

    area = (Path(nest_instance["db_path"]).parent / "__materialized"
            / owner["actor_id_hex"])
    owner_ws = WsRpcAdminClient(url, actor_id=owner["actor_id_bytes"],
                                signing_key=owner_seed)
    other_ws = WsRpcAdminClient(url, actor_id=other["actor_id_bytes"],
                                signing_key=bytes(other["signing_key"]))
    with owner_ws, other_ws:
        # ── Assert: the owner finds the phrase. ──
        deadline = time.monotonic() + _INDEXED_S
        hits = _mail_hits(owner_ws, nonce)
        while not hits and time.monotonic() < deadline:
            time.sleep(0.25)
            hits = _mail_hits(owner_ws, nonce)
        assert len(hits) == 1, (
            f"the owner's search must find the delivered mail by a body phrase "
            f"within {_INDEXED_S}s; mail hits: {hits!r}"
        )
        assert hits[0]["content_id"] == f"<{nonce}@{SENDER_DOMAIN}>", (
            f"the hit's content id must be the RFC Message-ID: {hits[0]!r}")
        assert area.is_dir(), f"the materialized area must exist at {area}"

        # ── Assert: nobody else's search does. ──
        assert _mail_hits(other_ws, nonce) == [], (
            "another member's search must never reach the owner's materialized mail")

        # ── Act + assert: turning it off deletes the nest's copy. ──
        owner_ws.call("fauna.capabilities.revoke", {"grant_id": grants[0]})
        assert not area.exists(), (
            f"revoke must delete the materialized area before it replies: {area}")
        assert _mail_hits(owner_ws, nonce) == [], (
            "after revoke the owner's search must no longer find the mail")

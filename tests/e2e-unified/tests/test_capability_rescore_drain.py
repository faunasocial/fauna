"""tier_3 BUILD-SUCCESS BAR: a re-score-on-model-change fires on an untrusted box
via a user-minted capability.

This is the LEAD build-success bar for the capability-mediated content-processing
design (tracked internally; § 2.6): a background re-score drain, running inside the real
`fauna-mail-bridge` MDA process, re-scores stale mail against a bumped model
version — but ONLY under a live user-minted `content.read{mail}` +
`content.label-write` capability grant, and it goes dark on revoke while the
obligation stays owed (§ 2.5 drain rendezvous).

Production data flow asserted end-to-end on real binaries (nest + MDA bridge,
real seal/open/scan, real `fauna.capabilities.*` + `submit_scores` WS-RPC):

  inbound SMTP → MTA seals to the recipient's MSEK-derived pubkey +
  `ingest_inbound_mail` writes a `content_scores` clamav row at the built-in
  scorer_version (1) → a model-version bump (clamav 1→2, i.e. a redeploy with a
  bumped `scorer_version::CLAMAV`) leaves the row behind → the owner mints a
  `content.read{mail}`+`content.label-write` grant to the MDA holder → a
  `config_changed` push wakes the drain → `rescore_worklist` → `fetch_message_
  ciphertext` → `open_mail_record_with_key` (the grant key) → co-resident clamd
  re-scan → `submit_scores` bumps the row to v2. Revoke → the drain goes dark
  (row stays owed) → re-mint → it drains to v3 (the obligation survived).

The mint here is the client-side operation the all-6-client capability settings
page performs in production (when that leg lands); the seal-helper drives it as a fixture precondition per
the E2E testing rules' fixture-setup carve-out (b) — arranging the world
(a user minted a grant),
not the mutation under test (the drain re-scoring).

Scanner realism: the MDA's drain re-runs the co-resident fake clamd/rspamd wired
onto the `mail_bridge_mda` fixture (a benign INSTREAM verdict). nest + bridge +
crypto are all real; only the AV daemon is stubbed — so this is tier_3 with the
fake-scanner caveat (a full-real-clamd variant belongs in `tests/platform/docker/`,
tier_4). No client driver participates (server-side / independent).
"""

import base64
import secrets
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import RESCORE_WORKLIST_SERVE_S
from helpers.mail_wire import _connect_smtp_starttls
from helpers.recipient_seal_key import derive_recipient_seal_key
from helpers.waiting import await_rescore_worklist_serve_after, rescore_worklist_serves

pytestmark = pytest.mark.tier_3


def _deliver_inbound(handle, raw_message: bytes, deadline: float) -> None:
    """Drive one real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's
    port-25 STARTTLS listener to the fixture's recipient (250 on `.` follows the
    synchronous `ingest_inbound_mail`)."""
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{handle.recipient_username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _clamav_content_ids(db_path: str, actor_id: bytes) -> set:
    """The set of the actor's `content_scores` content_ids for factor clamav."""
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.execute(
            "SELECT content_id FROM content_scores WHERE actor_id = ? AND factor = 'clamav'",
            (actor_id,),
        )
        return {bytes(cid) for (cid,) in cur.fetchall()}
    finally:
        conn.close()


def _clamav_scorer_version(db_path: str, actor_id: bytes, content_id: bytes):
    """The clamav `scorer_version` for one content item, or None if no row."""
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.execute(
            "SELECT scorer_version FROM content_scores "
            "WHERE actor_id = ? AND content_id = ? AND factor = 'clamav'",
            (actor_id, content_id),
        )
        row = cur.fetchone()
        return row[0] if row else None
    finally:
        conn.close()


def _bump_model_version(db_path: str, model_kind: str, version: int) -> None:
    """Raise the model-version registry for `model_kind` (a monotonic bump = a
    redeploy with a higher built-in `scorer_version::*`). No RPC exposes this —
    the registry is nest-internal; SQLite poke is the harness path (mirrors
    `_seed_spam_model`)."""
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "INSERT INTO model_versions (model_kind, version, updated_at) VALUES (?, ?, ?) "
            "ON CONFLICT(model_kind) DO UPDATE SET version = excluded.version, "
            "updated_at = excluded.updated_at",
            (model_kind, version, int(time.time())),
        )
        conn.commit()
    finally:
        conn.close()


def _recipient_ws(nest_url: str, recipient):
    """A User-class WS-RPC client authenticated as the recipient owner (mints /
    revokes its own grants)."""
    return WsRpcAdminClient(
        nest_url,
        actor_id=recipient.actor_id,
        signing_key=bytes(recipient.recipient["signing_key"]),
    )


def _mint_grant(
    run_seal_helper, nest_url, recipient, holder_pubkey, *, epoch_start, epoch_end,
    holder_mlkem_ek=None,
):
    """Build (via the seal-helper `mint-grant` mode) and deposit (via
    `fauna.capabilities.mint`, as the owner) a content.read{mail}+content.label-write
    grant to the MDA holder. Returns the 16-byte grant_id.

    `holder_mlkem_ek` (the holder's published 1184-byte ML-KEM ek) selects the wrap
    suite: present ⇒ the X-Wing wrap (PQ-CAP-4, so a harvested `capability_grants`
    row is not a CRQC-openable bypass of the closed mail seal); None ⇒ the classical
    X25519 wrap. The mail payload is the 32+2400 X-Wing superset either way."""
    grant_id = secrets.token_bytes(16)
    params = {
        "owner_actor_id_b64": base64.b64encode(recipient.actor_id).decode(),
        "grant_id_b64": base64.b64encode(grant_id).decode(),
        "holder_pubkey_b64": base64.b64encode(holder_pubkey).decode(),
        "msek_b64": base64.b64encode(recipient.msek).decode(),
        "epoch_start": epoch_start,
        "epoch_end": epoch_end,
    }
    if holder_mlkem_ek is not None:
        params["holder_mlkem_ek_b64"] = base64.b64encode(holder_mlkem_ek).decode()
    grant_blob = run_seal_helper("mint-grant", params)
    with _recipient_ws(nest_url, recipient) as ws:
        reply = ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})
    assert reply.get("ok") is True, f"mint reply not ok: {reply!r}"
    return grant_id


def _revoke_grant(nest_url, recipient, grant_id: bytes) -> None:
    with _recipient_ws(nest_url, recipient) as ws:
        reply = ws.call("fauna.capabilities.revoke", {"grant_id": grant_id})
    assert reply.get("ok") is True, f"revoke reply not ok: {reply!r}"


def _observe_content_rescore_lease(nest_url, recipient):
    """The owner's `content-rescore` advisory lease as any of their clients
    observes it (`fauna.delegation.observe`, self-scoped) — or None when free.
    Task-delegation slice 6: a nest holding a sufficient grant heartbeats this
    lease, so the Task-delegation page shows it as the kind's runner."""
    with _recipient_ws(nest_url, recipient) as ws:
        reply = ws.call(
            "fauna.delegation.observe", {"task_kinds": ["content-rescore"]}
        )
    leases = reply.get("leases", [])
    return leases[0] if leases else None


def _poll_lease_free(nest_url, recipient, timeout=10.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if _observe_content_rescore_lease(nest_url, recipient) is None:
            return True
        time.sleep(0.5)
    return False


def _poke_config_changed(nest_url, admin) -> None:
    """Nudge the nest to fan a `config_changed` push to every approved bridge —
    waking the MDA's drain. `put_spam_policy` (re-applying the fixture's permissive
    values) is the cleanest trigger (it fans the push without rebinding listeners)."""
    admin_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        admin_ws.call(
            "fauna.bridges.put_spam_policy",
            {
                "baseline_standing_publish": False,
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "max_conn_per_min": 1000,
            },
        )


def _poll_scorer_version(db_path, actor_id, content_id, expected, timeout=30.0):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = _clamav_scorer_version(db_path, actor_id, content_id)
        if last == expected:
            return last
        time.sleep(0.25)
    return last


@pytest.mark.feature("task-delegation")
def test_rescore_drain_fires_on_model_bump_via_user_minted_capability(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    db_path = nest_instance["db_path"]
    nest_url = nest_instance["url"]
    admin = nest_instance["admin"]
    holder_pubkey = mail_bridge_mda.x25519_pubkey
    now = int(time.time())

    handle.assert_mta_running()

    # ── 1. Deliver a clean inbound mail. It lands a `content_scores` clamav row at
    # the built-in scorer_version (1). Identify the new content_id by diffing the
    # recipient's clamav rows before/after (robust to a shared session recipient).
    before = _clamav_content_ids(db_path, recipient.actor_id)
    nonce = f"rescoredrain{int(time.time() * 1000)}qx"
    raw = (
        "\r\n".join(
            [
                "From: External Sender <sender@external.test>",
                f"To: {handle.recipient_username}",
                "Subject: re-score drain seam proof",
                f"Message-ID: <{nonce}@external.test>",
                "Date: Mon, 06 Jul 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"Body {nonce} — clean, for the re-score drain.",
            ]
        )
        + "\r\n"
    ).encode()
    _deliver_inbound(handle, raw, time.monotonic() + 40.0)

    content_id = None
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        new = _clamav_content_ids(db_path, recipient.actor_id) - before
        if new:
            assert len(new) == 1, f"expected exactly one new clamav row, got {len(new)}"
            content_id = next(iter(new))
            break
        time.sleep(0.25)
    assert content_id is not None, "inbound delivery produced no clamav content_scores row"
    assert _clamav_scorer_version(db_path, recipient.actor_id, content_id) == 1, (
        "ingest must stamp the built-in clamav scorer_version (1)"
    )

    # ── 2. Bump the clamav model version 1→2 → the row now owes a re-score
    # (scorer_version 1 < registry 2).
    _bump_model_version(db_path, "clamav", 2)

    # ── 3. The user mints a content.read{mail}+content.label-write grant to the
    # MDA holder — the only confidentiality boundary that lets an untrusted box
    # re-read content (design § security analysis).
    grant_id = _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600
    )

    # ── 4. Wake the drain (config_changed) → it fetches the grant, re-scores the
    # stale row, and `submit_scores` bumps it to v2.
    _poke_config_changed(nest_url, admin)
    got = _poll_scorer_version(db_path, recipient.actor_id, content_id, expected=2)
    assert got == 2, (
        f"the drain did not re-score under the user-minted grant "
        f"(clamav scorer_version={got}, want 2; MDA log {mail_bridge_mda.log_file})"
    )

    # Task-delegation slice 6: holding the grant made the NEST the kind's
    # runner — the advisory lease the owner's clients observe carries a fresh
    # Nest holder (participants.md § Dispatch by kind: the grant IS the
    # assignment).
    lease = _observe_content_rescore_lease(nest_url, recipient)
    assert lease is not None and "Nest" in lease["holder"], (
        f"the granted nest must hold the content-rescore lease, got {lease!r}"
    )
    assert lease["holder_class"] == "AlwaysOnNest", lease

    # ── 5. Revoke arm: revoke the grant, THEN bump 2→3, then wake the drain. A
    # revoked holder has no grant → empty worklist → no write-back, so the drain
    # goes dark and the obligation STAYS owed (the row must not advance past 2).
    # Revoke-before-bump is load-bearing: a trailing coalesced drain run can be
    # mid-batch-loop right now, and a bump landing while the grant is still live
    # is legitimately drained (the labeler twin flaked exactly so, 2026-07-13).
    # The barrier is the nest's own decision, not elapsed time (convention 14):
    # a worklist serve counted after the revoke+bump was decided against the
    # post-revoke grant set. Without one, "the row did not advance" is vacuous —
    # the drain may simply not have looked yet.
    serves, _ = rescore_worklist_serves(nest_url)
    _revoke_grant(nest_url, recipient, grant_id)
    _bump_model_version(db_path, "clamav", 3)
    _poke_config_changed(nest_url, admin)
    await_rescore_worklist_serve_after(
        nest_url, serves, budget_s=RESCORE_WORKLIST_SERVE_S, what="the grant revoke + version bump"
    )
    assert _clamav_scorer_version(db_path, recipient.actor_id, content_id) == 2, (
        "a revoked grant must leave the drain dark — the obligation stays owed at v2"
    )

    # Task-delegation slice 6: the revoke also RELEASED the nest's lease —
    # "revoking the grant unassigns it" is prompt (the runner wake), not a
    # LEASE_STALE_MS wait.
    assert _poll_lease_free(nest_url, recipient), (
        "revoking the grant must free the nest-held content-rescore lease, got "
        f"{_observe_content_rescore_lease(nest_url, recipient)!r}"
    )

    # ── 6. Re-mint → wake the drain → the still-owed obligation drains to v3,
    # proving revoke kept it owed (not silently dropped).
    _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600
    )
    _poke_config_changed(nest_url, admin)
    got = _poll_scorer_version(db_path, recipient.actor_id, content_id, expected=3)
    assert got == 3, (
        f"the re-minted grant did not drain the still-owed obligation "
        f"(clamav scorer_version={got}, want 3; MDA log {mail_bridge_mda.log_file})"
    )


# ── PQ-CAP-4: the hybrid (X-Wing) variant ─────────────────────────────────────


def _recipient_ek(recipient, run_seal_helper) -> bytes:
    """The MSEK recipient's 1184-byte ML-KEM ek, re-derived from its MSEK — the
    half `_provision_msek_recipient` published beside the X25519 pubkey
    (`helpers/recipient_seal_key.py`), which is why the MTA seals this
    recipient's inbound mail X-Wing: `EncryptToRecipientHybrid` selects the
    suite on a valid 1184-B ek, and the only degrade is a seal *error*, which a
    well-formed derived ek never triggers."""
    return derive_recipient_seal_key(
        recipient.msek, run_seal_helper=run_seal_helper
    ).mlkem_ek


def _stored_recipient_ek(db_path, actor_id):
    """The recipient's stored ML-KEM ek (`actor_mls_pubkeys.mlkem_ek`), or None."""
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.execute(
            "SELECT mlkem_ek FROM actor_mls_pubkeys WHERE actor_id = ?", (actor_id,)
        )
        row = cur.fetchone()
        return bytes(row[0]) if row and row[0] is not None else None
    finally:
        conn.close()


def _holder_mlkem_ek(mail_bridge_mda, run_seal_helper) -> bytes:
    """The MDA capability holder's 1184-byte ML-KEM ek, derived from its keyfile
    Ed25519 seed (PQ-CAP-2 — the SAME seed the running bridge derives its *unseal* dk
    from, so a grant X-Wing-wrapped to this ek pairs with the holder's runtime dk).
    PQ-CAP-3 separately proves the production publish → `fetch_bridge_pubkey` path a
    real client mint uses instead of this local derivation."""
    import cbor2

    with open(mail_bridge_mda.keypair_file, "rb") as f:
        keyfile = cbor2.loads(f.read())
    seed = keyfile["ed25519_seed"]
    ek = run_seal_helper(
        "derive-bridge-mlkem-ek", {"ed25519_seed_b64": base64.b64encode(seed).decode()}
    )
    assert len(ek) == 1184, f"holder ek must be 1184 bytes, got {len(ek)}"
    return ek


def test_rescore_drain_hybrid_mail_under_xwing_grant(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    """tier_3 PQ-CAP-4: the re-score drain fires on X-Wing-sealed mail via a
    user-minted X-Wing capability grant. The recipient published an ML-KEM ek at
    provisioning, as every recipient does, so the MTA seals its inbound mail
    X-Wing in both tests. Two deltas from the classically wrapped grant above:

      * the grant is X-Wing-WRAPPED (holder ek) carrying the 32+2400 mail key, so the
        MDA holder can only recover it by opening the wrap with its own ML-KEM dk
        (PQ-CAP-2's holder-dk threading in the real bridge process); and
      * that 32+2400 key opens the X-Wing-sealed record (the drain's
        `open_mail_record_with_key`).

    A successful re-score therefore proves BOTH the holder-dk grant-unseal AND the
    hybrid-record open, end-to-end on real nest + MDA binaries. The in-process crypto
    proof — incl. the negative control that a 32-byte classical key CANNOT open the
    X-Wing record (so the drain isn't silently servicing a classical fallback) — is
    seal_test.go's `TestMintGrantXwingDrainsHybridMail`.
    """
    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    db_path = nest_instance["db_path"]
    nest_url = nest_instance["url"]
    admin = nest_instance["admin"]
    holder_pubkey = mail_bridge_mda.x25519_pubkey
    now = int(time.time())

    handle.assert_mta_running()

    # ── 1. The recipient's ek, as the nest holds it, + the holder ek. Assert the
    # stored ek is the MSEK-derived one — else the MTA would not seal to the key
    # the grant opens, and a degraded classical seal would make this test a false
    # green (the 32+2400 grant key opens classical mail too; the genuine-hybrid
    # assertion lives in the Go negative-control test).
    recipient_ek = _recipient_ek(recipient, run_seal_helper)
    assert _stored_recipient_ek(db_path, recipient.actor_id) == recipient_ek, (
        "the recipient's stored ML-KEM ek is not its MSEK-derived one"
    )
    holder_ek = _holder_mlkem_ek(mail_bridge_mda, run_seal_helper)

    # ── 2. Deliver a clean inbound mail. Now X-Wing-sealed; it lands a clamav row at
    # the built-in scorer_version (1). Diff before/after to identify the new row.
    before = _clamav_content_ids(db_path, recipient.actor_id)
    nonce = f"pqhybriddrain{int(time.time() * 1000)}qx"
    raw = (
        "\r\n".join(
            [
                "From: External Sender <sender@external.test>",
                f"To: {handle.recipient_username}",
                "Subject: PQ hybrid re-score drain proof",
                f"Message-ID: <{nonce}@external.test>",
                "Date: Mon, 06 Jul 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"Body {nonce} — clean, for the X-Wing re-score drain.",
            ]
        )
        + "\r\n"
    ).encode()
    _deliver_inbound(handle, raw, time.monotonic() + 40.0)

    content_id = None
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        new = _clamav_content_ids(db_path, recipient.actor_id) - before
        if new:
            assert len(new) == 1, f"expected exactly one new clamav row, got {len(new)}"
            content_id = next(iter(new))
            break
        time.sleep(0.25)
    assert content_id is not None, "hybrid inbound delivery produced no clamav content_scores row"
    assert _clamav_scorer_version(db_path, recipient.actor_id, content_id) == 1, (
        "ingest must stamp the built-in clamav scorer_version (1)"
    )

    # ── 3. Bump the clamav model version 1→2 → the row owes a re-score.
    _bump_model_version(db_path, "clamav", 2)

    # ── 4. The owner mints an X-Wing-WRAPPED grant (holder ek) to the MDA holder —
    # the confidentiality boundary that lets the untrusted box re-read hybrid content.
    _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey,
        epoch_start=now, epoch_end=now + 3600, holder_mlkem_ek=holder_ek,
    )

    # ── 5. Wake the drain → it opens the X-Wing record with its ML-KEM dk +
    # the 32+2400 grant key, re-scores, and `submit_scores` bumps the row to v2.
    _poke_config_changed(nest_url, admin)
    got = _poll_scorer_version(db_path, recipient.actor_id, content_id, expected=2)
    assert got == 2, (
        f"the drain did not re-score X-Wing-sealed mail under the user-minted X-Wing grant "
        f"(clamav scorer_version={got}, want 2; MDA log {mail_bridge_mda.log_file})"
    )

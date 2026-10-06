"""tier_3 FRAME BAR (leg 5): a community-labeler tier-3 score lands on the owner's
content via a user-minted capability, driven end-to-end over the real wire.

This is the last unbuilt piece of `content-moderation-and-ranking.md` frame leg 5
(the `fauna.labelers.*` community-labeler registry). It parallels
`test_capability_rescore_drain.py`, but the obligation is created by a **labeler
subscription** rather than a built-in model-version bump: a published, signed WASM
labeler scores the owner's mail inside the real `fauna-mail-bridge` MDA holder,
under a live `content.read{mail}` + `content.label-write` grant, writing a
`labeler:<hex>` tier-3 `ScoreEntry`.

Production data flow asserted end-to-end on real binaries (nest + MDA bridge, real
seal/open, real WASM execution in the fuel/memory-bounded wasmi sandbox, real
`fauna.labelers.*` + `fauna.capabilities.*` + `submit_scores` WS-RPC):

  a publisher publishes a signed `content_kind="mail"` cat-detector labeler
  (`fauna.labelers.publish`) → the owner delivers mail → mints the PER-LABELER
  content.read{mail}+content.label-write grant (every tuple and wrap confined
  to `labeler:<hex>` — the composed "read and filter my mail" grant licenses no
  community labeler) to the MDA holder → subscribes
  (`fauna.labelers.subscribe`), which registers the labeler's model version and
  seeds a `content_scores` backlog row (scorer_version 0) per mail → a
  `config_changed` push wakes the drain → `rescore_worklist` → `inspect` the
  module → `open_mail_record` (the grant key) → `run_wasm_labeler_score` (BARE
  `label()` ABI) → `submit_scores` writes `labeler:<hex>` {tier:3, score}. A cat
  mail scores 900; a non-cat mail scores 0 (empty `Vec<Label>`). Revoke → the
  drain goes dark (obligation stays owed) → re-mint → it drains (obligation
  survived).

The publish/mint/subscribe here are the client-side operations the all-6-client
capability + catalog UI performs in production (when that leg lands); the seal-helper drives them as fixture preconditions per the
E2E testing rules' fixture-setup carve-out (b) — arranging the world (a user
published/subscribed/granted), not the mutation under test (the drain scoring).

Scanner realism: nest + bridge + all crypto + the WASM execution are real; only
the co-resident AV daemons wired onto the `mail_bridge_mda` fixture are stubbed,
and the labeler path does not touch them. So this is tier_3 (a full-real-image
variant belongs in `tests/platform/docker/`, tier_4). No client driver
participates (server-side / independent).

The cat labeler WASM is the committed fixture `fixtures/labeler/cat_labeler.wat`,
pinned in-process by the Rust test `cat_labeler_wat_fixture_scores_cat_and_not`
(`libs/fauna-ffi/src/labeler.rs`); the seal-helper `publish-labeler` mode is
pinned by `TestPublishLabelerRoundTrip`.
"""

import base64
import secrets
import sqlite3
import time
from pathlib import Path

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.budgets import RESCORE_WORKLIST_SERVE_S
from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import await_rescore_worklist_serve_after, rescore_worklist_serves

pytestmark = pytest.mark.tier_3

# The committed BARE-emitting cat-detector fixture, published verbatim as the
# labeler's `wasm_bytes` (wasmi parses WAT text at both the nest publish gate and
# the MDA holder, so these bytes ARE the module — `wasm_hash`/`wasm_size` bind to
# exactly this file).
_CAT_WAT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "labeler" / "cat_labeler.wat"


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
    """The set of the actor's `content_scores` content_ids for factor clamav —
    ingest stamps one per inbound mail, so diffing before/after a delivery yields
    that mail's content_id (== `message_id`, shared across every factor, so the
    labeler seed/score rows key on the same id)."""
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.execute(
            "SELECT content_id FROM content_scores WHERE actor_id = ? AND factor = 'clamav'",
            (actor_id,),
        )
        return {bytes(cid) for (cid,) in cur.fetchall()}
    finally:
        conn.close()


def _labeler_row(db_path: str, actor_id: bytes, content_id: bytes, factor: str):
    """(score, scorer_version) of one content item's `labeler:<hex>` row, or None."""
    conn = sqlite3.connect(db_path)
    try:
        cur = conn.execute(
            "SELECT score, scorer_version FROM content_scores "
            "WHERE actor_id = ? AND content_id = ? AND factor = ?",
            (actor_id, content_id, factor),
        )
        row = cur.fetchone()
        return (row[0], row[1]) if row else None
    finally:
        conn.close()


def _bump_model_version(db_path: str, model_kind: str, version: int) -> None:
    """Raise the model-version registry for `model_kind` (== the labeler factor).
    No RPC exposes this — the registry is nest-internal; the SQLite poke is the
    harness path (mirrors `test_capability_rescore_drain._bump_model_version`,
    which bumps `clamav`). A bump makes the already-scored rows owe a re-score."""
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
    revokes its own grants, subscribes to labelers)."""
    return WsRpcAdminClient(
        nest_url,
        actor_id=recipient.actor_id,
        signing_key=bytes(recipient.recipient["signing_key"]),
    )


def _mint_grant(
    run_seal_helper, nest_url, recipient, holder_pubkey, *, epoch_start, epoch_end, labeler_id
):
    """Build (via the seal-helper `mint-grant` mode) and deposit (via
    `fauna.capabilities.mint`, as the owner) the PER-LABELER
    content.read{mail}+content.label-write grant to the MDA holder — every tuple
    and wrap confined to `labeler:<hex>` of ``labeler_id``, the grant a labeler
    subscription over sealed mail mints. The composed "read and filter my mail"
    grant licenses no community labeler, so a drain under it runs none.
    Returns the 16-byte grant_id."""
    grant_id = secrets.token_bytes(16)
    grant_blob = run_seal_helper(
        "mint-grant",
        {
            "owner_actor_id_b64": base64.b64encode(recipient.actor_id).decode(),
            "grant_id_b64": base64.b64encode(grant_id).decode(),
            "holder_pubkey_b64": base64.b64encode(holder_pubkey).decode(),
            "msek_b64": base64.b64encode(recipient.msek).decode(),
            "epoch_start": epoch_start,
            "epoch_end": epoch_end,
            "labeler_id_b64": base64.b64encode(labeler_id).decode(),
        },
    )
    with _recipient_ws(nest_url, recipient) as ws:
        reply = ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})
    assert reply.get("ok") is True, f"mint reply not ok: {reply!r}"
    return grant_id


def _revoke_grant(nest_url, recipient, grant_id: bytes) -> None:
    with _recipient_ws(nest_url, recipient) as ws:
        reply = ws.call("fauna.capabilities.revoke", {"grant_id": grant_id})
    assert reply.get("ok") is True, f"revoke reply not ok: {reply!r}"


def _poke_config_changed(nest_url, admin) -> None:
    """Nudge the nest to fan a `config_changed` push to every approved bridge —
    waking the MDA's drain. `put_spam_policy` (re-applying permissive values) is
    the cleanest trigger (it fans the push without rebinding listeners)."""
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


def _publish_cat_labeler(run_seal_helper, nest_url, port, admin, now):
    """Publish a signed `content_kind="mail"` cat-detector labeler as a fresh
    registered User whose keypair is ALSO the labeler's `algorithm_id` (signer;
    public-key-is-identity). Returns (labeler_id_bytes, factor)."""
    publisher = create_actor_and_register(
        port, base_url=nest_url, admin_signing_key=admin["signing_key"]
    )
    pub_seed = bytes(publisher["signing_key"])  # 32-byte Ed25519 seed
    labeler_id = publisher["actor_id_bytes"]  # == algorithm_id (verify key)
    wat = _CAT_WAT_PATH.read_bytes()
    metadata_blob = run_seal_helper(
        "publish-labeler",
        {
            "signing_seed_b64": base64.b64encode(pub_seed).decode(),
            "wasm_b64": base64.b64encode(wat).decode(),
            "version": 1,
            "needs_text": True,
            "needs_hashtags": False,
            "needs_media_metadata": False,
            "needs_author": False,
            "max_memory_bytes": 16 * 1024 * 1024,
            "max_cpu_microseconds": 100_000,
            "updated_at": now,
        },
    )
    pub_ws = WsRpcAdminClient(nest_url, actor_id=labeler_id, signing_key=pub_seed)
    with pub_ws:
        reply = pub_ws.call(
            "fauna.labelers.publish",
            {
                "metadata_blob": metadata_blob,
                "wasm_bytes": wat,
                "content_kind": "mail",
                "artifact_kind": "wasm",
            },
        )
    assert reply.get("ok") is True, f"publish reply not ok: {reply!r}"
    assert bytes(reply["labeler_id"]) == labeler_id, "publish echoed a different labeler_id"
    return labeler_id, "labeler:" + labeler_id.hex()


def _subscribe(nest_url, recipient, labeler_id: bytes, grant_id: bytes) -> str:
    """Subscribe the owner to `labeler_id` under `grant_id` (seeds the backlog +
    registers the labeler's model version). Returns the registered factor."""
    with _recipient_ws(nest_url, recipient) as ws:
        reply = ws.call(
            "fauna.labelers.subscribe", {"labeler_id": labeler_id, "grant_id": grant_id}
        )
    assert reply.get("ok") is True, f"subscribe reply not ok: {reply!r}"
    return reply["factor"]


def _unsubscribe(nest_url, recipient, labeler_id: bytes) -> None:
    with _recipient_ws(nest_url, recipient) as ws:
        reply = ws.call("fauna.labelers.unsubscribe", {"labeler_id": labeler_id})
    assert reply.get("ok") is True, f"unsubscribe reply not ok: {reply!r}"


def _deliver_and_capture(handle, db_path, subject: str, body: str) -> bytes:
    """Deliver one inbound mail and return its content_id, captured by diffing the
    recipient's clamav rows before/after (robust to a shared session recipient)."""
    recipient = handle.recipient
    before = _clamav_content_ids(db_path, recipient.actor_id)
    nonce = secrets.token_hex(6)  # hex has no 't', so a nonce can never spell "cat"
    raw = (
        "\r\n".join(
            [
                "From: External Sender <sender@external.test>",
                f"To: {handle.recipient_username}",
                f"Subject: {subject}",
                f"Message-ID: <{nonce}@external.test>",
                "Date: Wed, 08 Jul 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"{body} ref {nonce}",
            ]
        )
        + "\r\n"
    ).encode()
    _deliver_inbound(handle, raw, time.monotonic() + 40.0)
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        new = _clamav_content_ids(db_path, recipient.actor_id) - before
        if new:
            assert len(new) == 1, f"expected exactly one new clamav row, got {len(new)}"
            return next(iter(new))
        time.sleep(0.25)
    raise AssertionError(f"delivery of {subject!r} produced no clamav content_scores row")


def _poll_labeler_version(db_path, actor_id, content_id, factor, expected, timeout=30.0):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        row = _labeler_row(db_path, actor_id, content_id, factor)
        last = row[1] if row else None
        if last == expected:
            return last
        time.sleep(0.25)
    return last


@pytest.mark.feature("community-labelers")
def test_labeler_drain_scores_mail_via_subscription_and_user_minted_capability(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    db_path = nest_instance["db_path"]
    nest_url = nest_instance["url"]
    port = nest_instance["port"]
    admin = nest_instance["admin"]
    holder_pubkey = mail_bridge_mda.x25519_pubkey
    now = int(time.time())

    handle.assert_mta_running()

    # ── 1. A publisher publishes a signed mail-kind cat-detector labeler.
    labeler_id, factor = _publish_cat_labeler(run_seal_helper, nest_url, port, admin, now)

    # ── 2. Deliver a cat mail and a non-cat mail. Both enter the owner's mail
    # universe (`message_scan_results`), so the subscribe-time backlog seed picks
    # up both. content_id is stable across factors, so the clamav-row diff
    # identifies each mail's labeler row too.
    cat_id = _deliver_and_capture(handle, db_path, "feline note", "I love my cat.")
    plain_id = _deliver_and_capture(handle, db_path, "weather note", "The weather is nice.")
    assert cat_id != plain_id, "the two deliveries collided on content_id"

    # ── 3. The owner mints a content.read{mail}+content.label-write grant to the
    # MDA holder — the confidentiality boundary that lets the untrusted box unseal
    # and label the owner's mail.
    grant_id = _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600,
        labeler_id=labeler_id,
    )

    # ── 4. The owner subscribes — registers the labeler's model version (1) and
    # seeds a `content_scores` backlog row (scorer_version 0) per mail, so both
    # rows now owe a re-score. The reply's factor must match `labeler:<hex>`.
    got_factor = _subscribe(nest_url, recipient, labeler_id, grant_id)
    assert got_factor == factor, f"subscribe registered {got_factor!r}, expected {factor!r}"

    # ── 5. Wake the drain → it inspects the module, unseals each mail under the
    # grant, runs the WASM, and submits: cat → 900, non-cat → 0, both advanced to
    # scorer_version 1 (the drain closes the obligation even when the score is
    # unchanged).
    _poke_config_changed(nest_url, admin)
    assert _poll_labeler_version(db_path, recipient.actor_id, cat_id, factor, expected=1) == 1, (
        f"the cat mail did not drain under the subscription's grant "
        f"(MDA log {mail_bridge_mda.log_file})"
    )
    cat_score, _ = _labeler_row(db_path, recipient.actor_id, cat_id, factor)
    assert cat_score == 900, f"the cat labeler must score a cat mail 900 per-mille, got {cat_score}"

    assert _poll_labeler_version(db_path, recipient.actor_id, plain_id, factor, expected=1) == 1, (
        "the non-cat mail's obligation was not closed by the drain"
    )
    plain_score, _ = _labeler_row(db_path, recipient.actor_id, plain_id, factor)
    assert plain_score == 0, (
        f"a non-cat mail must score 0 (empty Vec<Label> path), got {plain_score}"
    )

    # ── 6. Revoke arm: revoke the grant, THEN bump the labeler model version
    # 1→2 (a fresh obligation on the already-scored rows), then wake the drain.
    # A revoked holder has no content.read grant → empty worklist → no
    # write-back, so the drain goes dark and the obligation STAYS owed (the cat
    # row must not advance past 1). Revoke-before-bump is load-bearing: the
    # drain's coalesced pokes (mint/subscribe/config_changed pile-ups) can
    # leave a trailing runOnce looping worklist batches right now, and a bump
    # that lands while the grant is still live is legitimately drained —
    # observed as a flake under parallel-suite load 2026-07-13.
    # The barrier is the nest's own decision, not elapsed time (convention 14):
    # `rescore_worklist_handler` intersects the holder's LIVE grants with the
    # obligation gap, so a serve counted after the revoke+bump is a decision made
    # against the post-revoke world. Waiting for that is what makes the absence
    # below evidence rather than vacuity — without a serve, "the row did not
    # advance" is trivially true because the drain may never have looked.
    serves, _ = rescore_worklist_serves(nest_url)
    _revoke_grant(nest_url, recipient, grant_id)
    _bump_model_version(db_path, factor, 2)
    _poke_config_changed(nest_url, admin)
    await_rescore_worklist_serve_after(
        nest_url, serves, budget_s=RESCORE_WORKLIST_SERVE_S, what="the grant revoke + version bump"
    )
    _, cat_ver = _labeler_row(db_path, recipient.actor_id, cat_id, factor)
    assert cat_ver == 1, (
        "a revoked grant must leave the drain dark — the labeler obligation stays owed at v1"
    )

    # ── 7. Re-mint → wake the drain → the still-owed obligation drains to v2,
    # proving revoke kept it owed (not silently dropped), and the score is stable.
    regranted_id = _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600,
        labeler_id=labeler_id,
    )
    _poke_config_changed(nest_url, admin)
    assert _poll_labeler_version(db_path, recipient.actor_id, cat_id, factor, expected=2) == 2, (
        f"the re-minted grant did not drain the still-owed labeler obligation "
        f"(MDA log {mail_bridge_mda.log_file})"
    )
    cat_score2, _ = _labeler_row(db_path, recipient.actor_id, cat_id, factor)
    assert cat_score2 == 900, f"the re-drained cat mail must still score 900, got {cat_score2}"

    # ── 8. Unsubscribe closes the client surface (subscribe/unsubscribe are the
    # owner-only ops); it deletes the subscription without disturbing prior scores.
    _unsubscribe(nest_url, recipient, labeler_id)

    # ── 9. Revoke the step-7 grant. Not hygiene theatre: the fixtures are
    # session-scoped, so this owner and the MDA holder are SHARED with every
    # sibling drain test, and step 7's re-mint is a `content.read{mail}` +
    # `content.label-write` grant with an hour-long window. Left live, it silently
    # breaks any later test whose premise is "this holder has no grant for this
    # owner" — `test_capability_rescore_drain.py`'s revoke arm is exactly that,
    # and it fails `assert 3 == 2` (the drain re-scores under THIS grant) when it
    # runs after this test. Verified 2026-08-13 as a pre-existing order
    # dependence: the arm fails identically with its original settle-sleep, so
    # the leak is this residue, not the barrier that surfaced it.
    _revoke_grant(nest_url, recipient, regranted_id)


@pytest.mark.feature("community-labelers")
def test_labeler_obligation_seeded_and_drained_at_ingest_without_config_changed(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    """S5 Arm 1 — the ingest-drain FAST PATH (`content-scoring.md` § Timing →
    *Delivery-time fast path*; design spec D5 *Ingest trigger*): a mail delivered
    AFTER the owner has already subscribed + granted must have its per-user labeler
    obligation created at ingest and drained *moments after delivery*, with NO
    `config_changed` poke.

    This is the inverse ordering of
    `test_labeler_drain_scores_mail_via_subscription_and_user_minted_capability`:
    there the mail predates the subscription, so the subscribe-time
    `seed_factor_backlog` covers it; here the mail arrives after, so that seed
    cannot — the obligation MUST be created by `persist_decoded_inbound_mail`
    (Arm 1 step 1: enumerate the owner's subscribed WASM/mail labelers +
    `INSERT OR IGNORE` a `labeler:<hex>` `content_scores` row at scorer_version 0)
    and the drain woken by the `fauna.bridges.rescore_ready` ingest push
    (Arm 1 step 2), never by a `config_changed` poke.

    Isolation of the ingest trigger: publish/mint/subscribe all happen BEFORE any
    mail exists, and the mint's `config_changed` (holder-refresh) is allowed to
    settle before delivery, so no obligation exists during any pre-delivery drain
    cycle. From delivery onward the test never pokes `config_changed`; a drain that
    closes the obligation is therefore attributable ONLY to the ingest push.
    """
    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    db_path = nest_instance["db_path"]
    nest_url = nest_instance["url"]
    port = nest_instance["port"]
    admin = nest_instance["admin"]
    holder_pubkey = mail_bridge_mda.x25519_pubkey
    now = int(time.time())

    handle.assert_mta_running()

    # ── 1. Arm the world fully BEFORE any mail exists: publish the cat labeler,
    # mint the grant, subscribe. The subscribe-time backlog seed finds zero items
    # (no `message_scan_results` rows yet), so nothing is owed at this point.
    #
    # The drain's worklist-serve counters are read here so step 2 can prove no
    # drain cycle ran while the world was being armed.
    _, armed_units = rescore_worklist_serves(nest_url)
    labeler_id, factor = _publish_cat_labeler(run_seal_helper, nest_url, port, admin, now)
    grant_id = _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600,
        labeler_id=labeler_id,
    )
    got_factor = _subscribe(nest_url, recipient, labeler_id, grant_id)
    assert got_factor == factor, f"subscribe registered {got_factor!r}, expected {factor!r}"

    # ── 2. The drain is idle here, and there is nothing to wait for — which the
    # 6s settle window this replaced did not know.
    #
    # That sleep was annotated "let the mint-triggered drain cycle settle". There
    # is no such cycle: `mint_grant_handler` emits NO `config_changed` push (it
    # calls `refresh_web_serve_holder` and wakes the lease runner —
    # `bins/fauna-nest/src/bridge_blob_handlers.rs`, the tail of the mint
    # handler), and neither does publish or subscribe. The MDA drain wakes on
    # exactly four things: bridge startup, a `config_changed` push, its 12h
    # backstop, and the `fauna.bridges.rescore_ready` ingest push
    # (`bins/fauna-bridges/internal/mda/rescore_drain.go`). None of them fires
    # between arming and delivery, so the sleep waited 6s for a run that never
    # ran — and the isolation this test needs is structural, not timing.
    #
    # Measured, not reasoned: waiting for a post-arming serve times out at the
    # full 90s budget with the counter still where it started
    # (`serves at plant: 2; now: (2, 0)`), which is what exposed the phantom
    # cycle in the first place.
    #
    # So this asserts the property directly and instantly instead: no work-unit
    # was handed to the drain across the whole arming window. A run that got zero
    # units cannot have closed any obligation, which is exactly what the step-4
    # attribution rests on. (The *serve* count is deliberately not pinned — an
    # unrelated in-flight `config_changed` from fixture setup may legitimately
    # move it, and an empty serve is harmless. `units` is the half that matters.)
    assert rescore_worklist_serves(nest_url)[1] == armed_units, (
        "a drain run was handed work-units while the world was being armed, but "
        "nothing is owed until the mail is delivered — step 4 attributes the "
        f"drain solely to the ingest push, and that only holds if no pre-delivery "
        f"run had work (units {armed_units} → {rescore_worklist_serves(nest_url)[1]})"
    )

    # ── 3. Deliver a cat mail — AFTER the subscription. Its `labeler:<hex>`
    # obligation can only come from the ingest seed (Arm 1 step 1); its drain can
    # only be woken by the ingest push (Arm 1 step 2). The test pokes NO
    # `config_changed` from here on.
    cat_id = _deliver_and_capture(handle, db_path, "feline note", "I love my cat.")

    # ── 4. The obligation drains to scorer_version 1, score 900 — moments after
    # delivery, with no `config_changed` poke. Before Arm 1 this row never even
    # exists (no ingest seed), so the poll times out at None and this fails.
    assert _poll_labeler_version(db_path, recipient.actor_id, cat_id, factor, expected=1) == 1, (
        "the post-subscription mail's labeler obligation was not created + drained "
        f"at ingest without a config_changed poke (MDA log {mail_bridge_mda.log_file})"
    )
    cat_score, _ = _labeler_row(db_path, recipient.actor_id, cat_id, factor)
    assert cat_score == 900, f"the cat labeler must score a cat mail 900 per-mille, got {cat_score}"

    _unsubscribe(nest_url, recipient, labeler_id)
    # Revoke what this test minted — see the sibling test's step 9 for why a live
    # grant left against session-scoped fixtures breaks later absence-of-access
    # tests rather than this one.
    _revoke_grant(nest_url, recipient, grant_id)

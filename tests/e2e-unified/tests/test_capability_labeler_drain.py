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
from helpers.labeler_publish import new_publisher, publish_labeler_version
from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import (
    await_rescore_worklist_serve_after,
    rescore_worklist_serves,
    wait_until,
)

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
    grant licenses no community labeler, so a drain under it runs none;
    ``labeler_id=None`` mints exactly that composed grant.
    Returns the 16-byte grant_id."""
    grant_id = secrets.token_bytes(16)
    params = {
        "owner_actor_id_b64": base64.b64encode(recipient.actor_id).decode(),
        "grant_id_b64": base64.b64encode(grant_id).decode(),
        "holder_pubkey_b64": base64.b64encode(holder_pubkey).decode(),
        "msek_b64": base64.b64encode(recipient.msek).decode(),
        "epoch_start": epoch_start,
        "epoch_end": epoch_end,
    }
    if labeler_id is not None:
        params["labeler_id_b64"] = base64.b64encode(labeler_id).decode()
    grant_blob = run_seal_helper("mint-grant", params)
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


# The cat module re-tuned to 0.5 confidence: the pre-baked hit blob's f64 0.9
# (`cd cc cc cc cc cc ec 3f`) swapped for 0.5 (`00 00 00 00 00 00 e0 3f`), so a
# cat mail scores 500 instead of 900. A *new version* of the labeler that scores
# differently is what lets a witness tell the new module's score from the old.
_CAT_090 = rb"\cd\cc\cc\cc\cc\cc\ec\3f"
_CAT_050 = rb"\00\00\00\00\00\00\e0\3f"


def _cat_wat(*, half_confidence: bool = False) -> bytes:
    wat = _CAT_WAT_PATH.read_bytes()
    if half_confidence:
        assert wat.count(_CAT_090) == 1, "the cat fixture's 0.9 confidence bytes moved"
        wat = wat.replace(_CAT_090, _CAT_050)
    return wat


def _publish_cat_version(run_seal_helper, nest_url, labeler_id, pub_seed, version, wat):
    """Publish ``version`` of the mail-kind cat labeler over ``wat``."""
    publish_labeler_version(
        run_seal_helper, nest_url, labeler_id, pub_seed, wat,
        version=version, content_kind="mail", artifact_kind="wasm",
    )


def _publish_cat_labeler(run_seal_helper, nest_instance):
    """Publish version 1 of a signed `content_kind="mail"` cat-detector labeler
    as a fresh registered User whose keypair is ALSO the labeler's
    `algorithm_id` (signer; public-key-is-identity). Returns (labeler_id_bytes,
    factor, publisher_seed) — the seed is what a later version is signed with."""
    labeler_id, pub_seed = new_publisher(nest_instance)
    _publish_cat_version(run_seal_helper, nest_instance["url"], labeler_id, pub_seed, 1, _cat_wat())
    return labeler_id, "labeler:" + labeler_id.hex(), pub_seed


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
    admin = nest_instance["admin"]
    holder_pubkey = mail_bridge_mda.x25519_pubkey
    now = int(time.time())

    handle.assert_mta_running()

    # ── 1. A publisher publishes a signed mail-kind cat-detector labeler.
    labeler_id, factor, pub_seed = _publish_cat_labeler(run_seal_helper, nest_instance)

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

    # ── 6. Revoke arm: revoke the grant, THEN republish the labeler as version
    # 2 (the publish raises the subscribed factor's registered version — a fresh
    # obligation on the already-scored rows), then wake the drain.
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
    _publish_cat_version(run_seal_helper, nest_url, labeler_id, pub_seed, 2, _cat_wat())
    _poke_config_changed(nest_url, admin)
    await_rescore_worklist_serve_after(
        nest_url, serves, budget_s=RESCORE_WORKLIST_SERVE_S, what="the grant revoke + republish"
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
    labeler_id, factor, _ = _publish_cat_labeler(run_seal_helper, nest_instance)
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


@pytest.mark.feature("community-labelers")
def test_a_new_labeler_version_rescores_the_mail_the_old_one_scored(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    """`community-labelers` outcome 9: when a labeler you subscribe to publishes a
    new version, the mail it already scored is scored again by the new one
    (`content-moderation-and-ranking.md` § Tier-3 → *Re-score obligation*).

    The publisher's own `fauna.labelers.publish` of version 2 is the only thing
    that changes between the two drains — no registry poke — and version 2 is a
    different module (the cat detector at 0.5 confidence instead of 0.9), so the
    mail's score moving 900 → 500 at scorer_version 2 is the NEW module's output,
    not a re-stamp of the old one.
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

    labeler_id, factor, pub_seed = _publish_cat_labeler(run_seal_helper, nest_instance)
    cat_id = _deliver_and_capture(handle, db_path, "feline note", "I love my cat.")
    grant_id = _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600,
        labeler_id=labeler_id,
    )
    assert _subscribe(nest_url, recipient, labeler_id, grant_id) == factor

    # ── Version 1 scores the mail.
    _poke_config_changed(nest_url, admin)
    assert _poll_labeler_version(db_path, recipient.actor_id, cat_id, factor, expected=1) == 1, (
        f"version 1 never scored the cat mail (MDA log {mail_bridge_mda.log_file})"
    )
    assert _labeler_row(db_path, recipient.actor_id, cat_id, factor)[0] == 900

    # ── The publisher releases version 2, a re-tuned module.
    _publish_cat_version(
        run_seal_helper, nest_url, labeler_id, pub_seed, 2, _cat_wat(half_confidence=True)
    )

    # ── The next drain re-scores the already-scored mail with version 2.
    _poke_config_changed(nest_url, admin)
    got = _poll_labeler_version(db_path, recipient.actor_id, cat_id, factor, expected=2)
    assert got == 2, (
        f"the mail version 1 scored was not re-scored after version 2 was published "
        f"(still at scorer_version {got}; MDA log {mail_bridge_mda.log_file})"
    )
    score = _labeler_row(db_path, recipient.actor_id, cat_id, factor)[0]
    assert score == 500, (
        f"the re-score must be version 2's output (500 per-mille), got {score} — "
        f"900 would mean the old module ran again"
    )

    _unsubscribe(nest_url, recipient, labeler_id)
    # Session-scoped holder + owner: never leave a live grant behind (see the
    # first test's step 9).
    _revoke_grant(nest_url, recipient, grant_id)


def _drain_pass_done(nest_url, admin, *, what):
    """Wake the drain, and return once a pass that was HANDED work has finished
    processing it.

    `await_rescore_worklist_serve_after` is the barrier for an owner the nest
    leaves OUT of the worklist. These arms are the other case: the nest hands
    the unit out and the holder is what refuses to run it, so the absence of a
    write-back is evidence only once the holder is done with the batch. A drain
    pass fetches its worklist, processes it and writes back before it ends, and
    passes never overlap — a poke landing mid-pass queues the next one (`runOnce`
    and `start` in `bins/fauna-bridges/internal/mda/rescore_drain.go`). So: wait
    for a serve that handed out work, poke again, and wait for the next serve;
    by then the pass that had the work is over, write-backs included
    (convention 14).
    """
    planted = rescore_worklist_serves(nest_url)
    _poke_config_changed(nest_url, admin)
    wait_until(
        lambda: rescore_worklist_serves(nest_url)[1] > planted[1],
        RESCORE_WORKLIST_SERVE_S,
        diagnose=lambda: (
            f"the nest handed the drain no work after {what} (at plant: "
            f"{planted}; now: {rescore_worklist_serves(nest_url)})"
        ),
    )
    served, _ = rescore_worklist_serves(nest_url)
    _poke_config_changed(nest_url, admin)
    await_rescore_worklist_serve_after(
        nest_url, served, budget_s=RESCORE_WORKLIST_SERVE_S,
        what=f"the pass that had the work after {what}",
    )


def _stored_artifact(db_path, labeler_id):
    """``(metadata_blob, wasm_bytes, wasm_hash, wasm_size)`` of the nest's
    stored copy of ``labeler_id`` — the row `fauna.labelers.inspect` serves."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT metadata_blob, wasm_bytes, wasm_hash, wasm_size FROM labelers "
            "WHERE labeler_id = ?",
            (labeler_id,),
        ).fetchone()
        assert row is not None, "the labeler is not in the registry"
        return row
    finally:
        conn.close()


def _write_stored_artifact(db_path, labeler_id, artifact) -> None:
    """Overwrite the nest's stored copy of ``labeler_id`` with ``artifact`` (a
    `_stored_artifact` tuple) — the nest acting as an adversary to the labeler
    it serves. A dishonest or compromised nest can hand the holder any bytes it
    likes from `fauna.labelers.inspect`; this is exactly that, done to the row
    inspect reads (`get_labeler`)."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "UPDATE labelers SET metadata_blob = ?, wasm_bytes = ?, wasm_hash = ?, "
            "wasm_size = ? WHERE labeler_id = ?",
            (*artifact, labeler_id),
        )
        conn.commit()
    finally:
        conn.close()


@pytest.mark.feature("community-labelers")
def test_the_mail_filtering_grant_lets_no_community_labeler_read_your_mail(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    """`community-labelers` outcome 6: letting your nest read and filter your
    mail does not let any community labeler read it; only that labeler's own
    grant does (§ Tier-3 → the composed mail grant carries no factor and
    licenses no community labeler).

    The owner subscribes to a mail labeler naming the composed "read and filter
    my mail" grant — the most a subscription could lean on it. A drain pass
    served after that grant is live leaves the mail unscored: the nest hands the
    holder no labeler work under it. Minting the labeler's OWN grant then lets
    the very next pass score the mail — so the first absence was the grant's,
    not a drain that never looked.
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

    labeler_id, factor, _ = _publish_cat_labeler(run_seal_helper, nest_instance)
    cat_id = _deliver_and_capture(handle, db_path, "feline note", "I love my cat.")

    composed_id = _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600,
        labeler_id=None,
    )
    own_id = None
    try:
        assert _subscribe(nest_url, recipient, labeler_id, composed_id) == factor

        # ── The composed grant alone: a pass served after it scores nothing.
        serves, _ = rescore_worklist_serves(nest_url)
        _poke_config_changed(nest_url, admin)
        await_rescore_worklist_serve_after(
            nest_url, serves, budget_s=RESCORE_WORKLIST_SERVE_S,
            what="the subscription under the composed mail grant",
        )
        row = _labeler_row(db_path, recipient.actor_id, cat_id, factor)
        assert row is not None and row[1] == 0, (
            f"the composed mail grant let the community labeler score the mail "
            f"(row {row!r}); only the labeler's own grant may"
        )

        # ── The labeler's own grant: the next pass scores it.
        own_id = _mint_grant(
            run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now,
            epoch_end=now + 3600, labeler_id=labeler_id,
        )
        _poke_config_changed(nest_url, admin)
        assert _poll_labeler_version(db_path, recipient.actor_id, cat_id, factor, expected=1) == 1, (
            f"the labeler's own grant did not let it score the mail "
            f"(MDA log {mail_bridge_mda.log_file})"
        )
        assert _labeler_row(db_path, recipient.actor_id, cat_id, factor)[0] == 900
    finally:
        _unsubscribe(nest_url, recipient, labeler_id)
        # Session-scoped holder + owner: never leave a live grant behind (see
        # the first test's step 9).
        _revoke_grant(nest_url, recipient, composed_id)
        if own_id is not None:
            _revoke_grant(nest_url, recipient, own_id)


@pytest.mark.feature("community-labelers")
def test_your_nest_cannot_swap_another_module_in_for_the_labeler_you_subscribed_to(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    """`community-labelers` outcome 7: only the labeler you subscribed to,
    exactly as its publisher signed it, ever runs over your mail; your nest
    cannot swap in another (§ Tier-3 → the holder pins the fetched module's
    signed `algorithm_id` to the factor's id, and re-verifies signature and
    hash before it runs anything).

    The nest is the adversary here, so the test plays it: between subscribe and
    the drain it rewrites its stored copy of the subscribed labeler — first to
    a DIFFERENT labeler, validly signed by that labeler's own publisher (it
    would score the cat mail 500), then to the subscribed labeler's own signed
    metadata over that other module's bytes. After a finished drain pass for
    each, the mail is unscored. Putting the genuine labeler back lets the next
    pass score it 900 — so the holder was running, and refused only the swap.
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

    labeler_id, factor, _ = _publish_cat_labeler(run_seal_helper, nest_instance)
    # Another publisher's labeler: a real, validly signed module that scores a
    # cat mail 500 — what a nest would swap in.
    other_id, other_seed = new_publisher(nest_instance)
    _publish_cat_version(
        run_seal_helper, nest_url, other_id, other_seed, 1, _cat_wat(half_confidence=True)
    )
    genuine = _stored_artifact(db_path, labeler_id)
    other = _stored_artifact(db_path, other_id)

    cat_id = _deliver_and_capture(handle, db_path, "feline note", "I love my cat.")
    grant_id = _mint_grant(
        run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now, epoch_end=now + 3600,
        labeler_id=labeler_id,
    )
    try:
        assert _subscribe(nest_url, recipient, labeler_id, grant_id) == factor

        swaps = {
            "another labeler, as its own publisher signed it": other,
            "the subscribed labeler's signature over another module": (
                genuine[0], other[1], other[2], other[3],
            ),
        }
        for what, artifact in swaps.items():
            _write_stored_artifact(db_path, labeler_id, artifact)
            _drain_pass_done(nest_url, admin, what=f"the nest swapped in {what}")
            row = _labeler_row(db_path, recipient.actor_id, cat_id, factor)
            assert row is not None and row[1] == 0, (
                f"the holder ran {what} over the mail (row {row!r}); only the "
                f"subscribed labeler as its publisher signed it may run "
                f"(MDA log {mail_bridge_mda.log_file})"
            )

        _write_stored_artifact(db_path, labeler_id, genuine)
        _poke_config_changed(nest_url, admin)
        assert _poll_labeler_version(db_path, recipient.actor_id, cat_id, factor, expected=1) == 1, (
            f"the genuine labeler never scored the mail once restored "
            f"(MDA log {mail_bridge_mda.log_file})"
        )
        assert _labeler_row(db_path, recipient.actor_id, cat_id, factor)[0] == 900
    finally:
        _write_stored_artifact(db_path, labeler_id, genuine)
        _unsubscribe(nest_url, recipient, labeler_id)
        _revoke_grant(nest_url, recipient, grant_id)


# A host import the module asks for — a socket send, in the WASI shape. Every
# import is a capability only the host can hand over, and the sandbox hands over
# none, so a module that asks for one cannot run.
_SOCKET_IMPORT = (
    b'(import "wasi_snapshot_preview1" "sock_send" '
    b"(func $sock_send (param i32 i32 i32 i32 i32) (result i32)))"
)


def _networked_cat_wat() -> bytes:
    """The cat module, unchanged but for one import of a socket send."""
    wat = _cat_wat()
    assert wat.count(b"\n(module\n") == 1, "the cat fixture's module header moved"
    return wat.replace(b"\n(module\n", b"\n(module\n  " + _SOCKET_IMPORT + b"\n")


@pytest.mark.feature("community-labelers")
def test_a_subscribed_labeler_that_wants_the_network_never_runs(
    mail_bridge_inbound_to_imap, mail_bridge_mda, nest_instance, run_seal_helper,
):
    """`community-labelers` outcome 16: a subscribed labeler runs sealed off from
    the network, so it cannot send what it reads anywhere (§ Tier-3 — the
    holder's sandbox links no host functions: no WASI, no clock, no I/O).

    Two labelers over the same cat mail, each subscribed under its own grant:
    the plain cat module, and the same module asking for a socket send. One
    finished drain pass scores the plain one 900 and leaves the networked one
    unscored — the holder had the mail open and the grant to label it, and the
    only difference is the network the module asked for and was not given.
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

    plain_id, plain_factor, _ = _publish_cat_labeler(run_seal_helper, nest_instance)
    net_id, net_seed = new_publisher(nest_instance)
    _publish_cat_version(run_seal_helper, nest_url, net_id, net_seed, 1, _networked_cat_wat())
    net_factor = "labeler:" + net_id.hex()

    cat_id = _deliver_and_capture(handle, db_path, "feline note", "I love my cat.")
    grants = []
    subscribed = []
    try:
        for lid in (plain_id, net_id):
            grants.append(_mint_grant(
                run_seal_helper, nest_url, recipient, holder_pubkey, epoch_start=now,
                epoch_end=now + 3600, labeler_id=lid,
            ))
            _subscribe(nest_url, recipient, lid, grants[-1])
            subscribed.append(lid)

        _drain_pass_done(nest_url, admin, what="subscribing to both labelers")

        plain = _labeler_row(db_path, recipient.actor_id, cat_id, plain_factor)
        assert plain == (900, 1), (
            f"the plain cat labeler did not score the mail in the pass (row {plain!r}) — "
            f"without it the networked labeler's absence proves nothing "
            f"(MDA log {mail_bridge_mda.log_file})"
        )
        net = _labeler_row(db_path, recipient.actor_id, cat_id, net_factor)
        assert net is not None and net[1] == 0, (
            f"a labeler that asked for a socket ran over the mail (row {net!r}); the "
            f"sandbox must give it no way to reach the network"
        )
    finally:
        for lid in subscribed:
            _unsubscribe(nest_url, recipient, lid)
        for gid in grants:
            _revoke_grant(nest_url, recipient, gid)

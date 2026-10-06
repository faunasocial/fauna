"""tier_3 BUILD-SUCCESS BAR: the deployment spam-baseline is aggregated OFF-BOX by
the granted holder, over sealed client-contributor models, via a keyless
`content.read{spam-model}` capability — and that keyless grant conveys **no**
standing read of a contributor's mailbox.

GREEN again since 2026-08-03 (the strict-xfail interval was 2026-08-02 → 2026-08-03):
the holder ruling (`mail-spam.md` § Encrypted-mode interaction, 2026-08-03) resolves
the aggregation holder over the content-processor FAMILY — a dedicated
`ContentProcessor`-role holder when enrolled, else the off-box x25519-attested MDA —
so the real MDA this test always ran is the production holder, exactly as below.
The assertions were never weakened; only the marker was deleted.

This is the full-stack proof of piece (c) of the grant-mediated baseline plumbing
(`docs/goal/behavior/mail-spam.md` § Encrypted-mode interaction, ratified
2026-07-13; § Implementation status item 7). Every binary is real: `fauna-nest`
plus the `fauna-mail-bridge` MDA (which runs the spam-baseline drain worker as its
capability holder), real seal/unseal, the real `fauna.capabilities.*` +
`fauna.bridges.{put_spam_model,set_baseline_contribution,publish_spam_baseline}`
WS-RPC, real SQLite. The in-process twin is the nest unit test
`publish_spam_baseline_drains_sealed_contributors_through_holder`
(`bridge_blob_handlers.rs`); this proves the same flow across the deployed binaries
— the nest→holder push, the real Go holder's unseal-merge with its OWN key halves,
and the submit back — which no in-process test can.

Production data flow asserted end-to-end:

  three contributors each seal a per-user spam model + a holder copy
  (`SpamModelCopyBlob`) to the aggregation holder's pubkey, opt into the baseline,
  and mint a KEYLESS `content.read{spam-model}` grant to the holder → an admin
  `publish_spam_baseline` registers a pending run, pushes the holder, and awaits
  its submit (SPAM_BASELINE_HOLDER_WAIT) → the real MDA holder pulls the run's
  grant-gated worklist, unseal-merges the three copies off-box with its own
  service-user key halves, and submits the merged half → the publish folds it and
  replies `published=true, contributors=3, skipped_contributors=0`. Revoke ONE
  grant → republish → the worklist serves only 2, the union count 2 < the k-anon
  floor 3, so the baseline is WITHHELD (`published=false`) and the erosion is
  reported honestly (`skipped_contributors=1`) — never silently.

The whole-point negative (a test, not a comment): with only the LIVE keyless
`content.read{spam-model}` grant standing — the very grant that just contributed
to the baseline — the holder's mail re-score drain leaves a contributor's owed
mail obligation UNTOUCHED. A spam-model grant is not a `content.read{mail}` grant;
it authorizes the off-box baseline merge and nothing else. (The positive — the
same drain DOES advance the row under a real `content.read{mail}` grant — is
`test_capability_rescore_drain.py`; the crypto layer's zero-wrapped-keys proof is
`seal_test.go`'s `TestMintSpamModelGrantIsKeyless`; the authz plane's in-process
proof is the nest unit test `spam_model_grant_conveys_no_mail_worklist`.)

The seal + grant-mint seeding is fixture setup (E2E rule 8 carve-out (b) —
arranging the world a real client/agent would, via the test-only seal-helper
standing in for the user's primary client), driven over the real WS-RPC wire; the
publish + drain under test is the real deployed flow. No client driver participates
(server-side / independent), exactly like `test_capability_rescore_drain.py`.
"""

import secrets
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _provision_baseline_contributor, _withdraw_baseline_contributors
from helpers.budgets import RESCORE_WORKLIST_SERVE_S
from helpers.waiting import await_rescore_worklist_serve_after, rescore_worklist_serves
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

# The k-anon floor (BASELINE_MIN_CONTRIBUTORS, libs/fauna-text-model/src/classifier.rs):
# a baseline derived from fewer opt-in contributors is withheld. Three contributors
# exactly meet it; revoking one drops us below it.
BASELINE_FLOOR = 3


# ── WS-RPC client factories ──────────────────────────────────────────────────


def _contributor_ws(nest_url, contributor):
    """A User-class WS-RPC client authenticated as a contributor — it writes its
    own sealed model + holder copy, opts in, and mints/revokes its own grant."""
    return WsRpcAdminClient(
        nest_url,
        actor_id=contributor.actor_id,
        signing_key=bytes(contributor.recipient["signing_key"]),
    )


def _admin_ws(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


# ── Publish + revoke (real WS-RPC; the flow under test) ───────────────────────


def _publish_baseline(nest_instance) -> dict:
    """Admin `publish_spam_baseline` — a single synchronous call that registers a
    run, pushes the holder, and awaits its submit (no separate poke). Returns the
    reply."""
    with _admin_ws(nest_instance) as ws:
        return ws.call("fauna.bridges.publish_spam_baseline", {})


def _revoke_grant(nest_url, contributor, grant_id: bytes) -> None:
    with _contributor_ws(nest_url, contributor) as ws:
        reply = ws.call("fauna.capabilities.revoke", {"grant_id": grant_id})
    assert reply.get("ok") is True, f"revoke reply not ok: {reply!r}"


# ── Negative-proof helpers (the mail re-score drain must stay dark) ───────────
#
# Mirror the small SQLite/RPC utilities of `test_capability_rescore_drain.py`
# (kept test-file-local there); here they arrange an OWED mail obligation for a
# spam-model contributor so the "row stays owed" assertion is non-vacuous.


def _seed_stale_mail_clamav_score(db_path: str, actor_id: bytes) -> bytes:
    """Insert a stale (`scorer_version`=1) mail `content_scores` clamav row for the
    actor and return its content_id. No FK (content_scores is a flat watermark
    table, migrations.rs), so a direct insert stands in for a delivered message's
    ingest row — enough to make the re-score worklist have SOMETHING to offer a
    holder that could reach this owner's mail. A spam-model-only holder cannot."""
    content_id = secrets.token_bytes(32)
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "INSERT OR REPLACE INTO content_scores "
            "(content_id, content_kind, factor, score, tier, scorer_version, scored_at, actor_id) "
            "VALUES (?1, 'mail', 'clamav', 0, 0, 1, ?2, ?3)",
            (content_id, int(time.time()), actor_id),
        )
        conn.commit()
    finally:
        conn.close()
    return content_id


def _clamav_scorer_version(db_path: str, actor_id: bytes, content_id: bytes):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT scorer_version FROM content_scores "
            "WHERE actor_id = ? AND content_id = ? AND factor = 'clamav'",
            (actor_id, content_id),
        ).fetchone()
        return row[0] if row else None
    finally:
        conn.close()


def _bump_clamav_model_version(db_path: str, version: int) -> None:
    """Raise the clamav model-version registry (a redeploy bump) so the seeded row
    now OWES a re-score. No RPC exposes the registry; the SQLite poke mirrors
    `_bump_model_version` in the re-score-drain test."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "INSERT INTO model_versions (model_kind, version, updated_at) VALUES ('clamav', ?, ?) "
            "ON CONFLICT(model_kind) DO UPDATE SET version = excluded.version, "
            "updated_at = excluded.updated_at",
            (version, int(time.time())),
        )
        conn.commit()
    finally:
        conn.close()


def _poke_config_changed(nest_instance) -> None:
    """Fan a `config_changed` push to every approved bridge, waking the MDA's
    re-score drain (re-applying the permissive spam policy is the cleanest trigger
    — it fans the push without rebinding listeners). Same trigger the re-score-drain
    test uses."""
    with _admin_ws(nest_instance) as ws:
        ws.call(
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


# ── The test ─────────────────────────────────────────────────────────────────


@pytest.mark.feature("spam")
def test_spam_baseline_drains_sealed_contributors_via_keyless_grant(
    mail_bridge_mda, nest_instance, seal_helper_binary, test_user,
):
    holder_x25519_pubkey = mail_bridge_mda.x25519_pubkey
    domain = mail_bridge_mda.domain
    nest_url = nest_instance["url"]
    db_path = nest_instance["db_path"]

    # ── 0. Establish the world: the three below are the ONLY contributors.
    # The baseline is a WHOLE-NEST aggregate, and the nest is session-shared. The
    # GUI spam tests log in as the session-scoped `test_user`: the contribute
    # toggle flips that account's opt-in ON and never back, and the mark-as-spam
    # witness then trains it a sealed model — an opted-in, sealed, granted 4th
    # contributor this test never named, which the holder either merges
    # (`contributors == 4`) or skips (`skipped_contributors == 1`). It bit the
    # first run of the windows spam set, where the toggle and the mark test both
    # run before this one (the toggle alone leaves 3; verified in isolation
    # 2026-09-26). Clearing the opt-in over the API is fixture setup
    # (e2e-conventions.md convention 8's carve-out), the same normalisation
    # `test_mail_spam_report_share_toggle_and_published_list` does for its flag.
    ws_api.baseline_contribution_set(nest_instance["port"], test_user, False)

    # ── 1. Three opted-in, sealed, keyless-grant-bearing contributors — what the
    # contribute-baseline toggle does in each one's app (the shared
    # `_provision_baseline_contributor`), sealed to THIS holder.
    contributors = []
    grant_ids = []
    try:
        for i in (1, 2, 3):
            c, gid = _provision_baseline_contributor(
                nest_instance=nest_instance,
                seal_helper_binary=seal_helper_binary,
                local_part=f"baseline-c{i}",
                token=f"qzspambaseline{i}wx",
                holder_x25519_pubkey=holder_x25519_pubkey,
                domain=domain,
            )
            contributors.append(c)
            grant_ids.append(gid)

        # ── 2. Publish. The admin call drives the whole drain synchronously: push the
        # holder → it pulls the grant-gated worklist, unseal-merges all three copies
        # off-box with its own key halves, and submits → the publish folds the merged
        # half. All three contributors reach the floor, nobody is eroded.
        reply = _publish_baseline(nest_instance)
        assert reply["published"] is True, (
            f"three opt-in sealed contributors meet the k-anon floor; got {reply!r} "
            f"(MDA log {mail_bridge_mda.log_file})"
        )
        assert reply["contributors"] == 3, (
            f"the holder must unseal-merge all three sealed contributors; got {reply!r} "
            f"(MDA log {mail_bridge_mda.log_file})"
        )
        assert reply["skipped_contributors"] == 0, f"nobody eroded this run; got {reply!r}"
        # Each contributor contributed exactly one spam sample (ham_messages=0), so the
        # union baseline carries three — proof the holder really merged three distinct
        # sealed models, not one served thrice.
        assert reply["sample_count"] == 3, (
            f"merged baseline must carry all three contributors' samples; got {reply!r}"
        )

        # ── 3. The whole-point negative: the LIVE keyless spam-model grant conveys NO
        # mail read. Give contributor 1 (its grant still live) an owed mail obligation,
        # wake the real MDA re-score drain, and prove the drain leaves it OWED — the
        # holder that just aggregated the baseline cannot re-score (open) the
        # contributor's mail. Contrast: under a real content.read{mail} grant the same
        # drain DOES advance the row (test_capability_rescore_drain.py).
        owed = _seed_stale_mail_clamav_score(db_path, contributors[0].actor_id)
        _bump_clamav_model_version(db_path, 2)
        assert _clamav_scorer_version(db_path, contributors[0].actor_id, owed) == 1, (
            "seeded mail obligation must start stale (scorer_version 1 < registry 2)"
        )
        # The barrier is the nest's own decision, not elapsed time (convention 14):
        # a spam-model grant is not a content.read{mail} grant, so the mail worklist
        # `rescore_worklist_handler` builds for this owner is empty — and a serve
        # counted after the poke is that decision, already made. Without waiting for
        # one, "the row stayed owed" would be vacuous rather than evidence.
        serves, _ = rescore_worklist_serves(nest_url)
        _poke_config_changed(nest_instance)
        await_rescore_worklist_serve_after(
            nest_url,
            serves,
            budget_s=RESCORE_WORKLIST_SERVE_S,
            what="the seeded mail obligation under a keyless spam-model grant",
        )
        assert _clamav_scorer_version(db_path, contributors[0].actor_id, owed) == 1, (
            "a keyless content.read{spam-model} grant must not let the holder re-score "
            "(open) the contributor's mail — the obligation must stay owed at v1 "
            f"(MDA log {mail_bridge_mda.log_file})"
        )

        # ── 4. Honesty arm: revoke ONE contributor's grant → republish. The holder's
        # worklist now serves only two copies, so the union count 2 < the floor 3: the
        # baseline is WITHHELD, and the eroded contributor is reported honestly rather
        # than silently dropped.
        _revoke_grant(nest_url, contributors[2], grant_ids[2])
        reply2 = _publish_baseline(nest_instance)
        assert reply2["contributors"] == 2, (
            f"revoking one grant drops the holder-merged count to two; got {reply2!r}"
        )
        assert reply2["skipped_contributors"] == 1, (
            f"the revoked-but-still-opted-in contributor must be reported as skipped, "
            f"not silently dropped; got {reply2!r}"
        )
        assert reply2["published"] is False, (
            f"two contributors fall below the k-anon floor {BASELINE_FLOOR}; the baseline "
            f"must be withheld; got {reply2!r}"
        )
        # Withheld ⇒ the stored baseline is reset to empty (no non-anonymous model served).
        assert reply2["sample_count"] == 0, (
            f"a withheld baseline publishes empty, withdrawing any prior one; got {reply2!r}"
        )
    finally:
        # The baseline is a whole-nest aggregate on the session-shared nest: leave
        # no opted-in contributor behind for a later publish to merge.
        _withdraw_baseline_contributors(nest_instance, contributors)

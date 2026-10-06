"""tier_3: the on-device INBOX spam scorer, observed through a Fauna app — a
trained user's inbound spam lands in Junk with **no IMAP MUA**. Runs `web` (the
reference leg, `WasmConversationsManager`) AND `linux` (the shared native leg):
both drive the SAME shared classifier + re-file, so the same seed + assertions
prove both. `web` and every native app (linux/apple/windows) reach the scorer
through one shared orchestrator — web via `WasmConversationsManager`, the natives
via the shared `NestMailInboundSource` (`libs/fauna-client-conversations`), which
holds the `InboxSpamScorer` in the receive loop's per-pass lifecycle
(`begin_pass` fetch-model+policy → `observe` at ingest → `end_pass` flush) — so
this is one code path, not four (priority #2). **apple: the scorer is PROVEN +
gated on macos** (`.macos`) via the apple e2e real-conversations harness
(`applySessionPatch` activates the real `ConversationsSession` under
`FAUNA_E2E_REAL_CONVERSATIONS`). The earlier "receive loop doesn't drain after a
per-module `recover()` relaunch" hypothesis was WRONG: the real defect was
cross-test contamination — this test seeds a balanced, full-confidence
`spam_models` row for the session-shared `test_user` and, before the
`_isolate_spam_model` teardown below deleted it, left it in place, so a *later*
real-receive test's token-neutral inbound scored 5250 ≥ the 5000 `spam_folder`
tier and the on-device scorer re-filed it to Junk (delivered to INBOX at the
nest, never surfacing client-side). The teardown restores cold-start for the next
test. `.ios` is gated too (green in a co-running `--client ios` session — the same
shared FaunaKit + real-conversations path); `.windows` is gated too (green
`--client windows` on Windows, verified 2026-07-02 — the identical shared Rust via
`NestMailInboundSource`); android adopted the same shared `conversations_session`
factory on 2026-07-20 (`ConversationsManagerHost.startConversationsSession`),
so this note (last touched 2026-07-02) is stale — see the marker fix below.

This is the whole-feature Success line for the on-device mail spam scorer
(`mail-spam.md` § Scoring placement — the **Fauna app (post-decrypt)**
position; § Re-file timing — "The Fauna-app score-at-ingest flow"). A pure
Fauna-app user reads mail solely over
`fauna.email.inbox.fetch` WS-RPC and never opens IMAP, so the MDA's
`SELECT INBOX`-time scoring (`spam_score.go`, proven by
`test_mail_bridge_mda.py::test_mda_imap_spam_scoring_*`) never fires for them —
their INBOX would never be per-user-scored. The client closes that gap: the web
receive poll (`apps/fauna-web/src/lib/conversations.ts::pollOnce`) fetches the
caller's sealed model (`fauna.bridges.fetch_spam_model`) + the admin-effective
policy (`fauna.bridges.get_spam_scoring_policy`), scores each just-decrypted INBOX
message at ingest against the SAME shared classifier + threshold the MDA/nest use,
and issues one `fauna.email.apply_spam_disposition` per pass (watermark all scored,
move the spam subset INBOX→Junk).

Every binary is real, the wire is real, the MTA seal / inbox-fetch / open / score /
re-file all run for real (`green-test-or-it-doesnt-work`). The model's *contents*
are the only fixture (`_seed_spam_model`, exactly as the MDA scoring tests seed
alice) — the seal→open→shared-scorer→re-file path itself is fully exercised.

Assertion — a trained model maps `spam_token` strongly to spam and `ham_token`
strongly to ham (balanced priors, full confidence, the calibration
`test_mda_imap_spam_scoring_refiles_trained_spam_to_junk` relies on): the spam
message scores well past the default `spam_folder` = 5 (→ 5000 milli) tier and the
ham well below. So after the receive poll:
  - the HAM message renders in the client conversations list (decrypted, proving the
    receive path completed), and
  - the SPAM message is ABSENT from that list — the on-device scorer kept it out
    of the inbox view (the client-side twin of the MDA's move-before-`SELECT`
    snapshot), and the nest re-filed it INBOX→Junk (a delta on the actor's
    Junk-watermarked count confirms the nest half, so a red pinpoints web-view
    vs. nest-side).

The rigorous per-message INBOX→Junk + `$FaunaSpamScored` DB assertion lives in the
API-helper `tests/api/test_mail_apply_spam_disposition.py` (a fresh actor + a
single message → exact match), so a failure here is triaged UI-side vs nest-side.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP wire, real
  MTA-seal → client-fetch → client-open → score → apply_spam_disposition.

Marked `web` + `linux` + `macos` + `ios` + `windows` + `android`: the reference leg
(`WasmConversationsManager`) and the shared native leg (`NestMailInboundSource`,
covering apple/windows/android) are proven on
the shared Linux dev host (web/linux); apple's real-conversations e2e harness drives the scorer green
`--client macos`/`--client ios` in a co-running session now that the
`_isolate_spam_model` teardown stops the seeded model poisoning later real-receive
tests; and Windows proves `--client windows` (verified 2026-07-02, the identical
shared Rust). android adopted the shared `conversations_session` factory
2026-07-20 (`ConversationsManagerHost.startConversationsSession`, row 74 pass
9/10's own finding) — its own `--client android` run is emulator-host-gated like
every android e2e track , so the marker here is a
coverage-contract claim, not a device-run claim.
"""

import sqlite3
import time

import pytest

from helpers.mail_wire import _connect_smtp_starttls
from helpers.mail_aliases import add_exact_alias

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

# The (external) sender's domain — no local-domain / loopback exemption, so the
# message arrives as genuine external inbound (mirrors test_mail_client_receive).
SENDER_DOMAIN = "external.test"

# Distinct single-unigram tokens (lowercase ASCII words the shared tokenizer maps
# to a unigram of themselves) so the seeded model's score is dominated by the one
# distinctive token + the prior. Separate strings from the MDA fixture's tokens
# (a different actor + model), but the SAME (110, 0) / (0, 110) balanced-prior
# calibration, so the spam scores ~10k milli (>> 5000) and the ham ~90 (<< 5000).
SPAM_TOKEN = "webspamtokenqz"
HAM_TOKEN = "webhamtokenqz"


@pytest.fixture(autouse=True)
def _isolate_spam_model(nest_instance, test_user):
    """Delete the session-shared actor's per-user spam model after this test, so a
    later real-receive test (e.g. ``test_mail_client_receive``) that shares the
    session-scoped ``nest_instance`` + ``test_user`` isn't scored against the
    balanced, full-confidence model this test seeds.

    Without this, the seeded ``(110, 110)`` model — ``sample_count`` 220, past the
    200-sample full-confidence ramp, so weight 0.7 — scores a *token-neutral*
    inbound body at the neutral 7500 → 5250 milli, which clears the default
    ``spam_folder = 5`` (5000 milli) tier, so the on-device ``InboxSpamScorer``
    re-files the next test's plain message to Junk and it never surfaces in the
    INBOX conversations view (delivered to INBOX at the nest, moved to Junk
    client-side). The model lives in the nest ``spam_models`` table, not
    the client's MLS store, so a client relaunch never clears it — only this teardown
    does. Runs on every app (the contamination is nest-side, platform-agnostic;
    it only surfaced when apple ran the three mail modules in one ``--client macos``
    invocation).
    """
    yield
    conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
    try:
        conn.execute(
            "DELETE FROM spam_models WHERE actor_id = ?1",
            (test_user["actor_id_bytes"],),
        )
        conn.commit()
    finally:
        conn.close()


def _deliver_inbound(mx_port: int, server_name: str, recipient_addr: str,
                     raw_message: bytes, deadline: float) -> None:
    """Drive one real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's
    port-25 STARTTLS listener. Returns after the `250` on `.`, which the MTA sends
    only once the WS-RPC `ingest_inbound_mail` (seal + store) committed."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _message(sender_name: str, recipient_addr: str, subject: str, message_id: str,
             body: str) -> bytes:
    lines = [
        f"From: {sender_name} <sender@{SENDER_DOMAIN}>",
        f"To: {recipient_addr}",
        f"Subject: {subject}",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        body,
    ]
    return ("\r\n".join(lines) + "\r\n").encode()


def _junk_watermarked_count(db_path: str, actor_id: bytes) -> int:
    """Rows this actor carries in Junk bearing the `$FaunaSpamScored` watermark —
    the nest-side signal that the on-device scorer's disposition re-filed spam.
    A DELTA (before vs after) is asserted, so a session-shared INBOX that already
    holds unrelated Junk mail doesn't perturb the result."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        (n,) = conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages "
            "WHERE actor_id = ?1 AND mailbox = 'Junk' AND flags LIKE '%$FaunaSpamScored%'",
            (actor_id,),
        ).fetchone()
        return int(n)
    finally:
        conn.close()


@pytest.mark.feature("spam", "email-in-conversations")
def test_client_scores_inbound_spam_to_junk(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, seal_helper_binary
):
    from conftest import _seed_spam_model

    app = logged_in_app
    actor_id = test_user["actor_id_bytes"]
    db_path = nest_instance["db_path"]

    # ── 1. Enable mail on the logged-in user (mints the MSEK, registers the
    # MSEK-derived recipient pubkey so the MTA can seal inbound to a key only this
    # client can open) and register the inbound routing alias for *this* actor.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    domain = mail_bridge_mta.domain
    local_part = "webspamuser"
    recipient_addr = f"{local_part}@{domain}"
    add_exact_alias(nest_instance["url"], test_user["signing_key"], domain, local_part)

    # ── 2. TRAIN the user's per-user model (seed straight into `spam_models`,
    # sealed to the actor's own recipient key — the key step 1's enable put on
    # file — exactly as the MDA scoring tests seed alice). `fetch_spam_model`
    # returns the stored sealed blob verbatim and the client unwraps + scores
    # against it. Seed BEFORE delivering so the very first receive poll that drains
    # these messages already fetched the trained model (an untrained fetch → the
    # message would be drained un-scored and then deduped, never re-scored).
    _seed_spam_model(
        db_path=db_path,
        actor_id=actor_id,
        seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
        spam_messages=110,
        ham_messages=110,
    )
    junk_watermarked_before = _junk_watermarked_count(db_path, actor_id)

    # ── 3. Deliver one spam + one ham message through the real MTA. Unique subject
    # nonces so the web-view assertion is unambiguous across a session-shared inbox.
    nonce = f"webspam{int(time.time() * 1000)}qx"
    spam_subject = f"Trained spam {nonce}"
    ham_subject = f"Routine ham {nonce}"
    spam_msg = _message(
        "External Spammer", recipient_addr, spam_subject,
        f"<spam-{nonce}@{SENDER_DOMAIN}>",
        f"Act now: the {SPAM_TOKEN} offer expires today.",
    )
    ham_msg = _message(
        "A Colleague", recipient_addr, ham_subject,
        f"<ham-{nonce}@{SENDER_DOMAIN}>",
        f"Thanks for the {HAM_TOKEN} notes from the meeting.",
    )
    deliver_deadline = time.monotonic() + 40.0
    _deliver_inbound(mail_bridge_mta.mx_port, domain, recipient_addr, spam_msg,
                     deliver_deadline)
    _deliver_inbound(mail_bridge_mta.mx_port, domain, recipient_addr, ham_msg,
                     deliver_deadline)

    # ── 4. The client receive poll runs on its own timer (E2E cadence) — web's
    # `pollOnce`, or the native receive loop's `poll_inbound_mail` per-pass
    # lifecycle: it fetches the model + policy, scores each drained INBOX message at
    # ingest, then flushes one `apply_spam_disposition`. Poll `list_threads` until
    # the HAM surfaces
    # decrypted — its plaintext subject carries the nonce, so its appearance proves
    # the receive path (inbox.fetch → open → score → ingest) completed a pass.
    def _threads_with_nonce():
        out = []
        for t in app.conversations.list_threads():
            hay = f"{t.label or ''} {t.snippet or ''}"
            if nonce in hay:
                out.append(t)
        return out

    def _has_subject(subject: str) -> bool:
        return any(subject in (t.label or "") for t in app.conversations.list_threads())

    deadline = time.monotonic() + 60.0
    while time.monotonic() < deadline and not _has_subject(ham_subject):
        time.sleep(1.0)

    threads_dump = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    assert _has_subject(ham_subject), (
        f"the ham message {ham_subject!r} never surfaced decrypted in the client "
        f"conversations list within 60s — the client receive path did not complete.\n"
        f"  threads: {threads_dump}\n"
        f"  conversations error: {app.error_text()!r}\n"
        f"  bridge log: {mail_bridge_mta.log_file}"
    )

    # ── 5. Nest-side confirmation (self-diagnosis): the scorer's disposition
    # re-filed the spam INBOX→Junk with the watermark. The move runs in
    # `flushSpamScoring` AFTER the INBOX drain, so poll the DB delta for a moment
    # after the ham appeared. A red here means the nest half didn't move it (the
    # apply_spam_disposition wire / handler); a green here with a failing step 6
    # means the client view didn't suppress the moved spam.
    db_deadline = time.monotonic() + 20.0
    while (
        time.monotonic() < db_deadline
        and _junk_watermarked_count(db_path, actor_id) <= junk_watermarked_before
    ):
        time.sleep(0.5)
    assert _junk_watermarked_count(db_path, actor_id) > junk_watermarked_before, (
        "the on-device scorer's spam disposition never reached the nest — no new "
        "$FaunaSpamScored row appeared in the actor's Junk mailbox "
        f"(before={junk_watermarked_before}). apply_spam_disposition (handler / "
        f"wire) did not re-file the spam. conversations error: {app.error_text()!r}"
    )

    # ── 6. THE PRIMARY, USER-FACING PROOF: the spam is ABSENT from the web
    # conversations list. The on-device scorer classified it spam and kept it out
    # of the inbox view (never ingested it into the thread store — the client-side
    # twin of the MDA moving spam out of INBOX before the SELECT snapshot), while
    # the ham renders. Without the on-device suppression the spam would still be
    # showing here even though the nest moved it (step 5 green) — exactly the gap
    # this leg closes.
    matched = _threads_with_nonce()
    labels = [t.label for t in matched]
    assert not _has_subject(spam_subject), (
        f"on-device-detected spam {spam_subject!r} must NOT appear in the client "
        f"conversations list — the client scorer moved it to Junk (step 5 confirmed "
        f"the nest re-file), so it must be suppressed from the inbox view.\n"
        f"  nonce-matching threads: {labels}\n"
        f"  conversations error: {app.error_text()!r}"
    )
    # And the ham is still there (the positive control — the scorer didn't over-move).
    assert _has_subject(ham_subject), (
        f"the ham message {ham_subject!r} must remain in the inbox view (scored "
        f"below the spam_folder threshold); nonce threads: {labels}"
    )

"""tier_3 — the delivery-time spam-threshold fold, end to end over real binaries
(`mail-aliases.md` § Spam-threshold override, ruled 2026-08-17 / built 2026-08-18).

**What this covers that nothing else does: the COMPOSITION.** Each link of the
chain is already pinned in isolation — nest folds the three tiers and stamps
(`bridge_routing_handlers.rs` resolver tests), the MTA seals the *stamped* buffer
(`resolve_cutover_test.go::TestStampedHeaderValueEntersTheSealedLocalCopy`), and
the MDA reads a stamp back and honours it
(`spam_score_test.go::TestScoreSelectedInboxPerMessageStampOutranksTheSessionPolicy`).
None of those prove the links join up: the stamp nest emits, the header the MTA
prepends, and the field name the MDA looks for are three separate spellings of
one contract, and a divergence between any two of them leaves every unit test
green while the user's threshold silently does nothing.

**Why the per-ACCOUNT tier with an override of `0`, and not a per-alias number.**
A numeric override would have to land between the message's Bayesian score and
the deployment default to be observable, and the score is a model-dependent value
this test would then be pinned to. `0` is the disabled tier
(`mail-spam.md` § Pipeline step 4) and needs no such band: the SAME message, under
the SAME model, must be re-filed to Junk without the override and stay in INBOX
with it. The verdict flips on one RPC and nothing else.

**The barrier (convention 14 — no settle-sleeps).** The negative half ("message A
must NOT be Junked") is anchored causally, not on a clock: after A is delivered
under the override, the override is CLEARED and a second spam B is delivered.
`scoreSelectedInbox` scores every un-watermarked INBOX message in one pass (both
are far under `spamScorePerPassCap`), so **B arriving in Junk proves the pass ran
over A as well**. Only then is A's placement asserted. No sleep decides anything.

**Red-verified 2026-08-18, not merely green.** With the MDA's stamp read disabled
(`spam_score.go`'s `if stamped := mailfauna.ReadSpamThresholdStamp(pt)` forced
false) this test fails on exactly the assertion it exists for — the override'd
message is Junked — while every other assertion, the control included, still
passes. So the green run measures the composition and not the fixture.

Fixture preconditions (a real message driven through real inbound SMTP + seal +
IMAP is the mutation under test; the model is a precondition, per the E2E rules'
carve-out (b)): the seeded per-user model and the MSEK recipient come from the
shared `mail_bridge_inbound_to_imap` fixture, exactly as
`test_mail_scored_before_visible.py` uses them.

**The `Allow` filter rule rides the same stamp** (`email-filters.md`
§ Multi-action composition): a fired Allow makes the MTA replace the stamp with
`0`, so the last test here witnesses mail-filter-rules outcome 15 over the same
seeded-model barrier.
"""

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _imap_auth_plain,
    _imap_cmd,
    _imap_uid_search,
    _imaps_connect,
)

pytestmark = pytest.mark.tier_3

# Same token shape as the scored-before-visible test: the seeded model puts a
# message carrying SPAM_TOKEN far past the deployment `spam_folder` default.
SPAM_TOKEN = "stospamtokenqz"
HAM_TOKEN = "stohamtokenqz"


def _seed_recipient_model(nest_instance, recipient, seal_helper_binary) -> None:
    from conftest import _seed_spam_model

    _seed_spam_model(
        db_path=nest_instance["db_path"],
        actor_id=recipient.actor_id,
        seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
        spam_messages=110,
        ham_messages=110,
    )


def _set_account_threshold(nest_instance, recipient, value) -> None:
    """Drive the real User-class door (`fauna.bridges.set_spam_threshold_override`),
    not a SQLite poke — the per-account tier's write path IS part of what this
    test covers. `None` clears the override (follow the admin default again)."""
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=recipient.actor_id,
        signing_key=bytes(recipient.recipient["signing_key"]),
    )
    with client:
        client.call(
            "fauna.bridges.set_spam_threshold_override",
            {"spam_threshold_override": value},
        )


def _spam_message(recipient_username: str, nonce: str) -> bytes:
    return (
        "\r\n".join(
            [
                "From: External Spammer <sender@external.test>",
                f"To: {recipient_username}",
                f"Subject: threshold override {nonce}",
                f"Message-ID: <{nonce}@external.test>",
                "Date: Mon, 06 Jul 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"Act now: the {SPAM_TOKEN} offer expires today ({nonce}).",
            ]
        )
        + "\r\n"
    ).encode()


def _deliver_inbound(handle, raw_message: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{handle.recipient_username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mail-aliases")
def test_per_account_spam_threshold_override_rides_the_message_to_the_scorer(
    mail_bridge_inbound_to_imap, nest_instance, seal_helper_binary,
):
    """A per-account `spam_threshold_override` of 0 keeps a message the
    deployment default WOULD file to Junk in the user's INBOX — because nest
    folded the tiers at RCPT and stamped the result onto that message, and the
    MDA consulted the stamp instead of its own session policy.

    The control is the same message under a cleared override: it must be Junked,
    which is what makes the first half meaningful rather than vacuous.
    """
    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    deadline = time.monotonic() + 180.0
    handle.assert_mta_running()
    _seed_recipient_model(nest_instance, recipient, seal_helper_binary)

    stamp = int(time.time() * 1000)
    nonce_kept = f"stokept{stamp}qx"
    nonce_junked = f"stojunked{stamp}qx"

    # A: delivered while the account override says "auto-Junk off for me".
    # The stamp is frozen at delivery, so clearing the override afterwards must
    # NOT change A's fate — which is exactly what the barrier below relies on.
    _set_account_threshold(nest_instance, recipient, 0)
    _deliver_inbound(handle, _spam_message(handle.recipient_username, nonce_kept), deadline)

    # B: the control, delivered after the override is cleared. Same sender, same
    # model, same spam token — the ONLY difference is the tier that was in force
    # at its RCPT.
    _set_account_threshold(nest_instance, recipient, None)
    _deliver_inbound(handle, _spam_message(handle.recipient_username, nonce_junked), deadline)

    sock, buf = _imaps_connect(handle.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "t1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the recipient"

        # The causal barrier: poll the client's ordinary re-SELECT until the
        # CONTROL lands in Junk. One scoring pass covers every un-watermarked
        # INBOX message, so this also establishes that A has been scored.
        junk_control = []
        inbox_kept = []
        poll_deadline = time.monotonic() + 60.0
        while time.monotonic() < poll_deadline and not junk_control:
            st, _ = _imap_cmd(sock, buf, "t2", "SELECT INBOX", deadline)
            assert st == "OK", f"SELECT INBOX must succeed; got {st}"
            _, inbox_kept = _imap_uid_search(sock, buf, "t3", f'BODY "{nonce_kept}"', deadline)
            st, _ = _imap_cmd(sock, buf, "t4", "SELECT Junk", deadline)
            assert st == "OK", f"SELECT Junk must succeed; got {st}"
            _, junk_control = _imap_uid_search(
                sock, buf, "t5", f'BODY "{nonce_junked}"', deadline
            )
            if not junk_control:
                time.sleep(1.0)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass

        assert junk_control, (
            f"the CONTROL spam ({nonce_junked}) must reach Junk under the deployment "
            f"default — without this the test cannot distinguish a working override "
            f"from a scorer that never ran (MDA log {handle.mda.log_file})"
        )

        # The pass that Junked B also scored A. Re-read both mailboxes now that
        # the barrier has fired.
        st, _ = _imap_cmd(sock, buf, "t6", "SELECT Junk", deadline)
        assert st == "OK", f"SELECT Junk must succeed; got {st}"
        _, junk_kept = _imap_uid_search(sock, buf, "t7", f'BODY "{nonce_kept}"', deadline)
        st, _ = _imap_cmd(sock, buf, "t8", "SELECT INBOX", deadline)
        assert st == "OK", f"SELECT INBOX must succeed; got {st}"
        _, inbox_kept = _imap_uid_search(sock, buf, "t9", f'BODY "{nonce_kept}"', deadline)

        assert not junk_kept, (
            f"the override'd message ({nonce_kept}) must NOT be in Junk: it was "
            f"delivered while the account's spam_threshold_override was 0, so the "
            f"stamp nest folded onto it disables auto-Junk for THAT message "
            f"(MDA log {handle.mda.log_file})"
        )
        assert inbox_kept, (
            f"the override'd message ({nonce_kept}) must still be in INBOX — it is "
            f"neither Junked nor lost"
        )
        sock.sendall(b"t99 LOGOUT\r\n")


def _alias_ws(nest_instance, recipient):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=recipient.actor_id,
        signing_key=bytes(recipient.recipient["signing_key"]),
    )


def _set_alias_threshold(ws, alias_id, pattern, value) -> None:
    controls = {"label": ""}
    if value is not None:
        controls["spam_threshold_override"] = value
    ws.call("fauna.bridges.update_account_alias",
            {"alias_id": alias_id, "pattern": pattern, "controls": controls})


def _deliver_to(handle, rcpt: str, raw_message: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mail-aliases")
def test_changing_an_address_threshold_never_refiles_delivered_mail(
    mail_bridge_inbound_to_imap, nest_instance, seal_helper_binary,
):
    """An address's spam threshold is applied to each message as it arrives, and
    changing it later moves nothing already delivered, in either direction: mail
    that arrived with auto-Junk off for the address stays in the inbox after the
    threshold is restored, and mail the default filed to Junk stays in Junk after
    auto-Junk is switched off.

    Barrier as in the per-account test above: the control (delivered under the
    default) reaching Junk proves the scoring pass ran over the message delivered
    under the override."""
    import secrets

    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    deadline = time.monotonic() + 180.0
    handle.assert_mta_running()
    _seed_recipient_model(nest_instance, recipient, seal_helper_binary)

    pattern = f"sto{secrets.token_hex(3)}"
    alias_addr = f"{pattern}@{handle.domain}"
    stamp = int(time.time() * 1000)
    nonce_kept = f"stoakept{stamp}qx"
    nonce_junked = f"stoajunked{stamp}qx"

    with _alias_ws(nest_instance, recipient) as ws:
        alias_id = ws.call("fauna.bridges.create_account_alias", {
            "kind": "exact", "local_domain": handle.domain, "pattern": pattern,
            "controls": {"label": "", "spam_threshold_override": 0},
        })["alias_id"]
        _deliver_to(handle, alias_addr, _spam_message(alias_addr, nonce_kept), deadline)
        _set_alias_threshold(ws, alias_id, pattern, None)
        _deliver_to(handle, alias_addr, _spam_message(alias_addr, nonce_junked), deadline)

    sock, buf = _imaps_connect(handle.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the recipient"

        def where(nonce, n):
            st, _ = _imap_cmd(sock, buf, f"a{n}i", "SELECT INBOX", deadline)
            assert st == "OK", f"SELECT INBOX must succeed; got {st}"
            _, inbox = _imap_uid_search(sock, buf, f"a{n}s", f'BODY "{nonce}"', deadline)
            st, _ = _imap_cmd(sock, buf, f"a{n}j", "SELECT Junk", deadline)
            assert st == "OK", f"SELECT Junk must succeed; got {st}"
            _, junk = _imap_uid_search(sock, buf, f"a{n}t", f'BODY "{nonce}"', deadline)
            return bool(inbox), bool(junk)

        control = (True, False)
        poll_deadline = time.monotonic() + 60.0
        n = 0
        while time.monotonic() < poll_deadline and control != (False, True):
            n += 1
            control = where(nonce_junked, n)
            if control != (False, True):
                time.sleep(1.0)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass
        assert control == (False, True), (
            f"the control ({nonce_junked}) must reach Junk under the default — the barrier "
            f"(MDA log {handle.mda.log_file})")
        assert where(nonce_kept, 90) == (True, False), (
            "mail delivered while the address had auto-Junk off must stay in the inbox "
            "after the threshold is restored")

        # The other direction: switching auto-Junk off now leaves the Junked
        # control where it is, however many passes run.
        with _alias_ws(nest_instance, recipient) as ws:
            _set_alias_threshold(ws, alias_id, pattern, 0)
        assert where(nonce_junked, 91) == (False, True), (
            "mail the default already filed to Junk must stay in Junk after the "
            "address's auto-Junk is switched off")
        sock.sendall(b"a99 LOGOUT\r\n")


def _spam_message_from(sender: str, recipient_username: str, nonce: str,
                       extra_headers=()) -> bytes:
    return (
        "\r\n".join(
            [
                f"From: Sender <{sender}>",
                f"To: {recipient_username}",
                f"Subject: allow past delivery {nonce}",
                f"Message-ID: <{nonce}@allow.test>",
                "Date: Mon, 06 Jul 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                *extra_headers,
                "",
                f"Act now: the {SPAM_TOKEN} offer expires today ({nonce}).",
            ]
        )
        + "\r\n"
    ).encode()


def _deliver_from(handle, sender: str, raw_message: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{handle.recipient_username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mail-filter-rules")
def test_allow_rule_keeps_a_senders_mail_in_the_inbox_past_every_scoring_pass(
    mail_bridge_inbound_to_imap, nest_instance, seal_helper_binary,
):
    """A rule that allows a sender keeps that sender's mail in the inbox even
    though it would otherwise be filed as spam — both when it arrives (the
    deployment's shared rules score it 20, past the spam-folder tier) and on
    every later pass of the user's own trained filter, which scores it as spam
    too. The same spam from a sender no rule allows is filed to Junk.

    The MTA carries the fired Allow with the message by replacing the
    recipient's `X-Fauna-Spam-Threshold` stamp with the disabled tier `0`, which
    both post-delivery scorers read. Barrier as in the per-account test above:
    the control reaching Junk proves the scoring pass ran over the allowed
    message too (convention 14 — no settle-sleep decides anything)."""
    import secrets

    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    deadline = time.monotonic() + 180.0
    handle.assert_mta_running()
    _seed_recipient_model(nest_instance, recipient, seal_helper_binary)

    allowed_domain = f"allow{secrets.token_hex(3)}.test"
    allowed_sender = f"friend@{allowed_domain}"
    other_sender = "sender@external.test"
    stamp = int(time.time() * 1000)
    nonce_kept = f"stoallowkept{stamp}qx"
    nonce_junked = f"stoallowjunked{stamp}qx"

    with _alias_ws(nest_instance, recipient) as ws:
        ws.call("fauna.email.filters.create", {
            "name": "always allow my friend",
            "rules": [{"SenderDomain": {"domain": allowed_domain}}],
            "combination": "all",
            "action": "Allow",
            "priority": 0,
        })

    # A: from the allowed sender, scored spam by BOTH the shared rules (at
    # delivery) and the user's own model (after delivery).
    _deliver_from(handle, allowed_sender, _spam_message_from(
        allowed_sender, handle.recipient_username, nonce_kept,
        extra_headers=["X-Test-Rspamd-Score: 20"]), deadline)
    # B: the control — the same model-spam content from a sender no rule
    # allows. No shared-rules score, so it reaches INBOX at delivery and only
    # the post-delivery pass files it: its arrival in Junk is the barrier.
    _deliver_from(handle, other_sender, _spam_message_from(
        other_sender, handle.recipient_username, nonce_junked), deadline)

    sock, buf = _imaps_connect(handle.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "w1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the recipient"

        def where(nonce, n):
            st, _ = _imap_cmd(sock, buf, f"w{n}i", "SELECT INBOX", deadline)
            assert st == "OK", f"SELECT INBOX must succeed; got {st}"
            _, inbox = _imap_uid_search(sock, buf, f"w{n}s", f'BODY "{nonce}"', deadline)
            st, _ = _imap_cmd(sock, buf, f"w{n}j", "SELECT Junk", deadline)
            assert st == "OK", f"SELECT Junk must succeed; got {st}"
            _, junk = _imap_uid_search(sock, buf, f"w{n}t", f'BODY "{nonce}"', deadline)
            return bool(inbox), bool(junk)

        control = (True, False)
        poll_deadline = time.monotonic() + 60.0
        n = 0
        while time.monotonic() < poll_deadline and control != (False, True):
            n += 1
            control = where(nonce_junked, n)
            if control != (False, True):
                time.sleep(1.0)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass
        assert control == (False, True), (
            f"the control ({nonce_junked}) from a sender no rule allows must reach Junk "
            f"— the barrier (MDA log {handle.mda.log_file})")
        assert where(nonce_kept, 90) == (True, False), (
            f"mail from the allowed sender ({nonce_kept}) must stay in the inbox through "
            f"delivery and the post-delivery scoring pass ({handle.bridge_log_hint()}, "
            f"MDA log {handle.mda.log_file})")
        sock.sendall(b"w99 LOGOUT\r\n")

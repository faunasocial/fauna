"""End-to-end proof of the full inbound-SMTP → IMAP-read mail seam.

The two halves of receive→read are each proven, but no single green test drove
real inbound SMTP DATA all the way to a decrypted IMAP FETCH through *both* Go
bridge binaries:

  - `bins/fauna-nest/tests/mail_inbound_seal_unseal_round_trip.rs` proves the
    nest-handler path *in-process* (it calls `ingest_inbound_mail` /
    `fetch_message_ciphertext` directly and seals via `seal_to_recipient`
    directly) — never the MTA SMTP listener (so no `Received:`-header prepend)
    nor the MDA IMAP(993) listener.
  - `test_mail_bridge_mta.py::test_inbound_mx_round_trip` drives real SMTP DATA
    but asserts only the `250` on `.` — the precise `green-test-or-it-doesnt-work`
    gap (a 250-on-ingest does NOT prove the read-back half).
  - `test_mail_bridge_mda.py` proves IMAP FETCH→decrypt but injects mail via
    **APPEND** (client-side), not real inbound SMTP through the MTA.

This is the first test to prove the **MTA-sealed** envelope opens via the MDA's
`OpenMailRecord` read path — the exact class of bug only tier_3 catches
(MTA-side `EncryptToRecipient` producing a payload type the
MDA's per-message open rejects; memory `mda-openmailrecord-not-decrypt`) — and
that the `Received:` trace header the MTA prepends *before* sealing
(`server.go:1237-1252`, before the seal at `:1530`) survives the seal/open
round-trip.

Production data flow asserted end-to-end on real binaries:

  external MX → MTA port-25 STARTTLS → `Rcpt` → `validate_recipient` resolves the
  actor → `Data` builds + prepends `Received:` → `EncryptToRecipient(raw, mlsPubkey)`
  HPKE-seals the trace-headed RFC 5322 to the recipient's MSEK-derived pubkey →
  `ingest_inbound_mail` stores it → IMAP(993) AUTH PLAIN (AEAD-unwrap-as-auth) →
  SELECT INBOX (`1 EXISTS`) → `UID FETCH BODY[]` → `fetch_message_ciphertext` →
  `MlsCapability.OpenMailRecord(envelope, snapshot)` with the SAME MSEK's leaf
  secret → byte-faithful body, `Received:` header and all.

Partition with the deploy-verify suite: that suite owns the docker/compose
full-stack round-trip (`tests/platform/docker/test_smtp_inbound_to_imap.py`,
~9 min, clamd/rspamd sidecars). This is the fast (seconds), CI-friendly
nest+bridge integration layer over the same seam — defense-in-depth + fast
feedback. The `mail_bridge_inbound_to_imap` fixture spins one MTA bridge + reuses
one MDA bridge on a shared domain serving a fresh MSEK recipient (clean INBOX).

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP + IMAP wire end-to-end,
  real seal/AUTH/decrypt.
- `independent`: server-side; no per-app driver participates.
"""

import time

import pytest

from helpers.mail_wire import (
    _connect_smtp_starttls,
    _imap_auth_plain,
    _imap_cmd,
    _imap_seq_fetch_body,
    _imap_uid_search,
    _imaps_connect,
)

pytestmark = pytest.mark.tier_3


def _select_inbox_count(sock, buf, tag: str, deadline: float) -> int:
    """SELECT INBOX and return the `* <n> EXISTS` count (asserting SELECT OK)."""
    status, resp = _imap_cmd(sock, buf, tag, "SELECT INBOX", deadline)
    assert status == "OK", f"SELECT INBOX must succeed; got {status}: {resp!r}"
    for line in resp.split("\n"):
        parts = line.split()
        if len(parts) >= 3 and parts[0] == "*" and parts[2].upper() == "EXISTS":
            return int(parts[1])
    raise AssertionError(f"SELECT INBOX reported no EXISTS line; got {resp!r}")


def _deliver_inbound(handle, raw_message: bytes, deadline: float) -> None:
    """Drive one real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's
    port-25 STARTTLS listener to the fixture's recipient. Returns after the `250`
    on `.`, which the MTA sends only once the WS-RPC `ingest_inbound_mail` (seal +
    index-hint seal + store) committed in nest."""
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{handle.recipient_username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("standard-mail-apps")
def test_inbound_smtp_delivers_and_imap_fetch_decrypts(mail_bridge_inbound_to_imap):
    """Inbound SMTP DATA → IMAP FETCH decrypts byte-faithfully, Received header and all.

    Drives a single real SMTP transaction through the MTA's port-25 STARTTLS
    listener to a fresh MSEK recipient, then reads it back through the MDA's
    IMAPS(993) listener and HPKE-opens it — proving the MTA-sealed envelope
    round-trips through `OpenMailRecord`, and that the MTA-prepended `Received:`
    trace header is inside the decrypted body. The fresh recipient's INBOX starts
    empty, so the delivered message is the deterministic single `EXISTS`.
    """
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()

    recipient_addr = handle.recipient_username
    # Unique markers so the assertions are unambiguous even if this fixture is
    # ever shared: a nonce body token + a unique Message-ID.
    nonce = f"inbound2imap{int(time.time() * 1000)}qx"
    message_id = f"<{nonce}@external.test>"
    body_lines = [
        "From: External Sender <sender@external.test>",
        f"To: {recipient_addr}",
        "Subject: inbound->IMAP-read seam proof",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must round-trip verbatim through seal and open.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()

    # ── Inbound: real SMTP MAIL/RCPT/DATA over port-25 STARTTLS (the MTA seals
    # to the recipient's MSEK-derived pubkey + ingests). 250 on `.` only after
    # the WS-RPC ingest landed in nest.
    deadline = time.monotonic() + 40.0
    _deliver_inbound(handle, raw_message, deadline)

    # ── Read: IMAP AUTH + SELECT (poll for the delivered message) + FETCH decrypt.
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the provisioned inbound recipient"

        # The ingest is synchronous (250 followed the nest insert), but SELECT
        # reads the MDA's nest-backed mailbox view; poll briefly for the count to
        # reach 1 to absorb any mailbox-state propagation.
        select_deadline = time.monotonic() + 20.0
        count = _select_inbox_count(sock, buf, "a2", deadline)
        tag_n = 3
        while count < 1 and time.monotonic() < select_deadline:
            time.sleep(0.25)
            count = _select_inbox_count(sock, buf, f"a{tag_n}", deadline)
            tag_n += 1
        assert count == 1, (
            f"fresh recipient INBOX must hold exactly the one inbound message; "
            f"got {count} EXISTS (see {handle.bridge_log_hint()})"
        )

        # The clean INBOX holds exactly our message → it is sequence number 1 in
        # the SELECTed mailbox; fetch it directly (no SEARCH needed).
        fetch_status, fetched = _imap_seq_fetch_body(sock, buf, f"a{tag_n}", 1, deadline)
        tag_n += 1
        assert fetch_status == "OK", f"FETCH 1 BODY[] must succeed; got {fetch_status}"
        assert fetched is not None, "FETCH 1 BODY[] must return a literal body"

        # ── The assertions that actually prove inbound→read works ──
        # (1) The MTA-sealed envelope opened via OpenMailRecord and the original
        #     RFC 5322 content is byte-faithful: the exact message the external MX
        #     sent appears verbatim as a suffix of the decrypted blob (the MTA only
        #     *prepends* trace headers, never rewrites the original bytes).
        assert raw_message in fetched, (
            "the decrypted body must contain the sent RFC 5322 message verbatim "
            f"(MTA-seal → MDA OpenMailRecord round-trip);\n want suffix {raw_message!r}\n"
            f"  got {fetched!r}"
        )
        # Belt-and-suspenders on the distinctive fields (independent of any future
        # header rewriting of the original message).
        assert message_id.encode() in fetched, "the original Message-ID must survive"
        assert nonce.encode() in fetched, "the unique body token must survive"

        # (2) The MTA prepended a `Received:` trace header BEFORE sealing
        #     (server.go:1237-1252) — so it is inside the decrypted body, ahead of
        #     the original `Subject:` header.
        received_idx = fetched.find(b"Received:")
        subject_idx = fetched.find(b"Subject:")
        assert received_idx != -1, (
            f"decrypted body must carry the MTA-prepended `Received:` trace header; got {fetched!r}"
        )
        assert received_idx < subject_idx, (
            "the `Received:` header must be prepended ahead of the original headers; "
            f"got received@{received_idx} subject@{subject_idx}"
        )

        sock.sendall(b"a99 LOGOUT\r\n")


def test_inbound_smtp_body_axis_search_opens_index_hint(mail_bridge_inbound_to_imap):
    """Inbound SMTP DATA → `UID SEARCH BODY` opens the MTA-sealed index hint.

    The companion FETCH test proves the MTA-sealed *body* envelope round-trips
    through `OpenMailRecord`; this proves the MTA-sealed *index hint* does too.
    On the inbound DATA path the MTA tokenizes `Subject + " " + BodyText` and
    seals that canonical token set as a SEPARATE `EncryptToRecipient`
    `MailRecordEnvelope` — distinct from the body envelope (`server.go:1260`
    tokenize → `:1534` `EncryptToRecipient(hint.CanonicalBytes, indexPubkey)`).
    The MDA's body-axis SEARCH (`internal/mda/imap/search.go` `bodySearch`)
    HPKE-opens that hint segment via the SAME `OpenMailRecord(envelope, snapshot)`
    primitive the body-FETCH path uses — never the MSEK-AEAD `Decrypt` — tokenizes
    the plaintext, and matches when every query token is present.

    Until now only an APPEND-injected index hint was exercised
    (`test_mda_imap_search_body_axis_decrypts_index_hint`); whether the genuine
    MTA *inbound* path produces a searchable hint that opens via `OpenMailRecord`
    was UNTESTED — the precise `mda-openmailrecord-not-decrypt` bug class for the
    SEARCH axis, "only tier_3 catches it".

    Robust to the session-scoped recipient's INBOX accumulating across the
    sibling FETCH test: a per-run nonce body token is unique to this message, so
    membership (exactly one matching UID) holds regardless of how many other
    messages share the mailbox. A non-empty `BODY` term is used here to exercise
    the body axis specifically; empty `UID SEARCH ALL` (the server-side axis with
    no terms) is covered separately by
    `test_mail_bridge_mda.py::test_mda_imap_uid_search_all_returns_every_uid`
    (was a Go nil-slice→CBOR-null wire bug, fixed 2026-05-25).
    """
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()

    recipient_addr = handle.recipient_username
    # A distinctive single-token nonce (UAX#29 keeps the letter+digit run as one
    # word) that is present in no other test message, plus a guaranteed-absent term.
    nonce = f"inboundsearchhint{int(time.time() * 1000)}qx"
    absent = "absentbodyterm9z7qx"
    message_id = f"<{nonce}@external.test>"
    body_lines = [
        "From: External Sender <sender@external.test>",
        f"To: {recipient_addr}",
        "Subject: inbound body-axis search proof",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:05:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} ledger reconciles every quarter.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()

    deadline = time.monotonic() + 40.0
    _deliver_inbound(handle, raw_message, deadline)

    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "b1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the provisioned inbound recipient"

        # Body-axis SEARCH requires a SELECTed mailbox (search.go guard).
        sel_status, _ = _imap_cmd(sock, buf, "b2", "SELECT INBOX", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # The ingest is synchronous (250 followed the nest insert), but poll the
        # SEARCH briefly to absorb any index-segment propagation. `bodySearch`
        # re-fetches the segment list from nest each call, so re-running it (no
        # re-SELECT) picks up the freshly-ingested hint.
        search_deadline = time.monotonic() + 20.0
        tag_n = 3
        hit_status, hits = _imap_uid_search(sock, buf, f"b{tag_n}", f'BODY "{nonce}"', deadline)
        tag_n += 1
        while hit_status == "OK" and not hits and time.monotonic() < search_deadline:
            time.sleep(0.25)
            hit_status, hits = _imap_uid_search(
                sock, buf, f"b{tag_n}", f'BODY "{nonce}"', deadline
            )
            tag_n += 1

        # ── The assertion that actually proves the inbound index-hint is searchable ──
        # SEARCH succeeds only by HPKE-opening the MTA-sealed index-hint segment via
        # OpenMailRecord (the EncryptToRecipient envelope is openable only with the
        # snapshot's leaf secret, not the MSEK-AEAD Decrypt).
        assert hit_status == "OK", (
            "UID SEARCH BODY on the inbound message must succeed by HPKE-opening the "
            f"MTA-sealed index hint via OpenMailRecord; got {hit_status} "
            f"(see {handle.bridge_log_hint()})"
        )
        assert len(hits) == 1, (
            f"the unique inbound body token {nonce!r} must match exactly its own "
            f"inbound message; got UIDs {sorted(hits)}"
        )
        matched_uid = next(iter(hits))

        # A guaranteed-absent token must not match our inbound message — proves the
        # match is real token-set intersection, not "opened therefore matched".
        miss_status, misses = _imap_uid_search(
            sock, buf, f"b{tag_n}", f'BODY "{absent}"', deadline
        )
        assert miss_status == "OK", (
            f"UID SEARCH BODY (absent term) must succeed; got {miss_status}"
        )
        assert matched_uid not in misses, (
            f"absent token must not match the inbound UID {matched_uid}; got {sorted(misses)}"
        )

        # ── The content-index hybrid, end to end (rollout S5's query half) ──
        # The searches above did more than answer: every hint they opened was
        # staged into the session's sealed mail slice and flushed to the
        # `__index` rail. So by now this message is COVERED, and a repeat search
        # takes the other branch of `bodySearch` — its verdict comes from the
        # slice and its hint is never opened again.
        #
        # The assertion is equality with the scan's answer, because that is the
        # whole contract: the two paths are indistinguishable from outside by
        # design, and the failure mode this guards is precisely a coverage or
        # match-set bug turning the second search into a WRONG answer (usually
        # empty) while the first stayed right. Before the query half existed
        # this simply re-scanned, so a green here is new behaviour being
        # exercised, not a tautology.
        tag_n += 1
        repeat_status, repeat_hits = _imap_uid_search(
            sock, buf, f"b{tag_n}", f'BODY "{nonce}"', deadline
        )
        assert repeat_status == "OK", (
            "a repeat body-axis SEARCH must succeed once the slice covers the "
            f"message; got {repeat_status} (see {handle.bridge_log_hint()})"
        )
        assert repeat_hits == hits, (
            "answering from the sealed slice must give the same UIDs as the hint "
            f"scan did; scan said {sorted(hits)}, slice said {sorted(repeat_hits)}"
        )

        # And the absent term still misses on the index branch — a covered
        # message must be answered NO, never widened into a match.
        tag_n += 1
        miss2_status, misses2 = _imap_uid_search(
            sock, buf, f"b{tag_n}", f'BODY "{absent}"', deadline
        )
        assert miss2_status == "OK", f"repeat absent-term SEARCH; got {miss2_status}"
        assert matched_uid not in misses2, (
            "a covered message must not match an absent token when the answer "
            f"comes from the slice; got {sorted(misses2)}"
        )

        sock.sendall(b"b99 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_an_over_frame_inbound_message_delivers_by_reference_and_fetches_byte_for_byte(
    mail_bridge_inbound_to_imap,
):
    """A 6 MB inbound message crosses the bulk-byte plane and reads back byte-for-byte.

    This is the end-to-end proof of the bulk-plane reference legs
    (`smtp-server.md` § Message size limits). The message is far too large for the
    2 MiB WS-RPC frame, so *every* leg below must do the reference thing, and a
    regression in any one of them fails this test rather than silently losing mail:

      real SMTP DATA (6 MB)
        → the Go MTA seals, sees the sealed body will not fit the frame, mints a
          `MailBody` bulk-byte token, splits it via shared Rust, and stages the
          chunks on `POST /api/v1/chunks`
        → `ingest_inbound_mail` carries only the ordered chunk hashes (a `body_ref`),
          not the bytes
        → nest resolves the reference from its own blob store, rejoins, and rests
          the message sealed in the identical at-rest shape an inline body takes
        → the Go MDA's `FETCH BODY[]` gets a `body_ref` back, GETs each chunk over
          the open download route, rejoins via the same shared-Rust join, and HPKE-opens
        → the plaintext is byte-identical to what the external MX sent.

    Before these legs existed this exact message was refused `552 5.3.4` at the
    perimeter (and, before *that*, deferred `451` forever). Its sibling
    `test_inbound_message_over_the_inline_ceiling_delivers_by_reference` pins the
    SMTP-side flip; this one pins the whole round trip, including the read leg —
    the half a store-side-only test cannot see.

    6 MB is deliberate, not arbitrary: comfortably over the frame, so the body can
    only arrive by reference. Since ceiling retirement the only ceiling is
    `max_message_bytes` (50 MB) and a sealed body of any size rests as continuation
    records, so this exercises the reference round trip well within the product
    ceiling.
    """
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()

    recipient_addr = handle.recipient_username
    nonce = f"bigbody{int(time.time() * 1000)}qx"
    message_id = f"<{nonce}@external.test>"

    # ~6 MB of body. No line begins with "." so SMTP dot-stuffing is a no-op and the
    # bytes on the wire are the bytes we assert on. The nonce is planted at both
    # ends of the filler so a truncated or mis-ordered chunk rejoin cannot pass:
    # getting the last marker back means every chunk before it landed, in order.
    filler_line = "y" * 76 + "\r\n"
    filler = filler_line * ((6 * 1024 * 1024) // len(filler_line))
    body = f"{nonce}-HEAD\r\n{filler}{nonce}-TAIL\r\n"
    header_lines = [
        "From: External Sender <sender@external.test>",
        f"To: {recipient_addr}",
        "Subject: over-frame inbound (bulk-byte-plane reference proof)",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "",
    ]
    raw_message = ("\r\n".join(header_lines) + body).encode()
    assert len(raw_message) > 2 * 1024 * 1024, (
        "the probe must exceed the 2 MiB WS-RPC frame — otherwise it would ride "
        "inline and prove nothing about the reference legs"
    )

    # Generous budgets: 6 MB over SMTP, then a chunked stage, then a chunked fetch.
    deadline = time.monotonic() + 180.0
    _deliver_inbound(handle, raw_message, deadline)

    deadline = time.monotonic() + 180.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "b1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the provisioned inbound recipient"

        select_deadline = time.monotonic() + 30.0
        count = _select_inbox_count(sock, buf, "b2", deadline)
        tag_n = 3
        while count < 1 and time.monotonic() < select_deadline:
            time.sleep(0.25)
            count = _select_inbox_count(sock, buf, f"b{tag_n}", deadline)
            tag_n += 1
        assert count >= 1, (
            "the over-frame message must be DELIVERED, not refused: nest stored nothing "
            f"(see {handle.bridge_log_hint()})"
        )

        fetch_status, fetched = _imap_seq_fetch_body(sock, buf, f"b{tag_n}", count, deadline)
        assert fetch_status == "OK", f"FETCH BODY[] must succeed; got {fetch_status}"
        assert fetched is not None, "FETCH BODY[] must return a literal body"

        # The whole point: byte-for-byte. A wrong chunk boundary, a dropped chunk, a
        # reordered rejoin, or a truncated stage would each corrupt this silently —
        # so assert the full original message, not just a marker.
        assert raw_message in fetched, (
            "the 6 MB message must survive stage → body_ref ingest → rest → "
            "body_ref fetch → rejoin → unseal byte-for-byte; "
            f"sent {len(raw_message)} bytes, got {len(fetched)} bytes back"
        )
        assert f"{nonce}-TAIL".encode() in fetched, (
            "the tail marker must survive — its absence means the chunk rejoin "
            "dropped or truncated the final chunk"
        )

        sock.sendall(b"b99 LOGOUT\r\n")

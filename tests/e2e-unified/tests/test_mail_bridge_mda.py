"""End-to-end tests for `fauna-mail-bridge` running in its MDA role.

The bridge spawns alongside a real `fauna-nest` via the
`mail_bridge_mda` fixture (conftest.py): admin-enrolled MDA
service-user keypair, admin-claimed primary local domain,
self-signed TLS cert provisioned via
`POST /api/admin/local_domains/{domain}/self_signed_cert` (sealed +
fanned out to the bridge through `Storage::store_acme_material`),
bridge process bound to ephemeral loopback ports for IMAPS, IMAP +
STARTTLS, and CalDAV-over-HTTPS.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real wire end-to-end.
- `independent`: the bridge is a server-side concern; no per-app
  driver participates, so the suite runs once per machine regardless
  of `--client`.

Scope today:
- The smoke test proves the TLS-fan-out pipeline end-to-end: Task A's
  plaintext-mode seal+fan-out + Task B's self-signed cert endpoint + the
  bridge's startup `fetch_tls_cert_blob` + HPKE-unseal, clearing the
  `TLSProvider != nil` gate (`internal/mda/mda.go:129-131`).
- IMAP AUTH PLAIN + SELECT INBOX, and a wrong-password rejection
  (mail-MDA body-decrypt work, Task B): the fixture provisions a wrapped-MSEK blob via
  the seal-helper `seal-wrapped-msek` subcommand + `provision_wrapped_mls_blob`,
  and the bridge AEAD-unwraps it at AUTH (`UnwrapMsekBlob`) — the full MUA-AUTH
  path on real binaries.
- The full **body-decrypt read path** (mail-MDA body-decrypt work, Tasks C/D/E): the
  fixture provisions the recipient's *MSEK-derived* HPKE pubkey
  (`fauna_mls::wrapped_blob::derive_recipient_hpke_keypair`, now FFI-exposed via
  the seal-helper `derive-recipient-pubkey` subcommand) + a sealed MLS snapshot
  (`seal-mls-snapshot` → `provision_mls_snapshot_blob`), both keyed to the same
  MSEK as the wrapped blob so the APPEND/PUT seal target and the snapshot's leaf
  secret match.
  - `test_mda_imap_append_fetch_roundtrip` — APPEND + `UID FETCH BODY[]` round-trip.
  - `test_mda_imap_idle_exists_push` — two-session IDLE `* EXISTS` push (count,
    no decrypt; rides the APPEND path).
  - `test_mda_imap_search_body_axis_decrypts_index_hint` — `UID SEARCH BODY`
    HPKE-opens each index-hint segment via `OpenMailRecord` (the same leaf-secret
    primitive as body-FETCH); the Go unit test stubs the decryptor, so this is
    the only proof the real index-hint seal→open works through the stack.
  - `test_mda_caldav_put_report_roundtrip` — CalDAV PUT + REPORT round-trip.
- The **Lane C wire extensions** end-to-end over a real authenticated IMAP
  connection (the IMAP Lane C wire-extensions work) — previously proven only by the go-imap
  fork's wire tests against a *mock* `Session`
  (`third_party/go-imap/imapserver/{condstore,qresync,quota}_fauna_test.go`),
  these drive the real nest counters / quota aggregation / expunge→tombstone:
  - `test_mda_imap_getquotaroot_reports_root_and_limits` — RFC 9208
    `GETQUOTAROOT` returns the per-actor `user/<handle>` root + the real
    `ImapPolicy::default()` limits (T3.2-c).
  - `test_mda_imap_condstore_fetch_returns_modseq` — `ENABLE CONDSTORE` →
    SELECT `[HIGHESTMODSEQ]` → `UID FETCH (FLAGS MODSEQ)` carries `MODSEQ (n)`
    (T3.2-a).
  - `test_mda_imap_qresync_expunge_emits_vanished` — `ENABLE QRESYNC` →
    `UID EXPUNGE` a `\\Deleted` message → `* VANISHED <uid>` (RFC 7162 §3.2.10),
    not the per-UID `* <seq> EXPUNGE` fallback (T3.2-b).
  - `test_mda_imap_qresync_select_inline_vanished_earlier` — the inline
    `SELECT (QRESYNC (uidvalidity m0))` fast-path: an expunge-since-`m0` UID
    surfaces as `* VANISHED (EARLIER) <uid>` *within* the SELECT response
    (T3.2-b2).
  - `test_mda_imap_condstore_unchangedsince_returns_modified` — a conditional
    STORE against a now-stale modseq returns `[MODIFIED <uid>]` with the flag
    unapplied (RFC 7162 §3.1.3). This one found+fixed a real interop bug: the
    MDA had emitted `[MODIFIED]` empty with the uid-set only in the free text.
  - `test_mda_imap_qresync_changedsince_vanished_fetch` — the non-inline
    QRESYNC fallback: `UID FETCH 1:* (FLAGS) (CHANGEDSINCE m0 VANISHED)` emits
    `* VANISHED (EARLIER) <uid>` for a since-`m0` tombstone (RFC 7162 §3.2.5.1).
    A distinct `fetch.go` path (`list_messages(since_modseq)` →
    `FetchWriter.WriteVanishedEarlier`) from T5.4's inline-SELECT fast-path (T5.5).
  - `test_mda_imap_idle_fetch_carries_modseq_under_condstore` — under
    `ENABLE CONDSTORE`, the IDLE append push's unsolicited `* <seq> FETCH`
    additionally carries `MODSEQ (<n>)` (RFC 7162 §3.1.7), driven by `idle.go`'s
    `flagsUpdate` → the FAUNA-FORK `WriteMessageFlagsModSeq` seam (T5.7).
"""

import base64
import re
import socket
import ssl
import time

import pytest

from helpers.mail_wire import (
    _imap_append,
    _imap_auth_plain,
    _imap_cmd,
    _imap_read_tagged,
    _imap_read_until_fetch_modseq,
    _imap_uid_fetch_binary_size,
    _imap_uid_fetch_body,
    _imap_uid_fetch_capture,
    _imap_uid_fetch_section,
    _imap_uid_search,
    _imap_wait_for_exists,
    _imaps_connect,
    _recv_line,
)

pytestmark = pytest.mark.tier_3


def test_mda_bridge_spawns_with_healthz_green(mail_bridge_mda):
    """Smoke: the MDA bridge fixture brought a real bridge process up.

    Achieving `/healthz == 200` from the fixture's wait loop
    (`_wait_for_mail_bridge_metrics`) is equivalent to asserting:

      1. The bridge dialed nest's WS-RPC endpoint successfully (uses
         the Ed25519 keypair the fixture wrote to disk).
      2. `fauna.bridges.whoami` resolved its role to `mda` (validates
         the admin enrollment landed in `bridge_service_users` with
         `status='approved'`).
      3. `fauna.bridges.fetch_tls_cert_blob` returned a non-empty
         sealed blob (validates Task A's plaintext-mode fan-out
         actually populated `bridge_tls_cert_blobs` AND Task B's
         self-signed endpoint produced a real cert).
      4. The bridge HPKE-Opened the blob with the x25519 secret in
         its keyfile, yielding a parseable `TlsCertBundle` (validates
         the round-trip seal/unseal pair).
      5. `mda.Run` passed the `TLSProvider != nil` hard gate, bound
         all three listeners (143 / 443 / 993) successfully, and
         transitioned the health-state to ready (validates the
         operator-hatch port overrides and the listener bind path).

    Any of those failing leaves /healthz timing out; the wait helper
    fails the fixture itself, not this test. So the test body is
    intentionally minimal — the fixture's success IS the assertion.
    """
    assert mail_bridge_mda.proc.poll() is None, (
        "mail-bridge process exited unexpectedly after /healthz went green"
    )
    assert mail_bridge_mda.bridge_role == "mda"
    assert mail_bridge_mda.imaps_port > 0
    assert mail_bridge_mda.imap_starttls_port > 0
    assert mail_bridge_mda.caldav_port > 0


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_login_and_select(mail_bridge_mda):
    """IMAP AUTH PLAIN + SELECT INBOX over IMAPS (mail-MDA body-decrypt work, Task B).

    Proves the full MUA-AUTH path end-to-end against a real nest + bridge: the
    fixture sealed a wrapped-MSEK blob under `recipient_password` (the seal-helper
    `seal-wrapped-msek` subcommand — the same client-side seal a user's primary
    client performs) and provisioned it via `provision_wrapped_mls_blob`. On AUTH
    PLAIN the bridge resolves the actor (`validate_recipient`), fetches the blob,
    and AEAD-unwraps it under the supplied password (`UnwrapMsekBlob`) — a
    successful unwrap IS the authentication signal (imap-server.md
    § Authentication). SELECT INBOX then returns the empty mailbox (0 EXISTS).
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 40.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        status = _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        )
        assert status == "OK", f"AUTH PLAIN must succeed for the provisioned recipient; got {status}"
        sock.sendall(b"a2 SELECT INBOX\r\n")
        status, untagged = _imap_read_tagged(sock, buf, "a2", deadline)
        assert status == "OK", f"SELECT INBOX must succeed; got {status}: {untagged!r}"
        exists = [ln for ln in untagged if ln.upper().endswith("EXISTS")]
        assert exists, f"SELECT must report an EXISTS line; got {untagged!r}"
        # `* 0 EXISTS` — a freshly-provisioned recipient has never received mail.
        count = int(exists[0].split()[1])
        assert count == 0, f"fresh INBOX must be empty; got {count} EXISTS"
        sock.sendall(b"a3 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_wrong_password_rejected(mail_bridge_mda):
    """A wrong password fails AUTH — AEAD-unwrap-as-auth (imap-server.md
    § Authentication). The wrapped-MSEK blob exists, but unwrapping it under the
    wrong credential AEAD-fails, so the bridge returns a tagged NO. Proves the
    auth decision is the AEAD tag, not a comparison the bridge could get wrong.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 40.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        status = _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, "wrong-password", deadline
        )
        assert status == "NO", f"wrong password must be rejected with a tagged NO; got {status}"


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_append_fetch_roundtrip(mail_bridge_mda):
    """APPEND a message, then FETCH its body back decrypted (mail-MDA body-decrypt work,
    Task C) — the Stage-3 "read received mail" keystone on real binaries.

    The bridge HPKE-seals the APPEND literal to the actor's registered
    *MSEK-derived* MLS pubkey (`imap/append.go`), nest stores the opaque
    ciphertext, and `UID FETCH BODY[]` round-trips it through
    `fetch_message_ciphertext` → `MlsCapability.OpenMailRecord(envelope,
    snapshot)` (`imap/fetch.go`), where the snapshot's leaf secret — derived
    from the *same* MSEK the fixture sealed into the wrapped blob + snapshot —
    opens what APPEND sealed. The recovered body must equal the bytes uploaded.

    Count-relative + UID-addressed because `mail_bridge_mda` is session-scoped:
    the recipient INBOX accumulates messages across tests in this file, so we
    assert the body of the UID we just APPENDed rather than an absolute count.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: alice@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: APPEND/FETCH body-decrypt round-trip\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <append-fetch@mda.fauna.test>\r\n"
        b"\r\n"
        b"Line one of the body.\r\n"
        b"Line two -- the MDA must recover this verbatim from the seal.\r\n"
    )
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        # APPEND (authenticated state; the nest handler seeds the standard
        # mailboxes before applying the Append, so no prior SELECT is needed).
        status, uid = _imap_append(sock, buf, "a2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

        # SELECT so the mailbox is in selected state for FETCH.
        sock.sendall(b"a3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "a3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        fetch_status, body = _imap_uid_fetch_body(sock, buf, "a4", uid, deadline)
        assert fetch_status == "OK", f"UID FETCH must succeed; got {fetch_status}"
        assert body == message, (
            "FETCH BODY[] must round-trip the APPENDed bytes exactly "
            f"(decrypt via the MSEK-derived snapshot leaf key);\n"
            f" want {message!r}\n  got {body!r}"
        )
        sock.sendall(b"a5 LOGOUT\r\n")


def test_mda_imap_uid_search_all_returns_every_uid(mail_bridge_mda):
    """`UID SEARCH ALL` (empty criteria) returns every UID in the mailbox.

    RFC 9051 § 6.4.4: the `ALL` search key matches every message. The MDA
    partitions a SEARCH into server-side axes + a local body axis
    (`imap-server.md` § SEARCH); `ALL` carries no terms in either axis, so the
    bridge issues `fauna.bridges.search_messages` with an empty criteria set
    (`search.go` `wantsFlagRPC = wantsFlag || !wantsBody` ⇒ true), and nest
    must return the full per-mailbox UID set rather than rejecting the request
    — its DB adds no extra WHERE clause when `terms` is empty
    (`search_bridge_imap_messages`; proven at the unit level by
    `search_empty_terms_returns_all_uids_ascending`). This is the end-to-end
    proof through the real bridge that the empty-criteria path is not rejected
    (the gap `test_mail_inbound_to_imap.py` § body-axis flagged as deferred).

    Session-scoped fixture ⇒ membership assertion (the freshly-APPENDed UID is
    in the result set) rather than an absolute count.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: alice@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: UID SEARCH ALL membership proof\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <search-all@mda.fauna.test>\r\n"
        b"\r\n"
        b"Body for the UID SEARCH ALL membership proof.\r\n"
    )
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, uid = _imap_append(sock, buf, "a2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

        sock.sendall(b"a3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "a3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        search_status, uids = _imap_uid_search(sock, buf, "a4", "ALL", deadline)
        assert search_status == "OK", (
            "`UID SEARCH ALL` must succeed — empty criteria makes the bridge call "
            "search_messages with no terms, and nest must return the full UID set "
            f"(not ok=false); got tagged {search_status}"
        )
        assert uid in uids, (
            "`UID SEARCH ALL` must return every UID in INBOX, including the "
            f"just-APPENDed UID {uid}; got {sorted(uids)}"
        )
        sock.sendall(b"a5 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_sectioned_body_fetch(mail_bridge_mda):
    """Sectioned BODY[<section>] FETCH (RFC 9051 §6.4.5) over the real stack —
    IMAP sectioned-FETCH work, Slice 1.

    APPEND a message (the bridge HPKE-seals it), then drive the three top-level
    section forms a real MUA leans on against the decrypted plaintext:

      * `BODY.PEEK[HEADER.FIELDS (Subject Message-Id)]` — header-sync: exactly
        the two named header lines (in MESSAGE order) + the blank line, and
        crucially NOT the From/To/Date lines.
      * `BODY[HEADER]` — the whole header block incl. the terminating blank line.
      * `BODY[TEXT]` — the body only (everything after the blank line).

    Proves the shared-Rust extractor (libs/fauna-mail/src/bodysection.rs) runs
    end-to-end through the FFI + the MDA emit path + the go-imap fork's response
    writer, and that HEADER ++ TEXT reconstructs the message the seal preserved.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    header_block = (
        b"From: alice@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: Sectioned fetch test\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <section-fetch@mda.fauna.test>\r\n"
        b"\r\n"
    )
    body_text = b"Body line one.\r\nBody line two -- after the blank line.\r\n"
    message = header_block + body_text

    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "s1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, uid = _imap_append(sock, buf, "s2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

        sock.sendall(b"s3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "s3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # Header-sync: only the two requested headers, message order, + blank line.
        st, fields = _imap_uid_fetch_section(
            sock, buf, "s4", uid, "BODY.PEEK[HEADER.FIELDS (Subject Message-Id)]", deadline
        )
        assert st == "OK", f"HEADER.FIELDS FETCH must succeed; got {st}"
        assert fields == (
            b"Subject: Sectioned fetch test\r\n"
            b"Message-ID: <section-fetch@mda.fauna.test>\r\n"
            b"\r\n"
        ), f"HEADER.FIELDS must return only the named headers in message order; got {fields!r}"

        # Whole header block (incl. the terminating blank line).
        st, hdr = _imap_uid_fetch_section(sock, buf, "s5", uid, "BODY[HEADER]", deadline)
        assert st == "OK", f"BODY[HEADER] FETCH must succeed; got {st}"
        assert hdr == header_block, f"BODY[HEADER] mismatch;\n want {header_block!r}\n  got {hdr!r}"

        # Body only.
        st, txt = _imap_uid_fetch_section(sock, buf, "s6", uid, "BODY[TEXT]", deadline)
        assert st == "OK", f"BODY[TEXT] FETCH must succeed; got {st}"
        assert txt == body_text, f"BODY[TEXT] mismatch;\n want {body_text!r}\n  got {txt!r}"

        # HEADER ++ TEXT reconstructs the sealed message.
        assert hdr + txt == message, "BODY[HEADER] ++ BODY[TEXT] must reconstruct the message"

        sock.sendall(b"s7 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_numbered_part_body_fetch(mail_bridge_mda):
    """Numbered-part BODY[N…] FETCH (RFC 9051 §6.4.5) over the real stack —
    IMAP sectioned-FETCH work, Slice 2.

    APPEND a multipart/mixed message (two leaf parts; the bridge HPKE-seals it),
    then drive the numbered-part forms a real MUA uses to lazily fetch attachments
    rather than pulling the whole body:

      * `BODY[1]` / `BODY[2]` — each leaf part's *contents* (body), without its
        MIME header (the CRLF before the boundary belongs to the boundary).
      * `BODY[1.MIME]` / `BODY[2.MIME]` — each part's own MIME header block.

    Proves the shared-Rust part-path navigation (libs/fauna-mail/src/bodysection.rs
    locate_part) runs end-to-end through the FFI + MDA emit path + go-imap fork's
    response writer on a real multipart message the seal preserved verbatim.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: alice@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: Numbered part test\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <numbered-part@mda.fauna.test>\r\n"
        b"Content-Type: multipart/mixed; boundary=NPBOUND\r\n"
        b"\r\n"
        b"--NPBOUND\r\n"
        b"Content-Type: text/plain\r\n"
        b"\r\n"
        b"first body\r\n"
        b"--NPBOUND\r\n"
        b"Content-Type: text/html\r\n"
        b"\r\n"
        b"<p>second</p>\r\n"
        b"--NPBOUND--\r\n"
    )

    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "n1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, uid = _imap_append(sock, buf, "n2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

        sock.sendall(b"n3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "n3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # BODY[1] / BODY[2]: each leaf part's body content (no MIME header, no
        # trailing CRLF — that CRLF belongs to the boundary delimiter).
        st, part1 = _imap_uid_fetch_section(sock, buf, "n4", uid, "BODY[1]", deadline)
        assert st == "OK", f"BODY[1] FETCH must succeed; got {st}"
        assert part1 == b"first body", f"BODY[1] = {part1!r}, want b'first body'"

        st, part2 = _imap_uid_fetch_section(sock, buf, "n5", uid, "BODY[2]", deadline)
        assert st == "OK", f"BODY[2] FETCH must succeed; got {st}"
        assert part2 == b"<p>second</p>", f"BODY[2] = {part2!r}, want b'<p>second</p>'"

        # BODY[N.MIME]: each part's own MIME header block (incl. blank line).
        st, mime1 = _imap_uid_fetch_section(sock, buf, "n6", uid, "BODY[1.MIME]", deadline)
        assert st == "OK", f"BODY[1.MIME] FETCH must succeed; got {st}"
        assert mime1 == b"Content-Type: text/plain\r\n\r\n", f"BODY[1.MIME] = {mime1!r}"

        st, mime2 = _imap_uid_fetch_section(sock, buf, "n7", uid, "BODY[2.MIME]", deadline)
        assert st == "OK", f"BODY[2.MIME] FETCH must succeed; got {st}"
        assert mime2 == b"Content-Type: text/html\r\n\r\n", f"BODY[2.MIME] = {mime2!r}"

        sock.sendall(b"n8 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_binary_section_fetch(mail_bridge_mda):
    """BINARY[N] / BINARY.SIZE[N] FETCH (RFC 3516 / RFC 9051 §6.4.5) over the
    real stack — IMAP sectioned-FETCH work, T7.7.

    APPEND a multipart/mixed message with a base64-encoded attachment (the
    bridge HPKE-seals it), then fetch the attachment via BINARY[2] and assert
    the bridge CTE-DECODED it (base64 → raw octets, no charset conversion) and
    that BINARY.SIZE[2] reports the *decoded* octet count, not the base64
    length. Proves the shared-Rust CTE decode (libs/fauna-mail/src/bodysection.rs
    fetch_binary_section / fetch_binary_size) runs end-to-end through the FFI +
    MDA emit path + the go-imap fork's `~{n}` literal8 writer on a real sealed
    message. RFC 9051 IMAP4rev2 folds BINARY into the baseline FETCH items, so a
    rev2 client is entitled to this even without a separate BINARY capability.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    # Part 2 is base64 "aGVsbG8gYmluYXJ5" == "hello binary" (12 octets decoded;
    # the encoded form is 16 chars — so SIZE proves we report the decoded len).
    message = (
        b"From: alice@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: Binary fetch test\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <binary-fetch@mda.fauna.test>\r\n"
        b"Content-Type: multipart/mixed; boundary=BFBOUND\r\n"
        b"\r\n"
        b"--BFBOUND\r\n"
        b"Content-Type: text/plain\r\n"
        b"\r\n"
        b"cover text\r\n"
        b"--BFBOUND\r\n"
        b"Content-Type: application/octet-stream\r\n"
        b"Content-Transfer-Encoding: base64\r\n"
        b"\r\n"
        b"aGVsbG8gYmluYXJ5\r\n"
        b"--BFBOUND--\r\n"
    )

    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "b1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, uid = _imap_append(sock, buf, "b2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

        sock.sendall(b"b3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "b3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # BINARY[2] = the attachment, base64-DECODED by the bridge (no charset
        # conversion); the `~{n}` literal8 carries the raw octets.
        st, bin2 = _imap_uid_fetch_section(sock, buf, "b4", uid, "BINARY[2]", deadline)
        assert st == "OK", f"BINARY[2] FETCH must succeed; got {st}"
        assert bin2 == b"hello binary", f"BINARY[2] = {bin2!r}, want b'hello binary'"

        # BINARY.SIZE[2] = the *decoded* octet count (12), not the 16-char
        # base64 length.
        st, size2 = _imap_uid_fetch_binary_size(sock, buf, "b5", uid, "BINARY.SIZE[2]", deadline)
        assert st == "OK", f"BINARY.SIZE[2] FETCH must succeed; got {st}"
        assert size2 == 12, f"BINARY.SIZE[2] = {size2}, want 12 (decoded octets)"

        # BINARY[1] = the 7bit text part, returned verbatim (no CTE to strip).
        st, bin1 = _imap_uid_fetch_section(sock, buf, "b6", uid, "BINARY[1]", deadline)
        assert st == "OK", f"BINARY[1] FETCH must succeed; got {st}"
        assert bin1 == b"cover text", f"BINARY[1] = {bin1!r}, want b'cover text'"

        sock.sendall(b"b7 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_seen_on_non_peek_fetch(mail_bridge_mda):
    """Implicit `\\Seen` on a non-PEEK body FETCH (RFC 9051 §6.4.5 + RFC 7162)
    over the real stack — a follow-on IMAP slice.

    APPEND two unseen messages, then prove the read-path side-effect end-to-end
    against the real nest placement state (not a stub flag table):

      * A non-PEEK `BODY[]` on message A marks it `\\Seen` server-side — a
        follow-up `UID FETCH (FLAGS)` shows `\\Seen`, and (because the response
        is non-mutating elsewhere) the implicit set is the only thing that could
        have flipped it.
      * Under `ENABLE CONDSTORE`, the implicit-set `BODY[]` response itself
        carries the bumped `MODSEQ (<n>)` AND the force-emitted `FLAGS (\\Seen)`
        on the same FETCH (RFC 7162 §3.1.4 — set BEFORE emit, Dovecot parity),
        not a follow-up unsolicited FETCH.
      * A `BODY.PEEK[]` on message B leaves it unseen.

    The Go unit test (fetch_seen_test.go) stubs store_flags, so this tier_3 is
    the only proof the implicit set lands in nest's `bridge_imap_messages.flags`
    and bumps `highestmodseq` through the deployed binaries.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0

    def _msg(subject: bytes, mid: bytes) -> bytes:
        return (
            b"From: alice@example.com\r\n"
            b"To: " + handle.recipient_username.encode() + b"\r\n"
            b"Subject: " + subject + b"\r\n"
            b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
            b"Message-ID: <" + mid + b"@mda.fauna.test>\r\n"
            b"\r\n"
            b"seen-track body\r\n"
        )

    msg_a = _msg(b"Seen track A", b"seen-a")
    msg_b = _msg(b"Seen track B", b"seen-b")

    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "z1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        st, uid_a = _imap_append(sock, buf, "z2", "INBOX", msg_a, deadline)
        assert st == "OK" and uid_a is not None, f"APPEND A: {st}/{uid_a}"
        st, uid_b = _imap_append(sock, buf, "z3", "INBOX", msg_b, deadline)
        assert st == "OK" and uid_b is not None, f"APPEND B: {st}/{uid_b}"

        # CONDSTORE so the implicit-\Seen FETCH must carry MODSEQ (RFC 7162 §3.1.4).
        st, _ = _imap_cmd(sock, buf, "z4", "ENABLE CONDSTORE", deadline)
        assert st == "OK", f"ENABLE CONDSTORE must succeed; got {st}"
        st, _ = _imap_cmd(sock, buf, "z5", "SELECT INBOX", deadline)
        assert st == "OK", f"SELECT INBOX must succeed; got {st}"

        # Baseline: A is unseen. A bare `(FLAGS)` fetch is metadata-only and must
        # NOT itself set \Seen (it's not a body fetch).
        st, flags_a0 = _imap_cmd(sock, buf, "z6", f"UID FETCH {uid_a} (FLAGS)", deadline)
        assert st == "OK", f"FLAGS fetch must succeed; got {st}"
        assert "\\Seen" not in flags_a0, f"message A must start unseen; got {flags_a0!r}"

        # Non-PEEK BODY[] on A → sets \Seen; the response carries the bumped
        # MODSEQ + the force-emitted FLAGS (\Seen) on the same FETCH.
        st, body_a, attrs_a = _imap_uid_fetch_capture(sock, buf, "z7", uid_a, "BODY[]", deadline)
        assert st == "OK", f"BODY[] FETCH must succeed; got {st}"
        assert body_a == msg_a, "BODY[] must return the sealed message verbatim"
        assert "\\Seen" in attrs_a, (
            f"implicit-\\Seen FETCH must force-emit FLAGS (\\Seen) even on a bare "
            f"BODY[] (Dovecot parity); got {attrs_a!r}"
        )
        assert re.search(r"MODSEQ\s+\(\d+\)", attrs_a), (
            f"CONDSTORE: the implicit-set FETCH must carry the bumped MODSEQ on "
            f"the same response; got {attrs_a!r}"
        )

        # Persisted server-side: a fresh metadata FETCH shows A is now \Seen.
        st, flags_a1 = _imap_cmd(sock, buf, "z8", f"UID FETCH {uid_a} (FLAGS)", deadline)
        assert st == "OK", f"FLAGS fetch must succeed; got {st}"
        assert "\\Seen" in flags_a1, f"A must be \\Seen after the non-PEEK BODY[]; got {flags_a1!r}"

        # BODY.PEEK[] on B → must NOT set \Seen.
        st, body_b, _ = _imap_uid_fetch_capture(sock, buf, "z9", uid_b, "BODY.PEEK[]", deadline)
        assert st == "OK", f"BODY.PEEK[] FETCH must succeed; got {st}"
        assert body_b == msg_b, "BODY.PEEK[] must return the sealed message verbatim"
        st, flags_b = _imap_cmd(sock, buf, "z10", f"UID FETCH {uid_b} (FLAGS)", deadline)
        assert st == "OK", f"FLAGS fetch must succeed; got {st}"
        assert "\\Seen" not in flags_b, f"B must stay unseen after BODY.PEEK[]; got {flags_b!r}"

        sock.sendall(b"z11 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_idle_exists_push(mail_bridge_mda):
    """Two IMAP sessions for the same actor: session 1 IDLEs on INBOX, session 2
    APPENDs, and session 1 sees an unsolicited `* <n> EXISTS` push within
    seconds (mail-MDA body-decrypt work, Task D; imap-server.md § IDLE).

    Proves the nest→MDA push path end-to-end: nest emits `BridgeMailboxState`
    on the session-2 APPEND, the MDA's notification router demuxes it to
    session 1's IDLE channel, and translates it to `* EXISTS`. EXISTS is a
    count (not a decrypt), so this asserts only that a push arrived — robust to
    the session-scoped INBOX's accumulated count — not a specific number; per
    the goal doc's § Upstream-blocked gaps we do NOT assert MODSEQ/VANISHED.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0

    idle_sock, idle_buf = _imaps_connect(handle, deadline)
    with idle_sock:
        assert _imap_auth_plain(
            idle_sock, idle_buf, "i1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK"
        idle_sock.sendall(b"i2 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(idle_sock, idle_buf, "i2", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # Enter IDLE; the server acks with a `+ idling` continuation.
        idle_sock.sendall(b"i3 IDLE\r\n")
        cont = _recv_line(idle_sock, idle_buf, deadline)
        assert cont.startswith("+"), f"expected IDLE continuation, got {cont!r}"

        # Second session APPENDs a message for the same actor.
        push_sock, push_buf = _imaps_connect(handle, deadline)
        with push_sock:
            assert _imap_auth_plain(
                push_sock, push_buf, "p1", handle.recipient_username, handle.recipient_password, deadline
            ) == "OK"
            message = (
                b"From: bob@example.com\r\n"
                b"To: " + handle.recipient_username.encode() + b"\r\n"
                b"Subject: IDLE push\r\n\r\nWake the idler.\r\n"
            )
            status, _ = _imap_append(push_sock, push_buf, "p2", "INBOX", message, deadline)
            assert status == "OK", f"second-session APPEND must succeed; got {status}"
            push_sock.sendall(b"p3 LOGOUT\r\n")

        # Session 1, still idling, must receive the unsolicited EXISTS push.
        exists_deadline = time.monotonic() + 20.0
        count = _imap_wait_for_exists(idle_sock, idle_buf, exists_deadline)
        assert count >= 1, f"IDLE EXISTS push must report >= 1 message; got {count}"

        idle_sock.sendall(b"DONE\r\n")
        done_status, _ = _imap_read_tagged(idle_sock, idle_buf, "i3", deadline)
        assert done_status == "OK", f"IDLE DONE must complete OK; got {done_status}"
        idle_sock.sendall(b"i4 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_getquotaroot_reports_root_and_limits(mail_bridge_mda):
    """GETQUOTAROOT INBOX returns the per-actor quota root + live limits over
    the wire (T5.1; imap-server.md § QUOTA / GETQUOTAROOT :257, § Quota root
    model :265).

    Proves the RFC 9208 reporting chain end-to-end on real binaries — the
    fork's GETQUOTAROOT parser + `SessionQuota` dispatch + `* QUOTAROOT` /
    `* QUOTA` writers (T3.2-c) → MDA `Session.{GetQuotaRoot,GetQuota}`
    (`imap/quota.go`) → `fauna.bridges.get_quota` → nest `get_quota_handler` →
    `imap_quota_usage` (Σ record block length sized through the CARv2 index by
    record_cid, not a SQL byte column) against the **effective** `ImapPolicy`.

    Asserts the root name (`user/<localpart>`, resolved from the AUTH'd
    localPart per `quota.go::GetQuotaRoot`) and the **limits** — 1 GiB =
    1048576 KiB STORAGE, 50000 MESSAGE, the catalog `ImapPolicy::default()`
    the MDA translates bytes→KiB. Usage is *not* asserted: `mail_bridge_mda`
    is session-scoped, so the recipient's accumulated INBOX usage drifts with
    sibling tests in this file — only the limits + wire structure are stable.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 40.0
    local_part = handle.recipient_username.split("@")[0]
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, resp = _imap_cmd(sock, buf, "a2", "GETQUOTAROOT INBOX", deadline)
        assert status == "OK", f"GETQUOTAROOT must succeed; got {status}: {resp!r}"
        assert "QUOTAROOT INBOX" in resp, (
            f"want `* QUOTAROOT INBOX user/{local_part}`; got {resp!r}"
        )
        assert f"user/{local_part}" in resp, (
            f"quota root must be the per-actor `user/{local_part}` "
            f"(imap-server.md § Quota root model); got {resp!r}"
        )
        # Limits are the catalog ImapPolicy::default() the MDA translated
        # bytes→KiB: STORAGE 1 GiB = 1048576 KiB, MESSAGE 50000.
        assert re.search(r"STORAGE\s+\d+\s+1048576\b", resp), (
            f"want `STORAGE <used> 1048576` (1 GiB default limit); got {resp!r}"
        )
        assert re.search(r"MESSAGE\s+\d+\s+50000\b", resp), (
            f"want `MESSAGE <used> 50000` (default message-count limit); got {resp!r}"
        )
        sock.sendall(b"a3 LOGOUT\r\n")


def test_mda_imap_condstore_fetch_returns_modseq(mail_bridge_mda):
    """After ENABLE CONDSTORE, SELECT reports [HIGHESTMODSEQ] and a FETCH that
    names MODSEQ carries `MODSEQ (<n>)` (T5.2; imap-server.md § CONDSTORE
    :187/193, RFC 7162).

    Proves the CONDSTORE chain end-to-end on real binaries — the fork's ENABLE
    routing + `[HIGHESTMODSEQ]` on SELECT + the MODSEQ FETCH-item parser +
    `FetchResponseWriter.WriteModSeq` (T3.2-a) → MDA `condStoreActive()` →
    `fetch.go` emitting the per-row modseq → nest `select_mailbox_handler`'s
    real `highestmodseq` and `bridge_imap_messages.modseq`. The Go fork wire
    test pins these against a *mock* session returning HIGHESTMODSEQ 7 /
    MODSEQ 42; this asserts they come from the real nest counter (n≥1).
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: alice@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: CONDSTORE MODSEQ probe\r\n"
        b"Message-ID: <condstore@mda.fauna.test>\r\n"
        b"\r\nModseq must be reported for this row once CONDSTORE is enabled.\r\n"
    )
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, resp = _imap_cmd(sock, buf, "a2", "ENABLE CONDSTORE", deadline)
        assert status == "OK", f"ENABLE CONDSTORE must succeed; got {status}: {resp!r}"
        assert "ENABLED CONDSTORE" in resp, f"want `* ENABLED CONDSTORE`; got {resp!r}"

        status, uid = _imap_append(sock, buf, "a3", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

        sel_status, sel_resp = _imap_cmd(sock, buf, "a4", "SELECT INBOX", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}: {sel_resp!r}"
        m = re.search(r"HIGHESTMODSEQ\s+(\d+)", sel_resp)
        assert m, (
            "SELECT after ENABLE CONDSTORE must report `[HIGHESTMODSEQ <n>]`; "
            f"got {sel_resp!r}"
        )
        assert int(m.group(1)) >= 1, f"HIGHESTMODSEQ must be a real counter ≥1; got {m.group(1)}"

        fetch_status, fetch_resp = _imap_cmd(
            sock, buf, "a5", f"UID FETCH {uid} (FLAGS MODSEQ)", deadline
        )
        assert fetch_status == "OK", f"UID FETCH must succeed; got {fetch_status}: {fetch_resp!r}"
        fm = re.search(r"MODSEQ\s+\((\d+)\)", fetch_resp)
        assert fm, (
            "FETCH (FLAGS MODSEQ) after ENABLE CONDSTORE must carry `MODSEQ (<n>)` "
            f"(imap-server.md:193); got {fetch_resp!r}"
        )
        assert int(fm.group(1)) >= 1, f"per-row MODSEQ must be a real counter ≥1; got {fm.group(1)}"
        sock.sendall(b"a6 LOGOUT\r\n")


def test_mda_imap_qresync_expunge_emits_vanished(mail_bridge_mda):
    """After ENABLE QRESYNC, expunging a message reports `* VANISHED <uid>`
    (RFC 7162 §3.2.10) instead of the per-UID `* <seq> EXPUNGE` fallback
    (T5.3; imap-server.md § IDLE push wiring :238, § QRESYNC :199).

    Proves the QRESYNC expunge chain end-to-end on real binaries — the fork's
    ENABLE routing + `ExpungeWriter.WriteVanished` (T3.2-b) → MDA
    `qresyncActive()` → `expunge.go` emitting VANISHED → nest `expunge`
    performing the `\\Deleted ∩ uidset` removal (RFC 4315) and the tombstone
    insert. The Go fork wire test pins the wire shape against a *mock* session
    (`* VANISHED 2,4,6`); this drives a real STORE→EXPUNGE round-trip.

    `UID EXPUNGE` (not plain `EXPUNGE`) keeps this surgical: only the UID we
    just APPENDed + `\\Deleted`-flagged is removed, so the session-scoped INBOX
    that sibling tests share is otherwise undisturbed.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: bob@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: QRESYNC expunge target\r\n"
        b"Message-ID: <qresync-expunge@mda.fauna.test>\r\n"
        b"\r\nThis message will be \\Deleted then UID EXPUNGEd under QRESYNC.\r\n"
    )
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, resp = _imap_cmd(sock, buf, "a2", "ENABLE QRESYNC", deadline)
        assert status == "OK", f"ENABLE QRESYNC must succeed; got {status}: {resp!r}"
        assert "ENABLED QRESYNC" in resp, f"want `* ENABLED QRESYNC`; got {resp!r}"

        status, uid = _imap_append(sock, buf, "a3", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

        sel_status, _ = _imap_cmd(sock, buf, "a4", "SELECT INBOX", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        st_status, _ = _imap_cmd(
            sock, buf, "a5", f"UID STORE {uid} +FLAGS (\\Deleted)", deadline
        )
        assert st_status == "OK", f"UID STORE +FLAGS (\\Deleted) must succeed; got {st_status}"

        ex_status, ex_resp = _imap_cmd(sock, buf, "a6", f"UID EXPUNGE {uid}", deadline)
        assert ex_status == "OK", f"UID EXPUNGE must succeed; got {ex_status}: {ex_resp!r}"
        # Under ENABLE QRESYNC the removal is reported as `* VANISHED <uid>`
        # (RFC 7162 §3.2.10), carrying the UID directly — not the per-UID
        # `* <seq> EXPUNGE` fallback used pre-QRESYNC.
        assert re.search(rf"\bVANISHED\b[^\n]*\b{uid}\b", ex_resp), (
            f"want `* VANISHED {uid}` after ENABLE QRESYNC; got {ex_resp!r}"
        )
        assert not re.search(r"^\*\s+\d+\s+EXPUNGE\b", ex_resp, re.MULTILINE), (
            "QRESYNC-enabled session must NOT use the per-UID `* <seq> EXPUNGE` "
            f"fallback; got {ex_resp!r}"
        )
        sock.sendall(b"a7 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_qresync_select_inline_vanished_earlier(mail_bridge_mda):
    """The inline `SELECT (QRESYNC …)` fast-path (T5.4; proves **T3.2-b2**,
    imap-server.md § QRESYNC :199-216, RFC 7162 §3.2.5).

    The common-case QRESYNC reconnect: a client that last synced at modseq m0
    re-SELECTs supplying `(uidvalidity m0)` and the MDA delivers the expunged-
    since-m0 tombstones as `* VANISHED (EARLIER) <set>` **within the SELECT
    response itself** — not as a follow-up `UID FETCH (CHANGEDSINCE n VANISHED)`.
    This is the whole point of T3.2-b2's `SelectData.QResync` output extension,
    and it had only fork-wire (mock-session) coverage before.

    Flow: connection 1 APPENDs a message, SELECTs to capture the pre-expunge
    (UIDVALIDITY, HIGHESTMODSEQ=m0), then `\\Deleted` + `UID EXPUNGE`s it
    (advancing highestmodseq past m0 and writing the tombstone). Connection 2
    reconnects fresh, `ENABLE QRESYNC`, and `SELECT INBOX (QRESYNC (uv m0))` —
    the gate `last_uid_validity == uid_validity AND last_modseq <= highestmodseq`
    (imap-server.md:209) holds, so the fast-path fires and the SELECT response
    must carry `* VANISHED (EARLIER) <uid>` before its tagged OK. Capturing m0
    *immediately before* our own expunge makes our UID the only tombstone with
    modseq > m0, so the VANISHED set is exactly that single UID (no range, no
    interference from the session-scoped INBOX's other tombstones).
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: carol@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: QRESYNC inline reconnect target\r\n"
        b"Message-ID: <qresync-inline@mda.fauna.test>\r\n"
        b"\r\nExpunged after m0 so it surfaces as VANISHED (EARLIER) on reconnect.\r\n"
    )

    # Connection 1: APPEND, capture pre-expunge (UIDVALIDITY, m0), then expunge.
    sock1, buf1 = _imaps_connect(handle, deadline)
    with sock1:
        assert _imap_auth_plain(
            sock1, buf1, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"
        status, resp = _imap_cmd(sock1, buf1, "a2", "ENABLE QRESYNC", deadline)
        assert status == "OK" and "ENABLED QRESYNC" in resp, f"ENABLE QRESYNC; got {resp!r}"

        status, uid = _imap_append(sock1, buf1, "a3", "INBOX", message, deadline)
        assert status == "OK" and uid is not None, f"APPEND must succeed; got {status}"

        sel_status, sel_resp = _imap_cmd(sock1, buf1, "a4", "SELECT INBOX", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}: {sel_resp!r}"
        uv_m = re.search(r"UIDVALIDITY\s+(\d+)", sel_resp)
        m0_m = re.search(r"HIGHESTMODSEQ\s+(\d+)", sel_resp)
        assert uv_m and m0_m, f"SELECT must report UIDVALIDITY + HIGHESTMODSEQ; got {sel_resp!r}"
        uidvalidity, m0 = uv_m.group(1), m0_m.group(1)

        st_status, _ = _imap_cmd(
            sock1, buf1, "a5", f"UID STORE {uid} +FLAGS (\\Deleted)", deadline
        )
        assert st_status == "OK", f"UID STORE \\Deleted must succeed; got {st_status}"
        ex_status, _ = _imap_cmd(sock1, buf1, "a6", f"UID EXPUNGE {uid}", deadline)
        assert ex_status == "OK", f"UID EXPUNGE must succeed; got {ex_status}"
        sock1.sendall(b"a7 LOGOUT\r\n")

    # Connection 2: reconnect and SELECT (QRESYNC (uv m0)) → inline VANISHED.
    sock2, buf2 = _imaps_connect(handle, deadline)
    with sock2:
        assert _imap_auth_plain(
            sock2, buf2, "b1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed on reconnect"
        status, resp = _imap_cmd(sock2, buf2, "b2", "ENABLE QRESYNC", deadline)
        assert status == "OK" and "ENABLED QRESYNC" in resp, f"ENABLE QRESYNC; got {resp!r}"

        sel_status, sel_resp = _imap_cmd(
            sock2, buf2, "b3", f"SELECT INBOX (QRESYNC ({uidvalidity} {m0}))", deadline
        )
        assert sel_status == "OK", (
            f"SELECT (QRESYNC ...) must succeed; got {sel_status}: {sel_resp!r}"
        )
        # The expunged-since-m0 UID must arrive inline as VANISHED (EARLIER),
        # within the SELECT response (the T3.2-b2 fast-path), not via a later
        # UID FETCH. m0 captured right before our expunge ⇒ our UID is the sole
        # post-m0 tombstone ⇒ the set is exactly `<uid>`.
        assert re.search(rf"VANISHED \(EARLIER\)[^\n]*\b{uid}\b", sel_resp), (
            "SELECT (QRESYNC ...) must emit inline `* VANISHED (EARLIER) "
            f"{uid}` (T3.2-b2 fast-path); got {sel_resp!r}"
        )
        sock2.sendall(b"b4 LOGOUT\r\n")


def test_mda_imap_condstore_unchangedsince_returns_modified(mail_bridge_mda):
    """CONDSTORE `UNCHANGEDSINCE` precondition → `OK [MODIFIED <uid>]` with the
    flag NOT applied (T5.6; imap-server.md § Write surface :152, RFC 7162
    §3.1.3) — the conditional-STORE race-prevention semantic.

    Proves the precondition end-to-end on real binaries: MDA `store.go` carries
    `unchanged_since` to `fauna.bridges.store_flags` and partitions the reply's
    `Modified` set into an `OK [MODIFIED …]` status response; nest's
    `apply_store_flags` (`db/bridge_imap.rs`) does the modseq comparison and
    returns the stale UIDs. The fork wire test only echoes the parsed
    UNCHANGEDSINCE value back — it never exercises a real stale-modseq race.

    Flow: APPEND a message; a first `+FLAGS (\\Flagged)` STORE advances its
    modseq to `m1`; a second STORE `(UNCHANGEDSINCE <m0>) +FLAGS (\\Seen)` with
    `m0 < m1` must be rejected for that UID — the tagged response carries
    `[MODIFIED <uid>]` and a follow-up FETCH shows `\\Seen` was NOT applied
    while `\\Flagged` (the un-conditional change) was.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: dave@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: CONDSTORE UNCHANGEDSINCE precondition\r\n"
        b"Message-ID: <unchangedsince@mda.fauna.test>\r\n"
        b"\r\nA stale conditional STORE against this message must be rejected.\r\n"
    )
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"
        status, resp = _imap_cmd(sock, buf, "a2", "ENABLE CONDSTORE", deadline)
        assert status == "OK" and "ENABLED CONDSTORE" in resp, f"ENABLE CONDSTORE; got {resp!r}"

        status, uid = _imap_append(sock, buf, "a3", "INBOX", message, deadline)
        assert status == "OK" and uid is not None, f"APPEND must succeed; got {status}"

        sel_status, _ = _imap_cmd(sock, buf, "a4", "SELECT INBOX", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # Capture the message's current modseq (m0).
        f_status, f_resp = _imap_cmd(sock, buf, "a5", f"UID FETCH {uid} (MODSEQ)", deadline)
        assert f_status == "OK", f"UID FETCH (MODSEQ) must succeed; got {f_status}: {f_resp!r}"
        m0_m = re.search(r"MODSEQ\s+\((\d+)\)", f_resp)
        assert m0_m, f"FETCH (MODSEQ) must report the message modseq; got {f_resp!r}"
        m0 = int(m0_m.group(1))

        # An unconditional flag change advances the modseq past m0.
        st1_status, _ = _imap_cmd(
            sock, buf, "a6", f"UID STORE {uid} +FLAGS (\\Flagged)", deadline
        )
        assert st1_status == "OK", f"unconditional STORE \\Flagged must succeed; got {st1_status}"

        # A conditional STORE against the now-stale m0 must be rejected for this
        # UID: tagged `OK [MODIFIED <uid>]`, and \Seen must NOT be applied.
        st2_status, st2_resp = _imap_cmd(
            sock, buf, "a7", f"UID STORE {uid} (UNCHANGEDSINCE {m0}) +FLAGS (\\Seen)", deadline
        )
        assert st2_status == "OK", f"conditional STORE returns tagged OK; got {st2_status}: {st2_resp!r}"
        assert re.search(rf"\[MODIFIED[^\]]*\b{uid}\b", st2_resp), (
            "stale UNCHANGEDSINCE must report `[MODIFIED <uid>]` (RFC 7162 §3.1.3); "
            f"got {st2_resp!r}"
        )

        # \Seen must NOT have been applied (precondition failed); \Flagged must
        # remain (the unconditional change landed).
        v_status, v_resp = _imap_cmd(sock, buf, "a8", f"UID FETCH {uid} (FLAGS)", deadline)
        assert v_status == "OK", f"verification FETCH must succeed; got {v_status}"
        flags_line = next((ln for ln in v_resp.split("\n") if "FETCH" in ln and "FLAGS" in ln), "")
        assert "\\Flagged" in flags_line, (
            f"the unconditional \\Flagged change must have landed; got {flags_line!r}"
        )
        assert "\\Seen" not in flags_line, (
            "\\Seen must NOT be applied — the conditional STORE failed its "
            f"UNCHANGEDSINCE precondition; got {flags_line!r}"
        )
        sock.sendall(b"a9 LOGOUT\r\n")


@pytest.mark.feature("standard-mail-apps")
def test_mda_imap_search_body_axis_decrypts_index_hint(mail_bridge_mda):
    """Body-axis SEARCH must HPKE-open each message's index hint and match its
    tokens (imap-server.md § SEARCH; parity-verification work for mail-MDA SEARCH).

    The keystone the Go unit `search_test.go` can't reach: `bodySearch`
    (`internal/mda/imap/search.go`) opens every segment's `EncryptedIndexHint`
    — sealed by APPEND/MTA inbound via `EncryptToRecipient` (a
    `MailRecordEnvelope`) — using the AUTH'd actor's snapshot-derived leaf
    secret, *exactly the same primitive the body-FETCH path uses*
    (`MlsCapability.OpenMailRecord`, not the MSEK-AEAD `Decrypt`). The unit
    test injects a stub decryptor over plaintext index hints, so it proves the
    partition/intersection logic but never the real seal→open round-trip — the
    deployed-stack half this asserts. A distinctive body token APPENDed here
    must come back from `UID SEARCH BODY`; a guaranteed-absent token must not.

    Session-scoped INBOX: a nonce body token guarantees no collision with the
    sibling APPEND/IDLE messages, so we assert membership of the UID we
    APPENDed rather than an absolute result count.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    token = "zylophonewombatqx"  # nonce: present in no other test message
    message = (
        b"From: carol@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: body-axis search parity\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <search-body@mda.fauna.test>\r\n"
        b"\r\n"
        b"The " + token.encode() + b" ledger reconciles every quarter.\r\n"
    )
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "s1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"

        status, uid = _imap_append(sock, buf, "s2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return an [APPENDUID] uid"

        sock.sendall(b"s3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "s3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # Body-axis hit: the unique token must resolve to our UID. This is the
        # path that fails on the wrong decrypt primitive — the index hint is an
        # EncryptToRecipient envelope, openable only via OpenMailRecord.
        hit_status, hits = _imap_uid_search(sock, buf, "s4", f'BODY "{token}"', deadline)
        assert hit_status == "OK", (
            "UID SEARCH BODY must succeed by opening the index hint via "
            f"OpenMailRecord (not the MSEK-AEAD Decrypt); got {hit_status}"
        )
        assert uid in hits, (
            f"body token {token!r} must match the APPENDed UID {uid}; got {sorted(hits)}"
        )

        # Body-axis miss: a guaranteed-absent token must not return our UID.
        miss_status, misses = _imap_uid_search(
            sock, buf, "s5", 'BODY "absent5p9qx2token"', deadline
        )
        assert miss_status == "OK", f"UID SEARCH BODY (absent term) must succeed; got {miss_status}"
        assert uid not in misses, (
            f"absent token must not match the APPENDed UID {uid}; got {sorted(misses)}"
        )
        sock.sendall(b"s6 LOGOUT\r\n")


# ── Per-user spam scoring at the AUTH'd-MDA SELECT position ─────────────────
#
# These three prove the post-delivery, per-user spam-scoring loop-closer
# (`mail-spam.md` § Scoring placement — the AUTH'd-MDA-session position) end-to-end through the deployed
# stack. The Go unit `spam_score_test.go` stubs BOTH the model opener and the
# scorer, so the real path — `fetch_spam_model` (the stored model, sealed at
# rest to the actor's MSEK pubkey, byte-identical `wrapped_blob` shape as a mail
# body; or, for an untrained actor, the published baseline sealed on read)
# → `OpenMailRecord`-unseal under the session MLS capability → the shared cgo
# scorer (`WeightedBayesianMilliForModel`) → `Move` INBOX→Junk — is proven
# only here. This is exactly the `imap-server.md` § SEARCH "a stub-decryptor
# unit test cannot catch the OpenMailRecord-vs-Decrypt seal-shape" class the
# body-axis SEARCH test above guards for the SEARCH path; the sealed model
# rides the same seal.
#
# They use the dedicated `mda_spam_scoring` recipients (conftest), NOT the
# session-shared `mail_bridge_mda.recipient_*`, so the SELECT-time INBOX→Junk
# moves never perturb the shared INBOX the IDLE / QRESYNC / CONDSTORE / SEARCH
# tests in this file accumulate into.


@pytest.mark.feature("spam")
def test_mda_imap_spam_scoring_refiles_trained_spam_to_junk(mda_spam_scoring):
    """A message matching the actor's trained per-user model is re-filed
    INBOX→Junk by the SELECT-time scoring pass (`spam_score.go`).

    alice's seeded model maps `spam_token` strongly to spam at full confidence,
    so a message bearing it scores ~10.4k milli — well past the default
    `spam_folder` = 5 → 5000-milli threshold the bridge seeds from
    `DefaultSpamPolicyThresholds()`. We APPEND it to INBOX, then a read-write
    SELECT INBOX runs `scoreSelectedInbox` BEFORE the `select_mailbox` snapshot,
    so by the time SELECT returns the message is already gone from INBOX and
    present in Junk — as if it had been filed there at delivery. Asserting it
    landed in **Junk** (not merely vanished) proves a MOVE, not an expunge.
    """
    h = mda_spam_scoring
    deadline = time.monotonic() + 90.0
    token = h.spam_token
    message = (
        b"From: sender@external.test\r\n"
        b"To: " + h.alice.username.encode() + b"\r\n"
        b"Subject: trained spam re-file\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <spam-refile@mda.fauna.test>\r\n"
        b"\r\n"
        b"Act now: the " + token.encode() + b" offer expires today.\r\n"
    )
    sock, buf = _imaps_connect(h.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "p1", h.alice.username, h.alice.password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the trained recipient"

        status, uid = _imap_append(sock, buf, "p2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return an [APPENDUID] uid"

        # Read-write SELECT INBOX → scoreSelectedInbox runs first and re-files
        # the spam to Junk, so neither the SELECT snapshot nor our follow-up
        # search still sees it in INBOX.
        sock.sendall(b"p3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "p3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        s_status, inbox_hits = _imap_uid_search(sock, buf, "p4", f'BODY "{token}"', deadline)
        assert s_status == "OK", f"UID SEARCH BODY in INBOX must succeed; got {s_status}"
        assert not inbox_hits, (
            f"spam token {token!r} must be gone from INBOX after the SELECT-time "
            f"scoring pass re-filed it; still matched {sorted(inbox_hits)}"
        )

        # And it must be present in Junk — proving a MOVE (envelope + index hint
        # carried over), not an expunge.
        sock.sendall(b"p5 SELECT Junk\r\n")
        junk_status, _ = _imap_read_tagged(sock, buf, "p5", deadline)
        assert junk_status == "OK", f"SELECT Junk must succeed; got {junk_status}"
        j_status, junk_hits = _imap_uid_search(sock, buf, "p6", f'BODY "{token}"', deadline)
        assert j_status == "OK", f"UID SEARCH BODY in Junk must succeed; got {j_status}"
        assert junk_hits, (
            f"spam token {token!r} must be found in Junk — the scoring pass must "
            "have MOVED it there, not expunged it"
        )
        sock.sendall(b"p7 LOGOUT\r\n")


@pytest.mark.feature("spam")
def test_mda_imap_spam_scoring_keeps_ham_in_inbox_and_watermarks(mda_spam_scoring):
    """A message the per-user model scores as ham stays in INBOX, carries the
    `$FaunaSpamScored` watermark, and a re-SELECT does not re-move it
    (idempotency — `spam_score.go` skips already-watermarked messages).

    alice's `ham_token` maps strongly to ham, so the message scores ~90 milli,
    far below the 5000-milli `spam_folder` threshold. The scoring pass still
    *scored* it (so it stamps the watermark), but does not move it. The watermark
    is what makes the next SELECT a no-op over this message — proven by a second
    SELECT INBOX leaving it in place.
    """
    h = mda_spam_scoring
    deadline = time.monotonic() + 90.0
    token = h.ham_token
    message = (
        b"From: colleague@external.test\r\n"
        b"To: " + h.alice.username.encode() + b"\r\n"
        b"Subject: routine ham stays put\r\n"
        b"Date: Wed, 15 May 2026 12:05:00 +0000\r\n"
        b"Message-ID: <ham-stays@mda.fauna.test>\r\n"
        b"\r\n"
        b"Thanks for the " + token.encode() + b" notes from the meeting.\r\n"
    )
    sock, buf = _imaps_connect(h.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "h1", h.alice.username, h.alice.password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the trained recipient"

        status, uid = _imap_append(sock, buf, "h2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return an [APPENDUID] uid"

        sock.sendall(b"h3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "h3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # The ham message must still be in INBOX (scored below threshold).
        s_status, inbox_hits = _imap_uid_search(sock, buf, "h4", f'BODY "{token}"', deadline)
        assert s_status == "OK", f"UID SEARCH BODY in INBOX must succeed; got {s_status}"
        assert uid in inbox_hits, (
            f"ham token {token!r} must stay in INBOX (scored below the spam_folder "
            f"threshold); got {sorted(inbox_hits)}"
        )

        # …and must carry the scoring watermark (it WAS scored, just not moved).
        f_status, flags_text = _imap_cmd(sock, buf, "h5", f"UID FETCH {uid} (FLAGS)", deadline)
        assert f_status == "OK", f"UID FETCH FLAGS must succeed; got {f_status}"
        assert "$FaunaSpamScored" in flags_text, (
            "a scored ham message must carry the $FaunaSpamScored watermark so a "
            f"later SELECT skips it; FLAGS were {flags_text!r}"
        )

        # Idempotency: a second read-write SELECT INBOX re-runs the pass, which
        # skips the watermarked message — it stays put, not re-scored/re-moved.
        sock.sendall(b"h6 SELECT INBOX\r\n")
        sel2_status, _ = _imap_read_tagged(sock, buf, "h6", deadline)
        assert sel2_status == "OK", f"second SELECT INBOX must succeed; got {sel2_status}"
        s2_status, inbox_hits2 = _imap_uid_search(sock, buf, "h7", f'BODY "{token}"', deadline)
        assert s2_status == "OK", f"second UID SEARCH BODY must succeed; got {s2_status}"
        assert uid in inbox_hits2, (
            f"the watermarked ham message must remain in INBOX after a re-SELECT "
            f"(idempotent pass); got {sorted(inbox_hits2)}"
        )
        sock.sendall(b"h8 LOGOUT\r\n")


@pytest.mark.feature("spam")
def test_mda_imap_spam_scoring_is_per_actor_isolated(mda_spam_scoring):
    """The scoring pass uses the SELECT'ing actor's OWN model — an untrained
    actor's mail is never moved by another actor's training (`mail-spam.md`
    § Cross-actor isolation — the central confidentiality property verified
    in review, tracked internally).

    The exact bytes that re-file to Junk for the trained `alice`
    (`spam_token`-bearing) are delivered to the untrained `bob`. `fetch_spam_model`
    returns `None` for bob, so the pass cold-starts and exits before scanning his
    INBOX — the message stays. Same content, opposite outcome, differing only by
    whose per-actor model the fetch returns: that IS the isolation proof.
    """
    h = mda_spam_scoring
    deadline = time.monotonic() + 90.0
    token = h.spam_token  # alice's spam token — but bob is untrained
    message = (
        b"From: sender@external.test\r\n"
        b"To: " + h.bob.username.encode() + b"\r\n"
        b"Subject: same content, different recipient\r\n"
        b"Date: Wed, 15 May 2026 12:10:00 +0000\r\n"
        b"Message-ID: <spam-isolation@mda.fauna.test>\r\n"
        b"\r\n"
        b"Act now: the " + token.encode() + b" offer expires today.\r\n"
    )
    sock, buf = _imaps_connect(h.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "i1", h.bob.username, h.bob.password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the untrained recipient"

        status, uid = _imap_append(sock, buf, "i2", "INBOX", message, deadline)
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return an [APPENDUID] uid"

        sock.sendall(b"i3 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "i3", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # bob is untrained ⇒ cold start ⇒ no scoring ⇒ the spam-token message
        # alice would have Junked stays right where it was delivered.
        s_status, inbox_hits = _imap_uid_search(sock, buf, "i4", f'BODY "{token}"', deadline)
        assert s_status == "OK", f"UID SEARCH BODY in INBOX must succeed; got {s_status}"
        assert uid in inbox_hits, (
            f"the spam-token message must stay in the UNTRAINED actor's INBOX — "
            f"alice's training must not move bob's mail; got {sorted(inbox_hits)}"
        )
        sock.sendall(b"i5 LOGOUT\r\n")


@pytest.mark.feature("spam")
def test_mda_imap_spam_scoring_cold_start_inherits_baseline(mda_spam_scoring):
    """A FRESH (untrained) actor inherits the admin-published deployment baseline
    at read time and re-files baseline-spam to Junk — the cold-start CONSUME path
    (`mail-spam.md` § Cold start, Path 2 step 4).

    carol has no per-user model of her own. We seed the single-row deployment
    baseline (`spam_baseline` — the stand-in for the admin `publish_spam_baseline`
    aggregator, whose produce path is unit-tested in nest) mapping
    `baseline_spam_token` strongly to spam at full confidence, then deliver a
    message bearing that token to carol. Her read-write SELECT INBOX runs the
    scoring pass, whose `fetch_spam_model` now returns a *baseline-seeded* model
    (it returned `None` before Slice 5b ⇒ cold start ⇒ no move), so the message
    is re-filed INBOX→Junk — proving the baseline is consumed through the real
    `fetch_spam_model` read-merge → seal → `OpenMailRecord`-unseal → shared
    scorer → `Move` path, the half the Go unit tests stub.

    The baseline is seeded + cleared inside this test (keyed off `db_path`), so
    no other scoring test observes it; `baseline_spam_token` is distinct from
    alice's `spam_token`, so the isolation test stays valid regardless of order.
    """
    from conftest import _clear_spam_baseline, _seed_spam_baseline

    h = mda_spam_scoring
    deadline = time.monotonic() + 90.0
    token = h.baseline_spam_token

    # Publish a deployment baseline: balanced priors (neutral prior) + the
    # distinctive token only in spam, so a fresh actor inheriting it scores the
    # token well past spam_folder = 5 → 5000 milli (mirrors alice's seed).
    _seed_spam_baseline(
        db_path=h.db_path,
        ngrams={token: (110, 0)},
        spam_messages=110,
        ham_messages=110,
    )
    try:
        message = (
            b"From: sender@external.test\r\n"
            b"To: " + h.carol.username.encode() + b"\r\n"
            b"Subject: cold-start baseline re-file\r\n"
            b"Date: Wed, 15 May 2026 12:15:00 +0000\r\n"
            b"Message-ID: <baseline-coldstart@mda.fauna.test>\r\n"
            b"\r\n"
            b"Limited time: the " + token.encode() + b" deal is ending.\r\n"
        )
        sock, buf = _imaps_connect(h.mda, deadline)
        with sock:
            assert _imap_auth_plain(
                sock, buf, "c1", h.carol.username, h.carol.password, deadline
            ) == "OK", "AUTH PLAIN must succeed for the cold-start recipient"

            status, uid = _imap_append(sock, buf, "c2", "INBOX", message, deadline)
            assert status == "OK", f"APPEND must succeed; got {status}"
            assert uid is not None, "APPEND must return an [APPENDUID] uid"

            # Read-write SELECT INBOX → the scoring pass fetches carol's model
            # (now baseline-seeded) and re-files the baseline-spam to Junk.
            sock.sendall(b"c3 SELECT INBOX\r\n")
            sel_status, _ = _imap_read_tagged(sock, buf, "c3", deadline)
            assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

            s_status, inbox_hits = _imap_uid_search(sock, buf, "c4", f'BODY "{token}"', deadline)
            assert s_status == "OK", f"UID SEARCH BODY in INBOX must succeed; got {s_status}"
            assert not inbox_hits, (
                f"the baseline-spam token {token!r} must be gone from the cold-start "
                f"actor's INBOX — the deployment baseline must seed her scoring; "
                f"still matched {sorted(inbox_hits)}"
            )

            # And present in Junk — proving a MOVE (envelope carried over), not an
            # expunge, driven entirely by the inherited baseline.
            sock.sendall(b"c5 SELECT Junk\r\n")
            junk_status, _ = _imap_read_tagged(sock, buf, "c5", deadline)
            assert junk_status == "OK", f"SELECT Junk must succeed; got {junk_status}"
            j_status, junk_hits = _imap_uid_search(sock, buf, "c6", f'BODY "{token}"', deadline)
            assert j_status == "OK", f"UID SEARCH BODY in Junk must succeed; got {j_status}"
            assert junk_hits, (
                f"the baseline-spam token {token!r} must be found in Junk — a fresh "
                "actor inheriting the deployment baseline must re-file it, not expunge it"
            )
            sock.sendall(b"c7 LOGOUT\r\n")
    finally:
        _clear_spam_baseline(db_path=h.db_path)


def test_mda_imap_qresync_changedsince_vanished_fetch(mail_bridge_mda):
    """The non-inline `UID FETCH … (CHANGEDSINCE n VANISHED)` fallback (T5.5;
    imap-server.md § QRESYNC :199-216, RFC 7162 §3.2.5.1).

    The QRESYNC reconnect path *other* than the inline SELECT fast-path (T5.4):
    a client with the mailbox already SELECTed asks for messages expunged since
    modseq `m0` via `UID FETCH 1:* (FLAGS) (CHANGEDSINCE <m0> VANISHED)`, and the
    MDA emits `* VANISHED (EARLIER) <set>` for the tombstones. This is a
    **distinct production path** from T5.4 — `fetch.go`'s VANISHED branch calls
    `list_messages(since_modseq=m0)` → `FetchWriter.WriteVanishedEarlier` (the
    FAUNA-FORK seam) — and had only fork-wire (mock-session) coverage before.

    Flow: ENABLE QRESYNC; APPEND a message; SELECT and capture HIGHESTMODSEQ=m0
    *immediately before* expunging our own UID (so our UID is the sole post-m0
    tombstone — the VANISHED set is exactly `<uid>`, robust against the
    session-scoped INBOX's other tombstones); `\\Deleted` + `UID EXPUNGE` it;
    then the CHANGEDSINCE-VANISHED fetch must carry `* VANISHED (EARLIER) <uid>`.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0
    message = (
        b"From: erin@example.com\r\n"
        b"To: " + handle.recipient_username.encode() + b"\r\n"
        b"Subject: QRESYNC CHANGEDSINCE VANISHED target\r\n"
        b"Message-ID: <qresync-changedsince@mda.fauna.test>\r\n"
        b"\r\nExpunged after m0 so a CHANGEDSINCE-VANISHED fetch reports it.\r\n"
    )
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "a1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"
        status, resp = _imap_cmd(sock, buf, "a2", "ENABLE QRESYNC", deadline)
        assert status == "OK" and "ENABLED QRESYNC" in resp, f"ENABLE QRESYNC; got {resp!r}"

        status, uid = _imap_append(sock, buf, "a3", "INBOX", message, deadline)
        assert status == "OK" and uid is not None, f"APPEND must succeed; got {status}"

        # Capture HIGHESTMODSEQ=m0 *before* the expunge so our UID is the only
        # tombstone with modseq > m0.
        sel_status, sel_resp = _imap_cmd(sock, buf, "a4", "SELECT INBOX", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}: {sel_resp!r}"
        m0_m = re.search(r"HIGHESTMODSEQ\s+(\d+)", sel_resp)
        assert m0_m, f"SELECT must report HIGHESTMODSEQ; got {sel_resp!r}"
        m0 = int(m0_m.group(1))

        st_status, _ = _imap_cmd(
            sock, buf, "a5", f"UID STORE {uid} +FLAGS (\\Deleted)", deadline
        )
        assert st_status == "OK", f"UID STORE \\Deleted must succeed; got {st_status}"
        ex_status, _ = _imap_cmd(sock, buf, "a6", f"UID EXPUNGE {uid}", deadline)
        assert ex_status == "OK", f"UID EXPUNGE must succeed; got {ex_status}"

        # The non-inline QRESYNC fallback: a CHANGEDSINCE-VANISHED fetch reports
        # the since-m0 tombstone as `* VANISHED (EARLIER) <uid>`.
        f_status, f_resp = _imap_cmd(
            sock, buf, "a7", f"UID FETCH 1:* (FLAGS) (CHANGEDSINCE {m0} VANISHED)", deadline
        )
        assert f_status == "OK", (
            f"UID FETCH (CHANGEDSINCE VANISHED) must succeed; got {f_status}: {f_resp!r}"
        )
        assert re.search(rf"VANISHED \(EARLIER\)[^\n]*\b{uid}\b", f_resp), (
            "UID FETCH (CHANGEDSINCE n VANISHED) must emit `* VANISHED (EARLIER) "
            f"{uid}` for the since-m0 tombstone; got {f_resp!r}"
        )
        sock.sendall(b"a8 LOGOUT\r\n")


def test_mda_imap_idle_fetch_carries_modseq_under_condstore(mail_bridge_mda):
    """Under `ENABLE CONDSTORE`, the IDLE append push carries `MODSEQ` on its
    unsolicited FETCH (T5.7; imap-server.md § IDLE push wiring :236, RFC 7162
    §3.1.7).

    The plain IDLE test (`test_mda_imap_idle_exists_push`) deliberately does NOT
    assert MODSEQ — it idles without CONDSTORE. This one ENABLEs CONDSTORE on the
    idling session first, so when a second session APPENDs, session 1's
    unsolicited `* <seq> FETCH (UID … FLAGS …)` must additionally carry
    `MODSEQ (<n>)` — `idle.go`'s `flagsUpdate` routes through the FAUNA-FORK
    `WriteMessageFlagsModSeq` seam once `condStoreActive()`. The modseq is the
    real nest counter the APPEND advanced (`MailboxStateEvent::Append.modseq`),
    so n≥1 — not a mock value.

    Mirrors the EXISTS-push test's two-session structure; ENABLE must precede
    SELECT (RFC 5161: authenticated state). Count assertions stay relative — the
    INBOX is session-scoped + shared.
    """
    handle = mail_bridge_mda
    deadline = time.monotonic() + 60.0

    idle_sock, idle_buf = _imaps_connect(handle, deadline)
    with idle_sock:
        assert _imap_auth_plain(
            idle_sock, idle_buf, "i1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK"
        en_status, en_resp = _imap_cmd(idle_sock, idle_buf, "i2", "ENABLE CONDSTORE", deadline)
        assert en_status == "OK" and "ENABLED CONDSTORE" in en_resp, (
            f"ENABLE CONDSTORE must succeed; got {en_status}: {en_resp!r}"
        )
        sel_status, _ = _imap_cmd(idle_sock, idle_buf, "i3", "SELECT INBOX", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # Enter IDLE; the server acks with a `+ idling` continuation.
        idle_sock.sendall(b"i4 IDLE\r\n")
        cont = _recv_line(idle_sock, idle_buf, deadline)
        assert cont.startswith("+"), f"expected IDLE continuation, got {cont!r}"

        # Second session APPENDs a message for the same actor.
        push_sock, push_buf = _imaps_connect(handle, deadline)
        with push_sock:
            assert _imap_auth_plain(
                push_sock, push_buf, "p1", handle.recipient_username, handle.recipient_password, deadline
            ) == "OK"
            message = (
                b"From: frank@example.com\r\n"
                b"To: " + handle.recipient_username.encode() + b"\r\n"
                b"Subject: CONDSTORE IDLE push\r\n\r\nWake the CONDSTORE idler.\r\n"
            )
            status, _ = _imap_append(push_sock, push_buf, "p2", "INBOX", message, deadline)
            assert status == "OK", f"second-session APPEND must succeed; got {status}"
            push_sock.sendall(b"p3 LOGOUT\r\n")

        # Session 1, idling under CONDSTORE, must receive `* EXISTS` AND an
        # unsolicited FETCH carrying `MODSEQ (<n>)`.
        push_deadline = time.monotonic() + 20.0
        pushes = _imap_read_until_fetch_modseq(idle_sock, idle_buf, push_deadline)
        joined = "\n".join(pushes)
        assert re.search(r"^\*\s+\d+\s+EXISTS\b", joined, re.MULTILINE), (
            f"IDLE append push must still report `* <n> EXISTS`; got {pushes!r}"
        )
        fm = re.search(r"FETCH\b[^\n]*MODSEQ\s+\((\d+)\)", joined)
        assert fm and int(fm.group(1)) >= 1, (
            "under ENABLE CONDSTORE the IDLE append push's unsolicited FETCH must "
            f"carry `MODSEQ (<n>)` with a real counter ≥1; got {pushes!r}"
        )

        idle_sock.sendall(b"DONE\r\n")
        done_status, _ = _imap_read_tagged(idle_sock, idle_buf, "i4", deadline)
        assert done_status == "OK", f"IDLE DONE must complete OK; got {done_status}"
        idle_sock.sendall(b"i5 LOGOUT\r\n")


def _idle_until_server_bye(handle, max_wait: float):
    """Open a fresh IMAPS session, AUTH + SELECT INBOX + IDLE, and wait for the
    MDA to *unilaterally* end the IDLE with an untagged `* BYE` (the per-server
    idle timeout, RFC 2177 §3). Never sends `DONE` — the whole point is the
    server-initiated teardown. Returns the elapsed seconds from `+ idling` to
    `* BYE`, or None if no BYE arrived within `max_wait` (the connection is still
    on the old long timeout — the hot-apply hasn't bound on it yet → the caller
    retries a fresh connection)."""
    setup_deadline = time.monotonic() + max_wait + 15.0
    sock, buf = _imaps_connect(handle, setup_deadline)
    try:
        assert (
            _imap_auth_plain(
                sock, buf, "t1", handle.recipient_username, handle.recipient_password, setup_deadline
            )
            == "OK"
        )
        sock.sendall(b"t2 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(sock, buf, "t2", setup_deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"
        sock.sendall(b"t3 IDLE\r\n")
        cont = _recv_line(sock, buf, setup_deadline)
        assert cont.startswith("+"), f"expected IDLE continuation, got {cont!r}"
        idle_start = time.monotonic()
        bye_deadline = idle_start + max_wait
        while True:
            try:
                line = _recv_line(sock, buf, bye_deadline)
            except (socket.timeout, AssertionError, OSError):
                # The read deadline passed with the connection still open (old
                # long timeout still in force on this connection), or the peer
                # closed without a parsable `* BYE` line: this attempt didn't
                # observe the server-initiated timeout.
                return None
            if line.startswith("* BYE"):
                return time.monotonic() - idle_start
            # Any other unsolicited line on an idle, empty INBOX is unexpected;
            # keep reading until the BYE or the deadline.
    finally:
        try:
            sock.close()
        except OSError:
            pass


@pytest.mark.feature("admin-mail-policy")
def test_mda_imap_idle_timeout_hot_reloads_without_restart(mail_bridge_mda, nest_instance):
    """A hot-reloaded `imap.idle_timeout_seconds` makes the MDA unilaterally end
    an idle IMAP connection at the NEW (short) timeout, with no bridge restart —
    the full-stack proof of config_changed hot-reload (mail-bridge hot-reload work,
    Slice 1; mail-bridge-lifecycle.md § Running — hot-reload mandatory).

    Flow on real binaries: admin WS-RPC `put_imap_policy{idle_timeout_secs: 3}`
    → nest writes the `mail_imap_policy` override + fans
    `fauna.bridges.config_changed{reason: imap_policy}`
    (`bridge_routing_handlers.rs` put_imap_policy_handler) → the Go bridge's
    `wsrpc.ConfigReloader` re-fetches the whole snapshot off the reader goroutine
    → `imap.Backend.ApplyConfig` hot-swaps the live IDLE timeout (an `atomic.Int64`)
    → the NEXT IMAP connection's `Session.IdleTimeout()` reads it and the MDA
    ends that idle session with `* BYE` ~3 s in (RFC 2177 §3 server-initiated end).
    The bridge process is never restarted (same PID throughout — hot-reload, not
    respawn).

    This is the *only* proof the knob is observable end-to-end. The Go in-process
    composition test (`config_changed_composition_test.go`) asserts the swapped
    value but not the wire effect; and before the vendored go-imap fork's
    `handleIdle` drove the IDLE read deadline from the configured timeout, the
    knob disconnected nobody — `Session.Idle` returned on timeout but `handleIdle`
    stayed blocked on the client's `DONE`, so the hard-coded 35-min
    `idleReadTimeout` was the real idle-disconnect. That gap was caught by this
    test and fixed in the same commit.

    The default is 1740 s (29 min); the override is **restored in `finally`** —
    `mail_bridge_mda` is session-scoped, and a stray 3 s timeout would `* BYE`
    every later IDLE test's connection mid-flight.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mda
    admin = nest_instance["admin"]
    SHORT = 3

    def _set_idle_timeout(secs: int) -> None:
        ws = WsRpcAdminClient(
            nest_instance["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with ws:
            ws.call("fauna.bridges.put_imap_policy", {"idle_timeout_secs": secs})

    pid_before = handle.proc.pid
    _set_idle_timeout(SHORT)
    try:
        # config_changed propagates async, and the new timeout binds only on the
        # NEXT connection. Poll fresh IDLE sessions until one is BYE'd near SHORT
        # (an earlier attempt may pre-date the hot-apply → no BYE → retry).
        overall_deadline = time.monotonic() + 50.0
        elapsed = None
        while time.monotonic() < overall_deadline:
            elapsed = _idle_until_server_bye(handle, max_wait=SHORT + 5.0)
            if elapsed is not None:
                break
            time.sleep(0.5)
        assert elapsed is not None, (
            "MDA never ended an idle IMAP session at the hot-reloaded "
            f"{SHORT}s timeout (no unilateral `* BYE` within 50s) — the "
            "config_changed hot-apply or the server-initiated IDLE teardown is broken"
        )
        # Driven by the NEW timeout: ~SHORT, and far below the 1740s default.
        assert SHORT - 2.0 <= elapsed <= SHORT + 5.0, (
            f"IDLE `* BYE` fired at {elapsed:.1f}s, expected ~{SHORT}s "
            "(the configured idle timeout)"
        )
        # No restart: the same live process served throughout (hot-reload, not
        # respawn — the product invariant the whole track exists to uphold).
        assert handle.proc.poll() is None, "bridge process exited during the test"
        assert handle.proc.pid == pid_before, "bridge process was restarted (PID changed)"
    finally:
        _set_idle_timeout(1740)


def _caldav_request(handle, method: str, path: str, body=None, extra_headers=None):
    """One authenticated CalDAV request over the MDA's implicit-TLS HTTPS port.

    Basic auth with (recipient_username, recipient_password) — the bridge
    AEAD-unwraps the wrapped-MSEK blob (`caldav/auth.go`), so unwrap-success IS
    the auth signal, same as IMAP. CERT_NONE: the fixture's self-signed cert
    exercises the mail path, not PKI. A fresh connection per call keeps each
    request independent of HTTP keep-alive state. Returns (status, text)."""
    import http.client

    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    conn = http.client.HTTPSConnection(
        "127.0.0.1", handle.caldav_port, context=ctx, timeout=20.0,
    )
    try:
        creds = f"{handle.recipient_username}:{handle.recipient_password}".encode()
        headers = {"Authorization": "Basic " + base64.b64encode(creds).decode()}
        if extra_headers:
            headers.update(extra_headers)
        conn.request(method, path, body=body, headers=headers)
        resp = conn.getresponse()
        return resp.status, resp.read().decode("utf-8", errors="replace")
    finally:
        conn.close()


@pytest.mark.feature("calendar-in-standard-apps")
def test_mda_caldav_put_report_roundtrip(mail_bridge_mda):
    """CalDAV PUT a VEVENT, then REPORT it back decrypted (mail-MDA body-decrypt work,
    Task E; caldav-server.md § Write/Read surface).

    Shares the IMAP body-decrypt key material: the bridge HPKE-seals the PUT
    VEVENT to the actor's registered MSEK-derived MLS pubkey (`caldav/put.go`),
    and REPORT calendar-query opens it via `MlsCapability.OpenMailRecord` with
    the snapshot's leaf secret (`caldav/report.go`). PROPFIND on the home set
    first lazily provisions the Personal calendar (§ Lazy "Personal" calendar)
    and surfaces its collection path; the PUT VEVENT's UID + SUMMARY must come
    back in the REPORT multistatus.
    """
    handle = mail_bridge_mda
    user = handle.recipient_username
    home = f"/caldav/{user}/"

    # 1. PROPFIND home set → lazy-provision Personal + discover its hex path.
    propfind_body = (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<propfind xmlns="DAV:"><prop><displayname/><resourcetype/></prop></propfind>'
    )
    status, body = _caldav_request(
        handle, "PROPFIND", home, propfind_body,
        {"Depth": "1", "Content-Type": "application/xml; charset=utf-8"},
    )
    assert status == 207, f"PROPFIND home set must be 207 Multi-Status; got {status}: {body!r}"
    m = re.search(r"/caldav/[^<>\s/]+/([0-9a-fA-F]{64})/", body)
    assert m, f"PROPFIND must surface a calendar collection path; got {body!r}"
    cal_path = f"{home}{m.group(1)}/"

    # 2. PUT a minimal-but-valid VEVENT (UID/DTSTAMP/DTSTART required by
    #    caldav/put.go's validateEventComponent).
    uid = "mda-e2e-put-report-uid-1"
    summary = "MDA e2e PUT/REPORT round-trip"
    vevent = (
        "BEGIN:VCALENDAR\r\n"
        "VERSION:2.0\r\n"
        "PRODID:-//fauna//mda-e2e//EN\r\n"
        "BEGIN:VEVENT\r\n"
        f"UID:{uid}\r\n"
        "DTSTAMP:20260516T120000Z\r\n"
        "DTSTART:20260520T100000Z\r\n"
        "DTEND:20260520T110000Z\r\n"
        f"SUMMARY:{summary}\r\n"
        "END:VEVENT\r\n"
        "END:VCALENDAR\r\n"
    )
    status, body = _caldav_request(
        handle, "PUT", f"{cal_path}event.ics", vevent,
        {"Content-Type": "text/calendar; charset=utf-8"},
    )
    assert status in (200, 201, 204), f"PUT VEVENT must succeed; got {status}: {body!r}"

    # 3. REPORT calendar-query → the VEVENT must round-trip (body decrypt).
    report_body = (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">\n'
        "  <D:prop>\n    <D:getetag/>\n    <C:calendar-data/>\n  </D:prop>\n"
        "  <C:filter>\n    <C:comp-filter name=\"VCALENDAR\">\n"
        "      <C:comp-filter name=\"VEVENT\"/>\n"
        "    </C:comp-filter>\n  </C:filter>\n</C:calendar-query>"
    )
    status, body = _caldav_request(
        handle, "REPORT", cal_path, report_body,
        {"Depth": "1", "Content-Type": "application/xml; charset=utf-8"},
    )
    assert status == 207, f"REPORT calendar-query must be 207 Multi-Status; got {status}: {body!r}"
    assert uid in body, (
        "REPORT calendar-data must contain the PUT VEVENT's UID "
        f"(proves PUT-seal → REPORT-decrypt round-trips); got {body!r}"
    )
    assert summary in body, f"REPORT must return the VEVENT SUMMARY; got {body!r}"


@pytest.mark.feature("mailbox-import")
def test_mda_append_populates_dedup_index_so_a_later_import_skips(
    mail_bridge_mda, nest_instance
):
    """The **Go** MDA's dedup key reaches nest over a real WS-RPC connection
    (mail-dedup live-provisioning work, B5).

    B2 pinned the key's *value* (Go unit test `TestMailDedupKeyBinding` across
    the FFI) and the nest's *storage* (Rust handler tests), but neither proves
    the Go MDA actually puts `dedup_key` on the wire. Only a real bridge + real
    nest can. The flow this asserts, end to end::

        IMAP APPEND
          → mda/imap/append.go      dedupKeys := mailfauna.MailDedupKeys(body)   [shared Rust, UniFFI]
          → wsrpc/methods.go:1639   cbor `dedup_key` on fauna.bridges.append
          → bridge_imap_handlers.rs  insert_dedup_key(&target, …)          [recipient actor]
          → bridge_import_handlers.rs dedup_hit(actor, key, None) → true   [no envelope key on the item: agrees]
          → ImportMessageOutcome::Skipped{reason: "dedup"}

    `msgid:v1:b5-dedup@example.com` is the shared-Rust key for this message —
    `normalize` trims, strips one `<>` pair and case-folds
    (`libs/fauna-mail/src/dedup_key.rs::normalize_message_id`), so the
    deliberately mixed-case `<B5-Dedup@Example.COM>` must fold to it. The Go
    binding's golden vector (`internal/mailfauna/mailfauna_test.go:157`) pins
    the same transformation.

    Two controls make the skip attributable:
      * a *different* Message-ID imports normally → we aren't skipping blindly;
      * `skip_dedup=true` imports the duplicate → the skip came from the dedup
        check, not from some unrelated rejection.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.mail_envelope_key import envelope_key

    handle = mail_bridge_mda
    recipient = handle.recipient_actor
    deadline = time.monotonic() + 60.0

    def _message(message_id: str) -> bytes:
        return (
            b"From: alice@example.com\r\n"
            b"To: " + handle.recipient_username.encode() + b"\r\n"
            b"Subject: dedup population through the real Go MDA\r\n"
            b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
            b"Message-ID: <" + message_id.encode() + b">\r\n"
            b"\r\n"
            b"B5: the APPEND that seeds actor_message_dedup.\r\n"
        )

    appended_key = "msgid:v1:b5-dedup@example.com"

    # 1. Deliver through the real Go MDA over real IMAPS. The bridge computes
    #    the dedup key itself; nothing test-side hands it one.
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "b1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"
        status, uid = _imap_append(
            sock, buf, "b2", "INBOX", _message("B5-Dedup@Example.COM"), deadline
        )
        assert status == "OK", f"APPEND must succeed; got {status}"
        assert uid is not None, "APPEND must return a UIDPLUS [APPENDUID] uid"

    # 2. As the *recipient actor* (User class, caller-scoped), try to import a
    #    message whose dedup key is the one the Go MDA just recorded.
    ws = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=recipient["actor_id_bytes"],
        signing_key=bytes(recipient["signing_key"]),
    )
    with ws:
        session_id = ws.call(
            "fauna.bridges.start_import_session",
            {"source_descriptor": "imap://legacy.example.org/alice", "total_count": 2},
        )["session_id"]

        def _import(dedup_key: str, *, source_uid: int, skip_dedup: bool = False) -> dict:
            body = _message("whatever@example.org")
            return ws.call(
                "fauna.bridges.import_message",
                {
                    "session_id": session_id,
                    "message": {
                        "mailbox": "INBOX",
                        "flags": [],
                        "body": body,
                        "timestamp": 1_783_000_000,
                        "body_size": len(body),
                        "sender_domain": "example.com",
                        "source_uid": source_uid,
                        "source_uid_validity": 7,
                        "dedup_key": dedup_key,
                        # The Message-ID is not an envelope field, so every
                        # `_message(...)` shares the appended copy's envelope
                        # key: a `dedup_key` hit then agrees, as a re-import of
                        # the same message would.
                        "envelope_key": envelope_key(body),
                    },
                    "skip_dedup": skip_dedup,
                },
            )

        skipped = _import(appended_key, source_uid=1)
        assert skipped["outcome"] == {"outcome": "skipped", "reason": "dedup"}, (
            "the Go MDA's APPEND must have written actor_message_dedup["
            f"{appended_key!r}] for the recipient — got outcome="
            f"{skipped['outcome']!r}. A plain `imported` here means the Go side "
            "never put `dedup_key` on the wire, or nest never stored it."
        )

        # Control A: an unrelated key still imports.
        fresh = _import("msgid:v1:b5-never-appended@example.com", source_uid=2)
        assert fresh["outcome"]["outcome"] == "imported", (
            f"a never-seen dedup key must import; got {fresh['outcome']!r}"
        )

        # Control B: the wizard's "Import duplicates anyway" bypasses the check,
        # so the skip above was the dedup gate and nothing else.
        forced = _import(appended_key, source_uid=3, skip_dedup=True)
        assert forced["outcome"]["outcome"] == "imported", (
            f"skip_dedup=true must import the duplicate; got {forced['outcome']!r}"
        )


@pytest.mark.feature("mail-server")
def test_mda_graceful_shutdown_byes_and_exits_clean(disposable_mda_bridge):
    """SIGTERM mid-IMAP-session drains gracefully (mail-bridge-lifecycle.md
    § Shutting down; T2.6, MDA half).

    With one IMAPS connection already open (parked at the greeting), SIGTERM
    the bridge. The MDA must:
      1. stop accepting new connections on 993 (a fresh connect is refused);
      2. send an untagged `* BYE` on the still-open connection so the session
         leaves promptly (RFC 9051 §7.1.5) rather than holding the full grace;
      3. drain cleanly and exit 0 (within `mail.bridge.shutdown_grace_seconds`,
         default 30 s).

    The `* BYE`-on-drain fires for *any* open connection regardless of auth
    state, so this proves the real cross-binary SIGTERM → BYE → exit-0 path at
    the pre-auth greeting level — no IMAP `LOGIN` (which needs the wrapped-MLS
    keychain provisioning surface deferred to the mail-MDA body-decrypt work). The fork's
    BYE / drain / force-close mechanics are unit-tested in
    `third_party/go-imap/imapserver/shutdown_fauna_test.go`; here we exercise
    the real signal path against a nest-provisioned bridge.

    Uses a *throwaway* `disposable_mda_bridge` (its own keypair + enrollment +
    ephemeral ports, reusing the nest-global state from `mail_bridge_mda`
    read-only) and SIGTERMs *it* (`handle.proc.terminate()`), so the
    destructive shutdown never strands the session-scoped `mail_bridge_mda`
    that other MDA tests share — mirroring the MTA half's `disposable_mta_bridge`.
    """
    handle = disposable_mda_bridge
    deadline = time.monotonic() + 40.0

    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE  # exercises the mail path, not PKI

    raw = socket.create_connection(("127.0.0.1", handle.imaps_port), timeout=15.0)
    with ctx.wrap_socket(raw, server_hostname=handle.domain) as sock:
        buf = bytearray()
        # Greeting: `* OK [CAPABILITY ...] IMAP server ready`. Reading it
        # ensures the connection is fully registered and parked before SIGTERM.
        greeting = _recv_line(sock, buf, deadline)
        assert greeting.startswith("* OK"), f"unexpected IMAPS greeting: {greeting!r}"

        # In flight (parked, pre-auth) when the shutdown signal arrives.
        handle.proc.terminate()

        # Wait until 993 stops accepting — the drain closes the listener before
        # waking parked connections, so a refused fresh connect means draining
        # is in effect.
        accept_deadline = time.monotonic() + 20.0
        while True:
            probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probe.settimeout(1.0)
            try:
                probe.connect(("127.0.0.1", handle.imaps_port))
                probe.close()
            except OSError:
                break
            if time.monotonic() > accept_deadline:
                raise AssertionError(
                    "IMAPS 993 still accepting after SIGTERM; drain did not begin "
                    f"(see {handle.log_file})"
                )
            time.sleep(0.1)

        # The parked connection is woken and BYE'd (untagged `* BYE`).
        line = _recv_line(sock, buf, deadline)
        assert line.startswith("* BYE"), (
            f"expected untagged `* BYE` on graceful shutdown, got {line!r} "
            f"(see {handle.log_file})"
        )

    # The connection left promptly via BYE → the drain completes cleanly → the
    # process exits 0.
    rc = handle.proc.wait(timeout=40)
    assert rc == 0, f"clean drain should exit 0, got exit code {rc}; see {handle.log_file}"

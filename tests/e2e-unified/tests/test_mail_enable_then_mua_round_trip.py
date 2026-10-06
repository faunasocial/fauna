"""Client-driven enable-mail → real MUA round-trip (the user's own scenario).

Every existing mail test proves ONE half in isolation:

  - `test_mail_credentials.py` drives the client's "Enable mail" toggle but
    asserts only UI state (a credential row, MUA-instructions visible) — never
    that mail actually works.
  - `test_mail_inbound_to_imap.py` proves a real inbound-SMTP → IMAP-read
    round-trip, but provisions the recipient SERVER-SIDE
    (`_provision_msek_recipient`), bypassing the client entirely.

No single test started from a claimed nest, had the **client enable mail
through its own UI**, then connected a **normal MUA** (Thunderbird-style IMAP
`AUTH PLAIN`) and proved a message round-trips. That gap is exactly what a user
hits manually: "I enabled mail in the client — does a real mail app actually
work against it?" This is the green test for that question (the
`green-test-or-it-doesnt-work` principle): the credential the MUA authenticates
with is the one the **client minted**, not one the test sealed server-side.

The actor here is the **nest admin** — the realistic single-user deployment
where the human who claims the nest is its first (and config-owning) user. That
also regression-guards the bug this test first caught: the three enable-mail
`fauna.bridges.provision_*` kinds were gated `User`-only, so an *admin* enabling
their own mail hit `fauna.bridges.permission_denied` and the toggle snapped back
("can't check the Enable mail box"). Fixed by widening those gates to
`User | Admin` (self-scoped writes), matching the sibling
`provision_recipient_mls_pubkey` gate.

Production data flow asserted end-to-end on real binaries:

  the nest admin (logged into the linux app) opens mail-settings → toggles
  "Enable mail" → picks PLAIN → submits a password →
  `MailSettingsMachine::EnableMail` provisions the recipient MLS pubkey +
  wrapped-MSEK (sealed under the password via Argon2id) + MLS snapshot +
  submission token over WS-RPC →
  an external MX delivers to `admin@<domain>` over port-25 STARTTLS → the MTA
  seals to the client-provisioned recipient pubkey + ingests →
  a normal MUA does IMAPS `AUTH PLAIN` with (admin@<domain>, the same password)
  → the bridge AEAD-unwraps the client-minted wrapped-MSEK blob (Argon2id) as
  the auth signal → `SELECT INBOX` → `FETCH BODY[]` → `OpenMailRecord` decrypts
  with the client-provisioned snapshot's leaf secret → byte-faithful body.

Only tier_3 catches this: the wrapped-MSEK + snapshot the *client* sealed must
open under the *bridge's* Argon2id AUTH + `OpenMailRecord` read path — a
client↔bridge seal/open contract that no stub or in-process test exercises.

A DEDICATED nest (`dedicated_mail_nest`) is used rather than the shared
`nest_instance`: the nest admin must be the only actor enabling mail there and
must start with mail disabled. On the shared nest other tests' users already
share the box.

The test is parametrized over both inbound MUA credential kinds:

  - **PLAIN** (`AUTH PLAIN`, the username+password a Thunderbird-style MUA uses)
    — the client mints a password; the bridge AEAD-unwraps the wrapped-MSEK with
    Argon2id.
  - **OAUTHBEARER** (SASL bearer-token) — the client mints a one-time bearer
    token (revealed once in `mail-add-credential-token-display`); the MUA does
    SASL OAUTHBEARER and the bridge unwraps with HKDF
    (`mailfauna.KdfKindHkdf`, imap/auth.go:156).

Both kinds seal/unwrap the SAME `default` credential slot the bridge hardcodes,
so they run against separate dedicated nests (the function-scoped
`dedicated_mail_nest`). The outbound-submission leg is a deliberate follow-up
(tracked internally).

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP + IMAP wire, real
  client UI, real client-side seal + bridge-side AUTH/decrypt.
"""

import time

import pytest

from helpers.mail_dedicated_nest import (
    alias_admin_to_address as _alias_admin_to_address,
    dedicated_node_url as _dedicated_node_url,
    login_as_nest_admin as _login_as_nest_admin,
)
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _connect_submission_tls,
    _imap_auth_oauthbearer,
    _imap_auth_plain,
    _imap_cmd,
    _imap_seq_fetch_body,
    _imaps_connect,
    _smtp_auth_oauthbearer,
    _smtp_auth_plain,
    _wait_for_tagged,
    dkim_signature_tag,
)

# Drives the client's own "Enable mail" settings UI. linux, web, windows and both
# apple apps implement the mail-settings page (the windows MailSettingsPanel
# renders every enable / credential / one-time-token id — both the PLAIN and the
# OAUTHBEARER enable paths, proven by the dedicated_mail_nest windows coverage;
# macos joined once the apple dedicated-nest mint was fixed, and ios rides the same
# shared FaunaKit surface — see test_addressbook). android doesn't yet. Scope to the
# implementing clients so --client deselects it on the others instead of failing on
# the missing enable toggle.
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]

# The admin-inject preamble (`_login_as_nest_admin`, `_dedicated_node_url`,
# `_alias_admin_to_address`) is shared with the Sent-feed round-trip — see
# `helpers/mail_dedicated_nest.py` (imported above). The bare local part is
# `mail_dedicated_nest.ADMIN_LOCAL_PART` (import it directly; it is NOT re-exported
# here as `rt._ADMIN_LOCAL_PART`).
# The PLAIN credential the client mints and the MUA authenticates with.
_MUA_PASSWORD = "round-trip-plain-pw-1"


def _select_inbox_count(sock, buf, tag: str, deadline: float) -> int:
    """SELECT INBOX → the `* <n> EXISTS` count (asserting SELECT OK)."""
    status, resp = _imap_cmd(sock, buf, tag, "SELECT INBOX", deadline)
    assert status == "OK", f"SELECT INBOX must succeed; got {status}: {resp!r}"
    for line in resp.split("\n"):
        parts = line.split()
        if len(parts) >= 3 and parts[0] == "*" and parts[2].upper() == "EXISTS":
            return int(parts[1])
    raise AssertionError(f"SELECT INBOX reported no EXISTS line; got {resp!r}")


def _client_enable_mail(app, cred_kind: str) -> str:
    """Drive the client's "Enable mail" gesture for `cred_kind` and return the
    secret a MUA authenticates with (the client-minted PLAIN password, or the
    one-time OAUTHBEARER token the client reveals)."""
    if cred_kind == "plain":
        app.mail_settings.enable_mail_plain(_MUA_PASSWORD)
        return _MUA_PASSWORD
    token = app.mail_settings.enable_mail_oauthbearer()
    assert token, (
        "enabling mail (OAUTHBEARER) must mint + reveal a one-time bearer token; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    return token


def _mua_authenticate(sock, buf, cred_kind: str, addr: str, secret: str, deadline: float) -> str:
    """Authenticate the MUA with the kind-appropriate SASL mechanism, returning
    the tagged status. PLAIN AEAD-unwraps with Argon2id; OAUTHBEARER with HKDF —
    both against the same `default` credential the client just sealed."""
    if cred_kind == "plain":
        return _imap_auth_plain(sock, buf, "c1", addr, secret, deadline)
    return _imap_auth_oauthbearer(sock, buf, "c1", addr, secret, deadline)


def _mua_submit_authenticate(conn, cred_kind: str, addr: str, secret: str, deadline: float) -> None:
    """Authenticate an SMTP *submission* session (port 465) with the kind-
    appropriate SASL mechanism. Both helpers assert the `235` success reply and
    raise on rejection — unwrapping the SAME `default` wrapped *submission token*
    the client sealed at EnableMail (PLAIN → Argon2id, OAUTHBEARER → HKDF), the
    submission-side twin of `_mua_authenticate`'s IMAP wrapped-MSEK unwrap."""
    if cred_kind == "plain":
        _smtp_auth_plain(conn, addr, secret, deadline)
    else:
        _smtp_auth_oauthbearer(conn, addr, secret, deadline)


@pytest.mark.parametrize("cred_kind", ["plain", "oauthbearer"])
@pytest.mark.feature("standard-mail-apps")
def test_client_enabled_mail_round_trips_through_a_normal_mua(
    app, dedicated_mail_nest, cred_kind, request
):
    """Enable mail in the client (as the nest admin, `cred_kind` credential), then
    receive a real inbound message through a normal IMAP MUA authenticating with
    the client-minted secret.

    The end-to-end proof that the client's own "Enable mail" produces a working
    mailbox a normal MUA can read — for both inbound auth mechanisms a real MUA
    offers (Thunderbird-style AUTH PLAIN, and SASL OAUTHBEARER bearer-token).
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    _login_as_nest_admin(app, nest, _dedicated_node_url(app, handle, request))
    admin_addr = _alias_admin_to_address(nest, domain)

    # ── Client UI: enable mail with the parametrized credential (the user's
    # gesture). The returned secret is what the MUA will authenticate with.
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    mua_secret = _client_enable_mail(app, cred_kind)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"enabling mail ({cred_kind}) must mint the first credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # The page itself must report mail as ENABLED — not stay stuck on "Mail is
    # disabled". This regression-guards the user-reported freeze: when the
    # EnableMail WS-RPC dispatch fails (RpcDisconnected against a remote nest),
    # `snap.enabled` stays false, the status indicator never leaves "Mail is
    # disabled", and a page error surfaces. A healthy enable flips the status to
    # "All up to date" with no error.
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "the mail-settings status indicator must report mail enabled after the "
        f"toggle; got status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    enable_error = app.mail_settings.page_error_text(timeout=2.0)
    assert enable_error == "", (
        "enabling mail must not surface a page error (e.g. an RPC-disconnect "
        f"storm); got error={enable_error!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    # This test IS the enable transition, so this is the step under test's own
    # deployment consequence, not incidental setup.
    handle.rebind_after_enable()

    # ── Inbound: a real external MX delivers to the client-enabled mailbox over
    # port-25 STARTTLS. The MTA seals to the recipient pubkey the CLIENT just
    # provisioned; the 250 on `.` follows the WS-RPC ingest.
    nonce = f"clientenable{cred_kind}{int(time.time() * 1000)}qx"
    message_id = f"<{nonce}@external.test>"
    body_lines = [
        "From: External Sender <sender@external.test>",
        f"To: {admin_addr}",
        f"Subject: client-enabled {cred_kind} round-trip proof",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:10:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must round-trip through the client-minted mailbox.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()

    deadline = time.monotonic() + 40.0
    with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{admin_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # ── Read: a normal MUA does IMAPS auth with the CLIENT-MINTED secret, then
    # SELECT + FETCH + decrypt. Auth succeeds only by AEAD-unwrapping the
    # wrapped-MSEK blob the client sealed under this secret.
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _mua_authenticate(sock, buf, cred_kind, admin_addr, mua_secret, deadline) == "OK", (
            f"IMAP AUTH ({cred_kind}) must succeed with the client-minted secret — "
            "the bridge unwraps the wrapped-MSEK blob the client sealed under it"
        )

        # Poll SELECT briefly for the delivered message to surface in the view.
        select_deadline = time.monotonic() + 20.0
        count = _select_inbox_count(sock, buf, "c2", deadline)
        tag_n = 3
        while count < 1 and time.monotonic() < select_deadline:
            time.sleep(0.25)
            count = _select_inbox_count(sock, buf, f"c{tag_n}", deadline)
            tag_n += 1
        assert count >= 1, (
            "the client-enabled mailbox must hold the delivered inbound message; "
            f"got {count} EXISTS (see {handle.bridge_log_hint()})"
        )

        # The freshly-aliased admin mailbox is clean, so the message is seq 1.
        fetch_status, fetched = _imap_seq_fetch_body(sock, buf, f"c{tag_n}", 1, deadline)
        assert fetch_status == "OK", f"FETCH 1 BODY[] must succeed; got {fetch_status}"
        assert fetched is not None, "FETCH 1 BODY[] must return a literal body"

        # The decrypted body carries the original RFC 5322 verbatim — proving the
        # client-sealed snapshot's leaf secret opens the MTA-sealed envelope.
        assert raw_message in fetched, (
            "the decrypted body must contain the sent message verbatim "
            f"(client EnableMail → MTA seal → MDA OpenMailRecord);\n want {raw_message!r}\n"
            f"  got {fetched!r}"
        )
        assert nonce.encode() in fetched, "the unique body token must survive"

        sock.sendall(b"c99 LOGOUT\r\n")


@pytest.mark.parametrize("cred_kind", ["plain", "oauthbearer"])
@pytest.mark.feature("standard-mail-apps")
def test_client_enabled_mail_submits_outbound_through_submission(
    app, dedicated_mail_nest, cred_kind, request
):
    """Enable mail in the client (as the nest admin, `cred_kind` credential), then
    *send* an outbound message through the SMTP submission listener authenticating
    with the client-minted credential — and prove the relayed message is
    DKIM-signed and DMARC-aligned.

    The outbound twin of `test_client_enabled_mail_round_trips_through_a_normal_mua`:
    the same client-driven EnableMail provisions a `WrappedSubmissionTokenBlob`
    (mail-settings.md § User actions), and a normal MUA's SMTP submission AUTH
    must succeed against *that* token — not a server-sealed one. The whole point
    (the `green-test-or-it-doesn't-work` principle) is that the credential the
    submission listener accepts is the one the **client minted**.

    Production data flow asserted end-to-end on real binaries:

      the nest admin (logged into the linux app) opens mail-settings → toggles
      "Enable mail" → `MailSettingsMachine::EnableMail` mints the credential and
      provisions the wrapped submission token (sealed under it: PLAIN→Argon2id,
      OAUTHBEARER→HKDF) over WS-RPC →
      a normal MUA connects to the submission listener (465, implicit TLS), does
      SASL AUTH with (admin@<domain>, the client-minted secret) → the bridge
      fetches the `default` wrapped submission token, AEAD-unwraps it with the
      MUA-supplied secret + verifies the inner Ed25519 signature (smtp-server.md
      § Submission auth) → MAIL FROM / RCPT to an external recipient → DATA →
      the MTA strips `Received:`, the nest DKIM-signs (`d=<domain>`) at the
      outbound hand-out, and the MTA relays via the
      operator-hatch `mta_mx_override` to the in-process stub MX →
      an independent verifier (dkimpy, never our `mail-auth` signer) confirms the
      signature cryptographically verifies against the provisioned public-key TXT
      and is DMARC-aligned (`d=` == From: domain).

    Only tier_3 catches this: the wrapped *submission token* the client sealed
    must open under the bridge's submission-AUTH path, and the relayed bytes must
    DKIM-verify at a third party — a client↔bridge seal/open + sign contract no
    stub exercises.
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    assert handle.dkim_public_dns_value, (
        "fixture must read the nest's DKIM record for the outbound-submission leg "
        "(handle.dkim_public_dns_value unset)"
    )
    nest = handle.nest
    domain = handle.domain

    _login_as_nest_admin(app, nest, _dedicated_node_url(app, handle, request))
    admin_addr = _alias_admin_to_address(nest, domain)

    # ── Client UI: enable mail with the parametrized credential. The returned
    # secret is what the submission MUA will authenticate with.
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    mua_secret = _client_enable_mail(app, cred_kind)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"enabling mail ({cred_kind}) must mint the first credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    # This test IS the enable transition, so this is the step under test's own
    # deployment consequence, not incidental setup.
    handle.rebind_after_enable()

    # ── Submission: a normal MUA AUTHs on 465 with the CLIENT-MINTED secret and
    # sends to an external recipient. The 235 succeeds only by AEAD-unwrapping
    # the wrapped submission token the client sealed under this secret.
    external_rcpt = "recipient@external.test"
    token = f"clientsubmit{cred_kind}{int(time.time() * 1000)}qx"
    message_id = f"<{token}@{domain}>"
    body_lines = [
        f"From: Nest Admin <{admin_addr}>",
        f"To: {external_rcpt}",
        f"Subject: client-enabled {cred_kind} outbound proof {token}",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:20:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {token} body must relay out DKIM-signed.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()

    deadline = time.monotonic() + 40.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        _mua_submit_authenticate(conn, cred_kind, admin_addr, mua_secret, deadline)
        conn.cmd(f"MAIL FROM:<{admin_addr}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # ── Relay: the bridge's OutboundWorker delivers via mta_mx_override to the
    # in-process stub MX. Capture the relayed bytes by their unique token.
    received = _wait_for_tagged(handle.stub_mx, token, timeout=20.0)
    assert received is not None, (
        "the stub external MX received no message tagged "
        f"{token} within 20s — the client-enabled submission did not relay out "
        f"(see {handle.bridge_log_hint()})"
    )

    # ── Verify: an independent verifier (dkimpy) confirms the
    # DKIM-Signature cryptographically verifies against the nest-published public
    # TXT (the in-CI proxy for Gmail's `dkim=pass`), fed via a dnsfunc so no real
    # DNS query fires. A misaligned d=/s= would query a name we don't answer.
    import dkim  # dkimpy — independent verifier; fleet venv dep

    expected_query = f"{handle.dkim_selector}._domainkey.{domain}.".encode()
    seen_queries: list[bytes] = []

    def dnsfunc(name, timeout=5):
        seen_queries.append(name)
        return handle.dkim_public_dns_value if name == expected_query else None

    assert dkim.verify(received, dnsfunc=dnsfunc), (
        "dkimpy rejected the DKIM-Signature on the client-enabled "
        "submission — the relayed message would fail `dkim=pass` at a real "
        f"receiver. dnsfunc queries: {seen_queries!r}. First 600 bytes:\n"
        f"{received[:600]!r}"
    )

    # DMARC alignment: the d= signing domain must equal the From: header domain.
    d_tag = dkim_signature_tag(received, "d")
    assert d_tag == domain, (
        f"DKIM d= ({d_tag!r}) is not aligned to the From: domain ({domain!r}) "
        "— DMARC would fail DKIM alignment"
    )

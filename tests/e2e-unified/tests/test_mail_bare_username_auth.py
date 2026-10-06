"""tier_3: the username-FORM matrix for the DEFAULT mail credential — the
`internal/auth.SplitEmailDefault` contract — over IMAP + SMTP
submission + CalDAV.

The user's report (2026-06-04): macOS Calendar.app hangs connecting to example.com
CalDAV though the password works for IMAP. Root cause: Apple
Calendar parses the configured `test@example.com`, uses the domain for server
discovery, and sends **only the bare local part `test`** (no `@domain`) as the
Basic-auth / SASL username. The strict pre-fix `auth.SplitEmail` 401-looped a
domain-less username → a "Connecting…" hang.

`SplitEmailDefault` (the fix) defaults a domain-less username to `PrimaryDomain`,
applied uniformly across CalDAV (`internal/mda/caldav/auth.go`), IMAP
(`internal/mda/imap/auth.go`), and MTA submission (`internal/mta/auth.go`). But
it rescues **only** a genuine no-`@` bare username — `user@` and `@domain` stay
malformed (parsers.go: "the other SplitEmail rejections … stay malformed, since
they are corrupt rather than domain-less"). This matrix locks BOTH halves of that
contract, on all three surfaces, in one tier_3 setup:

  POSITIVE  full-address   `admin@<domain>` → AUTH ok
  POSITIVE  bare-local     `admin`          → AUTH ok   (the Apple form)
  NEGATIVE  trailing-at    `admin@`         → AUTH rejected (must NOT be rescued)
  NEGATIVE  leading-at     `@<domain>`      → AUTH rejected (must NOT be rescued)

The negatives are the "fold the negative" of Gap 1b: they pin that the fix did
not OVER-loosen the parser into rescuing arbitrary malformed shapes — a future
"just resolve whatever" mis-fix would pass the positives but fail these. All four
forms map to credential_id `default`; the RFC 5233 `+credential` sub-address form
(a DIFFERENT credential) is the domain of `test_mail_multi_credential_auth.py`
(the canonical proof referenced by `mail-credentials.md` § MUA-username) and is
intentionally NOT duplicated here.

tier_3 (mocking depth): real nest + real MTA/MDA bridges, the real client UI
minting the credential, real IMAP + SMTP-submission + CalDAV wire AUTH. Only
tier_3 exercises the bridge's username→domain defaulting against a client-sealed
credential — a stub can't. See `docs/goal/architecture/testing.md` § Gap 1.
"""

import base64
import time

import pytest

from helpers.mail_dedicated_nest import ADMIN_LOCAL_PART
from helpers.mail_wire import _connect_submission_tls, _imap_auth_plain, _imaps_connect

# Reuse the enable-mail round-trip setup helpers + the multi-credential test's
# CalDAV PROPFIND helper rather than copy them — priority #2.
from . import test_mail_enable_then_mua_round_trip as rt
from .test_mail_multi_credential_auth import _caldav_propfind_status

# linux is the client the user reported against; windows now has the same
# mail-settings enable UI (shared ids, proven by the green windows run of
# test_caldav_external_appears.py, 2026-06-24), so this unified username-form
# matrix runs on both (priority #1 — one test, not a per-app copy). The
# matrix after the enable preamble is pure IMAP/SMTP/CalDAV wire — driver-
# agnostic — so no per-app branch is needed beyond the marker.
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    # tui added 2026-08-21: the matrix after the enable-mail preamble
    # is pure IMAP/SMTP/CalDAV wire — driver-agnostic — and the preamble is
    # already proven on tui.
    pytest.mark.tui,
    # web added: same reasoning as tui — the preamble is
    # already proven on web (test_mail_credentials.py).
    pytest.mark.web,
]

# Chosen (typed) password, auto-generate OFF, so the MUA authenticates with a
# known value (the masked auto-generate read-back is timing-flaky headless).
# a-zA-Z0-9 only — pastes cleanly through SASL PLAIN / HTTP Basic.
_PASSWORD = "UsernameFormPlainPw0004Dd"

# The username-form matrix for the DEFAULT credential. Templates fill {local}
# (= ADMIN_LOCAL_PART, "admin") and {domain} (the fixture's PrimaryDomain).
# (label, username_template, expect_ok). Add a row to lock a new real-client
# form quirk — the loop drives every row over all three surfaces.
_FORMS = [
    ("full-address", "{local}@{domain}", True),
    ("bare-local-part", "{local}", True),  # Apple Calendar/Mail
    ("trailing-at", "{local}@", False),  # malformed — SplitEmailDefault must NOT rescue
    ("leading-at", "@{domain}", False),  # malformed — "
]


def _smtp_auth(conn, username: str, password: str, expect_prefix: str, deadline: float) -> str:
    """Drive submission `AUTH PLAIN` with an initial response (RFC 4954 §4) and
    assert the reply starts with `expect_prefix` ("235" = accepted; "5" matches
    any 5xx rejection robustly). Raises (via `_SmtpConn.expect`) on mismatch, so
    the call itself is the assertion."""
    ir = base64.b64encode(
        b"\x00" + username.encode() + b"\x00" + password.encode()
    ).decode()
    return conn.cmd(f"AUTH PLAIN {ir}", expect_prefix, deadline)


@pytest.mark.feature("standard-mail-apps")
def test_default_credential_username_form_matrix(app, dedicated_mail_nest, request):
    """Enable mail (default credential), then drive every row of `_FORMS` over
    IMAP, SMTP submission, and CalDAV — positives must authenticate, negatives
    (malformed, non-bare forms) must be rejected with the SAME correct password,
    so a failure is attributable to the FORM, not the password.

    RED for the bare-local-part row until the fix; RED for a negative row if a
    future change over-loosens `SplitEmailDefault` to rescue malformed shapes.
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    rt._login_as_nest_admin(app, nest, rt._dedicated_node_url(app, handle, request))
    caldav_base = f"https://127.0.0.1:{handle.caldav_port}"

    # ── Client UI: enable mail (mints the 'default' credential + boots the
    #    bridge) with a chosen password. ──
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "enabling mail must mint the first ('default') credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "mail must report enabled after the toggle; "
        f"status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()

    # Route admin@<domain> to the admin actor so validate_recipient maps it.
    local = ADMIN_LOCAL_PART  # "admin"
    rt._alias_admin_to_address(nest, domain)  # admin@<domain>

    for label, template, expect_ok in _FORMS:
        username = template.format(local=local, domain=domain)
        verb = "authenticate" if expect_ok else "be rejected (malformed form not rescued)"

        # ── IMAP (macOS Mail). ──
        deadline = time.monotonic() + 60.0
        sock, buf = _imaps_connect(handle, deadline)
        with sock:
            res = _imap_auth_plain(sock, buf, "m1", username, _PASSWORD, deadline)
            if expect_ok:
                assert res == "OK", f"[{label}] IMAP AUTH as {username!r} must {verb}; got {res!r}"
                sock.sendall(b"m9 LOGOUT\r\n")
            else:
                assert res != "OK", f"[{label}] IMAP AUTH as {username!r} must {verb}; got {res!r}"

        # ── SMTP submission (macOS Mail send). "235" = accepted, "5" = any 5xx. ──
        deadline = time.monotonic() + 60.0
        with _connect_submission_tls(handle.submission_port_465, domain) as conn:
            conn.expect("220", deadline)
            conn.cmd(f"EHLO {domain}", "250", deadline)
            _smtp_auth(conn, username, _PASSWORD, "235" if expect_ok else "5", deadline)
            conn.cmd("QUIT", "221", deadline)

        # ── CalDAV (macOS Calendar). AUTH middleware runs before path routing, so
        #    a rejected credential 401s regardless of path; a positive may 404
        #    after auth on a bare-local path — assert NOT-401 for accept. ──
        status = _caldav_propfind_status(caldav_base, username, _PASSWORD)
        if expect_ok:
            assert status != 401, f"[{label}] CalDAV AUTH as {username!r} must {verb}; got HTTP {status}"
        else:
            assert status == 401, f"[{label}] CalDAV AUTH as {username!r} must {verb}; got HTTP {status}"

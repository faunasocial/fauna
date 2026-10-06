"""tier_3: a SECOND mail credential must authenticate over CalDAV + IMAP.

The user's report (2026-06-03): *"I add another plain-text password (i.e. change
password) in the linux client and that seems to work, but I still cannot connect
with either the macOS Calendar or Mail apps using the new password."*

Root cause this pins (RED until fixed): the MDA bridge's AUTH path hardcodes
`credential_id = "default"` — both the IMAP `AUTH PLAIN` path
(`bins/fauna-bridges/internal/mda/imap/auth.go` § `plainCredentialID`) and
its CalDAV Basic-Auth twin (`internal/mda/caldav/auth.go`). So it always fetches
and tries to unwrap the FIRST credential's wrapped-MSEK blob. A second credential
added via the client is sealed under a DIFFERENT `credential_id`
(`derive_credential_id`: "Phone" → "phone"), so its password can never unwrap the
`"default"` blob — every credential beyond the first is unreachable over
IMAP/CalDAV.

The fix the spec already mandates — `docs/goal/behavior/mail-credentials.md` rule
#7 + § MUA-username convention (line 124): the MUA username carries the
credential_id via RFC 5233 sub-addressing — `<handle>+<credential_id>@<domain>`
(the `+<credential_id>` suffix is omitted only for `"default"`). The bridge must
parse that suffix and fetch the matching wrapped-MSEK blob. nest already
sub-addresses inbound RCPT (`libs/fauna-mail/src/aliases/mod.rs` §
`split_subaddress`); only the bridge AUTH path lags ("when I3 amends the provision
shape … the PLAIN path will sniff it from the username" — `imap/auth.go:25`).

This test exercises a second PLAIN credential over BOTH MUA surfaces the user
named — Calendar (CalDAV) and Mail (IMAP) — using the spec-correct sub-addressed
username. It also pins the negative: the bare `<handle>@<domain>` username maps to
`"default"`, so the second credential's password must NOT authenticate there
(otherwise a "try every credential for any username" mis-fix would mask the real
per-credential routing the spec requires).

tier_3 (mocking depth): real nest + real MTA/MDA bridges, the real client UI
minting BOTH credentials, real IMAP + CalDAV wire AUTH against the client-sealed
blobs. Only tier_3 catches it: the per-credential wrapped-MSEK the *client* sealed
must open under the *bridge's* AUTH path keyed on the username-derived
credential_id — a client↔bridge seal/route contract no stub exercises.
"""

import time

import pytest
from requests.auth import HTTPBasicAuth

from helpers.mail_dedicated_nest import ADMIN_LOCAL_PART
from helpers.mail_wire import _imap_auth_plain, _imaps_connect

# Reuse the client-driven enable-mail round-trip sibling's setup helpers
# (login-as-admin, route an address to the admin actor, the web SPA node-url
# shim) rather than copy them — priority #2 (reuse, don't duplicate).
from . import test_mail_enable_then_mua_round_trip as rt

# Drives the mail-settings UI (enable + add-credential PLAIN) via 100%
# shared/driver-agnostic action methods (`enable_mail_plain`,
# `add_credential_plain`, `wait_for_credential_count_at_least`) — the same ones
# `test_mail_credentials.py`/`test_mail_disable.py` already drive with the
# windows marker. windows added 2026-07-18 (was a stale exclusion; the old
# "windows/apple/android have no mail-settings page yet" comment predated
# windows' page and was already contradicted by macos/ios above). web has the
# page but its PLAIN add-credential path is unverified here — lifting to web is
# a follow-on (the action is shared, so the lift is just re-verifying the same
# IDs drive the web form).
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    # tui added 2026-08-21: drives enable_mail_plain/add_credential_plain/
    # wait_for_credential_count_at_least — all already proven tui-portable
    # (test_mail_credentials.py's tui M8 slices); the rest is wire-level.
    pytest.mark.tui,
    # web added: same shared actions, already web-proven
    # via test_mail_credentials.py's web marker.
    pytest.mark.web,
]

# The two PLAIN credentials the client mints, with CHOSEN (typed) passwords so
# the MUA authenticates with a known value (the masked auto-generate field's
# read-back is timing-flaky headless). a-zA-Z0-9 only — pastes cleanly into a MUA
# and stays whitespace-free through HTTP Basic auth / SASL PLAIN.
_DEFAULT_PASSWORD = "DefaultCredPlainPw0001Aa"  # gitleaks:allow
_SECOND_NAME = "Phone"
# `derive_credential_id("Phone", ["default"])` → "phone" (kebab-case;
# libs/fauna-client-mail-settings/src/credential.rs).
_SECOND_CREDENTIAL_ID = "phone"
_SECOND_PASSWORD = "PhoneCredPlainPw0002Bb"


def _caldav_propfind_status(base_url: str, auth_username: str, password: str, *, timeout: float = 45.0) -> int:
    """PROPFIND the AUTH'd actor's CalDAV home set with Basic auth; return the
    HTTP status (401 = AUTH rejected, 207 = authenticated multistatus).

    The home set is keyed on the BASE mailbox (`userBasePath` →
    `sess.AuthedLocalPart()`, the +suffix-stripped local part —
    `bins/fauna-bridges/internal/mda/caldav/backend.go`), so we PROPFIND the
    base path `/caldav/<base>@<domain>/` even when the Basic-auth username carries
    an RFC 5233 `+credential` suffix — exactly what a real MUA does after
    principal/home-set discovery. The auth middleware runs before path routing, so
    a wrong-credential attempt 401s here regardless of path.

    Retries only connection-level errors while the MDA's self-signed HTTPS CalDAV
    listener cold-boots its bind — a real 401/207 reply returns immediately.
    `verify=False` because the local bridge serves a self-signed dev cert (the
    live example.com deployment serves a real ACME cert, so prefer verify=True
    there)."""
    import requests
    import urllib3

    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    body = (
        '<?xml version="1.0" encoding="utf-8"?>'
        '<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>'
    )
    local, _, dom = auth_username.partition("@")
    base_local = local.split("+", 1)[0]
    path_user = f"{base_local}@{dom}" if dom else base_local
    url = f"{base_url.rstrip('/')}/caldav/{path_user}/"
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while time.monotonic() < deadline:
        try:
            resp = requests.request(
                "PROPFIND",
                url,
                auth=HTTPBasicAuth(auth_username, password),
                headers={"Depth": "0", "Content-Type": "application/xml; charset=utf-8"},
                data=body,
                verify=False,
                timeout=6.0,
            )
            return resp.status_code
        except requests.exceptions.RequestException as e:
            last = e
            time.sleep(1.0)
    raise AssertionError(
        f"CalDAV listener at {base_url} never answered a PROPFIND within {timeout:.0f}s "
        f"(last error: {last!r}) — the MDA CalDAV HTTPS bind never came up"
    )


@pytest.mark.feature("standard-mail-apps")
def test_second_credential_authenticates_over_caldav_and_imap(app, dedicated_mail_nest, request):
    """Enable mail (default credential), add a SECOND PLAIN credential in the
    client, then authenticate with it over BOTH CalDAV (macOS Calendar) and IMAP
    (macOS Mail) using its RFC 5233 sub-addressed username.

    RED until the MDA AUTH path resolves credential_id from the username `+suffix`
    instead of hardcoding `"default"`.
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    rt._login_as_nest_admin(app, nest, rt._dedicated_node_url(app, handle, request))
    caldav_base = f"https://127.0.0.1:{handle.caldav_port}"

    # ── Client UI: mint the FIRST ('default') credential (enables mail + boots
    #    the bridge), with a CHOSEN password (typed, auto-generate OFF) so the MUA
    #    authenticates with a known value — no masked read-back. ──
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_DEFAULT_PASSWORD)
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

    # Route admin@<domain> to the admin actor so validate_recipient (AUTH +
    # RCPT) maps the address. After enable so the alias write isn't racing the
    # enable transition.
    admin_addr = rt._alias_admin_to_address(nest, domain)  # admin@<domain>
    second_addr = f"{ADMIN_LOCAL_PART}+{_SECOND_CREDENTIAL_ID}@{domain}"  # admin+phone@<domain>

    # ── Baseline: with ONLY the first credential present, it authenticates over
    #    both IMAP (macOS Mail) and CalDAV (macOS Calendar) at the bare username.
    #    Asserted BEFORE adding the second credential so a later regression check
    #    can attribute any cred-1 breakage specifically to the add. ──
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(sock, buf, "d1", admin_addr, _DEFAULT_PASSWORD, deadline) == "OK", (
            "the first ('default') credential must authenticate over IMAP "
            f"at {admin_addr}"
        )
        sock.sendall(b"d9 LOGOUT\r\n")
    base_caldav = _caldav_propfind_status(caldav_base, admin_addr, _DEFAULT_PASSWORD)
    assert base_caldav in (200, 207), (
        f"the first ('default') credential must authenticate over CalDAV at "
        f"{admin_addr}; got HTTP {base_caldav}"
    )

    # ── Client UI: the user's "add another password" gesture — a SECOND PLAIN
    #    credential, chosen password. Sealed under credential_id "phone"
    #    (≠ "default"). ──
    app.mail_settings.add_credential_plain(_SECOND_NAME, _SECOND_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(2, timeout=15.0), (
        "adding a second PLAIN credential must mint a 2nd credential row; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # ── THE BUG: the SECOND credential must authenticate over CalDAV (macOS
    #    Calendar) using its sub-addressed username + its own password. ──
    second_caldav = _caldav_propfind_status(caldav_base, second_addr, _SECOND_PASSWORD)
    assert second_caldav in (200, 207), (
        f"CalDAV AUTH with the second credential ({second_addr}) must succeed — "
        f"macOS Calendar cannot connect otherwise; got HTTP {second_caldav}. The "
        "MDA Basic-Auth path must resolve credential_id from the username +suffix "
        "(mail-credentials.md § MUA-username), not hardcode 'default'."
    )

    # ── THE BUG: the SECOND credential must authenticate over IMAP (macOS Mail). ──
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(sock, buf, "p1", second_addr, _SECOND_PASSWORD, deadline) == "OK", (
            f"IMAP AUTH with the second credential ({second_addr}) must succeed — "
            "macOS Mail cannot connect otherwise; the bridge must fetch the "
            f"wrapped-MSEK blob for credential_id {_SECOND_CREDENTIAL_ID!r}, not "
            "'default'."
        )
        sock.sendall(b"p9 LOGOUT\r\n")

    # ── Regression: the FIRST credential STILL authenticates after the second was
    #    added (catches a per-credential wrapped-blob storage overwrite). ──
    first_after = _caldav_propfind_status(caldav_base, admin_addr, _DEFAULT_PASSWORD)
    assert first_after in (200, 207), (
        f"the first ('default') credential must still authenticate over CalDAV at "
        f"{admin_addr} after a second credential was added; got HTTP {first_after}"
    )

    # ── Negative (pins per-credential routing): the SECOND credential's password
    #    must NOT authenticate against the BARE username, which maps to "default".
    #    This rejects a "try every credential for any username" mis-fix that would
    #    pass the positives above while violating the sub-addressing spec. ──
    bare_with_second = _caldav_propfind_status(caldav_base, admin_addr, _SECOND_PASSWORD)
    assert bare_with_second == 401, (
        f"the bare username {admin_addr} maps to credential_id 'default' "
        "(mail-credentials.md § MUA-username), so the SECOND credential's password "
        f"must be rejected there; got HTTP {bare_with_second} (a 200/207 means the "
        "bridge isn't routing per credential_id — it's trying every credential)."
    )

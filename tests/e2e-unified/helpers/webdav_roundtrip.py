"""Shared scaffolding for the WebDAV tier_3 round-trips (mount/gate + read/write).

One mail-enable preamble + one served-set precondition helper, reused by both
`tests/test_webdav_mount_and_gate.py` and `tests/test_webdav_read_write_roundtrip.py`
(priorities #2/#4 — one shape, not a copy per test), mirroring the CalDAV
`helpers/caldav_roundtrip.py` precedent.

Public API:
  - `PASSWORD` — the known PLAIN mail credential the WebDAV client AUTHs with.
  - `enable_mail_and_client(app, handle, request)` — log the client in as the
    dedicated nest admin, enable mail through its own UI (mints the credential +
    MSEK + MLS snapshot the MDA's `davauth` AUTH unwraps and `reconcile` seals
    under), give the admin a routable address, and return a SERVING `WebDAVClient`
    plus the nest + admin address.
  - `wait_status(fn, want, *, timeout, seen)` — re-issue a WebDAV request until
    it answers with the wanted status (a synchronous server-side record can race
    the next list/get by a settle tick); returns the last response either way,
    and appends every answer to `seen` when given, so a failure can name the
    FIRST wrong answer rather than only the last (see the poll cadence below for
    why the last one is often a rate limit).
  - `unthrottled(fn, *, timeout)` — issue a WebDAV request, re-issuing it only
    while the nest's bridge limiter refuses it as rate-limited; for journeys
    that make many requests in a row.
  - `serve_enable_folder(driver, folder, *, create, timeout)` — drive the
    `serve_enable_folder` linux test-agent command (the first non-test caller of
    `FoldersAuthor::serve_enable` + `reconcile_webdav_keys_blob`) and block until
    the served-set precondition (content-key genesis + the MSEK-sealed
    `WebdavKeysBlob`) lands. Twin of `mail_dedicated_nest.mint_caldav_mailbox`.

Authority for the wire contract: docs/goal/behavior/webdav-server.md.
"""

from __future__ import annotations

import time

from helpers.mail_dedicated_nest import (
    alias_admin_to_address as _alias_admin_to_address,
    dedicated_node_url as _dedicated_node_url,
    login_as_nest_admin as _login_as_nest_admin,
)
from helpers.waiting import wait_until
from helpers.webdav_client import WebDAVClient

# The PLAIN mail credential the client mints and the WebDAV client authenticates
# with (shared IMAP + CalDAV + CardDAV + WebDAV, AEAD-unwrap-as-auth). A KNOWN
# value so the round-trip holds the exact password — mirrors the proven-green
# CardDAV/CalDAV tier_3 tests' `_PASSWORD`.
PASSWORD = "WebDavRoundtripPlainPw0011Zz"  # gitleaks:allow


# A WebDAV write is a synchronous server-side record; a short retry guards a
# settle race between the record and the next list/get. A ceiling, not a
# target — a green run returns on the first matching answer.
SETTLE_S = 30.0

# The poll cadence is set by the NEST, not by taste: every DAV request the MDA
# serves spends `fauna.bridges.webdav_list_folders` / `webdav_list_files` calls
# from one `_webdav_files` bucket of the nest's bridge limiter — 30 events per
# 60 s per (bridge, actor) (`bins/fauna-nest/src/bridge_rate_limit.rs`), and a
# GET spends two. A 1 s poll burned the bucket in ~12 s (measured 2026-09-08),
# after which every answer was a 500 `fauna.bridges.rate_limited` that hid the
# real first failure. 2.5 s keeps a full `SETTLE_S` of GET polls (12 × 2 = 24)
# inside the bucket.
DAV_POLL_INTERVAL_S = 2.5


def wait_status(fn, want, *, timeout=SETTLE_S, seen=None):
    """Re-issue `fn()` (a request returning a `requests.Response`) until its
    status is `want`; return the last response either way, so the caller's own
    assertion names what actually came back (conventions point 6). `seen`, when
    a list, collects `(status_code, text[:300])` for every answer in order."""
    last = []

    def _probe():
        resp = fn()
        last.append(resp)
        if seen is not None:
            seen.append((resp.status_code, resp.text[:300]))
        return resp.status_code == want

    try:
        wait_until(_probe, timeout, interval=DAV_POLL_INTERVAL_S)
    except AssertionError:
        pass
    return last[-1]


def unthrottled(fn, *, timeout=90.0):
    """Issue `fn()` (a WebDAV request) and re-issue it while the nest's bridge
    limiter answers `rate_limited` (a 500 whose body names the code — see
    `DAV_POLL_INTERVAL_S` for the bucket). Any other answer is returned as-is.

    For journeys that make many DAV requests in a row: the limiter is a real
    per-actor budget (30 events / 60 s), and a file manager that hits it simply
    retries, which is what this does. It never retries a genuine answer, so a
    412 or a 404 under test still comes back on the first try."""
    deadline = time.monotonic() + timeout
    while True:
        resp = fn()
        if not (resp.status_code == 500 and "rate_limited" in resp.text):
            return resp
        if time.monotonic() >= deadline:
            return resp
        time.sleep(DAV_POLL_INTERVAL_S * 2)


def enable_mail_and_client(app, handle, request):
    """Log the client in as the dedicated nest admin, enable mail through its own
    UI (provisions the credential + MSEK + MLS snapshot the MDA's `davauth` AUTH
    unwraps and `reconcile_webdav_keys_blob` seals the blob under), give the admin
    a routable address, and return a SERVING `WebDAVClient` plus the nest + admin
    address. Mirrors the CardDAV `test_carddav_roundtrip._enable_mail_and_client`
    preamble (priority #2/#4 — one shape)."""
    nest = handle.nest
    domain = handle.domain

    _login_as_nest_admin(app, nest, _dedicated_node_url(app, handle, request))
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(
        1, timeout=app.mail_settings.ENABLE_SETTLE_S
    ), (
        "enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(
        timeout=app.mail_settings.ENABLE_SETTLE_S
    ), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()
    admin_addr = _alias_admin_to_address(nest, domain)  # admin@<domain>

    base = f"https://127.0.0.1:{handle.caldav_port}"
    client = WebDAVClient(base, admin_addr, PASSWORD, verify=False)
    client.wait_until_serving()
    return client, nest, admin_addr


def serve_enable_folder(driver, folder, *, create=True, timeout=45.0):
    """Arrange a WebDAV-served, content-keyed folder for the logged-in actor via
    the `serve_enable_folder` linux test-agent command, blocking until the reply
    lands. Runs `FoldersAuthor::serve_enable` (content-key genesis + the nest
    `webdav_enabled` flag) + `reconcile_webdav_keys_blob` (the MSEK-sealed
    `WebdavKeysBlob`), so a WebDAV client can PUT/GET the set. Requires mail
    already enabled (the reconcile seals under the MSEK) and the conversations
    session live (the shared `MlsEngine` the author is built over). Returns the
    number of served sets the reconciled blob carries. Twin of
    `mail_dedicated_nest.mint_caldav_mailbox` (fixture setup — carve-out (b): the
    *mutation under test* is the WebDAV PUT/GET through the real client)."""
    driver.call_command(
        "serve_enable_folder", {"folder": folder, "create": create}
    )
    deadline = time.monotonic() + timeout
    reply = None
    while time.monotonic() < deadline:
        state = driver.get_state() or {}
        reply = state.get("webdav_serve_reply")
        if reply is not None:
            break
        time.sleep(0.3)
    assert reply is not None and reply.get("ok") is True, (
        f"serve_enable_folder({folder!r}) must succeed; got {reply!r}"
    )
    return reply.get("served_sets")

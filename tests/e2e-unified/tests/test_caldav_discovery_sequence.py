"""tier_3: the real-client CalDAV discovery WALK (Apple-style) — Gap 1c — plus
the RFC 6638 server-side auto-schedule capability advertisement (Hop 4).

Real CalDAV clients (macOS Calendar) don't PROPFIND the calendar home directly —
they discovery-walk: **current-user-principal → calendar-home-set → list
calendars**, starting from the server root (after the `.well-known/caldav`
redirect). After discovery a real Apple client detects whether the server does
RFC 6638 scheduling (OPTIONS `calendar-auto-schedule` token + principal
scheduling props + schedule Inbox/Outbox collections); if it does, the client
stands down from its own client-side iMIP, so the server-side organizer fan-out
gateway doesn't produce DUPLICATE invites. Hop 4 below pins that advertisement. Every other CalDAV test in this suite SKIPS that walk by pointing
straight at `/caldav/{user}/` (the home), so a break anywhere in the discovery
chain — the class of bug that left macOS Calendar stuck "Connecting…" — would go
uncaught e2e. This drives the chain with `requests` (a real HTTP client, no new
PyPI dep) using the **bare-username** auth real Apple clients send, asserting each
discovery hop and following the returned hrefs (not hardcoding them).

This is the dep-free form of `testing.md` § Gap 1's "real-client wire-trace
corpus": rather than install a full CalDAV client lib (which would mean adding the
dep to every platform's dev environment + CI), it replays the exact discovery
*sequence* a real client performs against the MDA's emersion/go-webdav handler
(mounted at `/`, server.go:124; `CurrentUserPrincipal` → the single-segment
`/{u}@{d}/` so emersion's depth-router classifies it as the user-principal,
backend.go:97). tier_3: real nest + MDA, real HTTPS PROPFIND discovery against a
client-sealed credential — a stub can't.
"""

import re
import time

import pytest

from helpers.mail_dedicated_nest import ADMIN_LOCAL_PART

# Reuse the enable-mail round-trip setup helpers — priority #2.
from . import test_mail_enable_then_mua_round_trip as rt

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,  # unified 2026-06-25 — the shared enable_mail_plain preamble
    pytest.mark.macos,  # unified 2026-07-13 — same preamble, proven on macos once the
    pytest.mark.ios,  # apple dedicated-nest mint was fixed; ios verified 2026-07-13
    # (shared FaunaKit — the same mail-settings surface) (see test_addressbook).
    # is proven on windows; the discovery walk itself is driver-agnostic HTTP (requests).
    # Gap 1c FIXED (2026-06-05,): this faithful real-client discovery
    # walk now PASSES. Root cause confirmed (emersion principal-resource routing,
    # NOT a path-normalization detail): emersion/go-webdav routes resources by
    # path-segment DEPTH (`caldav.backend.resourceTypeAtPath`), so the principal
    # served at the 2-segment `/principals/{u}@{d}/` collided with the
    # calendar-home-set's depth and a PROPFIND of it returned an EMPTY multistatus
    # — the macOS Calendar "Connecting…" stall. Fix: the MDA now returns the
    # principal at a SINGLE-segment path (`backend.go::CurrentUserPrincipal`), so
    # emersion's own `propFindUserPrincipal` (which serves calendar-home-set)
    # fires. The walk below FOLLOWS the principal href (does not hardcode its
    # shape), so it pins the discovery *contract*, not the path. In-process Go
    # twin (fast, no nest): `internal/mda/caldav/server_test.go`
    # ::TestServerDiscoveryWalkReachesCalendarHomeSet. caldav-server.md
    # § Authentication (discovery).
    # tui added 2026-08-21: the whole discovery walk after the
    # enable-mail preamble is driver-agnostic raw HTTP (`requests`); the
    # preamble itself is already proven on tui.
    pytest.mark.tui,
    # web added: same reasoning as tui — the discovery
    # walk drives no app UI at all beyond enable_mail_plain, already proven
    # on web (test_mail_enable_then_mua_round_trip.py).
    pytest.mark.web,
]

_PASSWORD = "CalDavDiscoveryPlainPw0005Ee"
_CALDAV_NS = "urn:ietf:params:xml:ns:caldav"


def _propfind(base_url, path, username, password, prop_xml, *, depth="0", timeout=45.0):
    """Drive one PROPFIND with HTTP Basic auth; return (status, body). Retries
    only connection-level errors while the MDA's self-signed CalDAV HTTPS listener
    cold-boots — a real 207/401 returns immediately. verify=False: local dev cert."""
    import requests
    import urllib3
    from requests.auth import HTTPBasicAuth

    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    body = (
        '<?xml version="1.0" encoding="utf-8"?>'
        f'<d:propfind xmlns:d="DAV:" xmlns:c="{_CALDAV_NS}"><d:prop>{prop_xml}</d:prop></d:propfind>'
    )
    url = base_url.rstrip("/") + path
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            resp = requests.request(
                "PROPFIND", url,
                auth=HTTPBasicAuth(username, password),
                headers={"Depth": depth, "Content-Type": "application/xml; charset=utf-8"},
                data=body, verify=False, timeout=6.0,
            )
            return resp.status_code, resp.text
        except requests.exceptions.RequestException as e:
            last = e
            time.sleep(1.0)
    raise AssertionError(f"CalDAV PROPFIND {url} never answered within {timeout:.0f}s ({last!r})")


def _options(base_url, path, username, password, *, timeout=45.0):
    """Drive one OPTIONS with HTTP Basic auth; return (status, dav_header). Retries
    only connection-level errors while the CalDAV HTTPS listener cold-boots."""
    import requests
    import urllib3
    from requests.auth import HTTPBasicAuth

    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    url = base_url.rstrip("/") + path
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            resp = requests.request(
                "OPTIONS", url,
                auth=HTTPBasicAuth(username, password),
                verify=False, timeout=6.0,
            )
            return resp.status_code, resp.headers.get("DAV", "")
        except requests.exceptions.RequestException as e:
            last = e
            time.sleep(1.0)
    raise AssertionError(f"CalDAV OPTIONS {url} never answered within {timeout:.0f}s ({last!r})")


def _href_in(xml: str, prop_tag: str) -> str | None:
    """Extract the first <href> nested inside the <…:prop_tag>…</…:prop_tag> block
    (namespace-prefix agnostic), e.g. the principal href inside
    <d:current-user-principal>. Returns None if the prop or its href is absent."""
    block = re.search(rf"<[^>]*\b{prop_tag}\b[^>]*>(.*?)</[^>]*\b{prop_tag}\b[^>]*>", xml, re.S | re.I)
    if not block:
        return None
    href = re.search(r"<[^>]*\bhref\b[^>]*>([^<]+)</[^>]*\bhref\b[^>]*>", block.group(1), re.S | re.I)
    return href.group(1).strip() if href else None


@pytest.mark.feature("calendar-in-standard-apps")
def test_apple_style_caldav_discovery_walk(app, dedicated_mail_nest, request):
    """Walk the CalDAV discovery chain a real macOS Calendar client follows —
    current-user-principal → calendar-home-set → list calendars — with the bare
    username, asserting each hop and following the returned hrefs.

    RED if any discovery hop breaks for a bare-username real-client flow (the
    macOS-Calendar-stuck-Connecting class); the existing home-pointed PROPFIND
    tests would not catch it.
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    rt._login_as_nest_admin(app, nest, rt._dedicated_node_url(app, handle, request))
    base = f"https://127.0.0.1:{handle.caldav_port}"

    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"enabling mail must mint the default credential; error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()
    rt._alias_admin_to_address(nest, domain)
    bare = ADMIN_LOCAL_PART  # "admin" — the domain-less form Apple Calendar sends

    # ── Hop 1: the bootstrap PROPFIND. Real CalDAV clients request the principal
    #    props TOGETHER at the discovery root (current-user-principal +
    #    calendar-home-set in one PROPFIND), which is how emersion/go-webdav
    #    surfaces them. We try the combined root PROPFIND first; if it yields a
    #    home, that's the discovery answer. We also follow the strict RFC-6764
    #    principal walk as a fallback and record which path the MDA honors. ──
    st, root_body = _propfind(
        base, "/", bare, _PASSWORD, "<d:current-user-principal/><c:calendar-home-set/>"
    )
    assert st == 207, (
        f"the discovery-root PROPFIND at / must be 207 (authed); got HTTP {st}: {root_body[:400]!r}"
    )
    # Contract, not shape: assert a current-user-principal href exists and is an
    # absolute path we can follow — do NOT hardcode the path (the MDA returns a
    # single-segment principal so emersion's depth-router classifies it as the
    # user-principal, not the calendar-home-set; see the module header).
    principal = _href_in(root_body, "current-user-principal")
    assert principal and principal.startswith("/"), (
        f"discovery must return a current-user-principal href at the root; got {principal!r} in {root_body[:500]!r}"
    )

    # ── Hop 2: calendar-home-set — prefer it from the combined root response;
    #    fall back to the strict principal walk; record which the MDA honors. ──
    home = _href_in(root_body, "calendar-home-set")
    served_at = "root (combined)"
    if not home:
        st_p, body_p = _propfind(base, principal, bare, _PASSWORD, "<c:calendar-home-set/>")
        home = _href_in(body_p, "calendar-home-set")
        served_at = "principal"
    assert home and "/caldav/" in home, (
        "discovery must yield a calendar-home-set href (combined root PROPFIND or "
        f"the principal walk); root-body={root_body[:400]!r}; got home={home!r}"
    )
    print(f"[discovery] calendar-home-set served at: {served_at} → {home}")

    # ── Hop 3: list calendars at the home (Depth 1) — must surface a calendar
    #    collection so the client has something to subscribe to. ──
    st, body = _propfind(base, home, bare, _PASSWORD, "<d:resourcetype/><d:displayname/>", depth="1")
    assert st == 207, (
        f"calendar-home PROPFIND (Depth 1) at {home!r} must be 207; got HTTP {st}: {body[:400]!r}"
    )
    assert "calendar" in body.lower(), (
        f"discovery hop 3: the calendar home must list a calendar collection; got {body[:700]!r}"
    )

    # ── Hop 4: RFC 6638 server-side auto-schedule ADVERTISEMENT. A real Apple
    #    Calendar client, after discovering the home, detects whether the server
    #    does scheduling — if it does, the client STANDS DOWN from its own
    #    client-side iMIP (otherwise both send the invite → DUPLICATE invites).
    #    The signal is (a) the `calendar-auto-schedule` DAV-header token on an
    #    OPTIONS, (b) the principal's scheduling props, and (c) real, correctly
    #    typed schedule Inbox/Outbox collections (RFC 6638 §2.1/§2.2 MUST).
    #    The in-process Go twins (server_test.go::TestServer{Advertises…,
    #    PrincipalAdvertisesSchedulingProps, ScheduleInboxOutboxReportResourceType})
    #    pin the exact wire shape; this asserts it survives the full binary stack.
    #    caldav-server.md § Server-side auto-schedule. ──
    st_opt, dav = _options(base, home, bare, _PASSWORD)
    assert st_opt in (200, 204), f"OPTIONS at {home!r} must succeed (authed); got HTTP {st_opt}"
    assert "calendar-auto-schedule" in dav, (
        f"OPTIONS DAV header must advertise calendar-auto-schedule (RFC 6638 §2) so a "
        f"client defers to server-side scheduling; got DAV={dav!r}"
    )

    st_s, sched = _propfind(
        base, principal, bare, _PASSWORD,
        "<c:calendar-user-address-set/><c:schedule-inbox-URL/><c:schedule-outbox-URL/>",
    )
    assert st_s == 207, f"principal scheduling PROPFIND must be 207; got HTTP {st_s}: {sched[:400]!r}"
    addr = _href_in(sched, "calendar-user-address-set")
    assert addr and addr.startswith("mailto:") and bare in addr, (
        f"calendar-user-address-set must contain a mailto: for the AUTH'd user so the "
        f"client matches the VEVENT ORGANIZER/ATTENDEE; got {addr!r} in {sched[:500]!r}"
    )
    inbox = _href_in(sched, "schedule-inbox-URL")
    outbox = _href_in(sched, "schedule-outbox-URL")
    assert inbox and outbox, (
        f"principal must advertise schedule-inbox-URL + schedule-outbox-URL; got "
        f"inbox={inbox!r} outbox={outbox!r} in {sched[:600]!r}"
    )

    st_in, in_body = _propfind(base, inbox, bare, _PASSWORD, "<d:resourcetype/>")
    assert st_in == 207 and "schedule-inbox" in in_body, (
        f"schedule Inbox {inbox!r} must be a real collection reporting "
        f"CALDAV:schedule-inbox resourcetype (RFC 6638 §2.2); HTTP {st_in} body={in_body[:500]!r}"
    )
    st_out, out_body = _propfind(base, outbox, bare, _PASSWORD, "<d:resourcetype/>")
    assert st_out == 207 and "schedule-outbox" in out_body, (
        f"schedule Outbox {outbox!r} must be a real collection reporting "
        f"CALDAV:schedule-outbox resourcetype (RFC 6638 §2.1); HTTP {st_out} body={out_body[:500]!r}"
    )

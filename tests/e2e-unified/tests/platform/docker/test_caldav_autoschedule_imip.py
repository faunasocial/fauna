"""tier_4 e2e: server-side CalDAV ``calendar-auto-schedule`` in the real image.

The MDA's organizer fan-out (``caldav-server.md`` § Server-side auto-schedule)
runs ONLY inside the ``fauna-mail-bridge --role=mda`` binary, on a successful
CalDAV PUT/DELETE. Every link below it is unit/Go-twin/tier_3-proven
(``internal/mda/caldav/autoschedule_test.go`` + ``autoschedule_cancel_test.go``
drive a real emersion PUT/DELETE + real FFI; ``conformance_caldav_imip_send.rs``
drives the in-process send path). What no lower tier can reach is the
**image/packaging/supervision** half: that the shipped Docker image actually
*runs* the gateway — the MDA crate is in the binary, s6 starts the MDA with its
CalDAV listener bound behind the SNI router, a stock CalDAV PUT reaches it, the
gateway fires the caller-scoped ``enqueue_outbound_mail``, and the MTA's outbound
worker drains the queue and relays the iMIP onto the wire. That is the tier_4
acceptance owed by Step 4.

**What this asserts**, against the real image with the MDA + MTA serving and an
external recipient domain (``external.test``) routed to a stub MX sidecar (the
same outbound-capture rig the Slice-6/bidirectional relay tests use):

  A. An organizer (``alice@localhost``, a provisioned mail user) PUTs an event
     inviting two **external** attendees (``bob@external.test`` +
     ``carol@external.test``). The gateway fans an iMIP **REQUEST** to both, which
     the MTA relays out — the stub MX captures it (``METHOD:REQUEST``, both
     attendees in ``To:``, the event ``UID``).
  B. The organizer PUTs the SAME event with ``carol`` removed (roster → ``bob``).
     The gateway fans a fresh **REQUEST** to the reduced roster (``To: bob``, no
     carol) AND an iMIP **CANCEL** (RFC 5546 organizer withdrawal) to the removed
     attendee — both captured at the stub MX.
  C. The organizer **DELETEs** the event. The gateway fans an iMIP **CANCEL** to
     the remaining roster (``bob``), captured at the stub MX.

**Why A/B/C use EXTERNAL attendees.** They isolate the OUTBOUND fan-out — the
gateway builds the iMIP and the MTA relays it onto the wire — by routing the
attendee domain (``external.test``) to a stub MX that captures the relayed
``.eml`` (``METHOD`` + ``To:`` + delivery count). An in-domain attendee never
reaches the wire, so it can't be observed at a stub MX.

**The in-domain case has its own test** (``…autoschedule_in_domain_attendee_
delivered_to_inbox``): ``enqueue_outbound_mail`` **short-circuits** an in-domain
recipient that resolves to a local mailbox to local sealed delivery (smtp-server.md
§ Outbound delivery — *In-domain local-delivery short-circuit*), so an in-domain
attendee's iMIP lands in that user's sealed INBOX (read back over IMAP) and is
NEVER MX-relayed — the packaging/supervision acceptance for the self-loop fix:
without it the in-domain attendee MX-loops back and
``554``-bounces at the inbound HELO-identity gate (the docker hairpin).

**The capture sees the message DATA, not the SMTP envelope.** ``stub_mx.py``
persists each delivery's raw RFC-5322 bytes (``_write_out``); the envelope RCPT
TO is recorded in-process only, unreachable cross-container. The iMIP ``To:``
header lists the full roster the message was built for, so for the CANCEL the
``To:`` carries the *prior* roster while the envelope (the removed subset) is what
the unit test (``autoschedule_cancel_test.go`` asserts ``cancel.Recipients ==
[carol]``) pins. tier_4 therefore asserts on ``METHOD`` + ``To:`` + delivery
count — the removed-subset envelope precision stays tier_3's job.

Opt-in (builds the image + starts containers): ``just e2e-tier-4-test`` or
``pytest tests/e2e-unified/tests/platform/docker/test_caldav_autoschedule_imip.py
--tier 4``.
"""

import base64
import os
import re
import shutil
import socket
import ssl
import subprocess
import tempfile
import time
from pathlib import Path

import pytest


from .helpers import (
    IMAGE_TAG,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    create_network,
    docker_build,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_mail_user,
    register_primary_domain,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_stub_mx_sidecar,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

DOMAIN = "localhost"                 # the deployment's primary (local) mail domain
EXTERNAL_DOMAIN = "external.test"    # attendees' domain, routed to the stub MX
CLAIM_CODE = "AUTOSC1"
ORGANIZER_LOCAL = "alice"
ORGANIZER_PASSWORD = "autosched-organizer-1"
ATTENDEE_BOB = f"bob@{EXTERNAL_DOMAIN}"
ATTENDEE_CAROL = f"carol@{EXTERNAL_DOMAIN}"
EVENT_UID = "tier4-autosched-0001@fauna.test"
EVENT_SUMMARY = "Tier4 Auto-Schedule Review"

# In-domain attendee (a second provisioned mail user on the OWN domain) — the
# subject of the in-domain local-delivery short-circuit test below.
ATTENDEE_DAVE_LOCAL = "dave"
DAVE_PASSWORD = "autosched-attendee-dave-1"
ATTENDEE_DAVE = f"{ATTENDEE_DAVE_LOCAL}@{DOMAIN}"
EVENT_UID_IN_DOMAIN = "tier4-autosched-indomain-0001@fauna.test"


# ── iCalendar bodies (CRLF per RFC 5545; DTSTAMP mandatory — see Gap 2) ──────


def _ics(*attendees: str) -> bytes:
    """A VEVENT organized by ``alice@localhost`` inviting ``attendees`` (mailto
    CAL-ADDRESSes). The ORGANIZER must equal the CalDAV-AUTH'd user for the
    gateway's organizer gate to fire (autoschedule.go: ``req.From ==
    AuthedLocalPart@AuthedDomain``)."""
    lines = [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//fauna//tier4-autosched//EN",
        "BEGIN:VEVENT",
        f"UID:{EVENT_UID}",
        "DTSTAMP:20260601T120000Z",
        "DTSTART:20260815T150000Z",
        "DTEND:20260815T160000Z",
        f"SUMMARY:{EVENT_SUMMARY}",
        f"ORGANIZER:mailto:{ORGANIZER_LOCAL}@{DOMAIN}",
    ]
    for a in attendees:
        lines.append(f"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{a}")
    lines += ["END:VEVENT", "END:VCALENDAR"]
    return ("\r\n".join(lines) + "\r\n").encode()


# ── CalDAV-over-the-SNI-router request helper ───────────────────────────────


def _caldav_request(router_port: int, method: str, path: str, *,
                    username: str | None = None, password: str | None = None,
                    body: bytes = b"", depth: str | None = None,
                    content_type: str | None = None,
                    timeout: float = 20.0) -> tuple[int, dict[str, str], bytes]:
    """One HTTP/1.1 request to the in-container ``fauna-sni-router`` on
    ``127.0.0.1:router_port`` over TLS, sending SNI ``mail.<domain>`` (the routing
    key that splices the stream to the MDA's CalDAV listener) while the TCP target
    stays loopback. Optional HTTP Basic auth, request body, ``Depth``, and
    ``Content-Type``. Cert verification off (self-signed deploy cert). Returns
    ``(status, headers_lower, body)``.

    Raw socket (not ``requests``/``http.client``) because the SNI must differ from
    the connect host — those derive ``server_hostname`` from the URL host and so
    can't send ``mail.<domain>`` to ``127.0.0.1``. Mirrors
    ``test_caldav_sni_router.py::_sni_https_request`` with a request body added."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    sni = f"mail.{DOMAIN}"

    header_lines = [
        f"{method} {path} HTTP/1.1",
        f"Host: {sni}",
        "Connection: close",
        f"Content-Length: {len(body)}",
    ]
    if username is not None:
        cred = base64.b64encode(f"{username}:{password}".encode()).decode()
        header_lines.append(f"Authorization: Basic {cred}")
    if depth is not None:
        header_lines.append(f"Depth: {depth}")
    if content_type is not None:
        header_lines.append(f"Content-Type: {content_type}")
    req = ("\r\n".join(header_lines) + "\r\n\r\n").encode() + body

    raw = socket.create_connection(("127.0.0.1", router_port), timeout=timeout)
    try:
        with ctx.wrap_socket(raw, server_hostname=sni) as tls:
            tls.settimeout(timeout)
            tls.sendall(req)
            buf = b""
            while True:
                try:
                    chunk = tls.recv(4096)
                except (ssl.SSLError, OSError):
                    break
                if not chunk:
                    break
                buf += chunk
    finally:
        try:
            raw.close()
        except OSError:
            pass

    head, _, resp_body = buf.partition(b"\r\n\r\n")
    lines = head.split(b"\r\n")
    parts = lines[0].split(b" ") if lines else []
    status = int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else 0
    headers: dict[str, str] = {}
    for ln in lines[1:]:
        k, _, v = ln.partition(b":")
        if v:
            headers[k.decode().strip().lower()] = v.decode().strip()
    return status, headers, resp_body


# ── stub-MX delivery capture (the .eml files accumulate; snapshot per phase) ──


def _eml_names(out_dir: str) -> set[str]:
    try:
        return {fn for fn in os.listdir(out_dir) if fn.endswith(".eml")}
    except OSError:
        return set()


def _await_new_emls(out_dir: str, before: set[str], *, min_count: int,
                    timeout: float = 120.0) -> list[bytes]:
    """Poll the stub-MX out-dir for at least ``min_count`` ``.eml`` deliveries not
    in ``before``; return their raw bytes. After the count is met, settle briefly
    and re-read so a straggler sibling delivery (one outbound row per recipient)
    isn't missed. Returns whatever arrived on timeout (the caller asserts)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        new = sorted(_eml_names(out_dir) - before)
        if len(new) >= min_count:
            time.sleep(1.5)  # let same-enqueue sibling deliveries land
            new = sorted(_eml_names(out_dir) - before)
            break
        time.sleep(0.5)
    else:
        new = sorted(_eml_names(out_dir) - before)
    out = []
    for fn in new:
        try:
            out.append(Path(out_dir, fn).read_bytes())
        except OSError:
            pass
    return out


def _method(raw: bytes) -> str | None:
    m = re.search(r"METHOD:([A-Z]+)", raw.decode("utf-8", "replace"))
    return m.group(1) if m else None


def _to_header(raw: bytes) -> str:
    m = re.search(r"(?im)^To:[ \t]*(.+)$", raw.decode("utf-8", "replace"))
    return m.group(1).strip() if m else ""


# ── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def stub_mx(docker_image):
    """A stub external SMTP MX on a user-defined network the nest joins, the
    outbound relay target for ``external.test`` (the attendee domain). Each
    delivered message lands as a ``.eml`` in the mounted out-dir. Yields
    ``{network, mx_target, out_dir}``."""
    suffix = find_free_ports(1)[0]
    network = f"fauna-autosched-net-{suffix}"
    stub_name = f"fauna-autosched-stubmx-{suffix}"
    helpers_dir = str(get_repo_root() / "tests" / "e2e-unified" / "helpers")
    out_dir = tempfile.mkdtemp(prefix="fauna-autosched-stubmx-")
    # World-writable: the sidecar (root, maybe userns-remapped) writes through the
    # bind mount regardless of the daemon's uid-remap policy.
    os.chmod(out_dir, 0o777)
    create_network(network)
    try:
        mx_target = start_stub_mx_sidecar(network, helpers_dir, out_dir, name=stub_name)
        yield {"network": network, "mx_target": mx_target, "out_dir": out_dir}
    finally:
        remove_container(stub_name)
        remove_network(network)
        shutil.rmtree(out_dir, ignore_errors=True)


@pytest.fixture()
def autosched_nest(docker_image, stub_mx, run_seal_helper):
    """A claimed container on the stub-MX network with the MDA + MTA serving, the
    organizer (``alice@localhost``) provisioned as a full mail user (so a CalDAV
    Basic-auth PUT unwraps her capability + MLS snapshot — the CANCEL prior-read
    precondition), and ``external.test`` routed to the stub MX. The :443 router is
    mapped so a stock CalDAV client reaches the MDA via SNI ``mail.<domain>``.
    Storage mode committed plaintext (the deploy default; CalDAV + cert
    provisioning need a committed mode). Yields the nest dict + organizer +
    ``out_dir`` + ``router_port``."""
    http_port, router_port, *mail = find_free_ports(6)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-caldav-autosched-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, 443: router_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            # Relay the attendees' external domain to the stub MX (by IP:port).
            "FAUNA_MTA_MX_OVERRIDE": f"{EXTERNAL_DOMAIN}={stub_mx['mx_target']}",
        },
        network=stub_mx["network"],
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        nest = {
            "name": name,
            "port": http_port,
            "router_port": router_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
            "mail_ports": mail_ports,
            "out_dir": stub_mx["out_dir"],
        }
        register_primary_domain(nest, DOMAIN)
        organizer = provision_mail_user(
            nest, run_seal_helper, domain=DOMAIN,
            local_part=ORGANIZER_LOCAL, password=ORGANIZER_PASSWORD)
        # A second in-domain mail user — the attendee for the in-domain
        # local-delivery short-circuit test (its iMIP lands in this user's
        # sealed INBOX, never relayed). The external-attendee test ignores it.
        attendee = provision_mail_user(
            nest, run_seal_helper, domain=DOMAIN,
            local_part=ATTENDEE_DAVE_LOCAL, password=DAVE_PASSWORD)
        bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
        yield {**nest, "organizer": organizer, "attendee": attendee}
    finally:
        remove_container(name)


# ── Test ──────────────────────────────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_caldav_organizer_autoschedule_fans_imip_request_and_cancel(autosched_nest):
    nest = autosched_nest
    rp = nest["router_port"]
    out_dir = nest["out_dir"]
    name = nest["name"]
    org = f"{ORGANIZER_LOCAL}@{DOMAIN}"

    def diag(msg: str) -> str:
        return f"{msg}\n\n── bridge diagnostics ──\n{bridge_diag(name)}"

    # ── Discovery: PROPFIND the calendar-home so ListCalendars lazy-provisions
    # the "Personal" calendar (blake3("personal")[:32]); extract its collection
    # hex from the multistatus. A bare PUT to an unprovisioned calendar 404s
    # (put.go: PutEventCalendarNotFound), so this discovery step is required — and
    # is exactly what a stock CalDAV client does on first connect.
    propfind_body = (
        '<?xml version="1.0" encoding="utf-8"?>'
        '<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
        "<d:prop><d:resourcetype/><d:displayname/></d:prop></d:propfind>"
    ).encode()
    status, _h, body = _caldav_request(
        rp, "PROPFIND", f"/caldav/{org}/", username=org, password=ORGANIZER_PASSWORD,
        body=propfind_body, depth="1", content_type='application/xml; charset="utf-8"')
    assert status == 207, diag(
        f"PROPFIND of the calendar-home must be 207 multistatus; got {status}, "
        f"body={body[:300]!r}")
    # Tolerate any encoding of the `{u}@{d}` segment (some servers %40-encode the
    # `@`); the calendar collection href is `/caldav/<user>/<64-hex-cal-id>/`.
    m = re.search(r"/caldav/[^/]+/([0-9a-f]{64})/", body.decode("utf-8", "replace"))
    assert m, diag(
        f"calendar-home PROPFIND must enumerate the lazy Personal calendar's "
        f"collection href; none found in body={body[:400]!r}")
    cal_hex = m.group(1)
    event_path = f"/caldav/{org}/{cal_hex}/tier4-autosched.ics"

    # ── (A) Organizer invites bob + carol → iMIP REQUEST fans out to both. ──
    before = _eml_names(out_dir)
    status, headers, body = _caldav_request(
        rp, "PUT", event_path, username=org, password=ORGANIZER_PASSWORD,
        body=_ics(ATTENDEE_BOB, ATTENDEE_CAROL),
        content_type="text/calendar; charset=utf-8")
    assert status in (200, 201, 204), diag(
        f"organizer PUT (create) must succeed; got {status}, body={body[:300]!r}")
    # The server rewrites the filename to the canonical blake3(UID) href; DELETE
    # must address THAT, not the client slug.
    location = headers.get("location", "")
    assert location, diag(f"PUT 201 must carry a Location (canonical href); headers={headers}")

    msgs = _await_new_emls(out_dir, before, min_count=2)
    reqs = [r for r in msgs if _method(r) == "REQUEST"]
    cancels = [r for r in msgs if _method(r) == "CANCEL"]
    assert reqs, diag(
        f"adding attendees must fan an iMIP REQUEST out to the stub MX; "
        f"captured {len(msgs)} message(s), methods={[_method(r) for r in msgs]}")
    assert not cancels, diag(
        "a fresh invite must NOT emit a CANCEL (no prior roster to withdraw)")
    for r in reqs:
        to = _to_header(r)
        assert ATTENDEE_BOB in to and ATTENDEE_CAROL in to, diag(
            f"REQUEST To: must address both invitees; got To: {to!r}")
        assert EVENT_UID in r.decode("utf-8", "replace"), diag(
            "REQUEST must carry the event UID")
        assert org in r.decode("utf-8", "replace").lower(), diag(
            "REQUEST must carry the organizer (From)")
    # One outbound row per recipient → both invitees got a delivery.
    assert len(reqs) >= 2, diag(
        f"REQUEST must be relayed to BOTH invitees (one delivery each); got {len(reqs)}")

    # ── (B) Organizer removes carol → REQUEST to the reduced roster + CANCEL. ──
    before = _eml_names(out_dir)
    status, _h, body = _caldav_request(
        rp, "PUT", event_path, username=org, password=ORGANIZER_PASSWORD,
        body=_ics(ATTENDEE_BOB), content_type="text/calendar; charset=utf-8")
    assert status in (200, 201, 204), diag(
        f"organizer PUT (remove attendee) must succeed; got {status}, body={body[:300]!r}")

    msgs = _await_new_emls(out_dir, before, min_count=2)
    reqs = [r for r in msgs if _method(r) == "REQUEST"]
    cancels = [r for r in msgs if _method(r) == "CANCEL"]
    # REQUEST re-issued to the shrunken roster: To carries bob, NOT carol.
    reduced = [r for r in reqs if ATTENDEE_BOB in _to_header(r) and ATTENDEE_CAROL not in _to_header(r)]
    assert reduced, diag(
        f"removing an attendee must re-fan a REQUEST to the reduced roster "
        f"(To: bob, no carol); REQUEST To:s = {[_to_header(r) for r in reqs]}")
    # CANCEL withdrawn for the removed attendee (RFC 5546). The .eml DATA To:
    # carries the prior roster (the removed-subset envelope is the unit test's
    # job); the presence of a CANCEL after a removal is the tier_4 assertion.
    assert cancels, diag(
        f"removing an attendee must fan an iMIP CANCEL out; captured methods="
        f"{[_method(r) for r in msgs]}")
    for c in cancels:
        assert EVENT_UID in c.decode("utf-8", "replace"), diag(
            "CANCEL must carry the event UID")

    # ── (C) Organizer DELETEs the event → CANCEL to the remaining roster. ──
    # Address the server-returned canonical href (Location), normalized to a path
    # (emersion returns a relative path, but tolerate an absolute URL form too).
    from urllib.parse import urlsplit
    delete_path = urlsplit(location).path or location
    before = _eml_names(out_dir)
    status, _h, body = _caldav_request(
        rp, "DELETE", delete_path, username=org, password=ORGANIZER_PASSWORD)
    assert status in (200, 204), diag(
        f"organizer DELETE must succeed; got {status}, body={body[:300]!r}")

    msgs = _await_new_emls(out_dir, before, min_count=1)
    cancels = [r for r in msgs if _method(r) == "CANCEL"]
    assert cancels, diag(
        f"deleting the event must fan an iMIP CANCEL out to the roster; captured "
        f"methods={[_method(r) for r in msgs]}")
    for c in cancels:
        to = _to_header(c)
        assert ATTENDEE_BOB in to, diag(
            f"DELETE CANCEL must address the remaining roster (bob); got To: {to!r}")
        assert EVENT_UID in c.decode("utf-8", "replace"), diag(
            "DELETE CANCEL must carry the event UID")


@pytest.mark.feature("calendar-in-standard-apps")
def test_caldav_organizer_autoschedule_in_domain_attendee_delivered_to_inbox(autosched_nest):
    """The in-domain local-delivery short-circuit, in the real image
    (smtp-server.md § Outbound delivery — *In-domain local-delivery
    short-circuit*; caldav-server.md § Server-side auto-schedule).

    An organizer invites an attendee on the deployment's OWN mail domain
    (``dave@localhost``). The MDA gateway hands ``dave`` to
    ``enqueue_outbound_mail`` like any other email-reachable attendee — but
    because ``dave`` resolves to a local mailbox, nest seals + delivers the iMIP
    REQUEST straight into ``dave``'s INBOX instead of enqueuing it for MX relay.

    This is the packaging/supervision acceptance for the fix that no lower tier
    reaches: that the shipped image's MDA → ``enqueue_outbound_mail`` chokepoint
    short-circuits in-domain recipients. **RED before the fix:** ``dave`` would
    be MX-relayed back to the box and ``554``-bounce at the inbound HELO-identity
    gate (the docker-hairpin self-loop) — never landing in his INBOX. **GREEN
    after:** the iMIP REQUEST is in ``dave``'s INBOX and no relay was attempted.
    """
    nest = autosched_nest
    rp = nest["router_port"]
    out_dir = nest["out_dir"]
    name = nest["name"]
    mp = nest["mail_ports"]
    org = f"{ORGANIZER_LOCAL}@{DOMAIN}"
    dave = nest["attendee"]

    def diag(msg: str) -> str:
        return f"{msg}\n\n── bridge diagnostics ──\n{bridge_diag(name)}"

    # Discovery: PROPFIND the calendar-home so the lazy "Personal" calendar is
    # provisioned; extract its collection hex (a bare PUT to an unprovisioned
    # calendar 404s) — exactly what a stock CalDAV client does on first connect.
    propfind_body = (
        '<?xml version="1.0" encoding="utf-8"?>'
        '<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
        "<d:prop><d:resourcetype/><d:displayname/></d:prop></d:propfind>"
    ).encode()
    status, _h, body = _caldav_request(
        rp, "PROPFIND", f"/caldav/{org}/", username=org, password=ORGANIZER_PASSWORD,
        body=propfind_body, depth="1", content_type='application/xml; charset="utf-8"')
    assert status == 207, diag(
        f"PROPFIND of the calendar-home must be 207 multistatus; got {status}, "
        f"body={body[:300]!r}")
    m = re.search(r"/caldav/[^/]+/([0-9a-f]{64})/", body.decode("utf-8", "replace"))
    assert m, diag(
        f"calendar-home PROPFIND must enumerate the lazy Personal calendar; "
        f"body={body[:400]!r}")
    cal_hex = m.group(1)
    event_path = f"/caldav/{org}/{cal_hex}/tier4-autosched-in-domain.ics"

    # Organizer invites the in-domain attendee dave@localhost. The iMIP REQUEST
    # must be SHORT-CIRCUITED to local delivery (no stub-MX relay attempt).
    def _ics_in_domain() -> bytes:
        lines = [
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "PRODID:-//fauna//tier4-autosched-indomain//EN",
            "BEGIN:VEVENT",
            f"UID:{EVENT_UID_IN_DOMAIN}",
            "DTSTAMP:20260601T120000Z",
            "DTSTART:20260815T150000Z",
            "DTEND:20260815T160000Z",
            f"SUMMARY:{EVENT_SUMMARY} (in-domain)",
            f"ORGANIZER:mailto:{org}",
            f"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{ATTENDEE_DAVE}",
            "END:VEVENT",
            "END:VCALENDAR",
        ]
        return ("\r\n".join(lines) + "\r\n").encode()

    before = _eml_names(out_dir)
    status, _headers, body = _caldav_request(
        rp, "PUT", event_path, username=org, password=ORGANIZER_PASSWORD,
        body=_ics_in_domain(), content_type="text/calendar; charset=utf-8")
    assert status in (200, 201, 204), diag(
        f"organizer PUT (in-domain invite) must succeed; got {status}, body={body[:300]!r}")

    # The iMIP REQUEST lands in dave's sealed INBOX (the MDA decrypts server-side
    # at IMAP AUTH, so the client reads plaintext). This poll (≤90 s) also gives
    # any erroneous relay attempt ample time to surface at the stub MX.
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mp[993], DOMAIN, dave["username"], dave["password"])
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(diag(
            f"the in-domain attendee's iMIP REQUEST must land in his INBOX via the "
            f"local short-circuit (NOT MX-relayed); {e}")) from e
    text = raw.decode("utf-8", "replace")
    assert "METHOD:REQUEST" in text, diag(
        f"dave's INBOX message must be the iMIP REQUEST; body head={text[:400]!r}")
    assert EVENT_UID_IN_DOMAIN in text, diag("the REQUEST must carry the event UID")
    assert org in text.lower(), diag("the REQUEST must carry the organizer (From)")

    # No relay was attempted for the in-domain attendee (the whole point: it is
    # delivered locally, never self-SMTP-looped to the box's own MX).
    leaked = sorted(_eml_names(out_dir) - before)
    assert not leaked, diag(
        f"an in-domain attendee must NOT be MX-relayed (no stub-MX delivery); "
        f"leaked .eml(s)={leaked}")

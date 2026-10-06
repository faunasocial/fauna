"""tier_3 (LOCAL): a Fauna app seals + writes an event → a CalDAV MUA must see
it. The local, no-live-box reproduction of **GAP 2**.

The live 3-client run ( example.com) proved the whole reconnect /
discovery / bearer saga works, then failed at Case 1: an event the linux Events
page created was **invisible** to a CalDAV MUA's REPORT. The cross-language path

    client Rust seal (`fauna_client_caldav::seal_and_put_event`)
        → nest `bridge_caldav_events` row
        → Go MDA `QueryCalendarObjects` (`OpenMailRecord` + serve) → MUA

is untested anywhere — it is the symmetric counterpart to the already-fixed
metadata-canonicality asymmetry (`caldav-server.md` § Standard properties;
`TestSealCollectionMetadataIsCanonicalDagCbor`), now on the **event-body / read**
direction:

  - `bins/fauna-nest/tests/conformance_caldav_client.rs` — client seal → nest →
    *client* unseal (pure Rust; no Go MDA).
  (`tests/api/test_caldav.py` — in-core nest CalDAV, plaintext, not the MDA —
  used to sit here; the in-core CalDAV control plane it drove was retired in the
  Decision-B § 4c cleanup and the suite was deleted 2026-07-12.)
  - `tests/test_mail_enable_then_mua_round_trip.py` — MTA seal → MDA serve
    (mail body, not a client-sealed calendar event).

This test closes that gap with the **real client seal** (the linux Events UI, the
exact production write path the 5 other apps will copy) against a **local**
nest + Go MDA, then reads the calendar through the Python `CalDAVClient` MUA —
exactly what macOS Calendar / Thunderbird / Evolution do.

Faithfulness to the live scenario: the linux app is logged in as the nest
admin and writes under its own `keypair.actor_id()`; the MUA authenticates as
`admin@<domain>`, aliased to that same admin actor — so the actor is consistent
on both sides, mirroring production (where `EnableMail` provisions the recipient
under the client's actor). If the event is still invisible with a consistent
actor, the divergence is in the body-seal / serve direction, not the actor.

Test taxonomy: `tier_3` (mocking depth) — every binary real (nest + MTA + MDA),
real client-side seal, real HTTPS CalDAV wire, real `OpenMailRecord` decrypt on
serve. A stub or pure-Rust test can't catch a Rust↔Go wire/seal asymmetry.
"""

import re
import sqlite3
import time
import uuid

import pytest

from helpers.caldav_client import CalDAVClient
from helpers.mail_client_ui import deliver_inbound
from helpers.waiting import wait_until

# Reuse the enable-mail round-trip setup helpers — priority #2 (the canonical
# "log the client in as the dedicated nest's admin + alias it a mail address"
# path, shared with test_caldav_discovery_sequence.py).
from . import test_mail_enable_then_mua_round_trip as rt

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    # tui added 2026-08-21 (marker sets that stopped growing): the
    # Events page's create_event/select_calendar path used here is COMPLETE on
    # tui (ui-actual-tui.yaml), and the shared enable-mail preamble is already
    # proven on tui (test_mail_enable_then_mua_round_trip.py, this file's own
    # import). Green on --app tui before landing.
    pytest.mark.tui,
    # android added 2026-10-05: its mail-settings form paints every element
    # `enable_mail_plain` drives, and the dedicated nest reaches the device
    # through `_relaunch_trusting_nest`'s `adb reverse`.
    pytest.mark.android,
]

# The PLAIN mail credential the client mints and the MUA authenticates with
# (shared IMAP + CalDAV, AEAD-unwrap-as-auth).
_PASSWORD = "ClientSealToMuaPlainPw0006Ff"

# How long to wait for the client-written event to surface to the MUA's REPORT.
_PROP_TIMEOUT = 30.0


def _utc(offset_min: int) -> str:
    """ISO basic-format UTC timestamp `offset_min` minutes from now (the format
    the linux Events form + `build_vevent` use)."""
    t = time.gmtime(time.time() + offset_min * 60)
    return time.strftime("%Y%m%dT%H%M00Z", t)


def _wait_caldav_serving(base_url: str, *, timeout: float = 120.0) -> None:
    """Poll until the MDA's self-signed CalDAV HTTPS listener answers (any HTTP
    status — a 401 challenge counts). Connection-level errors mean it's still
    cold-booting (re-enroll + re-attest + bind)."""
    import requests
    import urllib3

    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            requests.request("PROPFIND", f"{base_url}/caldav/", timeout=6.0, verify=False)
            return
        except Exception as e:  # ConnectionError / SSLError / Timeout while booting
            last = type(e).__name__
            time.sleep(2.0)
    pytest.fail(f"MDA CalDAV endpoint {base_url} never served within {timeout:.0f}s ({last})")


def _wait_calendars_ready(app, *, timeout: float = 90.0) -> None:
    """Re-navigate the Events page until it has loaded at least one calendar
    (the post-enable WS-RPC can race the first `fetch_calendars`)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        app.events.navigate()
        if app.driver.count("calendar-item") > 0:
            return
        time.sleep(3.0)
    pytest.fail(f"client Events page never loaded a calendar within {timeout:.0f}s")


def _read_caldav_db(db_path: str):
    """Return (calendars, events) rows from the nest's encrypted CalDAV store, so
    a failure shows EXACTLY which (actor_id, calendar_id, uid_hash) the client
    wrote under vs what the MUA reads. Returns ([], []) on read error."""
    try:
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            cals = conn.execute(
                "SELECT hex(actor_id), hex(calendar_id) FROM bridge_caldav_calendars"
            ).fetchall()
            evs = conn.execute(
                "SELECT hex(actor_id), hex(calendar_id), hex(uid_hash), hex(record_cid) "
                "FROM bridge_caldav_events"
            ).fetchall()
            return cals, evs
        finally:
            conn.close()
    except Exception:  # noqa: BLE001 — diagnostic, never mask the real assert
        return [], []


def _dump_db(cals, evs) -> str:
    lines = [f"  bridge_caldav_calendars ({len(cals)} rows):"]
    lines += [f"    actor={a}  cal={c}" for a, c in cals] or ["    (none)"]
    lines += [f"  bridge_caldav_events ({len(evs)} rows):"]
    # The sealed body rests in the actor's `__calendar` segment store, addressed
    # by the row's record_cid; the row itself carries no body.
    lines += [
        f"    actor={a}  cal={c}  uid_hash={u}  record_cid={r}" for a, c, u, r in evs
    ] or ["    (none)"]
    return "\n".join(lines)


def _direct_get_probe(base: str, addr: str, password: str, cal_href: str, uid_hash_hex: str) -> str:
    """GET the event at its REAL uid_hash resource path. The MDA's
    GetCalendarObject full-fetches + matches uid_hash, then OpenMailRecord-opens:
    a **404** ⇒ fetchAllEvents returned the row nowhere (empty QueryEvents / Go
    decode dropped it); a **500** ⇒ the row WAS fetched but openEvent's decrypt
    failed (a body-seal asymmetry); a **200** ⇒ the single-event path works and
    only the REPORT/PROPFIND list path is broken. The decisive empty-vs-decrypt
    bisector the unreliable shared log can't give."""
    import requests
    import urllib3
    from requests.auth import HTTPBasicAuth

    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    url = f"{cal_href.rstrip('/')}/{uid_hash_hex}.ics"
    try:
        r = requests.get(url, auth=HTTPBasicAuth(addr, password), verify=False, timeout=10.0)
        return f"  direct GET {url}\n    → HTTP {r.status_code}; body[:300]={r.text[:300]!r}"
    except Exception as e:  # noqa: BLE001
        return f"  direct GET {url} raised {e!r}"


def _tail_mda_log(handle) -> str:
    """The MDA bridge's CalDAV-relevant log lines (query_events / REPORT / decrypt
    Warn) — now readable since the MTA/MDA log collision is fixed.

    The lines come from the VENUE (`bridge_log_lines`), not from a host path this
    helper opens itself: where the MDA's output lives is a fact about how the
    bridges run, and under `testing.md` § Default app and nest mode ruling (3)
    that differs by nest mode. The filtering below is this test's own business
    and stays here."""
    lines = [
        ln for ln in handle.bridge_log_lines("mda")
        if any(
            k in ln.lower()
            for k in ("caldav", "query", "report", "skipping", "undecryptable",
                      "openmailrecord", "snapshot", "capab", "event")
        )
    ]
    if not lines:
        return f"    (no caldav/query lines; {handle.bridge_log_hint('mda')})"
    return "\n".join("    " + ln for ln in lines[-25:])


@pytest.mark.feature("calendar-in-standard-apps")
def test_client_sealed_event_visible_to_mua(app, dedicated_mail_nest, request):
    """A client-Events-page-created (client-sealed) event must be visible to a
    CalDAV MUA's calendar-query REPORT. RED reproduces GAP 2 (the MUA's REPORT
    returns the event nowhere, despite the client UI showing it written).

    Runs on any client whose Events page drives the real seal path (linux +
    windows today; platform selection is in the marker, not the body — e2e rule 7).
    """
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain
    db_path = nest["db_path"]
    admin_actor_hex = bytes(nest["admin"]["signing_key"].verify_key).hex().upper()

    # 1) Log the client in as the nest admin + enable mail through its own
    #    UI (mints the recipient pubkey + wrapped-MSEK + MLS snapshot the MDA
    #    needs to OpenMailRecord, and the msek `caldav_context` seals with).
    rt._login_as_nest_admin(app, nest, rt._dedicated_node_url(app, handle, request))
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()
    admin_addr = rt._alias_admin_to_address(nest, domain)  # admin@<domain>

    base = f"https://127.0.0.1:{handle.caldav_port}"
    _wait_caldav_serving(base)

    # 2) The MUA connects + discovers the Personal calendar (lazy-created under
    #    the admin actor on first PROPFIND).
    mua = CalDAVClient(base, admin_addr, _PASSWORD, verify=False)
    cal_href = mua.personal_calendar()

    # 3) The client's Events page (the REAL client seal path) creates an event on
    #    the same Personal calendar. create_event waits for the event-card count
    #    to increase, so a return means the client write landed + the UI re-read
    #    shows it (rules out an optimistic-UI / silent-PUT-failure cause).
    _wait_calendars_ready(app)
    app.events.select_calendar("Personal")
    nonce = f"seal{int(time.time())}"
    summary = f"{nonce}-from-client"
    app.events.create_event(summary, _utc(60), _utc(120))
    assert any(summary in s for s in app.events.event_summaries()), (
        "sanity: the linux Events page must show the event it just created "
        "(the client seal + put_event_ciphertext succeeded)"
    )

    # 4) The MUA's REPORT must now surface that client-sealed event. This is the
    #    GAP-2 assertion: live, it returned empty.
    deadline = time.monotonic() + _PROP_TIMEOUT
    seen: list[str] = []
    while time.monotonic() < deadline:
        seen = mua.summaries(cal_href)
        if summary in seen:
            break
        time.sleep(2.0)

    if summary not in seen:
        cals, evs = _read_caldav_db(db_path)
        uid_hash_hex = evs[0][2].lower() if evs else ""
        get_probe = (
            _direct_get_probe(base, admin_addr, _PASSWORD, cal_href, uid_hash_hex)
            if uid_hash_hex
            else "  (no event row in DB to GET-probe)"
        )
        pytest.fail(
            f"GAP 2: event {summary!r} created on the linux Events page (client seal) "
            f"never appeared to the CalDAV MUA within {_PROP_TIMEOUT:.0f}s.\n"
            f"  MUA calendar href: {cal_href}\n"
            f"  MUA REPORT summaries: {seen!r}\n"
            f"  admin actor (client write + MUA-session via alias): {admin_actor_hex}\n"
            f"nest encrypted CalDAV store:\n{_dump_db(cals, evs)}\n"
            f"single-event bisector:\n{get_probe}\n"
            f"MDA CalDAV log (tail):\n{_tail_mda_log(handle)}"
        )


def _enable_mail_and_mua(app, handle, request):
    """This module's preamble as one step: sign the app in as the nest admin,
    enable mail through its own UI, and return a serving `CalDAVClient`, the
    Personal calendar's href and the admin's address."""
    nest = handle.nest
    rt._login_as_nest_admin(app, nest, rt._dedicated_node_url(app, handle, request))
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )
    handle.rebind_after_enable()
    admin_addr = rt._alias_admin_to_address(nest, handle.domain)
    base = f"https://127.0.0.1:{handle.caldav_port}"
    _wait_caldav_serving(base)
    mua = CalDAVClient(base, admin_addr, _PASSWORD, verify=False)
    return mua, mua.personal_calendar(), admin_addr


def _settles(predicate, budget_s: float, *, interval: float) -> bool:
    """Deadline-poll `predicate` (convention 14); answer whether it came true,
    leaving the caller's own assertion to name what it last saw."""
    try:
        wait_until(predicate, budget_s, interval=interval)
        return True
    except AssertionError:
        return False


def _attendee_partstats(ics: str) -> dict[str, str]:
    """`{address: PARTSTAT}` for every ATTENDEE of an iCalendar body."""
    unfolded = re.sub(r"\r?\n[ \t]", "", ics)
    out: dict[str, str] = {}
    for line in unfolded.splitlines():
        if not line.upper().startswith("ATTENDEE"):
            continue
        params, _, value = line.partition(":")
        addr = value.strip()
        if addr.lower().startswith("mailto:"):
            addr = addr[len("mailto:"):]
        m = re.search(r";PARTSTAT=([^;:]+)", params, re.IGNORECASE)
        out[addr.lower()] = m.group(1).upper() if m else "NEEDS-ACTION"
    return out


def _app_attendee_status(app, address: str) -> str | None:
    """The `attendee-status` the open event detail shows for `address`."""
    for i in range(app.driver.count("attendee-item")):
        if app.driver.get_text("attendee-id", index=i).strip().lower() == address.lower():
            return app.driver.get_text("attendee-status", index=i)
    return None


@pytest.mark.feature("calendar-in-standard-apps")
def test_interested_shows_as_tentative_and_a_calendar_apps_tentative_stays_tentative(
    app, dedicated_mail_nest, request
):
    """Interested in Fauna → the calendar app sees PARTSTAT=TENTATIVE (and Fauna
    keeps showing Interested); Tentative picked in a calendar app → Fauna shows
    Tentative, never Interested (`caldav-server.md` § RSVP semantics)."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    mua, cal_href, _admin_addr = _enable_mail_and_mua(app, handle, request)

    # ── Fauna Interested → calendar app TENTATIVE ───────────────────────
    _wait_calendars_ready(app)
    app.events.select_calendar("Personal")
    summary = f"interested-{int(time.time())}"
    app.events.create_event(summary, _utc(60), _utc(120))
    app.events.rsvp_event(summary, "interested")
    assert app.events.event_rsvp_count(summary) >= 1, (
        f"the Interested reply must register an attendee; error={app.error_text()!r}"
    )

    ics = None
    partstats: dict[str, str] = {}

    def _roster_seen() -> bool:
        nonlocal ics, partstats
        ev = next((e for e in mua.list_events(cal_href) if e.summary == summary), None)
        ics = ev.ics if ev else None
        partstats = _attendee_partstats(ics or "")
        return bool(partstats)

    _settles(_roster_seen, _PROP_TIMEOUT, interval=2.0)
    assert partstats, f"the calendar app must see the event's attendee; ics={ics!r}"
    assert set(partstats.values()) == {"TENTATIVE"}, (
        f"an Interested reply must reach the calendar app as PARTSTAT=TENTATIVE; "
        f"saw {partstats!r}\n{ics}"
    )
    self_addr = next(iter(partstats))
    status = _app_attendee_status(app, self_addr)
    assert status == "Interested", (
        f"Fauna must keep showing Interested for its own reply; attendee-status "
        f"for {self_addr!r} = {status!r}"
    )

    # ── calendar app Tentative → Fauna Tentative ────────────────────────
    uid = f"tentative-{uuid.uuid4()}"
    guest = f"guest-{uuid.uuid4().hex[:6]}@example.com"
    summary2 = f"tentative-{int(time.time())}"
    dtstart, dtend = _utc(180), _utc(240)
    vevent = "\r\n".join([
        "BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Fauna//e2e-rsvp//EN",
        "BEGIN:VEVENT", f"UID:{uid}", f"DTSTAMP:{dtstart}", f"DTSTART:{dtstart}",
        f"DTEND:{dtend}", f"SUMMARY:{summary2}",
        f"ATTENDEE;PARTSTAT=TENTATIVE:mailto:{guest}",
        "END:VEVENT", "END:VCALENDAR",
    ]) + "\r\n"
    mua.put_event(cal_href, uid, vevent)

    def _shows_in_fauna() -> bool:
        app.events.navigate()
        app.events.select_calendar("Personal")
        return any(summary2 in s for s in app.events.event_summaries())

    _settles(_shows_in_fauna, 60.0, interval=3.0)
    assert any(summary2 in s for s in app.events.event_summaries()), (
        f"the calendar app's event must show in Fauna; saw {app.events.event_summaries()!r}"
    )
    assert app.events.event_rsvp_count(summary2) >= 1, "its attendee must be listed"
    status = _app_attendee_status(app, guest)
    assert status == "Tentative", (
        f"a Tentative picked in a calendar app must show as Tentative in Fauna, "
        f"not Interested; attendee-status={status!r}"
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


def _imip_reply_mail(guest: str, organizer: str, uid: str, dtstart: str, dtend: str) -> bytes:
    """An RFC 6047 iMIP REPLY as a mail app sends it: the guest accepted."""
    ics = "\r\n".join([
        "BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//External//Mail//EN", "METHOD:REPLY",
        "BEGIN:VEVENT", f"UID:{uid}", f"DTSTAMP:{dtstart}", f"DTSTART:{dtstart}",
        f"DTEND:{dtend}", "SEQUENCE:0", f"ORGANIZER:mailto:{organizer}",
        f"ATTENDEE;PARTSTAT=ACCEPTED:mailto:{guest}",
        "END:VEVENT", "END:VCALENDAR",
    ]) + "\r\n"
    nonce = uuid.uuid4().hex[:12]
    return ("\r\n".join([
        f"From: Guest <{guest}>",
        f"To: {organizer}",
        "Subject: Accepted: planning",
        f"Message-ID: <{nonce}@{guest.rsplit('@', 1)[1]}>",
        time.strftime("Date: %a, %d %b %Y %H:%M:%S +0000", time.gmtime()),
        "MIME-Version: 1.0",
        'Content-Type: text/calendar; charset=utf-8; method=REPLY',
        "",
        ics,
    ]) + "\r\n").encode()


def _submit_from_mail_app(handle, sender: str, password: str, rcpt: str, raw: bytes) -> None:
    """Send `raw` the way the guest's own mail app does: implicit-TLS submission
    (port 465), AUTH PLAIN as `sender`, MAIL FROM the same address. The MTA's
    submission door validates that envelope sender as the authenticated actor's
    own and stamps it on the Fauna recipient's copy (`smtp-server.md`
    § Architectural rules → *The `X-Fauna-*` namespace*)."""
    import base64

    from helpers.mail_wire import _connect_submission_tls

    auth = base64.b64encode(b"\x00" + sender.encode() + b"\x00" + password.encode()).decode()
    deadline = time.monotonic() + 40.0
    with _connect_submission_tls(handle.submission_port_465, handle.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {handle.domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


# `real_conversations`: the reply is merged by the mail RECEIVE loop
# (`NestMailInboundSource::spawn_reply_merge`), which the launch-gated apps —
# windows and apple — start only under `FAUNA_E2E_REAL_CONVERSATIONS` (the marker
# sets it; `conftest.py` § the real-conversations env). The test was first
# witnessed on tui, which needs no such gate, while the module marker above had
# already put it on windows, macos and ios: without the loop the reply mail is
# delivered and never read, and the roster stays NEEDS-ACTION (measured on windows
# 2026-09-25, red alone and with its file). Web runs the same shared merge from
# its own receive loop (`WasmConversationsManager::ingest_sealed_inbound` hands
# each non-Junk INBOX record to `apply_inbound_reply_from_mail`), so this one test
# carries a web marker the module does not; the marker is a no-op there (web's
# conversations are a runtime toggle, not a launch gate).
@pytest.mark.real_conversations
@pytest.mark.web
@pytest.mark.feature("calendar-in-standard-apps")
def test_a_reply_by_mail_shows_on_the_organizers_event_in_the_calendar_app(
    app, dedicated_mail_nest, request, run_seal_helper
):
    """The organizer's invitation gets an iMIP REPLY by mail; once the organizer's
    Fauna app has synced it merges the reply, and the organizer's calendar app
    shows the attendee as accepted (`caldav-server.md` § The one operation with
    a cost: applying a `REPLY` to the organizer's stored event).

    **Which door, and why.** A mailed `REPLY` is applied only for the attendee
    the sealed copy's `X-Fauna-Authenticated-Sender` stamp names, and only a
    door that authenticated the sender writes one
    (`inbound-scheduling-authority.md` § The mail rail). The accepting guest is
    therefore a user of the organizer's own mail server whose mail app sends
    the reply through the MTA's **submission door** (port 465, SMTP AUTH),
    which stamps the envelope sender it validated as theirs. The external-
    attendee shape — the MX door, which stamps the `From:` only under a DMARC
    pass — cannot be witnessed here: the native venue's MTA resolves through
    the box's own resolver, where no test domain can publish a DKIM key or a
    DMARC policy. That door's stamp-under-pass is the Go
    `TestMxDataStampsFromOnlyUnderDmarcPass`.

    **The refusal half rides along.** A second, external attendee's REPLY is
    handed to the MX door first, unsigned and with no DMARC record, so the door
    stamps nothing and the app must refuse it (`sender_unauthenticated`). It is
    delivered BEFORE the stamped reply, and the receive loop hands INBOX records
    to the merge in delivery order, so once the stamped reply has merged the
    unstamped one has been handed over too — and that attendee must still be
    `NEEDS-ACTION` (convention 14: the absence is graded against a later
    positive, never a sleep). Each merge runs as its own task, so this half
    cannot prove the refusal finished first; the refusal's own proofs are the
    tier_3 Rust `a_mailed_reply_with_no_stamp_is_refused_as_unauthenticated`
    and `a_mailed_reply_from_someone_else_is_refused…`, and this half witnesses
    the real MX door writing no stamp an app would honour."""
    # The merge runs in the shared mail receive source on every app: the native
    # `NestMailInboundSource::spawn_reply_merge` that linux/tui and every UniFFI
    # app register, and its web twin in `WasmConversationsManager` — both over
    # the one gated `CalDavClient::apply_inbound_reply_from_mail`.
    from tests.platform.docker.helpers import provision_mail_sender

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    mua, cal_href, admin_addr = _enable_mail_and_mua(app, handle, request)

    guest_password = f"guest-{uuid.uuid4().hex[:12]}"
    guest = provision_mail_sender(
        handle.nest, run_seal_helper, domain=handle.domain,
        local_part=f"guest{uuid.uuid4().hex[:6]}", credential=guest_password,
    )["username"]
    stranger = f"stranger-{uuid.uuid4().hex[:6]}@external.test"
    uid = f"reply-merge-{uuid.uuid4()}"
    summary = f"reply-merge-{int(time.time())}"
    dtstart, dtend = _utc(60), _utc(120)
    invite = "\r\n".join([
        "BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Fauna//e2e-reply//EN",
        "BEGIN:VEVENT", f"UID:{uid}", f"DTSTAMP:{dtstart}", f"DTSTART:{dtstart}",
        f"DTEND:{dtend}", f"SUMMARY:{summary}", "SEQUENCE:0",
        f"ORGANIZER:mailto:{admin_addr}",
        f"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{guest}",
        f"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{stranger}",
        "END:VEVENT", "END:VCALENDAR",
    ]) + "\r\n"
    mua.put_event(cal_href, uid, invite)
    before = _attendee_partstats(mua.get_event(cal_href, uid) or "")
    assert before.get(guest) == "NEEDS-ACTION", f"the invitation's roster: {before!r}"
    assert before.get(stranger) == "NEEDS-ACTION", f"the invitation's roster: {before!r}"

    # The Fauna app is running and synced; the replies arrive by mail — the
    # unauthenticated one first.
    _wait_calendars_ready(app)
    deliver_inbound(
        handle.mx_port, handle.domain, stranger, admin_addr,
        _imip_reply_mail(stranger, admin_addr, uid, dtstart, dtend),
        time.monotonic() + 40.0,
    )
    _submit_from_mail_app(
        handle, guest, guest_password, admin_addr,
        _imip_reply_mail(guest, admin_addr, uid, dtstart, dtend),
    )

    after: dict[str, str] = {}

    def _merged() -> bool:
        nonlocal after
        after = _attendee_partstats(mua.get_event(cal_href, uid) or "")
        return after.get(guest) == "ACCEPTED"

    _settles(_merged, 120.0, interval=3.0)
    assert after.get(guest) == "ACCEPTED", (
        f"the guest's reply must show on the organizer's event in the calendar "
        f"app once the Fauna app has synced; roster {after!r}\n"
        f"MDA log (tail):\n{_tail_mda_log(handle)}"
    )
    assert after.get(stranger) == "NEEDS-ACTION", (
        f"a reply no door authenticated must be refused, leaving that attendee "
        f"unanswered; roster {after!r}"
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"

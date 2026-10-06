"""tier_3 — the nest's own CalDAV outcomes, witnessed from a calendar app's seat.

Every test here drives a real `fauna-nest` binary and a real mail-bridge MDA
over the CalDAV wire (and, for scheduling, the IMAP wire) with a scripted
client — the shape a stock calendar app speaks. No Fauna app participates, so
each is a `[nest]` witness (`docs/goal/architecture/feature-catalog.md` § The
two surfaces) for `docs/features/calendar-in-standard-apps.md`.

Goal doc: `docs/goal/behavior/caldav-server.md` — each test names its section.

Users are minted per test through the `dav_user` fixture (conftest — the
production Admin/User-class writers) on the session MDA's domain, so no test
shares a calendar, a lockout bucket or an inbox with another.
"""
from __future__ import annotations

import secrets
import time

import pytest

from helpers import budgets
from helpers.caldav_client import CalDAVClient, build_invite_vevent
from helpers.waiting import wait_until
from helpers.mail_wire import (
    _imap_auth_plain,
    _imap_cmd,
    _imap_uid_fetch_body,
    _imap_uid_search,
    _imaps_connect,
)

pytestmark = pytest.mark.tier_3

# How long a scheduling message may take to cross the nest's delivery: a
# ceiling on a condition poll (helpers.waiting.wait_until), never a sleep —
# every wait below returns the moment the condition holds (convention 14).
_DELIVERY_BUDGET_S = budgets.MAIL_OUTBOUND_CYCLE_S


def _caldav(handle, username: str, password: str) -> CalDAVClient:
    client = CalDAVClient(
        f"https://127.0.0.1:{handle.caldav_port}", username, password, verify=False,
    )
    client.wait_until_serving()
    return client


def _inbox_messages(handle, username: str, password: str) -> list[str]:
    """Every message in the user's INBOX, read once over IMAP."""
    deadline = time.monotonic() + budgets.RPC_ROUNDTRIP_S
    sock, buf = _imaps_connect(handle, deadline)
    try:
        assert _imap_auth_plain(sock, buf, "a1", username, password, deadline) == "OK"
        status, _ = _imap_cmd(sock, buf, "a2", "SELECT INBOX", deadline)
        assert status == "OK", f"SELECT INBOX failed for {username}"
        status, uids = _imap_uid_search(sock, buf, "a3", "ALL", deadline)
        assert status == "OK"
        messages = []
        for n, uid in enumerate(sorted(uids)):
            status, body = _imap_uid_fetch_body(sock, buf, f"f{n}", uid, deadline)
            messages.append(body.decode("utf-8", "replace") if status == "OK" else "")
        return messages
    finally:
        sock.close()


def _inbox_message_containing(handle, username: str, password: str, needles: list[str]) -> str:
    """Poll the user's INBOX until a message carries every needle; return it.
    Fails with what the inbox did hold at the budget."""
    def found():
        return next((m for m in _inbox_messages(handle, username, password)
                     if all(needle in m for needle in needles)), None)

    return wait_until(
        found, _DELIVERY_BUDGET_S, interval=1.0,
        diagnose=lambda: f"no message in {username}'s INBOX carried {needles}; it held:\n"
        + "\n---\n".join(m[:400] for m in _inbox_messages(handle, username, password)),
    )


def _calendar_event_containing(client: CalDAVClient, uid: str) -> str:
    """Poll the user's Personal calendar until the event with ``uid`` is served;
    return its body. Fails with the calendar's contents at the budget."""
    return wait_until(
        lambda: client.get_event(client.personal_calendar(), uid), _DELIVERY_BUDGET_S,
        interval=1.0,
        diagnose=lambda: f"the event {uid} never reached the calendar; it held "
        f"{client.summaries(client.personal_calendar())}",
    )


def _imip_request(organizer: str, attendee: str, uid: str, summary: str) -> bytes:
    """An iMIP invitation as another calendar server mails it (RFC 6047): a
    ``text/calendar; method=REQUEST`` part carrying the organizer's event."""
    ics = build_invite_vevent(
        uid, summary, "20361012T150000Z", "20361012T160000Z", organizer, [attendee],
    ).replace("VERSION:2.0\r\n", "VERSION:2.0\r\nMETHOD:REQUEST\r\n")
    return (
        "\r\n".join([
            f"From: {organizer}",
            f"To: {attendee}",
            f"Subject: Invitation: {summary}",
            f"Message-ID: <{uid}@external.test>",
            "Date: Mon, 06 Jul 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/calendar; charset=utf-8; method=REQUEST",
            "",
        ])
        + "\r\n" + ics
    ).encode()


# ── calendar-in-standard-apps 19 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_an_invitation_mailed_from_another_server_lands_on_the_calendar(
    mail_bridge_inbound_to_imap,
):
    """`caldav-server.md` § Server-side auto-schedule — "Inbound invite": an
    organizer on another server mails an iMIP ``REQUEST``; the MTA seals it to
    the recipient and places it on their calendar, where any calendar app sees it.
    """
    from helpers.mail_wire import _connect_smtp_starttls

    handle = mail_bridge_inbound_to_imap
    organizer = "organizer@external.test"
    uid = f"mailed-invite-{secrets.token_hex(6)}"
    summary = "Quarterly planning"
    deadline = time.monotonic() + 60
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{organizer}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{handle.recipient_username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(_imip_request(organizer, handle.recipient_username, uid, summary))
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    calendar_app = _caldav(
        handle.mda, handle.recipient_username, handle.recipient_password,
    )
    event = _calendar_event_containing(calendar_app, uid)
    for want in (f"SUMMARY:{summary}", f"ORGANIZER:mailto:{organizer}",
                 f"mailto:{handle.recipient_username}"):
        assert want in event.replace("\r\n ", ""), (
            f"the placed invitation lost {want!r}:\n{event}"
        )
    assert "METHOD:" not in event, (
        f"a calendar stores the event, not the scheduling message:\n{event}"
    )


@pytest.mark.feature("calendar-in-standard-apps")
def test_an_invitation_from_a_colleague_on_the_same_nest_lands_on_the_calendar(
    mail_bridge_mda, dav_user,
):
    """The same outcome when the organizer is on this nest's own domain: their
    calendar app's invitation is delivered by the nest itself (it never crosses
    the MTA), and the nest places it on the attendee's calendar too.
    """
    handle = mail_bridge_mda
    org_user, org_pw, _ = dav_user("cal-org")
    att_user, att_pw, _ = dav_user("cal-att")
    uid = f"colleague-invite-{secrets.token_hex(6)}"
    summary = "Design critique"

    organizer = _caldav(handle, org_user, org_pw)
    organizer.put_event(
        organizer.personal_calendar(), uid,
        build_invite_vevent(uid, summary, "20361019T090000Z", "20361019T100000Z",
                            org_user, [att_user]),
    )

    event = _calendar_event_containing(_caldav(handle, att_user, att_pw), uid)
    assert f"SUMMARY:{summary}" in event, f"the invitation lost its title:\n{event}"
    assert f"ORGANIZER:mailto:{org_user}" in event.replace("\r\n ", ""), event


# ── calendar-in-standard-apps 20 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_answering_an_invitation_in_a_calendar_app_replies_to_the_organizer(
    mail_bridge_mda, dav_user,
):
    """`caldav-server.md` § Server-side auto-schedule — "Responding": a user who
    answers an invitation in any calendar app has their answer sent to the
    organizer as an iMIP ``REPLY``, with no Fauna app in the loop.

    A stock calendar app answers by re-storing the event with its own
    ``PARTSTAT`` changed. The organizer's inbox must receive a ``REPLY``
    carrying exactly the attendee's answer for that event.
    """
    handle = mail_bridge_mda
    org_user, org_pw, _ = dav_user("cal-org")
    att_user, att_pw, _ = dav_user("cal-att")
    uid = f"reply-witness-{secrets.token_hex(6)}"
    invite = build_invite_vevent(
        uid, "Budget review", "20361005T090000Z", "20361005T100000Z",
        org_user, [att_user],
    )

    organizer = _caldav(handle, org_user, org_pw)
    organizer.put_event(organizer.personal_calendar(), uid, invite)

    # The attendee's calendar app accepts: the same event, their line ACCEPTED.
    attendee = _caldav(handle, att_user, att_pw)
    answered = invite.replace(
        f"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{att_user}",
        f"ATTENDEE;PARTSTAT=ACCEPTED:mailto:{att_user}",
    )
    assert answered != invite, "sanity: the attendee line must have been rewritten"
    attendee.put_event(attendee.personal_calendar(), uid, answered)

    reply = _inbox_message_containing(
        handle, org_user, org_pw, ["METHOD:REPLY", f"UID:{uid}"],
    )
    assert "method=REPLY" in reply, f"the reply is not a text/calendar REPLY:\n{reply}"
    assert f"PARTSTAT=ACCEPTED:mailto:{att_user}" in reply.replace("\r\n ", ""), (
        f"the organizer's REPLY does not carry the attendee's acceptance:\n{reply}"
    )
    assert reply.count("ATTENDEE") == 1, (
        f"a REPLY must name only the answering attendee (RFC 5546 §3.2.3):\n{reply}"
    )


# ── helpers for the per-outcome witnesses below ──────────────────────────────


def _event(uid: str, summary: str, dtstart: str = "20361102T100000Z",
           dtend: str = "20361102T110000Z", extra: str = "") -> str:
    """A minimal valid VEVENT (UID/DTSTAMP/DTSTART present), with optional extra
    property lines spliced before END:VEVENT."""
    return (
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fauna//e2e-outcomes//EN\r\n"
        "BEGIN:VEVENT\r\n"
        f"UID:{uid}\r\nDTSTAMP:20360101T000000Z\r\nDTSTART:{dtstart}\r\nDTEND:{dtend}\r\n"
        f"SUMMARY:{summary}\r\n{extra}"
        "END:VEVENT\r\nEND:VCALENDAR\r\n"
    )


def _calendar_id(cal_href: str) -> bytes:
    """The 32-byte calendar id a collection href carries (its last hex segment)."""
    return bytes.fromhex(cal_href.rstrip("/").rsplit("/", 1)[1])


def _as_owner(nest_instance, actor):
    """A WS-RPC session signed by the user's own identity — a Fauna app's seat,
    with no app in it."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


# ── calendar-in-standard-apps 16 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_a_password_is_never_accepted_over_an_unencrypted_connection(mail_bridge_mda):
    """`caldav-server.md` § Don't do these: "Don't allow Basic Auth over cleartext
    HTTP. The CalDAV path is HTTPS-only; the `Authorization` header is rejected
    pre-TLS."

    A calendar app that sends its password without TLS is refused at the
    protocol layer before any credential is looked at: the TLS listener either
    closes the connection or answers the plaintext-on-TLS 400 — never a 2xx,
    never even the 401 challenge that would mean the password was evaluated.
    """
    import base64
    import socket

    handle = mail_bridge_mda
    creds = base64.b64encode(
        f"{handle.recipient_username}:{handle.recipient_password}".encode()
    ).decode()
    request = (
        f"PROPFIND /caldav/{handle.recipient_username}/ HTTP/1.1\r\n"
        f"Host: 127.0.0.1:{handle.caldav_port}\r\n"
        f"Authorization: Basic {creds}\r\nDepth: 0\r\nContent-Length: 0\r\n"
        "Connection: close\r\n\r\n"
    ).encode()
    with socket.create_connection(("127.0.0.1", handle.caldav_port), timeout=budgets.RPC_ROUNDTRIP_S) as sock:
        sock.sendall(request)
        reply = b""
        while chunk := sock.recv(4096):
            reply += chunk
    status_line = reply.split(b"\r\n", 1)[0].decode("latin-1", "replace")
    assert reply == b"" or status_line.split()[1:2] == ["400"], (
        "cleartext HTTP carrying a password must be refused at the protocol "
        "layer (a closed connection, or the TLS listener's plaintext 400); got "
        f"{status_line!r}"
    )
    assert b"WWW-Authenticate" not in reply, (
        "the cleartext request reached credential evaluation — its password was "
        f"looked at over an unencrypted connection:\n{reply[:600]!r}"
    )

    # Sanity: the same request over TLS is served, so the refusal above is
    # about encryption, not a broken listener.
    client = _caldav(handle, handle.recipient_username, handle.recipient_password)
    assert client.raw("PROPFIND", client.home, headers={"Depth": "0"}).status_code == 207


# ── calendar-in-standard-apps 17 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_the_later_of_two_concurrent_edits_is_told_the_event_changed(
    mail_bridge_mda, dav_user,
):
    """`caldav-server.md` § Write surface: "`If-Match` mismatch → `412
    Precondition Failed`". Two calendar apps load the same event; the first
    saves; the second saves against the copy it loaded and is refused rather
    than silently overwriting the first app's edit.
    """
    handle = mail_bridge_mda
    user, pw, _ = dav_user("cal-412")
    app_a = _caldav(handle, user, pw)
    app_b = _caldav(handle, user, pw)
    cal = app_a.personal_calendar()
    uid = f"concurrent-{secrets.token_hex(6)}"
    app_a.put_event(cal, uid, _event(uid, "Original title"))
    loaded = app_a.etag_for_uid(cal, uid)
    assert loaded, "sanity: a stored event must carry an ETag"
    assert app_b.etag_for_uid(cal, uid) == loaded, "sanity: both apps loaded one version"

    href = app_a.href_for_uid(cal, uid)
    first = app_a.raw("PUT", href, headers={"If-Match": loaded, "Content-Type": "text/calendar"},
                      data=_event(uid, "Edited in app A"))
    assert first.status_code in (200, 201, 204), f"the first save must land: {first.status_code}"

    second = app_b.raw("PUT", href, headers={"If-Match": loaded, "Content-Type": "text/calendar"},
                       data=_event(uid, "Edited in app B"))
    assert second.status_code == 412, (
        f"the later save against a stale copy must be refused 412; got {second.status_code}"
    )
    assert "SUMMARY:Edited in app A" in (app_b.get_event(cal, uid) or ""), (
        "the refused save must leave the first app's edit in place"
    )
    stale_delete = app_b.raw("DELETE", href, headers={"If-Match": loaded})
    assert stale_delete.status_code == 412, (
        f"deleting from a stale copy must be refused too; got {stale_delete.status_code}"
    )


# ── calendar-in-standard-apps 18 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_a_broken_event_is_refused_and_an_oversized_one_is_refused_as_too_large(
    mail_bridge_mda, dav_user,
):
    """`caldav-server.md` § iCalendar parsing rules / § Size cap: a body that is
    not iCalendar, and a VEVENT missing a required property, are refused 400 —
    never stored half-understood; a body over the resource size cap is refused
    413. Nothing refused appears on the calendar.
    """
    handle = mail_bridge_mda
    user, pw, _ = dav_user("cal-400")
    app = _caldav(handle, user, pw)
    cal = app.personal_calendar()
    put = lambda slug, body: app.raw(  # noqa: E731
        "PUT", f"{cal}{slug}.ics", headers={"Content-Type": "text/calendar"}, data=body,
    )

    garbage = put("garbage", "this is not a calendar\r\n")
    assert garbage.status_code == 400, f"a non-iCalendar body must 400; got {garbage.status_code}"

    no_start = _event("broken-1", "No start").replace("DTSTART:20361102T100000Z\r\n", "")
    missing = put("broken-1", no_start)
    assert missing.status_code == 400, (
        f"a VEVENT without DTSTART must 400; got {missing.status_code}: {missing.text[:300]}"
    )

    too_big = _event("too-big-1", "Too big",
                     extra="DESCRIPTION:" + "x" * (16 * 1024 * 1024 + 1) + "\r\n")
    oversized = put("too-big-1", too_big)
    assert oversized.status_code == 413, (
        f"a body over the 16 MiB resource cap must 413; got {oversized.status_code}"
    )

    assert app.list_events(cal) == [], (
        f"nothing refused may reach the calendar; it holds {app.summaries(cal)}"
    )


# ── calendar-in-standard-apps 21 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_editing_in_a_calendar_app_keeps_what_only_fauna_knows(
    mail_bridge_mda, nest_instance, dav_user,
):
    """`caldav-server.md` § Event resources, sidecar invariant 2: "A MUA write
    (`PUT` carries no sidecar) must **preserve** the existing
    `encrypted_fauna_ext`". A Fauna app stores its refinement (an Interested
    reply rides there) beside the event; a calendar app then edits the event;
    the Fauna app reads the refinement back unchanged, attached to the edit.
    """
    handle = mail_bridge_mda
    user, pw, actor = dav_user("cal-ext")
    app = _caldav(handle, user, pw)
    cal = app.personal_calendar()
    cal_id = _calendar_id(cal)
    uid = f"sidecar-{secrets.token_hex(6)}"
    app.put_event(cal, uid, _event(uid, "Before the edit"))

    def stored_entry(ws):
        reply = ws.call("fauna.bridges.query_events", {
            "actor_id": actor["actor_id_bytes"], "calendar_id": cal_id,
            "since_modseq": None, "after_event_id": None, "limit": 0,
        })
        assert reply["outcome"] == "ok", reply
        (entry,) = reply["events"]
        return entry

    with _as_owner(nest_instance, actor) as fauna_app:
        entry = stored_entry(fauna_app)
        # The Fauna app's write: the same sealed body plus its sealed sidecar
        # (a sealed envelope stands in for the refinement — nest stores it
        # opaquely, which is exactly the property under test).
        sidecar = bytes(entry["encrypted_body"])
        fauna_app.call("fauna.bridges.put_event_ciphertext", {
            "actor_id": actor["actor_id_bytes"], "calendar_id": cal_id,
            "uid_hash": entry["uid_hash"], "encrypted_body": entry["encrypted_body"],
            "encrypted_index_hint": entry["encrypted_index_hint"],
            "timestamp": int(time.time()), "ciphertext_size": len(entry["encrypted_body"]),
            "if_match": None, "encrypted_fauna_ext": sidecar,
        })

        # The calendar app's edit carries no sidecar.
        app.put_event(cal, uid, _event(uid, "After the calendar app's edit"))
        assert "SUMMARY:After the calendar app's edit" in (app.get_event(cal, uid) or "")

        after = stored_entry(fauna_app)
    assert after["etag"] != entry["etag"], "sanity: the calendar app's edit must be a new version"
    assert bytes(after.get("encrypted_fauna_ext") or b"") == sidecar, (
        "a calendar app's edit dropped or altered what only Fauna knows about the event"
    )


# ── calendar-in-standard-apps 22 ─────────────────────────────────────────────


def _age_tombstones(nest_instance, actor, kind: str, days: int) -> int:
    """Move every one of the user's `kind` tombstones `days` into the past
    through the test-hooks nest — the retention window is days long (7-day
    floor), so a test cannot wait it out. Returns the rows moved."""
    import json
    import urllib.request

    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/test/content/age_dav_tombstones",
        data=json.dumps({"actor_id": actor["actor_id_hex"], "kind": kind,
                         "days": days}).encode(),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    with urllib.request.urlopen(req, timeout=budgets.RPC_ROUNDTRIP_S) as resp:
        return json.loads(resp.read())["aged"]


@pytest.mark.feature("calendar-in-standard-apps")
def test_a_calendar_app_away_too_long_is_sent_to_a_full_resync(
    mail_bridge_mda, nest_instance, dav_user,
):
    """`caldav-server.md` § Stale sync-token handling: a token that predates the
    tombstone-retention window gets the RFC 6578 `DAV:valid-sync-token`
    refusal, never a delta that silently omits deletions; the full re-sync it
    directs the app to is complete. Only the tombstones' age is moved; the
    retention check is the real handler's.
    """
    from helpers.caldav_client import CalDAVError

    handle = mail_bridge_mda
    user, pw, actor = dav_user("cal-stale")
    app = _caldav(handle, user, pw)
    cal = app.personal_calendar()
    kept, gone = f"kept-{secrets.token_hex(4)}", f"gone-{secrets.token_hex(4)}"
    app.put_event(cal, gone, _event(gone, "Deleted while away"))
    _changed, _removed, token = app.sync(cal)
    assert token, "sanity: the first sync must mint a token"

    app.put_event(cal, kept, _event(kept, "Still here"))
    app.delete_event(cal, gone)
    assert _age_tombstones(nest_instance, actor, "calendar", 400) >= 1, (
        "sanity: the deletion must have left a tombstone"
    )

    with pytest.raises(CalDAVError) as stale:
        app.sync(cal, token)
    assert "valid-sync-token" in str(stale.value), (
        f"a token past retention must be refused with DAV:valid-sync-token; got {stale.value}"
    )
    # The full re-sync may also list the deletion (a removed href is harmless on
    # a full sync); what it must hold is the whole calendar as it now stands.
    changed, _removed, fresh = app.sync(cal)
    assert fresh, "the full re-sync must mint a usable token"
    assert sorted(e.uid for e in changed) == [kept], (
        f"the full re-sync must hold exactly the calendar as it is: {[e.uid for e in changed]}"
    )


# ── calendar-in-standard-apps 23 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_after_a_restore_a_calendar_app_resyncs_and_its_edits_keep_working(
    mail_bridge_mda, nest_instance, dav_user,
):
    """`caldav-server.md` § Stale sync-token handling, *Restore divergence*: a
    calendar app holding state newer than a restored calendar is refused its
    token (`DAV:valid-sync-token`), re-syncs to what the nest holds, and a
    conditional edit against the re-synced copy lands. The CalDAV twin of
    `tests/api/test_dr_restore.py`'s RPC-level proof, driven from the app's seat.
    """
    from helpers.caldav_client import CalDAVError

    handle = mail_bridge_mda
    user, pw, actor = dav_user("cal-restore")
    app = _caldav(handle, user, pw)
    cal = app.personal_calendar()
    before, after = f"before-{secrets.token_hex(4)}", f"after-{secrets.token_hex(4)}"
    app.put_event(cal, before, _event(before, "In the backup"))

    with _as_owner(nest_instance, actor) as owner:
        snap = owner.call("fauna.filesync.snapshot.create_message_kind", {"kind": "calendar"})
        app.put_event(cal, after, _event(after, "Written after the backup"))
        _changed, _removed, ahead = app.sync(cal)
        assert ahead, "sanity: the app holds a token past the backup"
        owner.call("fauna.filesync.snapshot.restore_message_kind", {
            "snapshot_id": snap["snapshot_id"], "confirm_id": str(snap["snapshot_id"]),
        })

    with pytest.raises(CalDAVError) as stale:
        app.sync(cal, ahead)
    assert "valid-sync-token" in str(stale.value), (
        f"a token ahead of the restored calendar must be refused; got {stale.value}"
    )
    changed, _removed, _token = app.sync(cal)
    assert sorted(e.uid for e in changed) == [before], (
        f"the re-sync must match the restored calendar: {[e.uid for e in changed]}"
    )
    (resynced,) = changed
    edit = app.raw("PUT", resynced.href,
                   headers={"If-Match": resynced.etag, "Content-Type": "text/calendar"},
                   data=_event(before, "Edited after the restore"))
    assert edit.status_code in (200, 201, 204), (
        f"an edit against the re-synced copy must land; got {edit.status_code}"
    )
    assert "SUMMARY:Edited after the restore" in (app.get_event(cal, before) or "")


# ── calendar-in-standard-apps 24 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_a_date_range_holds_every_occurrence_of_a_repeating_event(mail_bridge_mda, dav_user):
    """`caldav-server.md` § Read surface (brief): time-range filtering is
    MDA-local and expands recurrence rules. A weekly event matches every week
    it occurs in — windows far past its first date included — and no week after
    its last; a one-off matches only its own window.
    """
    handle = mail_bridge_mda
    user, pw, _ = dav_user("cal-range")
    app = _caldav(handle, user, pw)
    cal = app.personal_calendar()
    weekly, once = f"weekly-{secrets.token_hex(4)}", f"once-{secrets.token_hex(4)}"
    # Mondays 10:00 UTC from 2036-01-07, five times: Jan 7, 14, 21, 28, Feb 4.
    app.put_event(cal, weekly, _event(weekly, "Weekly sync", "20360107T100000Z",
                                      "20360107T110000Z", extra="RRULE:FREQ=WEEKLY;COUNT=5\r\n"))
    app.put_event(cal, once, _event(once, "One-off", "20360122T100000Z", "20360122T110000Z"))

    for day in ("20360114", "20360128", "20360204"):
        uids = app.uids_in_range(cal, f"{day}T000000Z", f"{day}T235959Z")
        assert uids == [weekly], (
            f"the Monday {day} holds one occurrence of the weekly event and nothing "
            f"else; the range returned {uids}"
        )
    assert app.uids_in_range(cal, "20360122T000000Z", "20360122T235959Z") == [once], (
        "a day holding no occurrence of the weekly event must return only the one-off"
    )
    assert app.uids_in_range(cal, "20360211T000000Z", "20360212T000000Z") == [], (
        "the week after the last occurrence must return nothing"
    )


# ── calendar-in-standard-apps 25 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_moving_an_event_moves_it_and_a_failed_move_leaves_it(mail_bridge_mda, dav_user):
    """`caldav-server.md` § Write surface / § Atomicity rule: MOVE puts the event
    in the destination calendar and removes it from the source; a MOVE that
    cannot complete leaves the source intact.
    """
    handle = mail_bridge_mda
    user, pw, _ = dav_user("cal-move")
    app = _caldav(handle, user, pw)
    home = app.personal_calendar()
    work = app.mkcalendar(f"work-{secrets.token_hex(4)}", displayname="Work")
    uid = f"move-{secrets.token_hex(6)}"
    app.put_event(home, uid, _event(uid, "Moves to Work"))
    source = app.href_for_uid(home, uid)
    name = source.rstrip("/").rsplit("/", 1)[1]

    # A MOVE into a calendar that does not exist fails and must cost nothing.
    missing = app.raw("MOVE", source, headers={
        "Destination": f"{app.home}no-such-calendar-{secrets.token_hex(4)}/{name}",
        "Overwrite": "F",
    })
    assert missing.status_code >= 400, f"a MOVE into nowhere cannot succeed: {missing.status_code}"
    assert app.get_event(home, uid), "a failed MOVE must leave the event where it was"

    moved = app.raw("MOVE", source, headers={"Destination": f"{work}{name}", "Overwrite": "F"})
    assert moved.status_code in (201, 204), (
        f"MOVE between the user's own calendars must succeed; got {moved.status_code} "
        f"{moved.text[:300]}"
    )
    assert "SUMMARY:Moves to Work" in (app.get_event(work, uid) or ""), "the event is not in Work"
    assert app.get_event(home, uid) is None, "the moved event is still in the source calendar"


# ── calendar-in-standard-apps 26 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_events_many_years_ahead_are_accepted(mail_bridge_mda, dav_user):
    """`caldav-server.md` § Don't do these: "Don't reject CalDAV PUTs containing
    future `DTSTART` dates." An event 150 years out is stored and served."""
    handle = mail_bridge_mda
    user, pw, _ = dav_user("cal-future")
    app = _caldav(handle, user, pw)
    cal = app.personal_calendar()
    uid = f"far-future-{secrets.token_hex(6)}"
    app.put_event(cal, uid, _event(uid, "Time capsule", "21760704T120000Z", "21760704T130000Z"))
    assert "DTSTART:21760704T120000Z" in (app.get_event(cal, uid) or ""), (
        "a far-future event must be stored and served back"
    )


# ── calendar-in-standard-apps 27 ─────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_no_one_else_on_the_nest_can_read_or_change_your_calendar(
    mail_bridge_mda, nest_instance, dav_user,
):
    """`caldav-server.md` § Architectural rules: no caller reaches another
    actor's calendars. Another user's calendar app — even one that knows the
    exact paths — can neither read nor write this user's calendar, and the
    nest admin's own seat is refused the calendar RPCs for them.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mda
    owner_user, owner_pw, owner = dav_user("cal-owner")
    other_user, other_pw, _ = dav_user("cal-other")
    mine = _caldav(handle, owner_user, owner_pw)
    cal = mine.personal_calendar()
    uid = f"private-{secrets.token_hex(6)}"
    mine.put_event(cal, uid, _event(uid, "Private appointment"))
    href = mine.href_for_uid(cal, uid)

    theirs = _caldav(handle, other_user, other_pw)
    for method, url, headers, body in (
        ("PROPFIND", mine.home, {"Depth": "1"}, None),
        ("GET", href, None, None),
        ("REPORT", cal, {"Depth": "1"},
         '<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
         '<d:prop><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"/>'
         "</c:filter></c:calendar-query>"),
        ("PUT", href, {"Content-Type": "text/calendar"}, _event(uid, "Overwritten")),
        ("DELETE", href, None, None),
    ):
        resp = theirs.raw(method, url, headers=headers, data=body)
        # Refused outright, or — for a listing — answered with an empty
        # multistatus that names nothing (the MDA's home-set guard).
        empty_listing = resp.status_code == 207 and "response>" not in resp.text
        assert resp.status_code in (403, 404) or empty_listing, (
            f"another user's {method} of this calendar must be refused; got "
            f"{resp.status_code}: {resp.text[:300]}"
        )
        assert "Private appointment" not in resp.text, f"{method} leaked the event"

    admin = nest_instance["admin"]
    admin_ws = WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws, pytest.raises(Exception) as refused:
        admin_ws.call("fauna.bridges.query_events", {
            "actor_id": owner["actor_id_bytes"], "calendar_id": _calendar_id(cal),
            "since_modseq": None, "after_event_id": None, "limit": 0,
        })
    assert "Private appointment" not in str(refused.value)

    assert "SUMMARY:Private appointment" in (mine.get_event(cal, uid) or ""), (
        "the owner's event must be untouched by every refused attempt"
    )

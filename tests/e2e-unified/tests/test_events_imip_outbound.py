"""tier_3: the Fauna app's two outbound scheduling mails reach the wire.

`caldav-server.md` § Scheduling & invitations gives a Fauna app two legs of
iMIP (RFC 6047) that leave the box as ordinary mail:

- **Organizer fan-out** — inviting someone by email on the event detail
  (`attendee-invite-field` + `attendee-invite-button`) mails them a
  `text/calendar; method=REQUEST` invitation (the shared
  `dispatch_imip_request` → `NestImipDispatch`, its email arm the outbound
  submission rail).
- **Responding** — answering, in the app, an invitation an outside organizer
  sent mails a `method=REPLY` carrying your PARTSTAT back to that organizer
  (`imip_reply_for_rsvp` → `EmailClient::send`).

`test_events.py::test_event_invite_attendee` proves the roster write but not the
mail: the dispatch is best-effort and never surfaced (events.md § Errors), so
only the wire can witness it. Both legs here are graded at the stub external MX
the dedicated mail nest's `mta_mx_override` routes `external.test` to — the
outbound capture of `helpers/stub_mx.py`. The far ends are deliberately
off-box: an address on the nest's own domain short-circuits to a local mailbox
and never reaches the outbound queue (caldav-server.md, the in-domain arm).

**The sign-in is the ordinary e2e one** — a session handle with no `@`, the
address resolved from the account registry's `<handle>@<domain>` as on every
relaunch — not a full-address handle: an app that baked its own address from
the attach-time handle sends neither mail on that path, and that is exactly the
regression this module exists to catch (`conversations.md` § State & data
shape → *Self-address: live, never baked*).

The outside organizer's invitation reaches the calendar the way a stock
calendar app puts it there (a CalDAV PUT by the user's own calendar app); what
is witnessed is the Fauna app's answer to it, not the inbound rail, whose
authority rules have their own witnesses.

Test taxonomy: tier_3 — every binary real (nest + MTA + MDA), real client-side
seal, real SMTP wire out to the stub MX. tui, web, linux, android, macos and
ios: the gestures are the shared Events page's, and web reaches the dedicated nest through
its per-test SPA proxy (`dedicated_node_url`).
"""

import re
import time
import uuid

import pytest

from helpers.caldav_client import CalDAVClient
from helpers.caldav_roundtrip import _form_local
from helpers.e2e_session import e2e_login_device_id
from helpers.mail_dedicated_nest import alias_admin_to_address, dedicated_node_url
from helpers.waiting import wait_until

from . import test_caldav_client_seal_to_mua as seal

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    # macOS + iOS: `EventsVM`'s invite and RSVP ride the same shared
    # `FfiCaldavClient.invite_attendee` / `rsvp_event`, whose bodies dispatch
    # the iMIP `REQUEST` / `REPLY` (`libs/fauna-ffi/src/caldav_client.rs`).
    pytest.mark.macos,
    pytest.mark.ios,
]

# Routed to the stub external MX by the fixture's `mta_mx_override`.
EXTERNAL_DOMAIN = "external.test"
# The session handle the app signs in with: a bare localpart, as the registry
# holds it; aliased below so `<handle>@<domain>` is the admin's own mailbox.
HANDLE_LOCAL = "admin"
_PASSWORD = "EventsImipOutboundPlainPw0007Gg"
# How long a best-effort dispatch has to reach the stub MX once the gesture's
# own write has settled: one submission + one relay hop on a loaded box.
_RELAY_BUDGET_S = 60.0


def _unfold(raw: bytes) -> str:
    """The message as text with RFC 5545/5322 folded lines rejoined."""
    return re.sub(r"\r?\n[ \t]", "", raw.decode("utf-8", errors="replace"))


def _captured(stub_mx, predicate) -> str | None:
    """The first stub-MX message whose unfolded text satisfies `predicate`."""
    for raw in stub_mx.messages():
        text = _unfold(raw)
        if predicate(text):
            return text
    return None


def _wait_captured(handle, predicate, what: str, app) -> str:
    found: list[str] = []

    def _seen() -> bool:
        text = _captured(handle.stub_mx, predicate)
        if text is not None:
            found.append(text)
            return True
        return False

    try:
        wait_until(_seen, _RELAY_BUDGET_S, interval=0.5)
    except AssertionError:
        subjects = [
            line for raw in handle.stub_mx.messages()
            for line in _unfold(raw).splitlines() if line.lower().startswith("subject:")
        ]
        pytest.fail(
            f"the stub external MX never received {what} within "
            f"{_RELAY_BUDGET_S:.0f}s. Subjects it did receive: {subjects!r}; "
            f"app error: {app.error_text()!r}; {handle.bridge_log_hint()}"
        )
    return found[0]


def _sign_in_and_enable_mail(app, handle, request) -> tuple[CalDAVClient, str, str]:
    """Sign the app in as the dedicated nest's admin with a bare-localpart
    handle, enable mail through its own UI, alias `admin@<domain>` to the admin,
    and return a serving CalDAV client, the Personal calendar's href and the
    admin's address."""
    from conftest import _relaunch_trusting_nest
    from helpers.connection import wait_until_online

    nest = handle.nest
    admin_sk = nest["admin"]["signing_key"]
    _relaunch_trusting_nest(app.driver, nest)
    app.driver.set_state({
        "session": {
            "authenticated": True,
            # The raw nest URL for a native app; web's per-test SPA proxy.
            "node_url": dedicated_node_url(app, handle, request),
            "secret_hex": admin_sk.encode().hex(),
            "handle": HANDLE_LOCAL,
            "actor_id": bytes(admin_sk.verify_key).hex(),
            "device_id": e2e_login_device_id(app),
        },
        "nav": {"stack": [{"view": "conversations"}]},
    })
    wait_until_online(app.driver)
    if app.driver.is_web():
        # The patch's landing races the SPA's own launch redirect on a fresh
        # seat (`login_as_nest_admin`'s note); the launch has ended now that
        # the connection is live, so ask for the landing again.
        app.driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})

    app.mail_settings.navigate()
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(
        1, timeout=app.mail_settings.ENABLE_SETTLE_S
    ), (
        "enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    handle.rebind_after_enable()
    admin_addr = alias_admin_to_address(nest, handle.domain)
    base = f"https://127.0.0.1:{handle.caldav_port}"
    seal._wait_caldav_serving(base)
    mua = CalDAVClient(base, admin_addr, _PASSWORD, verify=False)
    return mua, mua.personal_calendar(), admin_addr


@pytest.mark.feature("calendar-and-events")
def test_inviting_someone_by_email_mails_them_the_invitation(app, dedicated_mail_nest, request):
    """Invite `guest@external.test` on the event detail → an iMIP REQUEST for
    that event, addressed to the guest and sent from the organizer's own
    address, reaches the outside world."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    _mua, _cal_href, admin_addr = _sign_in_and_enable_mail(app, handle, request)

    seal._wait_calendars_ready(app)
    app.events.select_calendar("Personal")
    summary = f"imip-request-{uuid.uuid4().hex[:8]}"
    app.events.create_event(summary, _form_local(60), _form_local(120))
    guest = f"guest-{uuid.uuid4().hex[:6]}@{EXTERNAL_DOMAIN}"
    app.events.invite_to_event(summary, guest)
    assert app.events.event_rsvp_count(summary) >= 1, (
        f"the invite must add the guest to the roster first; error={app.error_text()!r}"
    )

    text = _wait_captured(
        handle,
        lambda t: summary in t and "METHOD:REQUEST" in t.upper(),
        f"the iMIP REQUEST for {summary!r}",
        app,
    )
    assert re.search(r"content-type:\s*text/calendar;[^\r\n]*method=REQUEST", text, re.I), (
        f"the invitation must be a text/calendar method=REQUEST mail:\n{text}"
    )
    assert re.search(rf"ATTENDEE[^:\r\n]*:mailto:{re.escape(guest)}", text, re.I), (
        f"the REQUEST must roster the invited guest {guest!r}:\n{text}"
    )
    assert re.search(rf"ORGANIZER[^:\r\n]*:mailto:{re.escape(admin_addr)}", text, re.I), (
        f"the REQUEST must name the inviter {admin_addr!r} as ORGANIZER — an empty "
        f"organizer is an app that never resolved its own address:\n{text}"
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.feature("calendar-and-events")
def test_answering_an_outside_organizers_invitation_mails_the_reply_back(
    app, dedicated_mail_nest, request
):
    """An outside organizer's invitation sits on the user's calendar; Going on
    the event detail → an iMIP REPLY with PARTSTAT=ACCEPTED for that event,
    carrying only the user, reaches the organizer."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    mua, cal_href, admin_addr = _sign_in_and_enable_mail(app, handle, request)

    organizer = f"organizer-{uuid.uuid4().hex[:6]}@{EXTERNAL_DOMAIN}"
    uid = f"imip-reply-{uuid.uuid4()}"
    summary = f"imip-reply-{uuid.uuid4().hex[:8]}"
    dtstart, dtend = seal._utc(180), seal._utc(240)
    mua.put_event(cal_href, uid, "\r\n".join([
        "BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//External//Calendar//EN",
        "BEGIN:VEVENT", f"UID:{uid}", f"DTSTAMP:{dtstart}", f"DTSTART:{dtstart}",
        f"DTEND:{dtend}", f"SUMMARY:{summary}", "SEQUENCE:0",
        f"ORGANIZER:mailto:{organizer}",
        f"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{admin_addr}",
        "END:VEVENT", "END:VCALENDAR",
    ]) + "\r\n")

    def _shows_in_fauna() -> bool:
        app.events.navigate()
        app.events.select_calendar("Personal")
        return any(summary in s for s in app.events.event_summaries())

    seal._settles(_shows_in_fauna, 60.0, interval=3.0)
    assert any(summary in s for s in app.events.event_summaries()), (
        f"the outside organizer's invitation must show in Fauna; "
        f"saw {app.events.event_summaries()!r}"
    )

    app.events.rsvp_event(summary, "going")

    text = _wait_captured(
        handle,
        lambda t: uid in t and "METHOD:REPLY" in t.upper(),
        f"the iMIP REPLY for {uid!r}",
        app,
    )
    assert re.search(r"content-type:\s*text/calendar;[^\r\n]*method=REPLY", text, re.I), (
        f"the answer must be a text/calendar method=REPLY mail:\n{text}"
    )
    assert re.search(rf"^To:[^\r\n]*{re.escape(organizer)}", text, re.I | re.M), (
        f"the REPLY must be addressed to the organizer {organizer!r}:\n{text}"
    )
    attendees = re.findall(r"^ATTENDEE[^\r\n]*", text, re.I | re.M)
    assert len(attendees) == 1 and admin_addr.lower() in attendees[0].lower(), (
        f"a REPLY carries exactly the responding attendee {admin_addr!r}; "
        f"got {attendees!r}"
    )
    assert re.search(r"PARTSTAT=ACCEPTED", attendees[0], re.I), (
        f"Going must answer PARTSTAT=ACCEPTED; got {attendees[0]!r}"
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"

"""tier_3: the MDA server-side auto-schedule **mailbox-less sealed rail**, proven
end-to-end through the **real Go MDA binary** (caldav-server.md § Server-side
auto-schedule → "mailbox-less Fauna user (CalDAV enabled, email disabled) →
WS-RPC sealed delivery"; step C5).

What this uniquely proves over the Rust conformance twin
(`bins/fauna-nest/tests/conformance_caldav_scheduling_mailbox_less.rs`): that twin
talks WS-RPC to nest **directly** with an MDA-*like* ephemeral sender — it never
runs the Go MDA process, so it cannot prove the **classification + seal +
delivery happen inside the real `fauna-mail-bridge` MDA**. This test does: a
**stock external CalDAV organizer** (raw CalDAV wire — what Apple Calendar speaks,
no Fauna app) PUTs an invite through the real MDA's CalDAV server to a **mailbox-less**
Fauna attendee, and the MDA's `fanOutScheduling`:

  - classifies `<carol>@fauna.test`: `resolve_recipient` Reject (carol has a handle
    + key packages but **no mail alias**) → `fauna.actor.by_handle` resolves the
    handle → `fauna.conversations.keypackage.fetch` Some → the **sealed rail**;
  - seals a one-off `WelcomeKind::Scheduling` group **itself** (ephemeral MLS
    signer — the MDA never holds the organizer's Ed25519 secret) carrying the iMIP
    REQUEST as its first application message;
  - ships the opaque bytes over the caller-scoped `fauna.bridges.deliver_sealed_scheduling`
    RPC, landing a Scheduling welcome in carol's durable inbox + the iMIP on the
    bound channel — **with no email anywhere** (carol has no mailbox).

This is also the **empirical confirmation of the `by_handle` bridge-reachability
finding**: the gateway hinges on an authenticated bridge connection
reaching `fauna.actor.by_handle`, and C4 made **no nest allowlist change** on the
premise that the per-handler opt-in gate doesn't apply to the discovery handlers.
A real MDA calling `by_handle` end-to-end here is that premise under test — if it
were actually gated, carol would be misclassified onto the email rail (no alias →
bounce) and **no** Scheduling welcome would land, going RED.

## Scope: the sealed DELIVERY through the real MDA (same-nest)

The recipient-side **drain + apply** half (carol's client decrypting the welcome
and materializing the event on her calendar) is proven by the Rust conformance
twin's `mailbox_less_attendee_receives_and_rsvps_over_the_scheduling_rail_same_nest`.
It is **not** re-proven here through a GUI app because a genuinely mailbox-less
attendee has **no MSEK** (the calendar seal key is minted by `enable_mail`, which
would also give her an alias → the *email* rail, defeating the test), so
`NestSchedulingSink::apply_scheduling_imip` no-ops gracefully (the iMIP decrypts,
but the materialize-into-`bridge_caldav` seal is skipped — `fauna-client-conversations`
lib.rs). Wiring the MSEK to `enable_caldav` (so a mailbox-less user materializes
the event) is the independent **CalDAV-only-enablement** track (NEXT § SECOND
TRACK); the GUI materialize proof is sequenced after it. Here we assert the rail
the MDA is responsible for: the sealed welcome + iMIP land in the attendee's inbox.

tier_3: real `fauna-nest` binary + real `fauna-mail-bridge` MDA binary + a raw
CalDAV PUT over the MDA's self-signed HTTPS listener — a stub can't reach this.
"""

from __future__ import annotations

import time
from email.message import EmailMessage
from typing import NamedTuple

import pytest
from nacl.signing import SigningKey

from helpers import budgets
from helpers.caldav_client import CalDAVClient, build_invite_vevent
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
    mint_caldav_mailbox as _mint_caldav_mailbox,
)
from helpers.scheduling_inbox import (
    scheduling_welcomes as _scheduling_welcomes,
    utc_offset as _utc,
    wait_scheduling_welcomes as _wait_scheduling_welcomes,
)
from helpers.succession import register_recovery_kit, succeed_identity
from helpers.waiting import wait_until
from common.auth import register_handled_actor
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
]
# App marks live on each TEST, never here: every test in this module runs on a
# different set of columns, and a module-level app mark silently credits each
# test with a witness on that app whether or not it ever ran there.

# row 290: the second seat's iOS driver used to reuse the ORGANIZER's own
# `_get_ios_setup` simulator device, so `caldav_mailbox_less_attendee_app`'s
# launch()-time uninstall+install tore the organizer's still-running app down
# mid-test — `BridgeDead`/`Connection refused` on the organizer's driver,
# deterministically (`drivers/ios.py:125-127`), not a contention artifact.
# `conftest._get_ios_second_seat_udid` now hands the second seat its own
# ephemeral device instead.
#
# row 293: past that point, `mint_caldav_mailbox`'s 30s poll for
# `caldav_mailbox_reply` still timed out — also NOT contention. iOS's
# `enable_caldav_mailbox` handler always set `TestAgentReplies.caldavMailboxReply`,
# but iOS's `/app/state` provider (`FaunaApp.swift`) never copied it into the
# served state, unlike the macOS twin (`FaunaMacApp.swift:1607-1610`); the
# poll could never succeed at any load. Fixed by adding the same copy to
# iOS's state provider.

# The organizer's chosen mail/CalDAV password (PLAIN credential path), shared with
# the IMAP AEAD blob — the CalDAV client authenticates with it.
_PASSWORD = "MailboxLessAutoSchedPlainPw01Ab"
# The mailbox-less attendee's handle = the local part the organizer invites. The
# MDA `by_handle(<local>)` must resolve it, so it MUST be the registered handle.
_ATTENDEE_HANDLE = "carol"
# How many real key packages carol publishes. The MDA's `keypackage.fetch`
# consumes one per delivery (REQUEST, then CANCEL); publish enough for both plus a
# margin so a pre/post count is observable.
_KP_COUNT = 4


@pytest.mark.feature("calendar-in-standard-apps")
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
def test_mailbox_less_attendee_gets_sealed_scheduling_through_real_mda(
    app, dedicated_caldav_mailbox_less_nest, request
):
    """A stock CalDAV organizer invites a mailbox-less Fauna attendee → the real
    MDA classifies + seals + delivers the iMIP over the WS-RPC scheduling rail;
    the attendee's inbox gains a Scheduling welcome (+ the sealed iMIP on the
    bound channel). A CANCEL-on-removal fans a second sealed delivery.

    RED if the MDA dropped the mailbox-less attendee (the pre-C4 behavior) or
    misclassified her onto the email rail (which would bounce — no alias), so no
    Scheduling welcome would ever land.
    """
    handle = dedicated_caldav_mailbox_less_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain  # fauna.test

    # ── 1. Organizer = the claimed admin, mail-enabled via the client UI (the only
    #       GUI step; it mints the `default` mail credential the CalDAV client
    #       authenticates with — the AEAD blob is shared IMAP + CalDAV). ──
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — the CalDAV listener this test PUTs against
    # only exists on the far side of it.
    handle.rebind_after_enable()
    organizer_addr = alias_admin_to_address(nest, domain)  # admin@fauna.test

    # ── 2. The mailbox-less attendee: a registered Fauna user with a handle +
    #       published real key packages but NO mail alias. Self-service registration
    #       (`create_user_with_handle`) is the e2e mirror of the conformance test's
    #       `register_addressable` (db.create_user_with_handle). ──
    carol = register_handled_actor(nest["port"], handle=_ATTENDEE_HANDLE, domain=domain)
    attendee_addr = f"{_ATTENDEE_HANDLE}@{domain}"  # carol@fauna.test
    stored = conv_api.keypackage_upload(
        nest["port"], carol, conv_api.mint_key_packages(bytes(carol["signing_key"]), _KP_COUNT)
    )
    assert stored == _KP_COUNT, f"carol should have published {_KP_COUNT} key packages, stored={stored}"
    kp_before = conv_api.keypackage_count(nest["port"], carol, carol["actor_id_hex"])
    assert kp_before >= 1, "carol must have ≥1 usable key package so by_handle reports her addressable"
    assert _scheduling_welcomes(conv_api.inbox(nest["url"], carol)) == [], (
        "carol's inbox must start with no scheduling welcomes"
    )

    # ── 3. The stock CalDAV organizer connects over the MDA's self-signed HTTPS
    #       listener and lazy-provisions the Personal calendar (first PROPFIND). ──
    caldav_url = f"https://127.0.0.1:{handle.caldav_port}"
    organizer = CalDAVClient(caldav_url, organizer_addr, _PASSWORD, verify=False)
    organizer.wait_until_serving(timeout=120.0)
    cal = organizer.personal_calendar()

    # ── 4. Organizer PUTs an event inviting the mailbox-less attendee → the real
    #       MDA fans the iMIP REQUEST over the sealed scheduling rail. ──
    nonce = f"mbl{int(time.time())}"
    uid = f"{nonce}@{domain}"
    summary = f"Mailbox-less kickoff {nonce}"
    organizer.put_event(
        cal, uid,
        build_invite_vevent(uid, summary, _utc(60), _utc(120), organizer_addr, [attendee_addr]),
    )

    welcomes = _wait_scheduling_welcomes(nest, carol, at_least=1, timeout=90.0)
    assert len(welcomes) == 1, (
        f"the real MDA must classify the mailbox-less attendee {attendee_addr} onto the "
        f"sealed scheduling rail and deliver exactly one Scheduling welcome — got "
        f"{len(welcomes)}. A miss means the gateway either dropped her (pre-C4), could not "
        f"reach fauna.actor.by_handle (the reachability finding would be false), or "
        f"misrouted her to the email rail (no alias → bounce, no welcome).\n"
        f"  carol inbox (raw): {conv_api.inbox(nest['url'], carol)}\n"
        f"  {handle.bridge_log_hint('mda')}"
    )
    channel_id = welcomes[0]["channel_id"]
    assert channel_id, f"the scheduling welcome must carry the bound channel id; got {welcomes[0]!r}"

    # The sealed iMIP rides the bound channel as one application message (the MDA
    # `channel.send`'d it as the organizer inside `deliver_sealed_scheduling`).
    messages = conv_api.channel_fetch(nest["port"], carol, channel_id, after=0)
    assert len(messages) >= 1, (
        f"the bound scheduling channel {channel_id} must carry the sealed iMIP "
        f"application message; got {len(messages)} message(s)"
    )

    # The MDA fetched (consumed) one of carol's key packages to seal to her — proof
    # the `keypackage.fetch` leg of the classification fired.
    kp_after_request = conv_api.keypackage_count(nest["port"], carol, carol["actor_id_hex"])
    assert kp_after_request == kp_before - 1, (
        f"the MDA must have consumed exactly one of carol's key packages sealing the "
        f"REQUEST (was {kp_before}, now {kp_after_request})"
    )

    # ── 5. CANCEL-on-removal: the organizer removes the only attendee (empty
    #       roster, organizer retained) → the gateway reads the prior roster and
    #       fans an iMIP CANCEL to the dropped attendee over the SAME sealed rail
    #       (a second Scheduling welcome). No new REQUEST (the new roster is empty).
    #       The prior-roster read needs the organizer's MLS snapshot in-session,
    #       present because the step-4 sealing PUT succeeded. ──
    organizer.put_event(
        cal, uid,
        build_invite_vevent(uid, summary, _utc(60), _utc(120), organizer_addr, [], sequence=1),
    )
    welcomes2 = _wait_scheduling_welcomes(nest, carol, at_least=2, timeout=90.0)
    assert len(welcomes2) >= 2, (
        f"removing the mailbox-less attendee must fan a second sealed scheduling delivery "
        f"(the iMIP CANCEL) to her inbox — the scheduling-welcome tally stayed at "
        f"{len(welcomes2)} (was 1 after the REQUEST). Its absence means the CANCEL path's "
        f"mailbox-less classification did not fire.\n"
        f"  {handle.bridge_log_hint('mda')}"
    )


def _wait_event_summary(events_actions, summary: str, *, timeout: float) -> list[str]:
    """Poll the attendee's Events page until an `event-card-summary` contains
    `summary`. The agenda renders from the 10s `CALENDAR_POLL_INTERVAL_SECS`
    calendar poll over the encrypted `bridge_caldav_*` store, so a freshly
    materialized invite appears within a poll cycle of the drain. Returns the last
    observed summaries (for the failure message)."""
    deadline = time.monotonic() + timeout
    summaries: list[str] = []
    while time.monotonic() < deadline:
        summaries = events_actions.event_summaries()
        if any(summary in s for s in summaries):
            return summaries
        time.sleep(3.0)
    return summaries


@pytest.mark.feature("calendar-and-events")
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.web
@pytest.mark.real_conversations
# real_conversations added row 283 (macOS+iOS widening): linux/tui activate
# carol's real engine via the runtime toggle (`enable_real_faunamls`'s
# `runtime_toggle` branch), which needs no marker — but macOS/iOS/windows are
# launch-gate apps that register the real backend unconditionally at process
# launch behind FAUNA_E2E_REAL_CONVERSATIONS, set by conftest's
# `_apply_real_conversations_env` only when the collected session carries this
# marker (`actions/conversations.py::enable_real_faunamls`'s own docstring).
# Without it carol's macOS app launches on the mock backend and never
# publishes a key package — measured: `assert kp >= 1` got 0. Harmless for
# linux/tui, whose activation path does not consult this marker at all.
#
# tui-marked 2026-08-31. The one blocker the 2026-08-30
# comment here named — "step 6 waits ON the page, and tui has neither the 10 s
# calendar poll nor the `fauna.calendar.changed` refresh" — was already false
# when it was written: the push refresh landed (2026-08-21). The
# claim came from reading `apps/fauna-tui/src/events/mod.rs`'s module header,
# whose "NOT built yet" refers to the quick-appearance POLL; tui, like web,
# needs no poll because the push alone carries the while-on-page guarantee.
# Every other step of carol's path was already cross-app, as that comment said.
#
# windows-marked row 168 (2026-09-07), TEST level not module — a module mark
# would also cover `test_mailbox_less_attendee_gets_sealed_scheduling_
# through_real_mda` above (calendar-in-standard-apps outcome 4, an
# MTA-running venue this row does not verify), silently crediting windows
# with a witness that never ran there (the rule the module header now states
# for every app mark). windows'
# Events page has both the `fauna.calendar.changed` push and a backstop poll
# (`EventsPage.xaml.cs:206,217`), so it needs no different wait shape than tui.
#
# web-marked 2026-09-20, TEST level for the same reason.
# web was the LAST column on this rail, and it was short by one link rather than
# by a platform limit: the apply chain has always been WASM-safe
# (`caldav-server.md` § iCalendar parsing rules — only the crate-backed tree is
# native/MDA-only), and the SPA already JOINED `WelcomeKind::Scheduling` channels;
# only the *drain* over them was missing. `libs/fauna-wasm`'s `pollScheduling` now
# drives the same shared `poll_scheduling_feed` the native ticker runs, from the
# SPA's receive tick, so carol's web seat reaches her Events page by the identical
# path — which is what makes this a witness for the column and not a
# harness-shaped stand-in. Her seat is a twin PAGE (own BrowserContext), the
# `alice_second_web_device` idiom.
def test_mailbox_less_attendee_materializes_invite_on_events_page(
    app, dedicated_caldav_only_nest, caldav_mailbox_less_attendee_app, request
):
    """Slice C — the GUI **materialize** proof, end-to-end on a **CalDAV-only nest**.
    A mailbox-less Fauna attendee RECEIVES a server-side auto-schedule invite over
    the WS-RPC sealed rail **and materializes it on her Events page** — the e2e
    mirror of the Rust conformance twin's recipient-side drain+apply half
    (caldav-server.md § Server-side auto-schedule; Slice C).

    This is the realistic mailbox-less topology: a **CalDAV-only deployment** (email
    never enabled). Both the organizer (claimed admin) and the attendee (carol)
    enable CalDAV — minting the shared MSEK + `default` credential **and** the
    canonical `<handle>@<domain>` alias every CalDAV-enabled actor gets *for AUTH*
    (`ensure_canonical_handle_alias` fires on the recipient-pubkey provision
    regardless of email). On an organizer PUT the real MDA classifies carol: because
    the nest is **email-disabled** the classifier SKIPS the resolve_recipient
    short-circuit (the canonical alias would otherwise Resolve → email rail, but no
    MTA/INBOX delivers there) and routes her onto the sealed rail (by_handle +
    keypackage). carol's real engine drains the `WelcomeKind::Scheduling` welcome,
    `NestSchedulingSink::apply_scheduling_imip` finds her `mail.msek`, and
    `CalDavClient::apply_inbound_request` materializes the VEVENT into her lazy
    `Personal` calendar — which her Events page (polling the encrypted
    `bridge_caldav_*` store every 10s) renders.

    RED before this slice's classifier fix: on an email-disabled nest the MDA still
    resolved carol's canonical alias → email rail (no MTA → bounce, no welcome), so
    the invite never reached her. (Unit-pinned by autoschedule_test.go
    `emailDisabledNestLocalToSealedRail`.)
    """
    _invite_carol_onto_her_events_page(
        app, dedicated_caldav_only_nest, caldav_mailbox_less_attendee_app, request,
        nonce_prefix="mblgui",
    )


class _Invite(NamedTuple):
    """What an organizer's invite to carol left behind."""

    # The stock CalDAV client that sent it; None when a Fauna account sent it
    # over the app rail (`_deliver_as_fauna_app`), which holds no DAV session.
    organizer: CalDAVClient | None
    organizer_addr: str
    cal: str
    uid: str
    summary: str
    dtstart: str
    dtend: str


def _ready_carol_for_invites(app, handle, carol_seat, request) -> str:
    """Steps 1–4 of the Slice C journey — everything before an invite is sent:
    the claimed admin of the CalDAV-only nest enables CalDAV (returning the
    admin's organizer address), carol's engine has published key packages, her
    MSEK is minted, and her Events page is open and empty. Asserts each step."""
    nest = handle.nest
    domain = handle.domain  # fauna.test
    carol_app, carol = carol_seat

    # ── 1. Organizer = the claimed admin on a CalDAV-ONLY nest. Enable CalDAV with
    #       a KNOWN password (no mail — the deployment is email-disabled): mints the
    #       shared MSEK + the `default` credential the raw CalDAV client AUTHs with
    #       + the canonical admin alias. ──
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    _mint_caldav_mailbox(app.driver, password=_PASSWORD)
    organizer_addr = alias_admin_to_address(nest, domain)  # admin@fauna.test

    # ── 2. carol's real engine published her key packages at login (private half
    #       held, so the MDA-sealed welcome decrypts). Confirm she's reachable
    #       before the organizer invites her — NO manual upload (a throwaway package
    #       would lack the private half her engine holds). ──
    kp = 0
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        kp = conv_api.keypackage_count(nest["port"], carol, carol["actor_id_hex"])
        if kp >= 1:
            break
        time.sleep(1.0)
    assert kp >= 1, (
        "carol's real engine must publish ≥1 key package at login so the MDA's "
        f"by_handle → keypackage.fetch classification reaches her (got {kp})"
    )

    # ── 3. Mint carol's MSEK and WAIT for it BEFORE the organizer PUTs, so her
    #       NestSchedulingSink finds the key material on the first drain. The
    #       read-only recipe creates the canonical alias (AUTH-only here) but no
    #       submission token — on this email-disabled nest she rides the sealed
    #       scheduling rail regardless of the alias. ──
    _mint_caldav_mailbox(carol_app.driver, password=None)

    # ── 4. carol opens her Events page (starts the 10s encrypted-store calendar
    #       poll). She has no calendars yet, so it starts empty. ──
    carol_app.events.navigate()
    assert carol_app.events.event_summaries() == [], (
        "carol must start with no events before the invite is materialized; "
        f"got {carol_app.events.event_summaries()!r}"
    )
    return organizer_addr


def _invite_carol_onto_her_events_page(
    app, handle, carol_seat, request, *, nonce_prefix: str
) -> _Invite:
    """Steps 1–6 of the Slice C journey, shared by every test that starts from
    an invite already on carol's Events page: the claimed admin of the
    CalDAV-only nest enables CalDAV and invites carol from a stock CalDAV
    client; the real MDA fans the REQUEST over the sealed rail; carol's real
    engine drains it and her Events page shows it. Asserts each step."""
    nest = handle.nest
    domain = handle.domain  # fauna.test
    carol_app, carol = carol_seat
    attendee_addr = f"carol@{domain}"
    organizer_addr = _ready_carol_for_invites(app, handle, carol_seat, request)

    # ── 5. The stock CalDAV organizer connects over the MDA's self-signed HTTPS
    #       listener, lazy-provisions Personal, and PUTs an event inviting carol →
    #       the real MDA fans the iMIP REQUEST over the sealed scheduling rail
    #       (email-disabled nest → no resolve_recipient short-circuit). ──
    caldav_url = f"https://127.0.0.1:{handle.caldav_port}"
    organizer = CalDAVClient(caldav_url, organizer_addr, _PASSWORD, verify=False)
    organizer.wait_until_serving(timeout=120.0)
    cal = organizer.personal_calendar()
    nonce = f"{nonce_prefix}{int(time.time())}"
    uid = f"{nonce}@{domain}"
    summary = f"Mailbox-less GUI kickoff {nonce}"
    dtstart, dtend = _utc(60), _utc(120)
    organizer.put_event(
        cal, uid,
        build_invite_vevent(uid, summary, dtstart, dtend, organizer_addr, [attendee_addr]),
    )

    # ── 6. carol's engine drains the sealed welcome → NestSchedulingSink finds her
    #       MSEK → apply_inbound_request materializes the VEVENT into her lazy
    #       Personal calendar → her Events-page poll renders it. ──
    summaries = _wait_event_summary(carol_app.events, summary, timeout=120.0)
    assert any(summary in s for s in summaries), (
        f"the mailbox-less attendee's Events page must materialize the sealed "
        f"auto-schedule invite {summary!r} (email-disabled-nest classify → sealed "
        f"rail → drain → MSEK-keyed apply_inbound_request → Personal calendar → "
        f"Events poll); got {summaries!r}. A miss means the welcome never drained "
        f"(delivery/classification) or apply_scheduling_imip no-op'd (no MSEK).\n"
        f"  carol inbox (raw): {conv_api.inbox(nest['url'], carol)}\n"
        f"  {handle.bridge_log_hint('mda')}"
    )
    return _Invite(organizer, organizer_addr, cal, uid, summary, dtstart, dtend)


def _imip(method: str, invite: _Invite, attendee_addr: str) -> bytes:
    """An iMIP ``REQUEST`` or ``CANCEL`` for ``invite``, as raw RFC 5322 — the
    UID, ``ORGANIZER`` line and roster ``invite`` names."""
    cancel = method == "CANCEL"
    ics = "\r\n".join([
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Fauna//e2e-caldav-invite//EN",
        f"METHOD:{method}",
        "BEGIN:VEVENT",
        f"UID:{invite.uid}",
        f"DTSTAMP:{invite.dtstart}",
        f"DTSTART:{invite.dtstart}",
        f"DTEND:{invite.dtend}",
        f"SUMMARY:{invite.summary}",
        f"SEQUENCE:{1 if cancel else 0}",
        *(["STATUS:CANCELLED"] if cancel else []),
        f"ORGANIZER:mailto:{invite.organizer_addr}",
        f"ATTENDEE:mailto:{attendee_addr}",
        "END:VEVENT",
        "END:VCALENDAR",
        "",
    ])
    msg = EmailMessage()
    msg["From"] = invite.organizer_addr
    msg["To"] = attendee_addr
    msg["Subject"] = f"{'Cancelled' if cancel else 'Invitation'}: {invite.summary}"
    msg.set_content(ics, subtype="calendar", params={"method": method})
    return msg.as_bytes()


def _cancel_imip(invite: _Invite, attendee_addr: str) -> bytes:
    """The organizer's iMIP ``CANCEL`` for ``invite`` — the same UID, the same
    ``ORGANIZER`` line, the same roster: exactly what anyone holding the invite
    (every co-attendee does) can write."""
    return _imip("CANCEL", invite, attendee_addr)


def _deliver_as_fauna_app(nest, sender: dict, recipient: dict, imip: bytes) -> None:
    """Seal ``imip`` as a one-off scheduling delivery from ``sender`` and
    deliver it to ``recipient`` — fetch one of the recipient's key packages,
    deliver the Welcome, post the message: the calls a Fauna app makes, with
    the nest attesting ``sender`` as the poster."""
    kp = conv_api.keypackage_fetch(nest["port"], sender, recipient["actor_id_hex"])
    assert kp, "the recipient's engine keeps key packages published; the sender needs one"
    channel_id, welcome, envelope = conv_api.mint_scheduling_delivery(
        sender["signing_key"].encode(), kp, imip,
    )
    conv_api.welcome_deliver(
        nest["port"], sender, recipient["actor_id_hex"], channel_id, welcome,
        kind={"type": "scheduling"},
    )
    conv_api.channel_send(nest["port"], sender, channel_id, envelope)


# The refusal line both sinks log (native `tracing::warn!`, web `console.warn`)
# when the shared apply refuses an inbound scheduling message — the observable
# that says "carol's client processed the message and said no" on EVERY app,
# including the ones that do not render the list yet.
#
# ⚠ This used to be the *only* observable, because the events-surface record did
# not exist ("Inbound mutation authorization" gap (1)). It was CLOSED 2026-09-22:
# the record is written by both sinks and tui renders it, so what the USER sees
# now has a witness of its own —
# `test_a_refused_cancel_is_listed_on_the_events_page` below. A log line is the
# machine's account of the refusal; that test is the human's.
_REFUSED_LINE = "refused inbound scheduling iMIP"


def _refusal_logged(driver, reason: str) -> bool:
    """Has the seat's own log (the console ring on web) recorded the shared
    apply's refusal line naming ``reason``?"""
    return any(
        _REFUSED_LINE in ln and reason in ln for ln in driver.app_stderr_text().splitlines()
    )


# The forged-CANCEL journey on a GUI seat — the tier_3 twin of the Rust
# conformance test `a_non_organizer_cannot_cancel_or_rewrite_the_victims_event`
# (`bins/fauna-nest/tests/conformance_caldav_scheduling_mailbox_less.rs`), which
# drives the shared apply directly and so has no browser arm. Here the victim is
# a real app seat and the organizer a real MDA, so the binding comes from the
# gateway's attested delivery and the refusal from the seat's own drain.
#
# web-marked: the column this journey exists for (the browser drain's
# `pollScheduling` → the shared apply, `libs/fauna-wasm`). tui-marked: the
# same journey through the native sink, whose refusal line lands in the app's
# own log (`app_stderr_text`).
@pytest.mark.feature("calendar-and-events")
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.real_conversations
def test_a_co_attendees_forged_cancel_leaves_the_invite_on_the_events_page(
    app, dedicated_caldav_only_nest, caldav_mailbox_less_attendee_app, request
):
    """Only an event's organizer can cancel it on your calendar
    (caldav-server.md § Who may mutate an existing event over the inbound rail).

    carol holds an invite from the CalDAV-only nest's admin. mallory — someone
    carol knows, and who holds the invite's UID and the organizer's address as
    every co-attendee does — sends carol a CANCEL byte-identical to the
    organizer's, sealed by the same builder and delivered over the same
    scheduling rail. carol's client refuses it: the nest attests mallory as the
    poster, and the event is bound to the organizer who created it. The event
    stays on her Events page. The organizer's own CANCEL then removes it — the
    rule refuses the forger, not the organizer.

    RED before the rule: any CANCEL naming the UID removed the event.
    """
    handle = dedicated_caldav_only_nest
    nest = handle.nest
    domain = handle.domain
    carol_app, carol = caldav_mailbox_less_attendee_app
    attendee_addr = f"carol@{domain}"

    invite = _invite_carol_onto_her_events_page(
        app, handle, caldav_mailbox_less_attendee_app, request, nonce_prefix="mblforge",
    )

    # ── 7. mallory: another account on carol's nest, and one carol has accepted
    #       as a contact — a stranger's scheduling Welcome is held to carol's
    #       inbox mode like any other (only the MDA gateway's rail is exempt), and
    #       the point here is what happens AFTER delivery. ──
    mallory = register_handled_actor(nest["port"], handle="mallory", domain=domain)
    conv_api.accept_contact(nest["port"], carol, mallory["actor_id_hex"])

    # ── 8. mallory forges the organizer's CANCEL: fetch one of carol's key
    #       packages, seal the iMIP as a one-off scheduling delivery, deliver the
    #       Welcome and post the message — the two calls a Fauna app makes. ──
    _deliver_as_fauna_app(nest, mallory, carol, _cancel_imip(invite, attendee_addr))

    # ── 9. carol's client drains it and REFUSES it — the positive witness that
    #       the forged CANCEL was processed — and the event is still there. ──
    wait_until(
        lambda: _refusal_logged(carol_app.driver, "NotTheOrganizer"),
        budgets.RECEIVE_CYCLE_S,
        interval=2.0,
        diagnose=lambda: (
            "carol's client must drain mallory's forged CANCEL and refuse it as "
            "NotTheOrganizer (the event is bound to the organizer the MDA delivered "
            "for; the nest attests mallory). No refusal line means the message never "
            "drained, or was applied.\n"
            f"  carol inbox (raw): {conv_api.inbox(nest['url'], carol)}\n"
            f"  carol log tail:\n{carol_app.driver.app_stderr_text()[-4000:]}"
        ),
    )
    summaries = carol_app.events.event_summaries()
    assert any(invite.summary in s for s in summaries), (
        f"the refused CANCEL must leave {invite.summary!r} on carol's Events page; "
        f"got {summaries!r}"
    )

    # ── 10. The organizer withdraws the meeting (a DELETE through the MDA fans
    #        the real CANCEL over the sealed rail): the event leaves carol's page.
    invite.organizer.delete_event(invite.cal, invite.uid)
    wait_until(
        lambda: not any(invite.summary in s for s in carol_app.events.event_summaries()),
        120.0,
        interval=3.0,
        diagnose=lambda: (
            f"the organizer's own CANCEL must remove {invite.summary!r} from carol's "
            f"Events page; got {carol_app.events.event_summaries()!r}.\n"
            f"  carol log tail:\n{carol_app.driver.app_stderr_text()[-4000:]}\n"
            f"  {handle.bridge_log_hint('mda')}"
        ),
    )


# The succeeded-organizer journey on a GUI seat — the tier_3 twin of the Rust
# conformance test `a_succeeded_organizers_cancel_is_honoured_through_the_
# production_sink` (`bins/fauna-nest/tests/conformance_caldav_scheduling_
# mailbox_less.rs`), which pins the native sink with nothing injected. The
# forged-CANCEL journey above witnesses the rule's LOCAL comparison on web; this
# one witnesses the branch that comparison hands off to when it misses —
# `MemoizedSuccessionResolver::resolve_successor` over web's
# `WebOrganizerSuccessionDialer`, run inside `pollScheduling`'s
# drain: its blank-home fold onto `WsRpcClient::nest_url()` (here the SPA proxy
# the tab reaches its nest through), its read of the held chain head from the
# `fauna.state.succession-ledger` plane, and `succession_witness.rs`'s anonymous dial and walk under its 15 s
# budget. Any one of those failing answers *no answer*, which refuses the
# successor too — so the event leaving the page is the witness that all of them
# held.
#
# The organizer is a Fauna ACCOUNT rather than the MDA's admin: a succession
# needs a registered RecoveryKey and retires the key it succeeds, which the admin
# seat driving this session cannot give up. Its REQUEST and CANCELs ride the rail
# a Fauna app uses (`_deliver_as_fauna_app`), so the nest attests each poster and
# the event binds to the organizer's actor id.
#
# tui-marked beside web: the same journey through the native resolver
# (`fauna-client-conversations`), from a real app seat.
@pytest.mark.feature("calendar-and-events")
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.real_conversations
def test_a_succeeded_organizers_cancel_removes_the_invite_from_the_events_page(
    app, dedicated_caldav_only_nest, caldav_mailbox_less_attendee_app, request
):
    """An organizer who has taken their account back can still cancel
    (caldav-server.md § Who may mutate an existing event over the inbound rail
    → *A succeeded organizer*).

    dana, a Fauna account on carol's nest, invites carol; carol's client binds
    the event to dana's identity. dana then succeeds her identity with her
    recovery kit, so she speaks under a new actor id the binding has never
    seen. A stranger's CANCEL sent after the succession is still refused — the
    walk admits where the account ended up, not whoever asks — and the
    successor's CANCEL removes the event: carol's client dialled its own nest,
    walked dana's verified succession, and found the author at its end.

    RED when the succession branch answers nothing on the seat's platform: the
    successor's CANCEL is refused as NotTheOrganizer and the event stays.
    """
    handle = dedicated_caldav_only_nest
    nest = handle.nest
    domain = handle.domain
    carol_app, carol = caldav_mailbox_less_attendee_app
    attendee_addr = f"carol@{domain}"

    _ready_carol_for_invites(app, handle, caldav_mailbox_less_attendee_app, request)

    # ── 5. dana: a Fauna account carol has accepted as a contact (a Welcome from
    #       a non-contact is held to carol's inbox mode, like mallory's in the
    #       forged-CANCEL journey), holding a registered RecoveryKey. ──
    dana = register_handled_actor(nest["port"], handle="dana", domain=domain)
    conv_api.accept_contact(nest["port"], carol, dana["actor_id_hex"])
    dana_seed = bytes(dana["signing_key"]).hex()
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=dana["actor_id_hex"], identity_seed_hex=dana_seed
    )

    # ── 6. dana invites carol; carol's drain materializes the event, bound to
    #       dana's attested identity on this nest. ──
    nonce = f"mblheir{int(time.time())}"
    invite = _Invite(
        organizer=None, organizer_addr=f"dana@{domain}", cal="",
        uid=f"{nonce}@{domain}", summary=f"Succeeded-organizer kickoff {nonce}",
        dtstart=_utc(60), dtend=_utc(120),
    )
    _deliver_as_fauna_app(nest, dana, carol, _imip("REQUEST", invite, attendee_addr))
    summaries = _wait_event_summary(carol_app.events, invite.summary, timeout=120.0)
    assert any(invite.summary in s for s in summaries), (
        f"dana's REQUEST must materialize {invite.summary!r} on carol's Events "
        f"page; got {summaries!r}.\n"
        f"  carol inbox (raw): {conv_api.inbox(nest['url'], carol)}\n"
        f"  carol log tail:\n{carol_app.driver.app_stderr_text()[-4000:]}"
    )

    # ── 7. dana succeeds her identity on her home nest — the real ceremony
    #       kinds, the statement verified against her registration chain. ──
    heir_key = SigningKey.generate()
    heir_id = succeed_identity(
        nest["url"],
        old_actor_id_hex=dana["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=bytes(heir_key).hex(),
        old_seed_hex=dana_seed,
    )
    heir = {
        "signing_key": heir_key,
        "actor_id_bytes": bytes(heir_key.verify_key),
        "actor_id_hex": heir_id,
    }
    assert heir["actor_id_bytes"].hex() == heir_id, (
        "the successor's actor id IS its Ed25519 pubkey"
    )

    # ── 8. A stranger's CANCEL, after the succession: still refused — the walk
    #       ends at the heir, not at mallory. ──
    mallory = register_handled_actor(nest["port"], handle="mallory", domain=domain)
    conv_api.accept_contact(nest["port"], carol, mallory["actor_id_hex"])
    _deliver_as_fauna_app(nest, mallory, carol, _cancel_imip(invite, attendee_addr))
    wait_until(
        lambda: _refusal_logged(carol_app.driver, "NotTheOrganizer"),
        budgets.RECEIVE_CYCLE_S,
        interval=2.0,
        diagnose=lambda: (
            "carol's client must drain mallory's CANCEL and refuse it as "
            "NotTheOrganizer: a succession of the bound organizer does not make "
            "mallory its successor. No refusal line means the message never "
            "drained, or was applied.\n"
            f"  carol inbox (raw): {conv_api.inbox(nest['url'], carol)}\n"
            f"  carol log tail:\n{carol_app.driver.app_stderr_text()[-4000:]}"
        ),
    )
    summaries = carol_app.events.event_summaries()
    assert any(invite.summary in s for s in summaries), (
        f"the stranger's refused CANCEL must leave {invite.summary!r} on carol's "
        f"Events page; got {summaries!r}"
    )

    # ── 9. The successor's CANCEL: the nest attests an id the binding has never
    #       seen, and only the verified walk connects the two. The event goes. ──
    conv_api.accept_contact(nest["port"], carol, heir_id)
    _deliver_as_fauna_app(nest, heir, carol, _cancel_imip(invite, attendee_addr))
    wait_until(
        lambda: not any(invite.summary in s for s in carol_app.events.event_summaries()),
        120.0,
        interval=3.0,
        diagnose=lambda: (
            f"dana's successor's CANCEL must remove {invite.summary!r} from carol's "
            f"Events page — carol's resolver walks the bound identity's succession "
            f"at her own nest and admits its final successor; got "
            f"{carol_app.events.event_summaries()!r}. A second NotTheOrganizer "
            f"refusal below means the walk answered nothing (config unreadable, "
            f"own nest undialable, or the walk over budget).\n"
            f"  carol log tail:\n{carol_app.driver.app_stderr_text()[-4000:]}"
        ),
    )


# The SURFACING half of the same journey (`caldav-server.md` § Who may mutate an
# existing event over the inbound rail → *Surfacing*; `ui/events.md` § Refused
# scheduling changes). Its sibling above proves the refusal — the event survives
# — which is the security property; this proves the user is TOLD, which is the
# ruling's other half ("never applied, **never silently discarded**") and the one
# a log line cannot stand in for.
#
# A test of its own rather than three more asserts on the sibling, because the
# two have different app sets: the refusal happens on every app (shared Rust),
# while the LIST is rendered by tui today and trickles down to the other six
# trickles down to the other six. Folding these asserts into the web-marked sibling
# would either red web for a feature it is not owed yet or hide them behind an
# in-test conditional — an invisible skip, which e2e convention 7 exists to
# forbid. So the column set is declared in the markers, where the ratchet can
# see it, and grows as each app lands its render.
@pytest.mark.feature("calendar-and-events")
@pytest.mark.tui
@pytest.mark.real_conversations
def test_a_refused_cancel_is_listed_on_the_events_page(
    app, dedicated_caldav_only_nest, caldav_mailbox_less_attendee_app, request
):
    """The refused change is REPORTED to carol, not just refused.

    Same forgery as the sibling: mallory, a co-attendee holding the UID and the
    organizer's address, sends a CANCEL byte-identical to the organizer's over
    the real sealed rail. Her client refuses it — and her Events page lists what
    was tried, to which event, by whom, and why. She dismisses it; it stays
    dismissed across a re-entry, because the dismissal is recorded in her sealed
    config rather than in the view.

    RED before this slice: the refusal was logged and dropped, so the user never
    learned that someone had tried to cancel their meeting.
    """
    handle = dedicated_caldav_only_nest
    nest = handle.nest
    domain = handle.domain
    carol_app, carol = caldav_mailbox_less_attendee_app
    attendee_addr = f"carol@{domain}"

    invite = _invite_carol_onto_her_events_page(
        app, handle, caldav_mailbox_less_attendee_app, request, nonce_prefix="mblsurf",
    )
    assert carol_app.events.refused_change_count() == 0, (
        "nothing has been refused yet, so the list must be absent entirely — the "
        "ordinary account never sees this section"
    )

    # mallory forges the organizer's CANCEL and delivers it herself, exactly as
    # the sibling does: the two calls a Fauna app makes, with the nest attesting
    # HER as the poster.
    mallory = register_handled_actor(nest["port"], handle="mallory", domain=domain)
    conv_api.accept_contact(nest["port"], carol, mallory["actor_id_hex"])
    _deliver_as_fauna_app(nest, mallory, carol, _cancel_imip(invite, attendee_addr))

    # The row a human reads.
    row = carol_app.events.wait_for_refused_change(
        invite.summary, timeout=budgets.RECEIVE_CYCLE_S
    )
    assert invite.summary in row["title"], (
        f"the row must name the event by the title carol knows it under; got "
        f"{row['title']!r}"
    )
    assert S.events.refused_changes.unknown_sender not in row["author"], (
        "the nest attested mallory as the poster, so the row must name her "
        f"rather than reporting an unidentified sender; got {row['author']!r}"
    )
    assert mallory["actor_id_hex"][:8] in row["author"], (
        "carol has never messaged mallory, so this device resolves no handle for "
        "her and the row falls back to her short actor id — the ATTESTED sender. "
        f"Got {row['author']!r}; a row naming the ORGANIZER instead would mean it "
        "was built from the forged .ics, which carries the organizer's own "
        "ORGANIZER line verbatim."
    )
    assert row["reason"] == S.events.refused_changes.reason.not_the_organizer, (
        f"the row must say WHY, in the user's words; got {row['reason']!r}"
    )
    assert row["time"].strip(), "the row must say when"

    # The event is still hers — the row reports the refusal, it does not replace
    # it — and the list offers no way to apply the change (dismiss is the only
    # control the ruling allows).
    assert any(invite.summary in s for s in carol_app.events.event_summaries()), (
        f"the refused CANCEL must leave {invite.summary!r} on carol's Events page"
    )

    carol_app.events.dismiss_refused_change()
    wait_until(
        lambda: _dismissed(carol_app),
        60.0,
        interval=2.0,
        diagnose=lambda: (
            "a dismissed row must stay dismissed across a re-entry to the page — "
            "the dismissal is recorded in carol's sealed config, not in the view. "
            f"Rows still shown: {carol_app.events.refused_changes()!r}"
        ),
    )
    assert any(invite.summary in s for s in carol_app.events.event_summaries()), (
        "and dismissing the notice must not disturb the event it was about"
    )


def _dismissed(carol_app) -> bool:
    """Re-enter the Events page and answer whether the list is empty — the page
    loads the rows with its calendar hydration, so re-entry is the refresh."""
    carol_app.events.navigate()
    return carol_app.events.refused_change_count() == 0

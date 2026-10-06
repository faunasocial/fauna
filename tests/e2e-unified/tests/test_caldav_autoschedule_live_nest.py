"""tier_4 (live-remote, OPT-IN): server-side CalDAV ``calendar-auto-schedule``
fan-out proven **LIVE against example.com** — the production sibling of the tier_4
``test_caldav_autoschedule_imip.py`` (real local image) and the completion of the
CalDAV-e2e enduring goal (caldav-server.md § Server-side auto-schedule).

What it proves end-to-end on the *real deployed box* (real SNI router on :443,
real publicly-trusted cert, real DNS/MX, real seal — none of which a local image
exercises): a stock CalDAV organizer (raw CalDAV wire — exactly what Apple
Calendar speaks, no Fauna app in the loop) adds an attendee → the MDA's gateway
fans an iMIP **REQUEST** out → the MTA ``LookupMX``-relays it → because
``example.com``'s MX is the box itself, it loops back **inbound** → the inbound
perimeter seals it to the recipient's INBOX → the linux Fauna app sees the
invitation. Then the organizer **removes** the attendee → the gateway fans an iMIP
**CANCEL** the same way → the linux app sees a second scheduling delivery.

── Observation with NO side channel ───────────────────────────────────────────
The live-box constraint (memory ``caldav-live-test-no-side-channel``) forbids any
nest API / SSH / DB peek — the only ways to read box state are the linux app
and the mail/CalDAV protocols. So the attendee is a **sub-address of the
organizer**, ``test+autosched-<nonce>@example.com``:

  - It is **distinct** from the organizer (``test@example.com``), so the gateway's
    ``email_reachable_recipients`` keeps it as a real recipient
    (``fauna_core::ical`` excludes only the organizer's *own exact* address).
  - Sub-addressing is **on by default** (``fauna_mail::aliases``
    ``SUBADDRESSING_ENABLED_DEFAULT = true``), so the loop-back inbound
    ``resolve_recipient``-splits ``test+autosched-… → base test`` and seals into
    the **organizer's own** INBOX — readable in the linux conversations view.

One identity (``test@example.com``) is therefore organizer, attendee-domain *and*
observer at once — no catch-all designation, no second onboarding, no API peek.
The iMIP REQUEST surfaces as a normal inbound mail on the ``Smtp`` rail with
subject ``Invitation: <summary>`` (``fauna_core::ical::imip_subject``); the CANCEL
as ``Cancelled: <summary>``. We assert on the *mail-message tally* carrying the
run nonce (robust to whether REQUEST + CANCEL thread together or land as separate
threads — each delivered iMIP adds one message either way).

── Constraint (identical to test_caldav_live_nest) ─────────────────────────────
NO nest admin/config API; every box change is the linux app UI (onboard,
claim, factory-reset, enable mail, approve bridge) or the CalDAV protocol itself
(the real product surface). The bootstrap is reused verbatim from
``test_mail_zero_cheat_live`` (the canonical UI-only "reach admin → factory-reset
→ enable mail → approve bridge" path), and the CalDAV-listener readiness wait from
``test_caldav_live_nest``.

── Why this is owed beyond tier_4 ──────────────────────────────────────────────
tier_4 routes attendees to a stub-MX sidecar and asserts on the *captured outbound*
— it proves the shipped image fans the iMIP OUT, but it cannot prove the live box's
**MX loop-back + inbound seal + a Fauna app observing the invitation**, nor the
real SNI router actually exposing CalDAV externally, nor the real ACME cert. This
test closes exactly that gap. It is the last remaining piece of this feature's live-nest test coverage.

── Skipped unless these env vars are set (it drives a live external box) ───────
  --nest live:URL --live-box disposable
                            THE OPT-IN — this test is DESTRUCTIVE (it factory-resets the box through the UI), so
                            it runs only on a run that declares its box
                            disposable, which only a staging box can be
                            (testing.md § The shared-box rule → The
                            disposable-box declaration). The box is that run's
                            (``live_box_door.destructive_live_box``) — an
                            exported FAUNA_LIVE_NEST_URL never picks it alone.
  admin seed                resolved per box (``live_box_door.admin_seed``:
                            FAUNA_LIVE_SECRET_HEX > the box's
                            ``~/.config/fauna/staging-box/<host>.json`` >
                            ``~/.fauna-id``); ambient, so never the opt-in.
  mailbox                   resolved per box (``live_box_door.mailbox``):
                            FAUNA_LIVE_MAIL_ADDRESS / FAUNA_LIVE_MAIL_PASSWORD
                            > the box's staging-box file ``handle`` /
                            ``mail_password`` (the identity it was provisioned
                            under); each var overrides its own field.
                            The address is the re-claim handle; the
                            password is the PLAIN one (chosen-pw path).
Optional:
  FAUNA_LIVE_CLAIM_CODE     only if the nest is UNCLAIMED at start
  FAUNA_LIVE_CALDAV_URL     CalDAV root if it differs from https://mail.<domain>
                            (default — the SNI router serves SNI mail.<domain> → MDA)
  FAUNA_CONV_POLL_SECS=5 (export) so the linux app polls the mailbox quickly

⚠ Runtime: a full factory-reset cold-boot **plus** two real MX loop-back inbound
deliveries — and the inbound perimeter **greylists** a first-seen triplet (memory
``live-mail-selftest-benign-failures``: a first inbound can defer 60 s–4 h before
the MTA's retry is accepted). This is the first live test to drive a *real MTA
inbound* on the self-loop (the mail roundtrip tests inject via IMAP APPEND, which
bypasses greylisting), so the REQUEST/CANCEL waits are deliberately generous; a
timeout most likely means the self-loop triplet is greylisted (the loop-back is
SPF+DKIM-authenticated as example.com, so it is *accepted*, only *deferred*). If it
times out, that greylist-on-self-loop behavior is the gap to capture next.
"""

from __future__ import annotations

import os
import time

import pytest

from helpers.app_surface import skip_unbuilt
from helpers import live_box_door
from helpers.caldav_roundtrip import wait_caldav_serving
from helpers.live_admin import approve_pending_bridges, reach_admin_shell, wait_connected

# Reuse the zero-cheat UI-only bootstrap (reach admin → factory-reset → re-claim →
# enable mail → approve bridge: the shared `helpers.live_admin` steps plus that
# sibling's `_reclaim_after_ui_reset`) and the live-CalDAV listener-readiness
# wait — priority #2/#4 (reuse the richest existing path, don't copy). Exactly
# the reuse `test_caldav_live_nest` itself does.
from . import test_mail_zero_cheat_live as setup

# DESTRUCTIVE (see the module docstring): the box is the run's
# declared-disposable one and the admin seed is resolved for THAT box
# (`live_box_door.destructive_live_box` / `admin_seed`). The seed is ambient —
# a staging-box file or `~/.fauna-id` — so it is never the opt-in; the run's
# `--live-box disposable` declaration is (testing.md § The shared-box rule).
URL, _NOT_DISPOSABLE = live_box_door.destructive_live_box()
SECRET, SECRET_SOURCE = live_box_door.admin_seed(URL)

ADDRESS, PASSWORD = live_box_door.mailbox(URL)

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_box,
    pytest.mark.skipif(_NOT_DISPOSABLE is not None, reason=_NOT_DISPOSABLE or ""),
    pytest.mark.skipif(
        not (SECRET and ADDRESS and PASSWORD),
        reason="live CalDAV auto-schedule test: provide the box's admin seed "
        + f"({live_box_door.SEED_SOURCES}) and its mailbox ({live_box_door.MAILBOX_SOURCES})"
        + " to run (drives a live external nest via the linux client; opt-in)",
    ),
]

CLAIM_CODE = os.environ.get("FAUNA_LIVE_CLAIM_CODE", "")
DOMAIN = ADDRESS.split("@", 1)[-1] if "@" in ADDRESS else ADDRESS
LOCAL = ADDRESS.split("@", 1)[0] if "@" in ADDRESS else ADDRESS
# CalDAV is served by the MDA behind the SNI router at SNI `mail.<domain>` (the
# router splices `mail.<domain>`:443 → MDA CalDAV; the apex `<domain>`:443 is
# nest). The box's cert SAN covers `mail.<domain>`, so verify stays on.
CALDAV_URL = os.environ.get("FAUNA_LIVE_CALDAV_URL", f"https://mail.{DOMAIN}").rstrip("/")

# Generous to absorb the inbound greylist on the first-seen self-loop triplet
# (see the module docstring). The REQUEST is the must-pass core proof.
REQUEST_TIMEOUT = float(os.environ.get("FAUNA_AUTOSCHED_REQUEST_TIMEOUT", "600"))
CANCEL_TIMEOUT = float(os.environ.get("FAUNA_AUTOSCHED_CANCEL_TIMEOUT", "600"))


def _utc(offset_min: int) -> str:
    """ISO basic-format UTC `offset_min` minutes from now (whole minutes).
    Uniqueness comes from the SUMMARY nonce, not the time."""
    t = time.gmtime(time.time() + offset_min * 60)
    return time.strftime("%Y%m%dT%H%M00Z", t)


def _mail_tally(app, nonce: str) -> tuple[int, list[str]]:
    """(total iMIP mail messages carrying `nonce`, the matching thread labels) on
    the ``Smtp`` rail. Robust to threading: whether the REQUEST and CANCEL land in
    one thread (message_count grows) or two (two threads each carry the nonce in
    their ``Invitation:`` / ``Cancelled:`` subject), each delivered scheduling
    message adds exactly one to the tally."""
    app.conversations.navigate()
    total = 0
    labels: list[str] = []
    for t in app.conversations.list_threads():
        if t.rail != "Smtp":
            continue
        hay = f"{t.label}\n{t.snippet}".lower()
        if nonce.lower() in hay:
            total += max(1, t.message_count)
            labels.append(t.label)
    return total, labels


def _wait_tally(app, nonce: str, at_least: int, timeout: float) -> tuple[int, list[str]]:
    deadline = time.monotonic() + timeout
    tally, labels = 0, []
    while time.monotonic() < deadline:
        tally, labels = _mail_tally(app, nonce)
        if tally >= at_least:
            return tally, labels
        time.sleep(4.0)
    return tally, labels


@pytest.mark.feature("calendar-in-standard-apps")
def test_live_caldav_autoschedule_fans_imip(app):
    """Organizer test@example.com PUTs a CalDAV invite to its own sub-address →
    iMIP REQUEST loops back to its INBOX (observed in the linux app); remove
    the attendee → iMIP CANCEL loops back the same way."""
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the live-nest CalDAV autoschedule iMIP observer drive",
            detail="drives the linux Fauna app UI as the observer; the "
            "other apps are the remaining cross-app follow-on",
            tracked="caldav-server.md",
        )

    from helpers.caldav_client import CalDAVClient, build_invite_vevent

    nonce = f"as{int(time.time())}"
    attendee = f"{LOCAL}+autosched-{nonce}@{DOMAIN}"
    summary = f"Live AutoSched {nonce}"
    uid = f"{nonce}@{DOMAIN}"

    # 1) Reach the admin shell, factory-reset, enable mail, approve bridge — all
    #    through the linux UI (the no-API constraint), reusing the zero-cheat
    #    bootstrap verbatim. Mail-enable mints the credential the CalDAV client
    #    authenticates with (the AEAD blob is shared IMAP+CalDAV).
    reach_admin_shell(app, nest_url=URL, secret_hex=SECRET, claim_code=CLAIM_CODE)
    wait_connected(app)
    app.admin.factory_reset_via_ui()
    setup._reclaim_after_ui_reset(app)
    wait_connected(app)
    mail_password = app.mail_settings.provision_default_credential_autogen()
    assert mail_password, (
        "could not read the auto-generated mail credential password from the UI "
        "(mail-add-credential-password-input empty after provisioning)"
    )
    approve_pending_bridges(app)

    # Wait for the MDA's CalDAV listener to bind after the cold-boot re-enroll
    # (the SNI router yields a TLS EOF until the bind completes).
    wait_caldav_serving(CALDAV_URL)

    # 2) The organizer connects as a stock CalDAV client over the box's real cert
    #    and lazy-provisions the Personal calendar (first PROPFIND).
    organizer = CalDAVClient(CALDAV_URL, ADDRESS, mail_password, verify=True)
    cal = organizer.personal_calendar()

    # 3) Organizer invites its own sub-address → the gateway fans an iMIP REQUEST
    #    that MX-loops back into the organizer's INBOX (test+… → base test).
    organizer.put_event(
        cal, uid,
        build_invite_vevent(uid, summary, _utc(60), _utc(120), ADDRESS, [attendee]),
    )
    tally, labels = _wait_tally(app, nonce, at_least=1, timeout=REQUEST_TIMEOUT)
    assert tally >= 1, (
        f"the iMIP REQUEST for {summary!r} (invited {attendee}) never surfaced in "
        f"the organizer's conversations within {REQUEST_TIMEOUT:.0f}s. The gateway "
        "builds the REQUEST from the just-PUT body (no prior read / MLS snapshot "
        "needed), the MTA MX-relays it, and example.com's MX is the box itself — so "
        "the most likely cause is the inbound perimeter GREYLISTING the first-seen "
        "self-loop triplet (the loop-back is SPF+DKIM-authenticated, so it is "
        "deferred, not rejected). Confirm CalDAV is reachable + the gateway is "
        f"deployed.\n  Smtp threads carrying the nonce: {labels}\n"
        f"  conversations error: {app.error_text()!r}"
    )
    print(f"[autosched-live] REQUEST observed: tally={tally} labels={labels}")

    # 4) Organizer removes the only attendee (empty roster, organizer retained) →
    #    the gateway reads the prior roster and fans an iMIP CANCEL to the removed
    #    sub-address (no new REQUEST — the new roster is empty), which loops back
    #    the same way. The prior read needs the organizer's MLS snapshot in-session
    #    (the same precondition that lets a sealing PUT succeed at all), so a
    #    successful step 3 PUT means the snapshot is present here too.
    organizer.put_event(
        cal, uid,
        build_invite_vevent(uid, summary, _utc(60), _utc(120), ADDRESS, [], sequence=1),
    )
    tally2, labels2 = _wait_tally(app, nonce, at_least=tally + 1, timeout=CANCEL_TIMEOUT)
    assert tally2 >= tally + 1, (
        f"removing the attendee did not deliver a second scheduling message (the "
        f"iMIP CANCEL) within {CANCEL_TIMEOUT:.0f}s — the mail tally for the nonce "
        f"stayed at {tally2} (was {tally} after the REQUEST). A removal fans a "
        "CANCEL to the dropped attendee (RFC 5546); its absence means either the "
        "prior-roster read was skipped (no organizer MLS snapshot for the CalDAV "
        "session — but the REQUEST PUT sealing succeeded, so the snapshot IS "
        "present) or the CANCEL was greylisted longer than the timeout.\n"
        f"  Smtp threads carrying the nonce: {labels2}\n"
        f"  conversations error: {app.error_text()!r}"
    )
    # The second delivery is the CANCEL (a removal fans no new REQUEST on an empty
    # roster). Surface the subjects so the proof is self-describing in the log.
    print(f"[autosched-live] CANCEL observed: tally={tally2} labels={labels2}")

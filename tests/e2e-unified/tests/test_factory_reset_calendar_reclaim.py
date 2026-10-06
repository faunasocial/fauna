"""tier_3 local coverage for the factory-reset → re-claim → enable-mail →
encrypted-calendar cycle — the first local e2e that re-claims after a wipe.

Why this exists
---------------
The live CalDAV roundtrip (the CalDAV live e2e vs example.com) hit a bug NO local e2e
caught: after ``factory_reset → re-claim (same identity) → enable-mail`` the linux
Events page failed at ``create_event`` ("Event did not appear", actions/events.py:68)
with ``CalendarsLoaded: 0``. The local suite never re-claimed (``test_events.py``
runs ``ensure_mail_enabled()`` on a fresh ``logged_in_app`` and stops), so the whole
"works on first claim, breaks after re-claim" class was invisible to CI.

This test exercises that cycle locally via ``common.nest.factory_reset_and_restart``
(the local stand-in for the s6 restart-wipe), running the SAME enable-mail →
create-event flow twice — once on a fresh claim, once after a re-claim — so the reset
is the only variable. It re-onboards through the SAME client process, the faithful
mirror of the live flow (``test_mail_zero_cheat_live._reclaim_after_ui_reset``,
whose client's FactoryResetComplete handler returns it to onboarding to re-claim).
No-modes retirement (ratified 2026-07-12): the re-onboard used to also pick a
storage mode (``encrypt-storage-radio``) at re-claim; that step is gone — every
nest is sealed at rest unconditionally now, so re-claiming just re-claims.

What this catches — and what it does NOT
----------------------------------------
It guards the **nest-side** recoverability transition (``docs/goal/architecture/
nest/common.md`` § Client-state recoverability lists mail-enable-after-reset as
not-yet-audited end-to-end): if a change ever breaks msek provisioning / calendar
provisioning / event seal-and-PUT after a re-claim, this goes red.

It does NOT reproduce the live ``CalendarsLoaded: 0``. A 4-variant matrix
(plaintext|encrypted × fresh-reonboard|same-window, back when the storage-mode axis
still existed) showed the cycle is SOUND at tier_3 with the faithful fresh-reonboard
flow regardless of storage mode — eliminating the live candidate "msek not derivable
after re-claim". The live failure therefore lives in a layer tier_3 cannot exercise:
the real Docker image + MDA/s6 bridge cold-boot (tier_4), or the live
transport/readiness (`rpc disconnected`). The faithful tier_4 repro is the open
follow-up (tracked internally). The one tier_3 variant that DID fail —
keeping a *stale* session across the reset (no re-onboard) — is not the live flow
(the live test re-onboards), so it is not asserted here.

Linux-only: the linux Events page is the encrypted-CalDAV-store (`bridge_caldav_*`)
surface where the live symptom appears (events.md Decision B); all apps use the same
`bridge_caldav` store; the other apps are not exercised here.
"""

import uuid
from datetime import datetime, timedelta

import pytest

from common.nest import factory_reset_and_restart
from helpers.app_surface import skip_unbuilt

pytestmark = [pytest.mark.tier_3, pytest.mark.tier1]


def _future_dt(days_ahead: int, hour: int, minute: int = 0) -> str:
    dt = datetime.now() + timedelta(days=days_ahead)
    return dt.replace(hour=hour, minute=minute, second=0, microsecond=0).strftime(
        "%Y-%m-%dT%H:%M"
    )


def _login_admin_as_user(app, nest) -> None:
    """Log the linux app in as the nest admin on the regular (feed) shell.

    The admin is the calendar actor — mirroring the live CalDAV test, where the
    linux app is the admin throughout. The admin survives
    ``factory_reset_and_restart`` (re-claimed with the same identity), so the same
    session re-points at the same actor on the re-claimed nest.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    admin = nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": "test-device-reclaim",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })


def _enable_mail_and_open_events(app) -> None:
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    app.events.navigate()


def _create_event_and_assert_visible(app, summary: str, *, phase: str) -> None:
    """Select Personal + create an event + assert it renders — the exact user
    action that failed live (actions/events.py:68 "Event did not appear")."""
    count = app.count("calendar-item")
    assert count >= 1, (
        f"{phase}: the Personal calendar should be present; got {count} "
        f"calendars (CalendarsLoaded: {count}). error={app.error_text()!r}"
    )
    app.events.select_calendar("Personal")
    app.events.create_event(summary, start=_future_dt(7, 10), end=_future_dt(7, 11))
    assert any(summary in s for s in app.events.event_summaries()), (
        f"{phase}: event {summary!r} created in Personal should be visible. "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("factory-reset")
def test_calendar_works_after_factory_reset_reclaim(app, reclaimable_nest):
    """The Personal calendar must provision AND accept an event after
    factory_reset → re-claim (same identity) → enable-mail, exactly as on a first
    claim. Fresh re-onboard = the faithful live flow.
    """
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the encrypted-CalDAV calendar-after-reclaim observation",
            detail="the linux Events page is the observation surface for "
            "this cycle; the other apps are the remaining cross-app "
            "follow-on",
            tracked="caldav-server.md",
        )

    nest = reclaimable_nest

    # ── Phase 1: fresh-claim baseline (proves the operation works) ──────────
    _login_admin_as_user(app, nest)
    _enable_mail_and_open_events(app)
    _create_event_and_assert_visible(
        app, f"baseline-{uuid.uuid4().hex[:8]}", phase="phase 1 (fresh claim)"
    )

    # ── Phase 2: the re-claim cycle ────────────────────────────────────────
    # factory_reset → restart-wipe → re-claim admin (same identity), then
    # re-onboard the SAME client process — driver.reset() ("clear stores, return
    # to onboarding", no relaunch) + re-login — exactly the live flow, where the
    # client's FactoryResetComplete handler returns it to onboarding and it
    # re-claims. The only difference from phase 1 is the reset cycle, so a
    # phase-2-only failure is the recoverability regression.
    factory_reset_and_restart(nest)
    app.driver.reset()
    _login_admin_as_user(app, nest)
    _enable_mail_and_open_events(app)
    _create_event_and_assert_visible(
        app, f"reclaim-{uuid.uuid4().hex[:8]}", phase="phase 2 (after re-claim)"
    )

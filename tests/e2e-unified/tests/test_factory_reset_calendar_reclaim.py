"""tier_3 local coverage for the factory-reset → re-claim → enable-mail →
encrypted-calendar cycle — the first local e2e that re-claims after a wipe.

Why this exists
---------------
The live CalDAV roundtrip (the CalDAV live e2e vs the live nest) hit a bug NO local e2e
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

Apps: linux, macos, ios, tui and web (the surface `calendar-after-reclaim`). The linux
Events page is the encrypted-CalDAV-store (`bridge_caldav_*`) surface where the live
symptom appears (events.md Decision B); every built app's Events page drives the same
store through the shared events actions. windows and android are not exercised here yet.
"""

import uuid
from datetime import datetime, timedelta

import pytest

from common.nest import factory_reset_and_restart
from helpers import caldav_roundtrip as rt
from helpers.app_surface import require_surface
from helpers.e2e_session import e2e_login_device_id, login_as

pytestmark = [pytest.mark.tier_3, pytest.mark.tier1]


def _future_dt(days_ahead: int, hour: int, minute: int = 0) -> str:
    dt = datetime.now() + timedelta(days=days_ahead)
    return dt.replace(hour=hour, minute=minute, second=0, microsecond=0).strftime(
        "%Y-%m-%dT%H:%M"
    )


@pytest.fixture
def reclaimable_spa_url(spa_proxy_for, reclaimable_nest):
    """SPA proxy → this test's `reclaimable_nest`, so the web seat reaches it
    without CORS. Resolved lazily by `login_as` on web only, so a native run never
    builds the SPA."""
    return spa_proxy_for(reclaimable_nest["url"])


def _login_admin_as_user(app, nest, request) -> None:
    """Log the app in as the nest admin on the regular (feed) shell, through the
    shared `login_as` seam — its live-session barrier (convention 14) waits until
    the app's account runtime reports the admin actor, and its web leg goes
    through the SPA proxy.

    The admin is the calendar actor — mirroring the live CalDAV test, where the
    app is the admin throughout. The admin survives ``factory_reset_and_restart``
    (re-claimed with the same identity), so the same session re-points at the same
    actor on the re-claimed nest.
    """
    login_as(
        app,
        nest,
        nest["admin"],
        handle="admin",
        device_id=e2e_login_device_id(app),
        request=request,
        spa_url_fixture="reclaimable_spa_url",
    )


def _enable_mail_and_open_events(app) -> None:
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    app.events.navigate()


def _create_event_and_assert_visible(app, summary: str, *, phase: str) -> None:
    """Select Personal + create an event + assert it renders — the exact user
    action that failed live (actions/events.py:68 "Event did not appear")."""
    # The page loads its calendars on ENTRY, and an entry that raced the
    # enable-mail provisioning reads none; `wait_calendars_ready` re-enters the
    # page until the nest-provisioned calendar lists, failing self-diagnosing
    # past its deadline — so a calendar that never provisions after the
    # re-claim (the live `CalendarsLoaded: 0` class) still goes red, while a
    # bare `count("calendar-item")` read on first entry raced the load on tui
    # (convention 14).
    rt.wait_calendars_ready(app)
    try:
        app.events.select_calendar("Personal")
    except TimeoutError as exc:
        raise AssertionError(
            f"{phase}: the Personal calendar should be present. "
            f"error={app.error_text()!r}; {exc}"
        ) from exc
    app.events.create_event(summary, start=_future_dt(7, 10), end=_future_dt(7, 11))
    assert any(summary in s for s in app.events.event_summaries()), (
        f"{phase}: event {summary!r} created in Personal should be visible. "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("factory-reset")
def test_calendar_works_after_factory_reset_reclaim(app, reclaimable_nest, request):
    """The Personal calendar must provision AND accept an event after
    factory_reset → re-claim (same identity) → enable-mail, exactly as on a first
    claim. Fresh re-onboard = the faithful live flow.
    """
    require_surface(app.driver, "calendar-after-reclaim")

    nest = reclaimable_nest

    # ── Phase 1: fresh-claim baseline (proves the operation works) ──────────
    _login_admin_as_user(app, nest, request)
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
    _login_admin_as_user(app, nest, request)
    _enable_mail_and_open_events(app)
    _create_event_and_assert_visible(
        app, f"reclaim-{uuid.uuid4().hex[:8]}", phase="phase 2 (after re-claim)"
    )

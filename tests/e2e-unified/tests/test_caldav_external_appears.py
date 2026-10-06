"""tier_3: an externally-created CalDAV calendar + event appears on the client's
Events page *quickly, while the user stays on the page* — no re-login, no
navigate-away-and-back. Unified across every app with a while-on-page
refresh mechanism (priority #1): the poll on linux (2026-06-13), windows
(2026-06-24), macOS + iOS (2026-07-15, `EventsVM.pollWhileVisible`); the
`fauna.calendar.changed` push on web (2026-07-17 — web has no poll, so its leg
doubles as the push's end-to-end client proof; transport.md § Push events) and
on tui (2026-08-21 — like web, push-only and no poll: the fold
`PushEvent::CalendarChanged → StaleSurfaces { events: true }` re-runs
`events::nav_enter_op` from `apply_resync`).

The manual-test loop (2026-06-13, linux) found a calendar/event added via macOS
Calendar.app (CalDAV → MDA → encrypted `bridge_caldav_*` store) did not appear on
the Events page until the user navigated away and back, or re-logged in.
A nav-refresh stopgap (linux `app.rs` visible-child-notify → `fetch_calendars()`)
closed the navigate-away-and-back path, but the goal doc's actual mechanism is a
**poll + a calendar-change push** (events.md § Where logic lives:
"CalDAV sync. Shared Rust: `sync_calendar_since` (RFC 6578) poll + a
calendar-change push"). This test pins the *while-on-the-page* guarantee that the
nav-refresh stopgap does NOT provide: a change made by an external CalDAV client
surfaces on the Events page within a bounded interval **without** the test
ever re-navigating. windows mirrors linux with a ~10s `DispatcherTimer` on
`EventsPage` re-running `EventsViewModel.RefreshIfChangedAsync`.

Why this can't be the existing `test_caldav_mkcalendar_create.py`: that test
verifies the external MUA's *own* view of its write (PROPFIND/REPORT round-trip).
THIS test verifies the *Fauna app's* view — the encrypted-store decode
path (events.md § Where logic lives: MDA-written and client-written events are
"mutually readable") *and* the refresh mechanism that makes an external write
visible without a manual nav.

RED before the poll lands: the Events page only re-fetches calendars on
login / navigate-to-events / a local create-delete-import; sitting on the page
while an external client writes shows nothing (`sync_calendar_since` exists in
shared Rust but the client never polls it, and there is no calendar-change push).
GREEN after: a periodic poll (re-fetch while the Events page is visible) surfaces
the external calendar + event within the bounded interval.

caldav-server.md § Calendar collection model / § Sync model (RFC 6578) +
events.md § Where logic lives / § Implementation status today.
"""

import time
import uuid

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.caldav_client import CalDAVClient, build_vevent

# Reuse the dedicated-nest admin-inject + enable-mail preamble (priority #2 — the
# canonical "log the linux app in as the dedicated nest's admin + alias it a
# mail address" path, shared with test_caldav_mkcalendar_create.py /
# test_mail_enable_then_mua_round_trip.py; one preamble, not a copy per test).
from . import test_mail_enable_then_mua_round_trip as rt

# tui IS marked as of 2026-08-31. Both of the reasons it
# was held out are gone: the `dedicated_mail_nest` general-path fixture fix
# landed, and the while-on-page refresh this case
# needs — the one the guard below cited as tui's "separate, still-open reason" —
# has been wired since 2026-08-21: `PushEvent::CalendarChanged`
# folds to `StaleSurfaces { events: true }` and tui's `apply_resync` re-runs
# `events::nav_enter_op` on it, the same shared-classifier seam every other app
# rides. The comment claiming otherwise outlived its own subject by nine days —
# it was written against the module header of `apps/fauna-tui/src/events/mod.rs`,
# which says "NOT built yet" about the quick-appearance POLL, never about the
# push. tui needs no poll for the same reason web needs none: the push alone
# carries the while-on-page guarantee (`events.md` § Implementation status
# today, the quick-appearance matrix).
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.web,
    pytest.mark.tui,
]

# The PLAIN mail credential the client mints and the external CalDAV client
# authenticates with (shared IMAP + CalDAV, AEAD-unwrap-as-auth).
_PASSWORD = "CalDavExternalAppearsPlainPw0011Hh"

# How long the Events page may take to surface the external write *while
# staying on the page* — must comfortably exceed the client's poll interval so
# the assertion sees at least one full poll cycle. Both apps poll ~10s
# (linux CALENDAR_POLL_INTERVAL_SECS, windows PollIntervalSecs); 45s gives
# several cycles plus network/seal settle.
_APPEAR_TIMEOUT = 45.0


def _utc(offset_min: int) -> str:
    """ISO basic-format UTC timestamp `offset_min` minutes from now (the format
    build_vevent uses)."""
    return time.strftime("%Y%m%dT%H%M00Z", time.gmtime(time.time() + offset_min * 60))


def _calendar_names(app) -> list[str]:
    """The calendar names currently rendered in the Events sidebar — read
    straight from the live widget tree (`calendar-item`), WITHOUT navigating, so
    a background poll's effect is observable but a nav-refresh is not triggered.
    Driver-agnostic: reads via `app.driver.count`/`get_text` over the shared
    `calendar-item` id, so it works on every app that renders that id.
    """
    count = app.driver.count("calendar-item")
    return [app.driver.get_text("calendar-item", index=i) for i in range(count)]


def _event_summaries(app) -> list[str]:
    count = app.driver.count("event-card-summary")
    return [app.driver.get_text("event-card-summary", index=i) for i in range(count)]


@pytest.mark.feature("calendar-and-events")
def test_external_caldav_calendar_and_event_appear_while_on_page(
    app, dedicated_mail_nest, request
):
    """An external CalDAV client (the shape macOS Calendar.app uses) creates a
    calendar + event; the Events page — already open, never re-navigated —
    shows both within a bounded interval, no re-login.

    Proves two things at once: (1) the externally-sealed calendar metadata + event
    body decode on the client (mutual readability), and (2) the refresh
    mechanism surfaces an external write while the user sits on the page (the poll,
    not the nav-refresh stopgap).

    Runs on every app that has a while-on-page refresh mechanism (priority
    #1 — one unified test, not a per-app copy): the poll on linux
    (2026-06-13), windows (2026-06-24), macOS + iOS (2026-07-15), and the
    `fauna.calendar.changed` push on web (2026-07-17 — web has NO poll, so on
    web this test is the push's end-to-end proof: nothing else can make a
    mounted page move; transport.md § Push events). android's leg stays
    host-emulator-gated.

    Web now renders the no-selection union of every owned calendar's events
    (events.md dated history 2026-07-15 — converged onto windows/apple, the
    richest existing pattern), so on web this test needs no interaction at all:
    the externally-created CALENDAR appears in the sidebar and its EVENT appears
    in the agenda with zero clicks (both push-driven — web has no poll, so only
    the `fauna.calendar.changed` push can move a mounted page). Phase 2 PUTs a
    SECOND external event and asserts it too appears with zero further
    interaction (the push-armed `refreshEvents` re-querying the union).
    """
    if not (app.driver.is_linux() or app.driver.is_windows() or app.driver.is_macos()
            or app.driver.is_ios() or app.driver.is_web() or app.driver.is_tui()):
        skip_unbuilt(
            app.driver,
            surface="the while-on-page calendar refresh push (fauna.calendar."
            "changed → refreshEvents)",
            detail="landed on linux/windows/macos/ios/web/tui; android's push "
            "wiring landed but e2e is host-emulator-gated, so android is the "
            "one remaining cross-app follow-on",
            tracked="caldav-server.md",
        )

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    # 1) Log the client in as the dedicated nest admin + enable mail through
    #    its own UI — mints the recipient pubkey + wrapped-MSEK the MDA seals the
    #    external calendar/event to and the Events page unseals with.
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
    mua = CalDAVClient(base, admin_addr, _PASSWORD, verify=False)
    mua.wait_until_serving()

    # 2) Open the Events page and snapshot the baseline. This is the LAST
    #    navigation the test performs — every later assertion re-reads the live
    #    widget tree, so anything that appears must arrive via the page's own
    #    refresh cadence (the poll), not a nav-refresh.
    app.events.navigate()
    baseline_cals = _calendar_names(app)

    # 3) An external CalDAV client adds a calendar (MKCALENDAR at an opaque UUID
    #    slug, the macOS Calendar.app shape) and PUTs an event into it — the exact
    #    CalDAV → MDA → encrypted-store write the manual test exercised.
    slug = str(uuid.uuid4()).upper()
    cal_name = f"ExternalCal-{uuid.uuid4().hex[:8]}"
    cal_href = mua.mkcalendar(slug, displayname=cal_name, color="#34C759")
    uid = f"ext-evt-{uuid.uuid4().hex[:8]}@fauna"
    summary = f"ExternalEvent-{uuid.uuid4().hex[:8]}"
    mua.put_event(cal_href, uid, build_vevent(uid, summary, _utc(60), _utc(120)))

    # 4) WITHOUT re-navigating or re-logging in, the new calendar must surface
    #    on the still-open Events page within the bounded interval — via the
    #    poll on the native apps, via the `fauna.calendar.changed` push on
    #    web (which has no poll: only the push can move a mounted web page).
    def _wait_for(predicate):
        deadline = time.monotonic() + _APPEAR_TIMEOUT
        last = None
        while time.monotonic() < deadline:
            last = predicate()
            if last[0]:
                return last
            time.sleep(2.0)
        return last

    def _cal_probe():
        cals = _calendar_names(app)
        return (any(cal_name in c for c in cals), cals)

    seen, last_cals = _wait_for(_cal_probe)
    assert seen, (
        f"externally-created calendar {cal_name!r} never appeared in the "
        f"Events sidebar within {_APPEAR_TIMEOUT:.0f}s while on the page (no "
        f"re-navigation). baseline={baseline_cals!r} last={last_cals!r}. Either the "
        "page's refresh mechanism (poll on native, fauna.calendar.changed push on "
        "web) never fired, or the externally-sealed calendar metadata failed to "
        "decode on the client."
    )

    def _evt_probe():
        evts = _event_summaries(app)
        return (any(summary in s for s in evts), evts)

    seen, last_evts = _wait_for(_evt_probe)
    assert seen, (
        f"externally-created event {summary!r} never appeared in the Events "
        f"agenda within {_APPEAR_TIMEOUT:.0f}s while on the page (no re-navigation). "
        f"last={last_evts!r}. The calendar appeared but its event did not — either "
        "the event body failed to decode or per-calendar event sync never refreshed."
    )

    if app.driver.is_web():
        # 5) Web phase 2 — the pure-push assertion for the EVENT half: with NO
        #    calendar selected (the union view) and the page untouched from here
        #    on, a second external write must appear with ZERO interaction. Web
        #    has no poll, so only the push-armed `refreshEvents` re-querying the
        #    union can deliver this.
        uid2 = f"ext-evt-{uuid.uuid4().hex[:8]}@fauna"
        summary2 = f"ExternalEvent2-{uuid.uuid4().hex[:8]}"
        mua.put_event(cal_href, uid2, build_vevent(uid2, summary2, _utc(180), _utc(240)))
        def _evt2_probe():
            evts = _event_summaries(app)
            return (any(summary2 in s for s in evts), evts)

        seen, last_evts = _wait_for(_evt2_probe)
        assert seen, (
            f"second externally-created event {summary2!r} never appeared within "
            f"{_APPEAR_TIMEOUT:.0f}s with zero interaction. last={last_evts!r}. "
            "The fauna.calendar.changed push arm on the web Events page is dead "
            "(web has no poll to mask it — that is the point of this assertion)."
        )

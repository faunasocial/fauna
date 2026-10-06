"""Shared 3-client CalDAV round-trip engine.

Extracted from `tests/test_caldav_live_nest.py` so the live-remote test and the
local tier_3 onboarding-variant discovery matrix
(`tests/test_caldav_onboarding_variants.py`) share ONE implementation, not two
(priorities #2/#4 — reuse, don't copy). The "three clients" are two simulated
CalDAV MUAs (`helpers/caldav_client.CalDAVClient`) plus the native Fauna app's
Events page (any UI driver — the `app` fixture).

Public API:
  - `caldav_has(client, cal_href, summary, *, timeout)` / `caldav_lacks(...)`
    — poll a `CalDAVClient`'s `summaries(cal_href)` for presence / absence.
  - `native_has(app, summary, *, timeout)` / `native_lacks(...)`
    — poll the native app's Events page for presence / absence. Driver-agnostic
    (uses only `app.events.navigate()` + `app.events.event_summaries()`).
  - `wait_caldav_serving(base_url, *, verify, timeout)`
    — PROPFIND `/caldav/` until the MDA's listener answers HTTPS; `pytest.fail`
    on timeout. `verify=` is parameterized so a self-signed local MDA can connect.
  - `wait_calendars_ready(app, *, timeout)`
    — re-navigate Events until at least one `calendar-item` loads.
  - `run_create_visibility_matrix(app, mua_a, cal_a, mua_b, cal_b, *, nonce, ...)`
    — the symmetric "each of 3 creates → visible in the other 2" check.

Authority for the wire contract: docs/goal/behavior/caldav-server.md.
"""

from __future__ import annotations

import time

import pytest

from helpers.caldav_client import build_vevent


def _utc(offset_min: int) -> str:
    """An ISO basic-format UTC timestamp `offset_min` minutes from now, rounded
    to whole minutes — the iCalendar `DTSTART`/`DTEND` shape a CalDAV MUA puts
    on the wire. Deterministic enough for the assertions; uniqueness comes from
    the per-run nonce in the SUMMARY, not the time."""
    t = time.gmtime(time.time() + offset_min * 60)
    return time.strftime("%Y%m%dT%H%M00Z", t)


def _form_local(offset_min: int) -> str:
    """A local `YYYY-MM-DDTHH:MM` timestamp `offset_min` minutes from now — what a
    user types into the app's `event-dtstart`/`event-dtend` fields, and the one
    shape every app's composer takes (`docs/goal/ui/events.md` § Where logic
    lives, the A2 rule; `test_events.py` types it on all of them). Not `_utc`: a
    zoned basic-format stamp only *passes through* the A2 rule on a text field,
    and web's native `datetime-local` input refuses it outright."""
    t = time.localtime(time.time() + offset_min * 60)
    return time.strftime("%Y-%m-%dT%H:%M", t)


# ── cross-client visibility helpers ───────────────────────────────────────────


def caldav_has(client, cal_href, summary, *, timeout=60.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if summary in client.summaries(cal_href):
            return True
        time.sleep(2.0)
    return False


def caldav_lacks(client, cal_href, summary, *, timeout=60.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if summary not in client.summaries(cal_href):
            return True
        time.sleep(2.0)
    return False


def native_has(app, summary, *, timeout=60.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        app.events.navigate()
        if any(summary in s for s in app.events.event_summaries()):
            return True
        time.sleep(2.0)
    return False


def native_lacks(app, summary, *, timeout=60.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        app.events.navigate()
        if not any(summary in s for s in app.events.event_summaries()):
            return True
        time.sleep(2.0)
    return False


def wait_calendars_ready(app, *, timeout: float = 120.0) -> None:
    """Wait until the native app's Events page has loaded at least one calendar.

    After the factory-reset + enable-mail cold-boot, the native app's first
    Events navigate fires `fetch_calendars` while the WS-RPC is still settling
    its post-reset reconnect (a fix preserves the deployment key so pinned
    clients DO reconnect — but there's a window). In that window
    `caldav_context`'s fresh mail-state read can fail transiently, or the
    just-minted `mail.msek` may not yet be readable, so the page degrades to
    `CalendarsLoaded: 0` and `select_calendar("Personal")` silently no-ops (it
    iterates `calendar-item`, of which there are none). Re-navigating re-fires
    `fetch_calendars` (`main.rs` `"events" => fetch_calendars()`), so we poll the
    navigate until the Personal calendar materialises — the CalDAV analogue of
    `wait_caldav_serving`. If it never loads, the app.err `caldav_context:`
    diagnostic distinguishes a transient load failure from a genuine absent msek.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        app.events.navigate()
        if app.driver.count("calendar-item") > 0:
            return
        time.sleep(3.0)
    pytest.fail(
        f"the native Events page never loaded a calendar within {timeout:.0f}s "
        "after enable-mail + bridge-approve (CalendarsLoaded stayed 0). The "
        "CalDAV MUAs found the Personal calendar server-side, so the encrypted "
        "store has it — the native app's calendar-load path is the gap. Read "
        "the native app.err `caldav_context:` line: 'config load failed' ⇒ a "
        "transient post-reset reconnect (needs a retry in caldav_context); "
        "'mail.msek is None' ⇒ msek was not persisted to the `fauna.state.mail` plane despite "
        "mail being enabled (a provisioning gap)."
    )


def wait_caldav_serving(
    base_url: str, *, verify: bool | str = True, timeout: float = 180.0, mda=None
) -> None:
    """Wait until the MDA's CalDAV listener is bound and answering HTTPS.

    After the UI factory-reset brings the mail bridge down and `_enable_mail`
    re-enables it, the MDA must cold-boot, re-`request_enrollment`, get approved,
    re-attest, and only THEN bind its CalDAV listener — the SNI router yields a
    TLS EOF until that bind completes (so a plain TCP connect is not enough; we
    need a completed handshake + HTTP response). Any HTTP status (e.g. the
    `401 Basic realm="fauna-caldav"` challenge) means the listener is serving.

    `verify=` is parameterized: the live test points at a real ACME cert
    (`verify=True`); a local self-signed MDA needs `verify=False`.

    `mda`, if given (an e2e-spawned `conftest._SpawnedBridge`), plays the
    s6-supervisor role for the MDA's exit-for-rebind mechanism (`mda.go`'s
    gating-tuple watcher): the MDA binds its listener set once at `Run()`
    startup and exits 0 the moment a later `config_changed` reports a gating
    tuple different from what it bound — production runs it under s6, which
    restarts it bound to the new shape; this harness has none unless told to
    here. Needed because the MDA can cold-boot from an UNCLAIMED nest (all
    gates closed) before the client's onboarding flow finishes deriving/
    enabling mail+caldav, and the two enablement RPCs land as independent
    fire-and-forget calls — so the MDA's first successful config fetch can
    land on a partial or idle gating tuple, requiring exactly the same
    exit-and-respawn dance `test_caldav_admin_port_rebind.py` already plays
    for a port change. Omit for a real deployment (a live box, an installed
    package) where an actual supervisor already does this job.
    """
    import requests

    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        if mda is not None:
            rc = mda.proc.poll()
            if rc is not None:
                if rc != 0:
                    raise RuntimeError(
                        f"MDA bridge exited {rc}, not 0 — a crash, not the "
                        f"documented exit-for-rebind; see {mda.log_file_path}"
                    )
                mda.respawn(wait_for_serving=False)
        try:
            requests.request("PROPFIND", f"{base_url}/caldav/", timeout=6.0, verify=verify)
            return  # handshake completed + HTTP response → listener bound
        except Exception as e:  # ConnectionError / SSLError / Timeout while booting
            last = type(e).__name__
            time.sleep(3.0)
    pytest.fail(
        f"the MDA CalDAV endpoint at {base_url} never served within {timeout:.0f}s "
        f"after enable-mail (last error: {last}). If IMAP came up but CalDAV did "
        f"not, the MDA's CalDAV listener is not binding on enable (caldav_enabled "
        f"fallback / cold-boot bind gap)."
    )


# ── the symmetric round-trip composer ─────────────────────────────────────────


def run_create_visibility_matrix(
    app,
    mua_a,
    cal_a,
    mua_b,
    cal_b,
    *,
    nonce: str,
    prop_timeout: float = 60.0,
) -> None:
    """The symmetric "each of 3 clients creates an event → it becomes visible on
    the other 2" round-trip. Three clients share one Personal calendar:
    `app.events` (the native app) and two CalDAV MUAs (`mua_a` on `cal_a`,
    `mua_b` on `cal_b`). Each `assert` carries a diagnostic message.
    """
    # 1) The native app creates → both CalDAV MUAs see it.
    s_native = f"{nonce}-from-native"
    app.events.create_event(s_native, _form_local(60), _form_local(120))
    assert caldav_has(mua_a, cal_a, s_native, timeout=prop_timeout), (
        f"event {s_native!r} created on the native Events page never appeared to "
        "CalDAV client A — the native page and the CalDAV store are not unified."
    )
    assert caldav_has(mua_b, cal_b, s_native, timeout=prop_timeout), (
        f"event {s_native!r} created on the native Events page never appeared to "
        "CalDAV client B."
    )

    # 2) CalDAV A creates → CalDAV B and the native app see it.
    s_a = f"{nonce}-from-a"
    uid_a = f"{nonce}-uid-a"
    mua_a.put_event(cal_a, uid_a, build_vevent(uid_a, s_a, _utc(180), _utc(240)))
    assert caldav_has(mua_b, cal_b, s_a, timeout=prop_timeout), (
        f"event {s_a!r} (created by CalDAV client A) never appeared to CalDAV client B."
    )
    assert native_has(app, s_a, timeout=prop_timeout), (
        f"event {s_a!r} created by CalDAV client A never appeared on the native "
        "Events page (disjoint stores)."
    )

    # 3) CalDAV B creates → CalDAV A and the native app see it.
    s_b = f"{nonce}-from-b"
    uid_b = f"{nonce}-uid-b"
    mua_b.put_event(cal_b, uid_b, build_vevent(uid_b, s_b, _utc(300), _utc(360)))
    assert caldav_has(mua_a, cal_a, s_b, timeout=prop_timeout), (
        f"event {s_b!r} (created by CalDAV client B) never appeared to CalDAV client A."
    )
    assert native_has(app, s_b, timeout=prop_timeout), (
        f"event {s_b!r} created by CalDAV client B never appeared on the native "
        "Events page (disjoint stores)."
    )

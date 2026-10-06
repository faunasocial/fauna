"""tier_4 (live-remote, OPT-IN): three-client CalDAV calendar round-trip against
a LIVE nest (e.g. example.com), the calendar sibling of
`test_mail_enable_live_nest.py`.

Goal (the user's target): multiple calendar clients sync one calendar through
example.com under the handle `test@example.com` — an appointment created, edited, or
deleted from ANY client becomes visible on the others quickly. We model three
clients:

  - **caldav_a**, **caldav_b** — two simulated CalDAV MUAs (Python, raw wire via
    `helpers/caldav_client.py`), exactly what macOS Calendar.app / Thunderbird /
    Evolution speak: HTTPS + HTTP Basic Auth + PUT/GET/DELETE/REPORT.
  - **linux** — the real Fauna desktop app's Events page (UI driver).

All three share the actor's single "Personal" calendar.

CONSTRAINT (user, 2026-06-01): the test makes **no nest admin/config API calls**
to example.com. Every change to the box goes through the linux app UI (onboard,
claim, enable mail, approve bridge, create/edit/delete events) OR through the
CalDAV protocol itself (which is the real product surface, not a back channel).
The only out-of-band touch is read-only SSH diagnostics (e.g. reading a fresh
claim code from `docker logs`), which never mutates the box.

**Skipped unless these env vars are set** (it drives a live external box):
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
                            password is the PLAIN mail password (= CalDAV
                            Basic auth, shared with IMAP).
Optional:
  FAUNA_LIVE_CALDAV_URL     CalDAV root base URL if it differs from the nest URL
                            (e.g. https://mail.example.com:8443). Defaults to the
                            nest URL — see § Known gap below.
  FAUNA_CONV_POLL_SECS=5 (export) so the linux app polls quickly.

KNOWN GAPS this test pins (RED until closed — tracked internally):
  1. **CalDAV is not exposed externally on example.com.** docker-compose.yml
     publishes 80/443/25/465/587/993 but NOT the MDA's CalDAV port — host 443
     maps to nest (8443), and the MDA's CalDAV listener (container :443) is
     unpublished. The CalDAV clients can't connect until exposure is decided +
     deployed (see TODO § Decision A).
  2. **Store unification — RESOLVED (Decision B; legacy path retired § 4c).** The
     linux Events page (and all 7 apps) now read/write the SAME encrypted
     `bridge_caldav_*` store the CalDAV MUAs reach through the MDA, via the shared
     `fauna.bridges.*` calendar RPCs + `fauna-client-caldav` — so linux-created
     events and CalDAV-client events are mutually visible. The old disjoint
     plaintext `content`-table path (REST `/api/calendars` + `fauna.{events,
     calendars}.*`) was deleted in the § 4c legacy-calendar retirement.

This is the failing-test-first (TDD red) spec for that work.
"""

from __future__ import annotations

import os
import time

import pytest

# The 3-client round-trip engine lives in a shared helper so this live-remote
# test and the local tier_3 onboarding-variant discovery matrix
# (`test_caldav_onboarding_variants.py`) share ONE implementation (priorities
# #2/#4 — reuse, don't copy). `_utc` is re-exported here because two of this
# module's cases call it directly.
from helpers.app_surface import skip_unbuilt
from helpers.caldav_roundtrip import (
    _utc,
    caldav_has,
    caldav_lacks,
    native_has,
    native_lacks,
    wait_caldav_serving,
    wait_calendars_ready,
)
from helpers import live_box_door
from helpers.live_admin import approve_pending_bridges, reach_admin_shell, wait_connected

# Reuse the ZERO-CHEAT mail test's UI-only setup path: the shared
# `helpers.live_admin` steps (reach the admin shell, wait connected, approve
# the bridge) plus that sibling's `_reclaim_after_ui_reset` — the canonical
# "reach the admin shell + factory-reset + enable mail + approve bridge with NO
# nest API calls" path, every step the linux app UI (priority #2/#4: reuse,
# don't copy). We deliberately reuse it, not `test_mail_enable_live_nest`,
# because the latter bootstraps via a `fauna.admin.factory_reset` WS-RPC call,
# which the no-API constraint forbids.
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
        reason="live CalDAV test: provide the box's admin seed "
        + f"({live_box_door.SEED_SOURCES}) and its mailbox ({live_box_door.MAILBOX_SOURCES})"
        + " to run (drives a live external nest via the linux client; opt-in)",
    ),
]

CLAIM_CODE = os.environ.get("FAUNA_LIVE_CLAIM_CODE", "")
CALDAV_URL = os.environ.get("FAUNA_LIVE_CALDAV_URL", URL).rstrip("/")
DOMAIN = ADDRESS.split("@", 1)[-1] if "@" in ADDRESS else ADDRESS

# How long to wait for a change to propagate to another client (poll-based sync:
# CalDAV getctag / sync-collection + the linux app's inbound poll loop).
PROP_TIMEOUT = float(os.environ.get("FAUNA_CALDAV_PROP_TIMEOUT", "60"))


# ── the test ──────────────────────────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_three_client_caldav_roundtrip(app):
    """One Personal calendar at test@example.com, three clients (2 CalDAV MUAs +
    the linux Events page). Create/edit/delete from each → visible on the others.
    """
    from helpers.caldav_client import CalDAVClient, build_vevent

    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the live-nest 3-client CalDAV round-trip drive",
            detail="drives the linux Fauna app UI as the 3rd client; the "
            "other apps are the remaining cross-app follow-on",
            tracked="caldav-server.md",
        )

    nonce = f"cal{int(time.time())}"

    # 1) Get the linux app logged in as test@example.com with mail enabled +
    #    bridge serving — ALL through the UI, NO nest API calls (the no-API
    #    constraint). This mirrors test_mail_zero_cheat_live's setup exactly:
    #    reach the admin shell (sign in or claim) → factory-reset via the
    #    Danger-zone button → re-claim (pre-filled code) → enable mail → approve
    #    bridge. Mail-enable mints the credential whose password the CalDAV
    #    clients authenticate with (the AEAD blob is shared across IMAP+CalDAV).
    reach_admin_shell(app, nest_url=URL, secret_hex=SECRET, claim_code=CLAIM_CODE)
    wait_connected(app)
    app.admin.factory_reset_via_ui()
    setup._reclaim_after_ui_reset(app)
    wait_connected(app)
    # Provision the PLAIN "default" credential with auto-generate ON and read the
    # generated password back from the UI — the realistic mail-credentials.md
    # flow (auto-generate is the default). We can't reuse FAUNA_LIVE_MAIL_PASSWORD:
    # onboarding's set_mail_enabled (onboarding.md §3b) already enabled the mail
    # subsystem, so the chosen-password path doesn't apply; the password the MUA
    # authenticates with is whatever the client minted. This is the credential the
    # bridge's AUTH PLAIN path looks up (credential_id "default"), shared IMAP+CalDAV.
    mail_password = app.mail_settings.provision_default_credential_autogen()
    assert mail_password, (
        "could not read the auto-generated mail credential password from the UI "
        "(mail-add-credential-password-input was empty after provisioning)"
    )
    approve_pending_bridges(app)

    # Wait for the MDA's CalDAV listener to actually bind after the cold-boot —
    # the bridge re-enrolls + re-attests post-reset, so the endpoint races the
    # connect (mirrors the mail test's _wait_imap_serving).
    wait_caldav_serving(CALDAV_URL)

    # 2) The two CalDAV MUAs connect over the box's real cert (verify on), using
    #    the auto-generated password read from the UI (shared IMAP+CalDAV credential).
    caldav_a = CalDAVClient(CALDAV_URL, ADDRESS, mail_password, verify=True)
    caldav_b = CalDAVClient(CALDAV_URL, ADDRESS, mail_password, verify=True)
    cal_a = caldav_a.personal_calendar()
    cal_b = caldav_b.personal_calendar()

    # 3) The linux app opens its Events page on the same Personal calendar.
    #    Wait for the calendar list to actually load first — the post-reset
    #    cold-boot can race the WS-RPC reconnect, leaving the first navigate's
    #    fetch_calendars empty (see `wait_calendars_ready`).
    wait_calendars_ready(app)
    app.events.select_calendar("Personal")

    # ── Case 1: linux creates → both CalDAV clients see it ────────────────────
    s1 = f"{nonce}-from-linux"
    app.events.create_event(s1, _utc(60), _utc(120))
    assert caldav_has(caldav_a, cal_a, s1, timeout=PROP_TIMEOUT), (
        f"event {s1!r} created on the linux Events page never appeared to CalDAV "
        "client A — the linux page and the CalDAV store are not unified (GAP 2)."
    )
    assert caldav_has(caldav_b, cal_b, s1, timeout=PROP_TIMEOUT), (
        f"{s1!r} not visible to CalDAV client B"
    )

    # ── Case 2: CalDAV A edits → CalDAV B and linux see the edit ──────────────
    uid2 = f"{nonce}-uid-a"
    s2 = f"{nonce}-from-a"
    caldav_a.put_event(cal_a, uid2, build_vevent(uid2, s2, _utc(180), _utc(240)))
    assert caldav_has(caldav_b, cal_b, s2, timeout=PROP_TIMEOUT), (
        f"{s2!r} (created by A) not visible to B"
    )
    assert native_has(app, s2, timeout=PROP_TIMEOUT), (
        f"event {s2!r} created by CalDAV client A never appeared on the linux "
        "Events page (GAP 2 — disjoint stores)."
    )
    s2_edited = f"{nonce}-from-a-EDITED"
    caldav_a.put_event(
        cal_a, uid2, build_vevent(uid2, s2_edited, _utc(180), _utc(240), sequence=1)
    )
    assert caldav_has(caldav_b, cal_b, s2_edited, timeout=PROP_TIMEOUT), (
        f"edit {s2_edited!r} not visible to B"
    )
    assert native_has(app, s2_edited, timeout=PROP_TIMEOUT), (
        f"edit {s2_edited!r} not visible on linux"
    )

    # ── Case 3: CalDAV B deletes → CalDAV A and linux no longer see it ────────
    caldav_b.delete_event(cal_b, uid2)
    assert caldav_lacks(caldav_a, cal_a, s2_edited, timeout=PROP_TIMEOUT), (
        f"{s2_edited!r} still visible to A after B deleted it"
    )
    assert native_lacks(app, s2_edited, timeout=PROP_TIMEOUT), (
        f"{s2_edited!r} still on linux after B deleted it"
    )

    # ── Case 4: linux deletes the event it created → both CalDAV clients lose it ─
    app.events.navigate()
    app.events.delete_event(s1)
    assert caldav_lacks(caldav_a, cal_a, s1, timeout=PROP_TIMEOUT), (
        f"{s1!r} still visible to A after linux deleted it"
    )
    assert caldav_lacks(caldav_b, cal_b, s1, timeout=PROP_TIMEOUT), (
        f"{s1!r} still visible to B after linux deleted it"
    )

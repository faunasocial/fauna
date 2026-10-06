"""tier_3: the macOS Calendar.app "add a calendar" flow — client-create via the
dedicated MKCALENDAR verb, end-to-end through the real binary stack.

The symmetric WRITE companion to `test_caldav_discovery_sequence.py` (which pins
the READ/discovery WALK). The user's macOS Calendar.app hand-proof
against example.com found CONNECT + READ + SYNC worked live but **creating a
calendar in Calendar.app failed** — the alert "This is not a location that
supports this request". Root cause: macOS issues the dedicated RFC 4791 §5.3.1
`MKCALENDAR` verb, which emersion/go-webdav routes nowhere (it knows only `MKCOL`)
→ `405`; and even once routed, the macOS calendar URL is an opaque UUID slug, not
the 64-hex the old path parser required. Slice 1 fixed both: `newMkcalendarInterceptor`
(routes `MKCALENDAR` → `provision_calendar(update_metadata=false)`) + a shared
`resolveCalendarSegment` (64-hex → decode; else → `blake3(slug)[:32]`) used by
every path parser, so the client's slug resolves to the same `calendar_id` on
every later PROPFIND/PUT/REPORT/DELETE.

`internal/mda/caldav/mkcalendar_test.go` pins the parse/seal/outcome in an
in-process Go twin (no nest). THIS test pins the same create through the FULL
stack — real nest + real MDA, real HTTPS, real seal of the collection metadata
with the actor's MLS pubkey, real `provision_calendar` insert into
`bridge_caldav_calendars` — then writes + reads an event back **at the client's
own slug URL**, which is the macOS contract `resolveCalendarSegment` guarantees
(a real client keeps using its slug, never re-homing to the server's canonical
hex href). A stub can't catch the seal / provision / segment-routing drift this
exercises; only tier_3 — or the user's manual Mac hand-proof — does. This makes
the create-calendar guarantee a standing regression guard rather than a one-shot
manual proof.

caldav-server.md § Write surface (MKCALENDAR row) + § Implementation status today
(macOS Calendar.app hand-proof bullet).
"""

import time
import uuid

import pytest

from helpers.caldav_client import CalDAVClient, CalDAVError, build_vevent

# Reuse the dedicated-nest admin-inject + enable-mail preamble — priority #2 (the
# canonical "log the client in as the dedicated nest's admin + alias it a mail
# address" path, shared with test_caldav_discovery_sequence.py /
# test_caldav_client_seal_to_mua.py; one preamble, not a copy per test).
from . import test_mail_enable_then_mua_round_trip as rt

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    # web added: MKCALENDAR is driver-agnostic raw HTTP
    # on top of the shared enable_mail_plain preamble, already proven on web.
    pytest.mark.web,
]

# The PLAIN mail credential the client mints and the MUA authenticates with
# (shared IMAP + CalDAV, AEAD-unwrap-as-auth).
_PASSWORD = "CalDavMkcalendarPlainPw0007Gg"

# How long to wait for the event PUT to surface to a calendar-query REPORT (a
# server-side seal, so usually immediate — a short retry guards a settle race).
_PROP_TIMEOUT = 30.0


def _utc(offset_min: int) -> str:
    """ISO basic-format UTC timestamp `offset_min` minutes from now (the format
    build_vevent / the linux Events form use)."""
    return time.strftime("%Y%m%dT%H%M00Z", time.gmtime(time.time() + offset_min * 60))


@pytest.mark.feature("calendar-in-standard-apps")
def test_macos_style_mkcalendar_create_then_event_round_trip(app, dedicated_mail_nest, request):
    """Drive the exact CalDAV exchange macOS Calendar.app performs to ADD a
    calendar — a MKCALENDAR at a client-chosen UUID slug carrying an initial
    displayname/color — then prove the client's own slug URL round-trips: the new
    calendar is discoverable, reports its sealed displayname, and an event PUT at
    the slug surfaces to a calendar-query REPORT at the SAME slug. Re-MKCALENDAR on
    the existing slug is 405.

    RED before Slice 1: MKCALENDAR → 405 ("This is not a location that supports
    this request"); and even routed, a non-hex slug would not resolve on the
    follow-up PUT/REPORT without the shared resolveCalendarSegment.
    """
    app.mail_settings.require_scripted_mua_seed_supported()

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    # 1) Log the client in as the dedicated nest admin + enable mail through its
    #    own UI — mints the recipient pubkey + wrapped-MSEK + MLS snapshot the
    #    MDA seals the new calendar's metadata with (sess.MLSPubkey()).
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

    # 2) Baseline: the lazy Personal calendar is auto-created on first PROPFIND.
    before = mua.list_calendars()
    assert before, "the lazy Personal calendar must exist before client-create"

    # 3) macOS "add a calendar" → MKCALENDAR at an opaque UUID slug (NOT 64-hex,
    #    so it routes through resolveCalendarSegment's blake3 branch) carrying the
    #    client's initial displayname + color, exactly as Calendar.app sends.
    slug = str(uuid.uuid4()).upper()
    displayname = f"QA-MkCal-{int(time.time())}"
    cal_href = mua.mkcalendar(
        slug, displayname=displayname, color="#FF2D55", description="added in Calendar.app"
    )  # raises CalDAVError on any non-201

    # 4) The new calendar must be discoverable AND carry the sealed displayname the
    #    client set — proves the seal → provision → decrypt-on-read path, and that
    #    a PROPFIND of the client's own slug resolves to the provisioned id.
    after = mua.list_calendars()
    assert len(after) == len(before) + 1, (
        f"MKCALENDAR must add exactly one calendar; before={before!r} after={after!r}"
    )
    got_name = mua.displayname(cal_href)
    assert got_name == displayname, (
        f"the new calendar at the client slug {cal_href!r} must report the sealed "
        f"displayname {displayname!r}; got {got_name!r}"
    )

    # 4b) Rename via a standalone PROPPATCH (the macOS "rename after creating"
    #     flow) and prove the new name PERSISTS through the real seal→provision→
    #     decrypt-on-read path. This is the contract the create-then-rename race
    #     fix must not regress (caldav-server.md § Create-then-rename race): a
    #     post-create rename always sticks because the calendar is fully
    #     committed before the PROPPATCH runs. (The race itself — a rename that
    #     beats its own create's visibility — is pinned deterministically by the
    #     Go twin internal/mda/caldav/props_test.go::TestPropPatchAbsorbsCreateThenRenameRace,
    #     which can stage the empty→populated list_calendars window a sequential
    #     full-stack test can't.)
    renamed = f"{displayname}-renamed"
    mua.proppatch_displayname(cal_href, renamed)  # raises CalDAVError on a non-207
    got_renamed = mua.displayname(cal_href)
    assert got_renamed == renamed, (
        f"a post-create PROPPATCH rename must persist; calendar {cal_href!r} reported "
        f"{got_renamed!r}, want {renamed!r} (the rename did not round-trip through "
        "the metadata re-seal)"
    )

    # 5) Write + read an event back at the CLIENT'S OWN slug URL — the macOS
    #    contract resolveCalendarSegment guarantees (the client keeps PUTting /
    #    REPORTing at its slug, never the server's canonical hex href). A break in
    #    any single path parser's slug→id mapping would lose the event here.
    uid = f"mkcal-evt-{int(time.time())}@fauna"
    summary = f"MkCal round-trip {int(time.time())}"
    mua.put_event(cal_href, uid, build_vevent(uid, summary, _utc(60), _utc(120)))  # raises on non-2xx

    deadline = time.monotonic() + _PROP_TIMEOUT
    seen: list[str] = []
    while time.monotonic() < deadline:
        seen = mua.summaries(cal_href)
        if summary in seen:
            break
        time.sleep(2.0)
    assert summary in seen, (
        f"event {summary!r} PUT at the client slug {cal_href!r} never surfaced to a "
        f"calendar-query REPORT at the same slug within {_PROP_TIMEOUT:.0f}s; saw {seen!r}. "
        "The MKCALENDAR-created calendar's slug must resolve identically across PUT + "
        "REPORT (shared resolveCalendarSegment)."
    )

    # 6) RFC 4791 §5.3.1 / RFC 4918 §9.3: re-MKCALENDAR on an existing collection →
    #    405 Method Not Allowed, *regardless of metadata*. Both a re-create with
    #    IDENTICAL props and one with DIFFERENT props (bare → defaults) must be 405.
    #    This is the path the in-process Go twin can't exercise: SealCollectionMetadata
    #    HPKE-re-seals with a fresh ephemeral key each call, so neither re-create's
    #    ciphertext byte-matches the stored blob and the nest answers Conflict either
    #    way — which RFC maps to 405 for a create verb. (Returning the nest's raw 409
    #    here was an RFC §5.3.1 violation this tier_3 test caught.)
    for label, kwargs in (
        ("identical-metadata", dict(displayname=displayname, color="#FF2D55",
                                    description="added in Calendar.app")),
        ("different-metadata (bare → defaults)", {}),
    ):
        with pytest.raises(CalDAVError) as exc:
            mua.mkcalendar(slug, **kwargs)
        status = getattr(exc.value.resp, "status_code", None)
        assert status == 405, (
            f"re-MKCALENDAR on an already-provisioned calendar ({label}) must be 405 "
            f"Method Not Allowed (RFC 4791 §5.3.1), not the nest's raw Conflict 409; got {status!r}"
        )

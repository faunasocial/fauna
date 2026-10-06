"""CalDAV onboarding-variant matrix — any-locator regression guard.

A tier_3 matrix: for a zero-config nest onboarded entirely through the client
UI, assert that every ``address-type × enablement`` combination yields a
working CalDAV calendar — verified by a 3-client round-trip (2 Python CalDAV
MUAs + the native app's Events page), each of the three in turn creating an
event that must become visible in the other two.

No-modes retirement (ratified 2026-07-12): every nest is sealed at rest
unconditionally now, so the former ``storage-mode`` axis (encrypted/plaintext)
is RETIRED — there is no client-observable difference left to matrix over, so
this file drops that axis entirely (was 12 cells = 3 address_type × 2 storage
× 2 enablement; now 6 = 3 address_type × 2 enablement). Enablement itself is
no longer an onboarding-time checkbox choice either — it's a MACHINE-DERIVED
default (ON iff the handle targets a real registerable domain) applied by the
post-claim launch glue; a cell that wants a NON-default enablement combination
(e.g. "caldav-only" on a real-domain handle, where mail derives ON) reaches it
via the REAL admin Mail / Calendar settings toggle post-claim
(`helpers.caldav_onboarding.onboard_and_enable` § step 4) — the same path a
real user would use, not an onboarding-time checkbox.

All 6 cells (real_domain / localhost / ip × mail+caldav / caldav-only) target
``expect="works"`` and run GREEN — the **any-locator serving work is LANDED
and tier_3-proven** (the design's Definition of Done, § 5, which overturns the
``2026-06-17`` "IP-only = confident negative" verdict). On a domainless /
bare-IP / localhost nest the MDA serves nest's self-signed FLOOR cert (the Go
MDA builds its TLS provider even with no PrimaryDomain — main.go; binds
CalDAV/IMAP with no mail_domains rows — mda.go; and nest writes the floor on
every entry path via ``start_server`` → ``self_signed_cert::ensure_floor_present``,
so even the plain-HTTP ``FAUNA_INSECURE_DISABLE_TLS`` e2e nest has one), and
local IMAP/CalDAV login resolves the bare handle via the handle→actor store
(Change A, ``validate_recipient`` fallback). The MTA correctly idles on a
domainless box (external mail needs a domain, spec § 6) and the onboarding
helper skips its serving-wait there (design tracked internally). These cells
carry NO ``xfail`` marker — a regression must surface as a RED here, never a
silent xfail.

Two run lanes:
  - The GUARD cell (``test_guard_real_domain_mail_caldav``) is the always-on
    regression baseline — the local twin of ``test_caldav_live_nest``
    (``real_domain × mail+caldav``). It MUST stay green.
  - The 6-cell parametrized ``test_caldav_onboarding_variant`` is the opt-in
    matrix, gated behind ``-m caldav_matrix``.

Authority: ``docs/goal/behavior/caldav-server.md`` (§ Independent enablement,
§ Authentication, § Network exposure); design decisions tracked internally
(§ 5 DoD; §§ 4–5, overturned for the IP cells).
"""

import time

import pytest

from helpers.caldav_client import CalDAVClient
from helpers.caldav_onboarding import onboard_and_enable
from helpers.mail_dedicated_nest import dedicated_node_url
from helpers import caldav_roundtrip as rt

# windows added 2026-07-18 (was a stale exclusion, plus a redundant in-body
# skip duplicating this same restriction — e2e rule 7 forbids platform checks
# in test files); see test_caldav_onboarding_derived_enablement.py's marker
# comment for the shared onboarding-mechanism evidence. `onboard_and_enable`
# additionally drives the admin settings toggle + the native Events page
# (`app.events.select_calendar`), both already windows-portable per the
# windows-green `test_caldav_client_seal_to_mua.py`. Live-verify owed
# (tracked internally).
# tui added 2026-08-21: CONFIRMED GREEN after a real fix, not just a
# theoretical gap. `test_guard_real_domain_mail_caldav` reliably reds on tui with the
# MDA's CalDAV listener never binding within 180s — root-caused via the MDA's own log
# (`mail-bridge-mda.log`): the MDA cold-boots from the UNCLAIMED nest before onboarding
# finishes, so its FIRST `mda.Run` reads a PARTIAL gating tuple (e.g.
# `mail_enabled=false, caldav_enabled=true` — the two enablement RPCs land as
# independent fire-and-forget calls, not atomically). The moment the second flag
# lands, nest's `config_changed` push makes the MDA exit 0 for a supervisor rebind
# (`mda.go`'s documented "the in-process listener set can't be re-bound live" —
# `bins/fauna-bridges/internal/mda/mda.go:469-494`) — production runs it under s6;
# this e2e harness's bare subprocess (`conftest.py::_spawn_mda_bridge`) has none, and
# nothing in `onboard_and_enable`/`wait_caldav_serving` ever played that role, so the
# MDA just stayed dead for the rest of the 180s wait. Not tui-specific in origin (the
# same race is reachable from any app depending on relative onboarding timing; linux
# apparently never lands in the torn window) — fixed at the root:
# `caldav_roundtrip.wait_caldav_serving` gained an optional `mda=` param that plays
# the exit-for-rebind's supervisor role, exactly like `test_caldav_admin_port_rebind.py`'s
# established `mda.respawn()` pattern for the analogous port-change rebind. Verified:
# the MDA log for a green `--app tui` run shows the exact predicted sequence (cold-boot
# with `mail_enabled:false,caldav_enabled:true` → "mda listener gating changed;
# exiting for s6 rebind" 0.7s later → the harness's respawn → the SECOND `mda.Run`
# with the full tuple → CalDAV serves). `git log --grep 'the MDA.s exit-for-rebind
# needs the e2e harness to play supervisor'`.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.tui,
    # web added 2026-09-21 at MODULE level on purpose: it
    # enrolls the guard (outcome 2's witness) as well as the matrix (outcome 3's),
    # and both were run and recorded on web in the same pass — widening a module
    # is only honest when every test it widens has been run on the new column.
    # `onboard_and_enable` takes the app's dial address from the caller: the
    # variant nest's own SPA proxy on web (`dedicated_node_url` →
    # `spa_proxy_for`), since a browser cannot reach a raw nest.
    pytest.mark.web,
]


@pytest.mark.feature("calendar-in-standard-apps")
def test_guard_real_domain_mail_caldav(app, unclaimed_caldav_nest, request):
    """KNOWN-GREEN regression guard: real-domain + mail&caldav → full 3-client
    round-trip works (local twin of ``test_caldav_live_nest``).

    This is the load-bearing regression baseline and the first end-to-end
    exercise of ``onboard_and_enable``; it MUST end green.
    """
    h = unclaimed_caldav_nest("real_domain")
    creds = onboard_and_enable(
        app, h, node_url=dedicated_node_url(app, h, request),
        enable_mail=True, enable_caldav=True,
    )
    base = f"https://127.0.0.1:{h.caldav_port}"
    rt.wait_caldav_serving(base, verify=False, mda=h.mda)
    mua_a = CalDAVClient(base, creds["handle"], creds["password"], verify=False)
    mua_b = CalDAVClient(base, creds["handle"], creds["password"], verify=False)
    cal_a, cal_b = mua_a.personal_calendar(), mua_b.personal_calendar()
    rt.wait_calendars_ready(app)
    app.events.select_calendar("Personal")
    rt.run_create_visibility_matrix(
        app, mua_a, cal_a, mua_b, cal_b, nonce=f"g{int(time.time())}"
    )


# ── The 6-cell any-locator matrix ─────────────────────────────────────────────
#
# (address_type, enable_mail, enable_caldav, expectation). Every cell is
# `expect="works"` — it runs the full 3-client round-trip. The handle-derived
# caldav default is still asserted per-cell (OFF for domainless, ON for a real
# domain; decision-2: default OFF, not force-disabled), but CalDAV is reachable
# + the round-trip succeeds on every address type once enabled — including a
# cell whose enablement combination differs from the derived default, reached
# via the admin settings toggle (see module docstring).

_CELL_FIELDS = "address_type,en_mail,en_caldav,expect"


def _cell_id(address_type, en_mail, en_caldav, expect):
    mail = "mail" if en_mail else "nomail"
    cal = "caldav" if en_caldav else "nocaldav"
    return f"{address_type}-{mail}-{cal}-{expect}"


# Raw cells (no xfail marks). Dispositions applied in CELLS below.
_RAW_CELLS = [
    ("real_domain", True, True, "works"),    # == guard
    ("real_domain", False, True, "works"),   # caldav-only (toggled off derived-ON mail)
    ("localhost", True, True, "works"),      # mail toggled on over derived-OFF
    ("localhost", False, True, "works"),     # caldav-only == the derived default here
    ("ip", True, True, "works"),             # mail toggled on over derived-OFF, by IP
    ("ip", False, True, "works"),            # caldav-only == the derived default here
]


def _param(cell, *, marks=()):
    return pytest.param(*cell, id=_cell_id(*cell), marks=marks)


# All 6 cells target `expect="works"` and run green — the any-locator serving
# work is LANDED and tier_3-proven (the design's Definition of Done, § 5; it
# overturns the `2026-06-17` "IP-only = confident negative" verdict). On a
# domainless / bare-IP / localhost nest:
#   • the MDA serves nest's self-signed FLOOR cert with no registered mail-domain
#     (main.go builds the TLS provider when PrimaryDomain is empty; mda.go binds
#     CalDAV/IMAP with no mail_domains rows; `start_server` writes the floor on
#     every entry path so even the plain-HTTP e2e nest has one — caldav-imap-any-
#     locator); and
#   • login resolves the bare handle via the handle→actor store (Change A,
#     `validate_recipient` fallback). The MTA correctly idles (external mail needs
#     a domain, spec § 6) and the helper skips its serving-wait for a domainless
#     nest — CalDAV + local IMAP serve; the round-trip exercises CalDAV.
# (The earlier `xfail(strict=True)` placeholders for the ip/localhost cells were
# removed when the follow-ups landed — keep these cells un-marked; a regression
# must surface as a RED here, never a silent xfail.)
CELLS = [_param(c) for c in _RAW_CELLS]


@pytest.mark.caldav_matrix
@pytest.mark.parametrize(_CELL_FIELDS, CELLS)
@pytest.mark.feature("calendar-in-standard-apps")
def test_caldav_onboarding_variant(
    app, unclaimed_caldav_nest, request, address_type, en_mail, en_caldav, expect
):
    """One matrix cell: onboard a fresh zero-config nest through the client UI
    for this address_type, settle to the derived mail/caldav default, then
    (for a cell that wants a different combination) flip the mismatched axis
    through the admin settings toggle — then run the full 3-client round-trip.
    Every cell `expect`s "works".
    """
    assert expect == "works", f"every cell now runs the round-trip; got expect={expect!r}"
    h = unclaimed_caldav_nest(address_type)
    creds = onboard_and_enable(
        app, h, node_url=dedicated_node_url(app, h, request),
        enable_mail=en_mail, enable_caldav=en_caldav,
    )
    base = f"https://127.0.0.1:{h.caldav_port}"

    # The handle-derived caldav default stays what it was (decision-2: default
    # OFF, NOT force-disabled): OFF for a domainless / bare-IP / localhost nest
    # (the user opts in), ON for a real domain. The any-locator fix makes
    # CalDAV *work when enabled* on a domainless nest — it does NOT flip the
    # default. `caldav_default` reflects the SETTLED state read right after
    # claim, before `onboard_and_enable` applied any admin-toggle adjustment.
    expect_default_on = address_type == "real_domain"
    assert creds["caldav_default"] is expect_default_on, (
        f"{address_type} nest: caldav derived default should be "
        f"{expect_default_on}, got {creds['caldav_default']!r}"
    )

    rt.wait_caldav_serving(base, verify=False, mda=h.mda)
    mua_a = CalDAVClient(base, creds["handle"], creds["password"], verify=False)
    mua_b = CalDAVClient(base, creds["handle"], creds["password"], verify=False)
    cal_a, cal_b = mua_a.personal_calendar(), mua_b.personal_calendar()
    rt.wait_calendars_ready(app)
    app.events.select_calendar("Personal")
    rt.run_create_visibility_matrix(
        app, mua_a, cal_a, mua_b, cal_b,
        nonce=f"{address_type[:2]}{int(time.time())}",
    )

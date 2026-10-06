"""tier_3 e2e: the domain-expiry watch reaches the every-page banner.

Goal docs: ``docs/goal/architecture/nest/domains-and-tls-bootstrap.md``
§ Domain loss → *Detection* (the mechanism: nest RDAP fetch/persist, a
User-class read kind, the sweep feeder, and the two-arm signal set) and
``docs/goal/behavior/critical-alerts.md`` § Feeders (the ``domain-expiry`` row
and why it clears the severity bar).

**What only a full-stack run can prove.** The two halves are separately pinned
already — ``fauna_protocol::domain_expiry``'s unit tests own the two-arm
decision, and the sweep crate's own tests own the post/clear/skip behavior
against an in-memory transport. Neither can reach the claim the feature rests
on: that a record the **nest** wrote, served over a **real** ``fauna.domain.
expiry.get`` on a real WS-RPC connection, decodes on the client and lands in the
shared registry as a banner. That path crosses two binaries and a wire encoding
— exactly the drift tier_3 exists to catch, and exactly what a mocked feeder
cannot see.

**Why the record is seeded rather than fetched.** The watch's input is a public
registry reached over the internet: no test can make a domain enter
``redemptionPeriod``, and pointing the watch at a local fake would test the
fake. The seam is therefore cut at the *record* (``/api/v1/test/domain-expiry``,
``test-hooks``-gated) — everything downstream of the fetch is real. The fetch
and RDAP-parse halves upstream of it are pinned by
``bins/fauna-nest/src/domain_expiry.rs``'s own unit tests.

The status arm is the one driven here, deliberately: a future-dated
registration carrying ``redemptionPeriod`` is the exact case the second arm
exists for (a registry auto-renewal pushes the date a year out on a domain that
is already dying), so a test using it proves the arm rather than re-proving the
date comparison.

Latency discipline (convention 14): the sweep is spawned at session
establishment, so the assertion is a deadline poll for a caused state
transition with a generous ceiling — a green run pays only the real round-trip.
No settle-sleeps, no wall-clock asserts.

tier_3: needs a real ``fauna-nest`` binary built with ``--features test-hooks``.
Runs on any app that renders the banner and calls the sweep — today tui, the
lead app; the other six inherit it with no change of their own, since this
feeder joined by the sweep's seam alone.
"""
from __future__ import annotations

import json
import time
import urllib.request

import pytest

from i18n.strings import S

pytestmark = pytest.mark.tier_3

CRITICAL_ALERTS = "critical-alerts"
CRITICAL_ALERT = "critical-alert"

# The domain the seeded record is about. Never fetched — the seam is the record.
_DOMAIN = "lapsing-example.test"
_LAPSE_STATUS = "redemption period"

# A distinctive fragment, so the assertion pins *this* feeder rather than "some
# alert is up" — another feeder's alarm would satisfy a bare presence check.
#
# Deliberately a phrase BOTH role lines share ("…and is being withdrawn from
# DNS" for an admin, "…is being withdrawn ({status})" for a resident), so the
# presence polls stay role-agnostic and the *role* is checked once, exactly, by
# the full-line assertion below. The first version of this test used an
# admin-only fragment and failed against a perfectly correct banner — the test
# actor is an ordinary resident, which is the case worth covering here anyway.
_HEADLINE_FRAGMENT = "is being withdrawn"


def _seed_record(
    nest_url: str,
    *,
    domain: str,
    outcome: str,
    expires_at: int | None = None,
    statuses: list[str] | None = None,
    detail: str | None = None,
) -> None:
    """``POST /api/v1/test/domain-expiry`` — write the watch's record."""
    body = json.dumps(
        {
            "domain": domain,
            "outcome": outcome,
            "expires_at": expires_at,
            "statuses": statuses or [],
            "detail": detail,
        }
    ).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/domain-expiry",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10.0) as resp:
        assert resp.status == 200, f"seed domain-expiry returned {resp.status}"


def _alert_text(app) -> str:
    """The first active alert row's text, or empty when the banner is absent.

    Presence of the banner *is* the rendering contract
    (``critical-alerts.md`` § Rendering contract), so callers never test the
    registry a second way. Row 0 is addressed by driver-level index, never by a
    ``critical-alert[0]`` id string.
    """
    if not app.driver.is_visible(CRITICAL_ALERTS):
        return ""
    return app.driver.get_text(CRITICAL_ALERT) or ""


def _poll_for(app, fragment: str, deadline_s: float = 60.0) -> str:
    """Deadline-poll the feed page until the banner contains ``fragment``."""
    deadline = time.monotonic() + deadline_s
    text = ""
    while time.monotonic() < deadline:
        text = _alert_text(app)
        if fragment in text:
            return text
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
    return text


def _poll_until_gone(app, fragment: str, deadline_s: float = 60.0) -> str:
    """Deadline-poll until the banner no longer contains ``fragment``."""
    deadline = time.monotonic() + deadline_s
    text = ""
    while time.monotonic() < deadline:
        text = _alert_text(app)
        if fragment not in text:
            return text
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
    return text


@pytest.mark.feature("critical-alerts")
def test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it(
    app, nest_instance, request
):
    """A lapse-class registration is loud on the feed page after a session
    start, and a healthy record on the next sweep takes the banner down.

    Both halves in one journey on purpose: a feeder that can post but never
    clear leaves a permanent, non-dismissable alarm about a domain that was
    renewed weeks ago, which is the crying-wolf failure the severity bar exists
    to prevent — so "it clears" is as load-bearing as "it posts", and asserting
    only the first would let the regression through.
    """
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)

    # ── 1. The nest's watch has seen a lapse-class registration. ──
    # A *future* expiry, so only the status arm can be what alarms.
    _seed_record(
        nest_instance["url"],
        domain=_DOMAIN,
        outcome="checked",
        expires_at=int(time.time()) + 365 * 24 * 60 * 60,
        statuses=[_LAPSE_STATUS],
    )

    # ── 2. Establish the session: the universal post-auth hook, the only thing
    #       that runs the sweep. ──
    _login_app_as(app, request, nest_instance, user)

    # ── 3. The banner is up on the feed page, which nothing in this condition
    #       lives on — the set-and-forget half of the contract. ──
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    text = _poll_for(app, _HEADLINE_FRAGMENT)
    assert _HEADLINE_FRAGMENT in text, (
        "a lapse-class registration must reach the every-page banner after a "
        f"session start; banner reads {text!r}, error surface: "
        f"{app.error_text()!r}"
    )
    # The shared i18n line, rendered verbatim, so all seven apps say the same
    # thing. This actor is an ordinary resident, not an admin — which is the
    # ratified audience rule at the one place it is observable end-to-end.
    expected = S.critical_alerts.domain_lapsing_resident(
        domain=_DOMAIN, status=_LAPSE_STATUS
    )
    assert expected in text, (
        "a non-admin must get the RESIDENT line (their addresses and recovery "
        f"locator die too, but they cannot renew); got {text!r}"
    )
    assert _DOMAIN in text, f"the alert must name the domain at stake; got {text!r}"
    assert _LAPSE_STATUS in text, (
        f"the alert must name the registry status it read; got {text!r}"
    )

    # ── 4. The registration is renewed — the next sweep must take the banner
    #       down, and nothing else may. ──
    _seed_record(
        nest_instance["url"],
        domain=_DOMAIN,
        outcome="checked",
        expires_at=int(time.time()) + 400 * 24 * 60 * 60,
        statuses=["active"],
    )
    _login_app_as(app, request, nest_instance, user)
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    text = _poll_until_gone(app, _HEADLINE_FRAGMENT)
    assert _HEADLINE_FRAGMENT not in text, (
        "a renewed registration must clear the banner — a non-dismissable alarm "
        f"about a resolved condition is the crying-wolf failure; got {text!r}"
    )


@pytest.mark.feature("critical-alerts")
def test_an_rdap_failure_leaves_a_standing_alarm_alone(app, nest_instance, request):
    """Unreachable is not resolved.

    The fail-safe direction, end-to-end: the nest recorded that RDAP itself
    errored, so it knows nothing new — and "nothing new" must never read as
    "the domain is fine". This is the arm a mocked feeder proves in isolation
    but nobody proves *across the wire*, and it is the one whose regression is
    silent: the banner simply disappears and the user is never warned again.
    """
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)

    _seed_record(
        nest_instance["url"],
        domain=_DOMAIN,
        outcome="checked",
        expires_at=int(time.time()) + 365 * 24 * 60 * 60,
        statuses=[_LAPSE_STATUS],
    )
    _login_app_as(app, request, nest_instance, user)
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    assert _HEADLINE_FRAGMENT in _poll_for(app, _HEADLINE_FRAGMENT), (
        "precondition: the alarm must be standing before the failure is seeded"
    )

    # RDAP went down. The record now says so, and carries NO statuses and NO
    # expiry — i.e. read naively it looks exactly like a healthy silent domain.
    _seed_record(
        nest_instance["url"],
        domain=_DOMAIN,
        outcome="failed",
        detail="503 from the registry's RDAP service",
    )
    _login_app_as(app, request, nest_instance, user)
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})

    # A causal barrier, not a settle-sleep: the sweep that must NOT clear is the
    # one this session start just ran, so wait for a *positive* transition that
    # can only happen after it completed — the banner still being up once the
    # feeder has demonstrably run again. Poll the whole window and assert the
    # alarm survived all of it.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        assert _HEADLINE_FRAGMENT in _alert_text(app), (
            "an RDAP failure must leave a standing alarm alone — unreachable is "
            "not resolved, and clearing here means a user stops being warned "
            "about a domain that is still dying"
        )
        time.sleep(0.5)  # sleep-ok: re-asserting an invariant across the window


@pytest.mark.feature("critical-alerts")
def test_an_admin_is_warned_with_the_remedy_that_is_theirs(app, nest_instance, request):
    """The same lapse reaches the admin with the ADMIN line — the renewal is
    theirs to make, so their line names the registrar, not "whoever runs this
    nest".

    The other half of the role-differentiated audience rule the resident journey
    above pins (`critical-alerts.md` § Feeders — the domain-expiry feeder's
    audience is every authenticated user, with lines by role; the nest answers
    the role in the same reply as the record, so this is also the one place the
    `admin` bit is proven to cross the wire end-to-end). Asserted on the page the
    admin lands on, which is not where the condition lives.
    """
    from conftest import _login_admin_as

    _seed_record(
        nest_instance["url"],
        domain=_DOMAIN,
        outcome="checked",
        expires_at=int(time.time()) + 365 * 24 * 60 * 60,
        statuses=[_LAPSE_STATUS],
    )
    try:
        # Establishing the admin session is what runs the sweep — seeded first,
        # so the pass that starts at sign-in is the one that reads the lapse.
        _login_admin_as(
            app,
            request,
            nest_instance,
            spa_url_fixture="spa_url",
            fixture_name="test_an_admin_is_warned_with_the_remedy_that_is_theirs",
        )
        text = _poll_for(app, _HEADLINE_FRAGMENT)
        expected = S.critical_alerts.domain_lapsing_admin(
            domain=_DOMAIN, status=_LAPSE_STATUS
        )
        assert expected in text, (
            "the nest's admin must get the ADMIN line (only an admin can renew, so "
            f"their remedy is the registrar); got {text!r}, error surface: "
            f"{app.error_text()!r}"
        )
        resident = S.critical_alerts.domain_lapsing_resident(
            domain=_DOMAIN, status=_LAPSE_STATUS
        )
        assert resident not in text, (
            f"the admin was shown the RESIDENT remedy as well; got {text!r}"
        )
    finally:
        # Deployment-scoped: a lapse left standing would alarm every later test
        # on this nest.
        _seed_record(
            nest_instance["url"],
            domain=_DOMAIN,
            outcome="checked",
            expires_at=int(time.time()) + 400 * 24 * 60 * 60,
            statuses=["active"],
        )

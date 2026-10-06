"""The awaiting-manual-DNS poll must not leak an unhandled promise rejection.

``recheckAwaitingDns()`` (``+page.svelte``) drives ``recheck_manual_dns()`` on a
``setInterval`` for as long as the user sits on the "Almost ready" surface
(``docs/goal/behavior/onboarding.md`` § "Almost ready" surface). Its structural
twin, the pending-invite poll (``onPollInvite()``), already wraps its single
await in a try/catch and logs at ``warn`` on failure — this poll did not, so any
failure of ``recheckManualDns()`` (an actual wasm/JS throw, not the ordinary
unreachable-nest case — ``recheck_manual_dns()`` swallows that to a still-waiting
``Pending`` snapshot internally, see ``machine.rs::recheck_manual_dns``) would
have surfaced as an unhandled promise rejection every poll interval.

This test drives a REAL recheck against the fixture's unreachable loopback nest
(``go_to_awaiting_manual_dns``'s default ``nest_url``) via the recheck button —
the same call path the interval timer takes — and asserts the poll settles back
to "still waiting" with no ``[pageerror]`` in the browser's console ring
(``server.py``'s ``_attach_console_capture``, which surfaces Chromium's
``pageerror`` event for exactly this class of bug). Because
``recheck_manual_dns()`` is infallible on this path, this does not red-first the
original defect (no fixture reaches the actual throw the try/catch guards) — it
proves the real poll path stays clean, and stands as the regression guard for
that path a genuine fault-injection test would need new wasm-level test-hook
infrastructure to reach beyond it.

Web-specific (the console/pageerror ring is a Playwright-only signal — other
apps have no browser console to leak into). Lives in ``tests/web/`` so the
cross-app conftest auto-deselects for non-web apps.
"""

from __future__ import annotations

import time

import pytest

pytestmark = [pytest.mark.web, pytest.mark.tier_2]


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_recheck_against_unreachable_nest_logs_no_pageerror(app):
    app.onboarding.go_to_awaiting_manual_dns()

    app.click("awaiting-dns-recheck-button")

    # Deadline poll on the real settle signal (the busy-derived disabled state
    # clearing once the machine snapshot returns to `Pending`), never a fixed
    # sleep (e2e-conventions.md convention 14).
    deadline = time.monotonic() + 15
    settled = app.driver.is_enabled("awaiting-dns-recheck-button")
    while time.monotonic() < deadline and not settled:
        time.sleep(0.3)
        settled = app.driver.is_enabled("awaiting-dns-recheck-button")
    assert settled, (
        "awaiting-dns-recheck-button never re-enabled after the recheck -- "
        f"the poll may be wedged: {app.driver.diagnose('awaiting-dns-recheck-button')} "
        f"error={app.error_text()!r}"
    )

    # Still on the surface -- an unhandled rejection or a mis-routed outcome
    # would either wedge here or, per the Done-is-still-waiting gotcha, exit
    # the wizard onto a nest that never claimed.
    assert app.is_visible("awaiting-dns-records"), (
        "expected to remain on the 'Almost ready' surface after a recheck "
        f"against an unreachable nest: error={app.error_text()!r}"
    )

    console = app.driver.console_log()
    pageerrors = [line for line in console if "[pageerror]" in line]
    assert not pageerrors, (
        "recheckAwaitingDns() leaked an unhandled promise rejection during a "
        f"real poll against an unreachable nest: {pageerrors!r}"
    )

"""The `/app` root index redirect has ONE owner — the layout guard.

Web-only routing concern (SvelteKit `base: '/app'`; native apps have no
URL-based redirect, so this is a structural web-only test, not a platform skip).

The nest serves the SPA at `nest_service("/app", …)` (bins/fauna-nest/src/lib.rs)
and its info page links to bare `/app` (no trailing slash, lib.rs), so a real
user can enter the app at EITHER `/app` or `/app/`.

Before the one-owner fix, TWO `onMount` redirects competed on the index:
  - the root `+page.svelte` unconditionally `goto('/app/conversations')`, and
  - the `+layout.svelte` guard `goto('/app/onboarding')` when unauthenticated.
The layout guard only matched `pathname.startsWith('/app/')`, so at the bare
`/app` (no trailing slash) it MISSED entirely — the index page's redirect won
and an UNAUTHENTICATED user landed on the feed shell with no identity, stranded,
the exact CR-2 state the guard exists to prevent (common.md § Client-state
recoverability). Even at `/app/` the two redirects raced, the winner decided by
Svelte's onMount ordering — a "one owner per claim" violation.

The fix makes the layout the single owner: it matches `/app` too, routes an
unauthenticated (or pending-factory-reset) entry to onboarding, and — when
authenticated on the bare index — to the default landing page. The index
`+page.svelte` no longer redirects, so nothing races the guard.

The witness is `location.pathname` after the redirect settles: the routing
decision itself, independent of which surface elements happen to render.
"""
from __future__ import annotations

import time

import pytest

from drivers import create_driver

pytestmark = [pytest.mark.tier_3, pytest.mark.web]


def _settle_pathname(driver, *, want: str, timeout: float = 15.0) -> str:
    """Poll `location.pathname` until it reaches `want` (prefix) or stops
    changing for >1s, so we read the SETTLED route after any onMount redirect,
    not the transient landing. Returns the final pathname."""
    deadline = time.monotonic() + timeout
    last = None
    stable_since = None
    final = ""
    while time.monotonic() < deadline:
        cur = driver.eval_js("location.pathname") or ""
        final = cur
        if cur.startswith(want):
            return cur
        if cur == last:
            if stable_since is None:
                stable_since = time.monotonic()
            elif time.monotonic() - stable_since > 1.0:
                return cur
        else:
            last = cur
            stable_since = None
        time.sleep(0.25)
    return final


def test_bare_app_root_unauth_settles_on_onboarding(spa_url):
    """An UNAUTHENTICATED entry at the bare `/app` (no trailing slash) settles on
    onboarding — the layout guard, the single owner of the index redirect,
    catches it — NOT on the feed shell where the competing +page.svelte redirect
    used to strand it (CR-2)."""
    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        # Fresh BrowserContext ⇒ empty localStorage already; clear it explicitly
        # to make the unauthenticated precondition deterministic and self-documenting.
        driver.eval_js("localStorage.clear()")

        # Enter at the BARE /app root (no trailing slash) — the path the nest's
        # info-page link (`<a href="/app">`) produces, the one the layout guard
        # used to miss. `page.goto` waits for load, so onMount runs on arrival.
        bare_app = (driver._spa_url or "").rstrip("/")
        assert bare_app.endswith("/app"), f"unexpected spa url {driver._spa_url!r}"
        driver._post("/navigate", {"url": bare_app})

        pathname = _settle_pathname(driver, want="/app/onboarding")

        # Corroborate with the on-screen surface (query the DOM directly — no
        # agent needed): onboarding shows the identity-choice, the feed shell
        # shows navigation tabs.
        on_onboarding = driver.eval_js(
            "!!document.querySelector('[data-testid=\"create-identity-button\"]')"
        )
        on_feed_shell = driver.eval_js(
            "!!document.querySelector('[data-testid=\"conversations-tab\"]')"
            " || !!document.querySelector('[data-testid=\"feed-tab\"]')"
        )

        assert pathname.startswith("/app/onboarding"), (
            "an unauthenticated bare-/app entry did NOT settle on onboarding: "
            f"pathname={pathname!r}, on_onboarding={on_onboarding}, "
            f"on_feed_shell={on_feed_shell}. The index redirect must have ONE "
            "owner (the layout guard); the competing +page.svelte redirect used "
            "to strand the user on the feed shell "
            "(CR-2, common.md § Client-state recoverability)."
        )
        assert not on_feed_shell, (
            f"settled on {pathname!r} but the feed shell is showing "
            f"(on_feed_shell={on_feed_shell}) — an unauthenticated user is stranded."
        )
    finally:
        driver.teardown()

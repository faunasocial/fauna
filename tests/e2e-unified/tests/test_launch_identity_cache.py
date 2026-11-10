"""Your handle and tier show at launch from what the app last saw, and stay in
place while your nest cannot be reached (``docs/features/account.md`` outcome 16).

``docs/goal/architecture/long-term-store.md`` § Implementation status today: the
signed-in account's handle and tier load from the registry's cache at launch,
and a failed refresh is logged and swallowed — the previous values stay visible.
The cache has ONE writer, the sign-in check's verify reply
(``session::establish → spawn_self_identity_follow → AccountRegistry::update_cache``),
so a changed tier reaches the app only on the next check. The launch surface
reads it through the one shared reader, ``AccountRegistry::launch_identity_line``
(``FfiAccountRegistry.launchIdentityLine`` native, ``accountIdentityLine`` wasm).

With the nest down the app never enters the shell, so the Settings identity cell
is not reachable: the cached identity shows on the launch surface, as
``launch-identity-label`` (``handle@domain · tier``) beside ``launch_retry``
(``docs/goal/behavior/onboarding.md`` § App-launch routing).

The journey, every wait on state (convention 14):

1. signed in with the nest up — the verify reply fills the cache, and the
   Settings identity cell paints the handle and tier;
2. the nest stops and the app restarts onto the same store — the retry surface
   paints the cached handle and tier;
3. the nest returns, an admin moves the account to another tier (the scenario,
   not a stand-in for a user action — convention 8), and the retry button makes
   the new sign-in check — the cache takes the new tier;
4. the nest stops again and the app restarts — the new, last good tier shows.

tui led the witness (``testing.md`` § Default app and nest mode); web, linux and
android joined in one trickle-down batch, the build having landed on all three
together. The restart is ``hard_reload(replay_login=False)`` over a pinned store
(``preserve_state_across_relaunch``) — no login replay, which with the nest down
would push the app off the launch surface — the shape ``test_offline_queued_like.py``
runs on the same apps; the remaining apps add their marker as theirs reaches them.
"""

from __future__ import annotations

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
]

#: The tier the account is moved onto in step 3 — a name no default tier has.
MOVED_TIER = "witness-plus"
IDENTITY = "launch-identity-label"
RETRY = "launch-retry-button"


def _shows(driver, *needles: str) -> bool:
    if not driver.is_visible(IDENTITY):
        return False
    text = driver.get_text(IDENTITY)
    return all(n in text for n in needles)


def _settings_cell(app, seen: dict) -> bool:
    """Open Settings and read the identity cell; False while the shell (or the
    cell) is not up yet — the retry's sign-in may still be in flight."""
    try:
        app.settings.navigate()
    except Exception as e:  # still on the launch surface
        seen["navigate"] = repr(e)
        return False
    d = app.driver
    if not (d.is_visible("settings-handle-label") and d.is_visible("settings-tier-label")):
        return False
    seen["handle"] = d.get_text("settings-handle-label")
    seen["tier"] = d.get_text("settings-tier-label").strip()
    return True


@pytest.mark.feature("account")
def test_launch_paints_the_cached_handle_and_tier_and_keeps_them_while_the_nest_is_down(
    app, request, nest_instance
):
    from common.auth import ensure_tier, set_user_tier
    from conftest import _make_user
    from helpers import enrollment

    nest = nest_instance
    user = _make_user(nest)
    actor = user["actor_id_hex"]
    handle = user["handle"]
    admin_sk = nest["admin"]["signing_key"]
    driver = app.driver

    enrollment.sign_in(app, request, nest, user)
    # The restart must be THIS device restarting: without the pin a relaunch
    # gets an empty store, and the cached identity would be gone for that reason.
    assert driver.preserve_state_across_relaunch(), (
        f"{type(driver).__name__} could not pin its client store across the "
        "relaunch, so the launch half of this outcome would be vacuous"
    )

    nest_down = False
    try:
        # (1) Signed in with the nest up, and the identity follower has landed the
        # handle and tier in the cache: the Settings cells read the same cache.
        seen: dict = {}
        wait_until(
            lambda: _settings_cell(app, seen) and handle in seen["handle"],
            APP_RELAUNCH_S,
            diagnose=lambda: f"the Settings identity cell never showed {handle!r} "
            f"(read {seen!r}): " + driver.diagnose("settings-handle-label"),
        )
        first_tier = seen["tier"]
        assert first_tier and MOVED_TIER not in first_tier, (
            f"precondition: the account starts on its default tier, read {first_tier!r}"
        )

        # (2) The nest stops; the app restarts onto the same store and cannot
        # sign in — the retry surface leads with the cached identity.
        stop_nest(nest, graceful=True)
        nest_down = True
        driver.hard_reload(replay_login=False)
        wait_until(
            lambda: _shows(driver, handle, first_tier),
            APP_RELAUNCH_S,
            diagnose=lambda: f"{IDENTITY} never painted the cached handle {handle!r} and "
            f"tier {first_tier!r} on a launch with the nest down: "
            + driver.diagnose(IDENTITY),
        )
        assert driver.is_visible(RETRY), (
            "the nest is down, so the launch must sit on launch_retry: "
            + driver.diagnose(RETRY)
        )

        # (3) The nest returns and the account moves to another tier. The cache's
        # only writer is the sign-in check, so the retry (a new check) must land it.
        start_nest_in_place(nest)
        nest_down = False
        ensure_tier(nest["port"], admin_signing_key=admin_sk, name=MOVED_TIER,
                    base_url=nest["url"])
        set_user_tier(nest["port"], actor, MOVED_TIER, admin_signing_key=admin_sk,
                      base_url=nest["url"])
        driver.click(RETRY)
        moved: dict = {}
        wait_until(
            lambda: _settings_cell(app, moved) and MOVED_TIER in moved["tier"],
            APP_RELAUNCH_S,
            diagnose=lambda: f"the retry's sign-in check never landed {MOVED_TIER!r} "
            f"(read {moved!r}): " + driver.diagnose("settings-tier-label"),
        )

        # (4) The nest stops again: the last good values stay.
        stop_nest(nest, graceful=True)
        nest_down = True
        driver.hard_reload(replay_login=False)
        wait_until(
            lambda: _shows(driver, handle, MOVED_TIER),
            APP_RELAUNCH_S,
            diagnose=lambda: f"{IDENTITY} lost the last good handle/tier "
            f"({handle!r}, {MOVED_TIER!r}) with the nest down again: "
            + driver.diagnose(IDENTITY),
        )
    finally:
        if nest_down:
            start_nest_in_place(nest)

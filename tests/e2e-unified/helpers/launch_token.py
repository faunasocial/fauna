"""The app-held session bearer, read and renewed through the cross-app agent seam.

``launch_token`` (state) and ``launch_refresh_token`` (command) name the bearer
the app actually holds and refreshes — ``fauna_e2e_agent::LAUNCH_TOKEN_KEY``
owns the contract and says whose bearer each app reports. The command is the
production refresh path forced now (clear the held bearer, re-mint over the
silent challenge), awaited so its ack lands after the outcome; the predecessor
id stays listed nest-side until it expires and stays in the app's own set
(``docs/goal/behavior/devices.md`` § The client's own session).
"""
from __future__ import annotations

from helpers import budgets
from helpers.waiting import wait_until


def launch_token(driver) -> dict:
    """The app's ``launch_token`` state — ``{"expires_in_secs", "own_session_ids"}``.

    JSON ``null`` is the key's documented "cannot answer yet" (no bearer held
    while a mint is in flight), so wait on the state itself (convention 14)
    before judging that the app publishes none.
    """
    try:
        wait_until(
            lambda: isinstance(driver.get_state("launch_token"), dict),
            budgets.RPC_ROUNDTRIP_S,
        )
    except AssertionError:
        pass
    token = driver.get_state("launch_token")
    assert isinstance(token, dict), (
        f"the app publishes no `launch_token` state — an app under test that does not "
        f"publish the key, not a schedule that failed. got {token!r}"
    )
    return token


def own_session_ids(driver) -> set[str]:
    """Every live session id this app minted for itself — the current one and
    the superseded-but-unexpired ones."""
    return set(launch_token(driver).get("own_session_ids") or [])


def renew_own_sign_in(driver) -> str:
    """Make the app renew its own sign-in NOW, through the production refresh
    path, and return the one new own id. The predecessor is NOT ended: it stays
    listed nest-side and in the app's own set — the precondition the Sessions
    page's own-row fold is about."""
    before = own_session_ids(driver)
    driver.call_command("launch_refresh_token", {}, timeout=60)
    after = own_session_ids(driver)
    gained = after - before
    assert before < after and len(gained) == 1, (
        f"the renewal did not add exactly one own session while keeping the rest: "
        f"before {sorted(before)!r}, after {sorted(after)!r}"
    )
    return gained.pop()

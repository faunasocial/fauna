"""Helpers that inject a `session` patch via `set_state`.

Single home for the values a `set_state({"session": {...}})` patch carries, so a
new helper doesn't have to rediscover them (or silently omit one — the
`login_as_nest_admin` `device_id` omission that made apple's mail machine a
silent no-op for two sessions started exactly that way).

Mirrors conftest's private `_E2E_LOGIN_DEVICE_ID` — helpers can't import conftest,
which is why the value lives in both places; keep them in step.
"""

from __future__ import annotations

import time

# A valid 64-char-hex (32-byte) device id for an injected e2e session. Real device
# ids ARE hex (linux `hex::encode(device_id())`), and the shared `MediaMachine`
# upload gesture hex-decodes `device_id`, so a non-hex placeholder fails
# `invalid device_id hex` on any client that reads it for an upload.
#
# Every `session` patch that sets `authenticated: True` should carry this (or
# another valid hex id): the macOS/iOS test agents build the authenticated
# `FaunaClient` from node_url + secret_hex + device_id, and a client-less
# authenticated shell renders fine while every nest-backed VM silently no-ops.
E2E_LOGIN_DEVICE_ID = "0123456789abcdef" * 4

# Generous, latency-independent budget (convention 14) for an actor switch to
# reach the LIVE session: the switch tears the authenticated shell down and
# rebuilds it (new window, new client, silent challenge, fresh feed manager).
# A green run pays only what it actually needs.
LIVE_ACTOR_SWITCH_WAIT_S = 45.0


def live_actor_id(app) -> str:
    """The actor id the app's **LIVE** session believes it is.

    Read from the Status sub-page's `account-actor-id` — the one sub-page every
    app renders it on (`test_settings.py::test_actor_id_visible` owns why), fed
    by the real authenticated session (linux: `DataMessage::AuthSuccess`).

    ⚠ **Deliberately NOT `get_state()`'s session block.** That block is the test
    agent's own `session_override` — the value the patch *asked* for — so it
    reports the incoming actor even when nothing switched, and an assertion
    built on it can never falsify the patch that set it
    (`docs/goal/architecture/e2e-conventions.md` convention 11 § *A
    HALF-APPLIED command is a dropped command*, which names this exact trap and
    the linux `set_state({session})` defect behind it). An assertion that cannot
    fail is not coverage.
    """
    app.settings._navigate_subpage("status")
    return app.settings.actor_id() or ""


def wait_live_actor_id(app, expected_hex: str, *, timeout: float | None = None) -> str:
    """Deadline-poll [`live_actor_id`] until it reports `expected_hex`; return
    what it last reported, so a caller's assertion can name the actor the app
    actually stayed as. Never raises — the caller owns the message."""
    deadline = time.monotonic() + (LIVE_ACTOR_SWITCH_WAIT_S if timeout is None else timeout)
    seen = ""
    while time.monotonic() < deadline:
        seen = live_actor_id(app)
        if expected_hex.lower() in seen.lower():
            return seen
        time.sleep(0.5)
    return seen


def resolve_node_url(app, nest, *, request=None, spa_url_fixture: str | None = None) -> str:
    """The address the client's WS-RPC layer connects to for a dedicated e2e
    nest: the per-test SPA proxy fixture for web (the browser needs a
    CORS-bearing origin the raw nest never sends — the blob-upload POST is
    otherwise blocked), the raw nest URL for every native app.

    `spa_url_fixture` is resolved lazily via `getfixturevalue` (never
    imported/requested eagerly) so a native-only run never instantiates the
    web-only proxy, which would build the SPA. Pass `spa_url_fixture=None`
    (the default) for a suite with no web SPA-proxy fixture of its own — the
    raw nest URL is then used unconditionally, `request` included."""
    if app.driver.is_web() and spa_url_fixture is not None:
        return request.getfixturevalue(spa_url_fixture)
    return nest["url"]


def login_as(
    app,
    nest,
    user,
    *,
    handle: str,
    device_id: str,
    request=None,
    spa_url_fixture: str | None = None,
    wait_for_id: str | None = None,
) -> None:
    """Drive the client into a logged-in session for `user` against `nest`,
    landing on the feed view — the same set_state login `logged_in_app` uses,
    re-pointed at a suite's own dedicated nest.

    Pass `request` + `spa_url_fixture` when the suite has a web leg backed by
    its own SPA proxy fixture (see `resolve_node_url`); omit both for a
    native-only suite. `wait_for_id`, if given, is awaited after the barrier
    below — a suite that immediately drives an element the login itself does
    not guarantee visible (e.g. a composer field) passes its id here instead
    of re-deriving its own wait.

    Barrier on the SWITCH ITSELF, not a settle-sleep: poll the **live** session
    ([`wait_live_actor_id`]) until it reports the incoming actor (testing.md
    convention 14). A sleep neither scales with load nor observes *who* is now
    logged in — the defect measured across all four of this helper's prior
    per-file copies (2026-08-10): with the session left pinned to the
    OUTGOING actor, a buyer/subscriber leg stayed green because the previous
    actor satisfies every assertion after it. The identity is the
    latency-independent state the switch is supposed to reach; a green run pays
    only the real switch cost.

    ⚠ **This barrier read `get_state("session").actor_id` until 2026-08-18, and
    that made it vacuous on linux** — the session block is the agent's own
    `session_override`, so it reported the incoming actor the instant the patch
    was applied whether or not anything switched, and the poll returned on its
    first iteration every time. It was therefore blind to precisely the defect
    its own paragraph above describes. `account-actor-id` is the surface a live
    session populates, and the one e2e-conventions.md convention 11 names for
    this question (see [`live_actor_id`]).

    Polled explicitly rather than via `get_state(wait_for=…)` because the web
    driver evaluates that predicate exactly once (`drivers/web.py::get_state`),
    which would silently degrade the barrier to a single read on one app.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    node_url = resolve_node_url(app, nest, request=request, spa_url_fixture=spa_url_fixture)
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": device_id,
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    expected_actor = user["actor_id_hex"]
    seen_actor = wait_live_actor_id(app, expected_actor)
    assert expected_actor.lower() in seen_actor.lower(), (
        f"the actor switch to {handle!r} never took effect: the app's LIVE session "
        f"reports account-actor-id={seen_actor!r}, expected {expected_actor!r}. Every "
        f"assertion after this point would have been made against the OUTGOING actor's "
        f"session, which is exactly how this leg passed while testing nothing."
    )
    # The barrier reads a Settings sub-page, so restore the landing this helper
    # promises. Cheap, and it keeps every caller's first action page-correct.
    app.driver.navigate_to("feed")
    if wait_for_id is not None:
        app.driver.wait_for(wait_for_id)

"""The URL half of the manager-singleton key: a SAME-ACTOR nest change inside
one page load must rebuild the feed manager, not keep the one bound to the
WS-RPC client `getClient()` has already retired.

⚠ READ THE `pytestmark` NOTE BELOW FIRST — this file is GREEN as of 2026-09-09,
and its green proves less than the name suggests. It was written as a
reproduction of the hypothesis described below and spent 2026-07-24 → 2026-09-09
as an `xfail(strict=True)`; the red is now best explained as a race in its own
synchronisation, not as the manager-lifecycle bug. Everything below is therefore
the hypothesis this file was WRITTEN to pin, not an established finding, and the
pass does not settle it. The note on `pytestmark` has the evidence and names the
residual question.

Sibling of `test_feed_manager_singleton_reset_web.py`, which pins the *actor*
half of the same class (sign-out → different identity; add-account → different
identity). Both doors there change the IDENTITY, and both are closed by
`resetFeedManager()` on `identity.logout()` / `identity.login()`.

This test pins the door neither call site covers: the actor never changes, so
neither `logout()` nor `login()` fires, but `nodeUrl()` does change — and
`rpc.ts`'s `getClient()` is keyed on `(actorId, nodeUrl())` (rpc.ts:437), so it
retires the old client (`retireClient` → `.close()`) out from under the cached
`managerPromise`, which `feed.ts` vends unconditionally with no key at all.

The production journey is walk-away-and-rejoin-elsewhere: the admin-nest page's
factory reset calls `accountsClearNestBinding()` then `goto('/app/onboarding')`
(admin/nest/+page.svelte:297) — a client-side nav, so module state survives —
and onboarding's `LoggedIn` exit writes the NEW per-actor nest URL and
`goto('/app/feed', {replaceState:true})` (onboarding/+page.svelte:1665,1701),
still without a reload. Land on Feed and `getFeedManager()` hands back a manager
wrapping the closed client for the nest the user just walked away from.

Topology: ONE nest behind TWO SPA proxies. Both proxy the same nest, so the
actor is registered on whichever URL is live and the nest stays a single
witness; the two proxies differ only in port, which is exactly what
`getClient()`'s key compares. (Two nests would also work but would make the
assertion about routing rather than about the retired client.)

The admin-shell hop in the middle is load-bearing, not decoration:
`getFeedManager()` alone never calls `getClient()` when the promise is cached, so
without an intervening RPC the old client is never retired and the stale manager
keeps working — the bug would not manifest and the test would pass unfixed. The
hop makes the admin dashboard's `onMount` issue a real `fauna.admin.stats` RPC
against the new URL, which is what retires the old client, exactly as the real
journey's post-re-claim traffic does. (The root layout's own `checkIsAdmin`
effect keys on `$identity`, not on the route, so a plain nav does NOT re-fire
it — hence the admin shell specifically.)

Why one nest was BELIEVED to suffice (see the xfail reason — this reasoning may
be exactly what is wrong), given both proxies terminate at the same place: the
retired client is permanently dead, not merely idle. `WsRpcClient::close`
(`libs/fauna-rpc-wasm/src/client.rs:211-225`) latches a one-way `closed` flag —
its own contract reads "One-way — a closed client never reconnects; build a new
one" — and the reconnect loop observes that flag and exits while `request` fails
fast as `NotConnected`. So an unfixed stale manager cannot quietly reconnect and
deliver anyway; the submit dies at the client. If that contract is ever relaxed,
this test would go green while the bug is live — redesign it onto two nests and
assert WHICH nest received the post.

Either way, confirm this RED against unfixed `feed.ts` before trusting it: a
manager-lifecycle test that has only ever been observed green proves nothing
about the lifecycle.
"""
from __future__ import annotations

import json

from helpers import web_store
import secrets
import time

import pytest

from actions import ActionLayer
from common.auth import register_handled_actor
from conftest import MAIL_PRIMARY_DOMAIN, _serve_spa_proxy
from drivers import create_driver
from tests.api import ws_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
]

# ⚠ THE `xfail(strict=True)` WAS REMOVED 2026-09-09, DELIBERATELY — read this
# before trusting this file's green, because the green proves LESS than the
# test's name suggests.
#
# The marker's own instruction was "if this ever passes, the marker must be
# removed deliberately rather than silently rotting". It started passing when
# the admin shell stopped redirecting a non-admin out of its own content
# (`admin.md` § Entry-level, never content-level): this test's synchronisation
# signal WAS that redirect, and the replacement — the admin dashboard's own
# Admin-gated `adminStats` rejection on `error-message` — resolves strictly
# LATER in the client-rebuild sequence than the bounce did.
#
# That is the whole change, and it is why the old red is best explained as a
# RACE rather than as the manager-lifecycle bug: the bounce fired as soon as one
# RPC resolved, so the test could reach the compose while the re-vended client
# was still coming up, and "Failed to post: not connected" plus `initial_count=0`
# is what losing that race looks like. Convention 14 forbids exactly this shape
# ("assert latency-independent state, never wall-clock timing"), so the old
# signal was a convention-14 defect and the new one is the fix. It also explains
# the anomaly the old marker recorded and could not place: keying
# `getFeedManager` on `(actorId, nodeUrl())` was implemented and REVERTED
# because the test stayed red — a correct fix would not move a red that was
# never about the manager.
#
# WHAT THE GREEN DOES NOT PROVE — do not let this file's pass be read as
# evidence that `getFeedManager`'s lifecycle is sound. It is vended with NO key
# at all (`feed.ts`) while `rpc.ts::getClient` keys on `(actorId, nodeUrl())`,
# so a manager outliving a retired client remains structurally possible; this
# test simply no longer demonstrates it either way. That residual question is
# tracked separately.
#
# Vacuity was checked before the marker came off, because a test that passes for
# a fake reason is worse than a red: (a) the wait cannot be satisfied by a stale
# banner — `MessageBanner` starts from its own empty prop, never seeds from
# `window.__fauna_messages`, and unmounts with its page on nav, so only the
# dashboard's own rejection can raise `error-message`; (b) `nodeUrl()` really
# does change — `tabNestUrl()` is null here (this flow seeds only the registry
# identity and never pins a tab, so `accounts.ts`'s `setTabNestUrl` branch never
# runs) and the sessionStorage dial override is never installed by the harness,
# only cleared, so `storedNestUrl()` falls through to the active account's
# `nest_url` the test rewrites; (c) green 3/3 on repeat runs.


def _text_or_absent(driver, element_id: str) -> str:
    """`get_text` that degrades to a marker instead of raising — only ever called
    from a failure path, where a second exception would replace the real
    diagnosis with a useless one. Idiom:
    `test_feed_manager_singleton_reset_web.py::_text_or_absent`."""
    try:
        return driver.get_text(element_id)
    except Exception:
        return "<absent>"


def _seed_registry_identity(driver, *, secret_hex: str, node_url: str) -> str:
    """Write one signed-in identity in the registry shape `identity.init()`
    reads (`helpers/web_store.seed_identity`); returns its actor id."""
    return web_store.seed_identity(driver, secret_hex, nest_url=node_url)


def _wait_connected(actions, timeout: float = 90.0) -> None:
    """Wait until the global `connection-status` indicator reads Connected.
    Idiom: `test_feed_manager_singleton_reset_web.py::_wait_connected`."""
    driver = actions.driver
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        last = _text_or_absent(driver, "connection-status")
        if last.startswith("Connected"):
            return
        time.sleep(1.0)  # sleep-ok: pacing between poll iterations of a bounded deadline loop
    console = "\n".join(driver.console_log()[-30:])
    raise AssertionError(
        f"connection never reached Connected within {timeout:.0f}s (last: {last!r}). "
        f"error: {actions.error_text()!r}. last console lines:\n{console}"
    )


def _wait_post_on_nest(actions, *, port: int, reader: dict, body: str, timeout: float = 45.0):
    """Poll the nest until a post with `body` lands. The nest is the witness —
    client-side state reads the same store on both sides of the fix and would
    prove nothing."""
    deadline = time.monotonic() + timeout
    seen: list = []
    while time.monotonic() < deadline:
        posts = ws_api.local_feed_posts(port, reader)
        for p in posts:
            if p.get("body") == body:
                return p
        seen = [p.get("body") for p in posts]
        time.sleep(1.0)  # sleep-ok: pacing between nest polls of a bounded deadline loop
    raise AssertionError(
        f"the post composed after the nest switch never reached the nest within "
        f"{timeout}s. Looked for {body!r}; the nest's local feed holds {seen!r}. "
        f"compose-error: {_text_or_absent(actions.driver, 'compose-error')!r}; "
        f"error: {actions.error_text()!r}.\n"
        "This is the unfixed shape: `feed.ts`'s `getFeedManager()` caches "
        "`managerPromise` with NO key, so after the nest URL changed it kept "
        "vending the manager built on the client `rpc.ts`'s `getClient()` then "
        "retired and CLOSED (`retireClient`) — every submit through it fails. "
        "`getFeedManager()` must re-vend on `(actorId, nodeUrl())` change, the "
        "same key `getClient()` itself uses (rpc.ts:437)."
    )


def test_web_same_actor_nest_switch_rebuilds_feed_manager(
    handled_nest, handled_spa_url, static_dir
):
    """A stays the same actor throughout; only the nest URL changes, with no
    page reload. The second post must reach the nest — proving the feed manager
    was rebuilt against the live client rather than reused from the retired one.

    Red before the `(actorId, nodeUrl())` key lands on `getFeedManager()`: the
    second compose submits through a manager whose WS-RPC client was closed, so
    the post never arrives (see `_wait_post_on_nest`'s failure text).
    """
    # `handled_nest`, not the shared `nest_instance`: `register_handled_actor`
    # needs an OPEN registration posture AND a `handle_domain`
    # matching `domain` (the registration signature is over
    # `actor_id || handle || domain` and the nest recomputes `domain` from its
    # own config), and the shared nest is started handle-less.
    port = handled_nest["port"]
    spa = handled_spa_url
    actor = register_handled_actor(
        port, handle="nestswitch" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    secret = bytes(actor["signing_key"]).hex()

    before_body = "nest-switch before " + secrets.token_hex(4)
    after_body = "nest-switch after " + secrets.token_hex(4)

    # The SECOND proxy onto the SAME nest — a genuinely different `nodeUrl()`
    # string (different port) for an actor that stays registered either way.
    alt_url, alt_server = _serve_spa_proxy(static_dir, handled_nest["url"])

    driver = create_driver("web")
    driver.launch({"url": spa + "/app/"})
    try:
        actions = ActionLayer(driver)

        # ── A signs in and BUILDS the FeedManager singleton. Visiting Feed is the
        #    only thing that calls `getFeedManager()` (feed/+page.svelte's
        #    onMount), and composing forces the full async build. ──
        _seed_registry_identity(driver, secret_hex=secret, node_url=spa)
        driver.hard_reload()
        driver.wait_for("feed-tab", timeout=30)
        driver.click("feed-tab")
        _wait_connected(actions)
        actions.feed.create_post(before_body)
        _wait_post_on_nest(actions, port=port, reader=actor, body=before_body)

        # ── The nest changes under the SAME actor, with NO reload — the shape the
        #    factory-reset walk-away + re-onboard journey leaves behind. ──
        web_store.seed(driver, {f"fauna/{actor['actor_id_hex']}/nest_url": alt_url})

        # ── Hop through the admin shell so a real RPC goes out against the NEW
        #    URL: `/app/admin` is the dashboard index route, and
        #    `admin/+page.svelte`'s `onMount` awaits `adminStats(secretHex)` →
        #    `rpc.ts::call()` → `getClient()`, which is what re-vends and retires
        #    (closes) the client the cached manager is bound to. Without an
        #    intervening RPC the old client is never retired and the bug cannot
        #    show. This mirrors the real journey, where the re-claim's
        #    `completeRegistration` updates the identity store and the root
        #    layout's `checkIsAdmin` effect re-fires against the new nest. ──
        #    The observable that the probe RESOLVED is the dashboard's own
        #    `error-message`: this actor is a non-admin (`handled_nest` claims
        #    admin for its own actor and `register_handled_actor` registers a
        #    plain one over open registration), `fauna.admin.stats` is
        #    `require_admin` nest-side, and the dashboard renders that rejection
        #    into its MessageBanner — so the label can only appear after the
        #    round-trip has come back.
        #    (Until 2026-09-09 the probe was `admin/+layout.svelte`'s own
        #    `checkIsAdmin`, and the observable was the bounce back to Settings
        #    it did on a negative. That content-level gate was web's lone
        #    deviation from the other six apps' nav-entry-only shape and is
        #    gone — admin.md § Navigation model. The dashboard's own Admin-gated read is the honest
        #    replacement: same `getClient()` path, same non-admin actor.)
        driver.set_state({"nav": {"stack": [{"view": "admin"}]}})
        driver.wait_for("error-message", timeout=30)

        # ── Back to Feed: `getFeedManager()` runs again. Unfixed it returns the
        #    cached promise (manager on the closed client); fixed it rebuilds. ──
        driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        driver.wait_for("compose-text-field", timeout=30)
        _wait_connected(actions)

        actions.feed.create_post(after_body)
        post = _wait_post_on_nest(actions, port=port, reader=actor, body=after_body)
        assert post["author"] == actor["actor_id_hex"], (
            f"the nest attributed the post-switch compose to {post['author']!r}, "
            f"but A is {actor['actor_id_hex']!r} — the rebuilt manager must keep "
            "signing with the same actor's secret across a nest change."
        )
    finally:
        driver.teardown()
        alt_server.shutdown()

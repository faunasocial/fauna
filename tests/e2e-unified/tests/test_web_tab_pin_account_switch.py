"""The web e2e agent's `set_state` account-switch path must drop THIS TAB's
account pin — a `sessionStorage` pair the harness's other teardown paths never
had to think about, because it does not live under the `localStorage`
`fauna/` namespace the registry sweep walks.

── Why this is a gap only the switch path has ─────────────────────────────

`tabPin.ts`'s pin (`fauna_tab_account`) + nest-url sibling
(`fauna_tab_nest_url`, `account-scoping.md` § Concurrent instances → *Web*)
are per-tab `sessionStorage`, written only by a real page load's
`accountsBoot()` (`accounts.ts`). Both the harness's `reset` and `logout`
actions already reach the pin's own erase: `stores.identity.logout()` chains
into `accountsClearAll()` → `clearTabPin()` (`accounts.ts:297`), and even
where `reset`'s hard `location.href` navigation races that async chain, the
next page load self-heals it (`store.ts`'s `init()`: an unresolvable pin is
dropped and the tab reloads once more, `store.ts:206-241`).

The **account-switch branch** (`agent.js`'s `applyPatch({session})`, taken
when a patch's `secret_hex` differs from what's stored) is the one door that
does NOT go through `identity.logout()` — it needs WASM, and this patch is
applied with no reload, which is the whole point of a `set_state` switch. So
a tab pinned to the OUTGOING actor by an earlier real load stays pinned to it
indefinitely: `accountsSessionMaterial()` then fails closed for every read
resolving through the pin, and `accounts.ts`'s `resolved === active` guard
skips the pin-mismatch re-resolve on the next boot.

Red before the fix (`agent.js`'s switch branch calling
`window.__fauna_clearTabPinForTest?.()`, `e2e-automation.ts`'s thin wrapper
around `clearTabPin`): the pin (and its nest-url sibling) survive the switch,
still naming actor A after B's session patch lands.

"""
from __future__ import annotations

import secrets
import time

import pytest

from common.auth import register_handled_actor
from conftest import MAIL_PRIMARY_DOMAIN
from drivers import create_driver
from helpers import web_store

pytestmark = [pytest.mark.tier_3, pytest.mark.web]


def _wait_tab_pin(driver, *, timeout: float = 30.0) -> str | None:
    """Poll `sessionStorage`'s tab-pin slot until it stops changing from its
    pre-boot `None`, or the deadline passes. `accountsBoot()`'s pin write sits
    behind an async `ensureWasm()`, while `feed-tab` first renders off the
    EARLIER synchronous `set(id)` boot step (`store.ts::init`) — so a bare
    read right after `wait_for('feed-tab')` can race the write and see
    `None` even on an unpinned-but-about-to-pin tab. Returns whatever the slot
    holds once it has gone non-`None` (or the last read, on timeout — the
    caller's own assertion names the failure). Idiom: `_wait_connected` /
    `_wait_post_on_nest` in the nest-switch sibling test."""
    deadline = time.monotonic() + timeout
    seen = None
    while time.monotonic() < deadline:
        seen = driver.eval_js("sessionStorage.getItem('fauna_tab_account')")
        if seen is not None:
            return seen
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations of a bounded deadline loop
    return seen


def test_web_set_state_switch_clears_tab_pin(handled_nest, handled_spa_url):
    """A real page load pins the tab to A; a `set_state` switch to B (no
    reload) must drop that pin and its nest-url sibling, not leave them
    naming A.
    """
    port = handled_nest["port"]
    spa = handled_spa_url

    actor_a = register_handled_actor(
        port, handle="tabpin-a" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    secret_a = bytes(actor_a["signing_key"]).hex()
    actor_b = register_handled_actor(
        port, handle="tabpin-b" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    secret_b = bytes(actor_b["signing_key"]).hex()

    driver = create_driver("web")
    driver.launch({"url": spa + "/app/"})
    try:
        # ── Pin the tab to A via TWO real page loads. The first seeds the
        #    registry identity and reloads: `accounts.ts`'s UNPINNED branch resolves
        #    it and pins the tab, but does not write the nest-url sibling —
        #    only the PINNED branch does that (it re-derives the pinned
        #    account's own nest url on every boot). So a second reload, now
        #    already pinned, is what actually populates both halves of the
        #    pin this test exercises. ──
        web_store.seed_identity(driver, secret_a, nest_url=spa)
        driver.hard_reload()
        driver.wait_for("feed-tab", timeout=30)
        pin = _wait_tab_pin(driver)
        assert pin == actor_a["actor_id_hex"].lower(), (
            f"precondition failed: the tab never pinned to A after its first "
            f"real load (got {pin!r}) — this test's setup, not the switch "
            f"path, is broken."
        )

        # A second real load, now already pinned: only the PINNED branch of
        # `accountsBoot()` writes the nest-url sibling (it re-derives the
        # pinned account's own nest url on every boot) — the first,
        # unpinned-becoming-pinned load never touches it.
        driver.hard_reload()
        driver.wait_for("feed-tab", timeout=30)
        pin = _wait_tab_pin(driver)
        assert pin == actor_a["actor_id_hex"].lower(), (
            f"precondition failed: the tab lost its pin to A across a second "
            f"real load (got {pin!r}) — this test's setup, not the switch "
            f"path, is broken."
        )
        nest_pin = driver.eval_js("sessionStorage.getItem('fauna_tab_nest_url')")
        assert nest_pin, (
            f"precondition failed: the pinned tab never recorded a nest url "
            f"(got {nest_pin!r}) — this test's setup, not the switch path, "
            f"is broken."
        )

        # ── The switch under test: a `set_state` session patch to B, with NO
        #    reload — `agent.js`'s account-switch branch, the one door
        #    `identity.logout()` never runs down. ──
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": spa,
                "handle": actor_b.get("handle") or "tabpin-b",
                "secret_hex": secret_b,
                "actor_id": actor_b["actor_id_hex"],
                "device_id": "test-device-tabpin-b",
            },
        })

        stale_pin = driver.eval_js("sessionStorage.getItem('fauna_tab_account')")
        stale_nest_pin = driver.eval_js("sessionStorage.getItem('fauna_tab_nest_url')")
        assert stale_pin is None and stale_nest_pin is None, (
            f"the tab pin survived a set_state actor switch: "
            f"fauna_tab_account={stale_pin!r}, fauna_tab_nest_url="
            f"{stale_nest_pin!r} (A was {actor_a['actor_id_hex']!r}, the "
            f"switch was to B={actor_b['actor_id_hex']!r}). A stale pin makes "
            f"`accountsSessionMaterial()` fail closed for every read that "
            f"resolves through it and makes `accounts.ts`'s boot skip the "
            f"boot resolution (`resolved === active` guard) — `agent.js`'s "
            f"account-switch branch must clear the pin through the SPA's own "
            f"synchronous erase (`window.__fauna_clearTabPinForTest`, "
            f"`$lib/tabPin.ts`'s `clearTabPin`), not leave it for the "
            f"registry's `localStorage`-only sweep to miss."
        )
    finally:
        driver.teardown()

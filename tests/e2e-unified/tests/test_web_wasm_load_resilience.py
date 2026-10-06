"""`ensureWasm()` (the web MAIN wasm chunk loader, `apps/fauna-web/src/lib/wasm.ts`)
must retry a transient main-chunk fetch abort, the same as its two siblings
`ensureLaunchWasm` / `ensureOnboardingWasm`.

Before the fix, `ensureWasm()` memoized the init *promise* but never reset it on
failure: a single aborted `fauna_wasm_bg.wasm` fetch (the incidental abort class
seen when `reset()`'s `location.href` reload cancels an in-flight fetch) poisoned
`wasmInit` for the whole JS context — every LATER caller (the onboarding page's
own boot `launch sequence` calls `ensureWasm()` at `routes/onboarding/+page.svelte:549`,
among many other call sites) inherited the same rejected promise and stayed
*permanently* wedged, even though nothing about a later attempt would itself fail.

This test proves the retry mechanically: a dedicated SPA proxy aborts the FIRST
`fauna_wasm_bg.wasm` response (a truncated body under a lying Content-Length —
a real network abort the browser detects mid-transfer, not an HTTP error status)
and serves the file normally on every later request. A fresh, unauthenticated
`/app/` launch's OWN boot sequence is the first (and only reliably-first) caller
of `ensureWasm()` — racing a second caller of our own would be timing-dependent,
so instead: wait for that natural boot call to hit the abort and fail (observable
on the console), THEN invoke the (wasm-independent-to-reach, `ensureWasm()`-
calling) `__fauna_enableDnsFakeProviderForTest` test hook once and assert it
succeeds — proving a LATER, independent `ensureWasm()` call retries fresh
instead of inheriting the cached rejection.
"""
from __future__ import annotations

import time

import pytest

from conftest import _serve_spa_proxy
from drivers import create_driver

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

_HOOK_JS = """
(async () => {
  try {
    await window.__fauna_enableDnsFakeProviderForTest();
    return 'ok';
  } catch (e) {
    return 'error:' + (e && e.message ? e.message : String(e));
  }
})()
"""


def _wait_for_hydration(driver, timeout: float = 15.0) -> None:
    """The layout's top-level script (where `__fauna_enableDnsFakeProviderForTest`
    is assigned, alongside `__fauna_stores`) runs during hydration — mirrors
    `WebBridgeDriver._ensure_agent`'s own hydration wait, needed here because
    this test drives a dedicated `create_driver("web")` + `driver.launch(...)`
    directly (bypassing `app`/`reset()`, which normally waits for this)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if driver.eval_js("typeof window.__fauna_enableDnsFakeProviderForTest === 'function'"):
            return
        time.sleep(0.3)
    raise AssertionError("SPA did not hydrate within the timeout — __fauna_enableDnsFakeProviderForTest never appeared")


def _wait_for_natural_boot_abort(driver, timeout: float = 15.0) -> None:
    """The onboarding page's own boot `launch sequence` calls `ensureWasm()`
    unconditionally (`routes/onboarding/+page.svelte:549`) — this IS the first
    caller the abort-once proxy catches, faster and more reliably than racing
    it with a caller of our own. Poll the bridge-captured console until that
    natural attempt has visibly failed against our abort, so the assertion
    below tests a definitely-LATER, independent call."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if any(
            "ERR_CONTENT_LENGTH_MISMATCH" in line or "launch sequence failed" in line
            for line in driver.console_log()
        ):
            return
        time.sleep(0.3)
    raise AssertionError(
        f"the onboarding page's natural boot never hit the wasm abort within "
        f"{timeout}s; console={driver.console_log()}"
    )


def test_ensure_wasm_retries_a_transient_main_chunk_abort(static_dir, nest_instance):
    url, server = _serve_spa_proxy(
        static_dir, nest_instance["url"], abort_once_paths={"/fauna_wasm_bg.wasm"},
    )
    try:
        driver = create_driver("web")
        driver.launch({"url": url + "/app/"})
        try:
            _wait_for_hydration(driver)
            _wait_for_natural_boot_abort(driver)

            retried = driver.eval_js(_HOOK_JS)
            assert retried == "ok", (
                "a LATER, independent ensureWasm() call must succeed: the "
                "onboarding boot's own attempt already hit (and consumed) the "
                "proxy's one-time abort, so this call's fetch would succeed IF "
                "it actually retried — pre-fix, wasmInit stays a cached "
                "rejected promise and this call never re-fetches at all, got "
                f"{retried!r}; console={driver.console_log()}"
            )
        finally:
            driver.teardown()
    finally:
        server.shutdown()

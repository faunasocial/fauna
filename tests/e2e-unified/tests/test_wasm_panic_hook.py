"""Wasm panic-hook fan-out: every wasm chunk names
itself in the browser console when it panics — a headless witness, not a
review-only claim.

**Why this needs a real browser, per chunk.** Each `libs/fauna-wasm*` crate
compiles to its OWN `.wasm` binary with its own Rust runtime
(`fauna-wasm-panic-hook`'s doc comment) — a hook installed in one chunk has
zero effect on any other. Without it, a panic aborts mid-poll with a bare
`RuntimeError: unreachable` (no message, no panic site, no chunk name) and
any `future_to_promise` task holding it never settles (testing.md § point 6:
failures must diagnose themselves). A prior pass fixed this for `libs/fauna-wasm`
(the core chunk) and proved it live against the send-hang bug it was
diagnosing; this fans the same `#[wasm_bindgen(start)]` + shared
`fauna_wasm_panic_hook::install(chunk)` call out to every other chunk. This
test is the automated proof that the fan-out actually works — the mechanism
is identical across all of them, but "compiles" is not "the hook fires and
names the right chunk", so each one gets a real panic driven through a real
browser.

**Build note.** `fauna-wasm-onboarding` reuses its EXISTING `test-helpers`
flavor (`just wasm-onboarding-test`, already in `web-test`'s pipeline — no
extra build). The other six have no such flavor, so `just wasm-panic-witness`
builds each under a dedicated `_panic_witness` stem the real SPA never
imports (no flavor-collision risk — see the justfile recipe's doc comment).
Both are prerequisites of `tests/e2e-unified/conftest.py`'s `static_dir`
fixture set — see its `wasm-panic-witness` dependency.

`fauna-wasm` itself (the core chunk) is intentionally NOT re-proven here —
a prior pass already did, live, against a real bug. `fauna-wasm-content-index` is
excluded: it is ON HOLD (see its own crate doc comment), not part of the
`wasm`/`web-test` pipeline, so nothing ever builds it into `static/` for a
browser to import.
"""

from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

# crate name -> the wasm-bindgen JS glue's stem already present in
# apps/fauna-web/static/ (see the module docstring's Build note).
WITNESS_CHUNKS = {
    "fauna-wasm-onboarding": "fauna_wasm_onboarding",
    "fauna-wasm-launch": "fauna_wasm_launch_panic_witness",
    "fauna-wasm-media": "fauna_wasm_media_panic_witness",
    "fauna-wasm-folders": "fauna_wasm_folders_panic_witness",
    "fauna-wasm-labeler-catalog": "fauna_wasm_labeler_catalog_panic_witness",
    "fauna-wasm-share": "fauna_wasm_share_panic_witness",
    "fauna-wasm-connected-apps": "fauna_wasm_connected_apps_panic_witness",
    "fauna-wasm-atproto-settings": "fauna_wasm_atproto_settings_panic_witness",
    "fauna-wasm-backups": "fauna_wasm_backups_panic_witness",
}


@pytest.mark.parametrize("chunk_name,stem", sorted(WITNESS_CHUNKS.items()))
def test_wasm_chunk_panic_hook_names_itself(logged_in_app, chunk_name, stem):
    """Dynamically loading `stem` and calling its `panicForTestOnly()` export
    writes `fauna wasm panic [<chunk_name>]` to the browser console.

    The import is independent of whatever page `logged_in_app` happens to be
    on — a wasm chunk is a static asset, importable by absolute URL from any
    same-origin page — so this needs no navigation to the chunk's own SPA
    page. `#[wasm_bindgen(start)]` (source: each crate's `src/lib.rs`) runs
    automatically on module instantiation, installing the hook before
    `panicForTestOnly` is ever reachable — there is no separate "did the hook
    install" step to race.
    """
    driver = logged_in_app.driver
    # `svelte.config.js` sets `paths.base = '/app'` — every built static asset
    # (including these wasm bundles) is served under that prefix, not the
    # site root (mirrors `$lib/wasm-media.ts`'s `${base}/fauna_wasm_media_bg.wasm`
    # pattern; hardcoded here since `base` is a compiled-in constant for the
    # whole SPA, not something a raw eval_js script can import).
    script = (
        "(async () => {"
        f"  const mod = await import('/app/{stem}.js');"
        f"  await mod.default('/app/{stem}_bg.wasm');"
        "  try { mod.panicForTestOnly(); } catch (e) { /* expected: the wasm trap */ }"
        "  return true;"
        "})()"
    )
    driver.eval_js(script)

    console = driver.console_log()
    needle = f"fauna wasm panic [{chunk_name}]"
    assert any(needle in line for line in console), (
        f"expected {needle!r} in the browser console after calling "
        f"{stem}.panicForTestOnly(), but it never appeared — the panic hook "
        f"either isn't installed for this chunk or doesn't name it. Last 20 "
        f"console lines: {console[-20:]!r}"
    )

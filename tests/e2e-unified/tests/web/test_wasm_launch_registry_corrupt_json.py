"""`fauna-wasm-launch`'s registry save accessors refuse corrupt JSON.

`registry_save_pending_invite` / `registry_save_awaiting_dns`
(`libs/fauna-wasm-launch/src/lib.rs`) document a defensive invariant: a
payload that doesn't parse as `PendingInviteRecord` / `AwaitingDnsRecord` is
refused (returns `false`, writes nothing) rather than stored — "a corrupt
slot would read back as absent anyway". This guard against corrupted/stale
`localStorage` state (cross-version drift, not normal TS-side input) had
**zero test coverage anywhere**: no crate-level test file, and the two
existing persistence regression files (`test_pending_invite_persistence.py`,
`test_awaiting_dns_persistence.py`) only exercise the well-formed-JSON path.

No UI journey reaches this branch — every TS call site `JSON.stringify`s a
typed object before calling `registrySave*`, so a real user session can never
hand it malformed JSON (the fixture-setup carve-out in
`docs/goal/architecture/e2e-conventions.md` point 8 applies). The crate's own
`test-helpers` + `panicForTestOnly` convention is e2e-level testing, not a
crate-level `wasm-bindgen-test` harness (no `[dev-dependencies]` exist), so
this reaches the free-standing wasm exports directly — the same "dynamically
import the built chunk from a real browser" pattern
`test_wasm_panic_hook.py` uses for `panicForTestOnly`.

Test file lives in tests/web/ so the cross-app conftest auto-deselects when
running non-web apps (mirrors test_pending_invite_persistence.py).
"""

import json

from helpers import web_store

import pytest

pytestmark = [pytest.mark.web, pytest.mark.tier_2]

VALID_SECRET_HEX = "11" * 32


def _load_registry_module(driver) -> None:
    """Dynamically import the same built chunk the SPA loads
    (`wasm-launch.ts::ensureLaunchWasm`) and stash it on
    `window.__faunaWasmLaunchTest`. A fresh dynamic import is a distinct wasm
    instance from whatever the SPA's own Vite-bundled import constructed
    (see test_wasm_panic_hook.py's module docstring on per-chunk isolation),
    but the registry* accessors read/write straight through to the shared
    browser `localStorage`, so a second instance observes the same state."""
    driver.eval_js(
        "(async () => {"
        "  const mod = await import('/app/fauna_wasm_launch.js');"
        "  await mod.default('/app/fauna_wasm_launch_bg.wasm');"
        "  window.__faunaWasmLaunchTest = mod;"
        "  return true;"
        "})()"
    )


def _seed_active_account(driver, secret_hex: str = VALID_SECRET_HEX) -> str:
    """One registered, active account in the registry shape — the only shape
    `AccountRegistry::active()` resolves (there is no single-slot bridge any
    more). Without an active account `registry_save_*` refuses for an
    *unrelated* reason (no actor to key the per-actor slot under), which would
    make a bare `refused is True` assertion pass for the wrong reason — this
    fixture rules that out. Returns the actor id; a test that then corrupts
    `fauna/index` overwrites the blob this wrote."""
    return web_store.seed_identity(driver, secret_hex)


def test_registry_save_pending_invite_refuses_corrupt_json(app):
    """Corrupt JSON is refused and nothing is written; a well-formed payload
    right after proves the refusal really was about the malformed JSON (not
    e.g. a missing active account)."""
    driver = app.driver
    _seed_active_account(driver)
    _load_registry_module(driver)

    refused = driver.eval_js(
        "window.__faunaWasmLaunchTest.registrySavePendingInvite('not json at all')"
    )
    assert refused is False, f"expected refusal for corrupt JSON, got {refused!r}"

    after_refusal = driver.eval_js(
        "window.__faunaWasmLaunchTest.registryLoadPendingInvite() ?? null"
    )
    assert after_refusal is None, (
        f"a refused corrupt payload must not be stored; read back {after_refusal!r}"
    )

    # separators matches serde_json::to_string's compact (no-space) output —
    # registryLoadPendingInvite() reads back exactly what Rust re-serialized,
    # not the original payload string, so the two must use the same style.
    valid_record = json.dumps({
        "nest_url": "https://nest.example.test",
        "handle": "alice@nest.example.test",
        "request_id": "req-corrupt-json-control-001",
        "status_json": "{}",
    }, separators=(",", ":"))
    accepted = driver.eval_js(
        "window.__faunaWasmLaunchTest.registrySavePendingInvite("
        f"{json.dumps(valid_record)})"
    )
    assert accepted is True, f"expected acceptance of well-formed JSON, got {accepted!r}"

    after_accept = driver.eval_js(
        "window.__faunaWasmLaunchTest.registryLoadPendingInvite()"
    )
    assert after_accept == valid_record, (
        f"well-formed write should read back verbatim; got {after_accept!r}"
    )


def test_registry_save_awaiting_dns_refuses_corrupt_json(app):
    """Same contract as above for `registrySaveAwaitingDns` /
    `registryLoadAwaitingDns` — a second, independently-guarded accessor over
    the same `serde_json::from_str` pattern."""
    driver = app.driver
    _seed_active_account(driver)
    _load_registry_module(driver)

    refused = driver.eval_js(
        "window.__faunaWasmLaunchTest.registrySaveAwaitingDns('{ this is: not, valid json }')"
    )
    assert refused is False, f"expected refusal for corrupt JSON, got {refused!r}"

    after_refusal = driver.eval_js(
        "window.__faunaWasmLaunchTest.registryLoadAwaitingDns() ?? null"
    )
    assert after_refusal is None, (
        f"a refused corrupt payload must not be stored; read back {after_refusal!r}"
    )

    # separators matches serde_json::to_string's compact (no-space) output —
    # see the pending-invite test's identical comment above.
    valid_record = json.dumps({
        "nest_url": "https://nest.example.test",
        "handle": "alice@nest.example.test",
        "dns_records_json": "[]",
        "claim_code": "claim-corrupt-json-control-001",
    }, separators=(",", ":"))
    accepted = driver.eval_js(
        "window.__faunaWasmLaunchTest.registrySaveAwaitingDns("
        f"{json.dumps(valid_record)})"
    )
    assert accepted is True, f"expected acceptance of well-formed JSON, got {accepted!r}"

    after_accept = driver.eval_js(
        "window.__faunaWasmLaunchTest.registryLoadAwaitingDns()"
    )
    assert after_accept == valid_record, (
        f"well-formed write should read back verbatim; got {after_accept!r}"
    )

"""Real e2e: a ``test@localhost`` handle drives the real browser onboarding all
the way to the admin-claim screen against a local nest.

Spins up a real *unclaimed* ``fauna-nest`` that ALSO serves the built web SPA at
``/app`` (``--static-dir``) — the production topology, where the SPA and the API
share one origin (so every request, incl. the anonymous WS-RPC connection, hits
the nest directly with no test proxy). The browser is driven through the
handle-first onboarding and asserts that typing a ``test@localhost:<port>``
handle:

  1. does NOT yield ``handle_check.outcome.tld_invalid`` (the reported bug), and
  2. resolves → skips DNS → probes the local nest (HTTP health/challenge +
     WS-RPC setup-status) → ``UnregisteredUnclaimedNest`` → Continue routes to
     the admin-claim (``claim_code``) screen, ready for the one-time code.

This is the only test that drives the real ``start_handle_check`` path through
the wasm bundle in a browser, so — unlike the Rust unit/integration tests — it
catches stale-wasm and web-glue regressions of the localhost handle-check fix.
(The ``static_dir`` fixture rebuilds the test-flavoured wasm via ``just
web-test``, which always re-runs wasm-pack, so the bundle reflects current Rust.)

Asserts the claim COMPLETES — reaches ``nat_mode_choice`` (the terminal
admin-path step every successful claim lands on directly, no-modes retirement
ratified 2026-07-12). The ``fauna.auth.claim_admin`` WS round-trip is the first
happy-path browser test for a WS-RPC pre-identity call that signs a timestamp,
so it guards against the ``now_ms()`` panic-on-wasm32 class the original
landing of S3b shipped (a ``std::time::SystemTime::now()`` aborting the
``spawn_local`` future silently).

See ``docs/goal/behavior/onboarding.md`` §2 "Local / loopback targets".
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from actions import ActionLayer
from common.nest import CLAIM_CODE, wait_for_node
from conftest import get_available_apps
from drivers import create_driver
from drivers.port_util import find_free_port

if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)

pytestmark = [pytest.mark.tier1, pytest.mark.web, pytest.mark.tier_3]


@pytest.fixture
def unclaimed_nest(nest_binary, static_dir, tmp_path_factory):
    """A fresh, never-claimed nest that also serves the SPA at /app/.

    `--static-dir <build>` makes the nest serve the web app from its own origin,
    so the browser hits one origin for both the SPA and the API (no proxy). The
    claim code is `CLAIM_CODE`, written to `<tmp>/claim-code`; nest output goes
    to `<tmp>/nest.log` for diagnostics.
    """
    tmp_dir = str(tmp_path_factory.mktemp("localhost-onboarding-nest"))
    port = find_free_port()
    db = os.path.join(tmp_dir, "nest.db")
    blob = os.path.join(tmp_dir, "blobs")
    os.makedirs(blob, exist_ok=True)
    cfg = os.path.join(tmp_dir, "config.toml")
    # `static_dir` must live in the config file: a `--config` file's `[nest]`
    # block wins over the `--static-dir` CLI arg, so the CLI arg alone is
    # silently ignored and the nest serves its info page instead of the SPA.
    with open(cfg, "w") as f:
        f.write(
            f'[nest]\nmode="public"\nlisten="127.0.0.1:{port}"\ndb_path="{db}"\n'
            f'blob_dir="{blob}"\nrequire_registration=false\nstatic_dir="{static_dir}"\n'
        )
    with open(os.path.join(tmp_dir, "claim-code"), "w") as f:
        f.write(CLAIM_CODE)
    log_path = os.path.join(tmp_dir, "nest.log")
    log_fh = open(log_path, "w")
    from drivers.port_util import popen_group_kwargs, reap_descendants_of

    proc = subprocess.Popen(
        [nest_binary, "--bind", f"127.0.0.1:{port}", "--db", db, "--config", cfg, "--blob-dir", blob],
        stdout=log_fh, stderr=subprocess.STDOUT,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — no-op off Windows.
    reap_descendants_of(proc.pid)
    wait_for_node(port)
    yield {"proc": proc, "port": port, "url": f"http://127.0.0.1:{port}", "log_path": log_path}
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except Exception:
        proc.kill()


@pytest.fixture
def local_web(unclaimed_nest):
    """A web driver loaded from the nest's own origin via `localhost`, so a
    `test@localhost:<port>` handle resolves to that same origin."""
    port = unclaimed_nest["port"]
    driver = create_driver("web")
    driver.launch({"url": f"http://localhost:{port}/app/"})
    yield driver, port
    driver.teardown()


@pytest.mark.feature("claim-a-fresh-nest")
def test_test_at_localhost_reaches_admin_claim(local_web):
    driver, port = local_web
    app = ActionLayer(driver)
    handle = f"test@localhost:{port}"

    # ── Onboarding: create identity, reach handle entry ──────────────────
    driver._post("/navigate", {"url": f"http://localhost:{port}/app/onboarding"})
    time.sleep(2)
    driver._ensure_agent()
    # Pillar C (uniform https): the typed `test@localhost:{port}` handle now
    # resolves to `https://localhost:{port}`, but this test nest serves plain
    # HTTP. Point the onboarding probe (health + anonymous WS-RPC handle-check)
    # at the http nest via the nest override — the same-box `nest_url` injection
    # seam (in production the desktop app injects its local nest's real
    # scheme+port). `state.nest_url` still records the resolved https URL. Set
    # before any identity exists (set_provider_base_urls re-mounts the page).
    driver.set_provider_base_urls({"nest": f"http://localhost:{port}"})
    driver.wait_for("create-identity-button", timeout=20)
    driver.click("create-identity-button")
    driver.wait_for("identity-continue-button", timeout=15)
    driver.click("identity-continue-button")

    # ── Handle check for test@localhost ──────────────────────────────────
    driver.wait_for("handle-input", timeout=15)
    driver.clear_and_type("handle-input", handle)
    driver.click("handle-check-button")

    # The bug: `localhost` was rejected as `tld_invalid`. Now the check must
    # probe the local nest and enable Continue (UnregisteredUnclaimedNest on a
    # fresh nest). Fail fast and loud if the old message reappears.
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if app.is_enabled("handle-entry-continue-button"):
            break
        msg = driver.get_text("handle-message-area") if driver.is_visible("handle-message-area") else ""
        assert "tld" not in msg.lower(), f"localhost rejected as invalid TLD: {msg!r}"
        time.sleep(0.5)
    else:
        msg = driver.get_text("handle-message-area") if driver.is_visible("handle-message-area") else "(no message)"
        pytest.fail(f"Continue never enabled for {handle!r}; handle-message-area: {msg!r}")

    assert "tld" not in driver.get_text("handle-message-area").lower()

    # ── Continue routes the test@localhost handle to the admin-claim screen ──
    # Reaching `claim_code` proves the whole probe chain succeeded for a
    # `test@localhost` handle: resolve → skip-DNS → HTTP health/challenge →
    # WS-RPC setup-status(claimed:false) → UnregisteredUnclaimedNest → Continue.
    # That is the full exercise of the localhost handle-check fix in a real
    # browser, end to end.
    driver.click("handle-entry-continue-button")
    driver.wait_for("claim-code-input", timeout=15)
    assert driver.is_visible("claim-code-submit-button"), (
        "test@localhost Continue should reach the admin-claim screen with the "
        "submit button rendered: "
        f"{driver.diagnose('claim-code-submit-button')} error={app.error_text()!r}"
    )
    driver.clear_and_type("claim-code-input", CLAIM_CODE)
    assert app.is_enabled("claim-code-submit-button"), "admin-claim screen not ready for test@localhost"

    # ── Submit the claim — proves WS-RPC `fauna.auth.claim_admin` completes ──
    # The claim rides the same anonymous WS-RPC connector as setup-status. Until
    # the `now_ms()`-panics-on-wasm32 fix (`std::time::SystemTime::now()` aborts
    # the spawn_local future silently on wasm32-unknown-unknown), this call
    # hung forever in the browser. Reaching `nat_mode_choice` proves the
    # WS round-trip + token + downstream `fauna.setup.status` recheck all
    # completed over the anonymous WS — the full happy-path that the
    # `onboarding_ws_rpc_roundtrip.rs` failure-only coverage missed.
    driver.click("claim-code-submit-button")
    driver.wait_for("nat-mode-confirm-button", timeout=30)
    assert driver.is_visible("nat-mode-confirm-button"), (
        "claim_admin did not reach nat_mode_choice — the WS-RPC happy-path "
        "is broken (was: now_ms() panic on wasm32)."
    )

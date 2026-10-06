"""tier_3 e2e: the onboarding handle-check resets when the identity is
re-imported.

Regression for the fix (`fix(rust): reset onboarding handle-check when
identity is re-imported/created`). In the handle-entry step the rich
`HandleCheckSnapshot` (plus the probe-derived `nest_url` / `nest_mode` /
`domain_status`) survived the user going **Back** and importing a *different*
identity, so the page kept showing the previous identity's conclusion (e.g.
"you're already on this nest as <other handle>") until the user clicked Check
again. The fix makes `confirm_imported_identity` / `confirm_generated_identity`
call `reset_handle_check` as they advance to `HandleEntry`.

The machine-level red-green guard lives in
`libs/fauna-onboarding-machine/tests/handle_check_local_nest.rs`
(`reimporting_identity_resets_stale_handle_check`). This is the full-stack
proof that the **web app UI** actually resets between two real identities
against a **real nest** — the case the user asked for: "juggling two separate
ids, one of which is already registered".

Topology mirrors `test_onboarding_localhost.py`: a real `fauna-nest` that also
serves the built SPA at `/app` (`--static-dir`), so the browser hits one origin
for both the SPA and the API (anonymous WS-RPC included, no proxy). The
difference: this nest is **already claimed by identity A** (handle `test`), so a
`test@localhost:<port>` handle-check with A's secret probes as `AlreadyOnNest`
— the "registered/already on nest" verdict that must NOT bleed into a freshly
imported identity B.

The reset is read the reliable way the target-state doc prescribes — the
`handle_check_snapshot()` machine state over the cross-app bridge
(`call_machine_method`), plus the snapshot-bound Continue button — not a brittle
message-area scrape.

See `docs/goal/behavior/onboarding.md` § Identity stage ("Both
`confirm_*_identity` transitions reset the handle-check result…") and
§ Handle-entry stage (the `HandleCheckOutcome` variants).
"""

from __future__ import annotations

import json
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

from common.auth import claim_admin, make_keypair
from common.nest import CLAIM_CODE, wait_for_node
from conftest import get_available_apps
from drivers import create_driver
from drivers.port_util import find_free_port

if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)

pytestmark = [pytest.mark.tier2, pytest.mark.web, pytest.mark.tier_3]


@pytest.fixture
def claimed_nest_with_a(nest_binary, static_dir, tmp_path_factory):
    """A real nest serving the SPA at /app/, **already claimed by identity A**.

    Identity A is the admin (handle `test`, a registered actor), so a browser
    handle-check of `test@localhost:<port>` with A's secret probes as
    `AlreadyOnNest`. A storage mode is committed so the nest is fully set up.
    Identity B is a fresh, never-registered keypair the test imports second.

    `--static-dir` must live in the config file: a `--config` file's `[nest]`
    block wins over the `--static-dir` CLI arg, so the CLI arg alone is silently
    ignored (the nest would serve its info page instead of the SPA).
    """
    from nacl.signing import SigningKey

    tmp_dir = str(tmp_path_factory.mktemp("handle-check-reset-nest"))
    port = find_free_port()
    db = os.path.join(tmp_dir, "nest.db")
    blob = os.path.join(tmp_dir, "blobs")
    os.makedirs(blob, exist_ok=True)
    cfg = os.path.join(tmp_dir, "config.toml")
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
        [nest_binary, "--bind", f"127.0.0.1:{port}", "--db", db,
         "--config", cfg, "--blob-dir", blob],
        stdout=log_fh, stderr=subprocess.STDOUT,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — no-op off Windows.
    reap_descendants_of(proc.pid)
    wait_for_node(port)

    # Identity A claims the nest as admin (handle `test`) → A is a registered
    # actor. The nest is content-ready from first boot (no-modes retirement,
    # ratified 2026-07-12).
    _actor_a_hex, secret_a = make_keypair()
    sk_a = SigningKey(bytes.fromhex(secret_a))
    claim_admin(port, CLAIM_CODE, handle="test", signing_key=sk_a)

    # Identity B: fresh, never registered on this (or any) nest.
    _actor_b_hex, secret_b = make_keypair()

    yield {"port": port, "secret_a": secret_a, "secret_b": secret_b,
           "log_path": log_path}

    proc.terminate()
    try:
        proc.wait(timeout=10)
    except Exception:
        proc.kill()


@pytest.fixture
def local_web(claimed_nest_with_a):
    """A web driver loaded from the nest's own origin via `localhost`, so a
    `test@localhost:<port>` handle resolves to that same origin."""
    driver = create_driver("web")
    driver.launch({"url": f"http://localhost:{claimed_nest_with_a['port']}/app/"})
    yield driver, claimed_nest_with_a
    driver.teardown()


# ── Helpers ──────────────────────────────────────────────────────────────────

def _import_identity_ui(driver, secret_hex: str) -> None:
    """Import a 64-hex secret via the identity-import UI, from either
    identity_choice (first import) or identity_import (after Back). Lands on
    handle_entry."""
    if not driver.is_visible("paste-secret-field"):
        driver.wait_for("import-identity-button", timeout=10)
        driver.click("import-identity-button")
    driver.wait_for("paste-secret-field", timeout=10)
    driver.clear_and_type("paste-secret-field", secret_hex)
    driver.click("import-submit-button")
    driver.wait_for("handle-input", timeout=15)


def _read_handle_check_snapshot(driver) -> dict:
    """Read `handle_check_snapshot()` over the cross-app machine bridge —
    the reliable state read the target-state doc prescribes over scraping the
    message area. Returns the parsed snapshot dict."""
    snap = driver.call_machine_method("handle_check_snapshot")
    if isinstance(snap, str):
        snap = json.loads(snap)
    return snap or {}


def _outcome_variant(snap: dict):
    """Return the `HandleCheckOutcome` variant name from a snapshot.

    serde externally-tags the enum: unit variants serialize as a bare string
    (`"None"`, `"NestRunningUserUnregistered"`), data variants as a single-key
    object (`{"AlreadyOnNest": {...}}`).
    """
    outcome = snap.get("outcome")
    if isinstance(outcome, str):
        return outcome
    if isinstance(outcome, dict) and len(outcome) == 1:
        return next(iter(outcome))
    return None


def _wait_continue_enabled(driver, handle: str, timeout: float = 30) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if driver.is_enabled("handle-entry-continue-button"):
            return
        msg = (driver.get_text("handle-message-area")
               if driver.is_visible("handle-message-area") else "")
        assert "tld" not in msg.lower(), \
            f"{handle!r} wrongly rejected as invalid TLD: {msg!r}"
        time.sleep(0.5)
    msg = (driver.get_text("handle-message-area")
           if driver.is_visible("handle-message-area") else "(no message)")
    pytest.fail(f"handle-check never enabled Continue for {handle!r}; "
                f"handle-message-area: {msg!r}")


def _wait_continue_disabled(driver, timeout: float = 5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not driver.is_enabled("handle-entry-continue-button"):
            return True
        time.sleep(0.2)
    return not driver.is_enabled("handle-entry-continue-button")


# ── Test ─────────────────────────────────────────────────────────────────────

@pytest.mark.feature("identity-on-another-device")
def test_handle_check_resets_on_identity_reimport(local_web):
    driver, nest = local_web
    secret_a = nest["secret_a"]
    secret_b = nest["secret_b"]
    handle = f"test@localhost:{nest['port']}"

    # Land on the onboarding identity-choice screen (same origin as the nest).
    driver._post("/navigate", {"url": f"http://localhost:{nest['port']}/app/onboarding"})
    time.sleep(2)
    driver._ensure_agent()
    # Pillar C (uniform https): `test@localhost:{port}` now resolves to https, but
    # this nest serves plain HTTP — point the onboarding probe at the http nest via
    # the nest override (the same-box `nest_url` injection seam). Set before any
    # identity exists (set_provider_base_urls re-mounts the page).
    driver.set_provider_base_urls({"nest": f"http://localhost:{nest['port']}"})
    driver.wait_for("create-identity-button", timeout=20)

    # ── Import identity A → handle entry → Check ─────────────────────────────
    _import_identity_ui(driver, secret_a)
    driver.clear_and_type("handle-input", handle)
    driver.click("handle-check-button")
    _wait_continue_enabled(driver, handle)

    # Precondition: A is the admin → its probe concludes AlreadyOnNest, the
    # "you're already on this nest" verdict that must not survive a swap.
    snap_a = _read_handle_check_snapshot(driver)
    assert _outcome_variant(snap_a) == "AlreadyOnNest", (
        "precondition: identity A (the nest admin) must probe as AlreadyOnNest; "
        f"got snapshot {snap_a!r}"
    )
    assert driver.is_enabled("handle-entry-continue-button"), (
        "AlreadyOnNest (admin A) should leave Continue enabled before the swap: "
        f"{driver.diagnose('handle-entry-continue-button')}"
    )

    # ── Back → import a DIFFERENT identity B ─────────────────────────────────
    driver.click("handle-entry-back-button")
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if (driver.is_visible("paste-secret-field")
                or driver.is_visible("create-identity-button")):
            break
        time.sleep(0.3)
    _import_identity_ui(driver, secret_b)

    # ── The bug: A's AlreadyOnNest verdict must NOT survive B's import ───────
    # Read the machine snapshot directly (authoritative) — this is what the
    # HandleEntry page renders from.
    snap_b = _read_handle_check_snapshot(driver)
    assert _outcome_variant(snap_b) == "None", (
        "stale handle-check outcome survived the identity swap — the page would "
        f"show identity A's verdict for identity B. snapshot: {snap_b!r}"
    )
    assert snap_b.get("phase") == "Idle", \
        f"handle-check phase must reset to Idle on re-import; got {snap_b!r}"
    assert _wait_continue_disabled(driver), (
        "Continue must be disabled on the reset (idle) handle-check; a stale "
        "enabled Continue means identity A's verdict survived B's import."
    )

    # ── Belt-and-suspenders: re-check B's handle → a FRESH, B-specific outcome.
    # B is unregistered on this claimed nest → NestRunningUserUnregistered
    # (distinct from A's AlreadyOnNest), proving the reset was a real clear and
    # the next probe reflects identity B.
    driver.clear_and_type("handle-input", handle)
    driver.click("handle-check-button")
    _wait_continue_enabled(driver, handle)
    snap_b2 = _read_handle_check_snapshot(driver)
    assert _outcome_variant(snap_b2) == "NestRunningUserUnregistered", (
        "B's fresh re-check on the claimed nest should be "
        f"NestRunningUserUnregistered; got {snap_b2!r}"
    )

"""Real e2e: a ``test@localhost`` handle drives the real browser onboarding all
the way to the admin-claim screen against a local nest.

Spins up a real *unclaimed* ``fauna-nest`` that ALSO serves the built web SPA at
``/app`` (``--static-dir``) — the production topology, where the SPA and the API
share one origin (so every request, incl. the anonymous WS-RPC connection, hits
the nest directly with no test proxy). The browser is driven through the
handle-first onboarding and asserts that typing a ``test@localhost:<port>``
handle:

  1. does NOT yield ``onboarding.handle_check.outcome.tld_invalid`` (the reported bug), and
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
from common.nest import CLAIM_CODE
from conftest import get_available_apps
from drivers import create_driver

if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)

pytestmark = [pytest.mark.tier1, pytest.mark.web, pytest.mark.tier_3]


@pytest.fixture
def local_web(spa_unclaimed_nest):
    """A web driver loaded from the nest's own origin via `localhost`, so a
    `test@localhost:<port>` handle resolves to that same origin."""
    port = spa_unclaimed_nest["port"]
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
    # identity_created → recovery_kit → handle_entry: the kit page sits between.
    for _ in range(60):
        if driver.is_visible("recovery-kit-confirm-button") or driver.is_visible("handle-input"):
            break
        time.sleep(0.5)
    if driver.is_visible("recovery-kit-confirm-button"):
        app.onboarding.confirm_recovery_kit()

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


def _claim_into_the_app(driver, port: int) -> ActionLayer:
    """Every step a new admin takes in the browser, from a fresh page to the
    signed-in app: Create Identity, the recovery kit, the `test@localhost:<port>`
    handle, the claim code, the network choice (its pre-selected answer
    confirmed). Returns the action layer once a main-view landmark shows."""
    from helpers.authenticated_shell import SHELL_MARKERS
    from helpers.waiting import wait_until

    app = ActionLayer(driver)
    driver._post("/navigate", {"url": f"http://localhost:{port}/app/onboarding"})
    time.sleep(2)  # sleep-ok: page settle before agent injection, as the claim-screen test
    driver._ensure_agent()
    # Uniform https: the `localhost` handle resolves to https while this nest
    # serves plain http — see `test_test_at_localhost_reaches_admin_claim`.
    driver.set_provider_base_urls({"nest": f"http://localhost:{port}"})
    driver.wait_for("create-identity-button", timeout=20)
    driver.click("create-identity-button")
    driver.wait_for("identity-continue-button", timeout=15)
    driver.click("identity-continue-button")
    wait_until(
        lambda: driver.is_visible("recovery-kit-confirm-button") or driver.is_visible("handle-input"),
        60.0,
        diagnose=lambda: f"error={app.error_text()!r} {driver.diagnose('handle-input')}",
    )
    if driver.is_visible("recovery-kit-confirm-button"):
        app.onboarding.confirm_recovery_kit()
    driver.wait_for("handle-input", timeout=60)

    driver.clear_and_type("handle-input", f"test@localhost:{port}")
    driver.click("handle-check-button")
    wait_until(
        lambda: app.is_enabled("handle-entry-continue-button"),
        45.0,
        diagnose=lambda: f"error={app.error_text()!r} {driver.diagnose('handle-message-area')}",
    )
    driver.click("handle-entry-continue-button")
    driver.wait_for("claim-code-input", timeout=60)
    driver.clear_and_type("claim-code-input", CLAIM_CODE)
    driver.click("claim-code-submit-button")

    def landed() -> bool:
        if any(driver.is_visible(m) for m in SHELL_MARKERS):
            return True
        try:
            app.onboarding.finish_nat_mode()
        except Exception:  # noqa: BLE001 — the choice may not be showing yet
            pass
        return False

    wait_until(
        landed,
        120.0,
        interval=2.0,
        diagnose=lambda: f"error={app.error_text()!r} {driver.diagnose('feed-view')}",
    )
    return app


@pytest.mark.feature("claim-a-fresh-nest")
def test_claiming_with_a_local_handle_leaves_mail_off(local_web):
    """claim-a-fresh-nest 4's local-handle clause (`onboarding.md` § 3b.
    Serving enablement at claim), web's witness: claiming with a local handle
    leaves mail off until the admin asks.

    The nest-served twin of `test_mail_enable_at_admin_claim.py`'s
    `test_admin_claim_with_loopback_handle_derives_mail_disabled` (web's wizard
    reaches a nest only when that nest serves the SPA itself — the
    `admin-claim-onboarding-drive` surface leaves web out). Every step is the
    user's, the network choice's pre-selected answer confirmed, so the handle's
    locality is what holds mail off; the home-router clause is
    `test_marking_the_box_private_at_claim_leaves_all_four_serving_off`'s.

    The verdict anchors on the post-claim glue's own completion record
    (convention 14, as its native twin does): once this identity's
    serving-enablement run is `completed`, nothing is left to write the
    deployment toggles, so one read of the nest's anonymous set-up status is
    final in both directions.
    """
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    from helpers.waiting import (
        await_serving_enablement_for,
        serving_enablement,
        wait_until,
    )

    driver, port = local_web
    app = _claim_into_the_app(driver, port)

    # The page was fresh and made one identity, so the record's only actor is
    # the admin this claim created: read it there, since the key never leaves
    # the page.
    def claimants() -> set[str]:
        runs = (serving_enablement(driver) or {}).get("runs", [])
        return {str(r.get("actor_id", "")).lower() for r in runs if r.get("actor_id")}

    actors = wait_until(
        claimants,
        60.0,
        diagnose=lambda: (
            "no serving-enablement run recorded after the claim (the key now: "
            f"{serving_enablement(driver)!r}); error={app.error_text()!r}"
        ),
    )
    assert len(actors) == 1, f"a fresh page claimed once, but the record names {actors}"
    decided = await_serving_enablement_for(driver, actors.pop(), what="the web claim")

    with WsRpcAnonClient(f"http://127.0.0.1:{port}") as anon:
        status = anon.call("fauna.setup.status", {})
    assert not (decided or {}).get("email") and status.get("email_enabled") is False, (
        "claiming with a local handle must leave mail off (onboarding.md § 3b: "
        f"no real-domain target), but the glue decided {decided!r} and the nest's "
        f"set-up status reads email_enabled={status.get('email_enabled')!r}; "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("encryption-settings")
def test_a_new_identity_is_reachable_as_soon_as_it_is_set_up(local_web):
    """encryption-settings 10 (`direct-messages.md` § Key Package Management),
    web's witness: the app publishes a new identity's first starter keys when it
    is set up, so someone can start a conversation with it straight away.

    The nest-served twin of `test_encryption_settings_page.py`'s native journey
    (web's wizard reaches a nest only when that nest serves the SPA itself —
    the `admin-claim-onboarding-drive` surface leaves web out). Every step is
    the user's: Create Identity, the handle, the claim code, the network
    choice; then Settings → Encryption. No other session ever held this
    identity, so every key counted for it was published by this set-up. The
    signing key never leaves the page, so the nest-equality half is outcome 2's
    witness; here the stated count must be > 0.
    """
    from actions.settings import SettingsActions
    from helpers.waiting import wait_until

    driver, port = local_web
    app = _claim_into_the_app(driver, port)

    settings = SettingsActions(driver)

    def stated() -> int:
        settings.open_encryption(timeout=60.0)
        return wait_until(
            lambda: (n := settings.encryption_keypackage_count()) is not None and (n,),
            60.0,
            diagnose=lambda: driver.diagnose("encryption-keypackage-count"),
        )[0]

    count = wait_until(
        lambda: (n := stated()) > 0 and n,
        60.0,
        diagnose=lambda: f"{driver.diagnose('encryption-keypackage-count')} error={app.error_text()!r}",
    )
    assert count > 0, "the new identity holds starter keys a peer can fetch"

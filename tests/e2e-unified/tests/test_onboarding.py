import uuid
import pytest


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


@pytest.mark.feature("connect-and-sign-in")
def test_login_and_see_feed(app, nest_instance, test_user):
    """Login with valid credentials and verify the app loads successfully.

    All apps use the bridge's `set_state` to atomically seed the
    authenticated session, then assert the feed renders. The
    iOS-specific "fresh keypair + register" branch was retired with the
    handle-first rewrite — registration happens via invite redemption
    now, which is exercised by `test_invite_request_states.py`.
    """
    secret_hex = test_user["signing_key"].encode().hex()
    app.auth.login(
        node_url=nest_instance["url"],
        username=test_user["actor_id_hex"],
        password="",
        secret_hex=secret_hex,
        handle=f"e2e-{uuid.uuid4().hex[:8]}",
    )
    if app.driver.is_ios():
        # iOS lands on Messages tab (Feed is behind "More"); assert the
        # main tab container instead.
        assert app.driver.is_visible("main-tab-view"), (
            "onboarding should complete to the main tab container on iOS: "
            f"{app.driver.diagnose('main-tab-view')} error={app.error_text()!r}"
        )
    else:
        assert app.feed.is_visible(), (
            "onboarding should complete to the feed on desktop: "
            f"error={app.error_text()!r}"
        )


@pytest.mark.feature("create-identity")
def test_onboarding_identity_choice(app):
    """Verify identity choice screen shows on first launch.

    Runs on **web** too since 2026-07-30. It used to carry a `skip_unbuilt`
    claiming "web's e2e reset() re-applies in-SPA state rather than driving the real
    onboarding wizard" — **live-verified false**: `reset()` + navigation reaches
    web's real identity-choice screen and this test passes unchanged. The guard was
    written from the harness's shape rather than from a run, which is how a stale
    skip reads as coverage for months (testing.md § convention 7 — a skip is not
    coverage). If it ever regresses, fix the reach; do not re-add the guard without
    a run that shows it.
    """
    import time

    # After reset, native apps should show the identity choice screen.
    # Wait for the view to render — SwiftUI may need a moment after reset.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if app.driver.is_visible("create-identity-button") or \
           app.driver.is_visible("import-identity-button"):
            return  # Pass
        time.sleep(0.5)
    assert False, "Expected identity choice screen on first launch (neither create-identity-button nor import-identity-button visible after 15s)"


# linux-only: the behavioural pin the linux leg owed. The linux
# driver always launches with FAUNA_E2E_CREDENTIAL_DIR (a per-launch tmpdir),
# so the credential store is a readable {dir}/{namespace}.json file.
@pytest.mark.linux
@pytest.mark.feature("create-identity")
def test_confirm_identity_commits_through_the_shared_registry(app):
    """Confirming a generated identity writes the PER-ACTOR registry, not only
    the bare identity secret — observed at the one moment that separates the two
    implementations (the linux leg's owed pin).

    linux's confirm-identity used to be a hand-rolled
    `store_credentials_partial(None, Some(&secret), None)` — the bare secret
    alone. The shared `persist_confirmed_identity` additionally
    creates the per-actor account (whose `fauna/index` entry is the registry's
    marker) and reads the secret back, refusing a keystore that kept nothing.
    Reverting linux's call sites to the partial write still compiles and reds
    NOTHING in Rust — the same call-site blind spot a review measured on row
    230 — so the discriminator lives here, on the file the store actually is
    under e2e.

    The WINDOW is the assertion (convention 14's latency-independent state):
    after identity-created's Continue, before any handle submission. After
    complete-login `fauna/index` exists either way (the append path's
    `add_account` writes it), so an assertion placed later proves nothing.
    """
    import json
    import time
    from pathlib import Path

    app.onboarding.navigate_to_status()
    app.onboarding.generate_identity()
    app.driver.click("identity-continue-button")
    # handle_entry rendering is the causal barrier that the confirm's commit
    # point has run — the wizard only advances out of identity_created through
    # the confirm return (no settle-sleep: the element IS the state). The
    # wizard's next page is the recovery kit (ui.yaml: identity_created ->
    # recovery_kit -> handle_entry, built on linux) — race the two landings
    # and confirm the kit when it shows, the same idiom
    # `test_confirmed_identity_survives_relaunch.py` uses.
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if (app.driver.is_visible("recovery-kit-confirm-button")
                or app.driver.is_visible("handle-input")):
            break
        time.sleep(0.5)
    if app.driver.is_visible("recovery-kit-confirm-button"):
        app.onboarding.confirm_recovery_kit()
    else:
        app.driver.wait_for("handle-input", timeout=15)

    cred_dir = getattr(app.driver, "_resolved_credential_dir", None)
    ns = getattr(app.driver, "_resolved_keyring_app", None)
    assert cred_dir and ns, (
        "the linux driver always resolves FAUNA_E2E_CREDENTIAL_DIR + "
        "FAUNA_KEYRING_APP under e2e; without them this pin cannot observe "
        f"the store (dir={cred_dir!r}, ns={ns!r})"
    )
    store_path = Path(cred_dir) / f"{ns}.json"
    assert store_path.exists(), (
        f"the credential store file must exist after confirm-identity: "
        f"{store_path}"
    )
    stored = json.loads(store_path.read_text())
    assert "fauna/index" in stored, (
        "confirm-identity must commit through the shared registry "
        "(persist_confirmed_identity): the per-actor account index "
        "(`fauna/index`) is missing, so the write was not the shared moment. "
        f"keys={sorted(stored)}"
    )
    # And ONLY the registry: nothing on linux writes a bare `secret_key`
    # slot — every in-run read goes through the registry.
    assert "secret_key" not in stored, (
        "a bare `secret_key` slot must not be written at confirm — identity "
        f"lives only in the registry rows. keys={sorted(stored)}"
    )


# Web-only: drives the SPA onboarding via the web-only `fresh_app` fixture.
# Deselected under non-web `--client` by the conftest marker-platform filter.
@pytest.mark.web
@pytest.mark.feature("create-identity")
def test_web_onboarding_generate_identity(fresh_app):
    """Generate identity and verify we reach the handle-entry screen.

    Handle-first onboarding (onboarding.md § The pages): identity_created's
    Continue lands on handle_entry — the old nest_choice / nest_connect manual
    node-URL screens were eliminated (§ Pages that do not exist)."""
    fresh_app.onboarding.navigate_to_status()
    fresh_app.onboarding.generate_identity()
    fresh_app.driver.click("identity-continue-button")
    fresh_app.driver.wait_for("handle-input", timeout=15)
    assert fresh_app.driver.is_visible("handle-input"), (
        "web identity Continue should land on handle_entry (handle-input): "
        f"{fresh_app.driver.diagnose('handle-input')} error={fresh_app.error_text()!r}"
    )


# Web-only: drives SPA import via the web-only `fresh_app` fixture. (Native
# identity-import is covered by `test_onboarding_errors`'s import-screen tests.)
@pytest.mark.web
@pytest.mark.feature("identity-on-another-device")
def test_identity_import(fresh_app):
    """Import a pre-generated secret key and verify it's stored."""
    from nacl.signing import SigningKey
    sk = SigningKey.generate()
    secret_hex = sk.encode().hex()

    fresh_app.onboarding.navigate_to_status()
    fresh_app.onboarding.import_key(secret_hex)
    assert fresh_app.onboarding.stored_secret() == secret_hex

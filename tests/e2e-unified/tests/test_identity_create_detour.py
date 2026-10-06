"""Looking at the create screen never costs you the key you came with.

`docs/goal/behavior/onboarding.md` § 1. Identity, two ratified rules that
together are `create-identity` outcome 5:

* **"entering a screen never changes which identity is canonical; only
  confirming on it does"** — a user who pasted their real key and then tapped
  "create" to look around must not have it displaced by the two-tap detour.
  Before 2026-08-28 they did: every authenticating call signed as the throwaway,
  so a registered user was told they were not registered, and the terminal
  persisted the throwaway as the installation's long-term identity.
* **"The create screen mints once per wizard run"** — re-entering
  `identity_created` after Back re-shows the **same** key, because a user who
  was told to write the phrase down, went back, and returned must never be
  silently looking at a different one. Confirming the second key would make the
  written-down one worthless, and at that point the identity secret has no
  escrow behind it.

**What each test here does and does not pin — read before extending.** The
precedence rule itself is shared-Rust and is pinned at the machine layer by
`libs/fauna-onboarding-machine/tests/identity_precedence.rs`, which can read the
secret that actually reached the wire (`FakeNestApi::silent_challenge_secrets`)
and can therefore isolate the entry-time-origin bug exactly. An app-level test
cannot: after the create detour the wizard sits on `identity_choice`, and every
route back to `handle_entry` goes through a *commit* that re-establishes the
origin, so the broken and the correct machine are indistinguishable from
outside. What the app adds instead — and what no unit test can say — is that the
detour is REACHABLE and survivable: the wizard does not strand the user, does
not lose the slot, and the key that reaches the nest at the end is still theirs.
This is the same division `test_trust_prompt.py` documents for its own
deliberately-unasserted half.

The second rule needs no such caveat — it is directly observable and is pinned
here exactly as stated.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

_e2e_dir = str(Path(__file__).resolve().parent.parent)
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from helpers.authenticated_shell import wait_for_authenticated_shell  # noqa: E402

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


@pytest.fixture
def tls_nest_with_registered_actor(app, request, nest_mode, tmp_path_factory):
    """A claimed nest serving real self-signed HTTPS, plus one registered actor.

    ⚠ **The TLS is not optional.** Under uniform-https a typed
    `<handle>@127.0.0.1:<port>` resolves to `https://127.0.0.1:<port>`, so a
    plain-HTTP nest (the shared `nest_instance`) is simply unreachable by the
    handle check: the probe fails, the outcome never lands, and Continue stays
    disabled — which surfaces as a refused click on a disabled control rather
    than as anything naming TLS. `serve_tls=True` is the same answer
    `test_mail_enable_at_admin_claim.py`'s loopback fixture gives, and the app
    trusts the self-signed floor through channel binding once
    `_relaunch_trusting_nest` has named the nest at launch.

    A dedicated nest rather than the session one also keeps the sign-in below
    off the shared `test_user` seat, whose accumulated state other modules
    depend on.

    **Web takes the other harness route, and it is the same seam, not a
    workaround.** Web's e2e origin is never TLS and the browser owns TLS in
    wasm, so the web probe keeps the strict client and can never trust the
    self-signed floor (onboarding.md § 2, *Local / self-hosted targets*). What
    reaches a plain-HTTP test nest there is the same-box `nest_url` injection
    that section names as the harness's seam (`_web_onboarding_routed_to`),
    aimed at `spa_url` — the SPA proxy onto the session nest. The actor is a
    fresh one on that nest, never `test_user`, so the reason for a dedicated
    nest holds there too. What the test proves is unchanged: the silent
    challenge after the detour is signed with the brought key.
    """
    if app.driver.is_web():
        from common.auth import create_actor_and_register
        from conftest import _web_onboarding_routed_to

        nest = request.getfixturevalue("nest_instance")
        user = create_actor_and_register(
            nest["port"],
            base_url=nest["url"],
            admin_signing_key=nest["admin"]["signing_key"],
        )
        with _web_onboarding_routed_to(app.driver, request.getfixturevalue("spa_url")):
            yield nest, user
        return

    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "identity-detour-nest", serve_tls=True
    )
    try:
        from common.auth import create_actor_and_register

        user = create_actor_and_register(
            nest["port"],
            base_url=nest["url"],
            admin_signing_key=nest["admin"]["signing_key"],
        )
        yield nest, user
    finally:
        cleanup()


@pytest.mark.feature("create-identity")
def test_re_entering_the_create_screen_reshows_the_same_secret(app):
    """Create → Back → Create again shows the SAME key, not a fresh one.

    The whole point is the user who wrote the phrase down. `begin_create_identity`
    mints only into an empty slot, so the second visit re-displays the first
    key; a mint on every entry would quietly invalidate what they wrote down.
    `reset()` clears the slot, so a genuinely new wizard run still gets a new key
    — which is why this test can rely on the `app` fixture's reset for isolation.
    """
    ob = app.onboarding
    ob.navigate_to_status()

    ob.generate_identity()
    first = app.driver.get_text("secret-key-display")
    assert first and len(first.strip()) >= 64, (
        "the create screen must display a full secret before this test means "
        f"anything, got {first!r}"
    )

    app.driver.click("identity-created-back-button")
    app.driver.wait_for("create-identity-button", timeout=15)

    ob.generate_identity()
    second = app.driver.get_text("secret-key-display")

    assert second == first, (
        "re-entering the create screen showed a DIFFERENT key. A user who was "
        "told to write the first one down and came back would now be looking at "
        "a key that is not theirs, and confirming it would make the written-down "
        "phrase worthless — the identity secret has no escrow behind it at this "
        "point in the wizard (onboarding.md § 1. Identity, 'The create screen "
        f"mints once per wizard run'). first={first[:16]}… second={second[:16]}…"
    )


@pytest.mark.feature("create-identity")
def test_the_create_screen_detour_still_signs_you_in_as_the_key_you_brought(
    app, tls_nest_with_registered_actor
):
    """Import a registered key, detour through the create screen, come back and
    finish — the nest recognises you.

    The observable is deliberately the sharpest one the app offers: on
    `AlreadyOnNest` the handle check's Continue exits STRAIGHT to the
    authenticated shell, while an identity the nest does not know is routed to
    the claim-code or invite page instead. So reaching the shell — and never
    seeing `claim-code-input` — says the silent challenge was signed with the
    imported, registered key after the detour. A wizard that had swapped in the
    throwaway would land on the "you are not registered" branch, which is
    exactly how the pre-2026-08-28 bug presented to real users.
    """
    from conftest import _relaunch_trusting_nest

    nest, user = tls_nest_with_registered_actor
    secret_hex = user["signing_key"].encode().hex()
    typed_handle = f"{user['handle']}@127.0.0.1:{nest['port']}"

    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=15)

    # Back out to the choice screen the way a curious user would: off the handle
    # step, then off whichever identity screen it routes to.
    app.driver.click("handle-entry-back-button")
    app.driver.wait_for("identity-import-back-button", timeout=15)
    app.driver.click("identity-import-back-button")
    app.driver.wait_for("create-identity-button", timeout=15)

    # The detour itself: open Create, look at the key, and leave without
    # confirming. Entering must not make this the canonical identity.
    ob.generate_identity()
    throwaway = app.driver.get_text("secret-key-display")
    assert throwaway.strip().lower() != secret_hex.lower(), (
        "the create screen displayed the imported key rather than a freshly "
        "minted one — the detour this test describes did not happen"
    )
    app.driver.click("identity-created-back-button")
    app.driver.wait_for("import-identity-button", timeout=15)

    # Finish as the user brought the key to do.
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=15)
    ob.fill_handle(typed_handle)
    ob.run_handle_check(timeout=45)
    ob.submit_handle()

    assert app.driver.is_absent("claim-code-input"), (
        "after the create-screen detour the handle check routed to the "
        "claim-code page, which means the nest did not recognise the identity "
        "that signed the silent challenge — the detour swapped the brought key "
        f"for the throwaway. error={app.error_text()!r}"
    )
    marker = wait_for_authenticated_shell(app)
    assert marker, (
        "the signed-in shell never rendered after a detour-then-import sign-in; "
        f"error={app.error_text()!r}"
    )

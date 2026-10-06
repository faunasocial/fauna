"""tier_3 e2e: web's CHANNEL 1 leg of the post-auth "nest identity changed"
surface — the bearer RE-MINT, as distinct from `test_nest_identity_pin_post_auth.py`'s
channel 3 (the background silent challenge).

``docs/goal/architecture/security.md`` § Transport trust → § Post-auth surfacing
(ratified 2026-07-23); ``tests/e2e-unified/ui.yaml`` page ``launch_identity_changed``.

Web has two live post-auth re-check points (`nest-identity-escalation.ts`'s own
doc comment names both): channel 1 is `$lib/api.ts`'s `getAuthToken`, whose wasm
`challengeVerify` (the silent challenge since 2026-09-23, `login.md` § When to
use which) possession-verifies its `cert_binding` against the TOFU pin on every
re-mint; channel 3 is `$lib/store.ts`'s `refreshFromServer`, the background
silent challenge `test_nest_identity_pin_post_auth.py` already covers end to
end (verdict → escalation → recovery round trip). Both funnel through the SAME
`escalateIfNestIdentityChanged` and the SAME blocking surface, so this module
does not re-prove the surface or the recovery mechanism (channel-agnostic,
already proven) — only that channel 1 actually *reaches* it, and that a benign
re-mint does not spuriously trip it.

**Web-only by construction, not by convention.** `bearer_force_refresh` is a
web-only e2e command (`identity-e2e.ts`) — native channel 1 has no e2e trigger
yet (equally untested today, out of this row's scope).
Kept in its own file rather than folded into the channel-3 module because that
module's `pin_app` fixture is forced onto every test in its file by an autouse
fixture (`_require_credential_persistence`), which would otherwise multiply
these tests across the other four post-auth apps for no reason — the same
reason `test_web_silent_sign_in_ws_rpc.py` is its own file rather than a case
inside a shared module.
"""

from __future__ import annotations

import pytest

from common.launch_harness import make_launch_harness, reached_authenticated_app
from helpers.app_surface import skip_unbuilt
from helpers.budgets import IDENTITY_REFRESH_S
from helpers.waiting import session_generation

pytestmark = [pytest.mark.web, pytest.mark.tier_3]

# Element IDs (tests/e2e-unified/ui.yaml § launch_identity_changed) — same
# surface channel 3 paints, mirrored here for a self-contained file.
IDENTITY_WARNING = "nest-identity-changed-warning"
IDENTITY_TRUST_BUTTON = "nest-identity-changed-trust-button"
LAUNCH_RETRY = "launch-retry-button"

#: A nest_actor_id no nest can ever prove possession of.
BOGUS_PIN = "ab" * 32


@pytest.fixture
def web_pin_env(request):
    """web's plain-HTTP harness + the identity it authenticates with — the
    same WITHDRAWN-arm shape `test_nest_identity_pin_post_auth.py::pin_env`
    uses for web (plain HTTP serves no `cert_binding`, so a poisoned pin is
    *unconfirmable* rather than *contradicted*; both are the same verdict
    class and reach the same surface — reviewed 2026-08-22: this is the
    real lever for closing gap 1, TLS is not required)."""
    spa_url = request.getfixturevalue("spa_url")
    user = request.getfixturevalue("test_user")
    harness = make_launch_harness("web", spa_url=spa_url)
    env = {
        "node_url": spa_url,
        "secret_hex": bytes(user["signing_key"]).hex(),
    }
    try:
        yield harness, env
    finally:
        try:
            harness.teardown()
        except Exception:
            pass


def _seed_pin(driver, nest_url, actor_id_hex):
    """Seed a TOFU pin through the shared machine-free bridge — mirrors
    `test_nest_identity_pin_post_auth.py::_seed_pin`."""
    import json

    driver.call_machine_method(
        "set_nest_identity_pin_for_test",
        json.dumps({"nest_url": nest_url, "actor_id": actor_id_hex}),
    )


def _read_pin(driver, nest_url):
    """Mirrors `test_nest_identity_pin_post_auth.py::_read_pin`."""
    import json

    raw = driver.call_machine_method(
        "nest_identity_pin_for_test", json.dumps({"nest_url": nest_url})
    )
    if raw in (None, "", "null"):
        return None
    if not isinstance(raw, str):
        return raw
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


def _app_diagnostics(driver):
    """Rule 6 — a failure must diagnose itself. Web's console + `pageerror`
    ring survives the escalation's navigation; mirrors the channel-3 module's
    reader, web branch only (this file is web-only)."""
    console = getattr(driver, "console_log", None)
    if not callable(console):
        return "no diagnostics available on this app"
    try:
        lines = console() or []
        hits = [ln for ln in lines if "identity" in ln.lower()]
        return f"console (identity lines): {hits[-20:]!r} | console (tail): {lines[-25:]!r}"
    except Exception as exc:  # never mask the real failure with a reader fault
        return f"console unavailable: {exc!r}"


@pytest.mark.feature("connect-and-sign-in")
def test_post_auth_identity_change_bearer_remint_channel(web_pin_env):
    """A mid-session identity flip detected on the bearer RE-MINT (channel 1,
    not the background silent challenge channel 3) must reach the same
    blocking surface.

    ``bearer_force_refresh`` drives ``getAuthToken(secret, undefined, true)`` —
    the production re-mint ``$lib/api.ts``'s WS-RPC token provider calls on a
    4401 — whose own catch escalates (``security.md`` § Post-auth surfacing,
    channel 1). Positive-only: the surface + recovery round trip is already
    proven end to end for channel 3 in
    ``test_nest_identity_pin_post_auth.py::test_post_auth_identity_change_warns_then_recovers``,
    and recovery is channel-agnostic (it re-enters the same ``LaunchMachine``
    regardless of which channel produced the verdict), so this only needs to
    prove channel 1 reaches the surface at all."""
    harness, env = web_pin_env
    driver = harness.launch_and_route(
        secret_hex=env["secret_hex"], node_url=env["node_url"], trust=None
    )
    reached_authenticated_app(driver, timeout=90)

    _seed_pin(driver, env["node_url"], BOGUS_PIN)
    assert _read_pin(driver, env["node_url"]) == BOGUS_PIN, (
        "the machine-free bridge must seed the pin store on an authenticated session"
    )

    generation_before = session_generation(driver)
    if generation_before is None:
        skip_unbuilt(
            driver,
            surface="the session_generation teardown counter",
            detail="mid-session identity-changed escalation must bump it at "
            "initiation, same as sign-out/switch/factory-reset",
            tracked="",
        )

    driver.call_command("bearer_force_refresh")

    try:
        driver.wait_for(IDENTITY_WARNING, timeout=IDENTITY_REFRESH_S)
    except TimeoutError as e:
        raise AssertionError(f"{e}. {_app_diagnostics(driver)}") from e
    assert driver.is_visible(IDENTITY_WARNING), (
        "a mid-session identity the nest can no longer prove — detected on the "
        "bearer re-mint — must block the session and paint the identity-changed surface"
    )
    assert session_generation(driver) > generation_before, (
        "the bearer-re-mint escalation must count itself as a session teardown, "
        "same as the background-challenge channel"
    )
    assert driver.is_visible(IDENTITY_TRUST_BUTTON), (
        "the surface must offer an explicit 'trust this nest' re-pin action"
    )
    assert driver.is_absent(LAUNCH_RETRY), (
        "a possible-MITM signal must NOT offer a Retry CTA"
    )


def test_post_auth_valid_bearer_remint_does_not_escalate(web_pin_env):
    """A forced re-mint whose verdict is NOT identity-changed must be
    swallowed: the session stays authenticated and never paints the surface.

    The channel-1 twin of
    ``test_nest_identity_pin_post_auth.py::test_post_auth_valid_refresh_does_not_escalate``
    — proves ``getAuthToken``'s own catch escalates ONLY the identity verdict,
    never a routine re-mint, and that a real change on THIS channel still
    fires (the causal barrier that rules out a wedged pipeline)."""
    harness, env = web_pin_env
    driver = harness.launch_and_route(
        secret_hex=env["secret_hex"], node_url=env["node_url"], trust=None
    )
    reached_authenticated_app(driver, timeout=90)

    driver.call_command("bearer_force_refresh")
    assert driver.is_absent(IDENTITY_WARNING), (
        "a re-mint against a still-valid identity must NOT paint the "
        "identity-changed surface"
    )
    reached_authenticated_app(driver, timeout=15)

    # Causal barrier (convention 14): prove the pipeline is still live after the
    # valid re-mint — a genuine identity change on THIS channel still fires.
    _seed_pin(driver, env["node_url"], BOGUS_PIN)
    driver.call_command("bearer_force_refresh")
    driver.wait_for(IDENTITY_WARNING, timeout=IDENTITY_REFRESH_S)
    assert driver.is_visible(IDENTITY_WARNING), (
        "after a benign re-mint, channel 1 must still escalate a real change — "
        "the benign one neither tore down nor wedged it"
    )

"""tier_3 e2e: a succeeded device is refused, and lands where it can act.

Goal doc: ``docs/goal/behavior/identity-succession.md`` § Propagation → *Own
device fleet* — "each device's next connect gets the ``superseded`` refusal →
the client surfaces 'this identity was succeeded — import the new identity' and
the user imports the new seed (QR / paste)".

**Why this is an e2e and not more unit tests.** Every piece of this affordance
is already individually pinned: the launch machine's terminal state, tui's
routing arm, the shared onboarding transition, the claim-free vs verified
message pair. What none of them can establish is that the pieces *compose* —
that a real nest's real refusal reaches a real app and leaves the user somewhere
they can act. That is precisely the class of break flow-tracing exists to catch:
symbol-existence holds at every link while the flow between them is severed.

The trigger is the device's **next connect**, exactly as the goal doc words it,
which is why the app is relaunched rather than merely waited on. A running tui
session does not poll for supersession: the verdict arrives from the silent
sign-in refresh, which ``session.rs`` spawns once at session start
(``spawn_domain_refresh``) — so an already-established session learns nothing
until it next starts one. Relaunching is therefore the honest reproduction of
the user's experience, not a shortcut.

The relaunch **preserves the client-local store** (``preserve_state_across_
relaunch``). Without that the drivers hand the new process a fresh data dir, the
identity is gone, and the app comes up on the onboarding screen — which is a
different app, not the *old device*, and would make this test pass or fail for
reasons unrelated to succession.

**Convention 14.** The waits below are generous deadline polls on stable end
states, never settle-sleeps: a green run pays nothing, and a slow machine does
not turn a correct app into a red test.

**One assertion deliberately lives at unit level, not here.** The goal doc's
message must not name the successor until the chain *proves* it, and that
negative is pinned in ``fauna-tui``'s launch tests where the pre-verification
state is deterministic. At this level it is a transient window before a
best-effort round trip: asserting it would mean catching the app between two
states, which is the wall-clock-dependent shape ``testing.md`` § point 14 calls
defunct. What this test pins instead is the *stable* half — the message
eventually names the verified successor — which is what proves the round trip
composes end to end.

**Both preconditions are arranged through shared Rust, not the app UI.** The
identity needs a registered RecoveryKey before it can be succeeded at all, and
the Settings → Recovery kit ceremony that mints one has landed on tui only.
Driving it through that screen would make this journey skip on six apps for a
reason that has nothing to do with what it asserts — so kit registration and the
succession are both fixture setup (convention 8's carve-out), signed by
``libs/fauna-client-recovery/examples/recovery_fixture.rs``. The UI ceremony
keeps its own coverage in ``test_recovery_kit_settings.py``. What stays in the
UI is everything this test is actually about: the refusal, the routing, the
message, and the import.

tier_3: needs a real ``fauna-nest`` binary. App-agnostic by construction — it
asserts only ui.yaml IDs that exist on every app.
"""
from __future__ import annotations

import secrets

import pytest

from helpers.budgets import APP_RELAUNCH_S, PRE_IDENTITY_CHAIN_WALK_S
from helpers.succession import register_recovery_kit, succeed_identity
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

# The kit secret and an actor id are both 32 bytes as lowercase hex.
_HEX32_LEN = 64


@pytest.mark.feature("take-your-account-back")
def test_a_succeeded_device_is_refused_and_routed_to_import(
    succeedable_app, nest_instance
):
    """succeed the identity → the old device is refused → it lands on the
    import flow naming the verified successor → importing the new seed is
    accepted.
    """
    app, user = succeedable_app

    # ── Precondition: this identity can be succeeded at all ──────────────
    # Succession is authorized by a RecoveryKey signature, so the identity must
    # have registered a kit. Registered through shared Rust rather than the
    # Settings ceremony on purpose — see the module docstring: that ceremony is
    # tui-only today, and this journey is not about it.
    kit_secret = register_recovery_kit(
        nest_instance["url"],
        actor_id_hex=user["actor_id_hex"],
        identity_seed_hex=bytes(user["signing_key"]).hex(),
    )
    assert len(kit_secret) == _HEX32_LEN, (
        f"the kit is 64-hex, got {len(kit_secret)} chars"
    )

    # Pin the client-local store BEFORE the relaunch below, so the app that
    # comes back is this same device with this same identity. Done here, while
    # the launch that created the state is still the current one.
    if not app.driver.preserve_state_across_relaunch():
        from helpers.app_surface import skip_environment

        skip_environment(
            f"the {app.driver.__class__.__name__} driver cannot pin its "
            "client-local store across a relaunch, so the relaunched process "
            "would be a fresh install rather than the succeeded device"
        )

    # ── Arrange: a genuine succession, minted outside the app ────────────
    # Fixture setup, not the behavior under test (convention 8's carve-out) —
    # see ``helpers/succession.py`` for why it is signed in shared Rust.
    successor_seed = secrets.token_bytes(32).hex()
    successor_id = succeed_identity(
        nest_instance["url"],
        old_actor_id_hex=user["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed,
        old_seed_hex=bytes(user["signing_key"]).hex(),
    )
    assert successor_id != user["actor_id_hex"], (
        "the successor must be a genuinely new actor id, so `actor_id ≡ pubkey` "
        "stays true everywhere"
    )

    # ── Act: the device's next connect ───────────────────────────────────
    # `recover()` is the drivers' teardown + relaunch-with-the-same-config, the
    # same pair `NativeLaunchHarness.relaunch` runs. The store is pinned above,
    # so this process comes up holding the succeeded identity.
    assert app.driver.recover(), "the app did not come back up after a relaunch"

    # ── Assert: the refusal reaches the app and routes it ────────────────
    # `paste-secret-field` IS the import affordance (page `identity_import`);
    # no new IDs were minted for this state, deliberately.
    wait_until(
        lambda: app.is_visible("paste-secret-field"),
        APP_RELAUNCH_S,
        diagnose=lambda: (
            "a succeeded device must be routed to the import flow, but it is "
            "elsewhere. This is the flow break the test exists to catch: the "
            "refusal reached the client and nothing routed it. Error surface "
            f"says: {app.error_text()!r}"
        ),
    )

    # The refusal must be explained, not silently dropped onto a dead end.
    assert app.error_text(), (
        "landing on the import page with no explanation leaves the user with "
        "no idea why their session ended — the reason must be machine state, "
        "set atomically with the step"
    )

    # The stable half of the message contract: once the chain PROVES the
    # successor, the app names it. Before that it must not, which is pinned in
    # the tui launch tests (see the module docstring).
    wait_until(
        lambda: successor_id in app.error_text(),
        PRE_IDENTITY_CHAIN_WALK_S,
        diagnose=lambda: (
            "the verified successor is never named, so the verification round "
            f"trip does not compose end to end. Successor is {successor_id}; "
            f"the message reads {app.error_text()!r}"
        ),
    )

    # ── The way out actually works ───────────────────────────────────────
    # The successor inherits the handle through the succession transaction, so
    # importing its seed is the whole remedy the goal doc promises.
    # `type_text` is not one of the facade's passthroughs, so it goes to the
    # driver directly — same as the onboarding action layer does.
    app.driver.type_text("paste-secret-field", successor_seed)
    app.click("import-submit-button")

    wait_until(
        lambda: not app.is_visible("paste-secret-field"),
        PRE_IDENTITY_CHAIN_WALK_S,
        diagnose=lambda: (
            "importing the successor seed must be accepted — the user is "
            "otherwise stranded on the page that told them what to do. Error "
            f"surface says: {app.error_text()!r}"
        ),
    )

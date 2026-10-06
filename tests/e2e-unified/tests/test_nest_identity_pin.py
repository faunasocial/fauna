"""tier_3 e2e: the nest-identity TOFU pin — the launch "nest identity changed"
warning — CROSS-APP (native + web).

``docs/goal/architecture/security.md`` § Transport trust;
``tests/e2e-unified/ui.yaml`` page ``launch_identity_changed`` (ALL-APP since
2026-07-13).

The client TOFU-pins ``(nest → nest_actor_id)`` on first contact and **warns
loudly** when a later launch can no longer prove the pinned identity. The warning
blocks auto-entry; the user either re-trusts (forget the pin → re-TOFU) or points
at a different nest. There is deliberately **no Retry CTA** — a retry loop on a
possible-MITM signal is what this rule forbids, and ``retry_silent_challenge()``
no-ops outside ``Offline{transient:true}`` anyway, so a Retry button here would be
*dead*. The ``launch_identity_changed`` page carries no ``launch-retry-button`` on
any client; that is asserted below.

**One module, two arms — the arm is a property of the nest's TLS posture, not the
client code.** The launch flow, the warning, the trust-recovery, and every UI
assertion are IDENTICAL across clients (priority #1). The only per-app
difference is what a stale pin *disagrees with*, which is fixed by which nest
fixture the client can reach:

* **Native** (linux, tui, macos, ios) ride ``self_signed_nest`` — the one fixture
  that serves real (self-signed) HTTPS. It binds ``127.0.0.1``, so every native
  app accepts the cert on the **loopback** branch of the trust posture
  (``security.md`` § Transport trust) — no per-app cert plumbing, which is why
  the apple legs need no driver change at all.
  Native pins ride the connect-time channel binding and
  both graduation points are gated on the ``https://`` scheme
  (``fauna-launch-machine/src/auth.rs`` + ``fauna-anon-client/src/bearer.rs``), so
  a seeded pin is only ever consulted over TLS. Against a genuine ``cert_binding``
  a disagreeing pin fails possession-verify as ``IdentityError::PinChanged`` — the
  **Changed** arm.
* **Web** rides the plain-HTTP ``spa_url`` proxy (its nest origin). A plain-HTTP
  nest serves no ``cert_binding`` at all, so a pre-seeded pin can never be
  *confirmed* → the **Withdrawn** arm. (Web can't reach Changed: its e2e origin is
  never TLS, so there is no binding to *disagree* with. The possession-verify
  itself is unit-tested in ``fauna_client_core::nest_trust``.)

Both arms render the same warning and recover the same way; they differ only in
what the re-TOFU leaves pinned — Changed re-pins the nest's genuine identity,
Withdrawn has no binding to re-pin so the origin's pin is simply forgotten. That
single divergence is data-driven off ``pin_env.arm``.

**Seeding the pin** goes through the shared E2E-bridge name
``set_nest_identity_pin_for_test`` (``onboarding.md`` § E2E bridge contract), which
writes whatever pin store the client installed at startup — ``DiskPinStore`` on
native, ``LocalStoragePinStore`` on web. One name, every app, and the harness
never learns either backend's on-disk shape.

The native ``teardown()``+``launch()`` relaunch and the web ``hard_reload()``
relaunch are unified behind ``common.launch_harness`` — web has no ``app_path``
and its "relaunch" keeps localStorage, which is why the two identity-pin modules
lived apart until that harness landed. ``nest_instance``/``self_signed_nest``
build a real ``fauna-nest`` binary (tier_3).
"""

import contextlib
import json

import pytest

from common.cred_store import requires_secret_service
from common.keyring import secret_service_available
from helpers.app_surface import (
    skip_environment,
    skip_unless_optimistic_launch_entry,
)
from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import _trust_seeder, get_available_apps
from helpers.waiting import session_generation

pytestmark = [
    pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui, pytest.mark.web,
    pytest.mark.macos, pytest.mark.ios,
]

#: Clients that render ``launch_identity_changed`` and can persist a pin across a
#: relaunch. android/windows join by growing a ``launch_harness`` leg — the bridge
#: seam they need is already all-app.
#:
#: macos/ios joined 2026-08-01 with **no new harness code**: the apple legs of
#: every piece already existed — ``AppleFileCredStore`` (``common/cred_store.py``,
#: the same ``FAUNA_E2E_CREDENTIAL_DIR`` file backend tui uses),
#: ``make_launch_harness``'s native branch, both drivers'
#: ``preserve_state_across_relaunch()``, the shared FaunaKit
#: ``LaunchIdentityChangedView``, and the value-returning ``machine_method_result``
#: reader path the ``nest_identity_pin_for_test`` half needs (apple's two stash
#: bugs there were fixed 2026-07-18 — ``onboarding.md`` § E2E bridge contract).
#: Joining was therefore this list + the ``pytestmark`` markers + iOS's
#: ``ios_setup`` fixture shape below.
_SUPPORTED_APPS = ("linux", "tui", "web", "macos", "ios")

# Element IDs (tests/e2e-unified/ui.yaml § launch_identity_changed + § launch).
IDENTITY_WARNING = "nest-identity-changed-warning"
IDENTITY_TRUST_BUTTON = "nest-identity-changed-trust-button"
LAUNCH_FALLTHROUGH = "launch-fallthrough-button"
LAUNCH_RETRY = "launch-retry-button"

#: A nest_actor_id no nest can ever prove possession of.
BOGUS_PIN = "ab" * 32


def _pin_clients():
    available = get_available_apps()
    return [c for c in _SUPPORTED_APPS if c in available]


@pytest.fixture(params=_pin_clients())
def pin_client(request):
    """The client under test; its id lands in the test name
    (``[linux]``/``[tui]``/``[web]``), which is what conftest's ``--client``
    filter reads."""
    return request.param


@pytest.fixture(autouse=True)
def _require_credential_persistence(pin_client):
    """linux's adapter runs a private Secret Service daemon of its own and skips
    only where the box cannot supply one; the tui/macos/ios file backend and
    web's localStorage need nothing and never skip."""
    if requires_secret_service(pin_client) and not secret_service_available():
        skip_environment(
            "linux's real-keyring launches run a private gnome-keyring-daemon, which this box cannot supply"
        )


class _PinEnv:
    """The launch harness for one app plus the identity/nest it pins and the
    arm that client's TLS posture reaches."""

    def __init__(self, *, harness, node_url, nest, secret_hex, arm):
        self.harness = harness
        self.node_url = node_url
        self.nest = nest  # the harness launch's `trust` (None on web, which reads no env)
        self.secret_hex = secret_hex
        self.arm = arm  # "changed" (native/TLS) | "withdrawn" (web/plain-HTTP)


@pytest.fixture
def pin_env(request, pin_client, tmp_path):
    """Per-app launch harness + the identity/nest it pins.

    Native rides ``self_signed_nest`` (real TLS) → the CHANGED arm; web rides the
    plain-HTTP ``spa_url`` proxy → the WITHDRAWN arm. The nest fixtures are pulled
    lazily per client (``getfixturevalue``) so a web run never spins up the TLS
    nest and vice-versa."""
    if pin_client == "web":
        spa_url = request.getfixturevalue("spa_url")
        user = request.getfixturevalue("test_user")
        harness = make_launch_harness("web", spa_url=spa_url)
        env = _PinEnv(
            harness=harness,
            node_url=spa_url,
            nest=None,
            secret_hex=bytes(user["signing_key"]).hex(),
            arm="withdrawn",
        )
    else:
        nest = request.getfixturevalue("self_signed_nest")
        if pin_client == "ios":
            # No bare `ios_app_path` fixture exists — iOS's direct-launch fixture
            # (`ios_setup`) returns `{"udid", "app_path"}` together, because
            # `drivers/ios.py`'s `launch()` requires both. Same branch as
            # `test_onboarding_launch_routing_smoke.py`'s `launch_harness`.
            ios_setup = request.getfixturevalue("ios_setup")
            harness = make_launch_harness(
                "ios", tmp_path=tmp_path, app_path=ios_setup["app_path"],
                udid=ios_setup["udid"], seed_trust=_trust_seeder(request),
            )
        else:
            app_path = request.getfixturevalue(f"{pin_client}_app_path")
            harness = make_launch_harness(
                pin_client, tmp_path=tmp_path, app_path=app_path,
                seed_trust=_trust_seeder(request),
            )
        env = _PinEnv(
            harness=harness,
            node_url=nest["url"],
            nest=nest,
            secret_hex=bytes(nest["user"]["signing_key"]).hex(),
            arm="changed",
        )
    try:
        yield env
    finally:
        with contextlib.suppress(Exception):
            harness.teardown()


def _seed_pin(driver, nest_url, actor_id_hex):
    """Seed a TOFU pin through the shared E2E bridge. Writes the *installed* pin
    store, so this one call seeds ``DiskPinStore`` on native and
    ``LocalStoragePinStore`` on web."""
    driver.call_machine_method(
        "set_nest_identity_pin_for_test",
        json.dumps({"nest_url": nest_url, "actor_id": actor_id_hex}),
    )


def _read_pin(driver, nest_url):
    """The pin the installed store now holds for ``nest_url`` (hex, or None).

    The dispatcher returns a JSON-serialised value (a *quoted* hex string, or
    ``null``), but clients differ in whether their agent re-wraps it before
    stashing it: web hands back the JSON verbatim, the native agents hand back the
    already-unwrapped string. Accept both rather than encode one app's
    convention."""
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
        return raw  # already unwrapped by the agent


# Deliberately NOT `@pytest.mark.feature("connect-and-sign-in")`. The user-visible
# outcome ("the app warns you, before signing in, if the nest identity changed") is
# outcome 3 of that page and is already witnessed on all five apps by
# `test_changed_nest_identity_warns_then_recovers` below. This test asserts an
# INTERNAL invariant on top of that surface, and it is live on ios alone — tagging it
# would file macos and tui as a `declared absence` against an outcome they both
# deliver, i.e. make the catalog lie about them.
def test_identity_changed_counts_the_optimistic_entry_teardown(pin_env):
    """On an app that entered OPTIMISTICALLY, `LaunchPhase::IdentityChanged` does
    not merely route a wizard — it tears down a session the user is already
    inside, and convention 14 requires every teardown arm to count itself.

    **iOS only, and that is the point, not a gap.** The six gating apps
    authenticate nothing before the verdict lands, so the same verdict destroys
    no session and there is no teardown to count; they declare that absence
    (`skip_unless_optimistic_launch_entry`) rather than assert a number. Read
    this as a MUTATION PIN on one arm, never as cross-app coverage — the
    assertion below is live on exactly one leg.

    **Why it needs its own test rather than a line in the sibling above.** The
    gate skips, and a skip in the middle of
    `test_changed_nest_identity_warns_then_recovers` would abandon its trust
    button / re-TOFU half on five apps to assert one number on one. Here the
    skip is free: it fires before the launch.

    **The arm this pins.** `runLaunch()` enters optimistically *before*
    `machine.start()`, so the relaunched process builds a client, the machine
    answers `.identityChanged`, and `dispatchLaunch` calls
    `leaveAuthenticatedSession()` — iOS's FIFTH teardown arm, and the only one
    the *connection* half of `security.md` § Post-auth surfacing ever reaches
    (a bearer re-mint or SPKI re-handshake re-runs the launch machine and lands
    straight here, never touching `tearDownSessionForSwitch`). It counted
    nothing until 2026-09-21, and the
    post-auth module cannot pin it: its own escalation also bumps the *switch*
    arm, so its `>` assert passes either way. Delete the
    `SessionGeneration.recordTeardown()` from `leaveAuthenticatedSession` and
    THIS test is what goes red.

    A fresh process starts the counter at 0 (`AutomationCounters.swift` —
    in-memory `static`, never persisted), and the relaunch below is a genuine
    force-quit + fresh process, so the post-relaunch reading is this launch's
    own teardown count and needs no baseline.
    """
    env = pin_env
    # Before the launch, so a gating app spends no app boot to skip.
    skip_unless_optimistic_launch_entry(env.harness.client)

    driver = env.harness.launch(
        secret_hex=env.secret_hex, node_url=env.node_url, trust=env.nest
    )
    reached_authenticated_app(driver, timeout=90)

    _seed_pin(driver, env.node_url, BOGUS_PIN)
    assert _read_pin(driver, env.node_url) == BOGUS_PIN, (
        "the bridge must actually seed the installed pin store; if this is the "
        "real nest id, `set_nest_identity_pin_for_test` never reached it"
    )

    if not driver.preserve_state_across_relaunch():
        pytest.skip(
            f"{env.harness.client} driver cannot pin its client store across a "
            "relaunch, so a seeded pin cannot survive to be checked"
        )
    env.harness.relaunch()

    driver.wait_for(IDENTITY_WARNING, timeout=60)
    generation = session_generation(driver)
    assert generation is not None, (
        "an app that enters optimistically must publish the session_generation "
        "teardown counter — without it convention 14's negative asserts cannot "
        "tell a relaunch from a quiet gesture on this app at all"
    )
    assert generation >= 1, (
        "the identity-changed launch verdict tore down the session this app had "
        "already entered optimistically, so it must have counted itself as a "
        "teardown (session_generation), exactly like the switch, sign-out, "
        "factory-reset and logout arms. A 0 here means the arm that dropped the "
        "session — `leaveAuthenticatedSession` — is bumping nothing, and every "
        "`assert_no_relaunch` spanning a gesture that can end in this verdict "
        "would report 'no relaunch' over a session that plainly went away"
    )


@pytest.mark.feature("connect-and-sign-in")
def test_changed_nest_identity_warns_then_recovers(pin_env):
    """A pinned identity the nest can no longer prove → ``LaunchPhase::IdentityChanged``
    → the launch screen blocks auto-entry and shows the warning; the trust button
    forgets the pin, re-TOFUs, and lands the user in the app.

    This is the arm a dead trust button fails: ``trust_nest_identity()`` is a
    no-op unless it is called on the machine that PRODUCED the IdentityChanged
    verdict (it reads the secret + nest_url off that state), so a button wired to a
    fresh machine silently does nothing and the user can never get in.

    Native reaches this over ``self_signed_nest``'s real binding (the pin
    *disagrees* → Changed); web reaches the same surface over its plain-HTTP origin
    (the pin can't be *confirmed* → Withdrawn). Same warning, same recovery."""
    env = pin_env

    # Bring up the client with the identity persisted. The two arms differ ONLY in
    # how first contact treats the pin store, which is a property of the nest's TLS
    # posture, not the client:
    driver = env.harness.launch(
        secret_hex=env.secret_hex, node_url=env.node_url, trust=env.nest
    )

    if env.arm == "changed":
        # Native / TLS: first contact TOFU-pins the nest's REAL identity. Wait for
        # that to settle (the online baseline) BEFORE overwriting it with the bogus
        # pin below, or the async TOFU races the seed and re-pins the real id. Fresh
        # web drivers compile wasm cold, so a generous ceiling — it returns the
        # instant the app is reached, so native is unaffected.
        reached_authenticated_app(driver, timeout=90)
    # The Withdrawn / web arm deliberately does NOT reach an authenticated session
    # first: its plain-HTTP first contact pins nothing (no binding to TOFU), and a
    # cached session would bypass the pinned silent challenge on the relaunch, so
    # the identity-changed surface would never render. Web stays on the reset launch
    # surface until `relaunch()` runs its first (and only) pinned challenge.

    # Overwrite (native) / set (web) the pin with one no nest can ever prove.
    _seed_pin(driver, env.node_url, BOGUS_PIN)
    assert _read_pin(driver, env.node_url) == BOGUS_PIN, (
        "the bridge must actually seed the installed pin store; if this is the "
        "real nest id, `set_nest_identity_pin_for_test` never reached it"
    )

    # Relaunch: the pin store must SURVIVE. The web driver keeps localStorage
    # across a `hard_reload()` by construction; the native drivers hand the
    # relaunched process a fresh data dir UNLESS `preserve_state_across_relaunch()`
    # pins this launch's stable dirs into the config `relaunch()` re-launches with
    # — so call it (it also gates a driver that genuinely can't preserve).
    if not driver.preserve_state_across_relaunch():
        pytest.skip(
            f"{env.harness.client} driver cannot pin its client store across a "
            "relaunch, so a seeded pin cannot survive to be checked"
        )
    env.harness.relaunch()

    driver.wait_for(IDENTITY_WARNING, timeout=60)
    assert driver.is_visible(IDENTITY_WARNING), (
        "a pinned identity the nest can no longer prove must block auto-entry and "
        "warn on the launch screen"
    )
    assert driver.is_visible(IDENTITY_TRUST_BUTTON), (
        "the warning must offer an explicit 'trust this nest' re-pin action"
    )
    assert driver.is_absent(LAUNCH_RETRY), (
        "a possible-MITM signal must NOT offer a Retry CTA; the "
        "launch_identity_changed page carries no launch-retry-button on any client"
    )

    # Trust this nest → forget the pin → re-run the silent challenge → into the
    # app. A dead trust button hangs here.
    driver.click(IDENTITY_TRUST_BUTTON)
    reached_authenticated_app(driver, timeout=60)
    assert driver.is_absent(IDENTITY_WARNING), (
        "after re-trusting, the warning should be gone and the user in the app"
    )

    # The stale pin was forgotten. Changed re-TOFU'd the nest's REAL identity over
    # the live binding; Withdrawn has no binding to re-pin, so the origin's pin is
    # simply cleared. Both must have dropped the bogus one.
    repinned = _read_pin(driver, env.node_url)
    assert repinned != BOGUS_PIN, (
        f"the stale pin should be forgotten and re-TOFU'd, still got {repinned!r}"
    )
    if env.arm == "withdrawn":
        assert repinned is None, (
            "a plain-HTTP nest serves no binding, so the re-TOFU has nothing to "
            f"re-pin — the origin's pin should be forgotten, got {repinned!r}"
        )
    else:
        assert repinned is not None, (
            "re-TOFU against a real TLS binding must re-pin the nest's genuine "
            f"identity, got {repinned!r}"
        )

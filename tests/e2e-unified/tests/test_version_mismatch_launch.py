"""tier_3 E2E: a degraded "needs-update" nest drives the app launch surface to
the non-retry "update required" affordance — the native launch-machine apps
(linux/tui/windows/macos/ios) and, in its own web-marked test below, the web SPA.

The client half of ``version-compatibility.md`` Dimension 4 / Track 3, proved
against a REAL degraded ``fauna-nest`` binary (the strongest proof — item d of
the Dim 4 backlog). The shared classifier/renderer and the launch-machine
routing are tier_1-proven in ``libs/fauna-protocol`` / ``libs/fauna-launch-machine``;
this test closes the loop end-to-end: a real binary nest that booted DEGRADED
(its on-disk schema is newer than the binary can operate) rejects the launch
silent-challenge handshake with the typed ``fauna.nest.outdated``, and the app
must route that to a NON-retry surface — render the localized
``error.nest.outdated`` message in ``error-message``, hide ``launch-retry-button``
(retrying the same nest is futile), keep ``launch-fallthrough-button``. That is
verbatim what ``docs/goal/behavior/onboarding.md`` § App-launch routing's
``fauna.nest.outdated`` row requires.

Mechanism (the launch-machine path, not the direct-FFI path — a degraded nest
rejects the *connect handshake*, so the launch silent challenge is what fires):
1. Boot a fresh unclaimed nest (records ``schema_meta=(1,1)``), stop it, stamp the
   reader floor above the binary's ``CURRENT_SCHEMA_VERSION``, restart in place —
   it boots degraded and answers ``fauna.nest.outdated`` to every WS-RPC call,
   the connect handshake included (``test_schema_version_compat.py`` proves the
   degraded boot at the API tier; this exercises a real client against it).
2. Pre-seed the app's file-backed credential store with a throwaway identity +
   the degraded nest's URL BEFORE launch, so the launch machine hydrates and runs
   the silent challenge against it (the authenticated-relaunch path) — case 1 of
   ``apps/fauna-linux/src/main.rs`` (identity + nest_url →
   ``launch_silent_challenge_flow``), and ``App::route_locked_store`` →
   ``launch::start`` on tui (``apps/fauna-tui/src/app.rs``). On apple the same
   seeded store is what ``checkKeychainOnLaunch`` hydrates before it runs the
   machine, so both targets reach ``dispatchLaunch`` with a real nest to
   challenge. The identity need not
   be registered: the degraded nest rejects the handshake with
   ``fauna.nest.outdated`` before any registration check, so a random Ed25519
   seed suffices.
3. The silent-challenge ceremony maps ``fauna.nest.outdated`` → ``NeedsUpdate`` →
   the launch machine's ``Offline { transient: false }`` → the app's non-retry
   surface: linux ``views::launch::LaunchPhase::NeedsUpdate``, tui
   ``launch::LaunchSurface::NeedsUpdate`` (``apps/fauna-tui/src/launch.rs``
   ``route()``, whose element list deliberately omits ``launch-retry-button``),
   macOS ``MacAppState.LaunchGate.needsUpdate`` and iOS
   ``AppState.needsUpdateMessage`` — both set from that same
   ``case .offline(let transient)`` arm of ``dispatchLaunch(_ snap:)``
   (``Fauna-macOS/App/FaunaMacApp.swift``, ``Fauna-iOS/App/FaunaApp.swift``),
   rendering the one shared ``LaunchNeedsUpdateView``.

The seed-then-boot half is the SHARED ``make_launch_harness`` native lifecycle
(``tests/common/launch_harness.py``) rather than a hand-rolled per-app
``seed_credentials`` payload — one fixture for every launch-machine app
(priority #1), the same seam ``test_onboarding_launch_routing_smoke.py`` and
``test_nest_identity_pin.py`` already share.
"""

import secrets

import pytest

from common.launch_harness import make_launch_harness
from common.nest import (
    stamp_schema_meta,
    start_nest_in_place,
    stop_nest,
)
from conftest import _serve_spa_proxy, _trust_seeder, get_available_apps


# tier_3 = full stack, real binary. App markers sit PER-TEST (the launch-machine
# test carries linux+tui, the direct-path web test carries web) so conftest's
# `--app` deselect selects exactly the tests each app can drive — a module-wide
# union would hand `--app linux` the web test, which needs a browser.
pytestmark = pytest.mark.tier_3

# Far above any plausible CURRENT_SCHEMA_VERSION this binary carries, so the
# incompatible verdict is unambiguous regardless of future baseline bumps
# (mirrors test_schema_version_compat.py).
FUTURE_VERSION = 9999

#: The apps that consume `fauna.nest.outdated` through the shared `LaunchMachine`
#: — the connect-handshake reject lands `Offline { transient: false }` and each
#: app's launch router maps it to its own non-retry surface. web is OUT of this
#: tuple because it has no credential-store backend for the native harness — its
#: identity seeds through localStorage instead — but it consumes the SAME wasm
#: `LaunchMachine` phase, so its arm is the separate web-marked test below.
#: **macos and ios joined 2026-09-21**, and the note that used to stand here —
#: "apple takes the *direct* silent-challenge path (`FfiError::NestOutdated`)",
#: read off `LaunchNeedsUpdateView`'s own prose — was wrong about the LAUNCH
#: path: `FfiError::NestOutdated` is where the handshake's refusal is *typed*,
#: but both apple targets project it through this very tuple's mechanism.
#: `dispatchLaunch(_ snap: LaunchSnapshot)` switches on `snap.phase` and takes
#: `case .offline(let transient)` — macOS to `LaunchGate.needsUpdate`, iOS to
#: `AppState.needsUpdateMessage` — exactly as linux/tui/windows do, which is
#: also why `test_account_index_unreadable_launch.py` can already drive the
#: side channel riding that same snapshot on both apple targets.
#: **windows joined 2026-09-20**: it mirrors linux's mechanism through the
#: same shared `LaunchMachine` (`App.xaml.cs`'s `LaunchPhase.Offline` with
#: `transient: false` routes to `LaunchNeedsUpdatePage`, which renders
#: `error-message` + `launch-fallthrough-button` and deliberately carries no
#: `launch-retry-button`), and the adapter the old note said was missing now
#: exists — `common/cred_store.py::WindowsFileCredStore`, dispatched by
#: `make_cred_store` on both the keyring and `file_backed` paths.
LAUNCH_MACHINE_APPS = ("linux", "tui", "windows", "macos", "ios")


def _apps(*want: str) -> list[str]:
    """The wanted apps this run actually selected, intersected with what the
    machine / ``--app`` offers — so ``--app tui`` runs [tui] and a bare ubuntu run
    stays [linux, tui]. Mirrors ``test_onboarding_launch_routing_smoke._clients``."""
    available = get_available_apps()
    return [a for a in want if a in available]


@pytest.fixture(scope="function")
def degraded_nest(request, nest_mode, tmp_path_factory):
    """A nest that boots DEGRADED: fresh and unclaimed (so it records
    ``schema_meta=(1,1)``), stopped, its reader floor stamped above this build's
    ``CURRENT_SCHEMA_VERSION``, restarted in place → it answers
    ``fauna.nest.outdated`` to every WS-RPC call, the connect handshake included.

    Both consumers used to build this through a module helper taking the binary,
    a root and a port — a raw ``start_nest`` behind a plain function, which is the
    shape neither AST pin can grade (``testing.md`` § Default app and nest mode,
    ruling (1)). Every act it performs is mode-agnostic, and none of that is new
    code: ``stop_nest`` signals only the handle's own ``proc`` (``docker kill`` on
    a container), ``db_path`` is real in docker because ``/data`` is a host
    bind-mount, and ``start_nest_in_place`` delegates to the provider's own
    ``start_in_place`` — so "stop it, stamp the DB, bring it back on the same data
    dir" is a sentence every mode can say. It asks for ``unclaimed``, which docker
    declares.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "degraded-nest", unclaimed=True,
    )
    try:
        stop_nest(nest, graceful=False)
        stamp_schema_meta(nest["db_path"], FUTURE_VERSION, FUTURE_VERSION)
        start_nest_in_place(nest)
        yield nest
    finally:
        cleanup()


@pytest.fixture
def outdated_nest_app(request, degraded_nest, tmp_path):
    """A native app launched already pointed (via a seeded credential store) at a
    real degraded nest, so its launch silent challenge hits
    ``fauna.nest.outdated``. Indirect-parametrized over ``LAUNCH_MACHINE_APPS``
    (``request.param`` is the app). Yields the launched driver; tears both down."""
    app = request.param
    harness = None
    nest = degraded_nest
    try:
        # `file_backed=True` keeps linux on the shared store's file backend
        # (`FAUNA_E2E_CREDENTIAL_DIR`) instead of its default real Secret
        # Service, so this case stays headless-safe exactly as the hand-rolled
        # `seed_credentials` payload it replaces was — no `secret_service_unlocked`
        # gate is owed. tui, windows and both apple targets are file-backed
        # either way (`AppleFileCredStore` is apple's only backend).
        if app == "ios":
            # No bare `ios_app_path` fixture exists — iOS's direct-launch
            # fixture (`ios_setup`) returns `{"udid", "app_path"}` together,
            # because `drivers/ios.py`'s `launch()` requires both. Same branch
            # as `test_account_index_unreadable_launch.py`'s own fixture, which
            # seeds this identical launch machinery on both apple targets.
            ios_setup = request.getfixturevalue("ios_setup")
            harness = make_launch_harness(
                "ios",
                tmp_path=tmp_path,
                app_path=ios_setup["app_path"],
                udid=ios_setup["udid"],
                file_backed=True,
                seed_trust=_trust_seeder(request),
            )
        else:
            harness = make_launch_harness(
                app,
                tmp_path=tmp_path,
                app_path=request.getfixturevalue(f"{app}_app_path"),
                file_backed=True,
                seed_trust=_trust_seeder(request),
            )
        # A throwaway 32-byte Ed25519 seed — never registered; the degraded nest
        # rejects the handshake before checking registration.
        #
        # `trust=None`: the degraded nest answers every kind — `nest.info`
        # included — with `fauna.nest.outdated`, so the escrow-trust seed cannot
        # read the identity it would name (the seed's `nest.info` read raised in
        # setup, every run, once launches had to name their nest), and this
        # launch stops at that refusal, long before an account runtime could
        # need a trusted holder.
        yield harness.launch(
            secret_hex=secrets.token_hex(32), node_url=nest["url"], trust=None
        )
    finally:
        if harness is not None:
            harness.teardown()


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.parametrize(
    "outdated_nest_app", _apps(*LAUNCH_MACHINE_APPS), indirect=True
)
@pytest.mark.feature("upgrades-never-lose-data")
def test_outdated_nest_launch_shows_non_retry_update_surface(outdated_nest_app):
    """Launching against a degraded nest must land the non-retry "update required"
    surface: the localized message in ``error-message``, NO ``launch-retry-button``
    (a doomed retry loop on a version mismatch), and ``launch-fallthrough-button``
    ("Use a different nest") still offered.

    One body for every native app on purpose (priority #1): the three assertions
    are the surface's ratified contract in ``onboarding.md`` § App-launch
    routing, not a per-app shape, and each app reaches them from the same
    ``Offline { transient: false }`` snapshot. Only web needs an arm of its own,
    and for a structural reason — a browser and an SPA proxy rather than a
    binary and a credential store."""
    driver = outdated_nest_app

    # The silent challenge runs async after launch; error-message becomes visible
    # only on the NeedsUpdate terminal phase (it's hidden while Launching, and the
    # TransientRetry phase uses launch-transient-error instead) — so its
    # visibility is the clean signal that the launch machine reached NeedsUpdate.
    driver.wait_for("error-message", timeout=30)

    message = driver.get_text("error-message")
    assert "outdated" in message.lower(), (
        f"expected the localized error.nest.outdated message, got {message!r}"
    )

    # The defining property of the NeedsUpdate surface vs. the transient-retry one:
    # no Retry CTA (retrying the same outdated nest is futile — Dim 4's
    # "update prompt vs. retry" distinction).
    assert driver.is_absent("launch-retry-button"), (
        "launch-retry-button must be hidden on the non-retry update surface"
    )
    # "Use a different nest" remains — the user's only actionable path here.
    assert driver.is_visible("launch-fallthrough-button"), (
        "launch-fallthrough-button should remain so the user can switch nests"
    )


@pytest.mark.web
@pytest.mark.feature("upgrades-never-lose-data")
def test_outdated_nest_launch_shows_non_retry_update_surface_web(
    degraded_nest, static_dir
):
    """The web arm of the same Dim 4 row — the
    upgrades-never-lose-data 3 wide gap: the SPA consumes the SAME wasm
    ``LaunchMachine`` phase (``Offline { transient: false }`` →
    ``launchState === 'needs_update'``, `wasm-launch.ts` ``offlineTransientOf``)
    but seeds its identity through localStorage rather than a credential-store
    backend, so it rides ``WebLaunchHarness`` instead of the native tuple.

    ``launch_and_route`` seeds the identity + degraded-nest origin into
    localStorage, then ``hard_reload()``s — the FIRST run of the routed launch
    path, the browser twin of a native binary routing on boot. The SPA's silent
    challenge hits ``fauna.nest.outdated`` and must land the identical non-retry
    surface: the localized message in the canonical ``error-message`` element,
    NO ``launch-retry-button``, ``launch-fallthrough-button`` still offered."""
    server = None
    harness = None
    nest = degraded_nest
    try:
        # The SPA proxy pins the browser's origin to the DEGRADED nest — the
        # session `spa_url` fixture only proxies the shared healthy
        # `nest_instance`, which would never produce the outdated verdict.
        proxy_url, server = _serve_spa_proxy(static_dir, nest["url"])
        harness = make_launch_harness("web", spa_url=proxy_url)
        # A throwaway 32-byte Ed25519 seed — never registered; the degraded nest
        # rejects the handshake before checking registration.
        driver = harness.launch_and_route(
            secret_hex=secrets.token_hex(32), node_url=proxy_url, trust=nest
        )

        # Fresh web drivers compile wasm cold and the challenge runs async after
        # the routed reload, so a generous ceiling (identity-pin web precedent).
        driver.wait_for("error-message", timeout=60)

        message = driver.get_text("error-message")
        assert "outdated" in message.lower(), (
            f"expected the localized error.nest.outdated message, got {message!r}"
        )
        assert driver.is_absent("launch-retry-button"), (
            "launch-retry-button must be hidden on the non-retry update surface"
        )
        assert driver.is_visible("launch-fallthrough-button"), (
            "launch-fallthrough-button should remain so the user can switch nests"
        )
    finally:
        if harness is not None:
            harness.teardown()
        if server is not None:
            server.shutdown()

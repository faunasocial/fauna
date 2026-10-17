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

The other direction — the APP too old for the nest (outcome 15,
``version-compatibility.md`` § Dimension 4) — has its own test below: a nest
whose WebSocket upgrade requires a subprotocol this build does not speak
completes the launch challenge's upgrade and closes it at once with 4426 (the
app offers a ``fauna.v<N>`` the nest does not serve), and the app must land the
same non-retry surface with the "your app is out of date" text.

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
from helpers.app_launch import app_launch_config
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


#: The subprotocol a `test-hooks` nest is told to require
#: (`FAUNA_TEST_REQUIRED_SUBPROTOCOL`, `routes.rs::required_client_subprotocol`)
#: so that this build's apps — which offer `fauna.v1` — are too old for it.
NEXT_MAJOR_SUBPROTOCOL = "fauna.v2"


@pytest.fixture(scope="function")
def too_new_nest(nest_binary, tmp_path_factory):
    """A locally built nest that requires ``NEXT_MAJOR_SUBPROTOCOL`` at both
    client WebSocket upgrades, so it answers this build's apps — which offer
    ``fauna.v1``, a Fauna client subprotocol it does not serve — with a
    completed upgrade closed at once with 4426: the nest an app too old for it
    meets (outcome 15). Only a `test-hooks` build
    reads the override, which is why this is ``_make_nest`` on the local binary
    and not the mode's provider (the PREMISE disposition in
    ``test_nest_mode_axis.py`` says so)."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary, tmp_path_factory, "too-new-nest", unclaimed=True,
        extra_env={"FAUNA_TEST_REQUIRED_SUBPROTOCOL": NEXT_MAJOR_SUBPROTOCOL},
    )
    try:
        yield nest
    finally:
        cleanup()


def _launch_pointed_at(request, app, tmp_path, nest):
    """Launch native ``app`` already pointed (via a seeded credential store) at
    ``nest``, so its launch silent challenge runs against it. Yields the
    launched driver; tears the harness down. One body for both directions of
    the version mismatch — only the nest differs."""
    harness = None
    try:
        # `file_backed=True` keeps linux on the shared store's file backend
        # (`FAUNA_E2E_CREDENTIAL_DIR`) instead of its default real Secret
        # Service, so this case stays headless-safe exactly as the hand-rolled
        # `seed_credentials` payload it replaces was — no `secret_service_unlocked`
        # gate is owed. tui, windows and both apple targets are file-backed
        # either way (`AppleFileCredStore` is apple's only backend).
        harness = make_launch_harness(
            app,
            tmp_path=tmp_path,
            **app_launch_config(app, request),
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
        # need a trusted holder. A nest this app is too old for refuses the
        # seed's connection the same way, with a 4426 close after the upgrade.
        yield harness.launch(
            secret_hex=secrets.token_hex(32), node_url=nest["url"], trust=None
        )
    finally:
        if harness is not None:
            harness.teardown()


@pytest.fixture
def outdated_nest_app(request, degraded_nest, tmp_path):
    """A native app launched against a real degraded nest, so its launch silent
    challenge hits ``fauna.nest.outdated``. Indirect-parametrized over
    ``LAUNCH_MACHINE_APPS`` (``request.param`` is the app)."""
    yield from _launch_pointed_at(request, request.param, tmp_path, degraded_nest)


@pytest.fixture
def too_old_app(request, too_new_nest, tmp_path):
    """A native app launched against a nest that requires a subprotocol it does
    not speak, so its launch challenge's upgrade is closed at once with 4426.
    Indirect-parametrized over ``LAUNCH_MACHINE_APPS``."""
    yield from _launch_pointed_at(request, request.param, tmp_path, too_new_nest)


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


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.parametrize("too_old_app", _apps(*LAUNCH_MACHINE_APPS), indirect=True)
@pytest.mark.feature("upgrades-never-lose-data")
def test_an_app_too_old_for_its_nest_says_to_update_and_offers_no_retry(too_old_app):
    """An app launched against a nest too new for it — the nest refuses the
    app's subprotocol, completing the WebSocket upgrade only to close it with
    4426 before any request is served — lands the same non-retry surface
    outcome 3 does, saying the APP is
    out of date: the localized ``errors.subprotocol_mismatch`` text in
    ``error-message``, NO ``launch-retry-button`` (no retry can fix a version
    mismatch), ``launch-fallthrough-button`` kept (``version-compatibility.md``
    § Dimension 4: refused at the upgrade, a terminal message, no retry).

    The 4426 is classified once, in shared Rust: the anonymous connector types
    the request it ended (``AnonClientError::SubprotocolMismatch``) and the
    shared ``silent_challenge_error`` lands it as ``NeedsUpdate`` naming the
    app → ``Offline { transient: false }``, the snapshot outcome 3's apps
    already route — the identical surface the pre-upgrade HTTP 426 (an offer
    with no Fauna client subprotocol at all) lands at connect
    (``transport-connection.md`` § Connection lifecycle, the 4426 paragraph).
    Before that, the refusal read as a transport blip and the app offered a
    Retry that could never succeed."""
    driver = too_old_app

    # `error-message` is visible only on the terminal NeedsUpdate phase — the
    # transient-retry phase a misread refusal used to land paints
    # `launch-transient-error` instead — so its visibility is the signal.
    driver.wait_for("error-message", timeout=30)

    message = driver.get_text("error-message")
    assert "out of date" in message.lower(), (
        f"expected the localized errors.subprotocol_mismatch text, got {message!r}"
    )
    assert driver.is_absent("launch-retry-button"), (
        "launch-retry-button must be hidden: no retry can fix an app too old "
        "for its nest"
    )
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


@pytest.mark.web
@pytest.mark.feature("upgrades-never-lose-data")
def test_an_app_too_old_for_its_nest_says_to_update_and_offers_no_retry_web(
    too_new_nest, static_dir
):
    """The web arm of outcome 15 — the reason the nest closes a refused
    subprotocol with 4426 at all: a browser reads a refused upgrade's HTTP 426
    as a bare 1006, so before the ruling a web tab too old for its nest retried
    until "Cannot connect" (``transport-connection.md`` § Connection lifecycle,
    the 4426 paragraph). The SPA proxy forwards ``Sec-WebSocket-Protocol`` and
    pumps frames verbatim, so the nest's 4426 reaches the browser; the wasm
    ``fixed_conn`` types the challenge's request as
    ``WsRpcError::SubprotocolMismatch``, the shared ``silent_challenge_error``
    lands ``NeedsUpdate`` naming the app, and the wasm ``LaunchMachine``'s
    ``Offline { transient: false }`` puts the SPA on ``needs_update`` — the
    surface the native arm above asserts, saying the APP is out of date
    (``errors.subprotocol_mismatch``), never the nest-outdated text."""
    server = None
    harness = None
    try:
        proxy_url, server = _serve_spa_proxy(static_dir, too_new_nest["url"])
        harness = make_launch_harness("web", spa_url=proxy_url)
        # A throwaway seed — the 4426 ends the dial before any registration
        # check. `trust=None`: this nest refuses every connection, so there is
        # no identity to seed (web ignores the argument either way).
        driver = harness.launch_and_route(
            secret_hex=secrets.token_hex(32), node_url=proxy_url, trust=None
        )

        driver.wait_for("error-message", timeout=60)

        message = driver.get_text("error-message")
        assert "out of date" in message.lower(), (
            f"expected the localized errors.subprotocol_mismatch text, got {message!r}"
        )
        assert "nest is running an outdated" not in message.lower(), (
            f"the app-outdated surface must not say the NEST is outdated, got {message!r}"
        )
        assert driver.is_absent("launch-retry-button"), (
            "launch-retry-button must be hidden: no retry can fix an app too old "
            "for its nest"
        )
        assert driver.is_visible("launch-fallthrough-button"), (
            "launch-fallthrough-button should remain so the user can switch nests"
        )
    finally:
        if harness is not None:
            harness.teardown()
        if server is not None:
            server.shutdown()


@pytest.fixture
def signed_in_on_dedicated_nest(request, app, nest_mode, tmp_path_factory):
    """``app`` signed in as a fresh actor on a DEDICATED claimed nest this test
    alone may stop — the mid-session half of outcome 3 needs to take the nest an
    app is already connected to down and bring it back degraded, which the
    shared session nest must never be. Yields ``(app, nest)``.

    web reaches the dedicated nest through an SPA proxy of its own (a browser
    can only talk to a nest through one, ``_serve_spa_proxy``), handed to the
    login as ``spa_url`` — the one difference from the native apps, which point
    straight at the nest, so one test body serves all of them. The proxy keeps
    forwarding to the same address across the in-place restart."""
    from conftest import _login_app_as, _make_user, _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "degraded-mid-session"
    )
    server = None
    try:
        spa_url = None
        if app.driver.is_web():
            spa_url, server = _serve_spa_proxy(
                request.getfixturevalue("static_dir"), nest["url"]
            )
        user = _make_user(nest)
        # The barrier on the switch itself: the app must be live on THIS nest
        # as THIS actor before the nest goes away, or the surface asserted
        # below could come from a session that never reached it.
        _login_app_as(
            app, request, nest, user, verify_live_actor=True, spa_url=spa_url
        )
        yield app, nest
    finally:
        if server is not None:
            server.shutdown()
        cleanup()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("upgrades-never-lose-data")
def test_a_signed_in_app_whose_nest_restarts_degraded_shows_the_update_surface(
    signed_in_on_dedicated_nest,
):
    """The mid-session half of outcome 3: an app already signed in to a nest that
    is restarted into the degraded needs-update mode stops reconnecting and lands
    the same non-retry surface the launch path shows — the localized
    ``error.nest.outdated`` text in ``error-message``, no ``launch-retry-button``,
    ``launch-fallthrough-button`` kept — with no relaunch.

    The degraded nest refuses the app's bearer upgrade with ``401`` (it can
    verify no bearer), so the reconnect re-mints over the silent challenge and
    meets ``fauna.nest.outdated``; the shared reconnect supervisor classifies
    that (``RpcError::action`` → ``NeedsUpdate``) as a refusal no retry can clear
    and stops, and the app routes the stop's ``SessionEndingVerdict`` to its
    launch surface (``version-compatibility.md`` § Dimension 4). web runs its
    own wasm reconnect loop: the browser cannot see the ``401``, but the
    dropped connection makes every later dial re-mint, the SPA's token provider
    rejects with a ``NestOutdatedError`` whose ``code`` is the wire code, the
    loop stops on it as ``nest_outdated``, and the SPA re-enters its launch
    route, whose own challenge lands the ``needs_update`` state. Before that, the
    degraded nest opened the session and refused each request in it, so the app
    reported itself connected and showed only per-screen failures."""
    app, nest = signed_in_on_dedicated_nest
    d = app.driver

    stop_nest(nest, graceful=False)
    stamp_schema_meta(nest["db_path"], FUTURE_VERSION, FUTURE_VERSION)
    start_nest_in_place(nest)

    from helpers import budgets
    from helpers.waiting import wait_until
    from i18n.strings import S

    message = wait_until(
        lambda: d.is_visible("launch-fallthrough-button")
        and d.is_visible("error-message")
        and d.get_text("error-message"),
        budgets.APP_RELAUNCH_S,
        diagnose=lambda: (
            f"no update surface after the nest restarted degraded; {_surface_state(d)}"
        ),
    )
    assert S.error.nest.outdated in message, (
        f"expected the localized error.nest.outdated message; {_surface_state(d)}"
    )
    assert d.is_absent("launch-retry-button"), (
        "launch-retry-button must be hidden on the non-retry update surface; "
        f"{_surface_state(d)}"
    )


def _surface_state(d) -> str:
    """The update surface's three elements, read into a mid-session test's
    failure message (convention 6)."""
    try:
        err = d.get_text("error-message") if d.is_visible("error-message") else None
    except Exception as e:  # noqa: BLE001 — diagnostic only
        err = f"<{type(e).__name__}: {e}>"
    return (
        f"error-message={err!r} "
        f"launch-retry-button visible={d.is_visible('launch-retry-button')} "
        f"launch-fallthrough-button visible={d.is_visible('launch-fallthrough-button')}"
    )


@pytest.fixture
def signed_in_on_too_new_capable_nest(request, app, nest_binary, tmp_path_factory):
    """``app`` signed in as a fresh actor on a DEDICATED, locally built claimed
    nest this test alone may stop and bring back as ``too_new_nest``'s kind —
    the mid-session half of outcome 15 needs the nest an app is already
    connected to upgraded past it. Yields ``(app, nest)``.

    It starts as an ordinary nest (requiring ``fauna.v1``, so the sign-in goes
    through), and it is ``_make_nest`` on the local binary rather than the
    mode's provider for ``too_new_nest``'s reason: only a ``test-hooks`` build
    reads ``FAUNA_TEST_REQUIRED_SUBPROTOCOL``, which the test sets before the
    in-place re-spawn (the PREMISE disposition in ``test_nest_mode_axis.py``
    says so).

    web reaches the nest through an SPA proxy of its own (``_serve_spa_proxy``),
    handed to the login as ``spa_url``, exactly as ``signed_in_on_dedicated_nest``
    does; the proxy keeps forwarding to the same address across the in-place
    re-spawn."""
    from conftest import _login_app_as, _make_nest, _make_user

    nest, cleanup = _make_nest(nest_binary, tmp_path_factory, "too-new-mid-session")
    server = None
    try:
        spa_url = None
        if app.driver.is_web():
            spa_url, server = _serve_spa_proxy(
                request.getfixturevalue("static_dir"), nest["url"]
            )
        user = _make_user(nest)
        # The barrier on the switch itself: the app must be live on THIS nest
        # as THIS actor before the nest goes away, or the surface asserted
        # below could come from a session that never reached it.
        _login_app_as(
            app, request, nest, user, verify_live_actor=True, spa_url=spa_url
        )
        yield app, nest
    finally:
        if server is not None:
            server.shutdown()
        cleanup()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("upgrades-never-lose-data")
def test_a_signed_in_app_whose_nest_is_upgraded_past_it_shows_the_update_surface(
    signed_in_on_too_new_capable_nest,
):
    """The mid-session half of outcome 15: an app already signed in to a nest
    restarted to require a client subprotocol this build does not speak
    (``NEXT_MAJOR_SUBPROTOCOL``; the app offers ``fauna.v1``) stops reconnecting
    and lands the same non-retry surface the launch path shows — the localized
    ``errors.subprotocol_mismatch`` text in ``error-message``, no
    ``launch-retry-button``, ``launch-fallthrough-button`` kept — with no
    relaunch.

    The road is the 4426 one: the restarted nest completes the reconnect's
    WebSocket upgrade and closes it at once with 4426, because the app offers a
    ``fauna.v<N>`` it does not serve (``transport-connection.md`` § Connection
    lifecycle, the 4426 paragraph). The shared supervisor decodes that close as
    ``NestClientError::SubprotocolMismatch``, which it holds terminal; a
    reconnect that must mint first meets the same close on the mint's upgrade
    (``ApiError::SubprotocolMismatch``, mapped to that same error). Either way
    the session ends on ``SessionEndingVerdict::AppOutdated``
    (``FfiSessionEndingVerdict::AppOutdated`` on android, apple and windows),
    which each app routes back into launch, whose own challenge lands the
    non-retry surface (``version-compatibility.md`` § Dimension 4, *the app is
    the one too old*)."""
    from helpers import budgets
    from helpers.waiting import wait_until
    from i18n.strings import S

    app, nest = signed_in_on_too_new_capable_nest
    d = app.driver

    stop_nest(nest, graceful=False)
    nest["extra_env"] = {"FAUNA_TEST_REQUIRED_SUBPROTOCOL": NEXT_MAJOR_SUBPROTOCOL}
    start_nest_in_place(nest)

    message = wait_until(
        lambda: d.is_visible("launch-fallthrough-button")
        and d.is_visible("error-message")
        and d.get_text("error-message"),
        budgets.APP_RELAUNCH_S,
        diagnose=lambda: (
            "no update surface after the nest was upgraded past the app; "
            f"{_surface_state(d)}"
        ),
    )
    assert S.errors.subprotocol_mismatch in message, (
        "expected the localized errors.subprotocol_mismatch message; "
        f"{_surface_state(d)}"
    )
    assert d.is_absent("launch-retry-button"), (
        "launch-retry-button must be hidden: no retry can fix an app too old "
        f"for its nest; {_surface_state(d)}"
    )

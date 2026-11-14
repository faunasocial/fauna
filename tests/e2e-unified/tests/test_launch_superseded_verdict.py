"""tier_3 e2e: a launch verdict still in flight never tears down the session
that superseded it — multiple-accounts outcome 17.

``docs/goal/behavior/onboarding.md`` § App-launch routing → *A verdict renders
only while its own launch is still the current one*: a launch verdict is decided
from a store read taken when the launch STARTED and rendered a round trip later.
Anything that becomes a newer session in between — an account switch, the
remove-account promote, the factory-reset re-onboard, the e2e session patch —
leaves a verdict in flight that was already wrong when it was computed, and the
app drops it whole instead of rendering it over the newer session.

The trigger here is the one the ruling names for the e2e harness: the session
patch. The window is held open DETERMINISTICALLY, never by a sleep (convention
14): the stored identity's nest is a local listener this test owns, which accepts
the launch's dial and answers nothing until the test releases it. So the launch is
provably in flight (its connection was accepted) when the patch builds the newer
session, and its verdict — a transport failure, the moment the test closes the
held connections — provably arrives after that session stands.

The negative assert ("the newer session still stands") rests on a causal barrier:
the app's ``launch_verdicts`` counters (``fauna_e2e_agent::LAUNCH_VERDICTS_KEY``),
bumped in the verdict handler for every delivered verdict, ``routed`` or
``superseded``. The test waits for their sum to move, then asserts which one did.

tier_3: a real ``fauna-nest`` binary (``nest_instance``) for the newer session.
tui, the lead app, built the counters first; linux and android joined
2026-11-07 (``apps/fauna-linux/src/launch_currency.rs``,
``apps/fauna-android/app/src/main/java/com/fauna/app/core/LaunchCurrency.kt``),
then web (``apps/fauna-web/src/lib/launch-currency.ts``) in its own web-marked
test below, then macos and ios (``LaunchMachineBox.deliver`` counting into the
shared FaunaKit ``LaunchVerdicts``), then windows
(``apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/LaunchCurrency.cs``, every
launch drive passing ``App.DriveLaunchAsync``). iOS enters optimistically from
the stored identity before the launch's verdict, so there the witness proves the stronger
property: the shell the session patch built over that optimistic entry survives
the stale verdict.

android reaches the held listener and the newer session's nest over ``adb
reverse``: the held port is a free port like any nest's and rides in the launch
config's ``url``; the real nest, named only by the session patch, gets its
reverse from ``ensure_reverse``.

web reaches a nest only through an SPA proxy, so its held listener sits behind
one (``_serve_spa_proxy``), and the patched session names the session
``spa_url`` proxy in front of the real nest.
"""

from __future__ import annotations

import contextlib
import secrets
import socket
import threading

import pytest

from common import create_actor_and_register
from common.launch_harness import make_launch_harness
from conftest import _serve_spa_proxy, _trust_seeder, get_available_apps
from helpers import budgets
from helpers.app_launch import app_launch_config
from helpers.waiting import await_session_actor, session_generation, wait_until

# App markers sit PER-TEST (the native test carries tui, linux, android, macos,
# ios and windows, the web test carries web), as in ``test_version_mismatch_launch.py``: a module-wide
# union would hand ``--app linux`` the web test, which needs a browser.
pytestmark = pytest.mark.tier_3

#: The apps that publish ``launch_verdicts`` and are driven through the native
#: launch harness (a seeded credential store, then a cold launch).
NATIVE_APPS = ("tui", "linux", "android", "macos", "ios", "windows")

#: The cross-app state key (``fauna_e2e_agent::LAUNCH_VERDICTS_KEY``).
LAUNCH_VERDICTS_KEY = "launch_verdicts"
#: ui.yaml § onboarding — the surfaces a rendered stale verdict would paint.
HANDLE_INPUT = "handle-input"
LAUNCH_TRANSIENT_ERROR = "launch-transient-error"


class _HeldNest:
    """A loopback listener that accepts every connection and answers nothing
    until :meth:`release` closes them all (and itself) — the launch's silent
    challenge stays in flight exactly as long as the test wants, then fails at
    once as a transport fault. Only the test's own sockets: no nest, no child."""

    def __init__(self, port: int = 0) -> None:
        self._listener = socket.socket()
        self._listener.bind(("127.0.0.1", port))
        self._listener.listen(16)
        self.url = f"http://127.0.0.1:{self._listener.getsockname()[1]}"
        self.dialed = threading.Event()
        #: Set once a connection's request line is the anonymous WS-RPC upgrade
        #: (``GET /api/v1/ws``) — the silent challenge's own dial, which web's
        #: SPA proxy replays verbatim. The web arm waits on this rather than on
        #: ``dialed``, because a browser page may reach its origin's ``/api/``
        #: for other reasons first.
        self.challenge_dialed = threading.Event()
        self._held: list[socket.socket] = []
        self._lock = threading.Lock()
        self._closed = False
        self._thread = threading.Thread(target=self._accept, daemon=True)
        self._thread.start()

    def _accept(self) -> None:
        while True:
            try:
                conn, _ = self._listener.accept()
            except OSError:
                return
            with self._lock:
                if self._closed:
                    conn.close()
                    return
                self._held.append(conn)
            self.dialed.set()
            threading.Thread(target=self._read_request_line, args=(conn,), daemon=True).start()

    def _read_request_line(self, conn: socket.socket) -> None:
        """Read the connection's first line (never answering it). The socket
        stays held; :meth:`release` closes it under this read."""
        buf = b""
        try:
            while b"\r\n" not in buf and len(buf) < 8192:
                chunk = conn.recv(1024)
                if not chunk:
                    return
                buf += chunk
        except OSError:
            return
        if buf.split(b"\r\n", 1)[0].startswith(b"GET /api/v1/ws"):
            self.challenge_dialed.set()

    def release(self) -> None:
        with self._lock:
            self._closed = True
            held, self._held = self._held, []
        for conn in held:
            with contextlib.suppress(OSError):
                conn.shutdown(socket.SHUT_RDWR)
            conn.close()
        self._listener.close()
        self._thread.join(timeout=5)


def _verdicts(driver) -> dict | None:
    value = driver.get_state(LAUNCH_VERDICTS_KEY)
    return value if isinstance(value, dict) else None


def _apps(*want: str) -> list[str]:
    """The wanted apps this run selected (``--app``), as in
    ``test_version_mismatch_launch._apps``."""
    available = get_available_apps()
    return [a for a in want if a in available]


@pytest.fixture
def launch_harness(request, tmp_path):
    """The native launch harness for ``request.param``. ``file_backed=True``
    keeps linux off its default real Secret Service (headless-safe, as
    ``test_version_mismatch_launch._launch_pointed_at``); tui is file-backed
    either way."""
    app = request.param
    harness = make_launch_harness(
        app, tmp_path=tmp_path, **app_launch_config(app, request),
        file_backed=True, seed_trust=_trust_seeder(request),
    )
    try:
        yield harness
    finally:
        harness.teardown()


def _supersede_the_held_launch(app, driver, held, nest_instance, *, node_url, before_patch=None):
    """The witness proper, once ``driver``'s launch is provably in flight against
    ``held``: build the newer session by the e2e session patch (naming
    ``node_url``, the newer session's nest as this app reaches it), release the
    held launch, and assert its verdict was dropped, not rendered. ``before_patch``
    runs just ahead of the patch (android's reverse for the newer nest)."""
    baseline = _verdicts(driver)
    assert baseline is not None, (
        f"{app} does not publish `{LAUNCH_VERDICTS_KEY}` — the barrier this "
        "witness rests on is missing (convention 11: refuse, never pass)"
    )
    assert baseline == {"routed": 0, "superseded": 0}, (
        f"a verdict was delivered before the held launch was released: {baseline}"
    )

    # The newer session: the e2e session patch, a registered identity on the
    # real nest, built while the launch above is still in flight.
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    newer_actor = actor["actor_id_hex"]
    if before_patch is not None:
        before_patch()
    driver.set_state({"session": {
        "authenticated": True,
        "node_url": node_url,
        "secret_hex": bytes(actor["signing_key"]).hex(),
        "actor_id": newer_actor,
        "handle": actor["handle"] or "",
    }})
    await_session_actor(
        driver, newer_actor, budget_s=budgets.APP_RELAUNCH_S, what="the session patch"
    )
    generation = session_generation(driver)
    assert generation is not None, f"{app} publishes `session_generation`"

    # Let the superseded launch's verdict land, and wait until the app has
    # accounted for it — rendered or dropped — before asserting anything.
    held.release()
    settled = wait_until(
        lambda: (v := _verdicts(driver)) and v["routed"] + v["superseded"] >= 1 and v,
        budgets.APP_RELAUNCH_S,
        diagnose=lambda: f"no verdict was ever delivered; last={_verdicts(driver)!r}",
    )

    assert settled == {"routed": 0, "superseded": 1}, (
        f"the superseded launch's verdict was RENDERED over the newer session: {settled}"
    )
    assert driver.get_state("session.authenticated") is True
    assert driver.get_state("session.actor_id") == newer_actor, (
        "the newer session must still be the patched identity's"
    )
    assert session_generation(driver) == generation, (
        "the dropped verdict tore the newer session down"
    )
    assert driver.is_absent(HANDLE_INPUT), "no wizard over the newer session"
    assert driver.is_absent(LAUNCH_TRANSIENT_ERROR), (
        "the stale transport failure must not paint the retry surface"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("multiple-accounts")
@pytest.mark.parametrize("launch_harness", _apps(*NATIVE_APPS), indirect=True)
def test_a_superseded_launch_verdict_never_tears_down_the_newer_session(
    request, nest_instance, launch_harness
):
    app = request.node.callspec.params["launch_harness"]
    if app == "android":
        from drivers.port_util import find_free_port

        held_port = find_free_port()
        held = _HeldNest(held_port)
    else:
        held = _HeldNest()
    try:
        # The stored identity's launch dials the held nest and waits there.
        driver = launch_harness.launch_and_route(
            secret_hex=secrets.token_hex(32), node_url=held.url, trust=None
        )
        assert held.dialed.wait(budgets.APP_RELAUNCH_S), (
            "the launch never dialed its stored nest — nothing was in flight to supersede"
        )
        before_patch = None
        if app == "android":
            # The device's loopback reaches only reversed ports, and this nest
            # was never in the launch config.
            def before_patch():
                driver.ensure_reverse(nest_instance["port"])
        _supersede_the_held_launch(
            app, driver, held, nest_instance, node_url=nest_instance["url"],
            before_patch=before_patch,
        )
    finally:
        held.release()


@pytest.mark.web
@pytest.mark.feature("multiple-accounts")
def test_a_superseded_launch_verdict_never_tears_down_the_newer_session_web(
    nest_instance, static_dir, spa_url
):
    """The web arm. The SPA proxy in front of the held listener is the page's
    origin AND the stored identity's ``nest_url`` (a browser reaches a nest only
    through one), so the routed reload's silent challenge is replayed into the
    held listener and waits there. The session patch names the session
    ``spa_url`` proxy, in front of the real nest — a second harness origin the
    SPA dials cross-origin, as any web session on another nest does. The patch is
    applied with no reload, which is exactly why the launch survives it."""
    held = _HeldNest()
    server = None
    harness = None
    try:
        proxy_url, server = _serve_spa_proxy(static_dir, held.url)
        harness = make_launch_harness("web", spa_url=proxy_url)
        driver = harness.launch_and_route(
            secret_hex=secrets.token_hex(32), node_url=proxy_url, trust=None
        )
        # Fresh web drivers compile wasm cold before the launch dials (the
        # identity-pin web precedent's ceiling).
        assert held.challenge_dialed.wait(max(budgets.APP_RELAUNCH_S, 60)), (
            "the web launch never opened its silent challenge against the stored "
            "nest — nothing was in flight to supersede"
        )
        _supersede_the_held_launch("web", driver, held, nest_instance, node_url=spa_url)
    finally:
        held.release()
        if harness is not None:
            harness.teardown()
        if server is not None:
            server.shutdown()

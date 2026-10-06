"""tier_1: the R14 (account-data-plane.md § The ratified decisions) escrow-trust seed is ON BY DEFAULT for every app launch.

The regression this pins (found 2026-08-17, while building the plane's first
app-level consumer — `test_custody_ceremony_journey.py`):
a plaintext e2e nest can never graduate the TLS channel-binding pin that
`trust::trusted_escrow_holders` reads, so the trust set was empty in EVERY
app-e2e run ever made — no generation tip resolved, every fleet-only sealed
write refused, `device_endpoints: Unmintable` in every pump report. The whole
R14 plane was dormant fleet-wide and nothing went red, because honest refusal
IS the fail-safe design. That is the shape this file exists to keep out: a
dormant plane is SILENT, so only a structural assertion notices it coming back.

Contract: `docs/goal/architecture/e2e-automation-surface-gating.md` § The e2e
trust seed. The end-to-end witness that the seeded plane actually mints is
`test_custody_ceremony_journey.py` (tier_3, a generation-sealed write through
the default seed); this file is the cheap ratchet underneath it — it spawns
nothing and builds nothing, so a dropped wiring is caught in milliseconds
rather than in a 300 s pump pass nobody runs on the inner loop.

Four surfaces:

  1. `_apply_r14_trust_env`'s own decisions — the `no_r14_trust` opt-out, the
     portless (`live_nest`-marker) skip, and the `--nest live:URL` mode skip
     (which does NOT go portless — see `test_live_mode_with_a_declared_port_
     is_skipped_not_dialed`), all without touching a nest.
  2. Every app branch of `_build_app_config` that builds a launch environment
     CALLS it. Hand-wired per branch, so a new app — or a branch someone
     rewrites — drops out of the seed silently; the call site is the contract.
  3. A declared absence stays declared: an app that reaches no environment is
     named in DECLARED_ABSENCES rather than left to look like the same silent
     gap this file pins against. None remains — web (wasm, no `std::env::var`)
     now seeds through its origin's localStorage once its SPA hosts the account
     runtime, and android (Intent launch, no process env) forwards the key
     through BridgeHttpServer.kt::launchApp() +
     MainActivity's Os.setenv re-export — both are SEEDED_APPS entries.
  4. The seed is KEYED by the nest it was read from — `<nest url>=<nest_id>`
     entries — because the TOFU pin it stands in for is per nest authority.
     The unkeyed seed trusted the launch nest's key for EVERY nest, so an app
     pointed at a dedicated nest ran each generation mint there to a
     post-deposit refusal ("the escrow receipt is signed by a holder this
     account does not trust") — the dormant plane again, only louder in the
     log and just as silent in the result. The reading side is
     `trust::trusted_escrow_holders`, whose unit tests pin the same grammar.
     A previous release's binary older than that grammar is seeded in the
     bare form it reads, decided by ancestry.

Apps a test launches itself, outside `_build_app_config`, are the twin file's:
`test_r14_trust_seed_self_launch.py`.
"""

import ast
import importlib
import inspect
from pathlib import Path
from types import SimpleNamespace

import pytest

import conftest

pytestmark = pytest.mark.tier_1

# The apps whose launch config carries an environment the app actually reads.
# web is wasm (no `std::env::var`): its driver writes the seed into the
# origin's localStorage instead, which the SPA-hosted account runtime reads.
# android was a declared absence (Intent launch — only keys
# BridgeHttpServer.kt::launchApp() forwards arrive) until it started forwarding
# this one key. No app is a declared absence now; a
# future one lands in DECLARED_ABSENCES.
SEEDED_APPS = ("ios", "macos", "linux", "windows", "tui", "android", "web")
DECLARED_ABSENCES = ()


class _Item:
    def __init__(self, markers=()):
        self._markers = set(markers)

    def get_closest_marker(self, name):
        return SimpleNamespace(name=name) if name in self._markers else None


def _request(items):
    return SimpleNamespace(session=SimpleNamespace(items=list(items)))


# ── 1. the helper's own decisions ────────────────────────────────────────────


def test_opt_out_marker_leaves_the_world_unseeded():
    env = {}
    conftest._apply_r14_trust_env(
        env,
        {"port": 1, "url": "http://127.0.0.1:1"},
        _request([_Item(markers={"no_r14_trust"}), _Item(markers={"tier_3"})]),
    )
    assert "FAUNA_E2E_TRUST_NEST_IDENTITY" not in env, (
        "a `no_r14_trust` test in the selection must leave the trust set "
        "empty — that marker exists precisely to assert the fail-safe posture"
    )


def test_portless_nest_stub_is_skipped_not_an_error():
    # A `live_nest` session's stub carries only ["url"]: a real TLS nest
    # graduates a real pin, which is the production path the seed stands in
    # for. Reaching for nest.info here would also start the local nest the
    # live-session gate exists to avoid (test_live_nest_session_gate.py).
    env = {}
    conftest._apply_r14_trust_env(
        env, {"url": "https://live.example"}, _request([_Item()])
    )
    assert "FAUNA_E2E_TRUST_NEST_IDENTITY" not in env


def test_live_mode_with_a_declared_port_is_skipped_not_dialed(monkeypatch):
    # Regression (found 2026-08-27): unlike the portless
    # `live_nest`-marker stub above, `--nest live:URL` mode's `_LiveProvider`
    # DOES declare a `port` (443, its own real HTTPS port — a capability, not
    # an absence), so the `if not port: return` guard alone let this call
    # through and dial `ws_api.nest_info(443)` against `127.0.0.1` instead of
    # the live box — `ConnectionRefusedError` on every app-fixture test in
    # every live-mode run since this seed went default-on (2026-08-17),
    # undetected because no live sweep ran in that window. The skip must key
    # off the MODE, not off `port`'s truthiness.
    from helpers import nest_mode as nest_mode_mod
    from tests.api import ws_api

    monkeypatch.setattr(
        nest_mode_mod, "run_mode", lambda: nest_mode_mod.NestMode(nest_mode_mod.LIVE)
    )
    dialed = []
    monkeypatch.setattr(
        ws_api,
        "nest_info",
        lambda port: dialed.append(port) or {"nest_id": "ab" * 32},
    )
    env = {}
    conftest._apply_r14_trust_env(
        env, {"port": 443, "url": "https://example.com"}, _request([_Item()])
    )
    assert "FAUNA_E2E_TRUST_NEST_IDENTITY" not in env
    assert dialed == [], (
        "the live-mode skip must short-circuit before nest.info is ever "
        "dialed — a live box's port is not a 127.0.0.1 port this harness "
        "could reach"
    )


# ── 2. every environment-carrying app branch calls it ────────────────────────


def _branch_calls(func, callee: str) -> set[str]:
    """App names whose `app_name ==` branch in `func` contains a `callee` call.

    Reads the source rather than running it: `_build_app_config` builds real
    apps and resolves real fixtures, so calling it would be a build, not a
    unit test. The branch structure IS what's being pinned.
    """
    tree = ast.parse(inspect.getsource(func))
    calls: set[str] = set()

    def app_names(test) -> set[str]:
        """The string literals an `app_name == "..."` test in `test` names."""
        out = set()
        for node in ast.walk(test):
            if (
                isinstance(node, ast.Compare)
                and isinstance(node.left, ast.Name)
                and node.left.id == "app_name"
            ):
                for c in node.comparators:
                    if isinstance(c, ast.Constant) and isinstance(c.value, str):
                        out.add(c.value)
        return out

    def calls_callee(node) -> bool:
        return any(
            isinstance(n, ast.Call)
            and isinstance(n.func, ast.Name)
            and n.func.id == callee
            for n in ast.walk(node)
        )

    def walk(stmts, names: set[str]) -> None:
        # `elif` is an `If` inside `orelse`, so branches must be dispatched on
        # the STATEMENT, never on a container's children — walking children
        # loses each elif's own `app_name ==` test (and with it every branch
        # past the first, which is how this walker's first draft reported
        # linux/macos/tui as unseeded when they were wired all along).
        for stmt in stmts:
            if isinstance(stmt, ast.If):
                walk(stmt.body, app_names(stmt.test) or names)
                walk(stmt.orelse, names)
                continue
            if names and calls_callee(stmt):
                calls.update(names)
            walk(
                [c for c in ast.iter_child_nodes(stmt) if isinstance(c, ast.stmt)],
                names,
            )

    walk(tree.body, set())
    return calls


def test_every_seeded_app_branch_applies_the_trust_env():
    seeded = _branch_calls(conftest._build_app_config, "_apply_r14_trust_env")
    missing = set(SEEDED_APPS) - seeded
    assert not missing, (
        f"_build_app_config branches {sorted(missing)} build a launch "
        "environment but never call `_apply_r14_trust_env`, so those apps "
        "launch with an EMPTY R14 trust set: no generation tip resolves and "
        "every fleet-only sealed write refuses — silently, because refusal is "
        "the fail-safe design. Add the call to the branch (see the seeded "
        "siblings), or make the app a DECLARED absence with its reason."
    )


def test_the_declared_absence_is_still_absent():
    # Not a wish — a statement of the mechanism. If an app declared absent here
    # grows an environment path, seed it and move it into SEEDED_APPS in the
    # same change.
    seeded = _branch_calls(conftest._build_app_config, "_apply_r14_trust_env")
    unexpected = set(DECLARED_ABSENCES) & seeded
    assert not unexpected, (
        f"{sorted(unexpected)} now seeds the R14 trust — good, but the "
        "declared absence in `_apply_r14_trust_env`'s docstring and in "
        "e2e-automation-surface-gating.md § The e2e trust seed must move with "
        "it, and the app must be added to SEEDED_APPS here"
    )


# ── 3. the seed reads the identity a client would PIN ────────────────────────


def test_seed_is_the_nest_info_nest_id(monkeypatch):
    # nest.info's `nest_id` IS the nest identity clients pin
    # (`discovery_core::nest_info_core`: `state.nest_identity.public_key_bytes()`).
    # Anything else here would seed a trust anchor no production client would
    # ever hold, which makes the whole plane's e2e coverage a fiction.
    from tests.api import ws_api

    nest_id = "ab" * 32
    seen = []
    monkeypatch.setattr(
        ws_api,
        "nest_info",
        lambda port: seen.append(port) or {"nest_id": nest_id},
    )
    env = {}
    conftest._apply_r14_trust_env(
        env, {"port": 4242, "url": "http://127.0.0.1:4242"}, _request([_Item()])
    )
    assert env["FAUNA_E2E_TRUST_NEST_IDENTITY"] == f"http://127.0.0.1:4242={nest_id}"
    assert seen == [4242]


def test_the_identity_is_re_read_per_launch(monkeypatch):
    # Deliberately NOT cached by port: `test_nest_rotation_*` rotates the nest
    # identity mid-session, and a cache would hand a later app launch the
    # identity of a nest that no longer exists — seeding trust in a key nobody
    # holds, which fails exactly like no seed at all but looks like coverage.
    from tests.api import ws_api

    ids = iter(["ab" * 32, "cd" * 32])
    monkeypatch.setattr(ws_api, "nest_info", lambda port: {"nest_id": next(ids)})
    nest = {"port": 4242, "url": "http://127.0.0.1:4242"}
    first, second = {}, {}
    conftest._apply_r14_trust_env(first, nest, _request([_Item()]))
    conftest._apply_r14_trust_env(second, nest, _request([_Item()]))
    assert _entries(first) == {"http://127.0.0.1:4242": "ab" * 32}
    assert _entries(second) == {"http://127.0.0.1:4242": "cd" * 32}


# ── 4. the seed is keyed by the nest it names ────────────────────────────────


def _entries(env) -> dict:
    """The seed as `{nest url: nest_id}` — the grammar the one door parses
    (`trust::trusted_escrow_holders`): `<nest url>=<64 hex>` entries joined by
    `,`, the key normalized there with `authority_of`."""
    return dict(
        entry.rsplit("=", 1)
        for entry in env["FAUNA_E2E_TRUST_NEST_IDENTITY"].split(",")
    )


def test_a_second_nest_joins_the_seed_rather_than_replacing_it(monkeypatch):
    # A launch that will talk to two nests (a seat on the session nest that a
    # test then points at a dedicated one) carries one entry per nest. Each
    # nest's key is trusted FOR THAT NEST only — never the other's, which is
    # the whole difference between this and the flat set the door refuses.
    from tests.api import ws_api

    ids = {4242: "ab" * 32, 5353: "cd" * 32}
    monkeypatch.setattr(ws_api, "nest_info", lambda port: {"nest_id": ids[port]})
    env = {}
    conftest._apply_r14_trust_env(
        env, {"port": 4242, "url": "http://127.0.0.1:4242"}, _request([_Item()])
    )
    conftest._apply_r14_trust_env(
        env, {"port": 5353, "url": "http://127.0.0.1:5353"}, _request([_Item()])
    )
    assert _entries(env) == {
        "http://127.0.0.1:4242": "ab" * 32,
        "http://127.0.0.1:5353": "cd" * 32,
    }


def test_re_reading_a_nest_replaces_its_own_entry(monkeypatch):
    # The per-launch re-read above, applied twice into ONE environment: the
    # fresher read replaces the nest's entry instead of leaving a stale key
    # beside it (a rotated nest must not keep trusting its predecessor).
    from tests.api import ws_api

    ids = iter(["ab" * 32, "cd" * 32])
    monkeypatch.setattr(ws_api, "nest_info", lambda port: {"nest_id": next(ids)})
    nest = {"port": 4242, "url": "http://127.0.0.1:4242"}
    env = {}
    conftest._apply_r14_trust_env(env, nest, _request([_Item()]))
    conftest._apply_r14_trust_env(env, nest, _request([_Item()]))
    assert _entries(env) == {"http://127.0.0.1:4242": "cd" * 32}


def _grammar_env(monkeypatch, *, reads_keyed, **kwargs) -> tuple[dict, list]:
    """The seed written for a build whose ancestry answers ``reads_keyed``."""
    from helpers import prev_build
    from tests.api import ws_api

    monkeypatch.setattr(ws_api, "nest_info", lambda port: {"nest_id": "ab" * 32})
    asked: list = []
    monkeypatch.setattr(
        prev_build, "_is_ancestor", lambda commit, of: asked.append((commit, of)) or reads_keyed
    )
    env: dict = {}
    conftest._apply_r14_trust_env(
        env, {"port": 4242, "url": "http://127.0.0.1:4242"}, _request([_Item()]), **kwargs
    )
    return env, asked


def test_a_build_older_than_the_keyed_grammar_is_seeded_bare(monkeypatch):
    # The version-skew grid launches the pinned previous release. A build from
    # before the keyed grammar reads the variable as ONE bare identity and
    # ignores a keyed entry, while today's reader honours a bare identity for no
    # nest — so the grammar follows the binary, and ancestry decides it.
    from helpers import prev_build

    env, asked = _grammar_env(monkeypatch, reads_keyed=False, build_commit="0123abcd")
    assert env["FAUNA_E2E_TRUST_NEST_IDENTITY"] == "ab" * 32
    assert asked == [(prev_build.KEYED_TRUST_SEED_COMMIT, "0123abcd")]


def test_a_build_that_reads_the_keyed_grammar_is_seeded_keyed(monkeypatch):
    # Once the pin advances past the keyed grammar, the bare arm is gone with no edit.
    env, _ = _grammar_env(monkeypatch, reads_keyed=True, build_commit="0123abcd")
    assert _entries(env) == {"http://127.0.0.1:4242": "ab" * 32}


def test_the_working_trees_own_build_is_always_keyed(monkeypatch):
    env, asked = _grammar_env(monkeypatch, reads_keyed=False)
    assert _entries(env) == {"http://127.0.0.1:4242": "ab" * 32}
    assert asked == [], "a launch of this checkout's build never asks ancestry"


def test_the_keyed_grammar_commit_is_in_this_history():
    # A mistyped constant would make every ancestry answer an error, and a commit
    # outside this history would read as "never keyed" forever.
    from helpers import prev_build

    assert prev_build._is_ancestor(prev_build.KEYED_TRUST_SEED_COMMIT, "HEAD")


def test_an_unusable_nest_id_fails_loudly(monkeypatch):
    # The dormant plane's whole lesson: a missing trust anchor is SILENT
    # downstream (every sealed write refuses, honestly). So the one place it
    # can still be caught cheaply must shout rather than shrug.
    from tests.api import ws_api

    monkeypatch.setattr(ws_api, "nest_info", lambda port: {"nest_id": "short"})
    with pytest.raises(AssertionError, match="no usable nest_id"):
        conftest._apply_r14_trust_env(
            {}, {"port": 4242, "url": "http://127.0.0.1:4242"}, _request([_Item()])
        )


# ── 5. a nest the launch did not name is trusted only through a relaunch ─────
#
# An environment is fixed at launch, so a session-cached app that a fixture is
# about to point at a DEDICATED nest must first be relaunched with that nest
# named (`conftest._relaunch_trusting_nest`, called at the login seams). Without
# it the keyed seed leaves the app's trust set empty on the dedicated nest and
# the plane there is dormant — quietly.

LAUNCH_NEST = {"port": 4242, "url": "http://127.0.0.1:4242"}
DEDICATED_NEST = {"port": 5353, "url": "http://127.0.0.1:5353"}
LATER_NEST = {"port": 6464, "url": "http://127.0.0.1:6464"}
LAUNCHED_SEED = f"http://127.0.0.1:4242={'ab' * 32}"


class _FakeDriver:
    """The driver surfaces `_relaunch_trusting_nest` reads: the environment the
    next relaunch starts with, the relaunch, and the post-relaunch state wait."""

    def __init__(self, environment):
        self._environment = environment
        self.relaunches: list[dict] = []

    def relaunch_environment(self):
        return self._environment

    def recover(self):
        self.relaunches.append(dict(self._environment))
        return True

    def wait_for_state(self, predicate, timeout=30):
        return {}


def _serve_ids(monkeypatch, ids: dict) -> list:
    from tests.api import ws_api

    dialed: list = []
    monkeypatch.setattr(
        ws_api, "nest_info", lambda port: dialed.append(port) or {"nest_id": ids[port]}
    )
    return dialed


def test_the_nest_the_app_launched_against_needs_no_relaunch(monkeypatch):
    dialed = _serve_ids(monkeypatch, {4242: "ab" * 32})
    driver = _FakeDriver({"FAUNA_E2E_TRUST_NEST_IDENTITY": LAUNCHED_SEED})
    conftest._relaunch_trusting_nest(driver, LAUNCH_NEST)
    assert driver.relaunches == []
    assert dialed == [], "the launch nest is trusted as launched — no dial per login"


def test_a_dedicated_nest_relaunches_the_app_seeded_for_it(monkeypatch):
    _serve_ids(monkeypatch, {5353: "cd" * 32})
    driver = _FakeDriver(
        {"FAUNA_E2E_TRUST_NEST_IDENTITY": LAUNCHED_SEED, "FAUNA_CONV_POLL_SECS": "2"}
    )
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    assert len(driver.relaunches) == 1, (
        "trust is captured at launch, so trusting a nest the launch did not name "
        "takes a launch that names it"
    )
    relaunched = driver.relaunches[0]
    assert _entries(relaunched) == {
        "http://127.0.0.1:4242": "ab" * 32,
        "http://127.0.0.1:5353": "cd" * 32,
    }
    assert relaunched["FAUNA_CONV_POLL_SECS"] == "2", "the rest of the launch is kept"


def test_a_second_login_on_the_same_dedicated_nest_does_not_relaunch_again(monkeypatch):
    _serve_ids(monkeypatch, {5353: "cd" * 32})
    driver = _FakeDriver({"FAUNA_E2E_TRUST_NEST_IDENTITY": LAUNCHED_SEED})
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    assert len(driver.relaunches) == 1


def test_a_later_dedicated_nest_replaces_the_earlier_one(monkeypatch):
    # Rebuilt from the LAUNCH seed every time, never accumulated: a dedicated
    # nest is gone when its test ends, and its port may go to the next one.
    _serve_ids(monkeypatch, {5353: "cd" * 32, 6464: "ef" * 32})
    driver = _FakeDriver({"FAUNA_E2E_TRUST_NEST_IDENTITY": LAUNCHED_SEED})
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    conftest._relaunch_trusting_nest(driver, LATER_NEST)
    assert _entries(driver.relaunches[-1]) == {
        "http://127.0.0.1:4242": "ab" * 32,
        "http://127.0.0.1:6464": "ef" * 32,
    }


def test_a_new_nest_on_a_recycled_port_is_re_read(monkeypatch):
    ids = {5353: "cd" * 32}
    _serve_ids(monkeypatch, ids)
    driver = _FakeDriver({"FAUNA_E2E_TRUST_NEST_IDENTITY": LAUNCHED_SEED})
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    ids[5353] = "99" * 32  # the first nest is gone; another took its port
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    assert len(driver.relaunches) == 2
    assert _entries(driver.relaunches[-1])["http://127.0.0.1:5353"] == "99" * 32


def test_an_unseeded_session_is_never_seeded_by_a_re_point(monkeypatch):
    # `no_r14_trust` and the live nest mode launch with no seed at all; pointing
    # the app somewhere else must not quietly seed the world they observe.
    dialed = _serve_ids(monkeypatch, {5353: "cd" * 32})
    driver = _FakeDriver({"FAUNA_CONV_POLL_SECS": "2"})
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    assert driver.relaunches == [] and dialed == []


def test_an_app_with_no_relaunchable_environment_is_left_alone(monkeypatch):
    # web reads no environment, and an app that cannot cold-relaunch (android)
    # has none to hand over: both answer `relaunch_environment()` with None.
    dialed = _serve_ids(monkeypatch, {5353: "cd" * 32})
    driver = _FakeDriver(None)
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    assert driver.relaunches == [] and dialed == []


class _FakeDeviceDriver(_FakeDriver):
    """A device app (android): it reaches a nest only through an `adb reverse`
    of the nest's port, and it cannot cold-relaunch (no environment)."""

    def __init__(self):
        super().__init__(None)
        self.reversed: list[int] = []

    def ensure_reverse(self, port):
        self.reversed.append(port)


def test_a_device_app_is_given_the_dedicated_nest_s_port_before_the_re_point(monkeypatch):
    # A nest started after the app launched was never `adb reverse`d, so the
    # device's own 127.0.0.1:<port> reaches nothing (testing.md § Android's run
    # venue, constraint 3). Every re-point seam calls this function first, so
    # it is the one place that opens the port — whether or not a relaunch
    # follows.
    dialed = _serve_ids(monkeypatch, {5353: "cd" * 32})
    driver = _FakeDeviceDriver()
    conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)
    assert driver.reversed == [5353]
    assert driver.relaunches == [] and dialed == []


def test_a_port_less_nest_opens_no_reverse(monkeypatch):
    _serve_ids(monkeypatch, {})
    driver = _FakeDeviceDriver()
    conftest._relaunch_trusting_nest(driver, {"url": "https://live.example"})
    assert driver.reversed == []


def test_a_relaunch_that_fails_fails_the_login_loudly(monkeypatch):
    _serve_ids(monkeypatch, {5353: "cd" * 32})
    driver = _FakeDriver({"FAUNA_E2E_TRUST_NEST_IDENTITY": LAUNCHED_SEED})
    driver.recover = lambda: False
    with pytest.raises(AssertionError, match="relaunch"):
        conftest._relaunch_trusting_nest(driver, DEDICATED_NEST)


def _calls_before_first(func, callee: str, anchor: str) -> bool:
    """Whether `func` calls `callee` on a line before its first `anchor` call."""
    lines = inspect.getsource(func).splitlines()
    callee_at = next((i for i, ln in enumerate(lines) if f"{callee}(" in ln), None)
    anchor_at = next((i for i, ln in enumerate(lines) if f".{anchor}(" in ln), None)
    return callee_at is not None and anchor_at is not None and callee_at < anchor_at


def test_every_driver_names_the_environment_its_next_relaunch_reads():
    # Not a table of answers: each driver's own relaunch mechanism, exercised
    # without launching anything. A driver that relaunched from somewhere this
    # does not return would take the trust seed's relaunch and come back
    # unchanged — seeded for nothing new, and silent about it.
    from drivers.android import AndroidBridgeDriver
    from drivers.ios import IosInProcessDriver
    from drivers.linux import LinuxBridgeDriver
    from drivers.macos import MacosInProcessDriver
    from drivers.tui import TuiDriver
    from drivers.web import WebBridgeDriver
    from drivers.windows import WindowsBridgeDriver

    environment = {"FAUNA_E2E_TRUST_NEST_IDENTITY": LAUNCHED_SEED}
    for cls in (TuiDriver, LinuxBridgeDriver, MacosInProcessDriver, IosInProcessDriver):
        driver = object.__new__(cls)
        driver._launch_config = {"environment": environment}
        assert driver.relaunch_environment() is environment, (
            f"{cls.__name__}.recover() relaunches from `_launch_config`"
        )

    windows = object.__new__(WindowsBridgeDriver)
    windows._session_body = {"environment": environment}
    assert windows.relaunch_environment() is environment, (
        "windows' recover() re-POSTs `_session_body`, so that body is its answer"
    )
    windows._session_body = None
    assert windows.relaunch_environment() is None, "never launched: nothing to relaunch"

    assert object.__new__(WebBridgeDriver).relaunch_environment() is None, (
        "a browser page reads no environment"
    )
    android = object.__new__(AndroidBridgeDriver)
    android._launch_config = {"environment": environment}
    assert android.relaunch_environment() is None, (
        "android cannot cold-relaunch (test_module_relaunch.py pins it), so it "
        "has no relaunch a seed could ride"
    )


@pytest.mark.parametrize(
    "seam", ["_login_app_as", "_login_admin_as", "handled_logged_in_app"]
)
def test_the_login_seams_relaunch_before_they_point_the_app_at_a_nest(seam):
    func = getattr(conftest, seam)
    func = getattr(func, "__wrapped__", func)
    assert _calls_before_first(func, "_relaunch_trusting_nest", "set_state"), (
        f"`{seam}` points the session-cached app at a nest it may not have "
        "launched against, so it must call `_relaunch_trusting_nest` BEFORE its "
        "`set_state` — a relaunch after the login would discard the session it "
        "just built, and none at all leaves the app's trust set empty there"
    )


# ── 6. the dedicated-nest witness reads the tip-sealed form, not any row ─────


def test_the_witness_counts_the_form_rust_seals_under_a_generation_tip():
    # `helpers/trust_seed_witness.py` (the dedicated-nest and crash-journey
    # witnesses) decides "a tip resolved" from the first byte of an envelope the
    # nest echoes. That byte is a wire fact Rust
    # owns, and a harness copy nothing compares drifts into a witness counting
    # the wrong form — while the gen-0 form fills the fleet plane on every first
    # pass, which is how a first draft of that file went green on a dormant nest.
    import re
    from pathlib import Path

    from common.auth import SEALED_ENTRY_V2, is_generation_sealed

    source = (
        Path(conftest.__file__).resolve().parents[2]
        / "libs/fauna-core/src/account_entry_crypto.rs"
    ).read_text()
    forms = {
        name: int(value)
        for name, value in re.findall(
            r"pub const (SEALED_ENTRY_V\d): u8 = (\d+);", source
        )
    }
    assert forms.get("SEALED_ENTRY_V2") == SEALED_ENTRY_V2, forms
    assert is_generation_sealed({"entry": bytes([forms["SEALED_ENTRY_V2"], 7, 7])})
    assert not is_generation_sealed({"entry": bytes([forms["SEALED_ENTRY_V1"], 7, 7])})
    assert not is_generation_sealed({"entry": None})


# ── 7. a crash journey relaunches seeded before it first points the app ──────


_CRASH_JOURNEYS = "test_crash_recovery_journeys.py"

# How a crash journey first points its app at its crash nest: the injected admin
# session, the onboarding walk that claims it (the mid-claim journey), or a full
# sign-in (the two-seat removal journey).
_CRASH_POINTING_CALLS = frozenset({
    "inject_admin_session", "navigate_to_status", "sign_in",
})

# What ends the app (or the nest) a crash journey then observes. A relaunch after
# one of these is a fresh launch over the very state the journey tests.
_CRASH_KILL_CALLS = frozenset({"kill_uncleanly", "stop_nest", "hard_reload", "recover"})


# Fixtures that hand a journey an app LAUNCHED against the nest it will use, on
# the standard launch config (`_launch_fresh_share_driver` → `_build_app_config`,
# which seeds that nest's escrow trust). Such a journey never re-points a
# session app at a crash nest, so there is no seed to arrive by relaunch and no
# pointing call to anchor on — the seed was in the launch environment all along.
_SELF_SEEDED_APP_FIXTURES = frozenset({"folder_share_owner_app"})


def _call_name(call: ast.Call) -> str | None:
    if isinstance(call.func, ast.Attribute):
        return call.func.attr
    if isinstance(call.func, ast.Name):
        return call.func.id
    return None


def _first_call(func: ast.FunctionDef, names) -> ast.Call | None:
    calls = [n for n in ast.walk(func) if isinstance(n, ast.Call) and _call_name(n) in names]
    return min(calls, key=lambda n: (n.lineno, n.col_offset), default=None)


def _fixture_named_by(func: ast.FunctionDef, arg: ast.expr) -> str | None:
    """The fixture parameter ``arg`` names — directly, or through one
    ``nest = crash_nest`` alias in the journey's body."""
    if not isinstance(arg, ast.Name):
        return None
    params = {a.arg for a in func.args.args}
    if arg.id in params:
        return arg.id
    for node in ast.walk(func):
        if (
            isinstance(node, ast.Assign)
            and isinstance(node.value, ast.Name)
            and node.value.id in params
            and any(isinstance(t, ast.Name) and t.id == arg.id for t in node.targets)
        ):
            return node.value.id
    return None


def test_every_crash_journey_trusts_its_crash_nest_before_the_first_kill():
    # A crash journey points the session app at a per-test crash nest its launch
    # did not name, so the seed for that nest can only arrive by a relaunch — and
    # a relaunch is a fresh launch (convention 10). After the crash it would erase
    # the on-disk state the journey exists to test, so it comes BEFORE the journey
    # first points the app there; the kill's own relaunch then re-reads the seeded
    # environment (`test_every_driver_names_the_environment_its_next_relaunch_
    # reads`). Anchored on the journey's own calls, not on `.set_state(`, which
    # sits inside `inject_admin_session` where a source-order check of the journey
    # cannot see it.
    path = Path(__file__).with_name(_CRASH_JOURNEYS)
    tree = ast.parse(path.read_text(), filename=str(path))
    journeys = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name.startswith("test_")
    ]
    assert journeys, f"{_CRASH_JOURNEYS} defines no journeys — has it moved?"

    wrong = []
    for journey in journeys:
        if {a.arg for a in journey.args.args} & _SELF_SEEDED_APP_FIXTURES:
            continue
        relaunch = _first_call(journey, {"_relaunch_trusting_nest"})
        pointing = _first_call(journey, _CRASH_POINTING_CALLS)
        kill = _first_call(journey, _CRASH_KILL_CALLS)
        if pointing is None:
            wrong.append(
                f"{journey.name}: calls none of {sorted(_CRASH_POINTING_CALLS)} — "
                "name the call that points its app at its nest in _CRASH_POINTING_CALLS"
            )
        elif relaunch is None:
            wrong.append(f"{journey.name}: never calls `_relaunch_trusting_nest`")
        elif relaunch.lineno >= pointing.lineno or (
            kill is not None and relaunch.lineno >= kill.lineno
        ):
            wrong.append(
                f"{journey.name}: relaunches at :{relaunch.lineno}, not before its "
                f"first pointing call (:{pointing.lineno}) and first kill "
                f"(:{kill.lineno if kill else '-'})"
            )
        elif len(relaunch.args) != 2 or not (
            _fixture_named_by(journey, relaunch.args[1]) or ""
        ).endswith("crash_nest"):
            seeded = ast.unparse(relaunch.args[1]) if len(relaunch.args) > 1 else "?"
            wrong.append(f"{journey.name}: seeds `{seeded}`, not its crash-nest fixture")
    assert not wrong, (
        "every crash journey must call `_relaunch_trusting_nest(app.driver, "
        "<its crash nest>)` before it first points the app there and before any "
        "kill — without it the app trusts no escrow holder on the crash nest and "
        "its generation plane is dormant, silently:\n  " + "\n  ".join(wrong)
    )


# ── 8. the shared login/pointing helpers OUTSIDE conftest relaunch too ───────
#
# Section 5's pin covers conftest's own three seams. Every OTHER shared helper
# that points a session-cached app at a nest it may not have launched against owes the same ordering — one entry per helper, the
# anchor named for whatever call actually POINTS the app there: `set_state`
# for an injected session, but a UI claim's anchor is its `navigate_to_*` /
# `call_machine_method` call instead (`set_state` never appears in one).

#: (module, function, anchor-call-name). `module` is import-path-from-`tests/
#: e2e-unified` (so a bare `helpers.x` or a `tests.`-prefixed sibling test
#: module), matching every other lazy `from X import Y` in this codebase.
_CROSS_FILE_SEAMS = [
    ("helpers.mail_dedicated_nest", "login_as_nest_admin", "set_state"),
    ("helpers.e2e_session", "login_as", "set_state"),
    ("helpers.mail_client_ui", "claim_to_nat_mode_page", "navigate_to_status"),
    ("helpers.provisioning_drive", "point_providers_at", "set_provider_base_urls"),
    ("tests.scenarios.conftest", "make_cached_app", "set_state"),
    ("tests.test_bridges", "_login_to_bridges_nest", "set_state"),
    ("tests.test_trust_prompt", "_claim_to_nat_mode", "navigate_to_status"),
    (
        "tests.test_mail_enable_at_admin_claim",
        "_drive_admin_claim_to_logged_in",
        "navigate_to_status",
    ),
    ("tests.test_factory_reset_calendar_reclaim", "_login_admin_as_user", "set_state"),
]


@pytest.mark.parametrize(
    "module_name, func_name, anchor",
    _CROSS_FILE_SEAMS,
    ids=[f"{m}.{f}" for m, f, _ in _CROSS_FILE_SEAMS],
)
def test_the_shared_login_helpers_relaunch_before_they_point_the_app_at_a_nest(
    module_name, func_name, anchor
):
    module = importlib.import_module(module_name)
    func = getattr(module, func_name)
    func = getattr(func, "__wrapped__", func)
    assert _calls_before_first(func, "_relaunch_trusting_nest", anchor), (
        f"`{module_name}.{func_name}` points the session-cached app at a nest it "
        "may not have launched against, so it must call `_relaunch_trusting_nest` "
        f"BEFORE its `.{anchor}(` call — a relaunch after would discard the "
        "session/wizard state it just built, and none at all leaves the app's "
        "trust set empty there"
    )

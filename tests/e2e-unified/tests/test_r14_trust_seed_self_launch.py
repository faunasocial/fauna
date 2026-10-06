"""tier_1: an app a test launches ITSELF carries the e2e escrow-trust seed.

The self-launch twin of `test_r14_trust_seed_default.py`, whose second surface
pins the seed onto every app `conftest._build_app_config` launches. A test that
starts an app some other way — a second seat, a launch/relaunch harness, a
previous release's binary, a plain `subprocess.Popen` — builds its launch
environment itself, and nothing noticed when that environment named no nest:
the app ran with an empty escrow trust set, so no generation tip resolved and
every fleet-only sealed write refused, quietly, because refusal is the
fail-safe design. Contract: `docs/goal/architecture/e2e-automation-surface-gating.md`
§ The e2e trust seed ("for every app launch").

Read from the source — this file launches nothing:

  * A DRIVER LAUNCH is `<driver>.launch(<config>)`: exactly one positional
    argument and no keywords, the one shape every driver's `launch` takes. A
    wrapper with a signature of its own (`harness.launch(node_url=...)`,
    `home.launch(app_path, seed=True)`) is not one; the driver launch inside
    the wrapper is scanned where it happens. The drivers' own relaunches
    (`drivers/`, a `recover()` re-launching the config a scanned launch built)
    and the browser servers are not scanned.
  * It is SEEDED when a function around it calls the seed writer
    (`SEED_CALLS`), directly or through a function of the same module that
    does.
  * It needs no seed when its config is WEB-SHAPED: a dict literal with no
    `app_path`, which only the browser driver takes. web is the seed's one
    declared absence (wasm has no environment to read).
  * Nor when it is a RELAUNCH of the config a driver already launched with
    (`driver._launch_config`, or a name bound from it in the same function):
    it carries whatever seed that launch carried. Pointing a relaunch at a
    nest its first launch did not name is `conftest._relaunch_trusting_nest`'s
    job, not a new launch.
  * Anything else is declared in `UNSEEDED` with its reason, and a declaration
    that matches no unseeded launch is red too, so the table cannot outlive
    its code.
  * A PROCESS LAUNCH — an app binary started without a driver — has no shape a
    scan can tell from any other subprocess, so each is named
    (`PROCESS_LAUNCHES`) and must seed like a driver launch.

What the scan cannot see is a seeding call whose value never reaches the
launched config. `_build_app_config` seeds only a nest handle that carries a
port, and `helpers/sync_seats.py` once handed it a port-less stub; that flow,
the launch harness's and the previous-build grammar are pinned by the tests at
the bottom, which run the real helpers against fakes.
"""

from __future__ import annotations

import ast
import functools
import os
from dataclasses import dataclass
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

REPO = Path(__file__).resolve().parents[3]

#: Where self-launches live: the e2e tree and the shared helpers it imports.
SCANNED_ROOTS = ("tests/e2e-unified", "tests/common")

#: Directories whose `.launch(` is not a test starting an app: the drivers'
#: own relaunch of a config some scanned launch built, and the browser servers.
UNSCANNED_DIRS = frozenset({"drivers", "web-bridge", "web-notes-harness", "node_modules"})

#: The seed writer and the conftest functions that call it: the inline form a
#: self-launch builds its environment with, and `_build_app_config`, which calls
#: it in every app branch
#: (`test_r14_trust_seed_default.py::test_every_seeded_app_branch_applies_the_trust_env`).
WRITER = "_apply_r14_trust_env"
SEED_CALLS = frozenset({WRITER, "_seeded_environment", "_build_app_config"})

#: App binaries started without a driver: (file, function) → what it starts.
PROCESS_LAUNCHES = {
    (
        "tests/e2e-unified/tests/platform/linux/test_desktop_join.py",
        "desktop_app",
    ): "`subprocess.Popen` of the desktop binary",
    (
        "tests/e2e-unified/tests/real_session/test_sync_agent_flatpak_seam.py",
        "test_flatpak_seam_always_on_agent",
    ): "`flatpak run`, whose sandbox takes the seed only as `--env=`",
}

#: Driver launches that carry no seed, and why: (file, function) → reason.
UNSEEDED: dict[tuple[str, str], str] = {
    (
        "tests/common/launch_harness.py",
        "NativeLaunchHarness.launch",
    ): "seeds through the `seed_trust` writer a native harness cannot be built "
    "without; `test_the_launch_harness_seeds_each_launch_and_relaunch` pins it",
    (
        "tests/common/launch_harness.py",
        "NativeLaunchHarness.relaunch",
    ): "re-seeds through the same writer, pinned by the same test",
    (
        "tests/e2e-unified/helpers/instance_guard.py",
        "expect_launch_refused",
    ): "the instance guard refuses this launch: the process exits before any "
    "account runtime assembles, so there is no trust set to seed",
    (
        "tests/e2e-unified/helpers/photo_library_grant.py",
        "main",
    ): "names no nest: the human-run Photos grant launches the venue bundle only to "
    "raise the one-time TCC prompt — no nest starts and no account runtime is used",
    (
        "tests/e2e-unified/helpers/sync_seats.py",
        "AppSeatDriver.start",
    ): "launches the config `make_seat` built through `_build_app_config`; "
    "`test_an_app_sync_seat_builds_its_config_for_a_nest_with_a_port` pins the handle",
    (
        "tests/e2e-unified/tests/test_driver_relaunch_pin.py",
        "_windows_launch_and_mint",
    ): "a stubbed windows driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_driver_relaunch_pin.py",
        "test_a_relaunch_carries_the_install_device_secret_at_launch",
    ): "a stubbed driver (tier_1, linux/tui client via `launching_driver`): "
    "`subprocess.Popen`/`spawn_pty` is monkeypatched to a stand-in child, so "
    "no bridge and no app process start — same shape as the windows/android "
    "stub entries above",
    (
        "tests/e2e-unified/tests/test_driver_relaunch_pin.py",
        "test_the_secret_survives_every_later_relaunch",
    ): "a stubbed driver (tier_1, linux/tui client via `launching_driver`): "
    "no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_driver_relaunch_pin.py",
        "test_a_torn_install_device_secret_is_never_carried",
    ): "a stubbed driver (tier_1, linux/tui client via `launching_driver`): "
    "no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_driver_relaunch_pin.py",
        "test_a_launch_that_lost_its_secret_carries_none_forward",
    ): "a stubbed driver (tier_1, linux/tui client via `launching_driver`): "
    "no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_driver_relaunch_pin.py",
        "test_the_first_launch_carries_nothing",
    ): "a stubbed driver (tier_1, linux/tui client via `launching_driver`): "
    "no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_driver_relaunch_pin.py",
        "test_a_store_the_caller_owns_gets_no_carried_secret",
    ): "a stubbed driver (tier_1, linux/tui client via `launching_driver`): "
    "no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_windows_driver_data_dir_isolation.py",
        "_launch_env",
    ): "a stubbed windows driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_windows_driver_data_dir_isolation.py",
        "_launch_args",
    ): "the same stubbed windows driver (tier_1): the launch only records the "
    "session body's args, no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_macos_artifact_launch_mode.py",
        "test_an_unknown_launch_mode_is_refused",
    ): "the driver refuses the launch mode before any process starts",
    (
        "tests/e2e-unified/tests/test_private_secret_service.py",
        "test_the_driver_refuses_the_mode_without_a_caller_owned_bus",
    ): "the driver refuses `use_real_keyring` without a caller-owned bus before "
    "any process starts — the launch raises, so there is no app and no nest to seed",
    (
        "tests/e2e-unified/tests/test_flaui_input_lock_windows.py",
        "test_physical_input_lock_keeps_a_siblings_keystrokes_out_of_this_app",
    ): "names no nest: both apps are driven through onboarding to handle entry "
    "and no further",
    # A stubbed android driver (tier_1) against a fake `adb` on a private PATH
    # dir: `_wait_for_health` and `_post("/session")` are neutralized (module
    # docstring), so no bridge and no app process ever start — the same shape
    # as the windows entries above.
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_launch_reverses_every_nest_port",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_reverse_maps_the_port_to_itself",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_the_forward_is_still_opened_and_is_not_a_reverse",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_launch_installs_both_apks",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_the_reverse_precedes_the_app_launch",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_every_adb_call_carries_the_serial",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_without_a_serial_no_dash_s_is_passed",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_teardown_removes_every_reverse_it_opened",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_reverse_removal_is_not_keyed_on_the_forward_port",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_teardown_clears_the_record_so_a_second_teardown_is_quiet",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_ensure_reverse_is_idempotent_and_records_late_nests",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_an_empty_seed_is_posted_not_dropped",
    ): "a stubbed android driver (tier_1): the `/session` POST is captured by "
    "`bridge_posts`, so no bridge and no app process start — and the seed under "
    "test is the credential seed, not the trust seed",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_no_seed_key_posts_no_seed",
    ): "a stubbed android driver (tier_1): the `/session` POST is captured by "
    "`bridge_posts`, so no bridge and no app process start",
    # The venue pins of the same file: the same stubbed driver against the same
    # fake `adb`, with a remote-adb-server launch config.
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_every_adb_call_names_the_remote_server",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_without_an_adb_server_no_dash_l_is_passed",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_the_venue_forward_port_comes_from_the_fixed_range",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_two_venue_seats_get_different_forward_ports_and_give_them_back",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_a_venue_reverse_for_an_untunnelled_nest_port_is_refused",
    ): "a stubbed android driver (tier_1): no bridge and no app process start",
    (
        "tests/e2e-unified/tests/test_android_driver_adb.py",
        "test_a_venue_launch_pointed_at_an_untunnelled_nest_fails_before_the_app_starts",
    ): "a stubbed android driver (tier_1): the launch raises at the reverse, "
    "before the `/session` POST that would start an app",
}


# ── the scan ─────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class Launch:
    path: str
    function: str
    line: int
    seeded: bool
    web_shaped: bool
    relaunch: bool

    @property
    def needs_a_seed(self) -> bool:
        return not (self.seeded or self.web_shaped or self.relaunch)

    def __str__(self) -> str:
        return f"{self.path}:{self.line} ({self.function})"


def _is_driver_launch(node: ast.AST) -> bool:
    return (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "launch"
        and len(node.args) == 1
        and not isinstance(node.args[0], ast.Starred)
        and not node.keywords
    )


def _is_web_shaped(call: ast.Call) -> bool:
    config = call.args[0]
    if not isinstance(config, ast.Dict) or None in config.keys:
        return False
    keys = [k.value for k in config.keys if isinstance(k, ast.Constant)]
    return len(keys) == len(config.keys) and "app_path" not in keys


def _mentions_launch_config(node: ast.AST) -> bool:
    return any(isinstance(n, ast.Attribute) and n.attr == "_launch_config" for n in ast.walk(node))


def _is_relaunch(call: ast.Call, function: ast.AST | None) -> bool:
    config = call.args[0]
    if _mentions_launch_config(config):
        return True
    if not isinstance(config, ast.Name) or function is None:
        return False
    return any(
        isinstance(n, ast.Assign)
        and any(isinstance(t, ast.Name) and t.id == config.id for t in n.targets)
        and _mentions_launch_config(n.value)
        for n in ast.walk(function)
    )


def _called_names(node: ast.AST, writers: frozenset = SEED_CALLS) -> set[str]:
    """The calls in `node` a seed can be reached through: a bare name, a method
    on `self`/`cls`, or one of `writers` reached through a module
    (`conftest._apply_r14_trust_env`)."""
    names: set[str] = set()
    for n in ast.walk(node):
        if not isinstance(n, ast.Call):
            continue
        f = n.func
        if isinstance(f, ast.Name):
            names.add(f.id)
        elif isinstance(f, ast.Attribute) and (
            f.attr in writers
            or (isinstance(f.value, ast.Name) and f.value.id in ("self", "cls"))
        ):
            names.add(f.attr)
    return names


def _seeding_names(tree: ast.Module, writers: frozenset = SEED_CALLS) -> set[str]:
    """`writers` plus every function of the module that reaches one."""
    functions = [
        n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    ]
    calls = {id(f): _called_names(f, writers) for f in functions}
    seeding = set(writers)
    grew = True
    while grew:
        grew = False
        for f in functions:
            if f.name not in seeding and calls[id(f)] & seeding:
                seeding.add(f.name)
                grew = True
    return seeding


def launches_in(source: str, path: str) -> list[Launch]:
    """Every driver launch in `source`, and whether it is seeded or web-shaped."""
    tree = ast.parse(source, filename=path)
    seeding = _seeding_names(tree)
    found: list[Launch] = []

    def visit(node: ast.AST, qualname: list[str], enclosing: list[ast.AST]) -> None:
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                visit(child, [*qualname, child.name], [*enclosing, child])
                continue
            if isinstance(child, ast.ClassDef):
                visit(child, [*qualname, child.name], enclosing)
                continue
            if _is_driver_launch(child):
                found.append(
                    Launch(
                        path=path,
                        function=".".join(qualname) or "<module>",
                        line=child.lineno,
                        seeded=any(_called_names(f) & seeding for f in enclosing),
                        web_shaped=_is_web_shaped(child),
                        relaunch=_is_relaunch(child, enclosing[-1] if enclosing else None),
                    )
                )
            visit(child, qualname, enclosing)

    visit(tree, [], [])
    return found


def _find_function(tree: ast.Module, qualname: str) -> ast.AST | None:
    node: ast.AST = tree
    for part in qualname.split("."):
        node = next(
            (
                c
                for c in ast.iter_child_nodes(node)
                if isinstance(c, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
                and c.name == part
            ),
            None,
        )
        if node is None:
            return None
    return node


def _tree_files():
    for root in SCANNED_ROOTS:
        for dirpath, dirnames, filenames in os.walk(REPO / root):
            dirnames[:] = sorted(
                d for d in dirnames if d not in UNSCANNED_DIRS and not d.startswith((".", "__"))
            )
            for name in sorted(filenames):
                if name.endswith(".py"):
                    path = Path(dirpath) / name
                    yield path.relative_to(REPO).as_posix(), path


@functools.lru_cache(maxsize=1)
def _tree_launches() -> tuple[Launch, ...]:
    return tuple(
        launch
        for rel, path in _tree_files()
        for launch in launches_in(path.read_text(encoding="utf-8"), rel)
    )


# ── the ratchet ──────────────────────────────────────────────────────────────


def test_every_self_launch_seeds_the_nest_it_points_at():
    unseeded = [
        launch
        for launch in _tree_launches()
        if launch.needs_a_seed and (launch.path, launch.function) not in UNSEEDED
    ]
    assert not unseeded, (
        f"{len(unseeded)} app launch(es) start an app whose environment carries no "
        "escrow-trust seed, so the app trusts no escrow holder: no generation tip "
        "resolves and every fleet-only sealed write refuses, silently. Seed the "
        "launch through `conftest._apply_r14_trust_env(environment, <each nest it "
        "will use>, request)` before it starts, or declare it in UNSEEDED with "
        "why it needs none:\n  " + "\n  ".join(map(str, unseeded))
    )


def test_every_unseeded_declaration_still_names_an_unseeded_launch():
    live = {(launch.path, launch.function) for launch in _tree_launches() if launch.needs_a_seed}
    stale = sorted(set(UNSEEDED) - live)
    assert not stale, (
        "UNSEEDED declares launches that are gone or seeded now; drop them so the "
        f"table keeps saying only what is true: {stale}"
    )


@pytest.mark.parametrize(
    "site", sorted(PROCESS_LAUNCHES), ids=lambda site: site[1] if isinstance(site, tuple) else site
)
def test_every_named_process_launch_seeds(site):
    rel, qualname = site
    tree = ast.parse((REPO / rel).read_text(encoding="utf-8"), filename=rel)
    function = _find_function(tree, qualname)
    assert function is not None, (
        f"{rel} no longer defines {qualname}, which PROCESS_LAUNCHES names as "
        f"{PROCESS_LAUNCHES[site]}: follow the launch and rename the entry"
    )
    assert _called_names(function) & _seeding_names(tree), (
        f"{rel}::{qualname} starts {PROCESS_LAUNCHES[site]} without the escrow-trust "
        "seed in its environment; build the environment through "
        "`conftest._apply_r14_trust_env` and hand it to the process"
    )


# ── the scan tells the shapes apart ──────────────────────────────────────────

_UNSEEDED = """
def test_second_seat(nest_instance, tui_app_path):
    driver = create_driver("tui")
    driver.launch({"app_path": tui_app_path, "url": nest_instance["url"]})
"""

_NO_ENVIRONMENT_KEY = """
def test_fresh_device(app_path):
    create_driver("linux").launch({"app_path": app_path})
"""

_SEEDED = """
def test_second_seat(nest_instance, tui_app_path, request):
    environment = {}
    _apply_r14_trust_env(environment, nest_instance, request)
    create_driver("tui").launch(
        {"app_path": tui_app_path, "url": nest_instance["url"], "environment": environment}
    )
"""

_SEEDED_THROUGH_A_HELPER = """
def _world(nest, request):
    environment = {}
    conftest._apply_r14_trust_env(environment, nest, request)
    return {"environment": environment}

def test_second_seat(nest_instance, tui_app_path, request):
    create_driver("tui").launch({"app_path": tui_app_path, **_world(nest_instance, request)})
"""

_WEB = """
def test_tab(spa_url):
    create_driver("web").launch({"url": spa_url + "/app/"})
"""

_WRAPPER = """
def test_relaunch(harness, nest):
    harness.launch(secret_hex="00", node_url=nest["url"], trust=nest)
"""

_RELAUNCH = """
def _relaunch(app, extra):
    config = dict(app.driver._launch_config)
    config.update(extra)
    app.driver.teardown()
    app.driver.launch(config)
"""


@pytest.mark.parametrize(
    "source, needs_a_seed",
    [
        pytest.param(_UNSEEDED, [True], id="an unseeded launch"),
        pytest.param(_NO_ENVIRONMENT_KEY, [True], id="a config with no environment"),
        pytest.param(_SEEDED, [False], id="a seeded launch"),
        pytest.param(_SEEDED_THROUGH_A_HELPER, [False], id="seeded through a helper"),
        pytest.param(_WEB, [False], id="a browser launch"),
        pytest.param(_RELAUNCH, [False], id="a relaunch of the launched config"),
        pytest.param(_WRAPPER, [], id="a wrapper with its own signature"),
    ],
)
def test_the_scan_tells_a_launch_that_needs_a_seed(source, needs_a_seed):
    assert [launch.needs_a_seed for launch in launches_in(source, "x.py")] == needs_a_seed


def test_an_app_sync_seat_builds_its_config_for_a_nest_with_a_port(monkeypatch, tmp_path):
    # `_apply_r14_trust_env` returns early on a handle with no port, and an app
    # seat's launch config is built from the handle `make_seat` passes. A
    # port-less `{"url": ...}` stub there left every app seat unseeded while
    # the scan saw `_build_app_config` and counted it seeded.
    import conftest
    from helpers import sync_seats

    handed = []
    monkeypatch.setattr(
        conftest,
        "_build_app_config",
        lambda app_name, nest, request: handed.append(nest) or {"environment": {}},
    )
    sync_seats.make_seat(
        "tui",
        name="seat-a",
        run_token="token",
        root=tmp_path,
        node_url="http://127.0.0.1:4242",
        node_port=4242,
        folder="folder",
        sign_in=None,
        request=None,
    )
    assert handed == [{"url": "http://127.0.0.1:4242", "port": 4242}]


def test_the_launch_harness_seeds_each_launch_and_relaunch(monkeypatch, tmp_path):
    # The harness lives in `tests/common` and takes its writer as an argument, so
    # the scan cannot follow it: run the real harness against a fake driver and a
    # fake writer and read what each launch carried.
    import drivers
    from common.launch_harness import make_launch_harness

    launched: list[dict] = []

    class _Driver:
        def launch(self, config):
            launched.append(dict(config.get("environment", {})))

        def teardown(self):
            pass

    monkeypatch.setattr(drivers, "create_driver", lambda client: _Driver())
    reads = iter(["ab" * 32, "cd" * 32])
    asked: list[dict] = []

    def seed(environment, nest):
        asked.append(nest)
        environment["FAUNA_E2E_TRUST_NEST_IDENTITY"] = f"{nest['url']}={next(reads)}"

    harness = make_launch_harness("tui", tmp_path=tmp_path, app_path="/unused", seed_trust=seed)
    proxy = "http://127.0.0.1:7070"
    harness.launch(node_url=proxy, trust={"url": "http://127.0.0.1:4242", "port": 4242})
    harness.relaunch()
    assert asked == [{"url": proxy, "port": 4242}] * 2, (
        "the seed is keyed by the url the app dials and read from the trusted "
        "nest's own port, on the launch AND on every relaunch"
    )
    assert launched == [
        {"FAUNA_E2E_TRUST_NEST_IDENTITY": f"{proxy}={'ab' * 32}"},
        {"FAUNA_E2E_TRUST_NEST_IDENTITY": f"{proxy}={'cd' * 32}"},
    ], "a relaunch must carry the identity read at the relaunch, not the first boot's"

    harness.launch(node_url="http://127.0.0.1:1", trust=None)
    assert launched[-1] == {}, "a launch naming no nest carries no seed"


def test_a_native_launch_harness_cannot_be_built_without_a_seed_writer(tmp_path):
    from common.launch_harness import make_launch_harness

    with pytest.raises(ValueError, match="seed_trust"):
        make_launch_harness("tui", tmp_path=tmp_path, app_path="/unused")


def test_every_seed_call_reaches_the_writer():
    # The scan trusts SEED_CALLS by name, so a name that stopped calling the
    # writer would green every launch it covers while seeding nothing.
    conftest_source = (REPO / "tests/e2e-unified/conftest.py").read_text(encoding="utf-8")
    reaching = _seeding_names(ast.parse(conftest_source), frozenset({WRITER}))
    assert SEED_CALLS <= reaching, (
        f"{sorted(SEED_CALLS - reaching)} no longer reach `{WRITER}` in conftest.py"
    )

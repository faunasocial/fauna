"""The helper-binary registry — every test-only binary a test body runs, built
at collection time and never inside a test's clock (e2e-conventions.md
convention 9, the prebuild bullet's 2026-10-30 ruling).

A *helper binary* is a small test-only program a helper function runs from
inside a test body (`mls-keypackage-gen` minting real key packages for an
engine-less peer, `recovery_fixture` signing succession statements, …). No
fixture name leads to it, so the fixture-closure prebuild cannot see it; until
this registry each helper built it on first use, inside the calling test's
900 s budget — where the machine-wide `build`-slot wait, bounded at 5400 s,
killed a windows run before its first assertion (3 h 31 m, 2026-10-29/30).

So the reach is declared here, once per binary: the test modules whose import
reaches it (directly or through a helper module that imports it — the scan is
transitive over this tree's own modules, `conftest` excluded) and the fixtures
whose closure reaches it. `conftest.py::_prebuild_binaries` adds one
stamp-gated, slot-wrapped job per entry a collected item reaches
(`reached_entries` → `build`), and the test body asks `helper_bin(name)`, which
replays the collection-time result and NEVER builds: a miss is a loud
`HelperBinNotPrebuilt` naming the entry and the module that should declare it.

Freshness is the workspace's, not the binary's own source: every helper links
shared crates, so a binary older than any of `libs/` signs or mints in a shape
the engine or the nest has moved past (`common.nest._example_newest_source_mtime`).
A crate living outside `libs/` adds itself through `sources`, or an edit to it
reads fresh and the binary built from the old source is what the test runs.
The stamp also records the binary's own mtime, so a binary another cargo run
rebuilt under different features (a `cargo test -p fauna-mls` builds
`mls-group-gen` without `test-helpers`) reads stale and is rebuilt; and the
cargo arguments it was built under, so an entry whose own features changed
reads stale over the binary its old features built (`stamp_text`).
"""

from __future__ import annotations

import ast
import inspect
import json
import sys
from dataclasses import dataclass
from pathlib import Path

_E2E_DIR = Path(__file__).resolve().parents[1]
_REGISTRY_PATH = "tests/e2e-unified/helpers/helper_bins.py"


@dataclass(frozen=True)
class HelperBin:
    """One test-only binary: how cargo builds it and what reaches it."""

    #: The binary's file stem — the registry key `helper_bin` takes.
    name: str
    package: str
    #: `"bin"` (`--bin`), `"example"` (`--example`), `"lib"` (`--lib`, a windows
    #: cdylib landing as `<name>.dll`) or `"test"` (the package's lib unit-test
    #: binary, `cargo test --lib --no-run`: its file name is hashed, so the path
    #: is read from cargo's JSON and recorded in the stamp — `lib_test_executable`).
    target: str
    features: tuple[str, ...] = ()
    #: Features added on one host OS alone, as `(sys.platform prefix, features)`
    #: pairs (`entry_features`). For an entry reached from more than one
    #: `tests/platform/<os>` tree — each pruned off every other OS, so never in
    #: one run or on one box — whose reaches need different shapes.
    host_features: tuple[tuple[str, tuple[str, ...]], ...] = ()
    #: Declared write of the build (`--disk-gb`; `common.nest.DISK_GB_CRATE`).
    disk_gb: float = 5
    #: Dotted module names (as tests import them, rooted at tests/e2e-unified)
    #: whose import reaches the binary; `conftest.<name>` for a conftest helper
    #: a test module imports by name.
    modules: tuple[str, ...] = ()
    #: Fixture names whose closure reaches the binary.
    fixtures: tuple[str, ...] = ()
    #: `"debug"`, or `"release"` (`--release`) for an artifact's INPUTS built in
    #: the shipped profile.
    profile: str = "debug"
    #: Repo-relative crate dirs outside `libs/` the binary is built from: its
    #: own, and any it depends on by path.
    sources: tuple[str, ...] = ()


_MLS_CALLERS = (
    "tests.api.conv_api",
    # A conftest helper that mints key packages, imported by name.
    "conftest.provision_foreign_handled_actor",
)
_LINUX_INSTALLER = ("tests.platform.linux.test_installer",)
#: Reached only for a run that will build the MSI (`windows_msi_inputs_owed`).
_WINDOWS_INSTALLER_MODULE = "tests.platform.windows.test_installer"
_WINDOWS_INSTALLER = (_WINDOWS_INSTALLER_MODULE,)
_MLS_FIXTURES = ("cross_nest_foreign_actor", "cross_nest_foreign_ephemeral")
_LAUNCH_AGENT_PROBE = ("tests.real_session.test_sync_agent_launch_agent",)

#: Not a fixture: the name `conftest.py::_prebuild_jobs` adds to the requested
#: set for every run that launches the windows app, the way it adds
#: `sync_agent_binary` there. An entry reached by every windows launch declares it
#: in `fixtures` — its binary is run by the driver's launch, not by any one test.
WINDOWS_APP_LAUNCH = "windows_app_launch"

REGISTRY: dict[str, HelperBin] = {
    entry.name: entry
    for entry in (
        # Real MLS key packages for an engine-less API-tier peer
        # (`conv_api.mint_key_packages`).
        HelperBin(
            "mls-keypackage-gen", "fauna-mls", "bin",
            modules=_MLS_CALLERS, fixtures=_MLS_FIXTURES,
        ),
        # A real MLS group + Welcome as the sender of a GUI receive proof
        # (`conv_api.mint_group_welcome` and siblings); `test-helpers` compiles
        # in the `--future-body` mode's raw sealer.
        HelperBin(
            "mls-group-gen", "fauna-mls", "bin", features=("test-helpers",),
            modules=_MLS_CALLERS, fixtures=_MLS_FIXTURES,
        ),
        # The hello plugin's component (`test_plugin_install.py::_hello_component`).
        HelperBin(
            "plugin-fixture-gen", "fauna-plugin-host", "bin", features=("test-helpers",),
            modules=("tests.test_plugin_install",),
        ),
        # Succession statements and recovery kits (`helpers/succession.py`). It
        # signs client-side with the keys it is handed, so every nest mode that
        # reaches it builds it — a docker nest's run included.
        HelperBin(
            "recovery_fixture", "fauna-client-recovery", "example",
            modules=("helpers.succession",),
        ),
        # The cloud-init user-data `build_cloud_init` renders (the
        # `render_cloud_init_binary` fixture).
        HelperBin(
            "render_cloud_init", "fauna-provisioning", "example",
            fixtures=("render_cloud_init_binary",),
        ),
        # The relay-endpoint driver `test_iroh_relay.py` runs inside throwaway
        # containers (`just nat-probe` builds the same example for
        # `p2p-nat-probe`).
        HelperBin(
            "nat_probe", "fauna-iroh-relay", "example", features=("relay",),
            modules=("tests.platform.docker.test_iroh_relay",),
        ),
        # The linux installer's release-profile INPUTS (`test_installer.py`'s
        # `desktop_binary` / `agent_binary` / `nest_binary` fixtures): the
        # packager's prerequisites, not its subject, so they are built here.
        HelperBin(
            "fauna-desktop", "fauna-linux", "bin", profile="release", disk_gb=10,
            modules=_LINUX_INSTALLER, sources=("apps/fauna-linux",),
        ),
        # Both installers ship the agent, and each entry here is one cargo
        # invocation of one package: naming a second package beside it would
        # unify their features. linux packages the binary release.yml ships.
        # The windows MSI is the e2e flavour — its app is published
        # `-p:FaunaE2eAgent=true` over the release test-helpers ffi — and the
        # agent that app spawns carries the same surface: `e2e-agent` compiles
        # in the credential redirect the harness hands both halves
        # (`FAUNA_E2E_CREDENTIAL_DIR`, `FAUNA_KEYRING_APP`). Built plain, the
        # agent read the box's real Credential Manager while the app minted its
        # writer key into the run's file store, so the agent's seedless account
        # runtime never assembled and nothing uploaded.
        HelperBin(
            "fauna-sync-agent", "fauna-sync-agent", "bin", profile="release",
            host_features=(("win32", ("e2e-agent",)),),
            disk_gb=10, modules=_LINUX_INSTALLER + _WINDOWS_INSTALLER,
            sources=("bins/fauna-sync-agent",),
        ),
        HelperBin(
            "fauna-nest", "fauna-nest", "bin", profile="release", disk_gb=10,
            modules=_LINUX_INSTALLER, sources=("bins/fauna-nest",),
        ),
        # The windows installer's release-profile INPUTS (`test_installer.py`'s
        # `_build_msi` stages each from here).
        HelperBin(
            "fauna-nest-svc", "fauna-nest-service", "bin", profile="release",
            disk_gb=10, modules=_WINDOWS_INSTALLER,
            sources=("apps/fauna-windows/fauna-nest-service", "bins/fauna-nest"),
        ),
        HelperBin(
            "fauna-bridge-svc", "fauna-bridge-service", "bin", profile="release",
            disk_gb=10, modules=_WINDOWS_INSTALLER,
            sources=("apps/fauna-windows/fauna-bridge-service",),
        ),
        # The crate overrides `[lib] name = "fauna_shell"`, so the artifact is
        # fauna_shell.dll, not the package-name default fauna_shell_ext.dll.
        HelperBin(
            "fauna_shell", "fauna-shell-ext", "lib", profile="release",
            modules=_WINDOWS_INSTALLER, sources=("apps/fauna-windows/shell-ext",),
        ),
        # The TerminalApp feature ($(var.TuiBin), $(var.CliBin)) ships the
        # terminal app and the cli app's device shell beside the agent.
        HelperBin(
            "fauna-tui", "fauna-tui", "bin", profile="release", disk_gb=10,
            modules=_WINDOWS_INSTALLER, sources=("apps/fauna-tui",),
        ),
        HelperBin(
            "fauna-cli", "fauna-cli-device", "bin", profile="release", disk_gb=10,
            modules=_WINDOWS_INSTALLER, sources=("apps/fauna-cli",),
        ),
        # The macOS launchd probe's subject is the round trip through launchd, not
        # the compile: the crate's lib unit-test binary is built here and the test
        # runs it with `--ignored --exact` (`test_sync_agent_launch_agent.py`).
        HelperBin(
            "fauna-client-sync-lib-tests", "fauna-client-sync", "test",
            modules=_LAUNCH_AGENT_PROBE,
        ),
        # The FlaUI bridge's stall capture (e2e convention 11, the windows app's
        # stall stack): the bridge runs it on its app-side verdict to name what
        # holds the app's UI thread, so every windows launch hands it over
        # (`drivers/windows.py::_default_stall_capture_bin`).
        HelperBin(
            "fauna-e2e-stall-capture", "fauna-e2e-agent", "bin", disk_gb=2,
            fixtures=(WINDOWS_APP_LAUNCH,),
        ),
        # The admin's headless reports pass, driven against the run's nest (any
        # mode: it takes the URL and the admin key) by `tests/api/test_reports_pass.py`.
        HelperBin(
            "fauna-reports-pass", "fauna-reports-pass", "bin",
            modules=("tests.api.test_reports_pass",),
            sources=("bins/fauna-reports-pass",),
        ),
    )
}

#: The run's collection-time build memo, `key -> ("ok", path) | ("failed", exc)`.
#: `conftest.py::_BINARY_BUILD_MEMO` is this same dict, so the accessor replays
#: exactly what the prebuild recorded and the warm pass's forget (and the
#: refused-pass retry) covers helper builds like every other.
BUILD_MEMO: dict[str, tuple[str, object]] = {}


class HelperBinNotPrebuilt(RuntimeError):
    """A test asked for a helper binary the collection-time pass did not build."""


def memo_key(name: str) -> str:
    return f"helper-bin:{name}"


# ── the accessor: replays, never builds ───────────────────────────────────────


def _calling_module() -> str:
    """The test module on the stack (else the first caller outside this file),
    as a dotted name rooted at tests/e2e-unified — the module that should be
    in an entry's `modules`."""
    here = Path(__file__).resolve()
    fallback = None
    for frame in inspect.stack()[2:]:
        path = Path(frame.filename).resolve()
        if path == here:
            continue
        if fallback is None:
            fallback = path
        if path.name.startswith("test_") and path.is_relative_to(_E2E_DIR / "tests"):
            return _dotted(path) or str(path)
    if fallback is None:
        return "<unknown>"
    return _dotted(fallback) or str(fallback)


def helper_bin(name: str) -> Path:
    """The prebuilt helper binary `name` — a memo replay, NEVER a build.

    A failed collection-time build re-raises as the original failure (its
    output is already in the log above). A binary the pass never built — an
    unregistered name, or a reach the registry does not declare — raises
    `HelperBinNotPrebuilt`, so the gap is a loud red at its first use instead
    of a build inside the test's clock.
    """
    if name not in REGISTRY:
        raise HelperBinNotPrebuilt(
            f"no helper-binary registry entry named {name!r}: add one to "
            f"{_REGISTRY_PATH} (REGISTRY) so the collection-time prebuild builds it "
            "— a test never builds a binary itself (e2e-conventions.md convention 9)"
        )
    state = BUILD_MEMO.get(memo_key(name))
    if state is None:
        raise HelperBinNotPrebuilt(
            f"helper binary {name!r} was not prebuilt at collection time: "
            f"{_calling_module()} reaches it, but {_REGISTRY_PATH} REGISTRY[{name!r}] "
            "declares no module or fixture this run's items import or request — "
            "add the reaching module to its `modules` (or the fixture to its "
            "`fixtures`). A test never builds a binary itself "
            "(e2e-conventions.md convention 9)."
        )
    kind, value = state
    if kind == "ok":
        return Path(value)
    raise RuntimeError(
        f"{name} already failed to build at collection time "
        f"(see the build output above): {value!r}"
    )


# ── the reach: which entries a run's items need ───────────────────────────────


def _dotted(path: Path) -> str | None:
    """`path`'s module name rooted at tests/e2e-unified, or None outside it."""
    try:
        rel = path.resolve().relative_to(_E2E_DIR)
    except ValueError:
        return None
    parts = list(rel.with_suffix("").parts)
    if parts and parts[-1] == "__init__":
        parts.pop()
    return ".".join(parts)


def _module_file(name: str) -> Path | None:
    base = _E2E_DIR.joinpath(*name.split("."))
    for candidate in (base.with_suffix(".py"), base / "__init__.py"):
        if candidate.is_file():
            return candidate
    return None


def _direct_imports(path: Path) -> set[str]:
    """Every module name `path` imports, at any depth of its body (a test
    importing a helper inside its function counts) — `from X import a` yields
    both `X` and `X.a`, since `a` may be a submodule or a name."""
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"))
    except (OSError, SyntaxError, ValueError):
        return set()
    own = _dotted(path) or ""
    package = own.rsplit(".", 1)[0] if path.name != "__init__.py" else own
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            module = node.module or ""
            if node.level:
                anchor = package.split(".") if package else []
                anchor = anchor[: len(anchor) - (node.level - 1)] if node.level > 1 else anchor
                module = ".".join([*anchor, module] if module else anchor)
            if module:
                names.add(module)
            names.update(f"{module}.{alias.name}" if module else alias.name for alias in node.names)
    return names


_REACH_MEMO: dict[str, frozenset[str]] = {}


def modules_reached(path: Path) -> frozenset[str]:
    """Every module `path` reaches by import, transitively over this tree's own
    modules (its own name included). `conftest` is a leaf: it imports nearly
    every helper lazily inside fixtures, so a conftest reach is declared by
    fixture name (`HelperBin.fixtures`) or by the conftest helper a test
    imports (`conftest.<name>`). Memoized per file."""
    key = str(Path(path).resolve())
    if key in _REACH_MEMO:
        return _REACH_MEMO[key]
    reached: set[str] = set()
    own = _dotted(Path(path))
    if own:
        reached.add(own)
    pending = [Path(path)]
    seen_files = {key}
    while pending:
        for name in _direct_imports(pending.pop()):
            reached.add(name)
            if name == "conftest" or name.startswith("conftest."):
                continue
            target = _module_file(name)
            if target is None:
                continue
            target_key = str(target.resolve())
            if target_key not in seen_files:
                seen_files.add(target_key)
                pending.append(target)
    result = frozenset(reached)
    _REACH_MEMO[key] = result
    return result


def _reached_modules(items) -> set[str]:
    paths = {
        str(path)
        for path in (getattr(item, "path", None) for item in items)
        if path is not None
    }
    reached: set[str] = set()
    for path in paths:
        reached |= modules_reached(Path(path))
    return reached


def windows_msi_inputs_owed(items) -> bool:
    """Whether a run collecting `items` will build the windows MSI, the one
    reach that needs more than an import. The installer module stays collected
    where it builds nothing — unelevated, its items skip-marked; past a fresh
    stamp, `msi_path` returning the cached MSI — and an input built for either
    is a cold release compile and a flavor switch nothing reads."""
    if _WINDOWS_INSTALLER_MODULE not in _reached_modules(items):
        return False
    from common.nest import get_repo_root
    from helpers import windows_installer

    return windows_installer.msi_build_owed(str(get_repo_root()))


def reached_entries(items, requested: set) -> list[HelperBin]:
    """The registry entries a run's `items` reach — by a fixture in `requested`
    (the run's real fixture closure) or by a module an item's file imports —
    in registry order. Pure over the items' files, but for the windows MSI's
    gate (`windows_msi_inputs_owed`)."""
    reached_modules = _reached_modules(items)
    if not windows_msi_inputs_owed(items):
        reached_modules.discard(_WINDOWS_INSTALLER_MODULE)
    return [
        entry
        for entry in REGISTRY.values()
        if requested.intersection(entry.fixtures) or reached_modules.intersection(entry.modules)
    ]


# ── the build: infrastructure, run by the collection-time prebuild only ──────


def binary_paths(entry: HelperBin, target_dir: Path) -> tuple[Path, Path]:
    """(built exe, freshness stamp) for one entry's single shape (its profile)."""
    suffix = ".exe" if sys.platform == "win32" else ""
    if entry.target == "lib":
        suffix = ".dll"
    profile_dir = target_dir / entry.profile
    out_dir = profile_dir / ("examples" if entry.target == "example" else "")
    stamp = profile_dir / f".helper-{entry.name.replace('_', '-')}-e2e.stamp"
    if entry.target == "test":
        # The hashed name is only known to the build that made it; a stamp that
        # records none (or no stamp yet) leaves a path that does not exist, which
        # reads stale.
        recorded = _recorded_binary_path(stamp)
        return (recorded or profile_dir / "deps" / f"{entry.name}{suffix}", stamp)
    return (out_dir / f"{entry.name}{suffix}", stamp)


BINARY_PATH_PREFIX = "binary-path: "


def _recorded_binary_path(stamp: Path) -> Path | None:
    if not stamp.exists():
        return None
    for line in stamp.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.startswith(BINARY_PATH_PREFIX):
            return Path(line[len(BINARY_PATH_PREFIX):])
    return None


def lib_test_executable(entry: HelperBin, cargo_json: str) -> Path | None:
    """The lib unit-test executable in `cargo test --no-run --message-format=json`
    output: the compiler-artifact of `entry`'s package lib built with the test
    profile. None when the output names none."""
    lib_name = entry.package.replace("-", "_")
    for line in cargo_json.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if (
            isinstance(message, dict)
            and message.get("reason") == "compiler-artifact"
            and message.get("executable")
            and (message.get("profile") or {}).get("test")
            and (message.get("target") or {}).get("name") == lib_name
            and "lib" in ((message.get("target") or {}).get("kind") or [])
        ):
            return Path(message["executable"])
    return None


def entry_features(entry: HelperBin, platform: str = sys.platform) -> tuple[str, ...]:
    """The features `entry` builds with on `platform`: its own, then that host's."""
    extra = tuple(
        feature
        for host, features in entry.host_features
        if platform.startswith(host)
        for feature in features
    )
    return (*entry.features, *extra)


def all_features(entry: HelperBin) -> tuple[str, ...]:
    """Every feature `entry` names on any host — what a pure check of the
    registry against the crates reads, on whichever box it runs."""
    return (*entry.features, *(f for _host, features in entry.host_features for f in features))


def cargo_args(entry: HelperBin, platform: str = sys.platform) -> list[str]:
    """The cargo arguments that build `entry`'s one shape on `platform`."""
    features = entry_features(entry, platform)
    if entry.target == "test":
        args = ["test", "-p", entry.package, "--lib", "--no-run", "--message-format=json"]
        if entry.profile == "release":
            args.append("--release")
        if features:
            args += ["--features", ",".join(features)]
        return args
    selector = ["--lib"] if entry.target == "lib" else [f"--{entry.target}", entry.name]
    args = ["build", "-p", entry.package, *selector]
    if entry.profile == "release":
        args.append("--release")
    if features:
        args += ["--features", ",".join(features)]
    return args


_BINARY_MTIME_PREFIX = "binary-mtime-ns: "
_CARGO_ARGS_PREFIX = "cargo-args: "


def stamp_text(entry: HelperBin, binary: Path) -> str:
    """The freshness stamp of `binary`, just built as `entry`: the binary's own
    mtime (a binary another cargo run rewrote reads stale) and the cargo
    arguments it was built under (a stamp left by other features reads stale)."""
    return (
        f"{entry.name} e2e freshness stamp; mtime = build start (slot grant)\n"
        f"{_BINARY_MTIME_PREFIX}{binary.stat().st_mtime_ns}\n"
        f"{_CARGO_ARGS_PREFIX}{' '.join(cargo_args(entry))}\n"
        + (f"{BINARY_PATH_PREFIX}{binary}\n" if entry.target == "test" else "")
    )


def _stamp_is_fresh(binary: Path, stamp: Path, newest_source_mtime, args: list[str]) -> bool:
    if not binary.exists() or not stamp.exists():
        return False
    lines = stamp.read_text(encoding="utf-8", errors="replace").splitlines()

    def recorded(prefix: str) -> list[str]:
        return [line[len(prefix):] for line in lines if line.startswith(prefix)]

    if recorded(_BINARY_MTIME_PREFIX) != [str(binary.stat().st_mtime_ns)]:
        return False
    # The binary's mtime alone cannot tell an entry whose features changed: no
    # cargo ran, so the old shape's binary is still the one on disk.
    if recorded(_CARGO_ARGS_PREFIX) != [" ".join(args)]:
        return False
    return newest_source_mtime() <= stamp.stat().st_mtime


def _newest_source_mtime(entry: HelperBin, repo: Path) -> float:
    """The workspace's newest input plus the entry's own crates outside it."""
    from common.nest import (
        _NEST_SOURCE_EXCLUDES,
        _example_newest_source_mtime,
        _load_build_if_stale,
    )

    bis = _load_build_if_stale()
    return max([
        _example_newest_source_mtime(repo),
        *(bis._source_newest(repo / rel, _NEST_SOURCE_EXCLUDES) for rel in entry.sources),
    ])


def build(entry: HelperBin) -> str:
    """Build `entry` and return its path — stamp-gated, so a warm tree runs no
    cargo and takes no slot, and slot-wrapped and win-aware otherwise
    (the slot-wrapped composition shared with `common.nest`'s builders,
    over `cargo_args`). Called by `conftest.py::_ensure_helper_bin_built`
    at collection time; never from a test body."""
    from common.nest import (
        _build_slot_cmd,
        _cargo_cmd,
        _cargo_target_dir,
        _load_build_if_stale,
        _run_slot_build,
        get_repo_root,
    )

    repo = get_repo_root()
    binary, stamp = binary_paths(entry, _cargo_target_dir(repo))
    cmd = cargo_args(entry)
    if _stamp_is_fresh(binary, stamp, lambda: _newest_source_mtime(entry, repo), cmd):
        return str(binary)

    result, instant = _run_slot_build(
        _build_slot_cmd(repo, entry.disk_gb) + _cargo_cmd(repo) + cmd, cwd=repo
    )
    # cargo's exit code can lie on win-arm64, so there a non-zero exit is
    # forgiven only when the exe was (re)written at or after the build start
    # (`instant`, the slot grant): a mere present exe is the OLD one after a
    # refused slot (exit 75) or a compile error, and must not be stamped fresh.
    if entry.target == "test":
        built = lib_test_executable(entry, result.stdout or "")
        if built is not None:
            binary = built
    rebuilt = binary.exists() and binary.stat().st_mtime >= instant
    failed = result.returncode != 0 and (sys.platform != "win32" or not rebuilt)
    if failed or not binary.exists():
        tail = result.stderr[-3000:] if result.stderr else (result.stdout or "")[-3000:]
        raise RuntimeError(
            f"{entry.name}: `cargo {' '.join(cmd)}` failed "
            f"(exit {result.returncode}, binary {'present' if binary.exists() else 'absent'} "
            f"at {binary})\nBuild output tail:\n{tail}"
        )
    _load_build_if_stale().commit_stamp(stamp, instant, stamp_text(entry, binary))
    return str(binary)

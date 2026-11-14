"""The windows installer's prerequisites are collection-time prebuilds; its
publish and its WiX link are the test's subject and stay in the fixture
(e2e-conventions.md convention 9, the 2026-10-30 ruling: *a build that IS a
test's subject stays in its test … its prerequisites are prebuilds like any
other*).

`_build_msi` used to make every input itself, inside the first test's clock: on
2026-10-28 an elevated run died at ~900 s into it twice, writing no MSI. What these pins hold, all pure — no cargo, no
slot, no MSI, on every machine, the installer's own module read as text because
it is pruned off-windows and skipped unelevated:

* every input the MSI stages is a prebuild job, the bridge through its fixture
  and the rest through the helper-binary registry;
* the jobs fire only for a run that will build the MSI: elevated (the module
  stays collected unelevated, every item skip-marked) and past a stale stamp;
* the release ffi is the LAST job, because the staged pair is one slot and the
  publish bundles whatever flavor sits in it;
* a wrong staged flavor stops `_build_msi` before it packages the wrong core;
* the module holds only its subject and declares the subject's budget.
"""

from __future__ import annotations

import ast
import os
import tomllib
import types
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

import conftest  # noqa: E402  (tests/e2e-unified is on sys.path via conftest)
from common import nest as common_nest  # noqa: E402
from helpers import helper_bins, windows_installer  # noqa: E402

_E2E = Path(helper_bins.__file__).resolve().parents[1]
_REPO = _E2E.parents[1]
_MODULE = _E2E / "tests" / "platform" / "windows" / "test_installer.py"

#: Every binary `_build_msi` stages from the release profile.
_RELEASE_INPUTS = (
    "fauna-sync-agent", "fauna-nest-svc", "fauna-bridge-svc", "fauna_shell",
    "fauna-tui", "fauna-cli",
)
_RELEASE_FFI_BUILDER = "_ensure_windows_release_ffi_built"


class _Item:
    callspec = None

    def __init__(self, path: Path, fixturenames=()):
        self.path = path
        self.fixturenames = list(fixturenames)

    def get_closest_marker(self, name):
        return None

    def iter_markers(self):
        return []


def _jobs(monkeypatch, *, owed: bool, extra=()):
    monkeypatch.setattr(conftest, "get_available_apps", lambda: [])
    monkeypatch.setattr(windows_installer, "msi_build_owed", lambda repo: owed)
    session = types.SimpleNamespace(items=[_Item(_MODULE), *extra])
    return conftest._prebuild_jobs(session)


def test_every_msi_input_is_a_job_for_a_run_that_will_build_it(monkeypatch):
    jobs = _jobs(monkeypatch, owed=True)
    labels = {label for label, _builder, _args in jobs}
    builders = {builder for _label, builder, _args in jobs}
    assert labels >= {"mail_bridge_binary", *_RELEASE_INPUTS}
    assert _RELEASE_FFI_BUILDER in builders


def test_a_run_that_will_not_build_the_msi_builds_none_of_its_inputs(monkeypatch):
    """Unelevated, every item is skip-marked but stays collected; with a fresh
    stamp `msi_path` returns the cached MSI. Either way a hoisted job would pay
    a cold release build, and a flavor switch, for an artifact nothing reads."""
    jobs = _jobs(monkeypatch, owed=False)
    labels = {label for label, _builder, _args in jobs}
    builders = {builder for _label, builder, _args in jobs}
    assert not labels & {"mail_bridge_binary", *_RELEASE_INPUTS}
    assert _RELEASE_FFI_BUILDER not in builders


def test_a_mail_test_in_the_same_run_keeps_the_bridge_job_whatever_the_gate(monkeypatch):
    """The gate withholds what the MSI would have asked for, never what another
    collected test asks for itself."""
    other = _Item(_E2E / "tests" / "test_addressbook.py", ["mail_bridge_binary"])
    labels = {label for label, _b, _a in _jobs(monkeypatch, owed=False, extra=[other])}
    assert "mail_bridge_binary" in labels


def test_the_release_ffi_is_the_last_job_so_the_publish_finds_its_flavor_staged(monkeypatch):
    """One staged pair serves every flavor (`FaunaApp.csproj`'s single
    `runtimes\\win-arm64\\native\\fauna_ffi.dll`), and the journey items reach the
    dev flavor through `_ensure_fauna_ffi_built`: release last is what leaves
    `.ffi-flavor` on the flavor the publish bundles."""
    journey = _Item(_MODULE, ["nest_instance"])
    jobs = _jobs(monkeypatch, owed=True, extra=[journey])
    builders = [builder for _label, builder, _args in jobs]
    assert "_ensure_fauna_ffi_built" in builders
    assert builders[-1] == _RELEASE_FFI_BUILDER


def test_a_module_that_does_not_reach_the_installer_is_untouched(monkeypatch, tmp_path):
    """The gate is never even asked for a run with no installer item."""
    other = tmp_path / "test_other.py"
    other.write_text("import os\n")

    def asked(repo):
        raise AssertionError("msi_build_owed read for a run with no installer item")

    monkeypatch.setattr(windows_installer, "msi_build_owed", asked)
    assert helper_bins.windows_msi_inputs_owed([_Item(other)]) is False


# ── the registry shapes the inputs need ───────────────────────────────────────


def test_the_shell_extension_builds_as_a_lib_and_lands_as_its_dll():
    """`fauna-shell-ext` overrides `[lib] name = "fauna_shell"`: no bin to name,
    and the artifact is `fauna_shell.dll`, not the package-name default."""
    entry = helper_bins.REGISTRY["fauna_shell"]
    assert helper_bins.cargo_args(entry) == [
        "build", "-p", "fauna-shell-ext", "--lib", "--release",
    ]
    binary, stamp = helper_bins.binary_paths(entry, Path("target"))
    assert binary == Path("target/release/fauna_shell.dll")
    assert stamp.parent == Path("target/release")


def test_each_input_is_one_invocation_of_one_package():
    """The sync agent's solo invocation is what release.yml ships: naming a
    second package in one cargo run unifies their features."""
    for name in _RELEASE_INPUTS:
        args = helper_bins.cargo_args(helper_bins.REGISTRY[name])
        assert args.count("-p") == 1, args
        assert "--release" in args, args


def test_the_installed_agent_carries_the_surface_of_the_app_that_spawns_it():
    """The MSI is the e2e flavour: its app is published `-p:FaunaE2eAgent=true`
    over the release test-helpers ffi, whose `fauna-client-sync/test-helpers`
    compiles in the harness's credential redirect (`FAUNA_E2E_CREDENTIAL_DIR`,
    `FAUNA_KEYRING_APP`). The app spawns the installed agent with that same
    environment, and both halves open one account-store slot, so the agent must
    carry the same surface. Built plain, it compiled the redirect out and read
    the box's real Credential Manager while the app minted its writer key into
    the run's file store: the agent's seedless account runtime found no writer
    key, every bound set stayed keyless and nothing uploaded. The linux installer packages the binary release.yml ships and
    drives nothing through the harness, so its agent stays plain."""
    entry = helper_bins.REGISTRY["fauna-sync-agent"]
    assert helper_bins.cargo_args(entry, platform="win32") == [
        "build", "-p", "fauna-sync-agent", "--bin", "fauna-sync-agent", "--release",
        "--features", "e2e-agent",
    ]
    assert "--features" not in helper_bins.cargo_args(entry, platform="linux")
    # The two halves of the claim: the app side really is the e2e flavour ...
    assert "-p:FaunaE2eAgent=true" in _MODULE.read_text(encoding="utf-8")
    assert windows_installer.RELEASE_TEST_FLAVOR.endswith(":test-helpers")
    # ... and the agent's `e2e-agent` really carries the credential redirect.
    manifest = tomllib.loads(
        (_REPO / "bins" / "fauna-sync-agent" / "Cargo.toml").read_text(encoding="utf-8")
    )
    assert "fauna-credential-store/e2e-agent" in manifest["features"]["e2e-agent"]


def test_every_staged_inputs_own_crate_and_flavour_stale_the_cached_msi(tmp_path, monkeypatch):
    """`msi_path` returns the cached MSI while its stamp is at least
    `source_mtime`, and only a stale MSI owes the prebuild. So a crate an input
    is built from, left out of that reading, edits without a rebuild and the
    suite witnesses the old binary — as does a changed flavour, which is
    declared in the registry. The agent's and the nest's crates were missing
    until the agent's flavour changed; the own
    `sources` of each registry entry the MSI stages are read from the registry
    since, so a new input brings its crate with it."""
    monkeypatch.setattr(windows_installer, "is_admin", lambda: True)
    sources = [
        rel
        for entry in helper_bins.REGISTRY.values()
        if helper_bins._WINDOWS_INSTALLER_MODULE in entry.modules
        for rel in entry.sources
    ]
    assert sources
    for i, rel in enumerate(sources):
        repo = _msi_tree(tmp_path / str(i), stamp_mtime=2_000.0)
        _touch(tmp_path / str(i) / rel / "src" / "nested" / "x.rs", 3_000.0)
        assert windows_installer.msi_build_owed(repo), rel
    assert "tests/e2e-unified/helpers/helper_bins.py" in windows_installer.SOURCE_PATTERNS


def test_every_input_names_a_target_its_manifest_declares():
    """A selector the manifest does not declare fails only at the first cold
    build, hours into an elevated run. Read from the manifest the entry's own
    first source names, the package and the target both."""
    for name in _RELEASE_INPUTS:
        entry = helper_bins.REGISTRY[name]
        manifest = tomllib.loads((_REPO / entry.sources[0] / "Cargo.toml").read_text(encoding="utf-8"))
        assert manifest["package"]["name"] == entry.package, name
        if entry.target == "lib":
            assert manifest["lib"]["name"] == entry.name, name
            assert "cdylib" in manifest["lib"]["crate-type"], name
        else:
            assert entry.name in {b["name"] for b in manifest.get("bin", [])}, name


def test_every_release_input_watches_its_own_crate():
    """`_example_newest_source_mtime` watches only `libs/`, enough for a helper
    whose crate lives there. A release input whose crate lives under `apps/` or
    `bins/` must name it, or an edit to it reads fresh and the MSI packages the
    old binary."""
    for entry in helper_bins.REGISTRY.values():
        if entry.profile != "release":
            continue
        assert entry.sources, f"{entry.name} declares none of its own sources"
        for rel in entry.sources:
            assert (_REPO / rel).is_dir(), (entry.name, rel)


def test_an_edit_to_an_inputs_own_crate_rebuilds_it(tmp_path, monkeypatch):
    entry = helper_bins.REGISTRY["fauna-nest-svc"]
    target_dir = tmp_path / "target"
    binary, stamp = helper_bins.binary_paths(entry, target_dir)
    binary.parent.mkdir(parents=True)
    binary.write_text("fake exe")
    stamp.write_text(helper_bins.stamp_text(entry, binary))
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        common_nest, "_example_newest_source_mtime",
        lambda repo: stamp.stat().st_mtime - 3600,
    )
    source = tmp_path / entry.sources[0] / "src" / "main.rs"
    source.parent.mkdir(parents=True)
    source.write_text("fn main() {}\n")
    old = stamp.stat().st_mtime - 7200
    os.utime(source, (old, old))
    for rel in entry.sources[1:]:
        (tmp_path / rel).mkdir(parents=True, exist_ok=True)

    def no_build(*a, **k):
        raise AssertionError("a warm input must spawn nothing")

    monkeypatch.setattr(common_nest.subprocess, "run", no_build)
    assert helper_bins.build(entry) == str(binary)

    newer = stamp.stat().st_mtime + 60
    os.utime(source, (newer, newer))
    with pytest.raises(AssertionError, match="must spawn nothing"):
        helper_bins.build(entry)


# ── the gate: will this run build the MSI? ────────────────────────────────────


def _msi_tree(tmp_path: Path, *, stamp_mtime: float | None, source_mtime: float = 1_000.0) -> str:
    source = tmp_path / "apps" / "fauna-windows" / "installer" / "Package.wxs"
    source.parent.mkdir(parents=True)
    source.write_text("<Wix/>")
    os.utime(source, (source_mtime, source_mtime))
    repo = str(tmp_path)
    if stamp_mtime is not None:
        msi, stamp = windows_installer.msi_paths(repo)
        os.makedirs(os.path.dirname(msi))
        Path(msi).write_text("fake msi")
        Path(stamp).write_text(str(stamp_mtime))
    return repo


def test_unelevated_owes_no_msi_whatever_the_cache_says(tmp_path, monkeypatch):
    monkeypatch.setattr(windows_installer, "is_admin", lambda: False)
    assert windows_installer.msi_build_owed(_msi_tree(tmp_path, stamp_mtime=None)) is False


def test_elevated_owes_an_msi_only_past_a_stale_stamp(tmp_path, monkeypatch):
    monkeypatch.setattr(windows_installer, "is_admin", lambda: True)
    (tmp_path / "absent").mkdir()
    (tmp_path / "fresh").mkdir()
    (tmp_path / "stale").mkdir()
    assert windows_installer.msi_build_owed(_msi_tree(tmp_path / "absent", stamp_mtime=None))
    assert not windows_installer.msi_build_owed(
        _msi_tree(tmp_path / "fresh", stamp_mtime=2_000.0)
    )
    assert windows_installer.msi_build_owed(
        _msi_tree(tmp_path / "stale", stamp_mtime=2_000.0, source_mtime=3_000.0)
    )


def _touch(path: Path, mtime: float) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("x")
    os.utime(path, (mtime, mtime))


def test_a_libs_edit_no_pattern_names_stales_the_cached_msi(tmp_path, monkeypatch):
    """Every staged binary links most of `libs/` — the agent, the nest service,
    the release `fauna_ffi.dll` the publish bundles — so a `libs/` crate no
    `SOURCE_PATTERNS` entry names is an input too. Read fresh, an elevated run
    on a tree where only it changed installs an MSI built from older Rust."""
    from pathlib import PurePosixPath

    monkeypatch.setattr(windows_installer, "is_admin", lambda: True)
    linked = "libs/fauna-sync-engine/src/lib.rs"
    assert not any(PurePosixPath(linked).full_match(p) for p in windows_installer.SOURCE_PATTERNS)
    repo = _msi_tree(tmp_path, stamp_mtime=2_000.0)
    _touch(tmp_path / linked, 1_500.0)
    assert not windows_installer.msi_build_owed(repo), "a stamp newer than the edit is cached"
    _touch(tmp_path / linked, 3_000.0)
    assert windows_installer.msi_build_owed(repo), "a libs edit past the stamp owes the MSI"


@pytest.mark.parametrize("rel", ["Cargo.lock", "rust-toolchain.toml"])
def test_the_lockfile_and_the_toolchain_pin_stale_the_cached_msi(tmp_path, monkeypatch, rel):
    monkeypatch.setattr(windows_installer, "is_admin", lambda: True)
    repo = _msi_tree(tmp_path, stamp_mtime=2_000.0)
    _touch(tmp_path / rel, 3_000.0)
    assert windows_installer.msi_build_owed(repo)


def test_the_helper_bins_excluded_output_keeps_a_warm_msi_cached(tmp_path, monkeypatch):
    """The workspace reading is the helper bins' own, excludes included: a
    wasm-pack out-dir or the regenerated Go bindings under `libs/`, written by
    another build after the stamp, must not make every warm run miss the cache."""
    import fnmatch

    monkeypatch.setattr(windows_installer, "is_admin", lambda: True)
    repo = _msi_tree(tmp_path, stamp_mtime=2_000.0)
    _touch(tmp_path / "libs" / "fauna-core" / "src" / "lib.rs", 1_500.0)
    for rel in (
        "libs/fauna-wasm/pkg/fauna_wasm.js",
        "libs/fauna-wasm/pkg-test/fauna_wasm.js",
        "libs/fauna-mail-go/fauna_ffi/fauna_ffi.go",
    ):
        path = tmp_path / rel
        assert any(fnmatch.fnmatch(path.as_posix(), p) for p in common_nest._NEST_SOURCE_EXCLUDES), rel
        _touch(path, 3_000.0)
    assert not windows_installer.msi_build_owed(repo)


def test_an_unreadable_stamp_owes_an_msi(tmp_path, monkeypatch):
    monkeypatch.setattr(windows_installer, "is_admin", lambda: True)
    repo = _msi_tree(tmp_path, stamp_mtime=2_000.0)
    Path(windows_installer.msi_paths(repo)[1]).write_text("not a number")
    assert windows_installer.msi_build_owed(repo)


# ── the staged flavor ─────────────────────────────────────────────────────────


def _marker(tmp_path: Path, value: str | None) -> str:
    path = tmp_path / "apps" / "fauna-windows" / ".ffi-flavor"
    path.parent.mkdir(parents=True)
    if value is not None:
        path.write_text(value + "\n")
    return str(tmp_path)


def test_the_release_test_flavor_passes(tmp_path):
    windows_installer.require_release_test_flavor(
        _marker(tmp_path, "win-arm64:release:test-helpers")
    )


@pytest.mark.parametrize("staged", [
    "win-arm64:dev:test-helpers",  # the dev flavor: a debug core in a Release app
    "win-arm64:release:",          # production: bindings the e2e agent cannot compile against
    None,                          # nothing staged at all
])
def test_any_other_staged_flavor_stops_the_msi_loudly(tmp_path, staged):
    with pytest.raises(RuntimeError) as err:
        windows_installer.require_release_test_flavor(_marker(tmp_path, staged))
    message = str(err.value)
    assert "win-arm64:release:test-helpers" in message
    assert ".ffi-flavor" in message


def test_the_publish_watches_the_flavor_private_dll():
    """The shared `target/release/fauna_ffi.dll` is whichever host build of
    fauna-ffi ran last; the flavor's own slot is the only one anything
    downstream reads (build-target-layout-windows.md)."""
    dll = Path(windows_installer.release_ffi_dll("repo"))
    assert dll == Path("repo/target/release/windows-ffi/win-arm64/test-helpers/fauna_ffi.dll")


# ── the module itself holds only its subject ──────────────────────────────────


def _module_tree() -> ast.Module:
    return ast.parse(_MODULE.read_text(encoding="utf-8"))


def _function(tree: ast.Module, name: str) -> ast.FunctionDef:
    return next(
        node for node in ast.walk(tree)
        if isinstance(node, ast.FunctionDef) and node.name == name
    )


def _calls(node: ast.AST, name: str) -> list[ast.Call]:
    return [
        call for call in ast.walk(node)
        if isinstance(call, ast.Call)
        and getattr(call.func, "id", getattr(call.func, "attr", None)) == name
    ]


def test_build_msi_runs_only_its_subject():
    """The publish and the WiX link, and between them the MSI's licence files —
    nothing else: the ratchet cannot see a `cargo_cmd` variable behind `cmd /c`,
    so this one reads the steps. The licence step (`just windows-msi-notices`)
    compiles nothing, and cannot move to collection time: its notices read the
    project.assets.json the publish's restore writes."""
    steps = _calls(_function(_module_tree(), "_build_msi"), "run_build_step")
    assert len(steps) == 3, [ast.unparse(step)[:80] for step in steps]
    first, notices, link = (step.args[0] for step in sorted(steps, key=lambda s: s.lineno))
    assert isinstance(first, ast.Name) and first.id == "publish_cmd"
    assert isinstance(notices, ast.List) and [ast.literal_eval(e) for e in notices.elts[:2]] == ["just", "windows-msi-notices"]
    assert isinstance(link, ast.BinOp), "the wix argv is a list + the source files"


def test_no_build_argv_is_left_in_the_module():
    tree = _module_tree()
    words = {
        element.value
        for node in ast.walk(tree) if isinstance(node, (ast.List, ast.Tuple))
        for element in node.elts
        if isinstance(element, ast.Constant) and isinstance(element.value, str)
    }
    assert not words & {"windows-ffi-test", "windows-mail-bridge-build", "cmd"}


def test_build_msi_checks_the_flavor_before_it_publishes():
    source = ast.unparse(_function(_module_tree(), "_build_msi"))
    assert source.index("require_release_test_flavor") < source.index("publish_cmd")


def test_build_msi_stages_what_the_prebuild_built():
    build = _function(_module_tree(), "_build_msi")
    assert _calls(build, "helper_bin"), "the stage must replay the prebuild's memo"


def test_msi_path_requests_the_bridge_on_a_cache_miss_only():
    msi_path = _function(_module_tree(), "msi_path")
    requests = _calls(msi_path, "getfixturevalue")
    assert [r.args[0].value for r in requests] == ["mail_bridge_binary"]


def test_the_module_declares_the_subject_budget():
    """Convention 9: a subject build stays in its test on a clock of at least
    3600 s. A session fixture's time is charged to the first test that asks."""
    tree = _module_tree()
    assign = next(
        node for node in tree.body
        if isinstance(node, ast.Assign) and getattr(node.targets[0], "id", "") == "pytestmark"
    )
    timeouts = _calls(assign, "timeout")
    assert timeouts, "pytestmark carries no timeout"
    assert timeouts[0].args[0].value >= 3600

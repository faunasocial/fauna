"""The windows installer's prerequisites are collection-time prebuilds; its
publish and its WiX link are the test's subject and stay in the fixture
(e2e-conventions.md convention 9, the 2026-10-30 ruling: *a build that IS a
test's subject stays in its test … its prerequisites are prebuilds like any
other*).

The build used to make every input itself, inside the first test's clock: on
2026-10-28 an elevated run died at ~900 s into it twice, writing no MSI. What these pins hold, all pure — no cargo, no
slot, no MSI, on every machine, the MSI's readers read as text because they
are pruned off-windows:

* the MSI has one door, the `msi_path` fixture (`conftest.py`), and every test
  that reads it — the elevated install suite and the unelevated structure
  suite — goes through it;
* every input the MSI stages is a prebuild job, the bridge through its fixture
  and the rest through the helper-binary registry, reached by that door;
* the jobs fire only for a run that will build the MSI: a selected test that is
  not skip-marked requests the door (the install suite stays collected
  unelevated, every item skip-marked; the structure suite needs no elevation,
  because building the MSI needs none — only installing it does), past a stale
  stamp;
* the release ffi is the LAST job, because the staged pair is one slot and the
  publish bundles whatever flavor sits in it;
* a wrong staged flavor stops `build_msi` before it packages the wrong core;
* the build holds only its subject, its compile takes a `build` grant, and each
  reader declares the subject's budget.
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
_STRUCTURE = _E2E / "tests" / "platform" / "windows" / "test_installer_structure.py"
_HELPER = _E2E / "helpers" / "windows_installer.py"
_CONFTEST = _E2E / "conftest.py"
_MSI = helper_bins.WINDOWS_MSI

#: Every binary `build_msi` stages from the release profile.
_RELEASE_INPUTS = (
    "fauna-sync-agent", "fauna-nest-svc", "fauna-bridge-svc", "fauna_shell",
    "fauna-tui", "fauna-cli",
)
_RELEASE_FFI_BUILDER = "_ensure_windows_release_ffi_built"


def _mark(name, *args, **kwargs):
    return types.SimpleNamespace(name=name, args=args, kwargs=kwargs)


class _Item:
    callspec = None

    def __init__(self, path: Path, fixturenames=(), marks=()):
        self.path = path
        self.fixturenames = list(fixturenames)
        self.marks = list(marks)

    def get_closest_marker(self, name):
        return next(self.iter_markers(name), None)

    def iter_markers(self, name=None):
        return iter([m for m in self.marks if name is None or m.name == name])


def _reader() -> _Item:
    """A structure-suite test: it reads the MSI through `msi_db`, which takes the door."""
    return _Item(_STRUCTURE, ["msi_db", _MSI])


def _skipped_install_item() -> _Item:
    """An install-suite test collected unelevated: skip-marked by the module's
    `pytestmark`, its closure holding the door through `_cleanup_stale_install`."""
    return _Item(_MODULE, ["_cleanup_stale_install", _MSI],
                 marks=[_mark("skipif", True, reason="Requires admin privileges")])


def _no_elevation_read(monkeypatch):
    def asked():
        raise AssertionError("the gate read elevation: a skip mark says which test "
                             "reads the MSI, and building it needs no elevation")

    monkeypatch.setattr(windows_installer, "is_admin", asked)


def _jobs(monkeypatch, *, owed: bool, extra=()):
    monkeypatch.setattr(conftest, "get_available_apps", lambda: [])
    monkeypatch.setattr(windows_installer, "msi_build_owed", lambda repo, items: owed)
    session = types.SimpleNamespace(items=[_Item(_MODULE, [_MSI]), *extra])
    return conftest._prebuild_jobs(session)


def test_every_msi_input_is_a_job_for_a_run_that_will_build_it(monkeypatch):
    jobs = _jobs(monkeypatch, owed=True)
    labels = {label for label, _builder, _args in jobs}
    builders = {builder for _label, builder, _args in jobs}
    assert labels >= {"mail_bridge_binary", *_RELEASE_INPUTS}
    assert _RELEASE_FFI_BUILDER in builders


def test_a_run_that_will_not_build_the_msi_builds_none_of_its_inputs(monkeypatch):
    """No selected test that would run reads the MSI (the install suite
    unelevated: every item skip-marked but collected), or the stamp is fresh
    and `msi_path` returns the cached MSI. Either way a hoisted job would pay a
    cold release build, and a flavor switch, for an artifact nothing reads."""
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


def test_a_run_with_no_msi_reader_reads_neither_the_tree_nor_the_cache(monkeypatch, tmp_path):
    """The gate is never even asked for a run in which no item requests the
    MSI: the stamp read walks `libs/`, and nearly every run is such a run."""
    other = tmp_path / "test_other.py"
    other.write_text("import os\n")

    def asked(*a, **k):
        raise AssertionError("the MSI's gate read for a run with no MSI reader")

    monkeypatch.setattr(windows_installer, "msi_build_owed", asked)
    monkeypatch.setattr(windows_installer, "source_mtime", asked)
    assert helper_bins.windows_msi_inputs_owed([_Item(other)]) is False
    assert helper_bins.windows_msi_inputs_owed([_skipped_install_item()]) is False
    reached = {e.name for e in helper_bins.reached_entries([_skipped_install_item()], {_MSI})}
    assert not reached & set(_RELEASE_INPUTS), reached


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
    assert "-p:FaunaE2eAgent=true" in _HELPER.read_text(encoding="utf-8")
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
    sources = [
        rel
        for entry in helper_bins.REGISTRY.values()
        if _MSI in entry.fixtures
        for rel in entry.sources
    ]
    assert sources
    for i, rel in enumerate(sources):
        repo = _msi_tree(tmp_path / str(i), stamp_mtime=2_000.0)
        _touch(tmp_path / str(i) / rel / "src" / "nested" / "x.rs", 3_000.0)
        assert _owed(repo), rel
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


def _owed(repo: str) -> bool:
    """The gate for a run selecting one structure-suite test."""
    return windows_installer.msi_build_owed(repo, [_reader()])


def test_an_unelevated_structure_run_owes_the_msi_only_past_a_stale_stamp(tmp_path, monkeypatch):
    """Building the MSI needs no elevation, only installing it does: the
    structure suite, which reads the built MSI and installs nothing, owes the
    build in the unelevated verify drain — unasked, the drain answered every
    such request `unrunnable` over its skipped legs."""
    _no_elevation_read(monkeypatch)
    (tmp_path / "absent").mkdir()
    (tmp_path / "fresh").mkdir()
    (tmp_path / "stale").mkdir()
    assert _owed(_msi_tree(tmp_path / "absent", stamp_mtime=None))
    assert not _owed(_msi_tree(tmp_path / "fresh", stamp_mtime=2_000.0))
    assert _owed(_msi_tree(tmp_path / "stale", stamp_mtime=2_000.0, source_mtime=3_000.0))


def test_a_run_selecting_only_the_skip_marked_install_suite_owes_none(tmp_path, monkeypatch):
    """Unelevated, the install suite stays collected with every item
    skip-marked: no test of it reads the MSI, so nothing is owed whatever the
    cache says — and the stamp's walk of the tree is never made."""
    _no_elevation_read(monkeypatch)

    def walked(repo):
        raise AssertionError("the stamp read for a run with no MSI reader")

    monkeypatch.setattr(windows_installer, "source_mtime", walked)
    repo = _msi_tree(tmp_path, stamp_mtime=None)
    assert windows_installer.msi_build_owed(repo, [_skipped_install_item()] * 3) is False


def test_an_elevated_install_run_still_owes_the_msi(tmp_path, monkeypatch):
    """Elevated, the install suite's items carry no skip and read the MSI."""
    _no_elevation_read(monkeypatch)
    repo = _msi_tree(tmp_path, stamp_mtime=None)
    assert windows_installer.msi_build_owed(repo, [_Item(_MODULE, ["_cleanup_stale_install", _MSI])])


def test_a_test_that_requests_no_msi_owes_none(tmp_path, monkeypatch):
    """A test of a reader module that reads only source asks for no MSI."""
    _no_elevation_read(monkeypatch)
    repo = _msi_tree(tmp_path, stamp_mtime=None)
    assert windows_installer.msi_build_owed(repo, [_Item(_STRUCTURE, ["tmp_path"])]) is False


@pytest.mark.parametrize("mark, skipped", [
    (_mark("skip", reason="r"), True),
    (_mark("skipif", True, reason="r"), True),
    (_mark("skipif", False, True, reason="r"), True),   # any true condition
    (_mark("skipif", condition=True, reason="r"), True),
    (_mark("skipif", reason="r"), True),               # no condition: unconditional
    (_mark("skipif", "sys.platform == 'win32'", reason="r"), True),  # read as applying
    (_mark("skipif", False, reason="r"), False),
    (_mark("skipif", condition=False, reason="r"), False),
    (_mark("timeout", 3600), False),
])
def test_a_skip_mark_of_any_shape_withholds_the_build(tmp_path, monkeypatch, mark, skipped):
    """A string condition, which pytest evaluates against the module's globals,
    is read as applying: misread the other way it would owe a cold release
    build to a test that then skips, while this way a test that does run
    fails loudly in the build, its inputs never prebuilt (`HelperBinNotPrebuilt`)."""
    _no_elevation_read(monkeypatch)
    repo = _msi_tree(tmp_path, stamp_mtime=None)
    item = _Item(_STRUCTURE, ["msi_db", _MSI], marks=[mark])
    assert windows_installer.msi_build_owed(repo, [item]) is (not skipped)


def _unelevated_jobs(monkeypatch, tmp_path, items):
    """`_prebuild_jobs` with the real gate over a tree holding no MSI."""
    _no_elevation_read(monkeypatch)
    monkeypatch.setattr(conftest, "get_available_apps", lambda: [])
    repo = Path(_msi_tree(tmp_path, stamp_mtime=None))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: repo)
    return conftest._prebuild_jobs(types.SimpleNamespace(items=items))


def test_an_unelevated_structure_run_prebuilds_every_input_and_switches_flavor_last(
        monkeypatch, tmp_path):
    jobs = _unelevated_jobs(monkeypatch, tmp_path, [_reader()])
    labels = {label for label, _builder, _args in jobs}
    assert labels >= {"mail_bridge_binary", *_RELEASE_INPUTS}
    assert jobs[-1][1] == _RELEASE_FFI_BUILDER


def test_an_unelevated_install_only_run_prebuilds_nothing_and_keeps_its_flavor(
        monkeypatch, tmp_path):
    """`require_release_test_flavor`'s switch stays out of a run that owes no
    MSI: it restages the one ffi pair every other flavor shares."""
    jobs = _unelevated_jobs(monkeypatch, tmp_path, [_skipped_install_item()])
    labels = {label for label, _builder, _args in jobs}
    builders = {builder for _label, builder, _args in jobs}
    assert not labels & {"mail_bridge_binary", *_RELEASE_INPUTS}
    assert _RELEASE_FFI_BUILDER not in builders


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

    linked = "libs/fauna-sync-engine/src/lib.rs"
    assert not any(PurePosixPath(linked).full_match(p) for p in windows_installer.SOURCE_PATTERNS)
    repo = _msi_tree(tmp_path, stamp_mtime=2_000.0)
    _touch(tmp_path / linked, 1_500.0)
    assert not _owed(repo), "a stamp newer than the edit is cached"
    _touch(tmp_path / linked, 3_000.0)
    assert _owed(repo), "a libs edit past the stamp owes the MSI"


@pytest.mark.parametrize("rel", ["Cargo.lock", "rust-toolchain.toml"])
def test_the_lockfile_and_the_toolchain_pin_stale_the_cached_msi(tmp_path, monkeypatch, rel):
    repo = _msi_tree(tmp_path, stamp_mtime=2_000.0)
    _touch(tmp_path / rel, 3_000.0)
    assert _owed(repo)


def test_the_helper_bins_excluded_output_keeps_a_warm_msi_cached(tmp_path, monkeypatch):
    """The workspace reading is the helper bins' own, excludes included: a
    wasm-pack out-dir or the regenerated Go bindings under `libs/`, written by
    another build after the stamp, must not make every warm run miss the cache."""
    import fnmatch

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
    assert not _owed(repo)


def test_an_unreadable_stamp_owes_an_msi(tmp_path, monkeypatch):
    repo = _msi_tree(tmp_path, stamp_mtime=2_000.0)
    Path(windows_installer.msi_paths(repo)[1]).write_text("not a number")
    assert _owed(repo)


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


# ── one door, and the build behind it holds only its subject ──────────────────


def _tree(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"))


def _module_tree() -> ast.Module:
    return _tree(_MODULE)


def _helper_tree() -> ast.Module:
    return _tree(_HELPER)


def _conftest_fixture(name: str) -> ast.FunctionDef:
    """One top-level function of the root conftest, its decorators included,
    parsed alone — the whole file is ~20k lines, and this suite runs at every
    merge-gate pass."""
    lines = _CONFTEST.read_text(encoding="utf-8").splitlines()
    head = next(n for n, line in enumerate(lines) if line.startswith(f"def {name}("))
    start = head
    while start and lines[start - 1].startswith("@"):
        start -= 1
    end = next(
        (n for n in range(head + 1, len(lines))
         if lines[n] and not lines[n][0].isspace() and not lines[n].startswith(")")),
        len(lines),
    )
    tree = ast.parse("\n".join(lines[start:end]))
    return next(node for node in tree.body if isinstance(node, ast.FunctionDef))


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
    steps = _calls(_function(_helper_tree(), "build_msi"), "run_build_step")
    assert len(steps) == 3, [ast.unparse(step)[:80] for step in steps]
    first, notices, link = (step.args[0] for step in sorted(steps, key=lambda s: s.lineno))
    assert isinstance(first, ast.Name) and first.id == "publish_cmd"
    assert isinstance(notices, ast.List) and [ast.literal_eval(e) for e in notices.elts[:2]] == ["just", "windows-msi-notices"]
    assert isinstance(link, ast.BinOp), "the wix argv is a list + the source files"


def test_the_publish_takes_a_build_grant():
    """The publish is a compile — the self-contained WinUI restore, XAML compile
    and bundle — and nothing compiles outside the count: both of its argvs
    open with the `build` pool's prefix, which re-enters a grant the caller
    already holds."""
    build = _function(_helper_tree(), "build_msi")
    assigns = [
        node for node in ast.walk(build)
        if isinstance(node, ast.Assign) and getattr(node.targets[0], "id", "") == "publish_cmd"
    ]
    assert len(assigns) == 2, "the MSBuild publish and the `dotnet publish` fallback"
    for node in assigns:
        assert ast.unparse(node.value).startswith("_build_slot_cmd("), ast.unparse(node.value)[:80]


@pytest.mark.parametrize("tree", [_module_tree, lambda: _function(_helper_tree(), "build_msi")],
                         ids=["install-suite", "build_msi"])
def test_no_build_argv_but_the_subject_is_left(tree):
    words = {
        element.value
        for node in ast.walk(tree()) if isinstance(node, (ast.List, ast.Tuple))
        for element in node.elts
        if isinstance(element, ast.Constant) and isinstance(element.value, str)
    }
    assert not words & {"windows-ffi-test", "windows-mail-bridge-build", "cmd"}


def test_build_msi_checks_the_flavor_before_it_publishes():
    source = ast.unparse(_function(_helper_tree(), "build_msi"))
    assert source.index("require_release_test_flavor") < source.index("publish_cmd")


def test_build_msi_stages_what_the_prebuild_built():
    build = _function(_helper_tree(), "build_msi")
    assert _calls(build, "helper_bin"), "the stage must replay the prebuild's memo"


def test_msi_path_requests_the_bridge_on_a_cache_miss_only():
    msi_path = _conftest_fixture(_MSI)
    requests = _calls(msi_path, "getfixturevalue")
    assert [r.args[0].value for r in requests] == ["mail_bridge_binary"]
    assert _calls(msi_path, "build_msi"), "the door builds through the one builder"


def test_the_door_is_one_conftest_fixture_and_the_registry_reaches_it():
    """One session fixture serves both readers, so one run builds the MSI once
    and the gate's question has one name to ask: a module-own copy would be a
    second door the registry cannot see."""
    assert _MSI == "msi_path"
    assert hasattr(conftest, _MSI)
    for path in (_MODULE, _STRUCTURE):
        assert not any(
            isinstance(node, ast.FunctionDef) and node.name == _MSI for node in _tree(path).body
        ), f"{path.name} defines its own {_MSI}"
    for name in _RELEASE_INPUTS:
        assert _MSI in helper_bins.REGISTRY[name].fixtures, name


def test_every_structure_leg_that_reads_the_build_goes_through_the_door():
    """The structure suite reads the MSI database through `msi_db` and the
    staged agent the build wrote beside it: both take the door, which builds a
    stale MSI, so no leg skips on a missing or stale build — a skip the
    verify drain answers `unrunnable`, never green."""
    tree = _tree(_STRUCTURE)
    msi_db = _function(tree, "msi_db")
    assert [a.arg for a in msi_db.args.args] == [_MSI]
    assert not _calls(msi_db, "skip"), "msi_db skips on a missing MSI instead of building it"
    staged = _function(tree, "test_staged_sync_agent_is_gui_subsystem")
    assert _MSI in [a.arg for a in staged.args.args]
    assert not any(
        isinstance(node, ast.FunctionDef) and node.name == "_require_msi_built_after"
        for node in ast.walk(tree)
    ), "a built-after skip outlived the door: the door's stamp already covers every input"


@pytest.mark.parametrize("path", [_MODULE, _STRUCTURE], ids=lambda p: p.stem)
def test_each_reader_declares_the_subject_budget(path):
    """Convention 9: a subject build stays in its test on a clock of at least
    3600 s. A session fixture's time is charged to the first test that asks,
    and either suite may be the run's first."""
    tree = _tree(path)
    assign = next(
        node for node in tree.body
        if isinstance(node, ast.Assign) and getattr(node.targets[0], "id", "") == "pytestmark"
    )
    timeouts = _calls(assign, "timeout")
    assert timeouts, "pytestmark carries no timeout"
    assert timeouts[0].args[0].value >= 3600

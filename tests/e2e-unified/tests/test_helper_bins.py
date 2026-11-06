"""The helper-binary registry and its never-building accessor
(`helpers/helper_bins.py`; e2e-conventions.md convention 9, the prebuild
bullet's 2026-10-30 ruling: a build is reached at collection time or not at
all).

Three pins, all pure — no cargo, no slot, no child pytest:

* the accessor never builds: an unregistered name, and a registered binary the
  collection-time pass did not build, both raise naming the gap and run no
  subprocess;
* the accessor replays the collection-time memo, a failure included;
* a test module that reaches an entry by import — directly, or through a
  helper module that imports it — selects that entry's prebuild job, and a
  module that reaches nothing selects none.

The bound-inversion pin (a slow helper build charged to collection passes
under a 6 s per-test timeout) is the child-pytest one in
`test_harness_self_termination.py` § 5.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

import conftest  # noqa: E402  (tests/e2e-unified is on sys.path via conftest)
from helpers import helper_bins  # noqa: E402


@pytest.fixture
def no_subprocess(monkeypatch):
    """Any subprocess the accessor started would be a build: fail on it."""

    def refuse(*a, **k):
        raise AssertionError(f"helper_bin ran a subprocess: {a!r}")

    monkeypatch.setattr(subprocess, "run", refuse)
    monkeypatch.setattr(subprocess, "Popen", refuse)


@pytest.fixture
def memo_isolated():
    """The run's build memo, snapshotted and cleared for the test."""
    saved = dict(helper_bins.BUILD_MEMO)
    helper_bins.BUILD_MEMO.clear()
    try:
        yield helper_bins.BUILD_MEMO
    finally:
        helper_bins.BUILD_MEMO.clear()
        helper_bins.BUILD_MEMO.update(saved)


def test_conftest_memo_is_the_registry_memo():
    """One memo: what the prebuild records is what the accessor replays, and
    the warm pass's forget covers helper builds like every other."""
    assert conftest._BINARY_BUILD_MEMO is helper_bins.BUILD_MEMO


def test_unregistered_helper_bin_raises_naming_the_gap(no_subprocess, memo_isolated):
    with pytest.raises(helper_bins.HelperBinNotPrebuilt) as err:
        helper_bins.helper_bin("no-such-helper")
    message = str(err.value)
    assert "no-such-helper" in message
    assert "helpers/helper_bins.py" in message


def test_unprebuilt_entry_raises_naming_the_module_that_should_declare_it(
    no_subprocess, memo_isolated
):
    with pytest.raises(helper_bins.HelperBinNotPrebuilt) as err:
        helper_bins.helper_bin("mls-keypackage-gen")
    message = str(err.value)
    assert "mls-keypackage-gen" in message
    # The test module that reached it without the registry seeing the reach.
    assert "tests.test_helper_bins" in message
    assert "helpers/helper_bins.py" in message


def test_accessor_replays_the_collection_time_memo(no_subprocess, memo_isolated, tmp_path):
    built = tmp_path / "mls-keypackage-gen"
    memo_isolated[helper_bins.memo_key("mls-keypackage-gen")] = ("ok", str(built))
    assert helper_bins.helper_bin("mls-keypackage-gen") == built

    memo_isolated[helper_bins.memo_key("mls-group-gen")] = ("failed", RuntimeError("E0425"))
    with pytest.raises(RuntimeError, match="E0425"):
        helper_bins.helper_bin("mls-group-gen")


class _Item:
    def __init__(self, path: Path, fixturenames=()):
        self.path = path
        self.fixturenames = list(fixturenames)


def _module(tmp_path: Path, name: str, body: str) -> Path:
    path = tmp_path / name
    path.write_text(body)
    return path


def test_a_module_reaching_an_entry_by_import_selects_its_job(tmp_path):
    direct = _module(tmp_path, "test_direct.py", "from tests.api import conv_api\n")
    # `helpers.folder_share` imports conv_api inside a function: transitive.
    via_helper = _module(
        tmp_path, "test_via_helper.py",
        "def test_x():\n    from helpers.folder_share import headless_member\n",
    )
    nothing = _module(tmp_path, "test_nothing.py", "import os\n")

    def selected(*items, requested=()):
        return {e.name for e in helper_bins.reached_entries(items, set(requested))}

    assert selected(_Item(nothing)) == set()
    assert {"mls-keypackage-gen", "mls-group-gen"} <= selected(_Item(direct))
    assert "mls-keypackage-gen" in selected(_Item(via_helper))
    # Exact: an MLS caller builds no recovery or plugin helper.
    assert "recovery_fixture" not in selected(_Item(direct))
    assert "plugin-fixture-gen" not in selected(_Item(direct))
    # A fixture-reached entry is selected by the item's closure.
    assert "render_cloud_init" in selected(
        _Item(nothing), requested={"render_cloud_init_binary"}
    )
    # The recovery helper signs client-side, with keys it is handed, against
    # whatever nest URL the test names: a docker nest's run builds it too.
    succession = _module(tmp_path, "test_succ.py", "from helpers.succession import registration_chain\n")
    assert "recovery_fixture" in selected(_Item(succession))


def test_prebuild_adds_one_job_per_reached_entry(monkeypatch, tmp_path):
    """`_prebuild_binaries` wires the registry: a session whose module imports
    conv_api builds both MLS helpers at collection, through the helper job."""
    mod = conftest
    saved = dict(mod._BINARY_BUILD_MEMO)
    built: list[str] = []
    monkeypatch.setattr(mod, "_ensure_helper_bin_built", built.append)
    for name in ("_ensure_fauna_ffi_loaded", "_ensure_fauna_ffi_built", "_ensure_sync_agent_built"):
        monkeypatch.setattr(mod, name, lambda: None)
    monkeypatch.setattr(mod, "get_available_apps", lambda: [])
    import types

    item = _Item(_module(tmp_path, "test_mls.py", "from tests.api import conv_api\n"))
    item.callspec = None
    item.get_closest_marker = lambda name: None
    item.iter_markers = lambda: []
    try:
        mod._prebuild_binaries(types.SimpleNamespace(items=[item]))
    finally:
        mod._BINARY_BUILD_MEMO.clear()
        mod._BINARY_BUILD_MEMO.update(saved)
    assert "mls-keypackage-gen" in built
    assert "mls-group-gen" in built
    assert "plugin-fixture-gen" not in built


def test_every_registry_reach_names_something_real():
    """A registry `modules` name that resolves to no file, or a `fixtures`
    name no conftest defines, would select nothing — a silent gap the
    accessor only reports at the first use."""
    e2e = Path(helper_bins.__file__).resolve().parents[1]
    for entry in helper_bins.REGISTRY.values():
        for module in entry.modules:
            if module.startswith("conftest."):
                assert hasattr(conftest, module.split(".", 1)[1]), (entry.name, module)
                continue
            rel = Path(*module.split("."))
            assert (e2e / rel.with_suffix(".py")).exists() or (e2e / rel / "__init__.py").exists(), (
                entry.name, module,
            )
        for fixture in entry.fixtures:
            if fixture == helper_bins.WINDOWS_APP_LAUNCH:
                # Not a fixture but the name conftest's prebuild requests for
                # every windows launch — so conftest must still request it.
                source = Path(conftest.__file__).read_text(encoding="utf-8")
                assert "_helper_bins.WINDOWS_APP_LAUNCH" in source, (entry.name, fixture)
                continue
            assert hasattr(conftest, fixture), (entry.name, fixture)


def test_a_windows_launch_reaches_the_bridge_stall_capture():
    """Every run that launches the windows app hands the FlaUI bridge its
    stall-capture helper (`drivers/windows.py::_default_stall_capture_bin`), so
    the launch name reaches that entry, and nothing else does."""
    reached = {
        e.name
        for e in helper_bins.reached_entries([], {helper_bins.WINDOWS_APP_LAUNCH})
    }
    assert "fauna-e2e-stall-capture" in reached
    assert "fauna-e2e-stall-capture" not in {
        e.name for e in helper_bins.reached_entries([], set())
    }


def _crate_features(package: str) -> set[str]:
    """Feature names `package` declares: its `[features]` keys plus its optional
    dependencies (cargo's implicit features). The crate is found by scanning the
    workspace manifests for `name = "<package>"` — no cargo invocation."""
    import tomllib

    root = Path(helper_bins.__file__).resolve().parents[3]
    for top in ("libs", "bins", "apps"):
        for manifest in (root / top).rglob("Cargo.toml"):
            if "target" in manifest.parts or "node_modules" in manifest.parts:
                continue
            try:
                data = tomllib.loads(manifest.read_text(encoding="utf-8"))
            except (OSError, tomllib.TOMLDecodeError):
                continue
            if data.get("package", {}).get("name") != package:
                continue
            names = set(data.get("features", {}))
            for table in ("dependencies", "build-dependencies"):
                for dep, spec in data.get(table, {}).items():
                    if isinstance(spec, dict) and spec.get("optional"):
                        names.add(dep)
            return names
    raise AssertionError(f"no workspace manifest declares package {package!r}")


def test_every_registry_feature_exists_in_its_crate():
    """A registry entry's `features` are passed to cargo as `--features`; one the
    crate no longer declares fails the build with `does not contain this
    feature` and leaves every warm step holding that entry cold."""
    missing = {
        (entry.name, feature)
        for entry in helper_bins.REGISTRY.values()
        for feature in helper_bins.all_features(entry)
        if feature not in _crate_features(entry.package)
    }
    assert not missing, f"registry features absent from their crate: {sorted(missing)}"


def test_host_features_apply_on_their_host_alone():
    """A host's extra features join the entry's own on that host and nowhere
    else — the sync agent's two installers, each pruned off the other's OS."""
    entry = helper_bins.HelperBin(
        "x", "x", "bin", features=("a",), host_features=(("win32", ("b",)),),
    )
    assert helper_bins.entry_features(entry, "win32") == ("a", "b")
    assert helper_bins.entry_features(entry, "linux") == ("a",)
    assert helper_bins.all_features(entry) == ("a", "b")


def test_a_stamp_left_by_other_cargo_args_reads_stale(tmp_path):
    """The stamp names the cargo arguments it was built under. Without that, an
    entry whose features changed read fresh over the binary its old features
    built — the binary's own mtime is unchanged, since nothing rebuilt it — and
    the run used the old shape with no word. A stamp written before the args
    were recorded reads stale the same way, so every such binary rebuilds once."""
    import dataclasses

    entry = helper_bins.HelperBin("x", "x", "bin", profile="release")
    binary = tmp_path / "x.exe"
    binary.write_text("fake exe")
    stamp = tmp_path / ".stamp"
    stamp.write_text(helper_bins.stamp_text(entry, binary))

    def sources():
        return stamp.stat().st_mtime - 60

    assert helper_bins._stamp_is_fresh(binary, stamp, sources, helper_bins.cargo_args(entry))
    flavoured = dataclasses.replace(entry, features=("e2e-agent",))
    assert not helper_bins._stamp_is_fresh(
        binary, stamp, sources, helper_bins.cargo_args(flavoured)
    )
    stamp.write_text(f"stamp\nbinary-mtime-ns: {binary.stat().st_mtime_ns}\n")
    assert not helper_bins._stamp_is_fresh(binary, stamp, sources, helper_bins.cargo_args(entry))

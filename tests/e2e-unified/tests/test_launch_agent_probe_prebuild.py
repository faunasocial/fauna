"""The macOS launchd probe's test binary is a collection-time prebuild
(e2e-conventions.md convention 9, the 2026-10-30 ruling: *a build, and any step
whose wait is a machine-wide queue's, is reached at collection time or not at
all*).

`tests/real_session/test_sync_agent_launch_agent.py` used to run
`cargo test -p fauna-client-sync --lib <probe> -- --ignored --exact` through a
`focus` slot inside its own 1200 s clock, so the compile — and the core-bound
wait in front of it — landed in the test's budget. Now the helper-binary
registry compiles the crate's lib unit-test binary (`cargo test --no-run`) at
collection, and the test runs that binary. What these pins hold, all pure — no
cargo, no slot, no launchd:

* the registry entry is a `test` target whose cargo argv compiles without
  running and emits the JSON that names the executable;
* the executable is read from cargo's JSON (a test binary's name is hashed, so
  the path cannot be guessed) and recorded in the freshness stamp;
* the probe module reaches the entry by import and holds no build of its own.
"""

from __future__ import annotations

import ast
import json
import types
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

from helpers import helper_bins  # noqa: E402

_E2E = Path(helper_bins.__file__).resolve().parents[1]
_MODULE = _E2E / "tests" / "real_session" / "test_sync_agent_launch_agent.py"
_NAME = "fauna-client-sync-lib-tests"


def test_the_probe_binary_is_a_registered_test_target():
    entry = helper_bins.REGISTRY[_NAME]
    assert entry.target == "test"
    assert entry.package == "fauna-client-sync"
    assert helper_bins.cargo_args(entry) == [
        "test", "-p", "fauna-client-sync", "--lib", "--no-run",
        "--message-format=json",
    ]


def test_the_probe_module_reaches_the_entry_by_import(tmp_path):
    entry = helper_bins.REGISTRY[_NAME]
    item = types.SimpleNamespace(path=_MODULE, fixturenames=[], callspec=None)
    reached = helper_bins.reached_entries([item], set())
    assert entry in reached
    assert "tests.real_session.test_sync_agent_launch_agent" in entry.modules


def test_the_executable_is_read_from_cargos_json():
    lines = [
        json.dumps({"reason": "compiler-artifact", "executable": None,
                    "target": {"name": "fauna_client_sync", "kind": ["lib"]},
                    "profile": {"test": False}}),
        json.dumps({"reason": "compiler-artifact", "executable": "/t/deps/fauna_client_sync-abc",
                    "target": {"name": "fauna_client_sync", "kind": ["lib"]},
                    "profile": {"test": True}}),
        json.dumps({"reason": "compiler-artifact", "executable": "/t/deps/other-def",
                    "target": {"name": "other", "kind": ["lib"]},
                    "profile": {"test": True}}),
        "not json at all",
    ]
    entry = helper_bins.REGISTRY[_NAME]
    assert helper_bins.lib_test_executable(entry, "\n".join(lines)) == Path("/t/deps/fauna_client_sync-abc")
    assert helper_bins.lib_test_executable(entry, "") is None


def test_a_recorded_path_is_the_binary_a_warm_run_returns(tmp_path):
    entry = helper_bins.REGISTRY[_NAME]
    binary = tmp_path / "deps" / "fauna_client_sync-abc"
    binary.parent.mkdir()
    binary.write_text("fake")
    _, stamp = helper_bins.binary_paths(entry, tmp_path)
    stamp.parent.mkdir(parents=True, exist_ok=True)
    stamp.write_text(f"{helper_bins.BINARY_PATH_PREFIX}{binary}\n")
    assert helper_bins.binary_paths(entry, tmp_path)[0] == binary


def test_the_probe_module_builds_nothing_itself():
    tree = ast.parse(_MODULE.read_text(encoding="utf-8"))
    constants = {n.value for n in ast.walk(tree) if isinstance(n, ast.Constant) and isinstance(n.value, str)}
    assert "cargo" not in constants
    assert "build-slot.py" not in "".join(sorted(constants))
    assert "helper_bin" in _MODULE.read_text(encoding="utf-8")

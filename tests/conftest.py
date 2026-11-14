"""Root conftest: add tests/ to sys.path so `from common import ...` works."""

import sys
from pathlib import Path

# Add the tests/ directory to sys.path so that `from common import X` resolves
# to tests/common/ for all test suites (e2e, e2e-unified, etc.).
_tests_dir = str(Path(__file__).resolve().parent)
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)

# An elevated run on windows takes a temp root of its own (testing.md § Test temp-dir
# hygiene). Loaded by path: importing the `common` package would run its imports too.
import importlib.util as _ilu

_elevation_spec = _ilu.spec_from_file_location(
    "fauna_test_elevation", str(Path(_tests_dir) / "common" / "elevation.py"))
_elevation = _ilu.module_from_spec(_elevation_spec)
_elevation_spec.loader.exec_module(_elevation)

# A bare pytest under tests/ (outside tests/e2e-unified, whose own conftest takes
# its slot) takes its own `tier_1` grant where the machine self-admits test runs. The hooks
# live beside the slot tool, which does not ship: absent, nothing is taken.
_test_run_slot_path = Path(__file__).resolve().parents[1] / "scripts" / "_test_run_slot.py"
if _test_run_slot_path.exists():
    _spec = _ilu.spec_from_file_location("fauna_test_run_slot", str(_test_run_slot_path))
    _test_run_slot = _ilu.module_from_spec(_spec)
    _spec.loader.exec_module(_test_run_slot)
    pytest_collection_finish = _test_run_slot.pytest_collection_finish
    pytest_sessionfinish = _test_run_slot.pytest_sessionfinish


def pytest_configure(config):
    """A goal's `test:` proof item this run executes gets its `PROVEN` record
    (task-kinds.md § Kinds → *A `GOAL` row is the fleet's flag*, ruling (3)).
    The helper lives beside the queue files it writes, so a tree without them
    installs nothing; a run under `e2e-unified`'s own rootdir never loads this.
    An elevated run's temp root is taken first, before anything makes a temp dir."""
    _elevation.take_elevated_temproot(config)
    path = Path(__file__).resolve().parents[1] / "scripts" / "_proof_hook.py"
    if path.exists():
        import importlib.util
        spec = importlib.util.spec_from_file_location("fauna_proof_hook", str(path))
        hook = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(hook)
        hook.install(config)

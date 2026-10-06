"""Root conftest: add tests/ to sys.path so `from common import ...` works."""

import sys
from pathlib import Path

# Add the tests/ directory to sys.path so that `from common import X` resolves
# to tests/common/ for all test suites (e2e, e2e-unified, etc.).
_tests_dir = str(Path(__file__).resolve().parent)
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)

# A bare pytest under tests/ (outside tests/e2e-unified, whose own conftest takes
# its slot) takes its own `tier_1` grant where the machine self-admits test runs. The hooks
# live beside the slot tool, which does not ship: absent, nothing is taken.
_test_run_slot_path = Path(__file__).resolve().parents[1] / "scripts" / "_test_run_slot.py"
if _test_run_slot_path.exists():
    import importlib.util as _ilu

    _spec = _ilu.spec_from_file_location("fauna_test_run_slot", str(_test_run_slot_path))
    _test_run_slot = _ilu.module_from_spec(_spec)
    _spec.loader.exec_module(_test_run_slot)
    pytest_collection_finish = _test_run_slot.pytest_collection_finish
    pytest_sessionfinish = _test_run_slot.pytest_sessionfinish

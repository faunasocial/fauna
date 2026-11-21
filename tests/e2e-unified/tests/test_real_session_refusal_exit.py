"""A real-session refusal exits ``REAL_SESSION_UNRUN_EXIT`` — never 1.

``refuse_unrun`` ends the run with ``pytest.exit(…, returncode=77)``, which the
verify queue reads as NOT RUN and leaves the request standing. Raised from a
FUNCTION-scoped fixture's setup after ``tmp_path`` was set up, that 77 is lost:
pytest writes the stash key ``tmp_path``'s teardown reads only in the setup
phase's report, which ``Exit`` skips, so the session-finish teardown raises
``KeyError`` and the run exits 1 — a red. Measured on the Flatpak seam witness,
answered ``exited 1`` twice for a live app's bus name. A refusal
fixture is therefore module-scoped (set up before every function fixture), or
the check runs in the test body.
"""

import ast
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

E2E_ROOT = Path(__file__).resolve().parents[1]
SEAM = E2E_ROOT / "tests" / "real_session" / "test_sync_agent_flatpak_seam.py"


def _run(tmp_path: Path, scope: str) -> int:
    (tmp_path / "test_probe.py").write_text(textwrap.dedent(f"""
        import sys
        sys.path.insert(0, {str(E2E_ROOT)!r})
        import pytest
        from helpers.real_session_refusal import refuse_unrun

        @pytest.fixture(scope={scope!r})
        def refusal():
            refuse_unrun("probe")

        @pytest.fixture(autouse=True)
        def early_tmp(tmp_path):
            yield

        def test_x(refusal, tmp_path):
            pass
    """))
    return subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider",
         "--rootdir", str(tmp_path), "-c", "/dev/null", str(tmp_path / "test_probe.py")],
        cwd=tmp_path, capture_output=True, text=True,
    ).returncode


def test_a_module_scoped_refusal_exits_unrun(tmp_path):
    from helpers.real_session_refusal import REAL_SESSION_UNRUN_EXIT

    assert _run(tmp_path, "module") == REAL_SESSION_UNRUN_EXIT


def test_a_function_scoped_refusal_after_tmp_path_loses_the_exit(tmp_path):
    """The trap itself, pinned so a pytest that fixes it is noticed."""
    from helpers.real_session_refusal import REAL_SESSION_UNRUN_EXIT

    assert _run(tmp_path, "function") != REAL_SESSION_UNRUN_EXIT


def test_the_seam_refusal_fixture_is_not_function_scoped():
    tree = ast.parse(SEAM.read_text(encoding="utf-8"))
    fixture = next(n for n in tree.body
                   if isinstance(n, ast.FunctionDef) and n.name == "no_live_fauna_app")
    scopes = [kw.value.value for d in fixture.decorator_list if isinstance(d, ast.Call)
              for kw in d.keywords if kw.arg == "scope"]
    assert scopes and scopes[0] in ("module", "package", "session"), (
        "no_live_fauna_app refuses with pytest.exit: function-scoped, its 77 "
        "becomes exit 1 behind tmp_path's teardown")


#: The form the verify queue's drain parses (its own test loads this helper and
#: parses `hold_token`'s output, so the two cannot drift apart unseen).
_TOKEN = "hold: dbus-name social.fauna.fauna pid 4242"


def test_a_refusal_naming_a_hold_prints_the_token_first(tmp_path):
    """A refusal under live work names the hold — resource and owner pid — first
    in its UNRUN line, so a clipped line still carries it, and the request stands
    with no try spent while the hold is live (delayed-testing.md § The verify
    queue)."""
    (tmp_path / "test_probe.py").write_text(textwrap.dedent(f"""
        import sys
        sys.path.insert(0, {str(E2E_ROOT)!r})
        import pytest
        from helpers.real_session_refusal import refuse_unrun

        @pytest.fixture(scope="module")
        def refusal():
            refuse_unrun("a Fauna app is live", hold=("dbus-name", "social.fauna.fauna", 4242))

        def test_x(refusal):
            pass
    """))
    out = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider",
         "--rootdir", str(tmp_path), "-c", "/dev/null", str(tmp_path / "test_probe.py")],
        cwd=tmp_path, capture_output=True, text=True,
    )
    from helpers.real_session_refusal import REAL_SESSION_UNRUN_EXIT

    assert out.returncode == REAL_SESSION_UNRUN_EXIT
    assert f"UNRUN: {_TOKEN} — a Fauna app is live" in out.stdout


def test_the_seam_refusal_names_the_bus_name_hold():
    src = SEAM.read_text(encoding="utf-8")
    assert 'hold=("dbus-name", GTK_APP_ID, pid)' in src

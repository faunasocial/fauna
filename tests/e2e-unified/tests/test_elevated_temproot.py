r"""An elevated pytest run on Windows takes a temp root of its own.

Owner: `docs/goal/architecture/testing.md` § Test temp-dir hygiene, the elevated-run
paragraph. Python ≥ 3.13 makes a `0o700` dir on Windows under a protected DACL that
grants only its owner, SYSTEM and Administrators, and an elevated process's objects
are owned by Administrators, so a dir an elevated run made in the user's `%TEMP%` —
pytest's numbered dirs, every `mkdtemp` — opens to no unelevated process. Two such
dirs stood under `pytest-of-User` on Windows from elevated installer-suite runs of
2026-10-27/28, skipped by every later cleanup. The helper `tests/common/elevation.py`
moves an elevated run's Python temp to `%TEMP%\fauna-elevated-pytest`, which elevated
runs reclaim themselves; these tests pin its decision, its reclaim, its configure
call, the redirect under the real pytest, and that every pytest root calls it first.
"""

from __future__ import annotations

import ast
import os
import subprocess
import sys
import tempfile
import textwrap
import time
from pathlib import Path

import pytest

from common import elevation

pytestmark = pytest.mark.tier_1

REPO = Path(__file__).resolve().parents[3]
DAY = 86400


class _Config:
    """The one piece of pytest's config the helper reads: `--basetemp`."""

    def __init__(self, basetemp=None):
        self._basetemp = basetemp

    def getoption(self, name, default=None):
        assert name == "basetemp"
        return self._basetemp


@pytest.fixture
def restore_tempdir(monkeypatch):
    """`take_elevated_temproot` sets the process's `tempfile.tempdir`; put it back."""
    monkeypatch.setattr(tempfile, "tempdir", tempfile.tempdir)


def _aged(path: Path, days: float) -> Path:
    t = time.time() - days * DAY
    os.utime(path, (t, t))
    return path


def test_an_elevated_run_on_windows_takes_the_elevated_root():
    root = elevation.elevated_temproot(
        platform="win32", elevated=True, environ={}, tempdir=r"C:\T",
    )
    assert root == os.path.join(r"C:\T", "fauna-elevated-pytest")


@pytest.mark.parametrize(
    "platform, elevated, environ",
    [
        ("win32", False, {}),  # the ordinary run: the user's own %TEMP%
        ("linux", True, {}),  # no Windows DACL, nothing to move
        ("darwin", True, {}),
        ("win32", True, {"PYTEST_DEBUG_TEMPROOT": r"D:\mine"}),  # the caller chose
    ],
)
def test_every_other_run_keeps_its_temp_root(platform, elevated, environ):
    assert elevation.elevated_temproot(
        platform=platform, elevated=elevated, environ=environ, tempdir=r"C:\T",
    ) is None


def test_taking_the_root_points_pytest_and_tempfile_at_it(tmp_path, restore_tempdir):
    environ: dict[str, str] = {}
    root = elevation.take_elevated_temproot(
        _Config(), platform="win32", elevated=True, environ=environ, tempdir=str(tmp_path),
    )
    assert root == str(tmp_path / "fauna-elevated-pytest")
    assert environ == {"PYTEST_DEBUG_TEMPROOT": root}
    assert tempfile.gettempdir() == root
    assert Path(root).is_dir()


def test_an_explicit_basetemp_is_left_alone(tmp_path, restore_tempdir):
    before = tempfile.tempdir
    environ: dict[str, str] = {}
    assert elevation.take_elevated_temproot(
        _Config(basetemp=str(tmp_path / "bt")), platform="win32", elevated=True,
        environ=environ, tempdir=str(tmp_path),
    ) is None
    assert environ == {}
    assert tempfile.tempdir == before
    assert not (tmp_path / "fauna-elevated-pytest").exists()


def test_an_unelevated_run_changes_nothing(tmp_path, restore_tempdir):
    before = tempfile.tempdir
    environ: dict[str, str] = {}
    assert elevation.take_elevated_temproot(
        _Config(), platform="win32", elevated=False, environ=environ, tempdir=str(tmp_path),
    ) is None
    assert environ == {}
    assert tempfile.tempdir == before
    assert list(tmp_path.iterdir()) == []


def test_an_elevated_run_reclaims_what_earlier_ones_left_past_a_week(tmp_path):
    """A kept or killed run's `mkdtemp` dirs beside pytest's own are the next elevated
    run's to remove once a week old; pytest's own root is left to pytest's keep, and
    anything younger stays for whoever is reading it."""
    root = tmp_path / "fauna-elevated-pytest"
    old = root / "fauna-e2e-localappdata-old"
    (old / "Fauna" / "logs").mkdir(parents=True)
    (old / "Fauna" / "logs" / "fauna.log").write_text("kept")
    _aged(old, 8)
    young = root / "fauna-e2e-localappdata-young"
    young.mkdir()
    _aged(young, 2)
    stray = root / "stray.txt"
    stray.write_text("x")
    _aged(stray, 9)
    pytest_root = root / "pytest-of-User"
    (pytest_root / "pytest-7").mkdir(parents=True)
    _aged(pytest_root, 30)

    removed = elevation.reclaim_stale(str(root), now=time.time())

    assert sorted(removed) == ["fauna-e2e-localappdata-old", "stray.txt"]
    assert not old.exists() and not stray.exists()
    assert young.is_dir() and (pytest_root / "pytest-7").is_dir()


def test_taking_the_root_reclaims_before_the_run_writes(tmp_path, restore_tempdir):
    root = tmp_path / "fauna-elevated-pytest"
    left = root / "fauna-e2e-creds-left"
    left.mkdir(parents=True)
    _aged(left, 10)
    elevation.take_elevated_temproot(
        _Config(), platform="win32", elevated=True, environ={}, tempdir=str(tmp_path),
    )
    assert not left.exists()


def test_the_root_taken_at_configure_is_where_the_real_pytest_writes(tmp_path):
    """The redirect rests on pytest reading its temp root when the first temp dir is
    made, after `pytest_configure`: a child pytest whose conftest takes the root
    (elevation forced, on any host) must hand its test a `tmp_path` under it, and its
    own `mkdtemp` must land there too."""
    project = tmp_path / "project"
    project.mkdir()
    temp = tmp_path / "temp"
    temp.mkdir()
    helper = REPO / "tests" / "common" / "elevation.py"
    (project / "conftest.py").write_text(textwrap.dedent(f"""
        import importlib.util

        _spec = importlib.util.spec_from_file_location("elevation", {str(helper)!r})
        elevation = importlib.util.module_from_spec(_spec)
        _spec.loader.exec_module(elevation)


        def pytest_configure(config):
            elevation.take_elevated_temproot(config, platform="win32", elevated=True)
    """), encoding="utf-8")
    (project / "test_child.py").write_text(textwrap.dedent(f"""
        import os
        import tempfile
        from pathlib import Path


        def test_temp_is_under_the_elevated_root(tmp_path):
            root = Path({str(temp)!r}).resolve() / "fauna-elevated-pytest"
            assert Path(os.environ["PYTEST_DEBUG_TEMPROOT"]).resolve() == root
            assert root in tmp_path.resolve().parents, tmp_path
            made = Path(tempfile.mkdtemp(prefix="fauna-e2e-probe-"))
            assert made.resolve().parent == root, made
            made.rmdir()
    """), encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "PYTEST_DEBUG_TEMPROOT"}
    # `tempfile.gettempdir()` in the child reads these first, so the root lands in
    # this test's own dir, never the machine's temp.
    env.update(TMPDIR=str(temp), TEMP=str(temp), TMP=str(temp))
    proc = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", str(project)],
        cwd=project, env=env, capture_output=True, text=True, timeout=300,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert (temp / "fauna-elevated-pytest" / f"pytest-of-{_pytest_user()}").is_dir()


def _pytest_user() -> str:
    """The user name pytest puts in `pytest-of-<user>` (`getpass.getuser`)."""
    import getpass

    return getpass.getuser()


@pytest.mark.parametrize("conftest", ["tests/e2e-unified/conftest.py", "tests/conftest.py"])
def test_every_pytest_root_takes_the_root_before_anything_else(conftest):
    """Each conftest a pytest root loads calls the helper as the first statement of its
    `pytest_configure`, before any hook body can make a temp dir."""
    _assert_takes_the_root_first(REPO / conftest)


def test_the_fleet_tooling_suite_takes_the_root_too():
    """The fleet tooling's own suite has a pytest root of its own, where the tooling is
    in the checkout at all."""
    conftest = REPO / "scripts" / "conftest.py"
    if not conftest.exists():
        from helpers.app_surface import skip_environment

        skip_environment("the fleet's tooling is not in this checkout")
    _assert_takes_the_root_first(conftest)


def _assert_takes_the_root_first(conftest: Path) -> None:
    tree = ast.parse(conftest.read_text(encoding="utf-8"))
    [configure] = [
        node for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "pytest_configure"
    ]
    body = [n for n in configure.body
            if not (isinstance(n, ast.Expr) and isinstance(n.value, ast.Constant))]
    first = body[0]
    assert isinstance(first, ast.Expr) and isinstance(first.value, ast.Call), ast.dump(first)
    func = first.value.func
    name = func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", None)
    assert name == "take_elevated_temproot", ast.dump(first)
    assert [ast.unparse(a) for a in first.value.args] == ["config"]

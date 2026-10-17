"""tier_1: `helpers/prev_build.py`'s command line never builds by accident.

A cold pinned build is a whole workspace compile plus the platform's app toolchain —
on Windows well over an hour, and the build-slot count only knows about it when it was
started through `just e2e-prev-build`. The script used to read its flags by
membership test and fall through to `build_prev` for anything it did not recognise,
so `prev_build.py --help` (or a misspelt `--bulid`) started a real, unslotted build of
the pinned commit inside the same source tree as any build already warming it. These
pin the refusal: an unknown flag builds nothing, `--help` prints the usage, and the
flags the recipes and the upgrade smoke pass still reach the build.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import prev_build  # noqa: E402

pytestmark = pytest.mark.tier_1


@pytest.fixture
def no_build(monkeypatch):
    """Fail the test the moment anything would build or touch the pin's cache."""
    def _refuse(*_a, **_k):
        pytest.fail("the command line started a build")
    monkeypatch.setattr(prev_build, "build_prev", _refuse)
    monkeypatch.setattr(prev_build, "cached_binaries", _refuse)


def _main(monkeypatch, *args):
    monkeypatch.setattr(prev_build.sys, "argv", ["prev_build.py", *args])
    return prev_build.main()


@pytest.mark.parametrize("flag", ["--help", "-h"])
def test_help_prints_the_usage_and_builds_nothing(flag, monkeypatch, no_build, capsys):
    assert _main(monkeypatch, flag) == 0
    out = capsys.readouterr().out
    assert "--build" in out and "--path" in out


@pytest.mark.parametrize("args", [("--bulid",), ("--client", "windows", "--dry-run")])
def test_an_unknown_flag_is_refused_before_anything_builds(args, monkeypatch, no_build, capsys):
    assert _main(monkeypatch, *args) == 2
    assert args[-1] in capsys.readouterr().err


def test_the_build_flags_the_recipe_passes_still_build(monkeypatch, capsys):
    """`just e2e-prev-build --client windows` → `prev_build.py --build --client windows`."""
    calls = []
    monkeypatch.setattr(prev_build, "build_prev",
                        lambda commit, profile, client: calls.append((profile, client)) or {})
    assert _main(monkeypatch, "--build", "--client", "windows", "--profile", "debug") == 0
    assert calls == [("debug", "windows")]

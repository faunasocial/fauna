"""An ERROR or a FAILED names its reason on the run's own clock, not at end-of-run.

`docs/goal/architecture/e2e-conventions.md` point 6 — a failure must diagnose
itself — applied to a long run's *timeline*. pytest holds every traceback until
the end-of-run summary, which is fine for a ten-minute run and useless for a long
one: the 2026-08-30 docker sweep spent 17 hours producing 135 fixture errors whose
reason nobody could read until it finished, and three consecutive sessions
bisected that cascade as a black box. The
reason string was in the run's hands the whole time.

So `conftest.pytest_runtest_logreport` prints one truncated crash line the moment
a report is recorded. That covered ERRORs only until 2026-09-21, on the reasoning
that a FAILED test "is already reported" — true only of a run that reaches its
summary. A held traceback is lost by ANY abrupt end, and on Windows that end is
the ORDINARY one: pytest-timeout has no `signal` method there, so its `thread`
method `os._exit`s the process and no summary is ever printed. The labels stay distinct (`[e2e] ERROR` / `[e2e] FAILED`) because the
diagnoses are: an ERROR is a precondition, a FAILED is the subject itself.

tier_1 by the taxonomy's decision tree: no nest binary, no driver, no external
process — the hook is exercised directly, against the real conftest rather than a
copy, so a regression in the shipped hook reds these. The companion proof that
the line actually survives a kill needs a child pytest, so it is tier_2 and lives
in `test_harness_self_termination.py` beside convention 9's other child runs.
"""
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import conftest  # noqa: E402


class _Crash:
    def __init__(self, message):
        self.message = message


class _LongRepr:
    def __init__(self, message):
        self.reprcrash = _Crash(message)

    def __str__(self):
        return "<longrepr str form>"


class _Report:
    """The slice of a pytest report the hook actually reads."""

    def __init__(self, *, when="setup", outcome="failed", longrepr=None,
                 nodeid="tests/test_x.py::test_y[tui-docker]", duration=0.0):
        self.when = when
        self.outcome = outcome
        self.longrepr = longrepr
        self.nodeid = nodeid
        self.duration = duration


BARRIER = (
    "ConnectionBarrierTimeout: tui's transport never came online: still "
    "'connecting' after 60s"
)


def test_a_setup_error_prints_its_crash_line_immediately(capsys):
    conftest._print_live_reason(_Report(longrepr=_LongRepr(BARRIER)), "ERROR")
    out = capsys.readouterr().out
    assert "[e2e] ERROR" in out, out
    assert "tests/test_x.py::test_y[tui-docker]" in out, out
    assert "transport never came online" in out, out


def test_only_the_first_line_is_printed_so_one_error_stays_one_line(capsys):
    conftest._print_live_reason(
        _Report(longrepr=_LongRepr(BARRIER + "\nassert 0\n  in wait_until_online")),
        "ERROR",
    )
    out = capsys.readouterr().out.strip()
    # The leading "\n" the hook writes to break out of pytest's progress line is
    # stripped above, so what remains must be exactly one line.
    assert len(out.splitlines()) == 1, out
    assert "assert 0" not in out, out


def test_a_very_long_reason_is_truncated(capsys):
    conftest._print_live_reason(_Report(longrepr=_LongRepr("x" * 5000)), "ERROR")
    out = capsys.readouterr().out.strip()
    assert len(out) < 300, len(out)
    assert out.endswith("..."), out


def test_a_longrepr_without_a_reprcrash_still_reports_something(capsys):
    conftest._print_live_reason(_Report(longrepr="a bare string longrepr"), "ERROR")
    out = capsys.readouterr().out
    assert "a bare string longrepr" in out, out


def test_a_broken_longrepr_never_raises(capsys):
    class _Exploding:
        @property
        def reprcrash(self):
            raise RuntimeError("boom")

    # Never fatal: a diagnostics failure must not turn a run that produced real
    # results red -- the rule `_write_feature_ledger` already follows.
    conftest._print_live_reason(_Report(longrepr=_Exploding()), "ERROR")
    out = capsys.readouterr().out
    assert "[e2e] ERROR" in out, out
    assert "reason unavailable" in out, out


def test_the_hook_prints_for_a_setup_error(capsys):
    conftest.pytest_runtest_logreport(
        _Report(when="setup", outcome="failed", longrepr=_LongRepr(BARRIER))
    )
    assert "transport never came online" in capsys.readouterr().out


def test_the_hook_stays_silent_for_a_pass(capsys):
    """A pass has no reason to name, and one line per passing test would drown
    the signal this exists to surface."""
    conftest.pytest_runtest_logreport(_Report(when="call", outcome="passed"))
    assert capsys.readouterr().out.strip() == ""


def test_the_hook_prints_for_a_call_failure_too(capsys):
    """A FAILED test's traceback is held for the end-of-run summary exactly like
    an ERROR's, so it is lost in exactly the same way when a run never reaches
    that summary — which on Windows is the ORDINARY end of a long run, not an
    exotic one (pytest-timeout has no `signal` method there, so its `thread`
    method `os._exit`s the process out from under the terminal reporter).

    Measured 2026-09-21: an 11-file windows run printed `F` for two tests, wedged
    in a third, and was killed at 900 s with no summary and no counts — both
    failures' tracebacks existed only in pytest's memory and neither reproduced,
    so their causes are permanently unrecoverable.
    """
    conftest.pytest_runtest_logreport(
        _Report(when="call", outcome="failed", longrepr=_LongRepr("assert 1 == 2"))
    )
    out = capsys.readouterr().out
    assert "[e2e] FAILED" in out, out
    assert "tests/test_x.py::test_y[tui-docker]" in out, out
    assert "assert 1 == 2" in out, out


def test_a_failure_and_an_error_are_labelled_apart(capsys):
    """The two carry different diagnoses — an ERROR is a *precondition* failing,
    a FAILED is the subject under test — so a reader (and a grep) must be able to
    tell them apart on the line itself, not only by re-deriving it later from a
    summary the run may never print."""
    conftest.pytest_runtest_logreport(
        _Report(when="call", outcome="failed", longrepr=_LongRepr("assert 1 == 2"))
    )
    failed_out = capsys.readouterr().out
    conftest.pytest_runtest_logreport(
        _Report(when="setup", outcome="failed", longrepr=_LongRepr(BARRIER))
    )
    error_out = capsys.readouterr().out

    assert "[e2e] ERROR" not in failed_out, failed_out
    assert "[e2e] FAILED" not in error_out, error_out


def test_an_xfail_is_not_a_failure_and_stays_silent(capsys):
    """`outcome == "failed"` is the gate, and a wasxfail report is classified
    `xfailed`/`xpassed` before it ever reaches that gate. An expected failure is
    not a diagnosis anybody is waiting on."""
    xfail = _Report(when="call", outcome="skipped", longrepr=_LongRepr("assert 1 == 2"))
    xfail.wasxfail = "known bad"
    conftest.pytest_runtest_logreport(xfail)

    xpass = _Report(when="call", outcome="passed")
    xpass.wasxfail = "known bad"
    conftest.pytest_runtest_logreport(xpass)

    out = capsys.readouterr().out
    assert "[e2e]" not in out, out

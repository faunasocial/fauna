"""tier_1 proofs for the `--max-run-secs` run ceiling (conftest
`_run_ceiling_breach`, enforced in `pytest_runtest_setup`).

A whole-suite run's only wall-clock bound used to be the armed nightly gate's
own (its `GATE_MAX_SECS`). A tier_2/3 measuring run
launched on its own had none, and one held the one-wide `e2e_other` lane for
13 h in a stream of pegged-bridge timeouts. The ceiling stops a run from
starting any more tests once it has passed; the per-test `timeout = 900`
bounds the one still running. The recipe's parity with the gate's bound is
pinned in `test_merge_gate_check.py`.
"""

import pytest

import conftest

pytestmark = pytest.mark.tier_1


def test_no_ceiling_never_breaches():
    """Off by default: the inner loop names its tests and is never cut short."""
    assert conftest._run_ceiling_breach(0.0, 10**9, None, 5, 10) is None


def test_a_run_inside_the_ceiling_keeps_going():
    assert conftest._run_ceiling_breach(100.0, 100.0 + 3599, 3600, 5, 10) is None


def test_a_run_past_the_ceiling_is_aborted_as_no_complete_verdict():
    message = conftest._run_ceiling_breach(100.0, 100.0 + 3600, 3600, 612, 1227)
    assert message is not None
    assert "[RUN CEILING]" in message
    assert "612 of 1227" in message, "the abort says how far the run got"
    assert "no complete verdict" in message


def test_the_ceiling_counts_from_the_first_test_not_from_launch():
    """No test has started yet — collection or the slot queue — so no lane is
    held and nothing is bounded."""
    assert conftest._run_ceiling_breach(None, 10**9, 60, 0, 10) is None


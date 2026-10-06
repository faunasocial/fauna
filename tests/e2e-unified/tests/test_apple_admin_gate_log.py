"""tier_1: the apple admin-gate log reader (`helpers/apple_admin_gate.py`).

Why this exists at all. The reader is a **diagnostic**, and a diagnostic that is
itself unverified is worse than none: it is consulted exactly once, in a failing
run that already cost ~6 minutes on a machine where iOS e2e is expensive, and a
wrong answer there sends the next session down a wrong branch of a four-link
chain. That is precisely how this bug survived five reproductions and two
speculative fixes — an inference ("admin-tab absent ⇒ the probe never saw
admin") that was structurally invalid on iOS and had no test to say so.

So the reader is pinned headlessly, in milliseconds, with no app, no simulator
and no nest — the mechanism tested here, the last inch (does a real run emit
these lines) left to the e2e. `libs/fauna-log`'s writer shape is the contract:
a daily-rolling `fauna.log.<date>` under `<data_dir>/logs/`.
"""
from __future__ import annotations

import pytest

from helpers.apple_admin_gate import NEVER_RAN, admin_gate_log

pytestmark = pytest.mark.tier_1

# A verbatim slice of what the ring actually writes: the `tracing` fmt layer's
# line, with the shell message (`log_message`) carried as the event body.
_FIRED_TRUE = (
    "2026-08-02T09:14:02.117832Z  INFO fauna_client: [admin-gate] am_i_admin=true "
    "log_target=fauna.app"
)
_FIRED_FALSE = (
    "2026-08-02T09:14:02.004411Z  INFO fauna_client: [admin-gate] am_i_admin=false "
    "log_target=fauna.app"
)
_NO_CLIENT = (
    "2026-08-02T09:14:01.998010Z  INFO fauna_client: [admin-gate] no client → "
    "isAdmin=false log_target=fauna.app"
)
_ENABLED = (
    "2026-08-02T09:14:02.118904Z  INFO fauna_client: [admin-auto-default] "
    "require_confirm_to_activate auto-enabled for cb830844 log_target=fauna.accounts"
)
_REFUSED = (
    "2026-08-02T09:14:02.119510Z  INFO fauna_client: [admin-auto-default] refused for "
    "cb830844 (already on, or user-set) log_target=fauna.accounts"
)
_NOISE = "2026-08-02T09:14:00.000000Z  INFO fauna_ffi: fauna client logging initialised"


def _support_dir(tmp_path, lines, name="fauna.log.2026-08-02"):
    """An Application Support dir shaped exactly like the one the app writes."""
    log_dir = tmp_path / "logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    (log_dir / name).write_text("\n".join(lines) + "\n")
    return str(tmp_path)


def test_reports_every_link_of_the_chain_in_order(tmp_path):
    """The whole point: the line SET, oldest first, is the diagnosis."""
    support = _support_dir(tmp_path, [_NOISE, _NO_CLIENT, _FIRED_TRUE, _ENABLED])
    assert admin_gate_log(support) == [_NO_CLIENT, _FIRED_TRUE, _ENABLED]


def test_unrelated_lines_are_not_reported(tmp_path):
    """A ring holding only unrelated traffic must not read as chain evidence."""
    support = _support_dir(tmp_path, [_NOISE, _NOISE])
    assert admin_gate_log(support) == [f"({NEVER_RAN})"]


def test_the_never_ran_verdict_is_distinguishable_from_ran_and_returned_false(tmp_path):
    """THE load-bearing distinction, and the only one `session.is_admin` cannot
    make: both leave the flag false, but they need opposite fixes — "the probe
    never fired" is a lifecycle/mount bug, "it fired and got false" is a
    readiness/race bug. Nothing else in the harness separates them."""
    never = admin_gate_log(_support_dir(tmp_path / "a", [_NOISE]))
    ran = admin_gate_log(_support_dir(tmp_path / "b", [_FIRED_FALSE]))
    assert never == [f"({NEVER_RAN})"]
    assert ran == [_FIRED_FALSE]
    assert never != ran


def test_reads_across_a_midnight_roll(tmp_path):
    """`tracing_appender::rolling::daily` opens a new dated file; a run that
    crosses midnight must not lose the earlier half."""
    log_dir = tmp_path / "logs"
    log_dir.mkdir(parents=True)
    (log_dir / "fauna.log.2026-08-01").write_text(_NO_CLIENT + "\n")
    (log_dir / "fauna.log.2026-08-02").write_text(_FIRED_TRUE + "\n")
    assert admin_gate_log(str(tmp_path)) == [_NO_CLIENT, _FIRED_TRUE]


def test_keeps_the_most_recent_lines_when_capped(tmp_path):
    """A long-lived app re-probes on every reconnect; the tail is what matters,
    because the LAST probe is the one whose result the assertion contradicts."""
    lines = [_FIRED_FALSE] * 20 + [_FIRED_TRUE, _ENABLED]
    got = admin_gate_log(_support_dir(tmp_path, lines), limit=3)
    assert got == [_FIRED_FALSE, _FIRED_TRUE, _ENABLED]


def test_the_refusal_line_is_reported_too(tmp_path):
    """The OFF-sticks half of the journey asserts precisely this refusal, so a
    silent one is indistinguishable from the write never being attempted."""
    assert admin_gate_log(_support_dir(tmp_path, [_FIRED_TRUE, _REFUSED])) == [
        _FIRED_TRUE,
        _REFUSED,
    ]


@pytest.mark.parametrize(
    "support, expect",
    [
        (None, "no app support dir"),
        ("/nonexistent/fauna-e2e/support", "no fauna.log*"),
    ],
)
def test_unreadable_sources_explain_themselves_instead_of_raising(support, expect):
    """A diagnostic must never mask the assertion it explains (e2e rule 6)."""
    got = admin_gate_log(support)
    assert len(got) == 1 and expect in got[0]

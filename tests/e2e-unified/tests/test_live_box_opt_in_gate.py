"""tier_1: the `live_box` opt-in gate — a default sweep must never drive the
SHARED live nest.

The regression this pins (found 2026-08-03).
Two tri-machine live modules — `test_filesync_multiseat_live.py` and (retired
2026-09-30) `test_filesync_multiseat_engine_live.py` — were `tier_4`, `live_box`, `live_nest`,
OPT-IN, THREE-MACHINE rounds that only mean anything when an operator runs the
announce/seat handshake on three boxes at once (e2e-conventions.md convention 16
— the explicit-`go` round). Their only collection gate was
``skipif(SECRET is None)``, i.e. *"does this machine have an account seed"* —
and `~/.fauna-id` exists on every dev machine, so the gate never fired. A plain

    pytest tests/e2e-unified/tests/ --app tui

therefore collected all five of their tests, ran the two seat steps unattended
(each `pytest.fail`-ing on the empty `FAUNA_MULTISEAT_RUN_ID`, which reads as a
product regression), and — worse — ran the announce and both preflights, which
sign in to the live box and read a real folder. Two independent >1h sweeps
filed those failures as regressions on "do NOT reopen" items; they were neither
regressions nor flakes, just a missing gate.

Every OTHER `live_box` module gates on `FAUNA_LIVE_NEST_URL` — an env var an
operator sets deliberately — and the three `just e2e-live-*` recipes export
`FAUNA_E2E_LIVE=1`, which `tests/live/conftest.py` calls "REQUIRED explicit
opt-in (belt-and-suspenders)". So the opt-in already existed as a convention;
the multiseat pair had simply drifted off it (priority #4 — resolve drift,
don't match it). This gate puts the guarantee on the MARKER instead of on each
module remembering to hand-roll a skip, so a future live module inherits it.

What is pinned here, without spawning pytest or touching a box:

  1. A `live_box` node with no opt-in yields a skip reason.
  2. `FAUNA_E2E_LIVE=1` (any truthy spelling) satisfies the opt-in.
  3. `--nest live` satisfies it too — naming the live box on the command line
     IS the explicit request; requiring both would be redundant friction on an
     already-fragile three-machine ceremony.
  4. An unmarked node is never gated, whatever the env says.
"""

from types import SimpleNamespace

import pytest

import conftest

pytestmark = pytest.mark.tier_1


class _Node:
    """A collected-item stand-in: markers by name."""

    def __init__(self, markers=()):
        self._markers = set(markers)

    def get_closest_marker(self, name):
        return SimpleNamespace(name=name) if name in self._markers else None


def _live_box_node():
    return _Node(markers={"live_box"})


def test_live_box_without_opt_in_is_skipped(monkeypatch):
    monkeypatch.delenv("FAUNA_E2E_LIVE", raising=False)
    reason = conftest._live_box_opt_in_reason(_live_box_node(), live_mode=False)
    assert reason, "a live_box test with no opt-in must be skipped"
    # The message has to tell the operator how to opt in — a bare "skipped"
    # here reads as coverage (convention 7: a skip is not coverage).
    assert "FAUNA_E2E_LIVE" in reason


@pytest.mark.parametrize("value", ["1", "true", "TRUE", "yes"])
def test_env_opt_in_satisfies_the_gate(monkeypatch, value):
    monkeypatch.setenv("FAUNA_E2E_LIVE", value)
    assert conftest._live_box_opt_in_reason(_live_box_node(), live_mode=False) is None


@pytest.mark.parametrize("value", ["", "0", "false", "no"])
def test_falsey_env_does_not_satisfy_the_gate(monkeypatch, value):
    monkeypatch.setenv("FAUNA_E2E_LIVE", value)
    assert conftest._live_box_opt_in_reason(_live_box_node(), live_mode=False)


def test_nest_live_mode_satisfies_the_gate(monkeypatch):
    monkeypatch.delenv("FAUNA_E2E_LIVE", raising=False)
    assert conftest._live_box_opt_in_reason(_live_box_node(), live_mode=True) is None


def test_unmarked_node_is_never_gated(monkeypatch):
    monkeypatch.delenv("FAUNA_E2E_LIVE", raising=False)
    assert conftest._live_box_opt_in_reason(_Node(), live_mode=False) is None


def test_the_two_multiseat_modules_still_carry_the_marker():
    """The gate is marker-keyed, so the marker is the contract. If a future
    edit drops `live_box` from these modules they become reachable by a default
    sweep again — silently, which is exactly how this bug got in."""
    import re
    from pathlib import Path

    here = Path(__file__).parent
    for name in (
        "test_filesync_multiseat_live.py",
    ):
        src = (here / name).read_text(encoding="utf-8")
        assert re.search(r"pytest\.mark\.live_box", src), (
            f"{name} is an opt-in three-machine live round; it must carry "
            "`live_box` or a default sweep will drive the shared box with it"
        )

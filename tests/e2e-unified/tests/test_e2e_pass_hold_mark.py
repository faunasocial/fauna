"""The held failure's mark (mac-simulator-hold.md § Per-core tokens, *Under a pass
hold the pass's own simulators run at the floor* → *A red under the hold is no
red*).

A background-tester run (`FAUNA_RUN_ORIGIN=tester`) whose item fails while the
machine's schedule word read `held` — at the item's setup start, its call start or
the failing report — prints one `[e2e-held] <key>` line and records nothing in the
last-green directory or the feature ledger; a green under the hold is recorded; a
failure under `day` is an ordinary red; a session's own run never marks. The check
scripts' half (the marks read back, kept out of the reds, handed back by the
picker) is pinned beside the picker itself and, for the shell text the three
scripts share, here.
"""

import re
import types
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

import conftest  # noqa: E402  (tests/e2e-unified is on sys.path via conftest)
from helpers import last_green, pass_hold  # noqa: E402

_REPO = Path(__file__).resolve().parents[3]
_CHECKS = [_REPO / "scripts" / n for n in
           ("merge-gate-check.sh", "merge-gate-check-mac.sh", "merge-gate-check-win.sh")]
NODE = "tests/e2e-unified/tests/test_x.py::test_one[tui]"


# ── the pure state ───────────────────────────────────────────────────────────

def test_a_failure_after_a_held_sample_is_marked_once():
    marks = pass_hold.HeldMarks()
    marks.sample("a", "held")
    marks.sample("a", "day")
    assert marks.judge("a", True, "day") is True
    assert marks.judge("a", True, "day") is False  # one line per item
    assert marks.is_marked("a")


def test_a_pass_under_the_hold_is_never_marked():
    marks = pass_hold.HeldMarks()
    marks.sample("a", "held")
    assert marks.judge("a", False, "held") is False
    assert not marks.is_marked("a")


def test_a_failure_under_day_is_an_ordinary_red():
    marks = pass_hold.HeldMarks()
    marks.sample("a", "day")
    marks.sample("a", "night")
    assert marks.judge("a", True, "day") is False


def test_the_word_at_the_failing_report_alone_marks():
    marks = pass_hold.HeldMarks()
    assert marks.judge("a", True, "held") is True


def test_a_broken_reader_reads_day():
    def boom():
        raise OSError("no fence")
    assert pass_hold.read_word(boom) == "day"
    assert pass_hold.read_word(None) == "day"
    assert pass_hold.read_word(lambda: "held") == "held"


def test_only_the_tester_marks(monkeypatch):
    monkeypatch.delenv("FAUNA_RUN_ORIGIN", raising=False)
    assert not pass_hold.enabled()
    monkeypatch.setenv("FAUNA_RUN_ORIGIN", "session")
    assert not pass_hold.enabled()
    monkeypatch.setenv("FAUNA_RUN_ORIGIN", "tester")
    assert pass_hold.enabled()


# ── the conftest's report hook ────────────────────────────────────────────────

@pytest.fixture
def held_run(monkeypatch):
    """A tester run whose word reader answers `word[0]`, a fresh last-green run
    keyed NODE, and the conftest's ledger and last-green notes recorded."""
    word = ["day"]
    monkeypatch.setenv("FAUNA_RUN_ORIGIN", "tester")
    monkeypatch.setattr(conftest, "_PASS_HOLD_MARKS", None)
    monkeypatch.setattr(conftest, "_PASS_HOLD_READER", [lambda: word[0]])
    monkeypatch.setattr(conftest, "_warm_only_mid_run_refusal", lambda report: None)
    monkeypatch.setattr(conftest, "_print_live_reason", lambda report, label: None)
    ledger = []
    monkeypatch.setattr(conftest, "_feature_ledger_note",
                        lambda report, outcome: ledger.append((report.when, outcome)))
    saved_run, saved_obs = dict(last_green._run), dict(last_green._observed)
    last_green._run.clear()
    last_green._observed.clear()
    last_green._run.update({"commit": "c", "base": "b", "dirty": False, "origin": "tester",
                            "node_ids": {"nid": NODE}, "today": "2026-11-12"})
    monkeypatch.setattr(conftest, "_baseline_results", {})
    yield types.SimpleNamespace(word=word, ledger=ledger)
    last_green._run.clear()
    last_green._run.update(saved_run)
    last_green._observed.clear()
    last_green._observed.update(saved_obs)


def _report(when, outcome, duration=1.0):
    return types.SimpleNamespace(nodeid="nid", when=when, outcome=outcome,
                                 failed=outcome == "failed", duration=duration,
                                 longrepr=None)


def _item():
    return types.SimpleNamespace(nodeid="nid")


def test_a_failed_item_under_held_is_marked_and_unrecorded(held_run, capsys):
    held_run.word[0] = "held"
    conftest._pass_hold_sample("nid")  # setup start
    held_run.word[0] = "day"
    conftest.pytest_runtest_logreport(_report("setup", "passed"))
    conftest.pytest_runtest_call(_item())
    conftest.pytest_runtest_logreport(_report("call", "failed"))
    conftest.pytest_runtest_logreport(_report("teardown", "passed"))
    out = capsys.readouterr().out
    assert out.count(f"[e2e-held] {NODE}") == 1
    assert re.search(rf"^\[e2e-held\] {re.escape(NODE)}$", out, re.M)
    assert held_run.ledger == [] and last_green._observed["nid"]["outcome"] is None


def test_a_passed_item_under_held_is_recorded(held_run, capsys):
    held_run.word[0] = "held"
    conftest._pass_hold_sample("nid")
    conftest.pytest_runtest_call(_item())
    conftest.pytest_runtest_logreport(_report("call", "passed"))
    assert "[e2e-held]" not in capsys.readouterr().out
    assert held_run.ledger == [("call", "passed")]
    assert last_green._observed["nid"]["outcome"] == "green"


def test_a_failure_with_the_word_day_is_an_ordinary_red(held_run, capsys):
    conftest._pass_hold_sample("nid")
    conftest.pytest_runtest_call(_item())
    conftest.pytest_runtest_logreport(_report("call", "failed"))
    assert "[e2e-held]" not in capsys.readouterr().out
    assert held_run.ledger == [("call", "failed")]
    assert last_green._observed["nid"]["outcome"] == "red"


def test_a_sessions_run_never_marks(held_run, capsys, monkeypatch):
    monkeypatch.setenv("FAUNA_RUN_ORIGIN", "session")
    held_run.word[0] = "held"
    conftest._pass_hold_sample("nid")
    conftest.pytest_runtest_logreport(_report("call", "failed"))
    assert "[e2e-held]" not in capsys.readouterr().out
    assert held_run.ledger == [("call", "failed")]


def test_a_teardown_error_under_held_drops_the_green_its_call_noted(held_run, capsys):
    held_run.word[0] = "held"
    conftest.pytest_runtest_call(_item())
    conftest.pytest_runtest_logreport(_report("call", "passed"))
    conftest.pytest_runtest_logreport(_report("teardown", "failed"))
    assert f"[e2e-held] {NODE}" in capsys.readouterr().out
    assert last_green._observed["nid"]["outcome"] is None


def test_the_reader_is_build_slots_schedule_reader():
    """The word is read through build-slot.py's `_gate_check_schedule`, never a
    second parse of fence.json."""
    src = (_REPO / "tests" / "e2e-unified" / "conftest.py").read_text(encoding="utf-8")
    body = src[src.index("def _pass_hold_reader"):src.index("def _pass_hold_marks")]
    assert "_gate_check_schedule(mod._slot_dir())" in body and "fence.json" not in body


# ── the check scripts' shared text ───────────────────────────────────────────

def _fn(text, name):
    lines = text.splitlines(True)
    at = next(i for i, l in enumerate(lines) if l.lstrip().startswith(f"{name}() {{"))
    pad = len(lines[at]) - len(lines[at].lstrip())
    end = next(i for i in range(at + 1, len(lines))
               if lines[i].startswith(" " * pad + "}") and len(lines[i]) - len(lines[i].lstrip()) == pad)
    return "".join(l[pad:] if l.startswith(" " * pad) else l for l in lines[at:end + 1])


@pytest.mark.parametrize("name", ["delayed_e2e_shard", "delayed_e2e_action"])
def test_the_three_scripts_read_the_marks_alike(name):
    texts = {p.name: _fn(p.read_text(encoding="utf-8"), name) for p in _CHECKS}
    assert len(set(texts.values())) == 1, f"{name} drifted between {sorted(texts)}"
    body = next(iter(texts.values()))
    assert "shard.held" in body
    if name == "delayed_e2e_shard":
        # Every run of the shard — the shard's own and each flaky re-run — reads its marks.
        assert body.count("delayed_e2e_pick held") == 2
        assert "$n_unrun unrun, $n_held held" in body
    else:
        assert 'elif [ "$n_held" -gt 0 ]; then echo "held $n_held' in body


def test_the_three_scripts_hand_held_rows_and_keys_on():
    for p in _CHECKS:
        src = p.read_text(encoding="utf-8")
        assert '--held-file "$DELAYED_E2E_DIR/shard.held"' in _fn(src, "delayed_e2e_answer"), p
        drain = _fn(src, "delayed_e2e_drain")
        assert re.search(r'^\s*held\)\n\s*#.*\n(\s*#.*\n)*\s*delayed_e2e_answer "\$file" green ""\n'
                         r'\s*verify_tally "\$action" "held \$rest" ;;', drain, re.M), p
        assert ("delayed_e2e_pick pick $(declare -F relief_held >/dev/null && relief_held "
                ">/dev/null && echo --held-now)") in src, p

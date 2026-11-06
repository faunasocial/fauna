"""The e2e prebuild keeps the build-slot wrapper's `[build-slot]` lines.

`tests/common/nest.py`'s cargo builds (the nest, the sync agent, the workspace
examples) run under the build-slot wrapper with stdout/stderr captured: the cargo
JSON is parsed from stdout and the failure tails and the Windows `Access is denied`
fallback read stderr. Capturing threw away every grant decision (HI sized or kept,
the answer wait, the bound hold, `granted … after Ns`) on a successful build, so the
pytest log had no line for any of them. `_run_slot_build` echoes them as
`[prebuild] [build-slot] …` lines and leaves `result.stderr` whole.

Owner: docs/goal/architecture/core-bound.md § Implementation status today.
"""

import subprocess

import pytest
from common import nest

pytestmark = pytest.mark.tier_1

SLOT_LINE = "[build-slot] HI 8 → 2 cores, sized from its usage record: 3 builds"
GRANTED_LINE = "[build-slot] granted a build slot after 4s"
CARGO_JSON = '{"reason":"compiler-artifact","executable":"/x/fauna-nest"}'


def _fake_run(stdout: str, stderr: str, returncode: int = 0):
    def run(cmd, **kwargs):
        # The harness contract the callers rely on: both streams captured as text.
        assert kwargs.get("capture_output") is True and kwargs.get("text") is True
        return subprocess.CompletedProcess(cmd, returncode, stdout, stderr)

    return run


def test_success_echoes_the_slot_lines_as_prebuild_lines(capsys):
    stderr = f"{SLOT_LINE}\n   Compiling x v0.1\n{GRANTED_LINE}\n"
    result, instant = nest._run_slot_build(
        ["slot", "--", "cargo"], run=_fake_run(CARGO_JSON + "\n", stderr)
    )
    out = capsys.readouterr().out
    assert f"[prebuild] {SLOT_LINE}" in out
    assert f"[prebuild] {GRANTED_LINE}" in out
    assert "Compiling" not in out  # only the wrapper's lines, not cargo's chatter
    # The readers' inputs are untouched: stdout parses, stderr is whole.
    assert result.stdout.strip() == CARGO_JSON
    assert result.stderr == stderr
    assert isinstance(instant, float)


def test_failure_keeps_stderr_for_the_access_denied_fallback(capsys):
    stderr = f"{SLOT_LINE}\nerror: Access is denied. (os error 5)\n"
    result, _ = nest._run_slot_build(
        ["slot", "--", "cargo"], run=_fake_run("", stderr, returncode=101)
    )
    assert result.returncode == 101
    assert "Access is denied" in result.stderr  # what build_node's fallback reads
    assert f"[prebuild] {SLOT_LINE}" in capsys.readouterr().out


def test_no_stderr_is_fine(capsys):
    result, _ = nest._run_slot_build(["x"], run=_fake_run("out", ""))
    assert result.stdout == "out"
    assert capsys.readouterr().out == ""

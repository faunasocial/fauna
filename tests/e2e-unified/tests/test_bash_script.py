"""tier_1: ``helpers.bash_script.run_bash`` runs a snippet longer than the
command line `bash -c` survives on Windows. Git's bash cuts a `bash -c` argument a
little past 8 KB without a word — the cut reads as `syntax error: unexpected end
of file` (measured 2026-11-17: 8,160 bytes passed, 8,760 failed, both of Git's
bash executables) — and a test extracting a check script's functions into its
snippet passes that easily (the served Windows tier_1 run's six
``test_gate_check_ledger_publish.py`` reds)."""
from __future__ import annotations

import pytest

from helpers.bash_script import run_bash

pytestmark = pytest.mark.tier_1


def test_a_snippet_far_past_the_command_line_cut_runs_whole():
    body = "".join(f"    v{i}=\"{'x' * 60}\"\n" for i in range(400))  # ~28 KB
    snippet = f"long() {{\n{body}    echo \"$v399\"\n}}\nlong\necho tail-reached\n"
    assert len(snippet.encode()) > 3 * 8192
    out = run_bash(snippet, capture_output=True, text=True, timeout=120)
    assert out.returncode == 0, out.stderr
    assert out.stdout.splitlines() == ["x" * 60, "tail-reached"], out.stdout


def test_crlf_never_reaches_bash_and_arguments_pass_through():
    out = run_bash('printf "%s|" "$1" "$2"\r\necho\r\n', args=("a b", "c"),
                   capture_output=True, text=True, timeout=120)
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "a b|c|"

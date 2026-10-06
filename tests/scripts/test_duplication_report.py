"""Tests for scripts/duplication_report.py shared output module."""
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "scripts"))
from duplication_report import Finding, Report, render_markdown, merge_save


def test_finding_to_dict():
    f = Finding(
        id="tc-001",
        kind="textual-clone",
        severity="high",
        description="duplicated block",
        locations=[{"file": "a.rs", "lines": [10, 20]}],
        suggestion="extract helper",
    )
    d = f.to_dict()
    assert d["id"] == "tc-001"
    assert d["kind"] == "textual-clone"
    assert d["severity"] == "high"
    assert d["locations"] == [{"file": "a.rs", "lines": [10, 20]}]
    assert d["suggestion"] == "extract helper"


def test_finding_severity_order():
    high = Finding("a", "textual-clone", "high", "x", [])
    medium = Finding("b", "textual-clone", "medium", "x", [])
    low = Finding("c", "textual-clone", "low", "x", [])
    findings = sorted([low, medium, high], key=lambda f: f.severity_rank())
    assert [f.severity for f in findings] == ["high", "medium", "low"]


def test_report_to_dict():
    f = Finding("tc-001", "textual-clone", "high", "dup", [{"file": "a.rs", "lines": [1, 5]}])
    r = Report(tool="textual-clones", config={"min_lines": 5}, findings=[f])
    d = r.to_dict()
    assert d["tool"] == "textual-clones"
    assert d["summary"] == {"total": 1, "high": 1, "medium": 0, "low": 0}
    assert len(d["findings"]) == 1
    assert "timestamp" in d


def test_report_empty():
    r = Report(tool="textual-clones", config={}, findings=[])
    d = r.to_dict()
    assert d["summary"] == {"total": 0, "high": 0, "medium": 0, "low": 0}


def test_render_markdown():
    f = Finding(
        "tc-001", "textual-clone", "high", "18-line dup",
        [{"file": "a.rs", "lines": [10, 27]}, {"file": "b.rs", "lines": [5, 22]}],
        suggestion="extract helper",
    )
    r = Report(tool="textual-clones", config={"min_lines": 5}, findings=[f])
    md = render_markdown([r])
    assert "## High severity (1)" in md
    assert "tc-001" in md
    assert "`a.rs:10-27`" in md
    assert "> extract helper" in md
    assert "| Textual clones" in md


def test_render_markdown_empty():
    r = Report(tool="textual-clones", config={}, findings=[])
    md = render_markdown([r])
    assert "No duplication findings" in md


def test_merge_save_creates_files():
    with tempfile.TemporaryDirectory() as tmpdir:
        out = Path(tmpdir)
        f = Finding("tc-001", "textual-clone", "high", "dup", [{"file": "a.rs", "lines": [1, 5]}])
        r = Report(tool="textual-clones", config={}, findings=[f])
        merge_save(r, json_path=out / "findings.json", md_path=out / "findings.md")
        assert (out / "findings.json").exists()
        assert (out / "findings.md").exists()
        data = json.loads((out / "findings.json").read_text())
        assert len(data) == 1
        assert data[0]["tool"] == "textual-clones"


def test_merge_save_replaces_own_tool():
    with tempfile.TemporaryDirectory() as tmpdir:
        out = Path(tmpdir)
        f1 = Finding("tc-001", "textual-clone", "high", "dup", [{"file": "a.rs", "lines": [1, 5]}])
        r1 = Report(tool="textual-clones", config={}, findings=[f1])
        merge_save(r1, json_path=out / "findings.json", md_path=out / "findings.md")

        f2 = Finding("cc-001", "cross-app-violation", "high", "inlined", [{"file": "b.kt", "lines": [10, 20]}])
        r2 = Report(tool="cross-app", config={}, findings=[f2])
        merge_save(r2, json_path=out / "findings.json", md_path=out / "findings.md")

        data = json.loads((out / "findings.json").read_text())
        assert len(data) == 2
        tools = [d["tool"] for d in data]
        assert "textual-clones" in tools
        assert "cross-app" in tools

        r3 = Report(tool="textual-clones", config={}, findings=[])
        merge_save(r3, json_path=out / "findings.json", md_path=out / "findings.md")
        data = json.loads((out / "findings.json").read_text())
        assert len(data) == 2
        tc = [d for d in data if d["tool"] == "textual-clones"][0]
        assert tc["summary"]["total"] == 0

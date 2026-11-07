"""The certification grade: `scripts/grade-store-certification.py` reads a Windows App
Certification Kit report and fails a Store build on a test that did not pass, unless
the allowlist names it with a reason (installers/windows.md § Store distribution
(MSIX) → Build & packaging shape).

Pure parsing, every machine. The report below keeps the shape the kit wrote on the
hosted `windows-latest` image (kit 10.0.26100.9457, 2026-11-06): `<REPORT
OVERALL_RESULT=…>` → `<REQUIREMENT>` → `<TEST NAME=… OPTIONAL=…>` with `<MESSAGE
TEXT=…>` children and the verdict in `<RESULT><![CDATA[…]]>`.
"""

import importlib.util
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = Path(__file__).resolve().parents[5]
_SCRIPT = _REPO_ROOT / "scripts" / "grade-store-certification.py"
_ALLOWLIST = _REPO_ROOT / "apps" / "fauna-windows" / "installer" / "store" / "certification-allowlist.toml"


def _grader():
    spec = importlib.util.spec_from_file_location("grade_store_certification", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod  # dataclasses resolve the module by name
    spec.loader.exec_module(mod)
    return mod


def _test(name, result, optional="TRUE", messages=()):
    msgs = "".join(f'<MESSAGE TEXT="{m}" />' for m in messages)
    msgs = f"<MESSAGES>{msgs}</MESSAGES>" if msgs else "<MESSAGES />"
    return (f'<TEST INDEX="1" NAME="{name}" DESCRIPTION="d" EXECUTIONTIME="0" OPTIONAL="{optional}">'
            f"{msgs}<RESULT><![CDATA[{result}]]></RESULT></TEST>")


def _report(*tests, overall="PASS"):
    return ('<?xml version="1.0" encoding="utf-8"?>\n'
            "<?xml-stylesheet type='text/xsl' href='results.xsl'?>\n"
            f'<REPORT xml:lang="en-US" OVERALL_RESULT="{overall}" APP_TYPE="Centennial">'
            '<REQUIREMENTS><REQUIREMENT NUMBER="12" TITLE="Package compliance test" RATIONALE="r">'
            + "".join(tests) + "</REQUIREMENT></REQUIREMENTS></REPORT>")


def _run(tmp_path, report, allowlist="", arch="x64"):
    rp, ap = tmp_path / "report.xml", tmp_path / "allow.toml"
    rp.write_text(report, encoding="utf-8")
    ap.write_text(allowlist, encoding="utf-8")
    return _grader().main(["--report", str(rp), "--arch", arch, "--allowlist", str(ap)])


def test_a_report_of_passes_and_warnings_passes(tmp_path, capsys):
    rc = _run(tmp_path, _report(_test("App manifest", "PASS", "FALSE"),
                                _test("DPIAwarenessValidation", "WARNING", messages=["not DPI aware"]),
                                overall="WARNING"))
    out = capsys.readouterr().out
    assert rc == 0, out
    assert "CERTIFICATION GRADE: PASS (x64) -- 2 tests" in out
    assert "WARNING: DPIAwarenessValidation" in out and "kit message (data): not DPI aware" in out


@pytest.mark.parametrize("optional", ["TRUE", "FALSE"])
def test_any_fail_fails_the_grade_optional_or_not(tmp_path, capsys, optional):
    rc = _run(tmp_path, _report(_test("App resources", "FAIL", optional, ["logo too small"])))
    out = capsys.readouterr().out
    assert rc == 1
    assert "FAILED: App resources [FAIL" in out and "CERTIFICATION GRADE: FAIL (x64)" in out


def test_an_allowlisted_fail_passes_only_on_its_arch(tmp_path, capsys):
    allow = ('[[allow]]\ntest = "App resources"\nresult = "FAIL"\narch = ["arm64"]\n'
             'why = "cannot apply on a hosted runner for this reason"\n')
    report = _report(_test("App resources", "FAIL"))
    assert _run(tmp_path, report, allow, arch="arm64") == 0
    assert "ALLOWLISTED: App resources" in capsys.readouterr().out
    assert _run(tmp_path, report, allow, arch="x64") == 1


def test_an_allowlist_entry_for_another_verdict_does_not_cover_a_fail(tmp_path):
    allow = ('[[allow]]\ntest = "App resources"\nresult = "NOT RUN"\n'
             'why = "cannot apply on a hosted runner for this reason"\n')
    assert _run(tmp_path, _report(_test("App resources", "FAIL")), allow) == 1


@pytest.mark.parametrize("report", [
    "not xml",
    "<OTHER />",
    '<REPORT><REQUIREMENTS /></REPORT>',
    '<REPORT OVERALL_RESULT="PASS"><REQUIREMENTS /></REPORT>',
])
def test_the_grade_fails_closed_on_a_report_it_cannot_read(tmp_path, capsys, report):
    assert _run(tmp_path, report) == 1
    assert "CERTIFICATION GRADE: FAIL (x64) --" in capsys.readouterr().out


def test_an_unknown_verdict_fails(tmp_path):
    assert _run(tmp_path, _report(_test("Branding", "ABORTED"))) == 1


def test_a_missing_report_fails(tmp_path):
    ap = tmp_path / "allow.toml"
    ap.write_text("", encoding="utf-8")
    rc = _grader().main(["--report", str(tmp_path / "none.xml"), "--arch", "x64", "--allowlist", str(ap)])
    assert rc == 1


@pytest.mark.parametrize("entry", [
    'test = "X"\nresult = "FAIL"\n',                                   # no reason
    'test = "X"\nresult = "PASS"\nwhy = "a reason long enough here"\n',  # PASS needs no entry
    'test = "X"\nresult = "FAIL"\narch = "x86"\nwhy = "a reason long enough here"\n',
    'test = "X"\nresult = "FAIL"\nwhy = "a reason long enough here"\nexpires = 1\n',
])
def test_a_malformed_allowlist_entry_fails_the_grade(tmp_path, entry):
    assert _run(tmp_path, _report(_test("X", "FAIL")), "[[allow]]\n" + entry) == 1


def test_the_checked_in_allowlist_parses_and_every_entry_says_why():
    _grader().parse_allowlist(_ALLOWLIST.read_text(encoding="utf-8"))

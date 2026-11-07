#!/usr/bin/env python3
"""Grade a Windows App Certification Kit report for a Fauna Store package.

    python scripts/grade-store-certification.py --report wack-x64.xml --arch x64 \\
        --allowlist apps/fauna-windows/installer/store/certification-allowlist.toml

The public `release-store-windows.yml` workflow certifies a test-signed COPY of each
Store package with the kit (`appcert.exe test`) on its hosted runner and grades the
report with this script: every test the kit ran is read by its own verdict, and the
job fails on a test that did not pass unless the allowlist names that test, that
verdict and this arch with a reason. Design of record:
docs/goal/architecture/installers/windows.md § Store distribution (MSIX) → Build &
packaging shape.

The report is tool output. Its message texts are printed as data for the reader of
the job log and are never acted on.

Fails closed: a report that does not parse, carries no `OVERALL_RESULT`, or lists no
test at all is a failed grade, as is any verdict other than PASS, WARNING or FAIL.
A WARNING is printed and does not fail the grade (the kit itself rates a run with
warnings below a pass but certifiable). Standard library only: it runs on the hosted
runner's own interpreter from the workflow's sparse grader checkout.
"""
from __future__ import annotations

import argparse
import sys
import tomllib
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path

ARCHES = ("x64", "arm64")
KNOWN = ("PASS", "WARNING", "FAIL")


@dataclass(frozen=True)
class Test:
    name: str
    result: str
    optional: bool
    messages: tuple[str, ...]


@dataclass(frozen=True)
class Allowed:
    test: str
    result: str
    arches: tuple[str, ...]
    why: str


class GradeError(Exception):
    """The report or the allowlist cannot be graded at all."""


def parse_report(text: str) -> tuple[str, list[Test]]:
    """The report's overall result and every test it lists, in report order."""
    try:
        root = ET.fromstring(text)
    except ET.ParseError as e:
        raise GradeError(f"the report does not parse as XML: {e}") from e
    if root.tag != "REPORT":
        raise GradeError(f"the report's root is <{root.tag}>, not <REPORT>")
    overall = (root.get("OVERALL_RESULT") or "").strip().upper()
    if not overall:
        raise GradeError("the report carries no OVERALL_RESULT")
    tests = []
    for el in root.iter("TEST"):
        name = (el.get("NAME") or "").strip()
        res = el.find("RESULT")
        result = ((res.text if res is not None else "") or "").strip().upper()
        if not name:
            raise GradeError("a test in the report has no NAME")
        msgs = tuple(m.get("TEXT", "").strip() for m in el.iter("MESSAGE"))
        tests.append(Test(name, result or "(NO RESULT)",
                          (el.get("OPTIONAL") or "").upper() == "TRUE", msgs))
    if not tests:
        raise GradeError("the report lists no test: the kit ran nothing")
    return overall, tests


def parse_allowlist(text: str) -> list[Allowed]:
    """Every `[[allow]]` entry; each names a test, a verdict, its arches and why."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise GradeError(f"the allowlist does not parse as TOML: {e}") from e
    unknown = set(data) - {"allow"}
    if unknown:
        raise GradeError(f"the allowlist has unknown top-level keys: {sorted(unknown)}")
    out = []
    for i, entry in enumerate(data.get("allow", [])):
        where = f"allowlist entry {i + 1}"
        extra = set(entry) - {"test", "result", "arch", "why"}
        if extra:
            raise GradeError(f"{where} has unknown keys: {sorted(extra)}")
        test, result, why = entry.get("test"), entry.get("result"), entry.get("why")
        arches = entry.get("arch", list(ARCHES))
        if not isinstance(test, str) or not test.strip():
            raise GradeError(f"{where} names no test")
        if not isinstance(result, str) or result.upper() == "PASS":
            raise GradeError(f"{where} ({test}) must name the non-passing verdict it accepts")
        if not isinstance(why, str) or len(why.strip()) < 20:
            raise GradeError(f"{where} ({test}) carries no reason: every entry says why")
        if isinstance(arches, str):
            arches = [arches]
        if not arches or any(a not in ARCHES for a in arches):
            raise GradeError(f"{where} ({test}) names an arch outside {ARCHES}: {arches}")
        out.append(Allowed(test.strip(), result.strip().upper(), tuple(arches), why.strip()))
    return out


def grade(tests: list[Test], allowed: list[Allowed], arch: str) -> tuple[list[str], list[str], list[str]]:
    """(failures, accepted, warnings) as printable lines; the grade passes iff no failure."""
    failures, accepted, warnings = [], [], []
    for t in tests:
        if t.result == "PASS":
            continue
        tag = f"{t.name} [{t.result}{', optional' if t.optional else ''}]"
        if t.result == "WARNING":
            warnings.append(tag)
            continue
        hit = next((a for a in allowed
                    if a.test == t.name and a.result == t.result and arch in a.arches), None)
        if hit is not None:
            accepted.append(f"{tag} -- allowlisted: {hit.why}")
        elif t.result in KNOWN:
            failures.append(tag)
        else:
            failures.append(f"{tag} -- a verdict this grade does not know")
    return failures, accepted, warnings


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--report", required=True, type=Path)
    ap.add_argument("--arch", required=True, choices=ARCHES)
    ap.add_argument("--allowlist", required=True, type=Path)
    a = ap.parse_args(argv)
    try:
        if not a.report.is_file():
            raise GradeError(f"no report at {a.report}: the kit wrote none")
        overall, tests = parse_report(a.report.read_text(encoding="utf-8-sig"))
        allowed = parse_allowlist(a.allowlist.read_text(encoding="utf-8"))
    except GradeError as e:
        print(f"CERTIFICATION GRADE: FAIL ({a.arch}) -- {e}")
        return 1
    failures, accepted, warnings = grade(tests, allowed, a.arch)
    by_name = {t.name: t for t in tests}
    for label, lines in (("FAILED", failures), ("ALLOWLISTED", accepted), ("WARNING", warnings)):
        for line in lines:
            print(f"  {label}: {line}")
            name = line.split(" [", 1)[0]
            for m in by_name[name].messages if name in by_name else ():
                print(f"      kit message (data): {m}")
    used = {ln.split(" [", 1)[0] for ln in accepted}
    for e in allowed:
        if a.arch in e.arches and e.test not in used:
            print(f"  NOTE: allowlist entry {e.test!r} ({e.result}) matched no test on {a.arch}")
    verdict = "FAIL" if failures else "PASS"
    print(f"CERTIFICATION GRADE: {verdict} ({a.arch}) -- {len(tests)} tests, kit overall "
          f"{overall}, {len(failures)} failed, {len(accepted)} allowlisted, {len(warnings)} warnings")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

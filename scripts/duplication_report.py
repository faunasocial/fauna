"""Shared output schema and report generation for duplication detection scripts.

All three lint scripts (textual-clones, cross-client, structural) use this
module for consistent JSON output, severity ranking, --save merge logic,
and Markdown report rendering.
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

SEVERITY_RANK = {"high": 0, "medium": 1, "low": 2}

TOOL_DISPLAY = {
    "textual-clones": "Textual clones",
    "cross-client": "Cross-client",
    "structural": "Structural",
}


@dataclass
class Finding:
    id: str
    kind: str  # textual-clone | cross-client-violation | structural-duplicate
    severity: str  # high | medium | low
    description: str
    locations: list[dict]
    suggestion: str = ""

    def severity_rank(self) -> int:
        return SEVERITY_RANK.get(self.severity, 99)

    def to_dict(self) -> dict:
        d = {
            "id": self.id,
            "kind": self.kind,
            "severity": self.severity,
            "description": self.description,
            "locations": self.locations,
        }
        if self.suggestion:
            d["suggestion"] = self.suggestion
        return d


@dataclass
class Report:
    tool: str
    config: dict
    findings: list[Finding]
    timestamp: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat(timespec="seconds"))

    def to_dict(self) -> dict:
        counts = {"total": 0, "high": 0, "medium": 0, "low": 0}
        for f in self.findings:
            counts["total"] += 1
            counts[f.severity] = counts.get(f.severity, 0) + 1
        return {
            "tool": self.tool,
            "timestamp": self.timestamp,
            "config": self.config,
            "findings": [f.to_dict() for f in self.findings],
            "summary": counts,
        }


def render_markdown(reports: list[Report]) -> str:
    """Render a list of reports into a Markdown summary."""
    all_findings: list[Finding] = []
    for r in reports:
        all_findings.extend(r.findings)

    if not all_findings:
        return "# Duplication Findings\n\nNo duplication findings.\n"

    all_findings.sort(key=lambda f: (f.severity_rank(), f.id))

    lines = ["# Duplication Findings", ""]
    ts = reports[0].timestamp if reports else ""
    lines.append(f"Generated: {ts}")
    lines.append("")

    for sev in ("high", "medium", "low"):
        group = [f for f in all_findings if f.severity == sev]
        if not group:
            continue
        lines.append(f"## {sev.capitalize()} severity ({len(group)})")
        lines.append("")
        for f in group:
            kind_label = f.kind.replace("-", " ")
            lines.append(f"### {f.id}: {f.description} ({kind_label})")
            for loc in f.locations:
                start, end = loc["lines"]
                lines.append(f"- `{loc['file']}:{start}-{end}`")
            if f.suggestion:
                lines.append(f"\n> {f.suggestion}")
            lines.append("")

    lines.append("## Summary")
    lines.append("| Kind | High | Medium | Low | Total |")
    lines.append("|------|------|--------|-----|-------|")
    for r in reports:
        s = r.to_dict()["summary"]
        name = TOOL_DISPLAY.get(r.tool, r.tool)
        lines.append(f"| {name} | {s['high']} | {s['medium']} | {s['low']} | {s['total']} |")
    lines.append("")

    return "\n".join(lines)


def merge_save(report: Report, json_path: Path, md_path: Path) -> None:
    """Save report, merging with existing data from other tools.

    Reads the existing JSON file, replaces the entry for this report's tool,
    and rewrites both JSON and Markdown files.
    """
    existing: list[dict] = []
    if json_path.exists():
        try:
            existing = json.loads(json_path.read_text())
        except (json.JSONDecodeError, OSError):
            existing = []

    other = [e for e in existing if e.get("tool") != report.tool]
    other.append(report.to_dict())
    other.sort(key=lambda e: e.get("tool", ""))

    json_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(other, indent=2) + "\n")

    all_reports = []
    for entry in other:
        findings = [
            Finding(
                id=f["id"],
                kind=f["kind"],
                severity=f["severity"],
                description=f["description"],
                locations=f["locations"],
                suggestion=f.get("suggestion", ""),
            )
            for f in entry.get("findings", [])
        ]
        all_reports.append(Report(
            tool=entry["tool"],
            config=entry.get("config", {}),
            findings=findings,
            timestamp=entry.get("timestamp", ""),
        ))
    md_path.write_text(render_markdown(all_reports))

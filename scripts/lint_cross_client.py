#!/usr/bin/env -S uv run --quiet
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""Cross-client pattern lint: detect inlined crypto/auth logic that should use shared libs.

Loads a pattern catalog (cross-client-patterns.yaml), scans each client app
directory for marker co-occurrence within a sliding window, and reports
violations via the shared report module.

Usage:
    python3 scripts/lint_cross_client.py [--save] [--pattern ID] [--client NAME]
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPT_DIR))
from duplication_report import Finding, Report, merge_save

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

CLIENT_DIRS: dict[str, tuple[str, set[str]]] = {
    "web": ("apps/fauna-web/src", {".ts", ".svelte", ".js"}),
    "windows": ("apps/fauna-windows", {".cs", ".xaml"}),
    "android": ("apps/fauna-android", {".kt", ".java"}),
    "apple": ("apps/fauna-apple", {".swift"}),
    "linux": ("apps/fauna-linux/src", {".rs"}),
}

DEFAULT_WINDOW = 30

# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------

def load_catalog(path: Path) -> list[dict]:
    """Load YAML catalog, return list of pattern dicts."""
    import yaml
    data = yaml.safe_load(path.read_text())
    if not data or "patterns" not in data:
        return []
    return data["patterns"]


def check_markers_in_window(
    lines: list[str], markers: list[str], window_size: int = 30
) -> tuple[int, int] | None:
    """Check if all markers co-occur within a sliding window.

    Returns (start_1indexed, end_1indexed) of the tightest window containing
    all markers, or None if not all markers appear within any window.
    """
    n = len(lines)
    if n == 0 or not markers:
        return None

    # Find all line indices where each marker appears (0-indexed)
    marker_lines: dict[str, list[int]] = {m: [] for m in markers}
    for i, line in enumerate(lines):
        lower = line.lower()
        for m in markers:
            if m.lower() in lower:
                marker_lines[m].append(i)

    # Check if any marker is entirely absent
    for m in markers:
        if not marker_lines[m]:
            return None

    # Sliding window: check each window of window_size lines
    for start in range(n - window_size + 1):
        end = start + window_size - 1
        all_present = True
        for m in markers:
            if not any(start <= idx <= end for idx in marker_lines[m]):
                all_present = False
                break
        if all_present:
            # Find the tightest range within this window
            first_marker_line = n
            last_marker_line = 0
            for m in markers:
                hits_in_window = [idx for idx in marker_lines[m] if start <= idx <= end]
                first_marker_line = min(first_marker_line, min(hits_in_window))
                last_marker_line = max(last_marker_line, max(hits_in_window))
            return (first_marker_line + 1, last_marker_line + 1)

    # Also handle case where file is shorter than window_size
    if n < window_size:
        all_present = all(len(marker_lines[m]) > 0 for m in markers)
        if all_present:
            first_marker_line = n
            last_marker_line = 0
            for m in markers:
                first_marker_line = min(first_marker_line, min(marker_lines[m]))
                last_marker_line = max(last_marker_line, max(marker_lines[m]))
            return (first_marker_line + 1, last_marker_line + 1)

    return None


def scan_file(
    path: Path, markers: list[str], window_size: int = 30
) -> tuple[int, int] | None:
    """Read file, call check_markers_in_window. Return line range or None."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    lines = text.splitlines()
    return check_markers_in_window(lines, markers, window_size)


def scan_file_with_negatives(
    path: Path,
    markers: list[str],
    negative_markers: list[str],
    window_size: int = 30,
) -> tuple[int, int] | None:
    """Return the clone range, or None if none or if a negative marker appears.

    A file is suppressed the moment any negative marker substring is found
    anywhere in the file. This models "the file already consumes the shared
    impl" — if you can see the shared import, we trust you.
    """
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    for neg in negative_markers:
        if neg and neg in text:
            return None
    lines = text.splitlines()
    return check_markers_in_window(lines, markers, window_size)


def should_exclude(rel_path: str, excludes: list[str]) -> bool:
    """Check if path matches any exclude prefix."""
    for prefix in excludes:
        if rel_path.startswith(prefix):
            return True
    return False


def scan_client(
    client_name: str,
    client_dir: str,
    extensions: set[str],
    pattern: dict,
    root: Path,
) -> list[Finding]:
    """Scan all files in a client dir for a pattern.

    Returns list of Finding objects for any violations found.
    """
    base = root / client_dir
    if not base.is_dir():
        return []

    markers = pattern.get("markers", [])
    excludes = pattern.get("exclude", [])
    negative_markers = pattern.get("negative_markers", [])
    exclude_files = pattern.get("exclude_files", [])  # list of regex strings
    exclude_regex = [re.compile(p) for p in exclude_files]
    severity = pattern.get("severity", "medium")
    pattern_id = pattern["id"]
    description = pattern.get("description", pattern_id)

    findings: list[Finding] = []

    for f in sorted(base.rglob("*")):
        if not f.is_file():
            continue
        if f.suffix not in extensions:
            continue

        try:
            rel = f.relative_to(root).as_posix()
        except ValueError:
            rel = f.as_posix()

        if should_exclude(rel, excludes):
            continue

        if any(r.search(rel) for r in exclude_regex):
            continue

        result = scan_file_with_negatives(f, markers, negative_markers)
        if result is not None:
            start, end = result
            finding_id = f"xc-{pattern_id}-{client_name}"
            findings.append(Finding(
                id=finding_id,
                kind="cross-client-violation",
                severity=severity,
                description=f"[{client_name}] {description}",
                locations=[{"file": rel, "lines": [start, end]}],
                suggestion=f"Use shared impl: {pattern.get('shared_impl', {})}",
            ))

    return findings


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Detect cross-client pattern violations (inlined crypto/auth logic)."
    )
    parser.add_argument(
        "--save", action="store_true",
        help="Save the findings report (JSON + Markdown) to disk",
    )
    parser.add_argument(
        "--pattern", type=str, default="",
        help="Only check a specific pattern ID (e.g., auth-body-construction)",
    )
    parser.add_argument(
        "--client", type=str, default="",
        help="Only check a specific client (e.g., web, windows, android, apple, linux)",
    )
    args = parser.parse_args()

    catalog_path = SCRIPT_DIR / "cross-client-patterns.yaml"
    if not catalog_path.exists():
        print(f"Error: catalog not found at {catalog_path}", file=sys.stderr)
        sys.exit(1)

    patterns = load_catalog(catalog_path)
    if args.pattern:
        patterns = [p for p in patterns if p["id"] == args.pattern]
        if not patterns:
            print(f"Error: pattern '{args.pattern}' not found in catalog", file=sys.stderr)
            sys.exit(1)

    clients = dict(CLIENT_DIRS)
    if args.client:
        if args.client not in clients:
            print(f"Error: unknown client '{args.client}'. Choose from: {', '.join(clients)}", file=sys.stderr)
            sys.exit(1)
        clients = {args.client: clients[args.client]}

    all_findings: list[Finding] = []

    for pattern in patterns:
        for client_name, (client_dir, extensions) in clients.items():
            findings = scan_client(client_name, client_dir, extensions, pattern, ROOT)
            all_findings.extend(findings)

    # Re-number findings
    for i, f in enumerate(all_findings, start=1):
        f.id = f"xc-{i:03d}"

    report = Report(
        tool="cross-client",
        config={
            "catalog": str(catalog_path.relative_to(ROOT)),
            "pattern_filter": args.pattern or "all",
            "client_filter": args.client or "all",
        },
        findings=all_findings,
    )

    # Print summary
    counts = {"high": 0, "medium": 0, "low": 0}
    for f in all_findings:
        counts[f.severity] = counts.get(f.severity, 0) + 1

    print(f"Cross-client pattern scan complete: {len(all_findings)} findings "
          f"(high={counts['high']}, medium={counts['medium']}, low={counts['low']})")
    print()

    for f in all_findings:
        sev_marker = {"high": "!!!", "medium": "!!", "low": "!"}[f.severity]
        print(f"  {f.id} [{f.severity}] {sev_marker} {f.description}")
        for loc in f.locations:
            print(f"    - {loc['file']}:{loc['lines'][0]}-{loc['lines'][1]}")
        print()

    if args.save:
        json_path = ROOT / "docs" / "todo" / "duplication-findings.json"
        md_path = ROOT / "docs" / "todo" / "duplication-findings.md"
        merge_save(report, json_path, md_path)
        print(f"Saved to {json_path} and {md_path}")


if __name__ == "__main__":
    main()

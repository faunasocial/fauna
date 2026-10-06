#!/usr/bin/env -S uv run --quiet
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""Textual clone detection: find duplicated code blocks across the codebase.

Normalizes source files (strips comments, collapses whitespace), fingerprints
sliding windows of lines via SHA-256, clusters identical hashes, filters noise
using duplication-ignore.yaml, and outputs findings via the shared report module.

Usage:
    python3 scripts/lint_textual_clones.py [--min-lines 5] [--save] [--lang rs,go] [--path bins/]
"""
from __future__ import annotations

import argparse
import hashlib
import re
import sys
from dataclasses import dataclass
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPT_DIR))
from duplication_report import Finding, Report, merge_save

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

LANG_EXTENSIONS: set[str] = {".rs", ".go", ".ts", ".svelte", ".cs", ".kt", ".swift", ".py"}

# Maps extension -> (line_comment_prefix, block_start, block_end)
# None means the language doesn't use that comment style.
COMMENT_PATTERNS: dict[str, tuple[str | None, str | None, str | None]] = {
    ".rs":     ("//", "/*", "*/"),
    ".go":     ("//", "/*", "*/"),
    ".ts":     ("//", "/*", "*/"),
    ".svelte": ("//", "/*", "*/"),
    ".cs":     ("//", "/*", "*/"),
    ".kt":     ("//", "/*", "*/"),
    ".swift":  ("//", "/*", "*/"),
    ".py":     ("#",  None,  None),
}

SCAN_DIRS: list[str] = ["libs", "bins", "apps", "tests"]

EXCLUDE_PREFIXES: list[str] = [
    "target/",
    "node_modules/",
    # Editor/agent scratch state, incl. any linked checkouts nested under it.
    ".claude/",
    # Generated i18n files
    "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/",
    "apps/fauna-web/src/lib/i18n/strings.ts",
    "apps/fauna-windows/FaunaApp/FaunaApp/Strings/",
    "apps/fauna-android/app/src/main/res/values/i18n_strings.xml",
    "apps/fauna-ios/Fauna/Resources/en.lproj/Localizable.strings",
    "libs/fauna-i18n/src/strings.rs",
    "tests/e2e-unified/i18n/strings.py",
]

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------

@dataclass
class CloneGroup:
    normalized_lines: list[str]
    locations: list[tuple[str, int, int]]  # (file, start_1indexed, end_1indexed)

# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------

def normalize_lines(lines: list[str], ext: str) -> list[str]:
    """Strip comments (language-aware) and collapse whitespace.

    Returns a list of normalized strings, one per input line. Empty/blank
    lines are preserved as empty strings.
    """
    patterns = COMMENT_PATTERNS.get(ext, ("//", "/*", "*/"))
    line_prefix, block_start, block_end = patterns

    result: list[str] = []
    in_block = False

    for line in lines:
        if in_block:
            # Look for end of block comment
            if block_end is not None:
                idx = line.find(block_end)
                if idx >= 0:
                    in_block = False
                    line = line[idx + len(block_end):]
                else:
                    result.append("")
                    continue
            else:
                # No block comments for this language
                in_block = False

        # Strip inline block comments (may be multiple on one line)
        if block_start is not None and block_end is not None:
            while True:
                start_idx = line.find(block_start)
                if start_idx < 0:
                    break
                end_idx = line.find(block_end, start_idx + len(block_start))
                if end_idx >= 0:
                    line = line[:start_idx] + line[end_idx + len(block_end):]
                else:
                    # Block comment starts but doesn't end on this line
                    line = line[:start_idx]
                    in_block = True
                    break

        # Strip line comments (but not :// in URLs)
        if line_prefix is not None:
            idx = line.find(line_prefix)
            while idx >= 0:
                if ext != ".py" and idx > 0 and line[idx - 1] == ":":
                    idx = line.find(line_prefix, idx + len(line_prefix))
                else:
                    line = line[:idx]
                    break

        # Collapse whitespace
        normalized = " ".join(line.split())
        result.append(normalized)

    return result


_STRUCTURAL_TOKENS = frozenset({
    "", "{", "}", "};", "},", ");", ")", "]", "},);", "});",
    "} else {", "Ok(())", "None", "return;",
})


def _is_structural(line: str) -> bool:
    """Return True if a normalized line is purely structural (braces, etc)."""
    return line in _STRUCTURAL_TOKENS


def fingerprint_file(
    normalized_lines: list[str], min_lines: int
) -> list[tuple[str, int]]:
    """Sliding window of min_lines lines, SHA-256 hash each window.

    Skips windows that are mostly empty or mostly structural tokens
    (closing braces, etc.) to avoid noise.
    Returns list of (hash_hex, start_line_0indexed).
    """
    results: list[tuple[str, int]] = []
    n = len(normalized_lines)

    for i in range(n - min_lines + 1):
        window = normalized_lines[i : i + min_lines]

        # Skip if more than half the lines are empty
        non_empty = sum(1 for ln in window if ln)
        if non_empty <= min_lines // 2:
            continue

        # Skip if more than half the lines are just structural tokens
        substantive = sum(1 for ln in window if not _is_structural(ln))
        if substantive <= min_lines // 2:
            continue

        joined = "\n".join(window)
        h = hashlib.sha256(joined.encode("utf-8")).hexdigest()
        results.append((h, i))

    return results


def cluster_clones(
    all_hashes: dict[str, list[tuple[str, int]]],
    min_lines: int,
) -> list[CloneGroup]:
    """Group identical hashes. Only keep groups with 2+ locations in different
    files (or same file 50+ lines apart).

    all_hashes: hash -> [(file_path, start_line_0indexed), ...]
    Returns CloneGroup objects. The normalized_lines field is left empty
    (populated later by the caller if needed for filtering).
    """
    groups: list[CloneGroup] = []

    for _hash, locations in all_hashes.items():
        if len(locations) < 2:
            continue

        # Filter: keep only if different files or same file 50+ lines apart
        valid_locs: list[tuple[str, int]] = []
        for file_path, start in locations:
            dominated = False
            for other_file, other_start in valid_locs:
                if file_path == other_file and abs(start - other_start) < 50:
                    dominated = True
                    break
            if not dominated:
                valid_locs.append((file_path, start))

        if len(valid_locs) < 2:
            continue

        # Convert to 1-indexed (start, end)
        locs_1indexed = [
            (f, start + 1, start + min_lines) for f, start in valid_locs
        ]
        groups.append(CloneGroup(
            normalized_lines=[],
            locations=locs_1indexed,
        ))

    return groups


def filter_clones(
    groups: list[CloneGroup], ignore_patterns: list[dict]
) -> list[CloneGroup]:
    """Match normalized text against regex patterns from duplication-ignore.yaml.
    Drop groups where any line matches any ignore pattern.
    """
    if not ignore_patterns:
        return list(groups)

    compiled = []
    for entry in ignore_patterns:
        try:
            compiled.append(re.compile(entry["pattern"]))
        except re.error:
            continue

    result: list[CloneGroup] = []
    for group in groups:
        matched = False
        text = "\n".join(group.normalized_lines)
        for pat in compiled:
            if pat.search(text):
                matched = True
                break
        if not matched:
            result.append(group)
    return result


def _merge_overlapping(groups: list[CloneGroup]) -> list[CloneGroup]:
    """Merge clone groups that overlap into larger regions.

    When two groups have the exact same location tuples (file, start) pattern
    and their line ranges overlap or are adjacent at every location, merge them
    into a single group covering the union. This eliminates the noise of many
    overlapping sliding windows reporting the same clone.
    """
    if not groups:
        return groups

    # Key each group by a tuple of (file, start_line) pairs sorted,
    # so we only merge groups that are the same clone shifted by one line.
    # Use a signature: sorted tuple of file paths + the relative offsets
    # between locations (to distinguish same-files-different-offsets).
    def _location_signature(g: CloneGroup) -> tuple:
        """Create a signature from the files and relative start offsets."""
        locs = sorted(g.locations, key=lambda x: (x[0], x[1]))
        if not locs:
            return ()
        # Use file names + offset from first location's start
        base = locs[0][1]
        return tuple((loc[0], loc[1] - base) for loc in locs)

    by_sig: dict[tuple, list[CloneGroup]] = {}
    for g in groups:
        sig = _location_signature(g)
        by_sig.setdefault(sig, []).append(g)

    merged: list[CloneGroup] = []

    for _sig, sig_groups in by_sig.items():
        if len(sig_groups) == 1:
            merged.append(sig_groups[0])
            continue

        # Sort by first location's start line
        sig_groups.sort(key=lambda g: g.locations[0][1])

        # Merge overlapping/adjacent groups
        current = sig_groups[0]
        for next_g in sig_groups[1:]:
            # Check if all locations overlap or are adjacent
            c_locs = sorted(current.locations, key=lambda x: (x[0], x[1]))
            n_locs = sorted(next_g.locations, key=lambda x: (x[0], x[1]))

            can_merge = len(c_locs) == len(n_locs)
            if can_merge:
                for c_loc, n_loc in zip(c_locs, n_locs):
                    if c_loc[0] != n_loc[0]:
                        can_merge = False
                        break
                    # Overlap: next start <= current end + 1
                    if n_loc[1] > c_loc[2] + 1:
                        can_merge = False
                        break

            if can_merge:
                new_locs = []
                for c_loc, n_loc in zip(c_locs, n_locs):
                    new_locs.append((
                        c_loc[0],
                        min(c_loc[1], n_loc[1]),
                        max(c_loc[2], n_loc[2]),
                    ))
                current = CloneGroup(
                    normalized_lines=current.normalized_lines,
                    locations=new_locs,
                )
            else:
                merged.append(current)
                current = next_g

        merged.append(current)

    return merged


def _collect_files(
    path: Path, lang_filter: list[str], root: Path
) -> list[Path]:
    """Collect source files matching language filter, excluding generated/build dirs."""
    extensions = set()
    for lang in lang_filter:
        ext = f".{lang}" if not lang.startswith(".") else lang
        if ext in LANG_EXTENSIONS:
            extensions.add(ext)

    if not extensions:
        extensions = LANG_EXTENSIONS

    files: list[Path] = []
    for f in sorted(path.rglob("*")):
        if not f.is_file():
            continue
        if f.suffix not in extensions:
            continue
        try:
            rel = f.relative_to(root)
        except ValueError:
            rel = f
        rel_str = str(rel)
        if any(rel_str.startswith(prefix) or ("/" + prefix) in rel_str
               for prefix in EXCLUDE_PREFIXES):
            continue
        files.append(f)
    return files


def scan_directory(
    path: Path,
    min_lines: int,
    lang_filter: list[str],
    ignore_patterns: list[dict],
    root: Path,
) -> list[Finding]:
    """Orchestrate: collect files, normalize, fingerprint, cluster, filter.

    Returns Finding objects ready for the shared report module.
    """
    files = _collect_files(path, lang_filter, root)

    # Phase 1: normalize and fingerprint all files
    all_hashes: dict[str, list[tuple[str, int]]] = {}
    file_lines: dict[str, list[str]] = {}  # path -> normalized lines

    for f in files:
        try:
            raw = f.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue

        try:
            rel = str(f.relative_to(root))
        except ValueError:
            rel = str(f)

        normalized = normalize_lines(raw, f.suffix)
        file_lines[rel] = normalized

        hashes = fingerprint_file(normalized, min_lines)
        for h, start in hashes:
            all_hashes.setdefault(h, []).append((rel, start))

    # Phase 2: cluster
    groups = cluster_clones(all_hashes, min_lines)

    # Phase 2.5: merge overlapping windows into larger clone regions
    groups = _merge_overlapping(groups)

    # Populate normalized_lines for filtering
    for group in groups:
        first_file, start_1, end_1 = group.locations[0]
        if first_file in file_lines:
            group.normalized_lines = file_lines[first_file][start_1 - 1 : end_1]

    # Phase 3: filter
    groups = filter_clones(groups, ignore_patterns)

    # Phase 4: convert to Findings
    findings: list[Finding] = []
    # Sort groups for deterministic IDs: by first location file, then line
    groups.sort(key=lambda g: (g.locations[0][0], g.locations[0][1]))

    for i, group in enumerate(groups, start=1):
        fid = f"tc-{i:03d}"
        n_lines = group.locations[0][2] - group.locations[0][1] + 1

        if n_lines >= 8:
            severity = "high"
        elif n_lines >= 5:
            severity = "medium"
        else:
            severity = "low"

        # Build description from normalized lines
        preview = " | ".join(
            ln for ln in group.normalized_lines[:3] if ln
        )
        if len(preview) > 100:
            preview = preview[:97] + "..."

        locations_dicts = [
            {"file": loc[0], "lines": [loc[1], loc[2]]}
            for loc in group.locations
        ]

        files_involved = [loc[0] for loc in group.locations]
        desc = f"{n_lines}-line clone in {len(files_involved)} locations"
        if preview:
            desc += f": {preview}"

        findings.append(Finding(
            id=fid,
            kind="textual-clone",
            severity=severity,
            description=desc,
            locations=locations_dicts,
            suggestion="Extract shared helper or deduplicate",
        ))

    return findings


def _load_ignore_patterns(yaml_path: Path) -> list[dict]:
    """Load ignore patterns from duplication-ignore.yaml."""
    if not yaml_path.exists():
        return []
    try:
        import yaml
        data = yaml.safe_load(yaml_path.read_text())
        return data.get("ignore", []) if data else []
    except Exception:
        return []


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Detect textual code clones across the codebase."
    )
    parser.add_argument(
        "--min-lines", type=int, default=5,
        help="Minimum number of identical normalized lines to flag (default: 5)",
    )
    parser.add_argument(
        "--save", action="store_true",
        help="Save results to docs/duplication-findings.json and .md",
    )
    parser.add_argument(
        "--lang", type=str, default="",
        help="Comma-separated language extensions to scan (e.g., rs,go,ts). Default: all",
    )
    parser.add_argument(
        "--path", type=str, default="",
        help="Subdirectory to scan (relative to repo root). Default: all SCAN_DIRS",
    )
    args = parser.parse_args()

    lang_filter = [x.strip() for x in args.lang.split(",") if x.strip()] if args.lang else []

    ignore_yaml = SCRIPT_DIR / "duplication-ignore.yaml"
    ignore_patterns = _load_ignore_patterns(ignore_yaml)

    scan_paths: list[Path] = []
    if args.path:
        p = ROOT / args.path
        if p.is_dir():
            scan_paths.append(p)
        else:
            print(f"Error: path {args.path} not found", file=sys.stderr)
            sys.exit(1)
    else:
        for d in SCAN_DIRS:
            p = ROOT / d
            if p.is_dir():
                scan_paths.append(p)

    all_findings: list[Finding] = []
    for scan_path in scan_paths:
        findings = scan_directory(
            path=scan_path,
            min_lines=args.min_lines,
            lang_filter=lang_filter,
            ignore_patterns=ignore_patterns,
            root=ROOT,
        )
        all_findings.extend(findings)

    # Re-number findings globally
    for i, f in enumerate(all_findings, start=1):
        f.id = f"tc-{i:03d}"

    report = Report(
        tool="textual-clones",
        config={
            "min_lines": args.min_lines,
            "lang_filter": lang_filter or "all",
            "path": args.path or "all",
        },
        findings=all_findings,
    )

    # Print summary
    counts = {"high": 0, "medium": 0, "low": 0}
    for f in all_findings:
        counts[f.severity] = counts.get(f.severity, 0) + 1

    print(f"Textual clone scan complete: {len(all_findings)} findings "
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

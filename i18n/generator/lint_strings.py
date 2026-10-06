"""Lint i18n strings for duplicates and near-duplicates.

Usage:
    python lint_strings.py           # report all issues
    python lint_strings.py --strict  # exit 1 if any issues found
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

# Reuse the YAML loader from the generator
sys.path.insert(0, str(Path(__file__).parent))
from generate import load_strings, flatten


def normalize(value: str) -> str:
    """Normalize a string for near-duplicate comparison.

    Strips trailing punctuation, collapses whitespace, lowercases.
    """
    v = value.strip().rstrip(".:!?…").strip()
    v = re.sub(r"\s+", " ", v)
    return v.lower()


def find_exact_duplicates(
    flat: dict[str, str], *, threshold: int = 3
) -> dict[str, list[str]]:
    """Find string values that appear in threshold or more keys."""
    by_value: dict[str, list[str]] = defaultdict(list)
    for key, value in flat.items():
        # Skip parameterized strings (they're functions, not constants)
        if "{" in value:
            continue
        by_value[value].append(key)
    return {v: keys for v, keys in by_value.items() if len(keys) >= threshold}


def find_near_duplicates(flat: dict[str, str]) -> list[tuple[str, str, str]]:
    """Find pairs of keys whose values differ only by trailing punctuation/case.

    Returns list of (key_a, key_b, normalized_value).
    Only reports pairs where the raw values are NOT identical (those are exact dupes).
    """
    by_normalized: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for key, value in flat.items():
        if "{" in value:
            continue
        n = normalize(value)
        if n:  # skip empty after normalization
            by_normalized[n].append((key, value))

    results = []
    for norm, entries in by_normalized.items():
        if len(entries) < 2:
            continue
        # Only report groups where at least two raw values differ
        raw_values = {v for _, v in entries}
        if len(raw_values) < 2:
            continue
        # Report all pairs with differing raw values
        for i, (k1, v1) in enumerate(entries):
            for k2, v2 in entries[i + 1 :]:
                if v1 != v2:
                    results.append((k1, k2, norm))
    return results


def find_common_shadows(flat: dict[str, str]) -> list[tuple[str, str, str]]:
    """Find section-specific keys whose value matches a common.* entry.

    Returns list of (common_key, shadow_key, value).
    """
    common_entries = {k: v for k, v in flat.items() if k.startswith("common.")}
    common_by_value = {v: k for k, v in common_entries.items()}

    results = []
    for key, value in flat.items():
        if key.startswith("common."):
            continue
        if value in common_by_value:
            results.append((common_by_value[value], key, value))
    return results


PLATFORM_PREFIXES = [
    "windows.",
    "macos.",
    "status_view.",
    "messages_ios.",
    "admin_view.",
    "peers_view.",
]

# Base section names corresponding to platform sections
BASE_SECTIONS = {
    "windows.feed": "feed",
    "windows.sync": "devices",
    "windows.moderation": "status.spam",
    "windows.p2p": "p2p",
    "status_view": "status",
    "messages_ios": "conversations",
    "admin_view": "admin",
    "peers_view": "devices",
    "notifications_page": "notifications",
}


def find_platform_shadows(flat: dict[str, str]) -> list[tuple[str, str, str]]:
    """Find platform-section keys whose value duplicates a base-section key.

    Returns list of (base_key, platform_key, value).
    """
    # Build lookup: base section prefix -> {value: key}
    base_by_value: dict[str, dict[str, str]] = defaultdict(dict)
    for key, value in flat.items():
        # Skip platform-specific keys
        if any(key.startswith(p) for p in PLATFORM_PREFIXES):
            continue
        section = key.split(".")[0]
        base_by_value[section][value] = key

    results = []
    for key, value in flat.items():
        for platform_prefix, base_section in BASE_SECTIONS.items():
            if key.startswith(platform_prefix + ".") or key == platform_prefix:
                # Look for identical value in base section
                base_key = base_by_value.get(base_section, {}).get(value)
                if base_key:
                    results.append((base_key, key, value))
                break
    return results


def main() -> int:
    parser = argparse.ArgumentParser(description="Lint i18n strings for duplicates")
    parser.add_argument(
        "--strict", action="store_true", help="Exit 1 if any issues found"
    )
    args = parser.parse_args()

    tree = load_strings()
    flat = flatten(tree)
    issues = 0

    # 1. Exact duplicates (3+ keys with same value)
    exact = find_exact_duplicates(flat)
    if exact:
        print(f"=== Exact duplicates ({len(exact)} values) ===")
        for value, keys in sorted(exact.items(), key=lambda x: -len(x[1])):
            print(f'\n  "{value}" ({len(keys)} copies):')
            for k in sorted(keys):
                print(f"    - {k}")
            issues += len(keys) - 1

    # 2. Near-duplicates (differ only by punctuation/case)
    near = find_near_duplicates(flat)
    if near:
        print(f"\n=== Near-duplicates ({len(near)} pairs) ===")
        for k1, k2, norm in sorted(near):
            print(f'  "{flat[k1]}" vs "{flat[k2]}"')
            print(f"    {k1}  <->  {k2}")
        issues += len(near)

    # 3. Section keys shadowing common.*
    shadows = find_common_shadows(flat)
    if shadows:
        print(f"\n=== Shadows of common.* ({len(shadows)} keys) ===")
        for common_key, shadow_key, value in sorted(shadows, key=lambda x: x[1]):
            print(f'  {shadow_key} = "{value}"  (same as {common_key})')
        issues += len(shadows)

    # 4. Platform sections duplicating base sections
    platform = find_platform_shadows(flat)
    if platform:
        print(f"\n=== Platform-section duplicates ({len(platform)} keys) ===")
        for base_key, platform_key, value in sorted(platform, key=lambda x: x[1]):
            print(f'  {platform_key} = "{value}"  (same as {base_key})')
        issues += len(platform)

    if issues:
        print(f"\n{issues} total issues found.")
    else:
        print("No issues found.")

    return 1 if args.strict and issues else 0


if __name__ == "__main__":
    sys.exit(main())

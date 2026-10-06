"""Shared, multi-line-aware Cargo.toml dependency scanning for convention 15
rule (b): a consumer NEVER names a seam-gating feature (`test-helpers`,
`test-hooks`, …) on a SHIPPING dependency line, where cargo turns it on
unconditionally regardless of the consuming crate's own build recipe —
surviving even `--no-default-features`.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention,
the *Shared Rust crates (`libs/fauna-*`)* bullet, rule (b).

Five witnesses (`test_ffi_flavor_split.py`, `test_agent_ipc_seam_gating.py`,
`test_mail_import_tls_seam_gating.py`, `test_nest_test_hook_seam_gating.py`,
`test_payments_excision_spine.py`) each carried their own verbatim copy of
this scan, all sharing the same two defects:

  A. **The crate axis was a hand-maintained list** — each witness named only
     the few crates its OWN seam family cared about (7 crates total across
     all four lists), leaving the other 35+ crates that declare a
     `test-helpers` feature uncovered by ANY witness on this axis.
  B. **The parser kept a line only if `{` appeared on THAT line**, so a dep
     entry whose feature array wraps onto a following line — `cargo fmt`'s
     own shape once the list is long enough — was invisible even to the
     witness that DID name the right crate: `foo = { …, features = [` has no
     `test-helpers` on it, and `    "test-helpers",` has no `{` and was never
     collected either.

This module fixes both: `shipping_dep_lines` now JOINS a dependency entry
across however many physical lines it spans before returning it, and
`crates_declaring_feature` DERIVES the crate axis by regex — no list for
anyone to update, and no longer stale the moment a new crate gains the
feature (measured: 42 crates at filing, already 44 five days later).
"""

from __future__ import annotations

import re
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


def all_manifests(repo: Path = REPO) -> list[Path]:
    """Every `Cargo.toml` in the workspace, vendor/target trees excluded —
    including the root manifest's own `[workspace.dependencies]` table
    (member crates using `dep.workspace = true` inherit its feature list,
    so it is as much a shipping surface as any member manifest)."""
    return [
        p
        for p in sorted(repo.rglob("Cargo.toml"))
        if "/vendor/" not in str(p) and "/target/" not in str(p)
    ]


def shipping_dep_lines(manifest: str) -> list[str]:
    """Dependency ENTRIES that reach a SHIPPED build, each joined into one
    string across however many physical lines it spans.

    `[dev-dependencies]`/`[build-dependencies]` stay excluded — this section
    logic is unchanged from the original per-witness helper: under the
    workspace's `resolver = "2"` a dev-dep's features are not unified into a
    normal build, so naming a seam feature there enables it for `cargo test`
    and for nothing that ships. Scanning the whole manifest instead — which
    the original pin did until 2026-08-10 — reports those as leaks, and a
    gate that cries wolf is a gate people learn to skip past (convention
    17's discipline). Do not re-widen this.

    The one behavioral change from the original: a dep entry is now
    recognized by BALANCED BRACES rather than requiring `{` and the matching
    `=` on the same physical line, so a wrapped `features = [...]` array
    (§ module docstring, defect B) is joined into the single string returned
    for that entry instead of silently vanishing between two lines neither
    of which individually matches.
    """
    entries: list[str] = []
    in_shipping_section = False
    pending: list[str] = []
    depth = 0
    for line in manifest.splitlines():
        stripped = line.strip()
        if depth == 0 and stripped.startswith("[") and stripped.endswith("]"):
            table = stripped[1:-1]
            in_shipping_section = table.endswith("dependencies") and not (
                table.endswith("dev-dependencies") or table.endswith("build-dependencies")
            )
            continue
        if depth == 0:
            if not in_shipping_section:
                continue
            # A dep line is `name = { ... }`; the `[features]` table's own
            # `test-helpers = [...]` forwarding declaration is what we WANT,
            # and is excluded by the section filter above regardless.
            if "{" not in stripped or "=" not in stripped.split("{")[0]:
                continue
            pending = [line]
            depth = stripped.count("{") - stripped.count("}")
        else:
            pending.append(line)
            depth += stripped.count("{") - stripped.count("}")
        if depth <= 0:
            entries.append("\n".join(pending))
            pending = []
            depth = 0
    return entries


def crates_declaring_feature(feature: str, repo: Path = REPO) -> set[str]:
    """Every crate under `repo` whose OWN `[features]` table declares
    `feature` — the axis a rule-(b) witness cares about, derived by regex
    rather than hand-listed (the discipline
    `test_shared_crate_seam_gating.py`'s own docstring already argues for),
    so a crate gaining the feature tomorrow is covered with no list for
    anyone to update.
    """
    decl = re.compile(rf"^{re.escape(feature)}\s*=")
    name_re = re.compile(r'^name\s*=\s*"([^"]+)"', re.MULTILINE)
    crates: set[str] = set()
    for manifest_path in all_manifests(repo):
        text = manifest_path.read_text(encoding="utf-8")
        in_features = False
        declared = False
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("[") and stripped.endswith("]"):
                in_features = stripped[1:-1] == "features"
                continue
            if in_features and decl.match(stripped):
                declared = True
                break
        if not declared:
            continue
        name_m = name_re.search(text)
        if name_m:
            crates.add(name_m.group(1))
    return crates


def shipping_feature_offenders(
    feature: str, repo: Path = REPO, crates: set[str] | None = None
) -> list[str]:
    """Every manifest/dependency-entry pair that turns `feature` on for a
    known crate on a SHIPPING dependency line — the general rule-(b)
    violation. `crates` defaults to every crate that declares `feature`
    (`crates_declaring_feature`); pass a narrower set to reproduce one
    witness's own family without re-deriving it.
    """
    known = crates if crates is not None else crates_declaring_feature(feature, repo)
    offenders: list[str] = []
    for manifest_path in all_manifests(repo):
        text = manifest_path.read_text(encoding="utf-8")
        for entry in shipping_dep_lines(text):
            dep_name = entry.strip().split("=", 1)[0].strip()
            if dep_name in known and feature in entry:
                offenders.append(f"{manifest_path.relative_to(repo)}: {entry.strip()}")
    return offenders

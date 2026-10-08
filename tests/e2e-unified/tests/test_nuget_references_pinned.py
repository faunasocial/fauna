"""Every NuGet reference stays pinned: an exact version, a committed lock, a locked restore.

`release-integrity.md` § Minimum release age for new dependency versions holds the
7-day age for NuGet at the round, not in a resolver — NuGet's restore has no
publish-age setting — and that only works while no tracked project can move a
version on its own. Owner-ruled 2026-10-05 and landed in bump round 1: every
`PackageReference` is an exact version, every project restores from its committed
`packages.lock.json` (the exact version AND content hash of every package,
transitive ones included), and `Directory.Build.props` makes that restore LOCKED,
so a resolution that would differ fails with NU1004 instead of moving silently.

Each half fails silently on its own:

  * a floating range (`8.*`, `[1.0,)`) or a missing version resolves to whatever
    nuget.org holds that day — the shape every reference had before the pin;
  * a project with no lockfile beside it is not locked at all (`Directory.Build.props`
    applies only where a lockfile exists, so a scratch project is never forced);
  * a lock whose direct entries disagree with the `.csproj` is one restore away
    from NU1004 on every machine;
  * committing the locked-mode switch off, or a forced re-evaluation, in a recipe,
    script or workflow turns every restore it runs back into a resolution.

tier_1: reads tracked files, runs no dotnet.
"""

import json
import re
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_PROPS = _REPO / "Directory.Build.props"

#: A bare exact version: NuGet resolves it to exactly that version (the lowest match).
_EXACT = re.compile(r"^\d+(\.\d+){1,3}(-[0-9A-Za-z][0-9A-Za-z.-]*)?$")
#: A lock's contentHash: base64 SHA-512.
_HASH = re.compile(r"^[A-Za-z0-9+/]{86}==$")

#: The per-invocation ways round the lock, built from parts so this file's own
#: walk does not find them here.
_OVERRIDES = (
    "RestoreLockedMode" + "=false",
    "<RestoreLockedMode>" + "false",
    "RestorePackagesWithLockFile" + "=false",
    "<RestorePackagesWithLockFile>" + "false",
    "--force" + "-evaluate",
)
_SCANNED_SUFFIXES = (".py", ".sh", ".yml", ".yaml", ".cmd", ".ps1", ".toml", ".props", ".targets", ".csproj")


def _projects() -> list[Path]:
    """Every tracked project — asked of git, so a new one is held from its first commit."""
    listed = subprocess.run(
        ["git", "-C", str(_REPO), "ls-files", "--", "*.csproj"],
        check=True, capture_output=True, text=True,
    ).stdout.split()
    return [_REPO / name for name in sorted(listed)]


def _references(project: Path) -> dict[str, str | None]:
    refs: dict[str, str | None] = {}
    for el in ET.parse(project).getroot().iter():
        if el.tag.rsplit("}", 1)[-1] != "PackageReference" or not el.get("Include"):
            continue
        version = el.get("Version")
        if version is None:
            child = next((c for c in el if c.tag.rsplit("}", 1)[-1] == "Version"), None)
            version = child.text.strip() if child is not None and child.text else None
        refs[el.get("Include")] = version
    return refs


def _ids(path: Path) -> str:
    return path.relative_to(_REPO).as_posix()


def test_the_tracked_projects_are_found():
    names = {_ids(p) for p in _projects()}
    assert {
        "apps/fauna-windows/FaunaApp/FaunaApp/FaunaApp.csproj",
        "apps/fauna-windows/FaunaApp/FaunaApp.Core/FaunaApp.Core.csproj",
        "apps/fauna-windows/FaunaApp/FaunaApp.Tests/FaunaApp.Tests.csproj",
        "tests/e2e-unified/flaui-bridge/FauiBridge.csproj",
    } <= names, names


@pytest.mark.parametrize("project", _projects(), ids=_ids)
def test_every_reference_is_an_exact_version(project: Path):
    loose = {pkg: v for pkg, v in _references(project).items() if v is None or not _EXACT.match(v)}
    assert not loose, (
        f"{_ids(project)}: {loose} — a floating range or a missing version resolves to whatever "
        "nuget.org holds that day. Pin the exact version; moving it is a bump round's "
        "(dependency-bump-report.md § Who opens a round)."
    )


@pytest.mark.parametrize("project", _projects(), ids=_ids)
def test_a_lock_beside_the_project_pins_every_direct_reference(project: Path):
    refs = _references(project)
    lock = project.parent / "packages.lock.json"
    if not refs:
        return
    assert lock.is_file(), (
        f"{_ids(project)} declares {len(refs)} package reference(s) and has no packages.lock.json "
        "beside it, so its restore is not locked (Directory.Build.props applies only where a lock exists)."
    )
    sections = json.loads(lock.read_text(encoding="utf-8")).get("dependencies") or {}
    assert sections, f"{_ids(lock)}: no target-framework section"
    problems = []
    for tfm, entries in sections.items():
        lowered = {k.lower(): v for k, v in entries.items()}
        # A runtime section (`net10.0/win-x64`) lists only what differs per runtime id;
        # the direct references live in each plain target-framework section.
        for pkg, version in (refs.items() if "/" not in tfm else ()):
            ent = lowered.get(pkg.lower())
            if ent is None:
                problems.append(f"[{tfm}] {pkg}: not in the lock")
            elif ent.get("type") != "Direct" or ent.get("resolved") != version:
                problems.append(f"[{tfm}] {pkg}: lock says {ent.get('type')} {ent.get('resolved')}, "
                                f"the project {version}")
        for pkg, ent in entries.items():
            if ent.get("type") != "Project" and not _HASH.match(str(ent.get("contentHash") or "")):
                problems.append(f"[{tfm}] {pkg}: contentHash missing or out of shape")
    assert not problems, f"{_ids(lock)} disagrees with {_ids(project)}:\n" + "\n".join(problems)


def test_the_restore_is_locked_wherever_a_lock_exists():
    root = ET.parse(_PROPS).getroot()
    groups = [g for g in root.iter("PropertyGroup")
              if "packages.lock.json" in (g.get("Condition") or "")]
    values = {el.tag: (el.text or "").strip() for g in groups for el in g}
    assert values.get("RestorePackagesWithLockFile") == "true" and values.get("RestoreLockedMode") == "true", (
        f"{_ids(_PROPS)}: RestorePackagesWithLockFile and RestoreLockedMode must both be `true` in the "
        f"group conditioned on packages.lock.json (got {values}) — without locked mode a drifted "
        "resolution rewrites the lock instead of failing."
    )


def _scanned_files() -> list[Path]:
    """Everything tracked that could run a restore. A walk, not a hand-listed set:
    the override is added by whoever the lock blocks, wherever they work."""
    out: list[Path] = [_REPO / "justfile", _PROPS]
    for root in (_REPO / "scripts", _REPO / ".github", _REPO / "tests" / "e2e-unified", _REPO / "apps" / "fauna-windows"):
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*")):
            if path.is_file() and path.suffix in _SCANNED_SUFFIXES:
                out.append(path)
    skip = {"node_modules", "target", "bin", "obj"}
    return [p for p in out if p.is_file() and not skip & set(p.parts)]


def test_no_tracked_file_switches_the_lock_off():
    hits = []
    for path in _scanned_files():
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            if any(o in line for o in _OVERRIDES):
                hits.append(f"{_ids(path)}:{number}: {line.strip()[:160]}")
    assert not hits, (
        "a tracked file switches the NuGet lock off for whatever it runs — a round regenerates "
        "a lock by hand on a scratch copy, never from a committed recipe:\n" + "\n".join(hits)
    )

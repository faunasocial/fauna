"""Read an android APK's backup posture — what its manifest says about cloud
backup and device-to-device transfer — with the SDK's ``aapt2``.

The witness of ``docs/features/identity-protected-on-this-device.md`` outcome 4
on android reads the INSTALLED package (``AndroidBridgeDriver.pull_installed_apk``)
through this; ``scripts/android-apk-nsc-check.sh`` is the prior art for resolving
a compiled XML resource through the APK's resource table (release builds shorten
resource paths, so the member is never assumed at its source path).
"""
from __future__ import annotations

import os
import re
import subprocess
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

ANDROID_NS = "http://schemas.android.com/apk/res/android"

#: Every domain a ``<data-extraction-rules>`` section can name
#: (developer.android.com "Back up user data with Auto Backup" → XML config).
#: Pinned beside the app's own ``CloudBackupPostureTest``, which asserts the same set.
ALL_DOMAINS = frozenset({
    "root", "file", "database", "sharedpref", "external",
    "device_root", "device_file", "device_database", "device_sharedpref",
})

#: The two sections ``android:dataExtractionRules`` governs (API 31+).
SECTIONS = ("cloud-backup", "device-transfer")


@dataclass
class BackupPosture:
    """What the manifest says: ``allow_backup`` (``None`` when the attribute is
    absent, which the platform reads as ``true``), the resource the
    ``dataExtractionRules`` attribute names (``None`` when absent), and per
    section the domains it excludes and includes."""

    allow_backup: bool | None
    rules_member: str | None
    excludes: dict[str, set[str]] = field(default_factory=dict)
    includes: dict[str, set[str]] = field(default_factory=dict)

    def sections_leaving_something(self) -> dict[str, list[str]]:
        """Per section, what is NOT excluded (every domain, when the section
        is missing) or is explicitly included — ``{}`` when nothing leaves."""
        leaks = {}
        for section in SECTIONS:
            missing = sorted(ALL_DOMAINS - self.excludes.get(section, set()))
            included = sorted(self.includes.get(section, set()))
            if missing or included:
                leaks[section] = missing + [f"include:{d}" for d in included]
        return leaks


def aapt2() -> str:
    """The newest ``aapt2`` under the SDK's build-tools (the nsc check's lookup)."""
    sdk = Path(os.environ.get("ANDROID_HOME") or Path.home() / "android-sdk")
    found = sorted(
        (p for p in (sdk / "build-tools").glob("*/aapt2") if p.is_file()),
        key=lambda p: [int(x) if x.isdigit() else x for x in re.split(r"[.-]", p.parent.name)],
    )
    if not found:
        raise FileNotFoundError(f"no aapt2 under {sdk}/build-tools")
    return str(found[-1])


def _run(*args: str) -> str:
    return subprocess.run(
        [aapt2(), *args], check=True, capture_output=True, text=True, timeout=120,
    ).stdout


def _manifest_attr(xmltree: str, element: str, name: str) -> str | None:
    """The raw value of ``android:<name>`` on the first ``<element>`` in an
    ``aapt2 dump xmltree`` reading, or ``None``."""
    inside = None
    for line in xmltree.splitlines():
        stripped = line.strip()
        depth = len(line) - len(line.lstrip())
        if stripped.startswith("E: "):
            if inside is not None and depth <= inside:
                return None
            if stripped.split()[1] == element and inside is None:
                inside = depth
            continue
        if inside is not None and stripped.startswith(f"A: {ANDROID_NS}:{name}("):
            return stripped.split("=", 1)[1].split(" (Raw", 1)[0].strip().strip('"')
    return None


def _resource_member(apk: Path, resource_id: str) -> str | None:
    """The APK member holding the file resource ``resource_id`` (``0x7f…``)."""
    want = int(resource_id.removeprefix("@"), 16)
    lines = _run("dump", "resources", str(apk)).splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"\s*resource (0x[0-9a-f]+) ", line)
        if not m or int(m.group(1), 16) != want:
            continue
        for follow in lines[i + 1:]:
            if re.match(r"\s*resource 0x", follow):
                break
            fm = re.search(r"\(file\) (\S+)", follow)
            if fm:
                return fm.group(1)
    return None


def _rules(xmltree: str) -> tuple[dict[str, set[str]], dict[str, set[str]]]:
    excludes: dict[str, set[str]] = {}
    includes: dict[str, set[str]] = {}
    section = None
    rule = None
    for line in xmltree.splitlines():
        stripped = line.strip()
        if stripped.startswith("E: "):
            tag = stripped.split()[1]
            if tag in SECTIONS:
                section, rule = tag, None
                excludes.setdefault(tag, set())
                includes.setdefault(tag, set())
            elif tag in ("exclude", "include"):
                rule = tag
            else:
                section = rule = None
            continue
        m = re.match(r'A: domain="([^"]*)"', stripped)
        if m and section and rule:
            (excludes if rule == "exclude" else includes)[section].add(m.group(1))
    return excludes, includes


def backup_posture(apk: Path) -> BackupPosture:
    """Read ``apk``'s ``<application>`` backup attributes and, when it names
    one, its data-extraction rules."""
    manifest = _run("dump", "xmltree", "--file", "AndroidManifest.xml", str(apk))
    allow = _manifest_attr(manifest, "application", "allowBackup")
    rules_ref = _manifest_attr(manifest, "application", "dataExtractionRules")
    posture = BackupPosture(
        allow_backup=None if allow is None else allow.lower() not in ("false", "0x0", "0"),
        rules_member=None,
    )
    if rules_ref is None:
        return posture
    member = _resource_member(apk, rules_ref)
    if member is None or member not in zipfile.ZipFile(apk).namelist():
        raise RuntimeError(
            f"{apk}'s manifest names dataExtractionRules {rules_ref}, but the resource "
            f"table resolves it to no member ({member!r})"
        )
    posture.rules_member = member
    posture.excludes, posture.includes = _rules(_run("dump", "xmltree", "--file", member, str(apk)))
    return posture


def dumpsys_flags(dumpsys: str) -> set[str]:
    """The package's ``flags=[ … ]`` set from ``dumpsys package`` (the first
    such line — the package's own, ahead of any per-user block)."""
    m = re.search(r"^\s*flags=\[([^\]]*)\]", dumpsys, re.MULTILINE)
    return set(m.group(1).split()) if m else set()

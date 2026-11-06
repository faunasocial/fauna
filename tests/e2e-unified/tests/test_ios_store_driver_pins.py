"""Every element id the iOS store driver reads is set where a Release build keeps it.

The store driver (``apps/fauna-apple/StoreDriver/``) drives a TestFlight install
from outside the app, through the accessibility tree alone (``feature-catalog.md``
§ Implementation status today, how the iOS store install is driven). Convention
15 compiles the in-process ``AutomationRegistry`` out of Release builds. So the
only id a TestFlight build exposes is one a view sets with
``.accessibilityIdentifier(Ids.<x>)`` OUTSIDE any ``#if DEBUG`` region. An id
the driver needs that is set only by an ``.automation*`` registration, or only
inside ``#if DEBUG``, would make the device run fail on a missing element after
a whole TestFlight round trip. This file pins the precondition on every
machine, before any device is involved.

The pin is a lower bound: it proves the modifier is compiled into the Release
binary, not that SwiftUI surfaces it to XCUITest on that exact screen. The
device run is the witness for that last inch.

Static-only: it reads repository files, builds nothing and launches nothing
(tier_1).
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

ROOT = Path(__file__).resolve().parents[3]
DRIVER = ROOT / "apps/fauna-apple/StoreDriver/StoreDriverUITests/StoreDriverTests.swift"
#: Where the iOS app's views live: its own target and the shared FaunaKit.
VIEW_ROOTS = (ROOT / "apps/fauna-apple/Fauna-iOS", ROOT / "apps/fauna-apple/FaunaKit/Sources/FaunaKit")

_IF = re.compile(r"^\s*#if\b(.*)$")
_ELSEIF = re.compile(r"^\s*#elseif\b(.*)$")
_ELSE = re.compile(r"^\s*#else\b")
_ENDIF = re.compile(r"^\s*#endif\b")


def _debug_only(cond: str) -> bool:
    """A branch condition that holds only in a DEBUG build (``DEBUG``,
    ``DEBUG || FAUNA_E2E_AGENT``, ``DEBUG && os(iOS)``): no Release build with
    the store flavor's flags compiles it."""
    c = cond.split("//")[0].strip()
    return bool(re.search(r"(?<![!\w])DEBUG\b", c)) and "!DEBUG" not in c.replace(" ", "")


def release_identifier_sites(text: str) -> set[str]:
    """``Ids`` members set by ``.accessibilityIdentifier(Ids.<x>)`` on a line no
    enclosing ``#if`` branch confines to DEBUG."""
    stack: list[bool] = []  # per open #if: is the CURRENT branch debug-only?
    found: set[str] = set()
    for line in text.splitlines():
        if m := _IF.match(line):
            stack.append(_debug_only(m.group(1)))
            continue
        if (m := _ELSEIF.match(line)) and stack:
            stack[-1] = _debug_only(m.group(1))
            continue
        if _ELSE.match(line) and stack:
            # The #else of a DEBUG branch is the Release branch; the #else of
            # any other branch is not confined to DEBUG by this condition.
            stack[-1] = False
            continue
        if _ENDIF.match(line) and stack:
            stack.pop()
            continue
        if any(stack):
            continue
        found.update(re.findall(r"\.accessibilityIdentifier\(\s*Ids\.(\w+)\s*\)", line))
    return found


def driver_ids() -> set[str]:
    return set(re.findall(r"\bIds\.(\w+)", DRIVER.read_text()))


def test_the_scanner_tells_a_debug_branch_from_a_release_one():
    src = "\n".join([
        ".accessibilityIdentifier(Ids.a)",
        "#if DEBUG",
        ".accessibilityIdentifier(Ids.b)",
        "#else",
        ".accessibilityIdentifier(Ids.c)",
        "#endif",
        "#if DEBUG || FAUNA_E2E_AGENT",
        ".accessibilityIdentifier(Ids.d)",
        "#endif",
        "#if !FAUNA_KIDS",
        ".accessibilityIdentifier(Ids.e)",
        "#endif",
    ])
    assert release_identifier_sites(src) == {"a", "c", "e"}


def test_the_driver_reads_ids_at_all():
    """Guards the pin below against a vacuous green: a driver edit that stopped
    spelling ids as ``Ids.<x>`` would otherwise check nothing."""
    assert {"importIdentityButton", "pasteSecretField", "handleInput", "mainTabView"} <= driver_ids()


def test_every_id_the_driver_reads_survives_a_release_build():
    sites: set[str] = set()
    for root in VIEW_ROOTS:
        for path in root.rglob("*.swift"):
            if "/Generated/" in str(path) or "/Testing/" in str(path):
                continue
            sites |= release_identifier_sites(path.read_text(errors="replace"))
    missing = sorted(driver_ids() - sites)
    assert not missing, (
        f"the iOS store driver reads {missing}, but no iOS view sets them with "
        f"`.accessibilityIdentifier(Ids.<x>)` outside `#if DEBUG` — a TestFlight build "
        f"would not expose them. Add the modifier beside the view's `.automation*` "
        f"registration, or drive the step by another id."
    )

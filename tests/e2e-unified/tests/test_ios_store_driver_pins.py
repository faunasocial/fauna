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


_ABSENT_BLOCK = re.compile(r"// store-absent:begin.*?// store-absent:end", re.S)


def driver_ids() -> set[str]:
    """``Ids`` members the driver reads as PRESENT: everything outside the
    ``store-absent`` block."""
    return set(re.findall(r"\bIds\.(\w+)", _ABSENT_BLOCK.sub("", DRIVER.read_text())))


def driver_absent_ids() -> set[str]:
    """``Ids`` members the driver asserts ABSENT from the store install (the
    ``store-absent`` block — the ``store-install`` outcomes)."""
    blocks = _ABSENT_BLOCK.findall(DRIVER.read_text())
    return {m for b in blocks for m in re.findall(r"\bIds\.(\w+)", b)}


CONTENT_VIEW = ROOT / "apps/fauna-apple/Fauna-iOS/App/ContentView.swift"
SETTINGS_PAGE = ROOT / "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/SettingsPage.swift"
UI_IDS = ROOT / "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/UiIds.swift"


AUTOMATION_REGISTRY = ROOT / "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Testing/AutomationRegistry.swift"
_AUTOMATION_TEXT = re.compile(r"^public func automationText\(\s*\n\s*_ id: String,.*?^\}", re.M | re.S)


def automation_text_sets_release_id() -> bool:
    """``automationText(_ id:, _ text:)`` is defined outside every DEBUG branch
    and its body sets ``.accessibilityIdentifier(id)`` — so an id passed to it
    survives a Release build as surely as a direct modifier does."""
    text = AUTOMATION_REGISTRY.read_text()
    m = _AUTOMATION_TEXT.search(text)
    if not m or ".accessibilityIdentifier(id)" not in m.group(0):
        return False
    marked = text[:m.start()] + ".accessibilityIdentifier(Ids.__automationText)" + text[m.end():]
    return "__automationText" in release_identifier_sites(marked)


def _camel(kebab: str) -> str:
    head, *rest = kebab.split("-")
    return head + "".join(p[:1].upper() + p[1:] for p in rest)


def composed_release_sites() -> set[str]:
    """Ids a view sets through a COMPOSED string the plain scanner cannot see,
    each recognised only where its composing modifier is on a release line:

    - the More list's rows, ``.accessibilityIdentifier("\\(item.id)-tab")`` over
      ``MoreView.items`` (``profile-tab``, ``settings-tab``, …);
    - the Settings rail rows, ``accessibilityIdentifier(page.navRowId)``, whose
      ``navRowId`` composes ``Ids.settingsNavRow``;
    - ``automationText(Ids.<x>, …)`` (``gated-post-badge`` and the like), the
      FaunaKit helper whose body sets ``.accessibilityIdentifier(id)`` — counted
      only while that helper is itself defined outside any DEBUG branch.
    """
    found: set[str] = set()
    if automation_text_sets_release_id():
        for text in _view_sources():
            found |= release_identifier_sites(
                re.sub(r"\bautomationText\(\s*Ids\.(\w+)", r".accessibilityIdentifier(Ids.\1)(", text))
    more = CONTENT_VIEW.read_text()
    if release_identifier_sites(more.replace('.accessibilityIdentifier("\\(item.id)-tab")',
                                             ".accessibilityIdentifier(Ids.__moreRow)")) >= {"__moreRow"}:
        body = more[more.index("struct MoreView"):]
        # `items` is the sum of the arrays it names (built in halves around the
        # kids build's absent Bridges row): read the rows of every definition of
        # each — the store flavor is not the kids build, so its `#else` arm counts.
        parts = re.search(r"private static let items: \[MoreItem\] = ([\w +]+)\n", body)
        assert parts, "MoreView.items no longer reads `private static let items: [MoreItem] = …`"
        for name in re.findall(r"\w+", parts.group(1)):
            defs = [m.start() for m in re.finditer(
                rf"private static let {name}: \[MoreItem\] = \[", body)]
            assert defs, f"MoreView.items names {name}, which is not defined"
            for start in defs:
                rows = body[start:body.index("]\n", start)]
                found |= {_camel(f"{i}-tab") for i in re.findall(r'\.init\(id: "([\w-]+)"', rows)}
    page = SETTINGS_PAGE.read_text()
    if ('"\\(Ids.settingsNavRow)[' in page
            and re.search(r"^\s*accessibilityIdentifier\(page\.navRowId\)", page, re.M)):
        found.add("settingsNavRow")
    return found


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


def test_automation_text_is_a_release_identifier_site():
    """The ``automationText`` arm of ``composed_release_sites`` holds only while
    the helper itself is Release-compiled and sets the identifier; a move under
    ``#if DEBUG`` must red here, not on a device."""
    assert automation_text_sets_release_id()
    assert "gatedPostBadge" in composed_release_sites()


def test_the_driver_reads_ids_at_all():
    """Guards the pin below against a vacuous green: a driver edit that stopped
    spelling ids as ``Ids.<x>`` would otherwise check nothing."""
    assert {"importIdentityButton", "pasteSecretField", "handleInput", "mainTabView"} <= driver_ids()
    assert {"subscriptionProviderAddButton", "composeSellPrice", "offlineShareButton",
            "subscriptionOfferPrice", "gatedPostBuyButton"} <= driver_absent_ids()


def _view_sources():
    for root in VIEW_ROOTS:
        for path in root.rglob("*.swift"):
            if "/Generated/" in str(path) or "/Testing/" in str(path):
                continue
            yield path.read_text(errors="replace")


def test_every_id_the_driver_reads_survives_a_release_build():
    sites: set[str] = set(composed_release_sites())
    for text in _view_sources():
        sites |= release_identifier_sites(text)
    missing = sorted(driver_ids() - sites)
    assert not missing, (
        f"the iOS store driver reads {missing}, but no iOS view sets them with "
        f"`.accessibilityIdentifier(Ids.<x>)` outside `#if DEBUG` — a TestFlight build "
        f"would not expose them. Add the modifier beside the view's `.automation*` "
        f"registration, or drive the step by another id."
    )


# ── the store-install absences ────────────────────────────────────────────
#
# An absence is worth something only when the id is one a full build would
# draw AND one the store flavor is meant to drop. Without the first, "not in the
# tree" holds of any id at all (a misspelling, a view never built on iOS); without
# the second, the device run would be asserting the absence of something the
# store build is supposed to keep.

_EXCISED_EXT = re.compile(r"^#if !FAUNA_EXCISE_\w+\s*\nextension Ids \{(.*?)^\}", re.M | re.S)
_MEMBER = re.compile(r'public static let (\w+) = "([^"]+)"')


def excised_members() -> dict[str, str]:
    """``Ids`` members declared only under a ``#if !FAUNA_EXCISE_*`` condition
    of ``UiIds.swift`` — the ids the store-safe flavor does not carry at all
    (``dynamic-features.md`` § What "completely compiled away" means)."""
    text = UI_IDS.read_text()
    return {m: v for block in _EXCISED_EXT.findall(text) for m, v in _MEMBER.findall(block)}


def test_every_id_asserted_absent_is_one_the_store_flavor_excises():
    excised = excised_members()
    assert excised, "found no `#if !FAUNA_EXCISE_*` Ids extension in UiIds.swift — the scan is broken"
    kept = sorted(driver_absent_ids() - set(excised))
    assert not kept, (
        f"the store driver asserts {kept} absent from the store install, but UiIds.swift does "
        f"not declare them under a `#if !FAUNA_EXCISE_*` condition — the store flavor keeps them"
    )


def test_every_id_asserted_absent_is_drawn_by_an_ios_view():
    excised = excised_members()
    views = "\n".join(_view_sources())
    undrawn = sorted(
        m for m in driver_absent_ids()
        if m in excised and not re.search(rf"\bIds\.{m}\b", views) and f'"{excised[m]}"' not in views
    )
    assert not undrawn, (
        f"the store driver asserts {undrawn} absent, but no iOS view draws them even in a "
        f"full build — the absence would hold of an id nothing paints"
    )


def test_the_settings_rows_the_driver_opens_are_rail_keys():
    """``openSettingsPage(key:)`` taps ``settings-nav-row[<key>]``; every key
    must be one ``SettingsPage`` gives a rail row."""
    keys = set(re.findall(r'openSettingsPage\(key: "([\w-]+)"', DRIVER.read_text()))
    assert keys, "the driver opens no Settings page by key — the scan is broken"
    page = SETTINGS_PAGE.read_text()
    unknown = sorted(k for k in keys if f'"{k}"' not in page)
    assert not unknown, f"the store driver opens Settings rows {unknown}, which SettingsPage.swift never names"

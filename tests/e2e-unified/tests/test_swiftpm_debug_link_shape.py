"""The debug SwiftPM link shape (mac-disk-budget.md § Where the disk goes and what
frees it (mac), what `swift test` writes).

Each pin below guards a failure that does not show up as a failure. Text analysis
only: the real witness is `just swift-test` reporting the full swift-testing count.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = (_REPO / "justfile").read_text(encoding="utf-8")
_MANIFEST = (_REPO / "apps/fauna-apple/Package.swift").read_text(encoding="utf-8")
_SKIP_ENV = "FAUNA_SWIFTPM_SKIP_LIBRARY_PRODUCTS"


def _swiftpm_debug() -> str:
    match = re.search(r'^swiftpm_debug := "(.*)"$', _JUSTFILE, re.M)
    assert match, "the swiftpm_debug variable is gone"
    return match.group(1)


def test_debug_links_keep_main_exported():
    """SwiftPM's testing helper finds a test bundle's entry point by symbol. With
    `-no_exported_symbols`, `swift test` ran none of the 1,227 swift-testing tests
    and still exited 0."""
    flags = _swiftpm_debug()
    assert "-no_exported_symbols" not in flags
    assert "-exported_symbol -Xlinker _main" in flags


def test_only_swift_test_skips_the_library_products():
    """The `.xcodeproj` links these products, and it reads the same manifest, so the
    variable must never reach an `xcodebuild`: a global export would break the app
    build. The manifest and the one recipe line must also agree on the name."""
    lines = [ln for ln in _JUSTFILE.splitlines() if f"{_SKIP_ENV}=1" in ln]
    assert len(lines) == 1, lines
    assert "swift test" in lines[0]
    assert f'environment["{_SKIP_ENV}"] == "1"' in _MANIFEST
    assert not re.search(rf"^export\s+{_SKIP_ENV}", _JUSTFILE, re.M)

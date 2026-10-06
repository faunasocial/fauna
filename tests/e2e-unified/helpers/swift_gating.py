"""Text-analysis primitives shared by the apple convention-15 gating tests.

Convention 15 (`docs/goal/architecture/e2e-automation-surface-gating.md` § The
convention) compiles the automation surface out of Release builds with `#if DEBUG`
on Swift. Every witness that pins that on the SOURCE — `test_apple_seam_gating.py`
for the `test-helpers` UniFFI seam call sites, `test_apple_release_surface_gating.py`
for the File Provider test CLI — needs the same three questions answered about
hand-written apple Swift, so the answers live here once: which files a human
wrote, what is code rather than comment, and which lines only a DEBUG build
compiles.

Pure text analysis — no build, no toolchain — so it runs on any dev machine.
"""

import re
from pathlib import Path

#: Path fragments of Swift that never compiles into a Release build (or is
#: generated, so a human did not write it): the gitignored generated bindings,
#: the built xcframework, SwiftPM build output, and the SwiftPM/Xcode
#: test-target directories.
_NOT_HAND_WRITTEN_RELEASE_CODE = (
    "/generated/",
    "/FaunaFFI.xcframework/",
    "/.build/",
    "/FaunaKit/Tests/",
    "/FaunaiOSTests/",
    "/Fauna-macOSTests/",
)


def strip_comments(src: str) -> str:
    """Blank out `//`/`///` and `/* */` comments, preserving line structure, so a
    doc comment *naming* a seam (several do) is not mistaken for a call site."""
    out, i, n = [], 0, len(src)
    while i < n:
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif src.startswith("/*", i):
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out.append("".join(c if c == "\n" else " " for c in src[i:j]))
            i = j
        else:
            out.append(src[i])
            i += 1
    return "".join(out)


def debug_protected_lines(src: str) -> set[int]:
    """1-indexed line numbers sitting inside an *active* `#if DEBUG` branch.

    Tracks nesting, because apple nests `#if os(iOS)` inside `#if DEBUG` and vice
    versa: a line counts as protected when ANY enclosing conditional is a DEBUG
    condition whose true-branch we are currently in. `#else` of a `#if DEBUG`
    therefore does NOT protect — that arm is exactly the production build.
    """
    protected: set[int] = set()
    stack: list[bool] = []  # per open #if: does this level currently protect?
    for lineno, raw in enumerate(src.splitlines(), start=1):
        s = raw.strip()
        if s.startswith("#if"):
            stack.append(bool(re.search(r"\bDEBUG\b", s)) and not re.search(r"!\s*DEBUG", s))
        elif s.startswith("#elseif"):
            if stack:
                stack[-1] = bool(re.search(r"\bDEBUG\b", s)) and not re.search(r"!\s*DEBUG", s)
        elif s.startswith("#else"):
            if stack:
                stack[-1] = False
        elif s.startswith("#endif"):
            if stack:
                stack.pop()
        elif any(stack):
            protected.add(lineno)
    return protected


def hand_written_swift(apple_root: Path) -> list[Path]:
    """Apple Swift a human wrote, under `apple_root` (`apps/fauna-apple`).

    Excludes the gitignored generated bindings (`generated/`, the copies under
    `FaunaFFISwift/Sources/`, and whatever the built xcframework carries), SwiftPM
    build output, and the SwiftPM/Xcode test-target directories — none of them
    compile into a Release build, so a test seam call there is exactly what the
    seam exists for."""
    out = []
    for f in apple_root.rglob("*.swift"):
        # POSIX form: the exclusion fragments are `/`-separated, and `str(f)` on
        # Windows is `\`-separated — which silently excluded nothing there.
        p = f.as_posix()
        if any(part in p for part in _NOT_HAND_WRITTEN_RELEASE_CODE):
            continue
        # FaunaFFISwift/Sources holds generated copies + the tracked, hand-written
        # FFICompat.swift; keep only the latter.
        if "/FaunaFFISwift/Sources/" in p and f.name != "FFICompat.swift":
            continue
        out.append(f)
    return sorted(out)

"""Convention 15, apple's app-side half: no hand-written Swift may reference a
`test-helpers` UniFFI seam from code that compiles into a Release build.

`docs/goal/architecture/testing.md` § Cross-app e2e conventions point 15. The
recipe split (2026-08-02, pinned by `test_ffi_flavor_split.py`) makes the
production apple FFI flavor stop exporting the seams; this file pins the other
half — that the Swift *call sites* stay behind `#if DEBUG`, which is what lets
the production flavor actually compile.

**Why this exists as a text test.** The mac merge gate (`apple-swift-build-check`
→ `mac-debug`) compiles the DEBUG configuration against the TEST FFI flavor, so
it can never see this class: a newly added ungated seam call site compiles fine
there and breaks only when someone builds `mac-release` / `mac-app release` /
`mac-dmg` — i.e. at release time, on the artifact that ships. Compiling the
production flavor in the gate would mean a second full `fauna-ffi` build at a
different feature set (a different cargo fingerprint ⇒ a near-full recompile each
way), which is too expensive for a merge gate. This costs milliseconds and
catches the same class.

**The seam set is DERIVED, not maintained.** It is read from the Rust source of
truth — the crates `libs/fauna-ffi`'s own `test-helpers` feature forwards to —
so a seam added there is covered here with no list for anyone to update. That is
the same self-maintaining shape as the production bindgen's own witness and
`scripts/check-wasm-seam-exclusion.py`.

Pure text analysis of Rust + Swift sources — no build, no toolchain, no driver,
so it runs on any development machine, not only the macOS ones that can compile
the Swift it scans.
"""

import re
from pathlib import Path

import pytest

from helpers.rust_attr_scan import iter_attr_bounds, strip_strings_and_comments
from helpers.swift_gating import debug_protected_lines as _debug_protected_lines
from helpers.swift_gating import hand_written_swift
from helpers.swift_gating import strip_comments as _strip_comments

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_APPLE = _REPO / "apps" / "fauna-apple"

# The one seam the export scan cannot see: a TYPE, not an exported method.
# `FfiChildAgentSpawner::spawn_agent()` execs a path read verbatim from
# `FAUNA_E2E_SYNC_AGENT_BIN` — a process-execution redirect. Gated in
# `libs/fauna-ffi/src/sync_agent_provisioning.rs` on 2026-08-02. Swift reaches it
# through the type name (its constructor), and constructors are deliberately left
# out of the derived set (every export impl has a `new`, which would match 200+
# ungated production lines), so the type name stays hand-listed. Every
# `test-helpers`-gated export named like ordinary API (`deviceSetStateJson`,
# `readMarkerStateJson`, `custodianRunPassNow`, …) is derived below, not listed.
_UNCONVENTIONAL_SEAMS = {"FfiChildAgentSpawner"}

_UNIFFI_EXPORT = re.compile(r"\b(?:uniffi|fauna_uniffi_async)\s*::\s*export\b")
_UNIFFI_CONSTRUCTOR = re.compile(r"\buniffi\s*::\s*constructor\b")
_TEST_HELPERS_GATE = re.compile(r'feature\s*=\s*"test-helpers"')
_IMPL_HEAD = re.compile(r"\bimpl\b[^{};]*\{")
_FN_NAME = re.compile(r"\bfn\s+(\w+)")
_ITEM_QUALIFIERS = re.compile(r"(?:(?:pub(?:\([^)]*\))?|async|unsafe|const)\s+)*")


def _forwarded_crates() -> list[str]:
    """The crates `fauna-ffi`'s own `test-helpers` feature forwards to."""
    manifest = (_REPO / "libs" / "fauna-ffi" / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"^test-helpers\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, "libs/fauna-ffi/Cargo.toml has no `test-helpers` feature declaration"
    return re.findall(r'"([a-z0-9-]+)/test-helpers"', m.group(1))


def _snake_to_lower_camel(name: str) -> str:
    head, *rest = name.split("_")
    return head + "".join(p[:1].upper() + p[1:] for p in rest)


def _attrs_above(
    text: str, clean: str, bounds: list[tuple[int, int]], head: int
) -> tuple[list[str], list[str]] | None:
    """The attributes stacked directly above the item whose keyword sits at
    `head`, as `(stripped, raw)` inner texts — `raw` read off the unstripped
    `text`, because stripping blanks the `"test-helpers"` literal a gate match
    needs. Everything between the previous item boundary and the item's
    qualifiers (`pub`, `async`, …) must be attributes, else `None`: so
    `-> impl Future {` in a signature is not an `impl` block."""
    boundary = max(clean.rfind(c, 0, head) for c in ";{}") + 1
    inside = [(s, e) for s, e in bounds if boundary <= s and e <= head]
    region = clean[boundary:head]
    for s, e in reversed(inside):
        region = region[: s - boundary] + region[e - boundary :]
    if not _ITEM_QUALIFIERS.fullmatch(region.strip() + " " if region.strip() else ""):
        return None
    return (
        [clean[s + 2 : e - 1] for s, e in inside],
        [text[s + 2 : e - 1] for s, e in inside],
    )


def _is_export(stripped: list[str]) -> bool:
    """A UniFFI export in either macro spelling, also behind `cfg_attr(…)`."""
    return any(_UNIFFI_EXPORT.search(a) for a in stripped)


def _is_test_helpers_gated(stripped: list[str], raw: list[str]) -> bool:
    """Compiled only with `test-helpers` on: a `cfg(…)` naming the feature, or a
    `cfg_attr(<it>, …export)` making the export itself conditional on it. A
    `not(…)` anywhere in the attribute disqualifies it (the inverse gate)."""
    for a, r in zip(stripped, raw):
        head = a.lstrip()
        if head.startswith(("cfg(", "cfg_attr(")) and _TEST_HELPERS_GATE.search(r):
            if "not(" not in r.replace(" ", ""):
                return True
    return False


def _derive_gated_exports(path: Path) -> set[str]:
    """Swift-facing names of the methods and functions in `path` that a
    `test-helpers`-gated UniFFI export produces — the gate on an `impl` block
    covers every method in it, a gate on one method only that one. Constructors
    are skipped (their Swift face is `init`, not a name)."""
    text = path.read_text(encoding="utf-8", errors="ignore")
    clean = strip_strings_and_comments(text)
    bounds = iter_attr_bounds(clean)
    names: set[str] = set()
    impl_spans: list[tuple[int, int]] = []
    for m in _IMPL_HEAD.finditer(clean):
        got = _attrs_above(text, clean, bounds, m.start())
        if got is None:
            continue
        depth, end = 1, m.end()
        while depth and end < len(clean):
            depth += {"{": 1, "}": -1}.get(clean[end], 0)
            end += 1
        impl_spans.append((m.end(), end))
        block_export = _is_export(got[0])
        block_gated = _is_test_helpers_gated(*got)
        level = 0
        for i in range(m.end(), end):
            ch = clean[i]
            if ch == "{":
                level += 1
            elif ch == "}":
                level -= 1
            elif ch == "f" and level == 0 and not (clean[i - 1].isalnum() or clean[i - 1] == "_"):
                fm = _FN_NAME.match(clean, i)
                fgot = _attrs_above(text, clean, bounds, i) if fm else None
                if fm is None or fgot is None or fm.group(1) == "new":
                    continue
                if any(_UNIFFI_CONSTRUCTOR.search(a) for a in fgot[0]):
                    continue
                if (block_export or _is_export(fgot[0])) and (
                    block_gated or _is_test_helpers_gated(*fgot)
                ):
                    names.add(_snake_to_lower_camel(fm.group(1)))
    # Free `#[uniffi::export] pub fn`s outside any impl block.
    for fm in _FN_NAME.finditer(clean):
        if any(s <= fm.start() < e for s, e in impl_spans):
            continue
        got = _attrs_above(text, clean, bounds, fm.start())
        if got is not None and _is_export(got[0]) and _is_test_helpers_gated(*got):
            names.add(_snake_to_lower_camel(fm.group(1)))
    return names


# Derived names that also have a PRODUCTION twin under the same Swift name, so
# deriving them would redden the witness on code that is not a seam call:
# `FfiChildAgentSpawner::spawn_agent` (gated) vs the production
# `SyncAgentSpawner.spawnAgent()` in `Fauna-macOS/Sync/SyncAgentSpawner.swift`, a
# Swift-side DECLARATION of the same-named method on the app's own spawner. The
# gated seam is still guarded through its type name, `FfiChildAgentSpawner`,
# which is the only way Swift can reach it.
_PRODUCTION_TWINS = {"spawnAgent"}


def _seam_names() -> set[str]:
    """Swift-facing names of every seam in the forwarded crates plus
    `libs/fauna-ffi` itself: the `*_for_test(s)` fns, every method or function a
    `test-helpers`-gated UniFFI export carries under whatever name, and the
    hand-listed remainder above.

    **Why this source-side derivation is not the refused cfg-scan.**
    `e2e-automation-surface-gating.md` refuses a cfg-scan over the Rust source as
    the *binding-face* witness (the production flavor's generated Swift is diffed
    by `check-ffi-seam-diff.py` instead) and carves out source-level call-site
    coverage as a separate job. This is that job: the production flavor's Swift
    bindings carry exactly what is exported, so the symbols an ungated call site
    can break on ARE the `test-helpers`-gated exports, enumerable from the Rust.
    It sees only exports; seams that are not exports (env reads, process spawn)
    stay the 2026-08-02 ruling's acknowledged blind spot."""
    names: set[str] = set(_UNCONVENTIONAL_SEAMS)
    roots = [_REPO / "libs" / c / "src" for c in _forwarded_crates()]
    roots.append(_REPO / "libs" / "fauna-ffi" / "src")
    for root in roots:
        if not root.is_dir():
            continue
        for f in root.rglob("*.rs"):
            for rust_name in re.findall(
                r"\bpub fn (\w+_for_tests?)\b", f.read_text(encoding="utf-8", errors="ignore")
            ):
                names.add(_snake_to_lower_camel(rust_name))
            names |= _derive_gated_exports(f)
    return names - _PRODUCTION_TWINS


def _hand_written_swift() -> list[Path]:
    return hand_written_swift(_APPLE)


def test_the_seam_set_derives_non_trivially():
    """Guard the derivation itself: a regex that silently stopped matching would
    make every assertion below vacuously pass — the classic way a self-maintaining
    witness rots into decoration."""
    names = _seam_names()
    assert len(names) >= 20, (
        f"only {len(names)} seam names derived from the Rust sources — the "
        "derivation is probably broken, which would make the gating test below "
        f"vacuous. Got: {sorted(names)}"
    )
    # The first three are the `*_for_test` convention; the last three are
    # `test-helpers`-gated exports named like ordinary API, derived from the
    # gated `#[fauna_uniffi_async::export]` blocks with no hand list behind them.
    for expected in (
        "clearForTest",
        "installMockBackendsForTest",
        "setPhaseForTest",
        "deviceSetStateJson",
        "readMarkerStateJson",
        "custodianRunPassNow",
    ):
        assert expected in names, (
            f"{expected!r} missing from the derived seam set — derivation is broken"
        )
    assert "new" not in names, "constructors must stay out of the derived seam set"


def test_no_apple_swift_reaches_a_seam_outside_if_debug():
    """The production apple FFI flavor does not export the seams, so any
    reference from code that survives into a Release build fails to compile —
    or, worse, is a PRODUCTION caller that was quietly relying on a test seam.

    That second case is not hypothetical: `tearDownSessionForSwitch` (the
    multi-account switch on both apple apps) called `clearForTest()` to wipe
    the outgoing identity's threads and drafts, and `#if DEBUG`-ing it would have
    shipped an account switch that renders the previous account's conversations
    to the incoming one. It was fixed by adding a PRODUCTION twin in shared Rust
    (`ConversationsManager::clear_for_identity_change`), not by gating the call —
    which is the right move whenever a flagged site turns out to be doing a real
    job. Gate it only when the site is genuinely test-only.
    """
    seams = _seam_names()
    # `(?<!\w)`, NOT the `(?<![\w.])` guard `lint-ffi-construction-sites.py` uses:
    # there, the dot exists to stop `self.init(` matching; here every seam call is
    # a method call (`manager.installMockBackendsForTest()`), so excluding a
    # leading dot makes the whole assertion vacuous. Verified by negative control —
    # un-gating a known site must redden this test.
    pattern = re.compile(r"(?<!\w)(" + "|".join(sorted(map(re.escape, seams))) + r")\b")
    offenders: list[str] = []
    for f in _hand_written_swift():
        raw = f.read_text(encoding="utf-8", errors="ignore")
        protected = _debug_protected_lines(raw)
        for lineno, line in enumerate(_strip_comments(raw).splitlines(), start=1):
            if lineno in protected:
                continue
            for m in pattern.finditer(line):
                offenders.append(
                    f"{f.relative_to(_REPO)}:{lineno}: {m.group(1)} — {line.strip()[:100]}"
                )
    assert not offenders, (
        "hand-written apple Swift references a `test-helpers` UniFFI seam from code "
        "that compiles into a Release build. The production FFI flavor does not export "
        "these, so `mac-release`/`mac-app release`/`mac-dmg` will not compile "
        "(testing.md § convention 15).\n"
        "Fix: if the site is genuinely test-only, put it behind `#if DEBUG`. If it is a "
        "PRODUCTION caller, it needs a production method in shared Rust instead — see "
        "`ConversationsManager::clear_for_identity_change`.\n  " + "\n  ".join(offenders)
    )

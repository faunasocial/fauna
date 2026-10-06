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

from helpers.swift_gating import debug_protected_lines as _debug_protected_lines
from helpers.swift_gating import hand_written_swift
from helpers.swift_gating import strip_comments as _strip_comments

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_APPLE = _REPO / "apps" / "fauna-apple"

# Seams whose names do NOT follow the `*_for_test` convention, so the derivation
# below cannot see them. Convention 15's 2026-08-02 seam-witness ruling states
# outright that the automated witnesses see only CONVENTIONALLY-NAMED seams, so
# this short list is the acknowledged remainder — not a maintained inventory of
# all seams. Add to it when a `test-helpers`-gated export is named unconventionally.
_UNCONVENTIONAL_SEAMS = {
    # `FfiChildAgentSpawner::spawn_agent()` execs a path read verbatim from
    # `FAUNA_E2E_SYNC_AGENT_BIN` — a process-execution redirect. Gated in
    # `libs/fauna-ffi/src/sync_agent_provisioning.rs` on 2026-08-02.
    "FfiChildAgentSpawner",
    # `FfiNestClient::device_set_state_json` — the fleet-removal convergence
    # reader (`account-data-taxonomy.md` § The generation machinery →
    # *Fleet-scope reclamation*, clause (4)): a plane-content read gated on
    # `test-helpers` in `libs/fauna-ffi/src/nest_client.rs`, named like ordinary
    # API rather than `*_for_test`. The production apple flavor's Swift bindings
    # do not carry it, so an ungated call fails `-c release`.
    "deviceSetStateJson",
    # `FfiSyncAgentProvisioner::custodian_run_pass_now` — the custodian-pull
    # causal-barrier poke, gated on `test-helpers` in
    # `libs/fauna-ffi/src/sync_agent_provisioning.rs` and named like ordinary
    # API. Same class as `deviceSetStateJson` above: absent from the production
    # flavor's Swift bindings, so an ungated call site fails `-c release`.
    "custodianRunPassNow",
}


def _forwarded_crates() -> list[str]:
    """The crates `fauna-ffi`'s own `test-helpers` feature forwards to."""
    manifest = (_REPO / "libs" / "fauna-ffi" / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"^test-helpers\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, "libs/fauna-ffi/Cargo.toml has no `test-helpers` feature declaration"
    return re.findall(r'"([a-z0-9-]+)/test-helpers"', m.group(1))


def _snake_to_lower_camel(name: str) -> str:
    head, *rest = name.split("_")
    return head + "".join(p[:1].upper() + p[1:] for p in rest)


def _seam_names() -> set[str]:
    """Swift-facing names of every `*_for_test(s)` fn in the forwarded crates,
    plus `libs/fauna-ffi` itself, plus the unconventional remainder above."""
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
    return names


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
    for expected in ("clearForTest", "installMockBackendsForTest", "setPhaseForTest"):
        assert expected in names, (
            f"{expected!r} missing from the derived seam set — derivation is broken"
        )


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

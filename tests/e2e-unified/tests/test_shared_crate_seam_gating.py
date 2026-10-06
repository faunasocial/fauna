"""Convention 15, the shared-Rust half's rule (a): every `_for_test` seam in a
`libs/fauna-*` crate, a `bins/*` binary OR one of the two Rust apps
(`apps/*/src/`) must be compiled out of release artifacts.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention,
the **Shared Rust crates (`libs/fauna-*`)** bullet, rule (a). Four existing
witnesses already cover the other faces of convention 15:
`test_ffi_flavor_split.py` (native-FFI), `check-wasm-seam-exclusion.py`
(wasm), `test_apple_seam_gating.py` (apple call sites), and
`test_agent_ipc_seam_gating.py` (the sync-agent IPC verb set). All four key on
a FEATURE or a generated-bindings face. A seam that names no feature and
crosses no UniFFI/wasm/IPC boundary — an ordinary `pub fn` on a shared-crate
or binary-crate struct, called only from Rust — leaves no trace in any of
those and is invisible to all four (`test_agent_ipc_seam_gating.py`'s own
docstring already makes this argument for `RequestMethod`; this file closes
the larger class it did not).

**Widened to `bins/*/src/`.** The original version of
this scanner covered `libs/fauna-*/src/` only, on the theory that `bins/`
binaries have no "release artifact" reuse concern the way a shared crate
does. That theory does not hold: `bins/fauna-nest` ships both `src/lib.rs`
and `src/main.rs`, so a `pub fn` there is genuinely cross-crate reachable
(consumed by `bins/fauna-nest/tests/*.rs` integration binaries and, in
principle, any future in-process consumer), not bin-private — and applying
this scanner's own gate logic to `bins/*/src/` unmodified found 5 real,
previously-ungated `_for_test` seams in `bins/fauna-nest`, one of them
(`clear_outbound_for_test`) destructive.

**Widened to `apps/*/src/` (2026-09-24).** A seam in
an app ships in that app's own release artifact, so the rule is the same;
the app half of convention 15 spells the gate `any(debug_assertions, feature
= "e2e-agent")`, both clauses already in `_SAFE_GATE_CLAUSES`. The widening
found three ungated fauna-desktop seams (gated in the same pass) and needed
one more resolution path first — see `_rust_source_files_including_apps`.

**The load-bearing detail: the gate can sit on the fn OR on its ENCLOSING
`impl` BLOCK.** `libs/fauna-onboarding-machine/src/machine.rs` has 20
`pub fn *_for_test` methods; 19 of them carry no fn-level gate at all — they
sit inside one `impl OnboardingMachine` block whose OWN `#[cfg(...)]`
(directly above the `impl` keyword) gates all 19 at once. A fn-level-only
scan would report all 19 as ungated false positives (measured against this
exact file during this row's own filing) and get ratcheted or deleted the
first time someone ran it.

**The seam set is DERIVED, not maintained**: every `pub fn`/`pub async fn`
whose name matches `*_for_test` under `libs/fauna-*/src/`, `bins/*/src/` or
`apps/*/src/` is
found by regex, so a new seam added tomorrow is covered with no list for
anyone to update.

Pure text analysis of Rust sources — no build, no driver. **Stays in the
tier_1 suite, NOT wired into the cheap merge-gate tier** (measured: a few
seconds to scan the full `libs/fauna-*/src/` + `bins/*/src/` tree, not the
sub-second bar the owning row set for that wiring) — run via
`just e2e-tier-1-test` or directly with `pytest`.

**Module-level complement.** A `pub fn
*_for_test` is not the only shape test infrastructure takes: a WHOLE MODULE
can lend test-only construction helpers with no `_for_test`-suffixed fn
anywhere in it — `bins/fauna-nest/src/test_support.rs` (246 lines, 6 pub
items, zero `_for_test` names) is exactly this, and the scan above cannot see
it. This file's second half reuses the SAME shape-matching
machinery (`_SAFE_GATE_CLAUSES`/`_is_safe_cfg_inner`/`_nearest_preceding_gate`
above) against a different selector: every `(pub )?mod` declaration whose
name contains `test_support`, under the same roots. Applying it, unmodified,
found two more correctly-gated instances of the same shape this file had never
scanned before: `bins/fauna-nest/src/bridge_approval_test_support.rs` (bare
`#[cfg(test)]`) and `libs/fauna-ipc/src/lib.rs`'s `test_support`
(`#[cfg(all(windows, any(test, debug_assertions, feature = "test-helpers")))]`
— the latter's bare `windows` qualifier was itself a genuine gap in
`_ARCH_QUALIFIER` below, which only recognized `target_arch = "..."` as a safe
narrowing clause; widened in the same pass). Deliberately narrow — `*test_support*`,
not a bare `*test*` — because `bins/fauna-nest/src/lib.rs`
also declares `pub mod protocol_test;`, an ungated but *shipped* RPC echo
handler unrelated to test infrastructure (a separate, unfiled question, not
swept into this scan). `*test_hook*` modules are a distinct, already-covered
family (`test_nest_test_hook_seam_gating.py`) and are not re-scanned here.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_LIBS = _REPO / "libs"
_BINS = _REPO / "bins"
_APPS = _REPO / "apps"


def _rust_source_files() -> list[Path]:
    """Every `.rs` file under a `libs/fauna-*/src/` crate or a `bins/*/src/`
    binary, in a stable sorted order — the two shared-code roots (§ module
    docstring, "Widened to `bins/*/src/`"). Every scan here now also reads
    `apps/*/src/` through `_rust_source_files_including_apps` below; this
    narrower list is kept for the one lookup that is deliberately shared-only
    (`_e2e_env_const_names`)."""
    return sorted(_LIBS.glob("fauna-*/src/**/*.rs")) + sorted(_BINS.glob("*/src/**/*.rs"))


def _rust_source_files_including_apps() -> list[Path]:
    """The two roots above PLUS `apps/*/src/` — the file set all three scans
    in this file walk.

    **Why the scans reach into the apps.**

    Until 2026-09-22 no scanner of any kind read the two Rust apps' own
    sources: this file stopped at `libs/`+`bins/`, and so did
    `test_ffi_flavor_split.py`'s `_RUST_SCAN_ROOTS`. The C# app has its own pin
    (`test_no_windows_app_source_names_a_fauna_e2e_var_outside_the_gate`); tui
    and fauna-desktop had none, while carrying ~80 hand-written
    `feature = "e2e-agent"` gates across 20 files. The blind spot produced a
    real leak — `backups::download_dir()` read `FAUNA_E2E_DOWNLOAD_DIR`
    ungated, FIRST, on a production path, so a release tui let the launch
    environment redirect a user's exported account data — and it was found by
    `strings` on the artifact, not by any test. The env-seed scan widened
    first, because it could widen clean: applied to
    `apps/*/src/` its gate logic reports **zero** false positives (measured
    2026-09-22, 329 files), because `_statement_start` + `_ITEM_HEAD` +
    the enclosing-`impl`/inline-`mod` checks already cover every shape the two
    apps use.

    The `_for_test` and `test_support` scans followed (2026-09-24), and could
    not widen clean. Over `apps/*/src/` the `_for_test` leg reported four, which
    were not one finding: three were real ungated seams in fauna-desktop, driven
    only by its gated test-agent command dispatcher
    (`p2p.rs::{force,clear}_bind_conflict_for_test`,
    `devices_folders::inject_locations_for_test`, now gated like that
    dispatcher), and the fourth was a FALSE POSITIVE —
    `apps/fauna-tui/src/settings/mod.rs::nest_for_test`, inside a
    `#[cfg(test)] mod tests` block the leg did not walk up to. So the leg was
    taught the enclosing-inline-`mod` check the env-read scan already had
    (`_for_test_seam_gates`), pinned by name in
    `test_the_scan_actually_finds_gated_seams`, before its roots widened. The
    `test_support` leg found nothing new under `apps/`.

    `test_ffi_flavor_split.py::_RUST_SCAN_ROOTS` reads `apps/` too (widened
    2026-09-24), so both harness-env sweeps now cover
    both Rust apps. That sweep's pattern is the wider of the two — it also
    holds `FAUNA_KEYRING_APP`, whose only `apps/` reader
    (`apps/fauna-linux/src/client.rs`) sits inside a `#[cfg(test)] mod` today —
    so that variable needs no entry in this scan's pattern. Its widening needed
    `_enclosing_scope_cfg_attrs` taught to climb past a multi-line parameter
    list first; this scan's `_statement_start` never had that blind spot.
    """
    return _rust_source_files() + sorted(_APPS.glob("*/src/**/*.rs"))


# The primitive conditions that are each independently absent from a
# plain `cargo build --release` with default features: `test` (the crate's
# own `cargo test`), `debug_assertions` (off under `--release`; meaningless
# for `libs/fauna-wasm*`, whose `wasm-pack build` is release-profile
# internally regardless of dev/prod flavor — testing.md convention 15 rule
# (b)), and the explicit `test-helpers`/`test-hooks` opt-in features. This
# crate/binary family does NOT wear one canonical gate string the way each of
# the four sibling witnesses' own single crate family does — measured
# DIFFERENT, each individually correct, spellings in the wild: the full
# `any(test, debug_assertions, feature = "test-helpers")` (native crates that
# also build under `debug_assertions`), `feature = "test-helpers"` alone
# (`libs/fauna-wasm*`, per rule (b) above), `any(test, feature =
# "test-helpers")` (`fauna-mls`, built both native and wasm so
# `debug_assertions` alone would be meaningless there too), bare `test`
# (strictest of all: compiles under NO build but that crate's own `cargo
# test`, so it trivially satisfies "never ships" and needs no `test-helpers`
# opt-in at all), `feature = "e2e-agent"` (`fauna-client-atproto`'s own
# opt-in name for the same shape, predating `test-helpers` in that crate,
# `Cargo.toml`: "Off by default, so the override is compiled out of shipped
# artifacts entirely"), and `feature = "test-hooks"` — the nest's own
# spelling of the same opt-in (`bins/fauna-nest/Cargo.toml`, not in
# `default`, already carried by six `required-features` test targets there;
# `e2e-automation-surface-gating.md` § The convention names it as the
# precedent `e2e-agent` mirrors), and `feature = "tls-test-fixtures"`
# (`libs/fauna-mail`'s fake IMAP-over-TLS test server — see its own comment
# below). **Honest framing:**
# what this checks the SHAPE of is the ATTRIBUTE'S BOOLEAN STRUCTURE — a bare
# clause, or an `any(...)`/`all(...)` nesting of the clauses below, one level
# deep (`_gate_is_safe`) — not the clause NAMES themselves. A gate spelled with
# a not-yet-seen opt-in feature still needs its literal string added to
# `_SAFE_GATE_CLAUSES` below; the shape check only spares this scanner from
# also hardcoding every crate's *combination* of these atoms.
_SAFE_GATE_CLAUSES = {
    "test",
    "debug_assertions",
    'feature = "test-helpers"',
    'feature = "e2e-agent"',
    'feature = "test-hooks"',
    # `libs/fauna-mail`'s `imap_client::test_fixtures` module (a fake
    # IMAP-over-TLS server) — not in that crate's `default` list
    # (`libs/fauna-mail/Cargo.toml`). A derivation ("safe when the feature is
    # absent from the crate's `default` list") was considered instead of a
    # sixth hardcoded spelling, but rejected: a feature absent from `default`
    # can still be reachable through the TRANSITIVE closure of default-enabled
    # features (`fauna-mail`'s own `default` list forwards through two dozen
    # features), so a correct derivation needs the whole feature graph, not a
    # direct-membership test — more machinery than this witness's one
    # currently-occupied blind spot (the module's fns carry no `*_for_test`
    # suffix today, so this entry pre-empts a future rename, not a live gap)
    # justifies. Revisit the derivation if a seventh spelling shows up.
    'feature = "tls-test-fixtures"',
    # `libs/fauna-client`'s `test-util` — off by default, and switched on only
    # by that crate's own self dev-dependency (`libs/fauna-client/Cargo.toml`),
    # so no shipping dependency line reaches it; it forwards the substrate's
    # `test-util`, the same dev-only opt-in `supervisor_channels_for_test`
    # sits behind.
    'feature = "test-util"',
}

# A qualifier that NARROWS a gate (one arch, or one OS family) without making
# it any less safe on its own — `libs/fauna-client-atproto/src/genesis_verify.rs`
# ANDs `target_arch = "wasm32"` into its wasm-only seam's gate via
# `all(target_arch = "wasm32", any(debug_assertions, feature = "e2e-agent"))`,
# and `libs/fauna-ipc/src/lib.rs`'s `test_support` module (found by the
# module-level scan below) ANDs the bare `windows`
# predicate into an otherwise-ordinary safety clause the same way:
# `all(windows, any(test, debug_assertions, feature = "test-helpers"))`.
# Narrowing to one arch or one OS family never WIDENS what ships (the code is
# simply absent everywhere the qualifier is false), so neither needs to
# appear in `_SAFE_GATE_CLAUSES` itself — each is simply ignored wherever it
# appears alongside a genuine safety clause inside an `all(...)`.
_ARCH_QUALIFIER = re.compile(
    r'^not\(target_arch = "[^"]*"\)$|^target_arch = "[^"]*"$'
    r"|^not\(windows\)$|^windows$|^not\(unix\)$|^unix$"
)


def _split_top_level(inner: str) -> list[str]:
    """Comma-split `inner` at PAREN DEPTH ZERO only, so `any(a, b)` inside an
    outer `all(any(a, b), c)` is not itself torn apart by the outer split."""
    parts: list[str] = []
    depth = 0
    current: list[str] = []
    for ch in inner:
        if ch == "(":
            depth += 1
            current.append(ch)
        elif ch == ")":
            depth -= 1
            current.append(ch)
        elif ch == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(ch)
    if current:
        parts.append("".join(current))
    return [p.strip() for p in parts if p.strip()]


def _is_safe_cfg_inner(inner: str) -> bool:
    """True iff `inner` — the parenthesized argument list of a `#[cfg(...)]`
    attribute — can only be true in a build that is not a plain release build
    — i.e. every safety-relevant clause it ANDs/ORs together is drawn from
    `_SAFE_GATE_CLAUSES`, an arch qualifier ignored on its own merits (see
    `_ARCH_QUALIFIER`), or `any(...)`/`all(...)` nesting one level deep of the
    same (the two real shapes measured in this codebase: a bare/`any(...)`
    clause list, or `all(<arch qualifier>, any(...) of safe clauses)`).
    """
    inner = inner.strip()
    all_m = re.fullmatch(r"all\((.*)\)", inner)
    if all_m:
        parts = _split_top_level(all_m.group(1))
        safety_parts = [p for p in parts if not _ARCH_QUALIFIER.match(p)]
        if len(safety_parts) != 1:
            return False
        inner = safety_parts[0]
    any_m = re.fullmatch(r"any\((.*)\)", inner)
    clauses = _split_top_level(any_m.group(1)) if any_m else [inner]
    return bool(clauses) and all(c in _SAFE_GATE_CLAUSES for c in clauses)


def _extract_attr_spans(text: str) -> list[str]:
    """Every `#[...]` attribute in `text`, as its inner content (the bytes
    between the brackets), found by depth-balanced bracket matching. This is
    what lets a `#[cfg(all(\n    target_arch = "wasm32",\n    any(...)\n))]`
    attribute — the shape `cargo fmt` produces once a clause list is long
    enough to wrap — be recovered as ONE span rather than several unmatched
    fragments (measured: `fauna-client-atproto/src/genesis_verify.rs` and
    `fauna-client-alert-sweep/src/lib.rs` both wear exactly this wrapped
    shape and were false-positived as ungated before this fix, since no
    individual physical line of a wrapped attribute matches `#[cfg(...)]` on
    its own).
    """
    spans: list[str] = []
    i = 0
    n = len(text)
    while True:
        start = text.find("#[", i)
        if start == -1:
            break
        depth = 1
        pos = start + 2
        while depth > 0 and pos < n:
            c = text[pos]
            if c == "[":
                depth += 1
            elif c == "]":
                depth -= 1
            pos += 1
        spans.append(text[start + 2 : pos - 1])
        i = pos
    return spans


_FOR_TEST_FN = re.compile(
    r"^(?P<indent>[ \t]*)pub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+(?P<name>\w*_for_test)\s*[<(]",
    re.MULTILINE,
)
_IMPL_HEAD = re.compile(r"^impl\b[^{]*\{", re.MULTILINE)
_TEST_SUPPORT_MOD = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+(?P<name>\w*test_support\w*)\s*;",
    re.MULTILINE,
)


def _strip_strings_and_comments(text: str) -> str:
    """Blank out string/char literal and comment bodies, preserving every
    other byte (and all newlines) in place, so brace-depth tracking below
    cannot be fooled by a `"{"` inside a doc comment or a string constant.
    Deliberately simple (no raw-string `r#"..."#` handling — none of this
    crate family uses raw strings for brace-bearing content), matching the
    scanning rigor of this project's other pure-text Rust scanners.
    """
    out = list(text)
    i = 0
    n = len(text)
    while i < n:
        two = text[i : i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j == -1 else j
            for k in range(i, j):
                out[k] = " "
            i = j
        elif two == "/*":
            j = text.find("*/", i + 2)
            j = n if j == -1 else j + 2
            for k in range(i, j):
                if out[k] != "\n":
                    out[k] = " "
            i = j
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"':
                if text[j] == "\\":
                    j += 1
                j += 1
            j = min(j + 1, n)
            for k in range(i, j):
                if out[k] != "\n":
                    out[k] = " "
            i = j
        elif text[i] == "'" and i + 1 < n and (text[i + 1] == "\\" or (i + 2 < n and text[i + 2] == "'")):
            # A char literal, not a lifetime — the closing `'` follows within
            # 1 (escaped) or 2 (plain) chars. A lifetime (`'a`) never closes,
            # so it falls through untouched.
            j = i + 2 if text[i + 1] != "\\" else i + 3
            while j < n and text[j - 1] != "'":
                j += 1
            for k in range(i, min(j, n)):
                if out[k] != "\n":
                    out[k] = " "
            i = j
        else:
            i += 1
    return "".join(out)


def _nearest_preceding_gate(clean: str, text_lines: list[str], pos: int) -> bool:
    """True iff a safe `#[cfg(...)]` gate (see `_is_safe_cfg_inner`) appears
    anywhere in the unbroken run of non-blank lines immediately above `pos`
    — an attribute STACK, not only the single nearest line. Rust stacks
    multiple attributes with no blank line between them (e.g. `#[cfg(...)]`
    then `#[cfg_attr(..., uniffi::export)]` directly above one `impl`), so
    checking only the nearest line misses the gate whenever anything else is
    stacked above it (measured: `machine.rs`'s `impl` block at line 6134
    carries exactly this two-attribute stack and was misreported as ungated
    before this fix). The whole run is joined into one string before
    extracting attributes (`_extract_attr_spans`), so a single attribute
    `cargo fmt` wraps across several physical lines is recovered as one span
    rather than torn apart per-line.

    Line boundaries and "which lines are blank" are read off `clean` (the
    comment/string-stripped text — a `//` comment or blank doc-comment line
    must not count as code, and must not itself satisfy the gate check), but
    the actual TEXT extracted and checked is read off `text_lines`, the
    ORIGINAL unstripped source split by line. `clean` blanks out the
    `"test-helpers"` string literal along with every other string in the
    file — including the one inside the gate attribute itself — so checking
    `clean`'s own version of that line can never match (measured: it
    silently zeroed every gate hit before that fix).
    """
    clean_preceding = clean[:pos]
    idx = clean_preceding.count("\n")  # 0-based line index of `pos`
    clean_lines = clean_preceding.splitlines()
    i = idx - 1
    while i >= 0 and clean_lines[i].strip():
        i -= 1
    run_start = i + 1
    joined = " ".join(line.strip() for line in text_lines[run_start:idx])
    for attr in _extract_attr_spans(joined):
        m = re.fullmatch(r"cfg\((.*)\)", attr.strip())
        if m and _is_safe_cfg_inner(m.group(1)):
            return True
    return False


def _impl_block_spans(
    clean: str, text_lines: list[str], head: re.Pattern[str] = _IMPL_HEAD
) -> list[tuple[int, int, bool]]:
    """Every top-level `impl ... { ... }` block as `(start, end, gated)`,
    `end` exclusive of the closing brace, found by depth-balanced brace
    matching over the comment/string-stripped text. `gated` is true iff a
    safe gate (`_is_safe_cfg_inner`) sits in the attribute stack immediately
    before the `impl` keyword.

    `head` is the block-opening pattern; the env-literal scan below passes
    `_INLINE_MOD_HEAD` to resolve an inline `mod x { ... }` the same way (the
    `dial.rs` shape: one `#[cfg]` over a whole inline module).
    """
    spans: list[tuple[int, int, bool]] = []
    for m in head.finditer(clean):
        start = m.start()
        depth = 1
        pos = m.end()
        while depth > 0 and pos < len(clean):
            c = clean[pos]
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
            pos += 1
        spans.append((start, pos, _nearest_preceding_gate(clean, text_lines, start)))
    return spans


def _fn_level_gated(clean: str, text_lines: list[str], fn_start: int) -> bool:
    """True iff a safe gate (`_is_safe_cfg_inner`) sits in the attribute
    stack immediately above the fn's own `pub fn` line (skipping doc
    comments, which the comment-stripped text already renders blank)."""
    return _nearest_preceding_gate(clean, text_lines, fn_start)


def _for_test_seam_gates() -> list[tuple[str, int, str, str | None]]:
    """Every `*_for_test` fn under the three roots as `(path, line, name,
    gate)`, where `gate` names what compiles it out — `"fn"` (its own
    attribute stack), `"impl"` (its enclosing impl block's), `"mod"` (an
    enclosing inline `mod x { … }`'s — the `#[cfg(test)] mod tests` shape,
    resolved with the same `_INLINE_MOD_HEAD` spans the env-read scan uses)
    — or `None` when nothing does. Checked in that order, so a seam gated
    twice is counted once, under the nearest gate."""
    seams: list[tuple[str, int, str, str | None]] = []
    for rs_path in _rust_source_files_including_apps():
        text = rs_path.read_text(encoding="utf-8")
        clean = _strip_strings_and_comments(text)
        text_lines = text.splitlines()
        impls = _impl_block_spans(clean, text_lines)
        mods = _impl_block_spans(clean, text_lines, _INLINE_MOD_HEAD)
        for m in _FOR_TEST_FN.finditer(clean):
            pos = m.start()
            if _fn_level_gated(clean, text_lines, pos):
                gate: str | None = "fn"
            elif any(start <= pos < end and gated for start, end, gated in impls):
                gate = "impl"
            elif any(start <= pos < end and gated for start, end, gated in mods):
                gate = "mod"
            else:
                gate = None
            line_no = clean.count("\n", 0, pos) + 1
            seams.append((rs_path.relative_to(_REPO).as_posix(), line_no, m.group("name"), gate))
    return seams


def _ungated_for_test_seams() -> list[str]:
    """Every `*_for_test` fn under `libs/fauna-*/src/`, `bins/*/src/` or
    `apps/*/src/` that is covered by NEITHER a direct fn-level gate NOR its
    enclosing impl block's or inline mod's gate, as `"path:line: name"`
    strings."""
    return [
        f"{path}:{line}: {name}" for path, line, name, gate in _for_test_seam_gates() if gate is None
    ]


def test_no_for_test_seam_ships_ungated():
    """Every `*_for_test` fn in a shared `libs/fauna-*` crate, a `bins/*`
    binary or a Rust app is compiled out of release artifacts, gated on the
    fn itself, its enclosing `impl` block or its enclosing inline `mod`.

    Resolving through the enclosing impl block is the whole point of this
    test (see the module docstring): a fn-level-only version of this
    assertion is not merely incomplete, it is actively wrong on this
    codebase — it would flag 19 correctly-gated seams in
    `fauna-onboarding-machine/src/machine.rs` alone.
    """
    offenders = _ungated_for_test_seams()
    assert not offenders, (
        "found *_for_test seam(s) in a shared crate, bins/* binary or Rust app with no "
        "safe e2e-only gate (a #[cfg(...)] whose clauses are all drawn from "
        f"{sorted(_SAFE_GATE_CLAUSES)!r}) on either the fn or its enclosing "
        "impl block or inline mod, so they compile into every release artifact "
        "(convention 15 rule (a)): "
        + repr(offenders)
    )


def _selected_test_support_modules() -> set[tuple[str, str]]:
    """Every `(pub )?mod *test_support*;` declaration `_TEST_SUPPORT_MOD`
    matches, as `(path, name)` pairs — the module-level scan's SELECTED set,
    independent of gate status (contrast `_ungated_test_support_modules`,
    which filters this same match set down to the ungated ones)."""
    selected: set[tuple[str, str]] = set()
    for rs_path in _rust_source_files_including_apps():
        text = rs_path.read_text(encoding="utf-8")
        clean = _strip_strings_and_comments(text)
        for m in _TEST_SUPPORT_MOD.finditer(clean):
            selected.add((rs_path.relative_to(_REPO).as_posix(), m.group("name")))
    return selected


def _ungated_test_support_modules() -> list[str]:
    """Every `(pub )?mod *test_support*;` declaration under `libs/fauna-*/src/`
    `bins/*/src/` or `apps/*/src/` with no safe gate (`_is_safe_cfg_inner`) in the attribute
    stack immediately above it, as `"path:line: name"` strings — the
    module-level complement to `_ungated_for_test_seams` above (§ module
    docstring, "Module-level complement")."""
    offenders: list[str] = []
    for rs_path in _rust_source_files_including_apps():
        text = rs_path.read_text(encoding="utf-8")
        clean = _strip_strings_and_comments(text)
        text_lines = text.splitlines()
        for m in _TEST_SUPPORT_MOD.finditer(clean):
            if _nearest_preceding_gate(clean, text_lines, m.start()):
                continue
            line_no = clean.count("\n", 0, m.start()) + 1
            offenders.append(f"{rs_path.relative_to(_REPO)}:{line_no}: {m.group('name')}")
    return offenders


def test_no_test_support_module_ships_ungated():
    """Every `*test_support*` module in a shared `libs/fauna-*` crate or a
    `bins/*` binary or a Rust app is compiled out of release artifacts, gated by a safe
    `#[cfg(...)]` immediately above its `(pub )?mod` declaration.

    `bins/fauna-nest/src/test_support.rs` motivated this: an in-process
    real-nest builder with no `_for_test`-suffixed fn for
    `test_no_for_test_seam_ships_ungated` to find (convention 15 rule (a)).
    """
    offenders = _ungated_test_support_modules()
    assert not offenders, (
        "found *test_support* module(s) with no safe e2e-only gate (a "
        "#[cfg(...)] whose clauses are all drawn from "
        f"{sorted(_SAFE_GATE_CLAUSES)!r}) immediately above their `mod` "
        "declaration, so they compile into every release artifact "
        "(convention 15 rule (a)): " + repr(offenders)
    )


def test_the_scan_actually_finds_gated_test_support_modules():
    """Vacuity self-check: the derivation currently finds at least 3 gated
    `*test_support*` modules — `bins/fauna-nest/src/test_support.rs` (`any(test,
    debug_assertions, feature = "test-helpers")`), its sibling
    `bridge_approval_test_support.rs` (bare `test`), and
    `libs/fauna-ipc/src/lib.rs`'s `test_support` (`all(windows, any(test,
    debug_assertions, feature = "test-helpers"))` — the platform-qualified
    shape that motivated widening `_ARCH_QUALIFIER`) — so the absence
    assertion above cannot pass by scanning nothing, and a regression that
    stops resolving any of the three safe-gate shapes fails loudly here
    rather than silently narrowing the offenders list.

    **The `>= 3` floor alone cannot notice one SPECIFIC module leaving the
    selected set — six modules match today, three of slack.** If `_TEST_SUPPORT_MOD`'s naming convention stops
    matching one of the three named above (e.g. a rename to `testing_support`,
    which does not contain the substring `test_support`), the module quietly
    leaves both this scan and `_ungated_test_support_modules`'s with nothing
    red — the count merely drops from 6 to 5, still `>= 3`. So this also pins
    each of the three by `(path, mod name)` in the SELECTED set (regardless of
    gate status — the point is noticing the module vanish from the selector,
    which a gate-status check can't distinguish from "still selected, newly
    ungated", a case `test_no_test_support_module_ships_ungated` already
    covers on its own)."""
    gated = 0
    for rs_path in _rust_source_files_including_apps():
        text = rs_path.read_text(encoding="utf-8")
        clean = _strip_strings_and_comments(text)
        text_lines = text.splitlines()
        for m in _TEST_SUPPORT_MOD.finditer(clean):
            if _nearest_preceding_gate(clean, text_lines, m.start()):
                gated += 1
    assert gated >= 3, (
        f"expected at least 3 gated *test_support* modules (bins/fauna-nest's "
        "test_support + bridge_approval_test_support, and fauna-ipc's "
        f"test_support), found {gated} — either one was removed/renamed or "
        "the module-level scan regressed"
    )

    selected = _selected_test_support_modules()
    expected = {
        ("bins/fauna-nest/src/lib.rs", "test_support"),
        ("bins/fauna-nest/src/lib.rs", "bridge_approval_test_support"),
        ("libs/fauna-ipc/src/lib.rs", "test_support"),
    }
    missing = expected - selected
    assert not missing, (
        "expected these *test_support* module declarations to still be found "
        f"by the selector (regardless of gate status): {sorted(missing)} — "
        "either removed, or renamed so `_TEST_SUPPORT_MOD` no longer matches "
        "(the count floor above has slack and cannot see this on its own)"
    )


def test_the_scan_actually_finds_gated_seams():
    """Self-check against vacuity: the absence assertion above is vacuously
    true over an empty set, so this asserts the derivation actually finds a
    nonzero, and specifically nontrivial, number of gated seams — including
    at least one relying on IMPL-BLOCK-only gating, so a regression in that
    resolution path (the load-bearing detail) fails loudly here rather than
    going quietly green.

    The same holds for the third resolution path, the enclosing inline
    `mod`: `apps/fauna-tui/src/settings/mod.rs`'s `nest_for_test` is a
    `pub(super)` helper inside that file's `#[cfg(test)] mod tests`, with no
    gate of its own, and is pinned BY NAME as mod-gated — it is the seam that
    reported this leg red before the check existed, so a regression in the
    mod path, or the seam moving out from under its module, fails here
    rather than being absorbed by a floor.
    """
    seams = _for_test_seam_gates()
    fn_gated = sum(1 for *_, gate in seams if gate == "fn")
    impl_only_gated = sum(1 for *_, gate in seams if gate == "impl")
    mod_only = {(path, name) for path, _, name, gate in seams if gate == "mod"}
    assert fn_gated > 0, "the scan found no directly fn-gated *_for_test seam anywhere — the regex likely broke"
    assert ("apps/fauna-tui/src/settings/mod.rs", "nest_for_test") in mod_only, (
        "the scan no longer resolves tui's `settings::tests::nest_for_test` through its "
        "enclosing `#[cfg(test)] mod tests` — either the inline-mod path broke or the "
        f"seam moved; mod-gated seams seen: {sorted(mod_only)!r}"
    )
    assert impl_only_gated > 0, (
        "the scan found no *_for_test seam covered ONLY by its enclosing impl "
        "block's gate — either every such seam gained its own fn-level gate "
        "(update this floor) or the impl-block resolution path silently "
        "stopped matching, which is exactly the regression this self-check "
        "exists to catch"
    )




# ---------------------------------------------------------------------------
# Third selector: `FAUNA_E2E_*` environment READS in shared/binary crates.
# ---------------------------------------------------------------------------
#
# The two scans above key on a NAME (`*_for_test`, `*test_support*`). A runtime
# automation SEED keys on none: it is an ordinary `std::env::var("FAUNA_E2E_…")`
# inside an ordinary fn — `fauna-launch-machine`'s wrong-clock offset,
# `fauna-sync-engine`'s debounce/rescan overrides, `fauna-credential-store`'s
# file backend, `fauna-anon-client`'s trust seed, `fauna-ipc`'s pipe override —
# and convention 15 is explicit that the env read is the INNER switch and the
# compile gate the boundary ("a runtime env-var gate alone is not enough: it
# ships scripted-control code inside every release binary, reachable by whoever
# controls the launch environment").
#
# **The READ is what must be gated, not the name.** Measured at filing: six
# `"FAUNA_E2E_*"` literals under the two roots are deliberately UNGATED `pub
# const` names (`fauna_client_sync::agent_spawner::AGENT_BIN_ENV`,
# `fauna_ipc::endpoint::E2E_PIPE_ENV`, `fauna_e2e_agent`'s three actuation
# flags, …) because the spawn side composes the matching argument from the
# same value — each one's doc says so and points at its gated reader
# (`pinned_from_env`, `e2e_pipe_override`). A literal-keyed scan flagged all
# six and was wrong six times; so the selector is every `env::var(…)` /
# `env::var_os(…)` call whose argument is a `"FAUNA_E2E_*"` literal or a const
# bound to one anywhere under the two roots, and the gate is resolved at the
# READ: the enclosing fn / const / static (the nearest preceding item head), an
# enclosing impl block, or an enclosing inline `mod { }`. Derived, not
# maintained — a new seed tomorrow is covered with no list to update. Known
# blind spot, stated: a whole FILE gated at its `mod foo;` declaration in
# `lib.rs` resolves as ungated here (no such seed exists today; add file-level
# resolution when the first one lands, do not ratchet it).

_E2E_ENV_CONST = re.compile(
    r'\bconst\s+(?P<name>\w+)\s*:\s*&(?:\'static\s+)?str\s*=\s*"FAUNA_E2E_\w+"'
)
# No `\s*` before the capture: on `clean` a literal argument is a run of blanks,
# and a leading `\s*` would swallow it whole and leave the group empty.
_ENV_READ = re.compile(r"\benv::var(?:_os)?\s*\((?P<arg>[^)]*)\)")
_ITEM_HEAD = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe)\s+)*(?:fn|const|static)\s+\w+",
    re.MULTILINE,
)
_INLINE_MOD_HEAD = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{", re.MULTILINE
)


def _e2e_env_const_names() -> set[str]:
    """Every const under the two roots bound to a `"FAUNA_E2E_*"` literal, by
    bare name — the identifiers an env read may name instead of the literal."""
    names: set[str] = set()
    for rs_path in _rust_source_files():
        text = rs_path.read_text(encoding="utf-8")
        if "FAUNA_E2E_" in text:
            names.update(m.group("name") for m in _E2E_ENV_CONST.finditer(text))
    return names


def _e2e_env_reads(text: str, clean: str, const_names: set[str]) -> list[tuple[int, str]]:
    """`(position, argument)` of every `env::var(…)`/`env::var_os(…)` read of
    a `FAUNA_E2E_*` seed in this file. The call shape is matched on `clean`
    (so a `//` comment spelling the call is not a read); the argument is read
    off `text` at the same span, because the stripper blanks a literal."""
    hits: list[tuple[int, str]] = []
    for m in _ENV_READ.finditer(clean):
        arg = text[m.start("arg") : m.end("arg")].strip()
        if arg.startswith('"'):
            is_seed = arg.startswith('"FAUNA_E2E_')
        else:
            is_seed = arg.rsplit("::", 1)[-1] in const_names
        if is_seed:
            hits.append((m.start(), arg))
    return hits


def _statement_start(clean: str, pos: int) -> int:
    """Char offset of the first line of the statement containing `pos`: walk
    up while the previous line (in `clean`) is non-blank, is not an attribute,
    and does not end with `;`, `{` or `}` — a line after any of those begins
    a statement. Lets a `#[cfg(...)]` placed on a STATEMENT (the
    `agent_spawner::WindowsDetachedSpawner` shape: a gated `let args = …` whose
    argument list spans lines and reads two seeds, beside an ungated
    production twin) count as the read's gate."""
    lines = clean[:pos].split("\n")
    i = len(lines) - 1
    while i > 0:
        prev = lines[i - 1].strip()
        if not prev or prev.startswith("#[") or prev.endswith((";", "{", "}")):
            break
        i -= 1
    return sum(len(line) + 1 for line in lines[:i])


def _ungated_e2e_env_reads() -> list[str]:
    """Every `FAUNA_E2E_*` env read under `libs/fauna-*/src/`, `bins/*/src/` or
    `apps/*/src/` whose own statement, nearest preceding fn/const/static head,
    enclosing impl block and enclosing inline mod are ALL ungated, as
    `"path:line: env::var(arg)"` (roots: `_rust_source_files_including_apps`)."""
    const_names = _e2e_env_const_names()
    offenders: list[str] = []
    for rs_path in _rust_source_files_including_apps():
        text = rs_path.read_text(encoding="utf-8")
        if "env::var" not in text:
            continue
        clean = _strip_strings_and_comments(text)
        text_lines = text.splitlines()
        impls = _impl_block_spans(clean, text_lines)
        mods = _impl_block_spans(clean, text_lines, _INLINE_MOD_HEAD)
        heads = [m.start() for m in _ITEM_HEAD.finditer(clean)]
        for pos, arg in _e2e_env_reads(text, clean, const_names):
            stmt_gated = _nearest_preceding_gate(clean, text_lines, _statement_start(clean, pos))
            preceding = [h for h in heads if h <= pos]
            item_gated = bool(preceding) and _nearest_preceding_gate(
                clean, text_lines, preceding[-1]
            )
            in_gated_impl = any(s <= pos < e and g for s, e, g in impls)
            in_gated_mod = any(s <= pos < e and g for s, e, g in mods)
            if stmt_gated or item_gated or in_gated_impl or in_gated_mod:
                continue
            line_no = text.count("\n", 0, pos) + 1
            offenders.append(f"{rs_path.relative_to(_REPO)}:{line_no}: env::var({arg})")
    return offenders


def test_no_e2e_env_seed_read_ships_ungated():
    """Every `FAUNA_E2E_*` environment seed a shared crate, `bins/*` binary or
    Rust app (`apps/*/src/`) READS is compiled out of release artifacts — the read sits in a fn, const
    or static gated on a safe clause, or inside a gated impl block or inline
    module. The runtime env read is convention 15's inner switch; this is the
    check that the outer boundary is there. The seed's NAME may be an ungated
    `pub const` (the spawn side composes from it); only the read is gated.
    """
    offenders = _ungated_e2e_env_reads()
    assert not offenders, (
        "found `FAUNA_E2E_*` environment READ(s) in a shared crate, bins/* "
        "binary or Rust app reachable from a release build — no safe e2e-only #[cfg(...)] "
        f"(clauses drawn from {sorted(_SAFE_GATE_CLAUSES)!r}) on the enclosing "
        "fn/const/static, impl block or inline mod. A runtime env gate alone "
        "ships the seam (convention 15): " + repr(offenders)
    )


def test_the_scan_actually_finds_gated_e2e_env_seed_reads():
    """Self-check for the scan above: it must FIND the known, correctly-gated
    reads in both spellings — `fauna-launch-machine`'s clock offset, read
    through its `OFFSET_ENV` const, and `fauna-sync-engine`'s rescan override,
    read as a bare literal — or the read selector / const resolution broke and
    the assertion above passes vacuously."""
    const_names = _e2e_env_const_names()
    assert "OFFSET_ENV" in const_names, f"const resolution broke: {sorted(const_names)!r}"
    seen: set[str] = set()
    for rs_path in _rust_source_files_including_apps():
        text = rs_path.read_text(encoding="utf-8")
        if "env::var" not in text:
            continue
        clean = _strip_strings_and_comments(text)
        seen.update(arg for _, arg in _e2e_env_reads(text, clean, const_names))
    for expected in ("OFFSET_ENV", '"FAUNA_E2E_RESCAN_MS"'):
        assert expected in seen, (
            f"the scan never saw a read of {expected} — the read selector broke; saw {sorted(seen)!r}"
        )
    # And the `apps/` root specifically: the widening that closed the two Rust
    # apps' blind spot is itself only as good as the
    # glob behind it, and an `apps/` leg that silently stopped matching would
    # let the assertion above pass over an empty set — which is exactly the
    # state the fleet was in before the widening, reported as green.
    app_files = [p for p in _rust_source_files_including_apps() if p.is_relative_to(_APPS)]
    assert len(app_files) > 100, (
        f"the `apps/*/src/` glob matched only {len(app_files)} file(s) — the apps "
        "leg of this scan is effectively empty and its green is vacuous"
    )
    assert '"FAUNA_E2E_DOWNLOAD_DIR"' in seen, (
        "the scan never saw the `apps/`-side `FAUNA_E2E_DOWNLOAD_DIR` reads (tui's "
        "`backups::download_dir`, fauna-desktop's two savers) — the apps root stopped "
        f"reaching them, so this scan's apps leg is vacuous; saw {sorted(seen)!r}"
    )

"""The apple xcframework's SLICE SHAPE is a deliberate axis, not a side effect.

Until 2026-08-22 `just apple-ffi` compiled `fauna-ffi` for
`aarch64-apple-watchos` and `aarch64-apple-watchos-sim` into every checkout's
xcframework — and *nothing could link them*: the watchOS app has no build path
at all — no `just` recipe, no target in `Fauna.xcodeproj`, no scheme. Two
release compiles per checkout, repaid on every cache-key bust, for an artifact
with zero consumers.

The split moves them behind `just apple-ffi-watch`. Three properties make that
safe rather than merely cheaper, and each is a real trap this file pins:

  (a) **The shape must reach the cache key and the `.ffi-flavor` marker.** Both
      shapes are the same features, built into the same target dir, staged at
      the same `FaunaFFI.xcframework` path — Package.swift's binaryTarget is
      fixed. A shape-blind key hands a 3-slice artifact back to an
      `apple-ffi-watch` as a cache HIT, which is `apple-ffi-host`'s known
      clobber gotcha (host flavor silently dropping the iOS slices) one axis
      over. The `just --show` hash in the key does NOT cover it: the shape is a
      recipe PARAMETER, so the recipe text is byte-identical for both shapes.

  (b) **One home for the slice list.** The triples feed three consumers — the
      cargo lines, build-if-stale's `--source` list, and `_apple-ffi-bindgen`'s
      slice arguments. A hand-kept second copy drifts from the shape actually
      built, and either direction is silent: a `--source` naming an unbuilt
      slice rebuilds forever, a `--source` missing a built one lets a stale
      binding through.

  (c) **Coverage may not be dropped along with the compile.** The old
      `merge-gate-check.md` residual accepted ungated watchOS slices *on the
      stated ground* that "a watchOS-only dep break is caught by the next full
      `apple-ffi`" — the 2026-08-12 `secret-service`→`zbus` break was found
      exactly that way, two days late. Making the slices opt-in deletes that
      net, so the mac merge gate must carry the watch triple itself.

tier_1: reads the justfile, builds nothing.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

_WATCH_TRIPLES = ("aarch64-apple-watchos", "aarch64-apple-watchos-sim")


def _recipe_body(name: str) -> str:
    """The indented body of justfile recipe `name`, joined into one string.

    A body whose LAST non-blank line is a bare delegation call — `[{{slot_build}}]
    just _<name>`, nothing else on that line — resolves through it: the slot-lock
    lease split moved the content these pins look for into
    `_apple-ffi-build-impl`'s own body, so reading only `_apple-ffi-flavor`'s
    literal cache-lookup text would see none of it. Any preceding lines are kept;
    the delegate's body is appended after them. Ported from
    `test_payments_excision_spine.py::_recipe_body` — small helpers are still duplicated per this suite's
    no-cross-import-between-test-files convention.
    """
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(
        rf"^{re.escape(name)}(?:\s+[^:]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.MULTILINE
    )
    assert m, f"no recipe named {name!r} in the justfile"
    body = m.group(1)
    lines = [line for line in body.splitlines() if line.strip()]
    if lines:
        delegate = re.match(r"^\s*(?:\{\{slot_build\}\}|\{\{slot_e2e\}\})?\s*just\s+(_\S+)\s*$", lines[-1])
        if delegate:
            prefix = "\n".join(lines[:-1])
            return (prefix + "\n" if prefix else "") + _recipe_body(delegate.group(1))
    return body


def _code_lines(body: str) -> list[str]:
    """Body lines that are shell code, not comments."""
    return [ln for ln in body.splitlines() if ln.strip() and not ln.strip().startswith("#")]


def test_the_default_apple_ffi_does_not_compile_the_watch_slices():
    """`just apple-ffi` — what every iOS e2e run and every checkout's inner loop
    reaches for — must not compile a triple nothing can link."""
    body = _recipe_body("apple-ffi")
    for line in _code_lines(body):
        for triple in _WATCH_TRIPLES:
            assert triple not in line, (
                f"`apple-ffi` names {triple} directly ({line.strip()!r}); the watch "
                "slices are opt-in via `apple-ffi-watch`"
            )
    assert '_apple-ffi-flavor "" ""' in body, (
        "`apple-ffi` must delegate with an EMPTY watch argument — that empty "
        "second argument IS the 3-slice default"
    )


def test_the_watch_slices_have_exactly_one_opt_in_door():
    """`apple-ffi-watch` is the deliberate 5-slice invocation."""
    body = _recipe_body("apple-ffi-watch")
    assert re.search(r'_apple-ffi-flavor\s+""\s+watch', body), (
        "`apple-ffi-watch` must delegate to the shared impl with the watch "
        f"argument set; got: {body.strip()!r}"
    )


def test_only_the_watch_guarded_branch_compiles_a_watch_triple():
    """A cargo BUILD of a watch triple may appear only inside the `$WATCH` branch.

    A `cargo check` elsewhere is fine and expected — that is the merge gate's
    compensating coverage below. What must not exist is a second unconditional
    place that pays the release compile again.
    """
    body = _recipe_body("_apple-ffi-flavor")
    lines = body.splitlines()
    for i, line in enumerate(lines):
        if not any(t in line for t in _WATCH_TRIPLES):
            continue
        if "cargo build" not in line and "cargo rustc" not in line:
            continue
        prelude = "\n".join(lines[max(0, i - 6) : i])
        assert 'if [ -n "$WATCH" ]' in prelude, (
            "a watch-triple cargo line sits outside the `$WATCH` guard — the "
            f"default shape would pay for it: {line.strip()!r}"
        )


def test_the_slice_shape_is_part_of_the_prebuilt_cache_key():
    """Trap (a): a shape-blind key serves a 3-slice artifact to a watch request."""
    body = _recipe_body("_apple-ffi-flavor")
    assert '"shape=$SHAPE"' in body, (
        "the prebuilt-FFI CACHE_KEY must carry the slice shape. The `just --show` "
        "hash cannot stand in for it: the shape is a recipe parameter, so the "
        "recipe text is identical for a 3-slice and a 5-slice build"
    )


def test_the_staged_artifacts_flavor_marker_records_the_slice_shape():
    """Trap (a), second half: the marker is what guards the shared staging slot."""
    body = _recipe_body("_apple-ffi-flavor")
    assert re.search(r'FLAVOR="full-\$SHAPE"', body), (
        "`.ffi-flavor` must record the slice shape in its FLAVOR half "
        '(full-3slice | full-5slice), not a shape-blind "full"'
    )
    assert 'echo "full:$FEATURES" > apps/fauna-apple/.ffi-flavor' not in body, (
        "a shape-blind `full:$FEATURES` marker write survives — it would read "
        "'fresh' across a 3-slice↔5-slice switch"
    )
    assert '"$FLAVOR:$FEATURES"' in body, (
        "the staleness guard must compare against the shape-carrying marker"
    )


def test_the_slice_list_has_a_single_home():
    """Trap (b): the cargo lines, build-if-stale and the bindgen call all derive
    their triples from SLICE_TRIPLES rather than re-listing them."""
    body = _recipe_body("_apple-ffi-flavor")
    assert "SLICE_TRIPLES=(" in body, "the slice set must be declared once, as SLICE_TRIPLES"
    assert 'for triple in "${SLICE_TRIPLES[@]}"' in body, (
        "build-if-stale's --source list and the bindgen slice arguments must be "
        "DERIVED from SLICE_TRIPLES"
    )
    # No hand-listed per-slice `--source` lines may survive alongside the loop.
    hand_listed = [
        ln for ln in _code_lines(body) if "--source target/aarch64-apple-" in ln
    ]
    assert not hand_listed, (
        "a hand-listed --source slice line survives beside the derived list — "
        f"that is the second copy this pin exists to prevent: {hand_listed}"
    )


def test_the_mac_merge_gate_still_compiles_the_watch_triple():
    """Trap (c): the opt-in split may not quietly delete watchOS coverage.

    The old accepted residual leaned on "the next full `apple-ffi`" as its net.
    Nothing builds the watch slices any more, so the gate carries the triple.
    """
    body = _recipe_body("apple-swift-build-check")
    watch_checks = [
        ln
        for ln in _code_lines(body)
        if "cargo check" in ln and "aarch64-apple-watchos" in ln
    ]
    assert watch_checks, (
        "`apple-swift-build-check` (the mac merge-gate check's one gate) must "
        "carry a watchOS `cargo check` — without it, making the watch slices "
        "opt-in removes the only thing that ever compiled them"
    )
    assert all("--no-default-features" in ln for ln in watch_checks), (
        "the watchOS check must use `--no-default-features`, the feature set "
        "`_apple-ffi-flavor` actually builds those slices with — checking a "
        f"different graph gates nothing: {watch_checks}"
    )

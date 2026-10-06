"""tier_1: how the mail-bridge Go bindings are obtained, pinned on the justfile.

`libs/fauna-mail-go` is the one UniFFI binding this project COMMITS (the bridge
consumes it as an ordinary Go module — `docs/goal/architecture/build-system.md`
§ UniFFI app bindings). Two consequences pull against each other, and this file
pins the shape that satisfies both.

**A dev machine regenerates.** The bindgen tail is keyed on the cargo-built
`.so`, which is what lets a rebase or a doc-comment edit under
`libs/fauna-ffi/src/` — the UniFFI API checksum covers docstrings, so such an
edit really does re-stale the binding — heal on the next build instead of
panicking at run time with a checksum mismatch. That self-heal is ratified, and
`regenerate` is therefore the DEFAULT.

**A CI runner cannot.** `uniffi-bindgen-go` compiles askama's compile-time
templates in a single rustc process, and a GitHub-hosted runner is shut down for
memory before it finishes — measured three times on 2026-08-28, at 11, 27 and 9
minutes in, in both the release and the debug profile. But a machine that cannot
build the generator can still consume its committed output, which is the whole
point of the binding being tracked. `FAUNA_GO_BINDINGS=tracked` is that mode.

The freshness of the tracked tree is not lost in `tracked` mode: it is gated
where the generator does build, by `mail-bridge-ffi-check` on a development
machine's merge gate, before the commit CI is testing ever landed.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_JUSTFILE = Path(__file__).resolve().parents[3] / "justfile"
_BINDINGS_DIR = Path(__file__).resolve().parents[3] / "libs" / "fauna-mail-go"


def _justfile() -> str:
    if not _JUSTFILE.is_file():
        pytest.skip(f"justfile not found at {_JUSTFILE}")
    return _JUSTFILE.read_text(encoding="utf-8")


def _recipe_body(text: str, name: str) -> str:
    """One recipe's body — from its header line to the next unindented line."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if re.match(rf"^{re.escape(name)}(:| )", line):
            body = []
            for follow in lines[i + 1:]:
                if follow and not follow[0].isspace():
                    break
                body.append(follow)
            return "\n".join(body)
    return ""


def test_regenerate_is_the_default_mode():
    """The dev loop's self-heal is the default; `tracked` is opted into."""
    text = _justfile()
    assert 'go_bindings := env_var_or_default("FAUNA_GO_BINDINGS", "regenerate")' in text, (
        "FAUNA_GO_BINDINGS must default to `regenerate` — a CI-shaped default "
        "would silently retire the .so-keyed self-heal on every dev machine"
    )


@pytest.mark.parametrize("recipe", ["mail-bridge-ffi", "mail-bridge-ffi-check"])
def test_both_bindgen_recipes_honour_the_mode(recipe):
    """Either recipe reaching the generator unconditionally re-breaks CI."""
    body = _recipe_body(_justfile(), recipe)
    assert body, f"{recipe} recipe not found"
    assert "{{go_bindings}}" in body, f"{recipe} must honour FAUNA_GO_BINDINGS"


@pytest.mark.parametrize("recipe", ["mail-bridge-ffi", "mail-bridge-ffi-check"])
def test_an_unrecognised_mode_is_a_loud_failure(recipe):
    """A typo'd value must not silently mean whichever branch the `if` falls to
    — the failure would be a stale binding shipped under a green check."""
    body = _recipe_body(_justfile(), recipe)
    assert "FAUNA_GO_BINDINGS must be 'regenerate' or 'tracked'" in body, (
        f"{recipe} must reject an unrecognised FAUNA_GO_BINDINGS value"
    )


def test_the_bindgen_pin_is_spelled_exactly_once():
    """It was spelled four times on 2026-08-29 — two justfile recipes in two
    different profiles, plus a workflow install and the cache key that warmed
    it. A pin with copies is a pin that drifts; `_uniffi-bindgen-go` owns it."""
    text = _justfile()
    installs = re.findall(r"cargo install uniffi-bindgen-go", text)
    assert len(installs) == 1, (
        f"the bindgen install must appear once, in `_uniffi-bindgen-go`; "
        f"found {len(installs)}"
    )
    assert _recipe_body(text, "_uniffi-bindgen-go"), "_uniffi-bindgen-go recipe not found"
    tags = re.findall(r"--tag v0\.7\.1\+v0\.31\.0", text)
    assert len(tags) == 1, f"the pinned tag must appear once; found {len(tags)}"


def test_tracked_mode_refuses_an_absent_binding():
    """`tracked` consumes a committed tree, so an absent one is a hard failure
    here — not an undefined-reference wall out of cgo three steps later."""
    body = _recipe_body(_justfile(), "mail-bridge-ffi")
    assert "libs/fauna-mail-go/fauna_mail/fauna_mail.go" in body, (
        "mail-bridge-ffi must assert the tracked binding exists before "
        "consuming it"
    )


def test_the_binding_tracked_mode_consumes_is_committed():
    """The mode's premise. A gitignored binding would make every `tracked` run a
    fresh-checkout failure."""
    assert (_BINDINGS_DIR / "fauna_mail" / "fauna_mail.go").is_file()
    assert (_BINDINGS_DIR / "go.mod").is_file()


def test_the_check_keeps_its_compile_half_in_tracked_mode():
    """Only the freshness DIFF needs the generator. Dropping the compile too
    would leave CI proving nothing about a binding it is about to link."""
    body = _recipe_body(_justfile(), "mail-bridge-ffi-check")
    compile_pos = body.index("go -C libs/fauna-mail-go build ./...")
    guard = re.search(r'if \[ "\{\{go_bindings\}\}" = tracked \]; then', body)
    assert guard, "mail-bridge-ffi-check must branch on the mode"
    assert guard.start() < compile_pos, (
        "the compile step must sit AFTER the mode branch, so `tracked` still "
        "reaches it"
    )
    assert "SKIPPED (FAUNA_GO_BINDINGS=tracked" in body, (
        "a narrowed check must say so in its own output — a green that means "
        "less than it did must never look identical"
    )

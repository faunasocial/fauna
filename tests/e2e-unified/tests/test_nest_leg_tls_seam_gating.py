"""Convention 15, the nest-leg trust seed: `FAUNA_E2E_NEST_EXTRA_CA_PEM` must be
compiled out of anything that ships, and may only ADD a trust anchor.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The nest-leg trust
seed. The seed sits on `fauna_ws_substrate::tls_verify::webpki_verifier` — the
ONE verifier every app nest leg classifies a served certificate through (the
pre-identity WS dial, the bearer dial, the reqwest byte plane) and the one
check standing between a bearer and an impostor nest. That reach is exactly why
the ruling admitted it only on five terms, and why each is pinned here rather
than trusted to review. Like the source-IMAP seed it crosses no UniFFI or wasm
boundary, so it leaves no trace in a generated binding and the FFI witnesses
are blind to it.

**The halves, each a false green alone:**

  1. *The gate exists, on both items* — the constant and its reader.
  2. *The environment is read only inside the gated reader* — `webpki_verifier`
     itself is unconditional, so an inlined read would ship.
  3. *The production twin exists* — the statement that a release build has no
     seed, and what keeps `webpki_verifier` compiling there.
  4. *The seed only ADDS an anchor and changes no policy* — the reader extends
     a `RootCertStore` and names no `NoPinPolicy`, no `ServerCertVerified`, no
     `dangerous()`; and every client config is still built on the one door.
  5. *The Rust and harness spellings agree* — nothing else connects the reader
     to the journey that sets it, so a rename on either side silently stops
     seeding and the journey walks the self-signed branch instead.

The shipping-dep-line half (no `[dependencies]` line turning
`fauna-ws-substrate/test-helpers` on) is the general, derived scan in
`test_test_helpers_dep_line_gating.py`, as the ACME directory seed's witness
does. The artifact measurement (the name in a debug rlib, absent from a release
one) is recorded in the goal doc's § Implementation status today. Pure text
analysis — no build, no driver.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_TLS_VERIFY = _REPO / "libs" / "fauna-ws-substrate" / "src" / "tls_verify.rs"
_SEED_TEST = _REPO / "libs" / "fauna-ws-substrate" / "tests" / "e2e_nest_trust_seed.rs"
_JOURNEY = _REPO / "tests" / "e2e-unified" / "tests" / "test_nest_identity_pin_webpki.py"

_SEAM_GATE = '#[cfg(any(test, debug_assertions, feature = "test-helpers"))]'
_TWIN_GATE = '#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]'
_SEED_ENV = "FAUNA_E2E_NEST_EXTRA_CA_PEM"


def _text() -> str:
    return _TLS_VERIFY.read_text(encoding="utf-8")


def _fn_body(text: str, start: int) -> tuple[int, int]:
    """(open, close) offsets of the brace-delimited body of the item starting at
    `start` — the first `{` after it, matched by depth."""
    open_ = text.index("{", start)
    depth = 0
    for i in range(open_, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return open_, i
    raise AssertionError("unbalanced braces in tls_verify.rs")


def _gated_reader(text: str) -> re.Match:
    m = re.search(re.escape(_SEAM_GATE) + r"\s*\n\s*fn e2e_extra_nest_roots\b", text)
    assert m, (
        "libs/fauna-ws-substrate/src/tls_verify.rs has no `e2e_extra_nest_roots` "
        f"directly under {_SEAM_GATE}. That reader is the live switch on the "
        "nest-leg verifier; ungated it is a trust-anchor override in every "
        "release artifact (convention 15)."
    )
    return m


def test_the_seed_constant_and_its_reader_are_both_gated():
    """Derived, not listed — and the derivation must find both, so the file
    cannot pass by scanning nothing."""
    gated = re.findall(
        re.escape(_SEAM_GATE) + r"\s*\n\s*(?:pub\s+)?(?:const|fn)\s+(\w+)",
        _text(),
    )
    for item in ("E2E_NEST_EXTRA_CA_ENV", "e2e_extra_nest_roots"):
        assert item in gated, (
            f"`{item}` is no longer directly under {_SEAM_GATE} in "
            "libs/fauna-ws-substrate/src/tls_verify.rs — the nest-leg trust "
            "seed would ship in every release artifact (convention 15)."
        )


def test_the_environment_is_read_only_inside_the_gated_reader():
    """Every `env::var` in `tls_verify.rs` sits inside the gated reader's body.

    `webpki_verifier` is unconditional; a read inlined into it "to save a
    function" leaves the gate on a helper nothing calls.
    """
    text = _text()
    open_, close = _fn_body(text, _gated_reader(text).end())
    reads = [m.start() for m in re.finditer(r"env::var", text)]
    assert reads, "the gated reader no longer reads the environment at all"
    for at in reads:
        assert open_ < at < close, (
            "libs/fauna-ws-substrate/src/tls_verify.rs reads the process "
            "environment outside the gated `e2e_extra_nest_roots` body — that "
            "read compiles into every release artifact."
        )


def test_the_reader_has_a_same_signature_production_twin():
    text = _text()
    gated = re.search(
        re.escape(_SEAM_GATE) + r"\s*\n\s*(fn e2e_extra_nest_roots\([^)]*\))", text
    )
    twin = re.search(
        re.escape(_TWIN_GATE) + r"\s*\n\s*(fn e2e_extra_nest_roots\([^)]*\))", text
    )
    assert gated and twin, (
        "the production no-op twin of `e2e_extra_nest_roots` is gone — the twin "
        "IS the statement that a shipped build has no nest-leg seed."
    )
    sig = lambda s: re.sub(r"\b_?(\w+):", r"\1:", s)  # noqa: E731 — `_roots` vs `roots`
    assert sig(gated.group(1)) == sig(twin.group(1)), (
        f"the twin's signature {twin.group(1)!r} drifted from the gated "
        f"reader's {gated.group(1)!r}; a release-only compile error no debug "
        "build or e2e run can see is one edit away."
    )
    _, close = _fn_body(text, twin.end())
    assert "env::var" not in text[twin.end() : close + 1] and _SEED_ENV not in text[
        twin.end() : close + 1
    ], "the production twin reads or names the seed"


def test_the_seed_only_adds_an_anchor_and_changes_no_policy():
    """Term 2: the seam changes which roots are trusted, never what trust
    permits. The reader touches a `RootCertStore` and nothing policy-shaped,
    the one door feeds it, and every client config is built on that door."""
    text = _text()
    open_, close = _fn_body(text, _gated_reader(text).end())
    body = text[open_:close]
    assert "roots.add(" in body, "the reader no longer ADDS to the root store"
    for forbidden in ("NoPinPolicy", "ServerCertVerified", "dangerous", "resolve_pin", "webpki_valid"):
        assert forbidden not in body, (
            f"the nest-leg seed's reader names `{forbidden}`: the ruling admits a "
            "seam that adds an anchor to the root set and changes no policy "
            "(e2e-automation-surface-gating.md § The nest-leg trust seed, term 2)."
        )
    door = re.search(r"fn webpki_verifier\(\)", text)
    assert door
    d_open, d_close = _fn_body(text, door.end())
    door_body = text[d_open:d_close]
    assert "e2e_extra_nest_roots(&mut roots)" in door_body and "Arc::new(roots)" in door_body, (
        "`webpki_verifier` no longer builds its verifier from the seeded root "
        "set — the one door every nest leg classifies through."
    )
    assert text.count("webpki_root_store()") == 1, (
        "tls_verify.rs builds a root store somewhere other than the one door; a "
        "second store is a second, unseeded trust policy on the nest legs."
    )
    assert "inner: webpki_verifier()," in text, (
        "`config_with` no longer builds its capturing verifier on `webpki_verifier()`"
    )


def test_the_rust_and_harness_spellings_of_the_seed_agree():
    for path in (_TLS_VERIFY, _SEED_TEST, _JOURNEY):
        assert f'"{_SEED_ENV}"' in path.read_text(encoding="utf-8"), (
            f"{path.relative_to(_REPO)} no longer names {_SEED_ENV}. The reader, "
            "its behaviour test and the journey that sets it are connected by "
            "this string alone; a rename on one side leaves the seed unset and "
            "the journey walking the self-signed branch."
        )

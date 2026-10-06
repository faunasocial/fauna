"""Convention 15, the drafts autosave-window half: the e2e window seam must be
compiled out of anything that ships — and it must actually REACH every app.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention +
§ The drafts autosave-window seam. The source-IMAP trust seed's twin of this
file is `test_mail_import_tls_seam_gating.py`; the native-FFI half is
`test_ffi_flavor_split.py`. This file pins a surface neither can see.

**Why this surface needs its own witness.** `FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS`
is read inside `fauna_client_drafts::autosave_debounce` — the ONE accessor every
shell's autosave timer reads, directly on linux and tui and through
`fauna-ffi`'s `autosave_debounce_ms` / `fauna-wasm`'s `autosaveDebounceMs`
elsewhere. That reach is the point of putting it there, and it is also why the
gate has to be one nobody can quietly drop.

**Five independent halves, because each alone false-greens.**

  1. *The gate exists, on both items.* An arm added without a `cfg`, or with one
     that is always true, ships. Read from the source.
  2. *The gate's feature is off in a shipped build.* `debug_assertions` is off
     under `--release` by construction, but `feature = "test-helpers"` is not: a
     `[dependencies]` line naming it turns it on unconditionally, surviving even
     `--no-default-features` (`test_ffi_flavor_split.py`'s rule (b)).
  3. *The env is read only from inside the gated reader.* Inlining the
     `env::var` call up into `autosave_debounce`'s body "to save a function"
     drops it out from under the gate, because the accessor itself is
     unconditional by design. The gate would still be there, on a helper nothing
     calls.
  4. *The Rust and Python spellings of the variable agree.* The Rust side reads
     an env var; the harness writes one. Nothing else connects the two, so a
     rename on either side degrades silently into "the window is simply never
     re-timed" — and THAT failure is the dangerous one here, because the ios
     leave-flush witness would go back to racing the ~1.5 s debounce while still
     reporting that it cannot.
  5. *Every app timer reads the ACCESSOR, not the constant.* This is the half
     the other seams do not need, and it is convention 11's third lesson one
     layer down: a seam that does not reach an app makes that app's witness
     measure nothing while reporting clean. A timer that goes back to
     `AUTOSAVE_DEBOUNCE` is invisible in every build, every test and every
     `strings` sweep — it just silently opts its app out.

Pure text analysis of Rust sources and Cargo manifests — no build, no driver.
"""

import re
from pathlib import Path

import pytest

from helpers.manifest_seams import shipping_feature_offenders

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SYNC = _REPO / "libs" / "fauna-client-drafts" / "src" / "sync.rs"

# The gate every e2e-only seam in this family wears. `debug_assertions` covers
# the dev/e2e builds, `test` the crate's own suites, and the feature is the
# explicit opt-in for a release-profile e2e build — which this seam genuinely
# needs, since `apple-ffi-test`'s iOS slice is `--release`.
_SEAM_GATE = '#[cfg(any(test, debug_assertions, feature = "test-helpers"))]'
_TWIN_GATE = '#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]'

# The env var's name, as the Python harness spells it. Half 4 pins the Rust side
# to this string; `conftest.py::_apply_drafts_autosave_window_env` is the writer.
_SEED_ENV = "FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS"

# Crates whose `test-helpers` feature reaches this seam: `fauna-client-drafts`
# declares it, `fauna-ffi` forwards it for apple/android/windows. Turning either
# on from a shipping dep line puts the seam back into a release artifact.
_SEAM_FEATURE_CRATES = {"fauna-client-drafts", "fauna-ffi"}

#: Every production autosave timer in the fleet's own source, and the door each
#: reads. The two Rust-native shells call the accessor directly; the rest cross
#: a generated boundary, so their door is the face rather than the accessor.
#: Derived checks below, not a transcription of behaviour.
_NATIVE_TIMER_SITES = (
    Path("apps/fauna-tui/src/drafts_autosave.rs"),
    Path("apps/fauna-tui/src/events/drafts.rs"),
    Path("apps/fauna-linux/src/drafts_autosave.rs"),
    Path("apps/fauna-linux/src/views/events/drafts.rs"),
)

#: The two door-crossing faces every other app reads the window through.
_FACE_SITES = (
    Path("libs/fauna-ffi/src/drafts.rs"),
    Path("libs/fauna-wasm/src/lib.rs"),
)


def _gated_items() -> list[str]:
    """Every item in `sync.rs` that sits directly under the seam gate.

    Derived rather than listed, so a second seam-adjacent item added tomorrow is
    covered with no list for anyone to update.
    """
    text = _SYNC.read_text(encoding="utf-8")
    return re.findall(
        re.escape(_SEAM_GATE) + r"\s*\n\s*(?:pub\s+)?(?:const|fn)\s+(\w+)",
        text,
    )


def test_the_window_seed_and_its_reader_are_both_gated():
    """Both halves of the seam carry the gate — and the derivation actually
    finds them, so this file cannot pass by scanning nothing.

    The self-check matters more than it looks: the assertions are about an
    absence in release builds, and an absence over an empty set is vacuously
    true. If `sync.rs` is restructured so the regex stops matching, this fails
    loudly here rather than going quietly green while the seam ships.
    """
    gated = _gated_items()
    for item in ("E2E_AUTOSAVE_DEBOUNCE_ENV", "e2e_autosave_debounce_override"):
        assert item in gated, (
            f"`{item}` is no longer directly under {_SEAM_GATE} in "
            "libs/fauna-client-drafts/src/sync.rs. It is the e2e autosave-window "
            "seam on the one accessor every app's timer reads, so an ungated one "
            "is a behaviour override shipped in every release artifact "
            "(convention 15). If it was renamed, rename it here; if it was "
            "deleted, delete this assertion with it."
        )


def test_the_production_twin_exists():
    """The `cfg(not(...))` no-op twin is present, so the shipped path is
    explicit rather than implied.

    Convention 15's "same-signature no-op twin wherever the caller is plumbing
    the app compiles unconditionally" — `autosave_debounce` is exactly that
    plumbing, called by every timer in the fleet.
    """
    text = _SYNC.read_text(encoding="utf-8")
    assert re.search(
        re.escape(_TWIN_GATE) + r"\s*\n\s*fn e2e_autosave_debounce_override\b", text
    ), (
        "libs/fauna-client-drafts/src/sync.rs has no `e2e_autosave_debounce_override` "
        f"under {_TWIN_GATE}. Without the twin the accessor does not compile in a "
        "shipped build at all, and the fix someone reaches for under that pressure "
        "is to drop the gate."
    )


def test_the_env_is_read_only_from_inside_the_gated_reader():
    """The env var is named under the gate and nowhere else in the crate.

    Anchors on the gate + definition together, not on the name alone: the module
    carries a same-signature twin, so a bare search would resolve to whichever of
    the two is written first — making the test pass or fail on declaration order.
    """
    text = _SYNC.read_text(encoding="utf-8")
    gated = re.search(
        re.escape(_SEAM_GATE) + r"\s*\n\s*fn e2e_autosave_debounce_override\b", text
    )
    assert gated, (
        "libs/fauna-client-drafts/src/sync.rs has no `e2e_autosave_debounce_override` "
        f"directly under {_SEAM_GATE}. The reader is the live switch."
    )

    # Every `env::var` in the crate must sit after the gated reader's start and
    # inside it — i.e. the crate has exactly one, and it is this one.
    crate_src = _REPO / "libs" / "fauna-client-drafts" / "src"
    readers = [
        p
        for p in crate_src.rglob("*.rs")
        if "env::var" in p.read_text(encoding="utf-8")
    ]
    assert readers == [_SYNC], (
        "libs/fauna-client-drafts reads the process environment outside "
        f"sync.rs's gated seam: {[str(p.relative_to(_REPO)) for p in readers]}. "
        "Every env read in this crate belongs under the seam gate."
    )
    # CODE lines only — the module's own prose explains the seam and names
    # `std::env::var` while doing so, and a witness that counted those would go
    # red on an edit to a comment.
    calls = [
        ln.strip()
        for ln in text.splitlines()
        if "env::var" in ln and not ln.lstrip().startswith(("///", "//!", "//"))
    ]
    assert len(calls) == 1, (
        f"sync.rs has {len(calls)} `env::var` calls in code, not 1: {calls}. The "
        "seam is one gated read; a second one is either an ungated duplicate or a "
        "new seam that needs its own gate and its own line in this witness."
    )


def test_no_shipping_dep_line_turns_the_seam_feature_on():
    """Rule (b): a `[dependencies]` line naming `test-helpers` on either crate
    in the forward chain turns the seam on unconditionally, surviving even
    `--no-default-features`.

    That is exactly how twelve seams reached five release artifacts through
    `fauna-ffi` until 2026-08-01. The forward must live in a `[features]` entry
    (`fauna-ffi`'s own `test-helpers`), never on a dep line.
    """
    offenders = shipping_feature_offenders(
        "test-helpers", repo=_REPO, crates=_SEAM_FEATURE_CRATES
    )
    assert not offenders, (
        "a shipping dependency line turns `test-helpers` on for a crate in the "
        "drafts-window seam's forward chain, which puts the seam into release "
        "artifacts:\n  " + "\n  ".join(offenders)
    )


def test_the_rust_and_python_spellings_agree():
    """The Rust reader and the harness writer name the same variable.

    A rename on either side degrades silently into "the window is never
    re-timed", which is the failure mode this whole seam exists to remove: the
    ios leave-flush witness would go back to racing the debounce while its
    docstring still says it cannot.
    """
    assert _SEED_ENV in _SYNC.read_text(encoding="utf-8"), (
        f"libs/fauna-client-drafts/src/sync.rs no longer names {_SEED_ENV}."
    )
    conftest = (_REPO / "tests" / "e2e-unified" / "conftest.py").read_text(
        encoding="utf-8"
    )
    assert _SEED_ENV in conftest, (
        f"tests/e2e-unified/conftest.py no longer names {_SEED_ENV}, so "
        "`drafts_autosave_window_ms` writes a variable nothing reads."
    )


@pytest.mark.parametrize("rel", _NATIVE_TIMER_SITES, ids=lambda p: p.as_posix())
def test_every_native_timer_reads_the_accessor_not_the_constant(rel):
    """linux's and tui's timers call `autosave_debounce()`, not the constant.

    A timer that reverts to `AUTOSAVE_DEBOUNCE` opts its app out of the seam
    silently — no build breaks, no test fails, no `strings` sweep sees it.
    Convention 11's third lesson one layer down: a flag that does not reach the
    app makes a sweep measure nothing while reporting clean.

    The constant stays legal in doc links and in tests that advance virtual time
    against the same window; what must not appear is a timer ARMED with it.
    """
    text = (_REPO / rel).read_text(encoding="utf-8")
    assert "autosave_debounce()" in text, (
        f"{rel} no longer calls `autosave_debounce()`. Its autosave timer has "
        "left the shared door, so the harness's window seam no longer reaches "
        "this app and any leave-flush witness of its own silently races the "
        "debounce again."
    )
    armed_with_const = re.findall(
        r"(?:sleep|timeout_add_local_once|arm_generation_debounce)\s*\([^)]*"
        r"\bAUTOSAVE_DEBOUNCE\b",
        text,
    )
    assert not armed_with_const, (
        f"{rel} arms a timer with the raw `AUTOSAVE_DEBOUNCE` constant "
        f"({armed_with_const}). Arm it with `autosave_debounce()` so the seam "
        "reaches this app; the constant is the production answer that accessor "
        "returns, not the door."
    )


@pytest.mark.parametrize("rel", _FACE_SITES, ids=lambda p: p.as_posix())
def test_every_door_crossing_face_answers_from_the_accessor(rel):
    """`fauna-ffi` and `fauna-wasm` answer from `autosave_debounce()`.

    These two faces are how android, apple, web and windows read the window, so
    a face that reverts to the constant opts FOUR apps out at once — and web is
    already a declared absence for a reason the source cannot show (wasm32 has
    no process environment), so a silent regression here would be indantly
    indistinguishable from that one.
    """
    text = (_REPO / rel).read_text(encoding="utf-8")
    assert "fauna_client_drafts::autosave_debounce()" in text, (
        f"{rel} no longer answers from `fauna_client_drafts::autosave_debounce()`. "
        "It is the door android/apple/windows (fauna-ffi) or web (fauna-wasm) read "
        "the autosave window through, so reverting it to the constant opts those "
        "apps out of the seam with nothing to see in any build."
    )

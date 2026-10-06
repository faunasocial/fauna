"""Convention 15, the sync agent's IPC half: the e2e control seams on the
app↔agent pipe must be compiled out of anything that ships.

`docs/goal/architecture/testing.md` § Cross-app e2e conventions point 15. The
native-FFI half is pinned by `test_ffi_flavor_split.py` and the apple call-site
half by `test_apple_seam_gating.py`; this file pins the third surface, which
neither of them can see — `fauna_ipc::sync::RequestMethod`, the verb set the app
sends the per-user `fauna-sync-agent` over its private socket.

**Why this surface needs its own witness.** The FFI witnesses work by diffing
*generated bindings*: a seam that never crosses a UniFFI or wasm boundary leaves
no trace in any generated file, so it is invisible to both. `RequestMethod` is
exactly that shape — a plain Rust enum, serialized by serde on a local socket —
and it carries the strongest test-only verbs in the tree (today
`CustodianRunPassNow`, which drives a real custodian pull pass and its check-in
to the nest on demand). Nothing else asserts they are gated.

**Two independent halves, because either alone false-greens.**

  1. *The gate exists.* A variant added without a `cfg` gate, or with one that is
     always true, ships. Asserted by reading the source.
  2. *The gate's feature is off in a shipped build.* `debug_assertions` is off in
     `--release` by construction, but `feature = "test-helpers"` is not: a
     `[dependencies]` line naming it turns it on unconditionally, surviving even
     `--no-default-features`. That is not hypothetical — it is exactly how twelve
     seams reached five release artifacts through `fauna-ffi` until 2026-08-01
     (`test_ffi_flavor_split.py`'s rule (b)). Asserted by scanning every
     workspace manifest.

**The seam set is DERIVED, not maintained**: half 1 reads whichever
`RequestMethod` variants carry the crate's e2e gate, so a verb added tomorrow is
covered with no list for anyone to update, and half 2 scans all manifests rather
than a named few.

Pure text analysis of Rust sources and Cargo manifests — no build, no driver.
"""

import re
from pathlib import Path

import pytest

from helpers.manifest_seams import shipping_feature_offenders

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_IPC_SYNC = _REPO / "libs" / "fauna-ipc" / "src" / "sync.rs"

# The gate every e2e-only seam in this family wears. `debug_assertions` covers
# the dev/e2e builds (the harness builds debug binaries), `test` the crate's own
# suites, and the feature is the explicit opt-in for a release-profile test
# build. A shipped artifact has none of the three.
_SEAM_GATE = '#[cfg(any(test, debug_assertions, feature = "test-helpers"))]'

# Crates whose `test-helpers` feature forwards this seam family. `fauna-ipc`
# declares it; `fauna-client-sync` forwards it (the app-side requester);
# `fauna-sync-agent` forwards it (the handler). Turning ANY of them on from a
# shipping dep line puts the verbs back into a release artifact.
_SEAM_FEATURE_CRATES = ("fauna-ipc", "fauna-client-sync", "fauna-sync-agent")


def _gated_request_methods() -> list[str]:
    """Every `RequestMethod` variant that sits directly under the e2e seam gate.

    Derived rather than listed, so a new test-only verb needs no edit here. The
    match is deliberately narrow — the gate line immediately followed by a
    variant name — because that is the only shape the enum actually uses, and a
    looser scan would start reporting the gated `ResponsePayload` arm and the
    struct definitions below it as `RequestMethod` variants.
    """
    text = _IPC_SYNC.read_text(encoding="utf-8")
    body = re.search(r"pub enum RequestMethod \{(.*?)\n\}", text, re.DOTALL)
    assert body, "libs/fauna-ipc/src/sync.rs no longer declares `pub enum RequestMethod`"
    return re.findall(
        re.escape(_SEAM_GATE) + r"\s*\n\s*([A-Z]\w+)",
        body.group(1),
    )


def test_the_agent_seam_verbs_are_gated_and_at_least_one_exists():
    """Every test-only `RequestMethod` verb carries the e2e gate — and the
    derivation actually finds verbs, so this file cannot pass by scanning
    nothing.

    The self-check matters more than it looks: the assertion below is an
    absence, and an absence over an empty set is vacuously true. If the enum is
    ever restructured so the regex stops matching, this fails loudly here rather
    than going quietly green while the real seams ship.
    """
    gated = _gated_request_methods()
    assert "CustodianRunPassNow" in gated, (
        "`RequestMethod::CustodianRunPassNow` is no longer directly under "
        f"{_SEAM_GATE} in libs/fauna-ipc/src/sync.rs. It runs a real custodian pull "
        "pass — pulling the owner's sealed corpus and checking in to the nest — on "
        "demand over the agent socket, so an ungated one is a production verb "
        "nobody meant to ship (testing.md convention 15). If it was renamed, rename "
        "it here; if it was deleted, delete this assertion with it."
    )


def test_the_handler_and_the_requester_carry_the_same_gate_as_the_verb():
    """The verb, the agent-side handler, and the app-side requester are one
    surface: gating any two of the three still ships the third.

    The failure this catches is a partial revert — dropping the `cfg` from the
    handler alone leaves a release agent carrying `handle_custodian_run_pass_now`
    and its whole call graph (the hosted-custodian slot, the pass driver) even
    though nothing can name the verb any more. Dead code that reaches a shipped
    artifact is still attack surface, and it is what a `strings` witness on the
    binary would report.
    """
    for path, symbol in (
        (
            _REPO / "bins" / "fauna-sync-agent" / "src" / "pipe_server.rs",
            "async fn handle_custodian_run_pass_now(",
        ),
        (
            _REPO / "libs" / "fauna-client-sync" / "src" / "agent.rs",
            "pub async fn custodian_run_pass_now(",
        ),
    ):
        text = path.read_text(encoding="utf-8")
        idx = text.find(symbol)
        assert idx != -1, f"{path.relative_to(_REPO)} no longer defines `{symbol}`"
        # The gate must be the nearest attribute above the definition. Searching
        # backwards from the symbol rather than anywhere in the file keeps an
        # unrelated gate elsewhere in these (large) files from satisfying it.
        preceding = text[:idx]
        gate_at = preceding.rfind(_SEAM_GATE)
        assert gate_at != -1, (
            f"{path.relative_to(_REPO)} defines `{symbol}` with no {_SEAM_GATE} "
            "anywhere above it"
        )
        between = preceding[gate_at + len(_SEAM_GATE) :]
        assert not re.search(r"^\s*(pub\s+)?(async\s+)?fn\s", between, re.MULTILINE), (
            f"the nearest {_SEAM_GATE} above `{symbol}` in "
            f"{path.relative_to(_REPO)} gates a DIFFERENT item — another function "
            "sits between them, so this seam is ungated and ships."
        )


def test_no_shipping_dep_line_turns_the_agent_seam_feature_on():
    """No crate in the workspace enables the seam feature on a `[dependencies]`
    line, where cargo turns it on unconditionally for every artifact.

    A consumer that needs the seams forwards them from its OWN opt-in feature
    (`fauna-tui`'s `e2e-agent` → `fauna-client-sync/test-helpers` is the
    reference shape), so the build recipe decides, not the manifest. Naming it on
    a dep line instead survives `--no-default-features` and every release recipe
    in the tree — convention 15 rule (b), and the exact mistake that shipped
    twelve FFI seams in five artifacts.

    Delegates the scan itself to `helpers.manifest_seams.shipping_feature_offenders`
    (multi-line-aware; see that module's docstring), narrowed to this family's own
    three crates so the message below stays specific to the IPC seam family; the
    general, all-crate form of this same scan lives in
    `test_test_helpers_dep_line_gating.py`.
    """
    offenders = shipping_feature_offenders(
        "test-helpers", _REPO, crates=set(_SEAM_FEATURE_CRATES)
    )
    assert not offenders, (
        "a shipping dependency line enables the agent-IPC seam feature, so "
        "`RequestMethod`'s test-only verbs (CustodianRunPassNow, …) are compiled "
        "into every artifact regardless of the build recipe (testing.md convention "
        "15 rule (b)). Forward `test-helpers` from the consuming crate's own e2e "
        "feature instead. Offending line(s): " + repr(offenders)
    )

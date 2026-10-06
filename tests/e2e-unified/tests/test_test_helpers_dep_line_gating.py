"""Convention 15 rule (b), the GENERAL form: no crate in the workspace may
name ANY `test-helpers`-declaring crate's `test-helpers` feature on a
SHIPPING dependency line — for every such crate, not only the handful an
existing witness happens to name.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention,
the *Shared Rust crates (`libs/fauna-*`)* bullet, rule (b) — *"A consumer
NEVER names `test-helpers` on a dep line; it forwards it from its own
`e2e-agent` feature."* Naming it on a shipping dep line turns the seams on
unconditionally, surviving `--no-default-features` and every release recipe
(the doc records the realized breach: `fauna-ffi` did exactly this until
2026-08-01, "which is why the seams rode five release artifacts" — twelve
FFI seams).

**Why this file exists alongside four narrower ones.**
`test_ffi_flavor_split.py`, `test_agent_ipc_seam_gating.py`,
`test_mail_import_tls_seam_gating.py` and `test_nest_test_hook_seam_gating.py`
each already pin rule (b) for the specific crate family THEIR OWN seam cares
about — three, three, three and one crates respectively, seven distinct
crates in total (`fauna-ipc`/`fauna-client-sync`/`fauna-sync-agent`,
`fauna-mail`/`fauna-client-mail-settings`/`fauna-ffi`, `fauna-nest`). **44
crates declare a `test-helpers` feature today** (found by
`helpers.manifest_seams.crates_declaring_feature`) — the other 37, including
`fauna-mls` (whose `test-helpers` is the raw-inject hook that MINTS,
`bins/fauna-nest/Cargo.toml`), `fauna-core`, `fauna-transport`,
`fauna-protocol`, `fauna-conversations`, `fauna-onboarding-machine`, and
`fauna-sync-engine` (whose feature flips an optional wiremock HTTP-server
dependency on), are covered by NO witness on this axis: if some future dep
line named any of them directly, nothing would catch it.

This file closes that gap the same way `test_shared_crate_seam_gating.py`
closes rule (a)'s analogous gap: **the crate axis is DERIVED, not
maintained** — every crate that declares `test-helpers` is found by regex,
so a crate gaining the feature tomorrow is covered with no list for anyone
to update. The narrower four witnesses stay: each explains a different
seam's blast radius in its own assertion message, which is worth preserving,
and each now delegates its own scan to the same shared, multi-line-aware
helper this file uses (`helpers.manifest_seams.shipping_feature_offenders`).

Pure text analysis of every workspace manifest — no build, no driver.
"""

from __future__ import annotations

import pytest

from helpers.manifest_seams import REPO as _REPO
from helpers.manifest_seams import crates_declaring_feature, shipping_feature_offenders

pytestmark = pytest.mark.tier_1

# Named in the finding as previously-uncovered by every existing witness —
# a floor this file's derivation must still clear, so a regex regression that
# silently narrows the crate axis fails loudly here rather than the absence
# assertion below merely passing over an emptier set.
_PREVIOUSLY_UNCOVERED = {
    "fauna-mls",
    "fauna-core",
    "fauna-transport",
    "fauna-protocol",
    "fauna-conversations",
    "fauna-onboarding-machine",
    "fauna-sync-engine",
}


def test_the_crate_axis_is_actually_derived_and_covers_the_known_gap():
    """Vacuity self-check: the derivation currently finds a wide, plausible
    set of `test-helpers`-declaring crates (44 at filing, with slack for
    growth), including every crate the finding named as uncovered by the
    four narrower witnesses. If the regex regresses to matching nothing (or
    almost nothing), `test_no_shipping_dep_line_turns_any_test_helpers_crate_on`
    below would pass VACUOUSLY — over an empty or near-empty set — which is
    exactly the failure mode this self-check exists to catch.
    """
    crates = crates_declaring_feature("test-helpers", _REPO)
    assert len(crates) >= 40, (
        f"expected at least 40 crates declaring a `test-helpers` feature (44 "
        f"measured at filing), found {len(crates)} — the derivation likely "
        "regressed rather than the tree actually losing crates"
    )
    missing = _PREVIOUSLY_UNCOVERED - crates
    assert not missing, (
        f"expected these crates (named in the finding as uncovered by every "
        f"existing witness) to be found by the derivation: {sorted(missing)} — "
        "either they no longer declare `test-helpers`, or the regex regressed"
    )


def test_no_shipping_dep_line_turns_any_test_helpers_crate_on():
    """No crate in the workspace enables ANY `test-helpers`-declaring crate's
    feature on a shipping `[dependencies]` line — the general form of rule
    (b), covering all 44 crates rather than the 7 an existing witness names.

    A consumer that needs the seams forwards them from its OWN opt-in
    feature (`libs/fauna-wasm`'s `test-helpers = ["fauna-conversations/test-helpers",
    "fauna-feed/test-helpers"]`, naming the feature on no dep line, is the
    reference shape), so the build recipe decides, not the manifest.
    """
    offenders = shipping_feature_offenders("test-helpers", _REPO)
    assert not offenders, (
        "a shipping dependency line enables a `test-helpers`-declaring crate's "
        "feature directly, turning its e2e-only seams on in every release "
        "artifact regardless of the build recipe "
        "(e2e-automation-surface-gating.md § The convention, convention 15 "
        "rule (b)). Forward it from the consuming crate's own opt-in feature "
        "instead. Offending line(s): " + repr(offenders)
    )

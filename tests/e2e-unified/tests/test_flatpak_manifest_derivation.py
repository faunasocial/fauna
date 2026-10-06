"""Pins for the test-flavoured Flatpak manifest derivation.

These run in every default loop (tier_1, no gates) precisely because the seam
test they protect runs in almost none (triple-gated real-session category) —
the 2026-08-11 credential-redirect gating inerted that test for four days and
nothing reported it. Each pin here fails fast where the gated test would fail
late, cryptically, after a ~45-minute flatpak build:

- the shipped manifest still builds ``--release`` with NO features
  (convention 15's direction: someone adding ``e2e-agent`` to the SHIPPED
  manifest ships the automation surface, and reds here);
- the derivation changes exactly the one build line and nothing else;
- the derived line carries the feature on BOTH packages, package-qualified;
- the shipped manifest is byte-untouched by deriving.
"""

from pathlib import Path

import pytest

from common import get_repo_root
from helpers.flatpak_manifest import (
    DERIVED_NAME,
    E2E_BUILD_LINE,
    SHIPPED_BUILD_LINE,
    derive_e2e_manifest,
)

pytestmark = [pytest.mark.tier_1]

SHIPPED = Path(get_repo_root()) / "apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml"


def test_shipped_manifest_builds_release_with_no_features():
    text = SHIPPED.read_text()
    assert text.count(SHIPPED_BUILD_LINE) == 1, (
        "the shipped Flatpak manifest's cargo build line moved — update "
        "helpers/flatpak_manifest.py in the same change, or the real-session "
        "seam test silently builds an inert install"
    )
    assert "--features" not in text, (
        "the SHIPPED Flatpak manifest must never enable cargo features: "
        "e2e-agent there ships the automation surface testing.md convention "
        "15 compiles out of release artifacts. The test flavour is derived, "
        "never committed — helpers/flatpak_manifest.py"
    )


def test_derivation_changes_exactly_the_build_line(tmp_path):
    # Run against a copy so the pin can't dirty the real packaging dir.
    shipped_copy = tmp_path / SHIPPED.name
    shipped_copy.write_text(SHIPPED.read_text())
    before = shipped_copy.read_text()

    derived = derive_e2e_manifest(shipped_copy)

    assert derived.name == DERIVED_NAME
    assert derived.parent == shipped_copy.parent, (
        "the derived manifest must sit BESIDE the shipped one: its module "
        "source is `type: dir, path: ../../../..`, resolved relative to the "
        "manifest's own directory"
    )
    assert shipped_copy.read_text() == before, "deriving must not touch the shipped file"

    old_lines = before.splitlines()
    new_lines = derived.read_text().splitlines()
    assert len(old_lines) == len(new_lines)
    diff = [
        (a, b) for a, b in zip(old_lines, new_lines) if a != b
    ]
    assert len(diff) == 1, f"exactly one line may differ, got {len(diff)}: {diff!r}"
    changed_old, changed_new = diff[0]
    assert SHIPPED_BUILD_LINE in changed_old
    assert E2E_BUILD_LINE in changed_new
    assert "fauna-linux/e2e-agent" in changed_new
    assert "fauna-sync-agent/e2e-agent" in changed_new


def test_derivation_refuses_a_reshaped_manifest(tmp_path):
    # The loud-failure contract: a manifest without the expected line must
    # raise, never silently yield an unflavoured (inert) install.
    reshaped = tmp_path / "reshaped.yml"
    reshaped.write_text("modules:\n  - name: fauna\n    buildsystem: simple\n")
    with pytest.raises(AssertionError, match="manifest changed shape"):
        derive_e2e_manifest(reshaped)

"""Derive the TEST-FLAVOURED Flatpak manifest from the shipped one.

The shipped manifest (`apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml`)
builds ``--release`` with no features — correctly: testing.md convention 15
compiles the automation surface out of release artifacts, and the
2026-08-11 credential-redirect gating put ``FAUNA_E2E_CREDENTIAL_DIR`` /
``FAUNA_KEYRING_APP`` behind ``cfg(any(test, debug_assertions,
feature = "e2e-agent"))``. That is exactly what made
``test_sync_agent_flatpak_seam.py`` inert: it handed both
vars to a sandbox whose binaries compiled the production twins, so its
scripted sign-in silently did nothing and all six downstream assertions were
unreachable.

The fix is NOT adding the feature to the shipped manifest — that would ship
the automation surface convention 15 exists to compile out. The real-session
test builds its own flavour instead, derived here by substituting exactly the
one cargo build line, so there stays a single source of truth and the shipped
manifest is byte-untouched.

The derived file is written BESIDE the shipped one on purpose: the manifest's
module source is ``type: dir, path: ../../../..`` — resolved relative to the
manifest's own directory — so a copy anywhere else would build from the wrong
tree (or nothing). The name is gitignored; each derivation overwrites it.
"""

from pathlib import Path

# The shipped build line, verbatim. If the shipped manifest changes this line,
# `derive_e2e_manifest` fails loudly instead of silently building an
# unflavoured (inert) test install — that failure IS the regression guard for
# the next gate landing (the success criterion).
SHIPPED_BUILD_LINE = "cargo build --release -p fauna-linux -p fauna-sync-agent"

# The test flavour: same profile, same packages, plus the `e2e-agent` feature
# on both — package-qualified, because a bare `--features` is ambiguous under
# multiple `-p`. The runtime mode switch stays OFF (`FAUNA_E2E_BRIDGE` /
# `_AGENT_PORT` unset ⇒ `e2e_mode_enabled()` is false), so the app still takes
# the real `LaunchChannel::FlatpakUnit` systemd/D-Bus path the seam test
# exists to exercise; only the credential backend becomes reachable.
E2E_BUILD_LINE = (
    "cargo build --release -p fauna-linux -p fauna-sync-agent"
    " --features fauna-linux/e2e-agent,fauna-sync-agent/e2e-agent"
)

DERIVED_NAME = "social.fauna.fauna.e2e-derived.yml"


def derive_e2e_manifest(shipped: Path) -> Path:
    """Write the test-flavoured manifest beside ``shipped`` and return its path.

    Raises if the shipped manifest does not contain exactly one occurrence of
    the expected build line — the derivation must be revisited by a human
    before the test can trust its own install again.
    """
    text = shipped.read_text()
    n = text.count(SHIPPED_BUILD_LINE)
    if n != 1:
        raise AssertionError(
            f"the shipped Flatpak manifest carries {n} occurrence(s) of the "
            f"expected build line {SHIPPED_BUILD_LINE!r} — the manifest "
            f"changed shape, and deriving the test flavour blindly would "
            f"build an inert (or wrong) install. Update "
            f"helpers/flatpak_manifest.py alongside the manifest."
        )
    derived = shipped.parent / DERIVED_NAME
    derived.write_text(text.replace(SHIPPED_BUILD_LINE, E2E_BUILD_LINE))
    return derived

"""The DMG channel: package, mount, copy out, and launch what the user launches.

The `.dmg` is one of the two macOS distribution channels (`installers/macos.md`
§ Distribution Channels), and it is the one where the app arrives *quarantined*:
LaunchServices stamps `com.apple.quarantine` on anything copied out of a mounted
disk image, exactly as it does on a browser download. Nothing else in the fleet
exercises that. A regression in the packaging — a bundle that survives
`hdiutil` compression with a broken signature, a framework that does not
round-trip through the image's filesystem, an app that will not start once
quarantined — reaches users through this channel and no test before this file
could see it.

**The credentialed half is deliberately NOT here, and the split is the point.**
`just mac-dmg` demands four `FAUNA_*` signing/notarization secrets and then
talks to Apple's notary service. Those stay a user-gated proof. Subtract them and what remains is the entire
packaging round trip — compression, the mounted volume, the copy-out, the
quarantine an install carries — which is testable headlessly on every build.
The human is left with the inch that genuinely needs Apple credentials
(does Gatekeeper accept a *notarized* build), not the mile.

**Where that split actually falls, measured rather than assumed (2026-08-27).**
Gatekeeper is not only LaunchServices' business: exec'ing a quarantined bundle's
inner executable invokes it too, and on an un-notarized build the answer is a
blocking modal (`helpers.macos_artifact.approve_quarantined_copy` carries the
evidence). So the headless half can prove the round trip, the quarantine stamp,
the surviving signature, Gatekeeper's *refusal*, and the launch a user reaches
after approving that refusal — but it cannot prove an unattended launch of a
quarantined download, because for this build there is no such thing. That last
one is not a gap in the tests; it is the credentialed inch, and it arrives with
the first notarized build.

Cost: an `hdiutil` create + attach over a ~570 MB debug bundle, ~1 min.
"""

import plistlib
from pathlib import Path

import pytest

from drivers.macos import (
    bundle_executable,
    read_bundle_id,
    stage_bundle_with_instance_id,
)
from helpers.macos_artifact import (
    QUARANTINE_XATTR,
    approve_quarantined_copy,
    assert_gatekeeper_refuses,
    make_unsigned_dmg,
    mounted,
    read_quarantine,
    run,
    set_quarantine,
)

pytestmark = [pytest.mark.tier_4, pytest.mark.macos]


@pytest.fixture(scope="module")
def installed_from_dmg(macos_artifact_bundle, artifact_scratch) -> Path:
    """A copy of `Fauna.app` that came out of a real mounted disk image, quarantined.

    This is the fixture that makes the module honest: every assertion below runs
    against a bundle that was compressed into a UDZO image, mounted, copied off a
    real HFS/APFS volume, and marked as downloaded — not against the build output
    with an xattr sprinkled on it.
    """
    dmg = make_unsigned_dmg(macos_artifact_bundle, artifact_scratch / "Fauna-test.dmg")
    dest_root = artifact_scratch / "installed"
    dest_root.mkdir(exist_ok=True)
    dest = dest_root / "Fauna.app"
    if dest.exists():
        run("rm", "-rf", str(dest))
    with mounted(dmg) as mount:
        src = mount / "Fauna.app"
        assert src.is_dir(), (
            f"the mounted image has no Fauna.app at its root — `hdiutil create "
            f"-srcfolder` did not package the bundle as expected. Contents: "
            f"{sorted(p.name for p in mount.iterdir())}"
        )
        # `ditto` rather than `cp`: it is what Finder's drag-install uses, and the
        # only copy that preserves extended attributes and the signature's
        # resource fork across the volume boundary. A plain `cp -R` can strip
        # metadata and produce a "damaged" app — a self-inflicted failure that
        # would read as a packaging bug.
        run("ditto", str(src), str(dest))
    # What LaunchServices stamps on anything copied off a mounted image. Applied
    # explicitly because `ditto` from a *locally created* image does not always
    # inherit it, and a test that silently skipped the quarantine would be testing
    # a plain directory copy.
    set_quarantine(dest)
    return dest


def test_the_dmg_carries_a_complete_bundle(installed_from_dmg):
    """The round trip preserved everything the app needs.

    Compression + mount + copy is where a symlinked framework or an
    extended-attribute-bearing resource silently loses a piece. Asserting the
    same runtime dependencies the loose bundle carries makes the channel's
    fidelity falsifiable rather than assumed.
    """
    for rel in ("Contents/MacOS/Fauna",
                "Contents/MacOS/fauna-sync-agent",
                "Contents/Resources/AppIcon.icns",
                "Contents/Info.plist"):
        assert (installed_from_dmg / rel).exists(), (
            f"{rel} did not survive the DMG round trip — it is present in the "
            f"built bundle but absent from the installed copy."
        )
    assert read_bundle_id(installed_from_dmg) == "social.fauna.fauna", (
        "the installed copy's bundle id is not the shipped one; this fixture must "
        "package the BUILD OUTPUT, not a staged e2e copy."
    )


def test_the_installed_copy_is_quarantined(installed_from_dmg):
    """The property that distinguishes an install from a directory copy.

    If this is ever absent, every Gatekeeper-shaped assertion in this module is
    vacuous — the app would be launching as ordinary local software. Asserted
    directly so that failure mode is loud instead of silent.
    """
    value = read_quarantine(installed_from_dmg)
    assert value, (
        f"no {QUARANTINE_XATTR} on the installed copy: this module would be "
        f"testing an unquarantined directory copy, not a DMG install."
    )


def test_the_signature_survives_the_round_trip(installed_from_dmg):
    """A quarantined app with a broken signature is what users see as "damaged".

    `codesign --verify --deep --strict` on the *installed* copy is the check
    Gatekeeper's own assessment builds on. It is worth its own test because the
    failure is channel-specific: the same bundle verifies fine in `build/`, and
    only the compress/mount/copy path can break it.
    """
    run("codesign", "--verify", "--deep", "--strict", str(installed_from_dmg))

    xml = run("codesign", "-d", "--entitlements", "-", "--xml",
              str(installed_from_dmg)).stdout
    start = xml.find("<?xml")
    assert start >= 0, "the installed copy carries no entitlements"
    ents = plistlib.loads(xml[start:].encode())
    assert ents.get("com.apple.security.application-groups"), (
        f"the app-group entitlement did not survive the DMG round trip: {sorted(ents)}"
    )


def test_an_ad_hoc_signed_build_is_refused_by_gatekeeper(installed_from_dmg):
    """Gatekeeper assessment on the installed copy — and what it must say TODAY.

    This build is ad-hoc signed (`codesign --sign -`), never Developer-ID signed
    or notarized, so `spctl` MUST reject it. That negative is the useful
    assertion, for two reasons:

      * it proves the assessment path is actually reachable and running against a
        quarantined app — the precondition every future Gatekeeper test needs; and
      * it pins the boundary. When the credentialed proof lands (a real
        `build.sh --sign` + notarized `.dmg`), this test is the one that flips to
        `accepted`, which makes "is the signed channel actually signed?" a code
        question instead of a human one.

    A green here is therefore NOT "Gatekeeper is happy" — it is "Gatekeeper is
    watching, and correctly refuses an unsigned build".

    It is also the regime check the launch test below depends on, which is why the
    assertion lives in a shared helper rather than inline: the two must never drift
    into disagreeing about what "un-notarized" means.
    """
    assert_gatekeeper_refuses(installed_from_dmg)


@pytest.mark.feature("get-the-app")
def test_the_dmg_installed_app_launches_and_renders(installed_from_dmg,
                                                    artifact_scratch, app):
    """The journey the channel exists for: install from DMG, then run it.

    Reuses the live `app` fixture's nest and driver rather than standing up a
    second one — what is under test is the *installed copy's* ability to launch,
    not a second login flow. The staged relaunch goes through exactly the same
    per-instance-id staging the driver uses, so the DMG copy is subject to the
    same anti-wedge discipline as every other artifact launch.

    **The launch copy is approved first, and that stand-in is the honest part.**
    This test used to carry the quarantine into the launch on the premise that
    *"running the inner executable directly does not invoke Gatekeeper (that is
    LaunchServices' job)"*. That premise is false on Darwin 25.6.0 and the test
    hung on it: exec logs `com.apple.syspolicy.exec` `GK performScan` →
    `GK evaluateScanResult`, and `CoreServicesUIAgent` then logs
    `present code-evaluation prompt` — the blocking "Apple could not verify …"
    dialog. The app stayed alive and silent behind that modal and never served its
    agent, so the run burned its budget rather than failing.

    Widening the budget cannot fix it — a modal nobody dismisses never resolves —
    and the test above already asserts Gatekeeper REJECTS this artifact, so
    carrying the stamp into the launch asked one artifact to pass an assessment
    its sibling test pins as failing. What an un-notarized build can honestly
    witness is the launch a user reaches *after* the approval macOS makes them
    give, so that approval is now modelled explicitly
    (`approve_quarantined_copy`) and gated on the build actually being
    un-notarized — the day it is notarized, that gate fails and the stand-in is
    retired, leaving the real outcome. The stamp itself is untouched on
    `installed_from_dmg`, which has its own test.
    """
    # The regime this stands in for. Asserted here, not assumed: it is what makes
    # the approval below self-retiring rather than a permanent lowered bar.
    assert_gatekeeper_refuses(installed_from_dmg)

    staged = stage_bundle_with_instance_id(
        installed_from_dmg, artifact_scratch / "dmg-launch", "dmg-probe"
    )
    assert read_quarantine(staged), (
        "the staged launch copy lost the quarantine xattr before this test could "
        "approve it — the copy step no longer preserves xattrs, so this test is "
        "silently launching an ordinary directory copy and proves nothing about "
        "the DMG channel. Fix the staging, do not delete this assertion."
    )
    approve_quarantined_copy(staged)

    exe = bundle_executable(staged)
    assert exe.exists(), f"no launchable executable in the DMG-installed copy: {exe}"

    driver = app.driver
    driver.teardown()
    try:
        driver.launch({**driver._launch_config,
                       "app_path": str(staged),
                       "launch_mode": "bundle"})
        driver.assert_render_ready()
        assert read_bundle_id(Path(driver.bundle_path())).startswith(
            "social.fauna.fauna.e2e."
        )
    finally:
        # Put the session-scoped driver back on the ordinary artifact so the next
        # module does not inherit a launch pinned to this scratch copy.
        driver.teardown()
        driver.launch(driver._launch_config)


@pytest.fixture(scope="module", autouse=True)
def _reclaim_dmg_scratch(artifact_scratch):
    """Delete the image and the installed copies when the module finishes.

    Not politeness: a DMG plus two copies of a ~570 MB bundle is real disk, and
    macOS runs chronically close to full — a full build has zero-filled binaries
    and damaged sibling sessions before now. `tmp_path_factory` keeps the last
    few sessions' trees around, so without this the artifact suite would leave
    gigabytes behind per run.
    """
    yield
    for child in ("Fauna-test.dmg", "installed", "dmg-launch"):
        run("rm", "-rf", str(artifact_scratch / child), check=False)

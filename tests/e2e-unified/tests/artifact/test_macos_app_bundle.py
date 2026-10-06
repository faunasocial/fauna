"""The SHIPPED macOS artifact — `Fauna.app` — launches, renders, and drives.

Subject: the real bundle `just mac-app` assembles, which `mac-dmg` packages and
the all-in-one installer `.pkg` installs. Every other apple suite drives the bare
swift-build Mach-O, so everything a bundle *adds* — `Bundle.main` resolution,
Info.plist keys, `Contents/Resources`, the embedded `Sparkle.framework` and its
rpath, the bundled `fauna-sync-agent`, the code signature and its entitlements —
is exercised by exactly nothing until this file runs. That is the client-side
shape of the gap tier_4 exists to close for the nest (`testing.md` § The
four-tier taxonomy): binaries bypass packaging, and packaging is where Apple
runtime behaviour changes.

Read `conftest.py` in this directory first — it carries the render-death history
this suite is built around, and the reason the debug-config bundle is the
faithful subject.
"""

import plistlib
import re
import subprocess
from pathlib import Path

import pytest

from drivers.macos import bundle_executable, read_bundle_id
from helpers.macos_artifact import run

pytestmark = [pytest.mark.tier_4, pytest.mark.macos]

# What `mac-app` puts in the bundle that a bare binary has no notion of. Each is
# a real runtime dependency, not a manifest nicety:
#   * Sparkle.framework — the app links `@rpath/Sparkle.framework/Versions/B/Sparkle`;
#     absent, the app cannot auto-update. The recipe only prints a WARNING when it
#     is missing and assembles the bundle anyway, so a broken build ships happily
#     and nothing but this assertion turns that warning red.
#   * fauna-sync-agent — the .dmg channel's agent source; `LaunchdSyncAgentSpawner`
#     resolves `Contents/MacOS/fauna-sync-agent` and self-installs its LaunchAgent
#     (sync-agent.md § Packaging + lifecycle). Absent, file sync silently never runs
#     for every user who installed from the DMG rather than the .pkg.
#   * AppIcon.icns — the brand mark; its absence is the shipped-without-an-icon bug
#     the windows installer carried.
#   * Fauna-FileProvider.appex — the File Provider extension. Absent, System
#     Settings → Extensions lists nothing to enable and no Fauna location can ever
#     appear in Finder, which is exactly what shipped until 2026-08-28 (the recipe
#     assembled the bundle from SwiftPM, which cannot build an app extension —
#     installers/macos.md § App extensions). Its UI sibling rides the same phase.
#   * Fauna-Widget.appex — the home-screen widget (apps/common.md § Home-screen
#     widget). Absent, the widget gallery offers no Fauna widget at all.
_REQUIRED_BUNDLE_CONTENTS = (
    "Contents/Frameworks/Sparkle.framework",
    "Contents/MacOS/fauna-sync-agent",
    "Contents/Resources/AppIcon.icns",
    "Contents/Info.plist",
    "Contents/PkgInfo",
    "Contents/PlugIns/Fauna-FileProvider.appex",
    "Contents/PlugIns/Fauna-FileProviderUI.appex",
    "Contents/PlugIns/Fauna-Widget.appex",
)


def test_the_bundle_carries_everything_it_needs_at_runtime(macos_artifact_bundle):
    """Packaging completeness — the assertions the recipe's own WARNING does not make."""
    missing = [
        rel for rel in _REQUIRED_BUNDLE_CONTENTS
        if not (macos_artifact_bundle / rel).exists()
    ]
    assert not missing, (
        f"{macos_artifact_bundle} is missing {missing}. `just mac-app` prints only a "
        f"WARNING for an absent Sparkle.framework and assembles the bundle anyway, so "
        f"a bundle that ships without one looks like a successful build."
    )


def test_the_bundle_resolves_its_embedded_sparkle_at_runtime(macos_artifact_bundle):
    """Present-in-the-bundle is not the same as loadable.

    The app links `@rpath/Sparkle.framework/...`, so the framework only resolves
    if the runpath actually landed — the project's `LD_RUNPATH_SEARCH_PATHS`
    since the bundle moved to xcodebuild (2026-08-28), `mac-app`'s
    `install_name_tool -add_rpath` before that. Either way, embedding the
    framework and losing the rpath produces a bundle that passes the
    file-presence check above and then dies at launch with a dyld error — which
    is why this is a separate assertion on the Mach-O load commands rather than a
    second `exists()`, and why it asserts the OUTCOME rather than the mechanism
    that produced it.
    """
    exe = bundle_executable(macos_artifact_bundle)
    load_commands = run("otool", "-l", str(exe)).stdout
    assert "@executable_path/../Frameworks" in load_commands, (
        f"{exe} carries no `@executable_path/../Frameworks` LC_RPATH, so the "
        f"embedded Sparkle.framework cannot be resolved at launch:\n"
        f"{[ln.strip() for ln in load_commands.splitlines() if 'path ' in ln]}"
    )


def test_the_bundle_is_signed_with_its_entitlements(macos_artifact_bundle):
    """A valid signature AND the declared entitlements.

    Entitlements are the half a bare binary cannot have at all: the app group
    (`7457N3M72H.group.social.fauna.shared` — Team-ID-prefixed on macOS, which the
    File Provider extension and the app share),
    photo-library read-write, and the network client right. A bundle signed without
    them runs with a different capability posture than the one users get — the exact
    divergence this suite exists to catch, and invisible to every other test.
    """
    run("codesign", "--verify", "--deep", "--strict", str(macos_artifact_bundle))

    xml = run(
        "codesign", "-d", "--entitlements", "-", "--xml", str(macos_artifact_bundle),
    ).stdout
    start = xml.find("<?xml")
    assert start >= 0, f"the bundle declares no entitlements at all:\n{xml[:500]}"
    ents = plistlib.loads(xml[start:].encode())
    assert ents.get("com.apple.security.application-groups"), (
        f"no app-group entitlement on the signed bundle — the app and its File "
        f"Provider extension share state through it. Got: {sorted(ents)}"
    )


def test_release_and_debug_bundles_are_packaged_by_the_same_steps():
    """The stand-in argument, pinned rather than assumed.

    This suite drives the DEBUG bundle because convention 15 compiles the
    automation surface out of release artifacts, so the release bundle cannot be
    driven at all. That substitution is only honest while `mac-app`'s packaging is
    config-independent — and "someone will notice if that changes" is not a gate.

    So: read the recipe and assert that `{{config}}` reaches only the compile.
    A new `$CONFIG` branch around a packaging step makes this red, which is the
    moment to re-argue the substitution rather than discover months later that the
    suite has been testing a bundle assembled differently from the shipped one.
    """
    from common import get_repo_root

    recipe = _mac_app_recipe(get_repo_root() / "justfile")
    config_lines = [
        ln.strip() for ln in recipe.splitlines()
        if "CONFIG" in ln and not ln.strip().startswith("#")
    ]
    # The legitimate uses: reading the parameter, the FFI-flavor branch, the
    # output/configuration name, the agent's artifact dir, and its --release flag.
    allowed = ("CONFIG=", "$CONFIG", "${CONFIG", "if [", "else", "fi")
    unexpected = [ln for ln in config_lines if not any(a in ln for a in allowed)]
    assert not unexpected, f"unclassifiable $CONFIG use in mac-app: {unexpected}"

    packaging = [
        ln for ln in config_lines
        if any(step in ln for step in ("Info.plist", "AppIcon", "PkgInfo",
                                       "Sparkle", "install_name_tool", "codesign"))
    ]
    assert not packaging, (
        f"`just mac-app` now branches its PACKAGING on {{config}}:\n"
        + "\n".join(packaging)
        + "\n\nThis suite drives the DEBUG bundle as a stand-in for the shipped "
          "RELEASE one, and that only holds while config changes the compile and "
          "nothing else. Re-argue the substitution (conftest.py's "
          "`helpers/macos_artifact.ARTIFACT_APP_RELPATH` note) before relaxing this."
    )


def _recipe_body(justfile: Path, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name config="release":` declaration alike."""
    lines = justfile.read_text().splitlines()
    header = re.compile(rf"^{re.escape(name)}(\s+\S+)*:")
    for i, ln in enumerate(lines):
        if header.match(ln):
            body = []
            for nxt in lines[i + 1:]:
                # A recipe body is indented; the next unindented non-blank line
                # starts the following recipe or comment block.
                if nxt and not nxt[0].isspace():
                    break
                body.append(nxt)
            return "\n".join(body)
    raise AssertionError(f"no `{name}` recipe found in {justfile}")


def _mac_app_recipe(justfile: Path) -> str:
    """Every line of the `just mac-app` path: the parametrized front recipe AND
    the `_mac-app-impl` it delegates to.

    Reading only `mac-app` made this check VACUOUS — the front recipe has been a
    four-line delegation since the one-slot split, so its body holds exactly one
    `$CONFIG` line (`export CONFIG=...`) and none of the packaging this test
    exists to police. Concatenating both bodies is also what keeps it honest if
    the path is split again.
    """
    return (
        _recipe_body(justfile, "mac-app")
        + "\n"
        + _recipe_body(justfile, "_mac-app-impl")
    )


# ---------------------------------------------------------------------------
# The launched artifact
# ---------------------------------------------------------------------------


def test_the_launched_app_is_the_bundle_under_a_per_instance_identity(app):
    """The mine this suite is built to avoid, asserted directly.

    Two properties in one launch, because they are the same property:
      * the process really came out of a staged `.app` (not the bare binary this
        run's `--macos-artifact` flag was supposed to replace — a silently
        un-flipped mode would otherwise produce a fully green artifact suite that
        never touched an artifact), and
      * that staged copy does NOT carry the app's fixed `social.fauna.fauna`
        identity, which is what wedged WindowServer over repeated launch-and-die
        cycles.
    """
    staged = app.driver.bundle_path()
    assert staged, (
        "the macOS driver launched in bare-binary mode: `--macos-artifact` did not "
        "reach `_build_app_config`, so this suite would pass without ever "
        "testing an artifact."
    )
    staged = Path(staged)
    assert staged.is_dir() and staged.suffix == ".app"

    identity = read_bundle_id(staged)
    assert identity != "social.fauna.fauna", (
        "the staged bundle kept the app's FIXED bundle id. That is the exact "
        "configuration that stops the app getting a WindowServer-backed window "
        "after enough launch-and-die cycles (apple-e2e-automation.md rule 9) — the "
        "render-death, which presents as a machine needing a reboot."
    )
    assert identity.startswith("social.fauna.fauna.e2e."), identity


def test_the_bundle_renders_a_real_window(app):
    """Render-readiness for the artifact path.

    `assert_render_ready` is the tripwire the bare-binary launch keeps as a
    formality — here it is the actual assertion. A bundle launch that answers
    `/health` while creating no window is precisely the render-death signature,
    and this is the test that would catch a regression in the per-instance-id
    staging (a reintroduced fixed id renders fine ONCE, then stops).
    """
    app.driver.assert_render_ready()


@pytest.mark.feature("get-the-app")
def test_the_bundle_drives_a_login_to_feed_journey(logged_in_app, test_user):
    """The minimal journey, end to end, against the artifact.

    Deliberately the ordinary one — `--macos-artifact` swaps only what gets
    launched, so this is the same login-to-feed path the bare-binary suite runs.
    Its value is not novelty but subject: it proves the packaged app can reach a
    signed-in feed with real entitlements, a sandboxed container, and
    `Bundle.main` pointing at the `.app`.
    """
    app = logged_in_app
    assert app.driver.bundle_path(), "not an artifact launch"
    app.wait_for("feed-tab")
    assert app.is_visible("feed-tab"), (
        f"the packaged app signed in but never rendered the feed. "
        f"error element: {app.error_text()!r}"
    )


# Generous, named: LaunchServices resolves an `open` of an already-running app in
# well under a second on an idle box, but this machine routinely runs 3+ sibling
# builds; the poll returns the moment the counter moves (convention 14).
REOPEN_DELIVERY_BUDGET = 60


@pytest.mark.feature("second-identity-in-its-own-window")
def test_launching_the_bundle_again_lands_on_the_running_instance(app):
    """A second launch takes you back to what is already running — never a copy.

    macOS's leg of `second-identity-in-its-own-window` outcome 3. The desktop
    platforms whose OS starts a second process on an icon re-click witness that
    outcome with a launch-collision chooser (`test_launch_instance_chooser_*`);
    macOS's ratified design has no colliding process to put a chooser in —
    LaunchServices deduplicates the second launch before any app code runs and
    hands the running instance a reopen event instead (`account-scoping.md`
    § Concurrent instances: the plain-launch raise layer). So the promise a
    macOS user gets from "launching the app again" is the raise itself, and
    this test witnesses it through the only door a real relaunch can take:
    `open` on the staged bundle, i.e. LaunchServices — which is also why it
    lives in the artifact suite; the bare binary the ordinary apple tests
    launch has no Launch Services identity to deduplicate against.

    Both ends are asserted, on linux's raise-channel pattern (the server's
    `raises_served` twin): the running app HANDLED the reopen (its
    `reopens_handled` counter advanced — delivery alone would not move it), and
    no second process of this bundle exists. The counter advancing is the causal
    barrier the negative half anchors to (convention 14): LaunchServices answers
    one `open` with either a launch or a reopen, so once the reopen is in hand,
    "no second copy" is that verdict's other face, not a settle-sleep guess.
    """
    staged = app.driver.bundle_path()
    assert staged, (
        "the macOS driver launched in bare-binary mode — this test's subject is "
        "the staged bundle's Launch Services identity, which a bare binary "
        "does not have."
    )

    before = app.driver.get_state() or {}
    assert "reopens_handled" in before, (
        "the macOS shell does not serialize `reopens_handled` — without it a "
        "reopen's arrival is unobservable and this test cannot tell 'the raise "
        "was handled' from 'nothing happened' (convention 11: an absent key "
        "must refuse loudly, never read as zero)."
    )
    baseline = before["reopens_handled"]

    launched_again = run("open", staged)
    assert launched_again.returncode == 0

    raised = app.driver.wait_for_state(
        lambda s: (s or {}).get("reopens_handled", baseline) > baseline,
        timeout=REOPEN_DELIVERY_BUDGET,
    )
    assert raised.get("reopens_handled", 0) > baseline, (
        f"the running instance never handled the reopen; reopens_handled="
        f"{raised.get('reopens_handled')!r} (baseline {baseline}). "
        f"error element: {app.error_text()!r}"
    )

    # The pattern pins the APP executable: the bundled `fauna-sync-agent` lives
    # in the same `Contents/MacOS/` dir, and pgrep is case-sensitive — `Fauna`
    # matches only the app binary (`CFBundleExecutable`), never the agent.
    inner = str(Path(staged) / "Contents" / "MacOS" / "Fauna")
    survivors = subprocess.run(
        ["pgrep", "-f", inner], capture_output=True, text=True,
    ).stdout.split()
    own_pid = str(app.driver._app_proc.pid)
    assert survivors == [own_pid], (
        f"expected exactly the driver's own app process ({own_pid}) after the "
        f"second launch, found {survivors} — LaunchServices started a second "
        f"copy instead of raising the running one."
    )


@pytest.mark.feature("second-identity-in-its-own-window")
def test_relaunching_the_bundle_after_quitting_starts_normally_as_the_same_account(
    logged_in_app,
):
    """When the running instance is gone, launching again starts normally.

    macOS's leg of `second-identity-in-its-own-window` outcome 6 ("if the place you
    asked to go back to is already gone, the app starts normally instead of
    failing"). macOS renders no chooser, and its way "back to" a running window is
    the LaunchServices relaunch (`account-scoping.md` § Concurrent instances):
    outcome 3's sibling above witnesses it when an instance is running and the
    launch raises it; this is the same door with nothing running behind it, where
    LaunchServices must start a fresh process instead — and that process must come
    up as a normal authenticated session, not fail, hang on a dead handle, or
    ask who you are.

    Quit goes through the app's own ⌘Q path (`quit_app()`, never a signal), the
    departure is asserted before relaunching (the relaunch door is only this one
    when nothing is running), and every wait is on state (convention 14). The
    relaunch reuses the first launch's isolated install through `open --env`, so
    "the same account" is what the app restores from its own store.
    """
    app = logged_in_app
    driver = app.driver
    assert driver.bundle_path(), (
        "the macOS driver launched in bare-binary mode — this test's subject is "
        "the staged bundle's LaunchServices relaunch, which a bare binary does not have."
    )

    session = driver._last_session or {}
    want = session.get("actor_id")
    assert want, f"the logged-in app carries no session actor to compare against: {session!r}"
    assert driver.get_state("session.authenticated"), "the app is not signed in before the quit"
    first_pid = driver._app_proc.pid

    driver.quit_app()
    assert driver.wait_app_exit(timeout=REOPEN_DELIVERY_BUDGET), (
        f"the app (pid {first_pid}) did not exit after its own quit path"
    )
    inner = str(Path(driver.bundle_path()) / "Contents" / "MacOS" / "Fauna")
    gone = subprocess.run(["pgrep", "-f", inner], capture_output=True, text=True).stdout.split()
    assert not gone, f"a process of the bundle survived the quit: {gone}"

    driver.relaunch_through_launch_services()

    restored = driver.wait_for_state(
        lambda s: bool(((s or {}).get("session") or {}).get("authenticated"))
        and ((s or {}).get("session") or {}).get("actor_id") == want,
        timeout=REOPEN_DELIVERY_BUDGET,
    )
    got = (restored or {}).get("session") or {}
    assert got.get("authenticated") and got.get("actor_id") == want, (
        f"the relaunched app did not start as the same signed-in account; "
        f"session={got!r}. error element: {app.error_text()!r}"
    )
    second_pid = driver._app_proc.pid
    assert second_pid != first_pid, "the 'relaunch' is the process that was quit"
    driver.assert_render_ready()
    survivors = subprocess.run(["pgrep", "-f", inner], capture_output=True, text=True).stdout.split()
    assert survivors == [str(second_pid)], (
        f"expected exactly the relaunched process ({second_pid}), found {survivors}"
    )


def test_two_artifact_launches_get_distinct_identities(app, macos_artifact_bundle,
                                                       artifact_scratch):
    """Concurrency safety, which is what the whole staging design buys.

    Several apple e2e runs sharing one macOS machine is the normal steady state,
    not an edge case, so the artifact mode is only usable if two live bundles never
    share one Launch Services identity. Staging a second copy while the first is
    running and comparing identities asserts that without needing a second nest or
    a second driver.
    """
    from drivers.macos import stage_bundle_with_instance_id

    live = read_bundle_id(Path(app.driver.bundle_path()))
    sibling = stage_bundle_with_instance_id(
        macos_artifact_bundle, artifact_scratch, "sibling-probe"
    )
    assert read_bundle_id(sibling) != live, (
        f"a second staged bundle got the same identity as the live one ({live}) — "
        f"two concurrent artifact launches would collide on it, which is the "
        f"WindowServer wedge."
    )
    # The staged sibling must be launchable too: an unsigned or badly-signed copy
    # would fail here rather than at some later launch.
    subprocess.run(
        ["codesign", "--verify", "--deep", "--strict", str(sibling)], check=True,
    )

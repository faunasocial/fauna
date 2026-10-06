"""The INSTALLED linux app — `install.sh` puts it in a prefix, and then it drives.

Subject: the app as an **install channel produced it**, not the binary sitting in
`target/debug`. That is the linux statement of this category's rule — the same
one `test_macos_app_bundle.py` makes for `Fauna.app` and the windows installer
suite's `TestFullJourneyInstalledApp` makes for the MSI-installed product:
packaging is a layer, and a layer nothing drives is a layer nothing checks.

**The gap this closes, exactly.** `tests/platform/linux/test_installer.py` and
`test_installer_structure.py` are thorough about *structure* — `install.sh`,
`dpkg -i`, the AppDir, the Flatpak and Snap manifests, the desktop entry, the
systemd unit's exec and condition lines — and every one of those assertions is a
statement about files. None of them starts the thing they installed and asks it
to do anything. So `docs/features/get-the-app.md` outcome 4, *"On Linux, the
packaged app installs **and** launches"*, had no witness and honestly read
`(none)`.

**What only this module can catch.** Everything that differs between "a binary
the build produced" and "a binary an install channel placed":

* The sync agent is resolved **beside the app** (`sync_agent.rs`
  `agent_binary_absolute`, whose failure path logs *"fauna-sync-agent not found
  beside the app or on PATH"*). In the build tree it is beside the app by
  accident — cargo put both in one directory. Under an install it is beside the
  app only because the channel deliberately copied it there, which every channel
  must (`installers/linux-desktop.md` § Installation Files) and which is exactly
  what broke live on windows in 2026-07-17. A channel that drops the agent
  leaves file sync dead for everyone who installed that way, behind a warn-level
  log line.
* The installed binary is launchable *at its installed path* — the
  `install -Dm755` mode, and nothing in the app resolving a path relative to the
  build tree.
* The desktop entry lands under the app-id basename, which is what associates a
  running window with its icon (`install.sh`'s own comment on `APP_ID`, and the
  2026-08-22 rename that made basename and id one string).

Read `helpers/linux_installed_product.py` first: it owns the prefix, the staging
seam that lets a shipped installer install a debug-config binary without being
given a test-only knob, and why every invocation carries a throwaway
`XDG_CONFIG_HOME`.
"""

import re
import stat
from pathlib import Path

import pytest

from common import get_repo_root
from helpers.linux_installed_product import (
    _STAGING_RELPATH,
    child_sees,
    install_prefix,
    installed_agent_path,
    installed_app_path,
    run_install,
    run_uninstall,
)

# tier_4: the subject is a real deployment artifact rather than locally-built
# binaries (`testing.md` § The four-tier taxonomy, point 4's shipped-CLIENT-artifact
# arm — the arm this whole directory is). The nest half stays a locally-built
# binary, which does not lower the tier: a tier names what the test's SUBJECT is,
# and here that is the install channel's output.
#
# `installed_product` is MODULE-level, not per-test, and must stay that way:
# `_installed_linux_app_override` reads the marker across the whole session
# (the linux driver is session-scoped via `_driver_cache`), so a marker on only
# some tests here would leave a `-k`-narrowed run silently driving the build
# tree — the false pass this module exists to make impossible.
pytestmark = [pytest.mark.tier_4, pytest.mark.linux, pytest.mark.installed_product]


@pytest.fixture(scope="session")
def installed_product(linux_app_path):
    """Run the real `install.sh`, yield the installed app, then uninstall it.

    `linux_app_path` is the ordering that matters: it BUILDS the debug binaries
    (and skips loudly if the build leaves none), so the staging copy always has
    something to copy. Session-scoped because the linux driver is, and because
    `_installed_linux_app_override` is session-wide — the install must exist
    before the first app launch of the run, whichever test triggers it.
    """
    repo = Path(get_repo_root())
    proc = run_install(repo, Path(linux_app_path).parent)
    assert proc.returncode == 0, (
        f"`install.sh` failed (rc={proc.returncode}) — nothing to drive.\n"
        f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    )
    app = installed_app_path(repo)
    assert app.is_file(), "`install.sh` reported success but %s does not exist" % app
    yield app
    out = run_uninstall(repo)
    assert out.returncode == 0, (
        f"`install.sh --uninstall` failed (rc={out.returncode}); the prefix "
        f"{install_prefix(repo)} may be left dirty.\nstderr:\n{out.stderr}"
    )


@pytest.fixture
def journey_app(installed_product, logged_in_app):
    """The INSTALLED app, logged in against a real nest.

    The fixture order IS the contract — the windows journey's rule, for the same
    reason: `installed_product` must materialize the binaries before
    `logged_in_app` launches a driver at them, or
    `_installed_linux_app_override` raises rather than quietly falling back to
    the build tree.
    """
    return logged_in_app


def test_the_installed_app_is_what_the_driver_launched(installed_product, app):
    """The subject check, before any assertion that depends on it.

    Without it the whole module could pass against `target/debug/fauna-desktop`
    — the false green this category exists to make impossible, and the one
    `test_the_bundle_drives_a_login_to_feed_journey` guards with its own
    `bundle_path()` assertion.
    """
    launched = app.driver.app_path()
    assert launched, "the linux driver recorded no app_path — it never launched"
    assert Path(launched) == installed_product, (
        f"the driver launched {launched}, not the installed app "
        f"{installed_product} — `_installed_linux_app_override` did not reach "
        f"`_build_app_config`, so this module would pass without ever testing an "
        f"install."
    )


def test_the_install_puts_the_sync_agent_where_the_app_looks_for_it(installed_product):
    """`agent_binary_absolute` resolves the agent BESIDE the app.

    Asserted here rather than only in the structure suite because this module is
    the one that knows the app's own resolution rule, and because the journey
    below proves the installed app actually runs — together they say the
    placement is right *for the binary that got installed*, not for a path a
    script printed.
    """
    agent = installed_agent_path(Path(get_repo_root()))
    assert agent.is_file(), (
        f"{agent} is missing: the app resolves fauna-sync-agent beside itself or "
        f"on PATH (sync_agent.rs agent_binary_absolute), so an install channel "
        f"that drops it leaves file sync dead with only a warn-level log line "
        f"(installers/linux-desktop.md § Installation Files)."
    )
    assert agent.stat().st_mode & stat.S_IXUSR, f"{agent} is not executable"


@pytest.mark.feature("get-the-app")
def test_the_installed_app_drives_a_login_to_feed_journey(journey_app, test_user):
    """The minimal journey, end to end, against what `install.sh` installed.

    Deliberately the ordinary one — the install swaps only *what gets launched*,
    so this is the same login-to-feed path every other linux suite runs. Its
    value is not novelty but subject: it is the sentence
    `docs/features/get-the-app.md` outcome 4 actually makes — the packaged app
    installs **and launches** — and until it existed that outcome had no witness.

    It asserts its own subject before its outcome, exactly as
    `test_the_bundle_drives_a_login_to_feed_journey` does with `bundle_path()`.
    That line is not redundant with
    `test_the_installed_app_is_what_the_driver_launched`: a mutation round
    (2026-08-27) pointed `_installed_linux_app_override` at the build tree and
    this test still passed on the strength of the other one failing — but the
    other one is a *different test*, so a `-k`-narrowed or partially-deselected
    run could report this outcome green while driving the build tree. The
    outcome cell this test colours has to be trustworthy standing alone.
    """
    app = journey_app
    launched = app.driver.app_path()
    assert launched and Path(launched) == installed_app_path(Path(get_repo_root())), (
        f"this outcome is only true of the INSTALLED app, and the driver launched "
        f"{launched} — colouring get-the-app outcome 4 from this run would claim "
        f"an install was proven when the build tree was driven."
    )
    app.wait_for("feed-tab")
    assert app.is_visible("feed-tab"), (
        f"the installed app signed in but never rendered the feed. "
        f"error element: {app.error_text()!r}"
    )


def test_install_sh_branches_on_no_build_configuration():
    """The honesty pin for the debug-binary substitution.

    This module installs DEBUG binaries, because convention 15 compiles the
    automation surface out of release builds and a release `fauna-desktop`
    therefore cannot be driven at all (`helpers/linux_installed_product` says so
    at length). The substitution is honest only while `install.sh` does the same
    thing whatever it is handed: it resolves two paths under
    `$CARGO_TARGET_DIR/release` and `install -Dm755` them, and every later step
    reads the repo, not the build config.

    So: read the script and assert no *control flow* in it tests a build
    configuration. The day someone adds a release-only step, this says so —
    instead of the module quietly testing a path no user walks. Exactly the role
    `test_release_and_debug_bundles_are_packaged_by_the_same_steps` plays for the
    macOS bundle recipe.
    """
    script = (Path(get_repo_root()) / "apps" / "fauna-linux" / "install.sh").read_text()

    # The staging seam: CARGO_TARGET_DIR/release/<name> is what the helper
    # populates with debug binaries. If these move, the seam moves with them.
    assert '"$CARGO_TARGET_DIR/release/fauna-desktop"' in script, (
        "install.sh no longer reads $CARGO_TARGET_DIR/release/fauna-desktop — "
        "helpers/linux_installed_product stages the debug binaries through "
        "exactly that path, so the seam is gone and the module is installing "
        "something other than it thinks."
    )

    control_flow = re.compile(r"^\s*(if|elif|while|until|case)\b")
    offenders = [
        line for line in script.splitlines()
        if control_flow.match(line) and re.search(r"\b(debug|release)\b", line, re.I)
    ]
    assert not offenders, (
        f"install.sh now branches on a build configuration: {offenders}. This "
        "module installs a DEBUG binary through the CARGO_TARGET_DIR seam "
        "because a release build has no automation surface (convention 15); a "
        "config-conditional step makes that substitution dishonest — the "
        "installed subject would no longer be what a user gets. Either keep the "
        "step config-independent, or build `--release --features e2e-agent` here "
        "and drop the substitution."
    )


def test_the_installer_reads_the_environment_this_module_hands_it():
    """The isolation and the staging seam, asserted where they actually bind.

    Both of this module's environment promises — the staging `CARGO_TARGET_DIR`
    that lets a shipped installer install a debug binary, and the throwaway
    `XDG_CONFIG_HOME` that keeps `--uninstall` out of the developer's real
    systemd user session (convention 10) — are made by handing `install.sh` an
    environment. So they must be asserted on **what the child reads**, never on
    the dict handed to `subprocess.run`.

    That distinction is the whole test, and it is not pedantry: the first run of
    this module failed with "release binaries not found" while the dict was
    perfectly correct. A dev machine here exports `BASH_ENV=$HOME/.bashrc`, bash
    sources `$BASH_ENV` at the start of every NON-interactive shell — after the
    passed environment is installed — and that rc re-exports `CARGO_TARGET_DIR`
    from the working directory, silently replacing ours. A dict-side assertion
    called that green. This one cannot: it launches a real bash child through the
    same helper and reads back what the child got.

    The `BASH_ENV` line below is the regression pin for that specific mechanism —
    if a future edit stops dropping it, every promise here quietly reverts to
    whatever the box's login rc says.
    """
    repo = Path(get_repo_root())
    seen = child_sees(repo, ("CARGO_TARGET_DIR", "XDG_CONFIG_HOME", "PREFIX", "BASH_ENV"))

    assert seen.get("BASH_ENV") == "", (
        f"the child inherited BASH_ENV={seen.get('BASH_ENV')!r}. Bash sources it "
        f"at the start of every non-interactive shell, AFTER the environment we "
        f"passed, so anything that file exports overwrites ours — which is how "
        f"this module's first run read the real build tree instead of its "
        f"staging copy. `_isolated_env` must keep dropping it."
    )

    staging = str(repo / _STAGING_RELPATH)
    assert seen.get("CARGO_TARGET_DIR") == staging, (
        f"the child reads CARGO_TARGET_DIR={seen.get('CARGO_TARGET_DIR')!r}, not "
        f"the staging tree {staging!r}. `install.sh` resolves its two source "
        f"binaries under $CARGO_TARGET_DIR/release, so the staging seam is what "
        f"lets it install the DEBUG binaries this module can actually drive — "
        f"and without it, it installs (or fails to find) the real release ones."
    )

    xdg = seen.get("XDG_CONFIG_HOME", "")
    assert xdg, (
        "the child reads no XDG_CONFIG_HOME — an `install.sh --uninstall` would "
        "now run `systemctl --user disable --now` and delete the unit in the "
        "developer's REAL session (convention 10)."
    )
    assert Path(xdg).is_relative_to(repo), (
        f"XDG_CONFIG_HOME is {xdg}, outside the repo — the redirect must land "
        f"somewhere disposable, never in the box's own config."
    )
    assert Path(xdg) != Path.home() / ".config", "the redirect IS the real config dir"

    assert seen.get("PREFIX") == str(install_prefix(repo)), (
        f"the child reads PREFIX={seen.get('PREFIX')!r}; `install.sh` would "
        f"install somewhere other than where this module looks for the result."
    )

    # And the script really does resolve the unit through XDG_CONFIG_HOME, so
    # redirecting it binds.
    script = (repo / "apps" / "fauna-linux" / "install.sh").read_text()
    assert "${XDG_CONFIG_HOME:-$HOME/.config}" in script, (
        "install.sh no longer resolves the agent unit through XDG_CONFIG_HOME, "
        "so redirecting it no longer isolates the uninstall. Re-read "
        "installers/linux-desktop.md § Uninstall and re-derive the seam."
    )

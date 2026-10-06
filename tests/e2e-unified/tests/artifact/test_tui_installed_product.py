"""The INSTALLED terminal app — the archive's `install.sh` puts it in a prefix, and then it drives.

Subject: the app as **its channel produced it** — the per-OS archive of
`installers/tui.md` § The ratified channel, unpacked and installed by the
`install.sh` it carries — not the binary sitting in `target/debug`. The terminal
app's statement of this category's rule, the one `test_linux_installed_product.py`
makes for `install.sh`, `test_macos_app_bundle.py` for `Fauna.app` and the windows
installer suite's `TestFullJourneyInstalledApp` for the MSI: packaging is a
layer, and a layer nothing drives is a layer nothing checks.

**The gap this closes.** `docs/features/get-the-app.md` outcome 1 — *getting the
app the way your platform provides it ends with the app open and you signed in*
— had no tui witness, because the terminal app had no channel: the catalog
ruled (`feature-catalog.md` § Implementation status today) that the channel
comes first and no witness is minted to fill a cell. The channel was ratified
2026-09-26 and this is its witness, added under the existing channel-neutral
outcome rather than a tui-specific one.

**What only this module can catch:**

* The archive carries BOTH binaries and `install.sh` installs both — the app
  resolves `fauna-sync-agent` beside itself (`agent_spawner::agent_binary_absolute`),
  and in the build tree it is beside the app by accident; under an install it is
  there only because the channel deliberately put it there.
* The installed binary is launchable *at its installed path* (the `install -Dm755`
  mode; nothing resolving a path relative to the build tree).
* The layout `assemble-archive.sh` produces is the one `install.sh` reads —
  the two scripts are the channel, and this runs the real ones.

Read `helpers/tui_installed_product.py` first: it owns the prefix, the staging
seam (the real `assemble-archive.sh` over the DEBUG binaries, and why debug),
and the throwaway `XDG_CONFIG_HOME` every invocation carries.
"""

import re
import stat
from pathlib import Path

import pytest

from common import get_repo_root
from helpers.tui_installed_product import (
    STAGED_BINARIES,
    child_sees,
    install_prefix,
    installed_agent_path,
    installed_app_path,
    run_install,
    run_uninstall,
    staged_archive_dir,
)

# tier_4: the subject is a real deployment artifact — the channel's output —
# rather than locally-built binaries (`testing.md` § The four-tier taxonomy,
# point 4's shipped-client-artifact arm). The nest half stays a locally-built
# binary, which does not lower the tier: a tier names what the SUBJECT is.
#
# `installed_product` is MODULE-level and must stay that way (the linux module's
# reason): `_installed_tui_app_override` reads the marker across the whole
# session, so a marker on only some tests would leave a `-k`-narrowed run
# silently driving the build tree — the false pass this module exists to make
# impossible.
pytestmark = [pytest.mark.tier_4, pytest.mark.tui, pytest.mark.installed_product]


@pytest.fixture(scope="session")
def installed_product(tui_app_path):
    """Stage the archive with the real `assemble-archive.sh`, run its real
    `install.sh`, yield the installed app, then uninstall it.

    `tui_app_path` is the ordering that matters: it BUILDS the debug binaries
    (`just tui-debug` builds `fauna-tui` and `fauna-sync-agent` together), so the
    staging always has both to carry. Session-scoped because the tui driver is,
    and because `_installed_tui_app_override` is session-wide — the install must
    exist before the first app launch of the run.
    """
    repo = Path(get_repo_root())
    proc = run_install(repo, Path(tui_app_path).parent)
    assert proc.returncode == 0, (
        f"the archive's `install.sh` failed (rc={proc.returncode}) — nothing to drive.\n"
        f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    )
    app = installed_app_path(repo)
    assert app.is_file(), f"`install.sh` reported success but {app} does not exist"
    yield app
    out = run_uninstall(repo)
    assert out.returncode == 0, (
        f"`install.sh --uninstall` failed (rc={out.returncode}); the prefix "
        f"{install_prefix(repo)} may be left dirty.\nstderr:\n{out.stderr}"
    )
    assert not app.exists(), f"`--uninstall` left {app} behind"


@pytest.fixture
def journey_app(installed_product, logged_in_app):
    """The INSTALLED app, logged in against a real nest. The fixture order IS
    the contract: `installed_product` must materialize the binaries before
    `logged_in_app` launches a driver at them."""
    return logged_in_app


def test_the_archive_carries_both_binaries_and_the_installer(installed_product):
    """The staged layout is the channel's whole payload: two binaries and the
    script — `assemble-archive.sh`'s one definition, read back here."""
    staged = staged_archive_dir(Path(get_repo_root()))
    for name in (*STAGED_BINARIES, "install.sh"):
        path = staged / name
        assert path.is_file(), f"{path} is missing from the staged archive layout"
        assert path.stat().st_mode & stat.S_IXUSR, f"{path} is not executable"


def test_the_installed_app_is_what_the_driver_launched(installed_product, app):
    """The subject check, before any assertion that depends on it."""
    launched = app.driver.app_path()
    assert launched, "the tui driver recorded no app_path — it never launched"
    assert Path(launched) == installed_product, (
        f"the driver launched {launched}, not the installed app {installed_product} — "
        f"`_installed_tui_app_override` did not reach `_build_app_config`, so this "
        f"module would pass without ever testing an install."
    )


def test_the_install_puts_the_sync_agent_where_the_app_looks_for_it(installed_product):
    """`agent_binary_absolute` resolves the agent BESIDE the app; the channel
    that drops it leaves the Folders/Devices pages unable to provision sync."""
    agent = installed_agent_path(Path(get_repo_root()))
    assert agent.is_file(), (
        f"{agent} is missing: the app resolves fauna-sync-agent beside itself or on "
        f"PATH (agent_spawner::agent_binary_absolute), so an install channel that "
        f"drops it ships an app whose sync pages cannot provision "
        f"(installers/tui.md § What any channel must satisfy)."
    )
    assert agent.stat().st_mode & stat.S_IXUSR, f"{agent} is not executable"


@pytest.mark.feature("get-the-app")
def test_the_installed_app_drives_a_login_to_feed_journey(journey_app, test_user):
    """The minimal journey, end to end, against what the archive's `install.sh`
    installed — the sentence `get-the-app` outcome 1 makes, on the terminal app.

    It asserts its own subject before its outcome (the linux leg's lesson: a
    `-k`-narrowed run could otherwise report this outcome green while driving
    the build tree)."""
    app = journey_app
    launched = app.driver.app_path()
    assert launched and Path(launched) == installed_app_path(Path(get_repo_root())), (
        f"this outcome is only true of the INSTALLED app, and the driver launched "
        f"{launched} — colouring get-the-app outcome 1 from this run would claim an "
        f"install was proven when the build tree was driven."
    )
    app.wait_for("feed-tab")
    assert app.is_visible("feed-tab"), (
        f"the installed app signed in but never rendered the feed. "
        f"error element: {app.error_text()!r}"
    )


def test_install_scripts_branch_on_no_build_configuration():
    """The honesty pin for the debug-binary substitution.

    This module installs DEBUG binaries, because convention 15 compiles the
    automation surface out of release builds. The substitution is honest only
    while the two shipped scripts do the same thing whatever they are handed:
    `assemble-archive.sh` copies the two files it is pointed at, and
    `install.sh` installs from beside itself. So: read both and assert no
    *control flow* tests a build configuration — the linux leg's pin, widened
    to the channel's two scripts."""
    packaging = Path(get_repo_root()) / "apps" / "fauna-tui" / "packaging"
    control_flow = re.compile(r"^\s*(if|elif|while|until|case)\b")
    for name in ("assemble-archive.sh", "install.sh"):
        script = (packaging / name).read_text()
        offenders = [
            line for line in script.splitlines()
            if control_flow.match(line) and re.search(r"\b(debug|release)\b", line, re.I)
        ]
        assert not offenders, (
            f"{name} now branches on a build configuration: {offenders}. This module "
            "installs a DEBUG binary through the archive layout because a release "
            "build has no automation surface (convention 15); a config-conditional "
            "step makes that substitution dishonest — the installed subject would no "
            "longer be what a user gets."
        )
    # And install.sh really does read from beside itself — the seam this module
    # stages through.
    install = (packaging / "install.sh").read_text()
    assert 'BINARY="$SCRIPT_DIR/fauna-tui"' in install and 'AGENT_BINARY="$SCRIPT_DIR/fauna-sync-agent"' in install, (
        "install.sh no longer installs the binaries beside itself — the staging "
        "seam is gone and this module is installing something other than it thinks."
    )


def test_the_installer_reads_the_environment_this_module_hands_it():
    """The isolation and the prefix, asserted on **what the child reads** — the
    linux leg's `BASH_ENV` lesson (a login rc re-sourced after the passed
    environment silently replaced it, and a dict-side assertion called that green)."""
    repo = Path(get_repo_root())
    seen = child_sees(repo, ("XDG_CONFIG_HOME", "PREFIX", "BASH_ENV"))

    assert seen.get("BASH_ENV") == "", (
        f"the child inherited BASH_ENV={seen.get('BASH_ENV')!r}; bash sources it "
        f"after the passed environment, so `_isolated_env` must keep dropping it."
    )
    xdg = seen.get("XDG_CONFIG_HOME", "")
    assert xdg and Path(xdg).is_relative_to(repo) and Path(xdg) != Path.home() / ".config", (
        f"XDG_CONFIG_HOME is {xdg!r} — an `install.sh --uninstall` would reach the "
        f"developer's REAL systemd user session (convention 10)."
    )
    assert seen.get("PREFIX") == str(install_prefix(repo)), (
        f"the child reads PREFIX={seen.get('PREFIX')!r}; `install.sh` would install "
        f"somewhere other than where this module looks for the result."
    )
    # And the script resolves the unit through XDG_CONFIG_HOME, so the redirect binds.
    script = (repo / "apps" / "fauna-tui" / "packaging" / "install.sh").read_text()
    assert "${XDG_CONFIG_HOME:-$HOME/.config}" in script, (
        "install.sh no longer resolves the agent unit through XDG_CONFIG_HOME, so "
        "redirecting it no longer isolates the uninstall."
    )

"""The INSTALLED linux product — one definition of where it lives and how it got there.

Imported by both the root ``conftest.py`` (which points the linux driver at the
installed binary) and ``tests/artifact/test_linux_installed_product.py`` (which
performs the install and asserts on it). Two copies of a path like this drift
silently, and the failure would be a suite driving a stale install — the same
reasoning ``helpers/macos_artifact.py`` gives for its single
``ARTIFACT_APP_RELPATH``.

**What this exists for.** Every other linux e2e drives ``target/debug/fauna-desktop``
straight out of the build tree. Nothing installs the app the way a user does and
then *drives what got installed*, so everything the install channel decides —
where the binary lands, whether the sync agent lands beside it (which is how
``sync_agent.rs`` ``agent_binary_absolute`` finds it at all), whether the desktop
entry and icon are written under the app-id basename — is asserted as a file
layout and never as a running app. That gap is what
``docs/features/get-the-app.md`` outcome 4 reads as ``(none)``: the linux suites
prove the package's *structure*, and the outcome is written "the packaged app
installs **and launches**". macOS and Windows already have install-then-drive
tests; this is the same shape for linux.

**Channel: `install.sh` with a user prefix.** The five linux channels are
``install.sh``, Snap, Flatpak, the Debian package and the AppImage
(``installers/linux-desktop.md`` § Distribution Methods). ``install.sh`` is the
one an unprivileged session can drive end to end — it selects ``$HOME/.local``
when not root, and honours ``$PREFIX`` — and it installs exactly the file set
every other channel installs, to the same relative paths under its prefix
(§ Installation Files). ``dpkg -i`` needs root, which the primary linux dev
machine does not have; a root-gated test would skip there, and a skip is not
coverage (convention 7).

**DEBUG binaries, and it must be** — the same substitution ``macos_artifact.py``
makes and for the same reason: convention 15 compiles the automation surface out
of release artifacts (``apps/fauna-linux/src/main.rs``: ``mod automation`` is
``#[cfg(any(debug_assertions, feature = "e2e-agent"))]``), so the release binary
``install.sh`` reads by default carries no in-process agent and cannot be driven
at all. The substitution is honest because ``install.sh`` branches on nothing:
it resolves two paths under ``$CARGO_TARGET_DIR/release`` and ``install -Dm755``
them, and every step after that — the desktop entry, the icon, the cache
refreshes — reads the repo, not the build config.
``test_install_sh_branches_on_no_build_configuration`` pins that by reading the
script, so the day someone adds a release-only step this stops being honest and
says so.

The staging directory is what makes the substitution possible without editing a
shipped installer: ``install.sh`` reads ``$CARGO_TARGET_DIR/release/<name>``, so
pointing ``CARGO_TARGET_DIR`` at a scratch tree holding copies of the debug
binaries hands it the subject we want through the seam it already has. No
test-only knob is added to a shipped script, and the real
``$CARGO_TARGET_DIR/release`` is never written to.

**Isolation.** ``install.sh --uninstall`` runs ``systemctl --user disable --now
fauna-sync-agent.service`` and deletes
``${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/fauna-sync-agent.service`` — that
is deliberate (it is the one channel that runs as the user and so can remove the
user's unit, ``installers/linux-desktop.md`` § Uninstall), and against an
unredirected environment it would reach into the developer's real desktop
session, which convention 10 bans outright. Every invocation here therefore
carries a throwaway ``XDG_CONFIG_HOME``. The prefix lives under this
repository copy's own untracked ``build/``, so an install here is invisible to
every other copy on the box — unlike the windows analogue, which takes over
``%ProgramFiles%\\Fauna`` and needs an exclusive box.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

#: Where the installed product lives, relative to the repo root — untracked, and
#: private to this repository copy. Deliberately NOT ``~/.local``, which is the
#: developer's own prefix and may hold an install they put there themselves.
INSTALL_PREFIX_RELPATH = "build/linux-installed-product"

#: Where the debug binaries are staged under the name ``install.sh`` reads them by.
_STAGING_RELPATH = "build/linux-installed-product-staging"

#: The two binaries ``install.sh`` copies out of ``$CARGO_TARGET_DIR/release``.
_STAGED_BINARIES = ("fauna-desktop", "fauna-sync-agent")


def install_prefix(repo_root: Path | str) -> Path:
    """The prefix ``install.sh`` is pointed at."""
    return Path(repo_root) / INSTALL_PREFIX_RELPATH


def installed_app_path(repo_root: Path | str) -> Path:
    """The installed ``fauna-desktop``, i.e. what the driver launches."""
    return install_prefix(repo_root) / "bin" / "fauna-desktop"


def installed_agent_path(repo_root: Path | str) -> Path:
    """The installed ``fauna-sync-agent`` — the one that must sit BESIDE the app.

    ``sync_agent.rs`` ``agent_binary_absolute()`` resolves the agent beside the
    app binary or on ``PATH``; if the install channel drops it, the app logs
    "fauna-sync-agent not found beside the app or on PATH" and file sync never
    starts for everyone who installed that way.
    """
    return install_prefix(repo_root) / "bin" / "fauna-sync-agent"


def _isolated_env(repo_root: Path | str, staging: Path, prefix: Path) -> dict:
    """The environment every ``install.sh`` invocation runs under.

    ``XDG_CONFIG_HOME`` is the load-bearing one — see the module docstring's
    *Isolation* note. Without it, an uninstall reaches the real session's
    systemd user manager.

    ⚠ **``BASH_ENV`` is dropped, and that is not tidiness — without it none of
    the rest of this dict reaches the script.** A dev machine here exports
    ``BASH_ENV=$HOME/.bashrc``, and bash sources ``$BASH_ENV`` at the start of
    every NON-interactive shell — i.e. after the environment the caller passed
    is already in place, so anything that file exports *overwrites* it. That rc
    recomputes and exports ``CARGO_TARGET_DIR`` from the working directory, so
    ``install.sh`` read the real build tree's ``release/`` and failed with
    "release binaries not found" while this dict said otherwise (measured
    2026-08-27, the first run of this module). Passing ``env=`` to a
    ``bash script.sh`` subprocess is therefore NOT sufficient on this box for
    any variable a login rc exports; the rc has to be kept out.

    Dropping it is also the more faithful subject: a user running
    ``./install.sh`` has no rc pointing at a fauna build directory, so an
    installer configured by the developer's shell was never what this module
    means to test.
    """
    env = os.environ.copy()
    env.pop("BASH_ENV", None)
    env["CARGO_TARGET_DIR"] = str(staging)
    env["PREFIX"] = str(prefix)
    env["XDG_CONFIG_HOME"] = str(Path(repo_root) / _STAGING_RELPATH / "xdg-config")
    Path(env["XDG_CONFIG_HOME"]).mkdir(parents=True, exist_ok=True)
    return env


def child_sees(repo_root: Path | str, names: tuple[str, ...]) -> dict:
    """What a bash child launched through :func:`_isolated_env` ACTUALLY reads.

    The only honest way to assert this module's isolation. Reading the dict
    `_isolated_env` returns proves nothing: bash re-sources ``$BASH_ENV`` after
    the passed environment is installed, so a variable can be correct in the
    dict and wrong in the script — which is exactly what happened, and what a
    dict-side assertion happily called green.
    """
    repo_root = Path(repo_root)
    env = _isolated_env(repo_root, repo_root / _STAGING_RELPATH, install_prefix(repo_root))
    script = "; ".join(f'echo "{n}=${{{n}-}}"' for n in names)
    proc = subprocess.run(
        ["bash", "-c", script],
        capture_output=True, text=True, timeout=60, cwd=str(repo_root), env=env,
    )
    seen = {}
    for line in proc.stdout.splitlines():
        key, _, value = line.partition("=")
        seen[key] = value
    return seen


def stage_debug_binaries(repo_root: Path | str, debug_dir: Path | str) -> Path:
    """Copy the built DEBUG binaries into a scratch ``release/`` tree.

    Returns the staging root, i.e. what ``CARGO_TARGET_DIR`` must be set to.
    Raises rather than skipping when a binary is absent: the caller has already
    run the build, so a missing file is a broken build, not an unbuilt tree, and
    a skip there would read as "linux install coverage ran".
    """
    repo_root = Path(repo_root)
    debug_dir = Path(debug_dir)
    staging = repo_root / _STAGING_RELPATH / "release"
    staging.mkdir(parents=True, exist_ok=True)
    for name in _STAGED_BINARIES:
        src = debug_dir / name
        if not src.is_file():
            raise RuntimeError(
                f"{src} does not exist, so `install.sh` would have nothing to "
                f"install. `just linux-debug` builds both {_STAGED_BINARIES}."
            )
        dst = staging / name
        shutil.copy2(src, dst)
        dst.chmod(0o755)
    return staging.parent


def run_install(repo_root: Path | str, debug_dir: Path | str) -> subprocess.CompletedProcess:
    """Stage the debug binaries and run the real ``install.sh``."""
    repo_root = Path(repo_root)
    staging = stage_debug_binaries(repo_root, debug_dir)
    prefix = install_prefix(repo_root)
    prefix.mkdir(parents=True, exist_ok=True)
    return subprocess.run(
        ["bash", str(repo_root / "apps" / "fauna-linux" / "install.sh")],
        capture_output=True,
        text=True,
        timeout=120,
        cwd=str(repo_root),
        env=_isolated_env(repo_root, staging, prefix),
    )


def run_uninstall(repo_root: Path | str) -> subprocess.CompletedProcess:
    """Run the real ``install.sh --uninstall`` against the same prefix."""
    repo_root = Path(repo_root)
    staging = repo_root / _STAGING_RELPATH
    prefix = install_prefix(repo_root)
    return subprocess.run(
        ["bash", str(repo_root / "apps" / "fauna-linux" / "install.sh"), "--uninstall"],
        capture_output=True,
        text=True,
        timeout=120,
        cwd=str(repo_root),
        env=_isolated_env(repo_root, staging, prefix),
    )

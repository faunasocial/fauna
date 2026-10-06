"""The INSTALLED terminal app — one definition of where it lives and how it got there.

Imported by both the root ``conftest.py`` (which points the tui driver at the
installed binary, ``_installed_tui_app_override``) and
``tests/artifact/test_tui_installed_product.py`` (which performs the install and
asserts on it). The linux twin, ``helpers/linux_installed_product.py``, says why
one module and not two: two copies of a path drift silently, and the failure is
a suite driving a stale install.

**The channel.** The terminal app ships as a per-OS archive — ``fauna-tui`` +
``fauna-sync-agent`` + ``install.sh`` in one directory, built by
``apps/fauna-tui/packaging/assemble-archive.sh`` for the release workflow
(``installers/tui.md`` § The ratified channel). The user unpacks it and runs its
``install.sh``, which copies the two binaries into ``$PREFIX/bin``. That is the
exact path this module walks: the REAL ``assemble-archive.sh`` stages the archive
layout, and the REAL ``install.sh`` inside it installs. Nothing test-only is
added to either shipped script.

**DEBUG binaries, and it must be** — the linux leg's substitution, for its
reason: convention 15 compiles the automation surface out of release builds
(``apps/fauna-tui/src/main.rs``: the agent and every ``FAUNA_E2E_*`` read are
``#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]``), so a release
``fauna-tui`` cannot be driven at all. The substitution is honest because
``assemble-archive.sh`` reads nothing but the two files in ``--bin-dir`` and
``install.sh`` installs from beside itself: neither branches on a build
configuration. ``test_install_scripts_branch_on_no_build_configuration`` pins
that by reading both scripts.

**Isolation.** ``install.sh --uninstall`` removes the agent's systemd user unit
when the desktop app is not installed in the same prefix (the shared
``agent_spawner`` writes ``fauna-sync-agent.service`` at first post-auth), so
every invocation carries a throwaway ``XDG_CONFIG_HOME`` under this repository
copy's untracked ``build/`` — convention 10, the linux helper's rule. The prefix
lives there too, so an install here is invisible to every other copy on the box.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

#: Where the installed product lives, relative to the repo root — untracked and
#: private to this repository copy. Never ``~/.local``, the developer's own prefix.
INSTALL_PREFIX_RELPATH = "build/tui-installed-product"

#: Where the archive layout is staged (``assemble-archive.sh --out``), and where
#: the throwaway ``XDG_CONFIG_HOME`` lives.
_STAGING_RELPATH = "build/tui-installed-product-staging"

#: The target suffix the staged archive is named by. Any suffix would do — the
#: layout does not depend on it — but naming this box's own keeps the staged
#: directory reading like a real download.
_SUFFIX = "e2e-local"

#: The two binaries the archive carries and ``install.sh`` installs.
STAGED_BINARIES = ("fauna-tui", "fauna-sync-agent")


def install_prefix(repo_root: Path | str) -> Path:
    """The prefix ``install.sh`` is pointed at."""
    return Path(repo_root) / INSTALL_PREFIX_RELPATH


def installed_app_path(repo_root: Path | str) -> Path:
    """The installed ``fauna-tui`` — what the driver launches."""
    return install_prefix(repo_root) / "bin" / "fauna-tui"


def installed_agent_path(repo_root: Path | str) -> Path:
    """The installed ``fauna-sync-agent`` — the one that must sit BESIDE the app.

    ``agent_spawner::agent_binary_absolute`` resolves the agent beside the app
    binary, then on ``PATH``; a channel that drops it leaves the Folders and
    Devices pages unable to provision sync for everyone who installed that way.
    """
    return install_prefix(repo_root) / "bin" / "fauna-sync-agent"


def staged_archive_dir(repo_root: Path | str) -> Path:
    """The unpacked-archive layout ``assemble-archive.sh`` staged."""
    return Path(repo_root) / _STAGING_RELPATH / f"fauna-tui-{_SUFFIX}"


def _isolated_env(repo_root: Path | str, prefix: Path) -> dict:
    """The environment every ``install.sh`` invocation runs under.

    ``BASH_ENV`` is dropped for the linux helper's measured reason: bash sources
    it at the start of every non-interactive shell, AFTER the passed environment
    is in place, so a login rc could overwrite anything here. ``XDG_CONFIG_HOME``
    is the isolation (module docstring); ``PREFIX`` is the seam ``install.sh``
    already has.
    """
    env = os.environ.copy()
    env.pop("BASH_ENV", None)
    env["PREFIX"] = str(prefix)
    env["XDG_CONFIG_HOME"] = str(Path(repo_root) / _STAGING_RELPATH / "xdg-config")
    Path(env["XDG_CONFIG_HOME"]).mkdir(parents=True, exist_ok=True)
    return env


def child_sees(repo_root: Path | str, names: tuple[str, ...]) -> dict:
    """What a bash child launched through :func:`_isolated_env` ACTUALLY reads."""
    repo_root = Path(repo_root)
    env = _isolated_env(repo_root, install_prefix(repo_root))
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


def stage_archive(repo_root: Path | str, debug_dir: Path | str) -> Path:
    """Run the real ``assemble-archive.sh`` over the built DEBUG binaries.

    Returns the staged archive directory (the unpacked-archive layout). Raises
    rather than skipping when a binary is absent: the caller has already run the
    build, so a missing file is a broken build, and a skip would read as "tui
    install coverage ran".
    """
    repo_root = Path(repo_root)
    debug_dir = Path(debug_dir)
    for name in STAGED_BINARIES:
        if not (debug_dir / name).is_file():
            raise RuntimeError(
                f"{debug_dir / name} does not exist, so the archive would have "
                f"nothing to carry. `just tui-debug` builds both {STAGED_BINARIES}."
            )
    out = repo_root / _STAGING_RELPATH
    out.mkdir(parents=True, exist_ok=True)
    proc = subprocess.run(
        [
            "bash",
            str(repo_root / "apps" / "fauna-tui" / "packaging" / "assemble-archive.sh"),
            "--bin-dir", str(debug_dir),
            "--out", str(out),
            "--suffix", _SUFFIX,
            "--no-tar",
        ],
        capture_output=True, text=True, timeout=120, cwd=str(repo_root),
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"assemble-archive.sh failed (rc={proc.returncode}):\n{proc.stdout}\n{proc.stderr}"
        )
    return staged_archive_dir(repo_root)


def run_install(repo_root: Path | str, debug_dir: Path | str) -> subprocess.CompletedProcess:
    """Stage the archive layout and run ITS ``install.sh``."""
    repo_root = Path(repo_root)
    staged = stage_archive(repo_root, debug_dir)
    prefix = install_prefix(repo_root)
    prefix.mkdir(parents=True, exist_ok=True)
    return subprocess.run(
        ["bash", str(staged / "install.sh")],
        capture_output=True, text=True, timeout=120, cwd=str(repo_root),
        env=_isolated_env(repo_root, prefix),
    )


def run_uninstall(repo_root: Path | str) -> subprocess.CompletedProcess:
    """Run the staged archive's ``install.sh --uninstall`` against the same prefix."""
    repo_root = Path(repo_root)
    return subprocess.run(
        ["bash", str(staged_archive_dir(repo_root) / "install.sh"), "--uninstall"],
        capture_output=True, text=True, timeout=120, cwd=str(repo_root),
        env=_isolated_env(repo_root, install_prefix(repo_root)),
    )

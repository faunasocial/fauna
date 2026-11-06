"""The windows MSI's cache and its staged ffi — what the collection-time prebuild
and `tests/platform/windows/test_installer.py` must agree on.

The MSI is that suite's subject, so its publish and its WiX link stay in the
fixture; every input is a prebuild (e2e-conventions.md convention 9, the
2026-10-30 ruling). So the prebuild has to answer, before any test runs, the
question the fixture answers for itself: will this run build the MSI? One
definition serves both, or a job would run for an MSI the fixture then finds
cached, or the fixture would miss on an input no job built.
"""

from __future__ import annotations

import glob
import os
import subprocess
import sys

#: The staged flavor the installer's publish bundles: the test flavor at the
#: `release` profile. The installed product is driven through the e2e agent, so
#: the publish compiles the automation surface into a Release build
#: (`-p:FaunaE2eAgent=true`), and that surface calls the `*ForTest` UniFFI seams
#: only this flavor's bindings export (testing.md convention 15). windows' twin
#: of a linux `--release --features e2e-agent` build; the SHIPPED MSI
#: (release.yml) sets neither.
RELEASE_TEST_FLAVOR = "win-arm64:release:test-helpers"

SOURCE_PATTERNS = [
    "apps/fauna-windows/installer/*.wxs",
    "apps/fauna-windows/*/Cargo.toml",
    "apps/fauna-windows/*/src/**/*.rs",
    "apps/fauna-windows/FaunaApp/FaunaApp/*.csproj",
    "apps/fauna-windows/FaunaApp/**/*.cs",
    "apps/fauna-windows/FaunaApp/**/*.xaml",
    "apps/fauna-windows/shell-ext/src/icons/*.ico",
    # The Go MDA the Bridge feature ships (built by `just windows-mail-bridge-build`)
    # + the gnullvm fauna_ffi.dll it links, so a change to either rebuilds the MSI.
    "bins/fauna-bridges/**/*.go",
    "libs/fauna-ffi/src/**/*.rs",
    # The terminal app the TerminalApp feature ships, and the cli app's device
    # shell beside it (the catalog crate is its whole surface).
    "apps/fauna-tui/src/**/*.rs",
    "apps/fauna-cli/src/**/*.rs",
    "libs/fauna-cli/src/**/*.rs",
    # The Sync feature's agent and the nest the NestService links: every staged
    # binary's own crate is an input, or an edit to it reads the cached MSI fresh.
    "bins/fauna-sync-agent/Cargo.toml",
    "bins/fauna-sync-agent/src/**/*.rs",
    "bins/fauna-nest/Cargo.toml",
    "bins/fauna-nest/src/**/*.rs",
    # Where each staged binary's features are declared (the agent's `e2e-agent`
    # among them): a flavour change is an input like a source edit.
    "tests/e2e-unified/helpers/helper_bins.py",
]


def is_admin() -> bool:
    """Whether this process is elevated — the installer suite's skip condition."""
    if sys.platform != "win32":
        return False
    import ctypes

    return ctypes.windll.shell32.IsUserAnAdmin() != 0


def _git_ignored(paths):
    """The subset of `paths` git ignores — this tree's one definition of build output."""
    if not paths:
        return set()
    # -z: NUL-separated in and out. Without it git C-quotes every path holding a
    # backslash ("D:\\src\\…"), so no Windows path would ever match.
    r = subprocess.run(
        ["git", "check-ignore", "-z", "--stdin"], input="\0".join(paths) + "\0",
        capture_output=True, text=True, cwd=os.path.dirname(os.path.abspath(__file__)),
    )
    return {os.path.normcase(os.path.normpath(p)) for p in r.stdout.split("\0") if p}


def max_mtime(patterns):
    """Return the maximum mtime across all files matching the glob patterns.

    Gitignored files are build OUTPUT and never count as input: `bin/`/`obj/`
    (the WinUI publish writes generated *.cs there), `Generated/uniffi/` and the
    test fakes the ffi job regenerates. All of these are written AFTER `msi_path`
    takes the stamp, so counting them made its cache miss on every run following
    a build. (Tracked generated files such as `Generated/UiIds.cs` stay inputs.)
    """
    paths = [p for pattern in patterns for p in glob.glob(pattern, recursive=True)]
    ignored = _git_ignored(paths)
    best = 0.0
    for path in paths:
        if os.path.normcase(os.path.normpath(path)) in ignored:
            continue
        try:
            best = max(best, os.path.getmtime(path))
        except OSError:
            pass
    return best


def msi_paths(repo: str) -> tuple[str, str]:
    """(the MSI, its freshness stamp)."""
    build_dir = os.path.join(repo, "build", "installer")
    return (
        os.path.join(build_dir, "Fauna-Setup-arm64.msi"),
        os.path.join(build_dir, "build.stamp"),
    )


def source_mtime(repo: str) -> float:
    """The newest of the MSI's tracked inputs — what its stamp records."""
    return max_mtime([os.path.join(repo, p) for p in SOURCE_PATTERNS])


def msi_is_cached(repo: str, current_mtime: float) -> bool:
    msi, stamp = msi_paths(repo)
    if not (os.path.exists(msi) and os.path.exists(stamp)):
        return False
    try:
        with open(stamp) as f:
            return float(f.read().strip()) >= current_mtime
    except (ValueError, OSError):
        return False


def msi_build_owed(repo: str) -> bool:
    """Whether a run collecting the installer suite will build the MSI: elevated
    (unelevated its items stay collected, every one skip-marked, so no test
    reads the MSI) and past a stale stamp (else `msi_path` returns the cached
    one, needing no input at all)."""
    return is_admin() and not msi_is_cached(repo, source_mtime(repo))


def release_ffi_dll(repo: str) -> str:
    """The flavor-private dll the publish bundles a copy of, never the shared
    `target/release/fauna_ffi.dll` every host build of fauna-ffi writes
    (build-target-layout-windows.md § Cargo target dir layout (win))."""
    return os.path.join(
        repo, "target", "release", "windows-ffi", "win-arm64", "test-helpers", "fauna_ffi.dll",
    )


def require_release_test_flavor(repo: str) -> None:
    """Refuse a publish of any other staged flavor. The WinUI project consumes one
    fixed pair of paths whatever the flavor, so a wrong one compiles, packages
    and installs without a word: the dev dll in a Release app, or bindings the
    e2e agent cannot compile against."""
    marker = os.path.join(repo, "apps", "fauna-windows", ".ffi-flavor")
    try:
        with open(marker, encoding="utf-8") as f:
            staged = f.read().strip()
    except OSError as exc:
        staged = f"unreadable ({exc})"
    if staged != RELEASE_TEST_FLAVOR:
        raise RuntimeError(
            f"the staged ffi flavor is {staged!r}, not {RELEASE_TEST_FLAVOR!r} "
            f"({marker}): the collection-time `just windows-ffi-test release` job "
            "stages it last, so a build or a recipe that ran after it restaged "
            "another flavor. Publishing now would package the wrong core."
        )

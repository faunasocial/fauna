"""Client-artifact category (opt-in: ``--client-artifact``, or ``--macos-artifact``).

Tests in this directory take the **shipped client artifact** as their subject —
the real ``Fauna.app`` bundle, a DMG-installed copy of it, the iOS archive, the
linux app as ``install.sh`` installs it — rather than the locally-built bare
executable every other app suite drives.
That is the client-side analogue of tier_4's nest arm: *"only a real deployment
artifact under real supervision catches image/packaging/supervision bugs the
binaries hide"* (``docs/goal/architecture/testing.md`` § The four-tier taxonomy).
On Apple platforms packaging is precisely where runtime behaviour changes —
bundle identity, entitlements, resource resolution, embedded frameworks and
their rpaths, quarantine — and none of it exists for a bare Mach-O.

**Why the default path is NOT this.** The macOS driver launches the bare
swift-build binary deliberately, and the reason is a mine worth not re-treading:
a bundle launched under the app's FIXED ``social.fauna.fauna`` identity
reliably stops getting a WindowServer-backed window after enough
launch-and-die cycles — process alive, ``/health`` 200, zero windows, empty
registry, every element 404 — the fleet-wide "render-death" once misdiagnosed as
a VM needing a reboot (``apple-e2e-automation.md`` § Registration rules rule 9).
The root cause was the fixed **id**, not the bundle: the bare binary's ad-hoc
identity is per-build, so no two launches ever share one Launch Services
identity. This suite restores that property explicitly — every launch stages a
private copy of the bundle under a per-instance ``CFBundleIdentifier`` — which is
what makes an artifact launch concurrency-safe beside a sibling session.

**Not one OS.** ``testing.md`` § The four-tier taxonomy point 4 names this
directory as the home of the shipped-client-artifact arm for the *apps*, not for
apple; the apple modules were simply the first written (2026-08-09). Each module
declares which OS its artifact belongs to in ``_MODULE_OS`` below, and a module
whose OS this box is not is pruned at collection — so ``--client-artifact`` on
linux collects the linux module and nothing else, exactly as it collects only the
apple ones on macOS.

Two gates, both mandatory:

1. **Collection**: ``pytest_ignore_collect`` prunes this directory unless
   ``--client-artifact`` (or its apple-flavoured superset ``--macos-artifact``)
   is passed, so no default sweep, tier filter, or ``just`` recipe can even
   import these files — and then prunes each module whose OS is not this box's.
2. **Platform**: the guard below refuses loudly rather than skipping into a
   vacuous green, for any module that survives gate 1 on the wrong OS.

**Cost** — the reason for gate 1. The bundle build is a full ``xcodebuild`` over
``Fauna.xcodeproj`` (the app plus its two embedded extensions) + ``cargo build -p
fauna-sync-agent`` + codesign
(minutes cold, seconds warm); the DMG module adds an ``hdiutil`` create/attach
round trip; the iOS module adds an ``xcodebuild archive``. The linux module is
much cheaper — a file-copy install over already-built debug binaries — but it is
in the same category for the same reason: its subject is what an install channel
produced, not what the build tree holds. Never the inner loop: run it with
``just e2e-macos-artifact-test`` / ``just e2e-linux-artifact-test``, as a
scheduled or pre-release sweep.
"""

import platform
import subprocess
import sys
from pathlib import Path

import pytest

from common import get_repo_root
from helpers.macos_artifact import ARTIFACT_APP_RELPATH

#: Which OS each module's artifact belongs to — the ``sys.platform`` prefixes the
#: box must match one of. Explicit rather than inferred from the filename: a
#: category that silently ran nothing because a module was renamed would be the
#: vacuous green convention 7 bans, and this map makes the omission a review
#: question instead. A module absent from the map is collected everywhere.
_MODULE_OS = {
    "test_macos_app_bundle.py": ("darwin",),
    "test_macos_dmg_install.py": ("darwin",),
    "test_ios_archive.py": ("darwin",),
    "test_linux_installed_product.py": ("linux",),
    # The terminal app's archive ships an `install.sh` for the two unix OSes;
    # on windows the zip is unpacked by hand (installers/tui.md).
    "test_tui_installed_product.py": ("linux", "darwin"),
}


def _runs_here(required: tuple[str, ...] | None) -> bool:
    return required is None or any(sys.platform.startswith(p) for p in required)


def pytest_ignore_collect(collection_path, config):
    """Prune the modules whose artifact belongs to another OS.

    The root conftest already prunes this whole directory without the opt-in
    flag; this narrows what survives to the modules this box can actually drive.
    Pruning rather than skipping, for gate 1's reason: a skip still imports the
    module, and importing an apple artifact module on linux would need every one
    of its apple-only imports to exist there.
    """
    required = _MODULE_OS.get(Path(str(collection_path)).name)
    return not _runs_here(required)


def _fail(msg: str) -> None:
    pytest.fail(msg, pytrace=False)


@pytest.fixture(autouse=True)
def _require_artifact_platform(request) -> None:
    """Belt-and-braces for gate 2: this module's artifact needs this box's OS.

    A skip would be wrong: the directory is already opt-in, so reaching it at all
    is an explicit request to test the artifact. Answering that request with a
    silent `s` is the failure mode convention 7 exists to prevent. In practice
    the collection prune above means this never fires — it is here so that a
    module added to the directory and forgotten in ``_MODULE_OS`` still cannot
    quietly run against the wrong OS's artifact.
    """
    required = _MODULE_OS.get(Path(str(request.node.path)).name)
    if not _runs_here(required):
        _fail(
            f"{Path(str(request.node.path)).name} drives the {'/'.join(required)} "
            f"client artifact; this box is {sys.platform} ({platform.system()})."
        )


@pytest.fixture(scope="session")
def _require_macos() -> None:
    """The apple modules' own platform assertion, requested by their fixtures.

    Kept as a distinct fixture (rather than folded into
    ``_require_artifact_platform``) because ``macos_artifact_bundle`` depends on
    it to order the check ahead of a minutes-long bundle build.
    """
    if platform.system() != "Darwin":
        _fail(
            f"this fixture drives the macOS client artifact and needs macOS; "
            f"this box is {platform.system()}. Run it on macOS."
        )


@pytest.fixture(scope="session")
def macos_artifact_bundle(_require_macos) -> Path:
    """The built ``build/Debug/Fauna.app``, assembled if absent.

    Built through ``just`` rather than a hand-rolled bundle assembly so the
    build-if-stale gates and generated-file deps fire (a warm re-run is seconds)
    and so this suite tests the recipe the release channels actually use. The
    recipe self-slots via ``{{slot_build}}`` — do NOT wrap it in another
    build-slot acquisition.
    """
    repo = get_repo_root()
    proc = subprocess.run(
        ["just", "mac-app", "debug"],
        cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    if proc.returncode != 0:
        _fail(
            f"`just mac-app debug` failed (rc={proc.returncode}) — the artifact "
            f"under test could not be built.\ntail:\n{proc.stdout[-3000:]}"
        )
    bundle = repo / ARTIFACT_APP_RELPATH
    if not bundle.is_dir():
        _fail(f"`just mac-app debug` reported success but {bundle} does not exist")
    return bundle


@pytest.fixture(scope="session")
def artifact_scratch(tmp_path_factory) -> Path:
    """A session-scoped scratch dir for staged bundles, DMGs and mount copies.

    Under `/tmp`, not the repo: a DMG plus a mounted copy of a ~570 MB debug
    bundle is real disk, and macOS runs chronically close to full (a full build
    has zero-filled binaries and damaged sibling sessions before now).
    """
    return tmp_path_factory.mktemp("macos-artifact")

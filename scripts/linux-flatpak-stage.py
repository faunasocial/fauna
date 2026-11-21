"""Stage the Linux desktop Flatpak's payload — the payload's one definition.

The Flatpak build packages, it never compiles (`docs/goal/architecture/installers/
linux-desktop.md` § 3 Flatpak): the manifest's one module installs, from the
directory this script fills, the two binaries a cargo build made outside the
Flatpak build, the desktop entry, the metainfo, the icon, Fauna's two licence
texts and the generated notices. Every `just linux-flatpak*` recipe calls it
over the binaries `just linux-release <target>` built; the real-session seam
test calls it over the automation-feature binaries it builds itself, into the
same directory, so both build the one shipped manifest.

The layout is flat: each file under its own name, which is the name the
manifest's `install` lines read (`PAYLOAD` is pinned against them by
`tests/e2e-unified/tests/test_flatpak_manifest_packages_only.py`). The output
directory is emptied first, so a file from an earlier staging never rides into
a bundle. A missing input fails the staging, naming it.

Usage: linux-flatpak-stage.py --bin-dir <dir> [--notices <file>] [--out <dir>]
"""

import argparse
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The directory the manifest's `dir` source names (relative to the repo root;
# gitignored under build/).
DEFAULT_OUT = "build/flatpak-staging"
# What `just linux-desktop-notices` writes.
DEFAULT_NOTICES = "build/shipped-notices/linux-desktop/THIRD-PARTY-NOTICES"

BINARIES = ("fauna-desktop", "fauna-sync-agent")
# Staged name → source, relative to the repo root. The two binaries and the
# notices file come from the arguments.
REPO_FILES = {
    "social.fauna.fauna.desktop": "apps/fauna-linux/packaging/social.fauna.fauna.desktop",
    "social.fauna.fauna.metainfo.xml": "apps/fauna-linux/packaging/social.fauna.fauna.metainfo.xml",
    "fauna.svg": "apps/fauna-linux/fauna.svg",
    "LICENSE-MIT": "LICENSE-MIT",
    "LICENSE-APACHE": "LICENSE-APACHE",
}
NOTICES_NAME = "THIRD-PARTY-NOTICES"

PAYLOAD = (*BINARIES, *REPO_FILES, NOTICES_NAME)


def stage(bin_dir: Path, notices: Path, out: Path) -> None:
    sources = {name: bin_dir / name for name in BINARIES}
    sources.update({name: REPO / rel for name, rel in REPO_FILES.items()})
    sources[NOTICES_NAME] = notices
    missing = [f"{name} ({src})" for name, src in sources.items() if not src.is_file()]
    if missing:
        sys.exit("linux-flatpak-stage: missing payload: " + ", ".join(missing))
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    for name, src in sources.items():
        shutil.copy2(src, out / name)
        mode = 0o755 if name in BINARIES else 0o644
        (out / name).chmod(mode)
    print(f"linux-flatpak-stage: staged {len(sources)} files into {out}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bin-dir", required=True, type=Path,
                    help="the directory holding fauna-desktop and fauna-sync-agent")
    ap.add_argument("--notices", type=Path, default=REPO / DEFAULT_NOTICES)
    ap.add_argument("--out", type=Path, default=REPO / DEFAULT_OUT)
    args = ap.parse_args()
    stage(args.bin_dir, args.notices, args.out)


if __name__ == "__main__":
    main()

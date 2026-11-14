#!/usr/bin/env python3
"""Does a built Android package carry the three licence files in its assets?

    uv run --no-project python scripts/android-package-notices.py <package.apk|package.aab>

Owner: docs/goal/architecture/shipped-notices.md § Per channel (the Android
row): every Android package carries LICENSE-MIT, LICENSE-APACHE and
THIRD-PARTY-NOTICES in its `assets/` (an App Bundle's base module holds them at
`base/assets/`). This reads the package's ENTRY LIST only — never a file's
text: the notices file is dependency-authored (release-integrity.md
§ Reviewing untrusted source without being subverted). Each shipping recipe
runs it after Gradle packages, so a package without them fails its build.

Standard library only. Exit 0 = all three present; 1 = one or more missing;
2 = usage or an unreadable package.
"""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path

FILES = ("LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES")


def missing(names: list[str], bundle: bool) -> list[str]:
    prefix = "base/assets/" if bundle else "assets/"
    present = {n for n in names if not n.endswith("/")}
    return [prefix + f for f in FILES if prefix + f not in present]


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__.split("\n\n")[1].strip(), file=sys.stderr)
        return 2
    pkg = Path(argv[0])
    try:
        with zipfile.ZipFile(pkg) as z:
            names = z.namelist()
    except (OSError, zipfile.BadZipFile) as e:
        print(f"android-package-notices: FAILED — cannot read {pkg}: {type(e).__name__}")
        return 2
    gone = missing(names, pkg.suffix == ".aab")
    if gone:
        print(f"android-package-notices: FAILED — {pkg} lacks {', '.join(gone)}")
        return 1
    print(f"android-package-notices: OK — {pkg} carries {', '.join(FILES)} in its assets")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

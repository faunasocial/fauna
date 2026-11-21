"""The package-identity publishers, read from their one build-time home.

`apps/fauna-windows/installer/PackageIdentity.props` holds every build's DEV
publisher DN (see its header for why there is exactly one copy). This module is how
its consumers — the store-package build script, the one packer of both full-MSIX
channels, and the e2e harness — read it, so none of them keeps a literal that
could drift.

The same file holds the direct-download package's PRODUCTION publisher
(installers/windows.md § Direct download, 5): its signing certificate's subject,
recorded before the first signed release, empty until the signer is chosen.

    python scripts/_windows_package_identity.py direct-production

prints it, or exits 1 with the refusal while it is empty — the check
`just windows-direct-package production` makes before anything builds.
"""

from __future__ import annotations

import os
import sys
import xml.etree.ElementTree as ET

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROPS = os.path.join(REPO, "apps", "fauna-windows", "installer", "PackageIdentity.props")
PROPERTY = "FaunaPackagePublisher"
DIRECT_PRODUCTION_PROPERTY = "FaunaDirectProductionPublisher"


def publisher(props: str = PROPS) -> str:
    """The default (dev) publisher DN the props file declares."""
    root = ET.parse(props).getroot()
    values = [el.text.strip() for el in root.iter(PROPERTY) if el.text and el.text.strip()]
    if len(values) != 1:
        raise ValueError(f"{props}: expected exactly one {PROPERTY}, found {values!r}")
    return values[0]


def direct_production_publisher(props: str = PROPS) -> str:
    """The direct-download package's production publisher, '' while no signer is
    chosen. The property must be declared exactly once, empty or not."""
    root = ET.parse(props).getroot()
    values = [(el.text or "").strip() for el in root.iter(DIRECT_PRODUCTION_PROPERTY)]
    if len(values) != 1:
        raise ValueError(f"{props}: expected exactly one {DIRECT_PRODUCTION_PROPERTY}, "
                         f"found {values!r}")
    return values[0]


def require_direct_production_publisher(props: str = PROPS) -> str:
    """The recorded production publisher; a SystemExit naming the property while it
    is empty, since a production direct build may not run without it."""
    value = direct_production_publisher(props)
    if not value:
        raise SystemExit(
            f"error: {DIRECT_PRODUCTION_PROPERTY} in {props} is empty: the direct-download "
            "package's production publisher is its signing certificate's subject, recorded "
            "there before the first signed release (installers/windows.md § Direct "
            "download, 5), and no signer is chosen yet. A dev build "
            "(`just windows-direct-package`) packs with the dev publisher.")
    return value


if __name__ == "__main__":
    if sys.argv[1:] != ["direct-production"]:
        sys.exit("usage: _windows_package_identity.py direct-production")
    print(require_direct_production_publisher())

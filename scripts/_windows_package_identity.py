"""The MSI channel's package-identity publisher, read from its one build-time home.

`apps/fauna-windows/installer/PackageIdentity.props` holds the DN (see its header for
why there is exactly one copy). This module is how the Python consumers — the sparse-
and store-package build scripts and the e2e harness — read it, so none of them keeps a
literal that could drift from the DN FaunaApp.exe's embedded manifest is built with.
"""

from __future__ import annotations

import os
import xml.etree.ElementTree as ET

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROPS = os.path.join(REPO, "apps", "fauna-windows", "installer", "PackageIdentity.props")
PROPERTY = "FaunaPackagePublisher"


def publisher(props: str = PROPS) -> str:
    """The default (dev) publisher DN the props file declares."""
    root = ET.parse(props).getroot()
    values = [el.text.strip() for el in root.iter(PROPERTY) if el.text and el.text.strip()]
    if len(values) != 1:
        raise ValueError(f"{props}: expected exactly one {PROPERTY}, found {values!r}")
    return values[0]

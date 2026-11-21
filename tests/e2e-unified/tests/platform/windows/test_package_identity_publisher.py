"""The package-identity publishers have ONE build-time home.

Authority: `docs/goal/architecture/installers/windows.md` § Package identity for
FaunaApp.exe. A package's `Identity/@Publisher` must equal its signing certificate's
Subject DN, and the publisher feeds the package family name, so a copy that drifts is
silently another app. The dev DN every dev build packs with is kept in exactly one
place: `apps/fauna-windows/installer/PackageIdentity.props` (`FaunaPackagePublisher`).
This suite pins that every consumer reads it and that no consumer keeps its own copy:

  * the store-package build script, the one packer of both full-MSIX channels,
    defaults its publisher to it (the Store's PRODUCTION publisher is its own Partner
    Center value, passed explicitly — only its dev default is shared);
  * the e2e harness registers its dev packages under it (`helpers/taskbar_badge.py`);
  * the DN literal appears in no other file under the trees those live in.

And that the app itself names no package: its identity is the full MSIX's own, so
the embedded manifest carries no `msix` element (§ Direct download, 6, which retired
the element with the sparse package it bound the exe to).

The same props file is the home of the direct-download package's PRODUCTION publisher
(§ Direct download, 5): a second property, empty until the signer is chosen, which a
production direct build refuses to run without (pinned at the end of this file).

tier_1 and fleet-wide: pure parsing plus in-process imports; the coupling it guards
drifts on any machine.
"""

import importlib.util
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

import pytest

pytestmark = pytest.mark.tier_1

MSIX_NS = "urn:schemas-microsoft-com:msix.v1"


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    ).stdout.strip()
    if sys.platform == "win32" and out.startswith("/"):
        out = out[1].upper() + ":" + out[2:]
    return os.path.normpath(out)


REPO = _repo_root()
WINDOWS = os.path.join(REPO, "apps", "fauna-windows")
PROPS = os.path.join(WINDOWS, "installer", "PackageIdentity.props")
APP_DIR = os.path.join(WINDOWS, "FaunaApp", "FaunaApp")


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="module")
def home_dn():
    """The DN the one home declares, read straight off the props XML (not through
    the shared reader, so a bug in the reader cannot agree with itself)."""
    values = [el.text.strip() for el in ET.parse(PROPS).getroot().iter("FaunaPackagePublisher")]
    assert len(values) == 1, f"PackageIdentity.props must declare FaunaPackagePublisher once, got {values}"
    dn = values[0]
    # A plain DN in the SIGNED namespace — what both `-Register` and a signed package
    # want (installers/windows.md § Store distribution → deployment path 3).
    assert re.match(r"^CN=[^,]+", dn), f"FaunaPackagePublisher {dn!r} is not a plain Subject DN"
    return dn


def test_the_shared_reader_returns_the_home_dn(home_dn):
    reader = _load("_windows_package_identity",
                   os.path.join(REPO, "scripts", "_windows_package_identity.py"))
    assert reader.publisher() == home_dn


def test_the_embedded_app_manifest_names_no_package():
    """FaunaApp.exe embeds the checked-in `app.manifest`, which carries no `msix`
    element: the app's identity is the full MSIX's own, reaching every process
    started in the package's context, and an element naming a package would bind the
    exe to an external-location one — the sparse package's mechanism, retired with
    it. No template or generation step stands beside it to bring one back."""
    with open(os.path.join(APP_DIR, "FaunaApp.csproj"), encoding="utf-8") as f:
        csproj = f.read()
    assert "<ApplicationManifest>app.manifest</ApplicationManifest>" in csproj, (
        "FaunaApp.csproj must embed the checked-in FaunaApp/app.manifest"
    )
    assert not os.path.exists(os.path.join(APP_DIR, "app.manifest.in")), (
        "an app.manifest.in template is back beside the manifest — nothing generates one"
    )
    root = ET.parse(os.path.join(APP_DIR, "app.manifest")).getroot()
    assert root.find(f"{{{MSIX_NS}}}msix") is None, (
        "app.manifest carries an msix identity element — it binds the exe to an "
        "external-location package, which no channel ships (installers/windows.md "
        "§ Direct download, 6)"
    )


def test_the_packer_defaults_to_the_home_dn(home_dn):
    mod = _load("build_store_package", os.path.join(REPO, "scripts", "build-store-package.py"))
    assert mod.DEV_PUBLISHER == home_dn


# ── The direct-download package's production publisher ───────────────────────────
#
# installers/windows.md § Direct download, 5 (*The publisher, by outcome*): the
# signing certificate's subject, read before the first signed release and recorded in
# a production-publisher property beside the dev one in PackageIdentity.props. A
# production direct build refuses while it is empty; a dev build uses the dev DN.

DIRECT_PROPERTY = "FaunaDirectProductionPublisher"


def _store_packer():
    return _load("build_store_package", os.path.join(REPO, "scripts", "build-store-package.py"))


def _props(tmp_path, value):
    path = tmp_path / "PackageIdentity.props"
    path.write_text(
        "<Project><PropertyGroup>"
        "<FaunaPackagePublisher>CN=Props Fixture (Dev)</FaunaPackagePublisher>"
        f"<{DIRECT_PROPERTY}>{value}</{DIRECT_PROPERTY}>"
        "</PropertyGroup></Project>", encoding="utf-8")
    return str(path)


def test_the_production_publisher_has_its_home_beside_the_dev_one(home_dn):
    """Declared once, and either empty (no signer chosen yet) or a plain subject
    DN that is not the dev one — a dev DN there would let a production build sign
    as the dev certificate's subject."""
    values = [(el.text or "").strip() for el in ET.parse(PROPS).getroot().iter(DIRECT_PROPERTY)]
    assert len(values) == 1, f"PackageIdentity.props must declare {DIRECT_PROPERTY} once, got {values}"
    if values[0]:
        assert re.match(r"^CN=[^,]+", values[0]), f"{DIRECT_PROPERTY} {values[0]!r} is not a plain Subject DN"
        assert values[0] != home_dn, f"{DIRECT_PROPERTY} is the dev DN"


def test_the_shared_reader_returns_the_production_publisher(tmp_path):
    reader = _load("_windows_package_identity",
                   os.path.join(REPO, "scripts", "_windows_package_identity.py"))
    assert reader.direct_production_publisher(_props(tmp_path, "")) == ""
    assert reader.direct_production_publisher(_props(tmp_path, "CN=Example Org, O=Example Org")) == \
        "CN=Example Org, O=Example Org"


def test_a_production_direct_build_refuses_while_the_publisher_is_empty(tmp_path):
    packer = _store_packer()
    with pytest.raises(SystemExit) as exc:
        packer.resolve_publisher("direct", None, production=True, props=_props(tmp_path, ""))
    assert DIRECT_PROPERTY in str(exc.value)
    assert packer.resolve_publisher("direct", None, production=True,
                                    props=_props(tmp_path, "CN=Example Org")) == "CN=Example Org"


def test_production_is_the_direct_channels_and_takes_no_other_publisher(tmp_path):
    """The Store's production identity is the Partner Center reservation, passed
    as arguments; a direct production build's publisher is the recorded one only."""
    packer = _store_packer()
    props = _props(tmp_path, "CN=Example Org")
    with pytest.raises(SystemExit):
        packer.resolve_publisher("store", None, production=True, props=props)
    with pytest.raises(SystemExit):
        packer.resolve_publisher("direct", "CN=Somebody Else", production=True, props=props)


def test_a_dev_direct_build_uses_the_home_dn(home_dn):
    packer = _store_packer()
    assert packer.resolve_publisher("direct", None) == home_dn
    assert packer.resolve_publisher("store", None) == home_dn
    assert packer.resolve_publisher("direct", "CN=Pack Test") == "CN=Pack Test"


def test_a_production_build_is_never_self_signed():
    """A self-signed certificate made for the production subject would be a
    certificate in the organisation's name that nobody issued."""
    out = subprocess.run([sys.executable, os.path.join(REPO, "scripts", "build-store-package.py"),
                          "--channel", "direct", "--production", "--self-sign"],
                         capture_output=True, text=True, timeout=60)
    assert out.returncode != 0, out.stdout
    assert "unrecognized arguments" not in out.stderr, out.stderr
    assert "production" in out.stderr and "self-sign" in out.stderr, out.stderr


def test_harness_registers_its_packages_under_the_home_dn(home_dn):
    sys.path.insert(0, os.path.join(REPO, "tests", "e2e-unified"))
    from helpers import taskbar_badge
    assert taskbar_badge.IDENTITY_PUBLISHER == home_dn


def test_the_dn_literal_lives_only_in_its_home(home_dn):
    """No consumer keeps a copy: across the trees the consumers live in, the DN
    string appears in PackageIdentity.props and nowhere else."""
    trees = [WINDOWS, os.path.join(REPO, "scripts"), os.path.join(REPO, "tests", "e2e-unified")]
    hits = []
    for tree in trees:
        for dirpath, dirnames, filenames in os.walk(tree):
            dirnames[:] = [d for d in dirnames
                           if d not in ("bin", "obj", "node_modules", "__pycache__", ".venv", "target")]
            for name in filenames:
                path = os.path.join(dirpath, name)
                if os.path.normcase(path) == os.path.normcase(PROPS):
                    continue
                try:
                    with open(path, encoding="utf-8") as f:
                        if home_dn in f.read():
                            hits.append(os.path.relpath(path, REPO))
                except (UnicodeDecodeError, OSError):
                    continue
    assert not hits, (
        f"the publisher DN {home_dn!r} is restated outside installer/PackageIdentity.props: "
        f"{hits} — read it from the home instead (scripts/_windows_package_identity.py)"
    )

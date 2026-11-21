"""The Store MSIX actually packs — `makeappx` schema-validates it, both arches ship.

Companion to `test_store_package.py` (tier_1, fleet-wide, template parsing only).
That file pins what the manifest *says*; this one pins that Windows agrees it is a
valid package. `xml.etree` will happily parse a manifest that `makeappx` rejects —
a misspelled extension category, a namespace missing from `IgnorableNamespaces`, an
element in the wrong order — so template assertions alone cannot tell us the Store
would accept the upload.

**Why this is headless and fast.** Packing needs the referenced payload to exist
(no `/nv` — path validation is deliberately on), but it does not need the payload to
be *real*: `makeappx` validates paths and the manifest, not PE contents. The build
script therefore takes `--payload-root`, and this test points it at a tree of stub
files. That splits the mile — the packaging mechanism is proven here in seconds,
leaving only "does the installed app actually run" to the local verification pass
, which needs a genuine 30-minute publish.

Windows-only because `makeappx.exe` ships with the Windows SDK. Skips (rather than
fails) when the SDK is absent, matching `test_installer_structure.py`'s handling of
an unbuilt MSI.
"""

import os
import subprocess
import sys
import xml.etree.ElementTree as ET
import zipfile

import pytest

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="makeappx.exe is Windows-only"),
    pytest.mark.tier_3,
]

PKG_NS = "http://schemas.microsoft.com/appx/manifest/foundation/windows10"

# Stand-ins for the Partner-Center-assigned identity. Track 2 (enrollment) has not
# run, so no real reservation exists yet; the packer must not care, and pinning
# obviously-fake values here keeps a real reservation from being quietly baked in.
TEST_IDENTITY = "FaunaSocialTest.FaunaStorePackTest"
TEST_PUBLISHER = "CN=Fauna Social (Pack Test)"

# Every payload path the manifest references, relative to the package root.
STUB_PAYLOAD = [
    os.path.join("App", "FaunaApp.exe"),
    # The app's native core: the packer refuses a payload without it (the
    # csproj includes it only if it exists, so a hole would otherwise ship).
    os.path.join("App", "fauna_ffi.dll"),
    "fauna-sync-agent.exe",
    "fauna_shell.dll",
    # The three licence files at the package root (shipped-notices.md § Per
    # channel, the Microsoft Store row): the packer refuses a payload without them.
    "LICENSE-MIT.txt",
    "LICENSE-APACHE.txt",
    "THIRD-PARTY-NOTICES.txt",
]

# Debug symbols the MSBuild publish drops beside the managed binaries. They are
# staged by the MSI build too, so the store packer sees them; it must NOT ship
# them. Named separately from STUB_PAYLOAD because the containment test asserts
# the opposite of these.
STUB_SYMBOLS = [
    os.path.join("App", "FaunaApp.pdb"),
    os.path.join("App", "FaunaApp.Core.pdb"),
]


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    ).stdout.strip()
    if out.startswith("/"):
        out = out[1].upper() + ":" + out[2:]
    return os.path.normpath(out)


@pytest.fixture(scope="module")
def stub_payload_root(tmp_path_factory):
    """A payload tree with the right *names* and no real binaries."""
    root = tmp_path_factory.mktemp("store-payload")
    for rel in STUB_PAYLOAD + STUB_SYMBOLS:
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"stub")
    return root


@pytest.fixture(scope="module")
def packed(tmp_path_factory, stub_payload_root):
    """Run the real build script over the stub payload; return {arch: msix path}."""
    repo = _repo_root()
    script = os.path.join(repo, "scripts", "build-store-package.py")
    assert os.path.exists(script), f"store build script missing: {script}"

    out_dir = tmp_path_factory.mktemp("store-out")
    result = subprocess.run(
        [sys.executable, script,
         "--payload-root", str(stub_payload_root),
         "--identity-name", TEST_IDENTITY,
         "--publisher", TEST_PUBLISHER,
         "--out-dir", str(out_dir)],
        capture_output=True, text=True, timeout=600, cwd=repo,
    )
    if result.returncode != 0:
        if "not found under the Windows SDK" in (result.stdout + result.stderr):
            pytest.skip("Windows SDK (makeappx.exe) not installed on this box")
        pytest.fail(
            f"build-store-package.py failed (rc={result.returncode}):\n"
            f"stdout: {result.stdout[-4000:]}\nstderr: {result.stderr[-4000:]}"
        )

    return {
        arch: os.path.join(str(out_dir), f"Fauna-Store-{arch}.msix")
        for arch in ("x64", "arm64")
    }


def test_staging_dir_is_derived_from_out_dir(tmp_path, stub_payload_root):
    """Two builds with different `--out-dir` must not share a staging directory.

    Regression guard, 2026-08-23. `pack_dir` used to be a fixed repo path
    (`build/store/<arch>`) that ignored `--out-dir`, and `_stage` rmtree's it. So
    this test suite — which sends its `.msix` to a temp dir precisely to stay out
    of the way — silently overwrote the developer's staged loose layout with this
    file's FAKE identity. The `.msix` was unaffected, which is what made it
    expensive: the layout is what `Add-AppxPackage -Register` consumes, so a human
    registering "the Store package" got `FaunaSocialTest.FaunaStorePackTest`
    instead, with no error anywhere. Cost a live debugging round on 2026-08-23.

    Deriving the staging dir from `--out-dir` makes isolation automatic for any
    caller that already isolates its output, rather than something each caller has
    to remember.
    """
    repo = _repo_root()
    script = os.path.join(repo, "scripts", "build-store-package.py")
    out_a, out_b = tmp_path / "out-a", tmp_path / "out-b"

    def build(out_dir, identity):
        r = subprocess.run(
            [sys.executable, script, "--arch", "arm64",
             "--payload-root", str(stub_payload_root),
             "--identity-name", identity,
             "--publisher", TEST_PUBLISHER,
             "--out-dir", str(out_dir)],
            capture_output=True, text=True, timeout=600, cwd=repo,
        )
        if r.returncode != 0:
            if "not found under the Windows SDK" in (r.stdout + r.stderr):
                pytest.skip("Windows SDK (makeappx.exe) not installed on this box")
            pytest.fail(
                "build failed (rc=%d): %s %s"
                % (r.returncode, r.stdout[-3000:], r.stderr[-3000:])
            )
        return r

    build(out_a, "PackIsolation.First")
    build(out_b, "PackIsolation.Second")

    # The first build's staged layout must still describe the FIRST identity —
    # the second build must not have reached into it.
    staged_a = os.path.join(str(out_a), "store-stage", "arm64", "AppxManifest.xml")
    assert os.path.exists(staged_a), (
        f"no staged layout under the first --out-dir; looked for {staged_a}. The "
        "staging dir must live under --out-dir so concurrent/differing builds "
        "cannot clobber each other"
    )
    identity = ET.parse(staged_a).getroot().find(f"{{{PKG_NS}}}Identity")
    assert identity.get("Name") == "PackIsolation.First", (
        f"the first build's staged layout now says {identity.get('Name')!r} — the "
        "second build overwrote it. This is the exact bug that made a registered "
        "package silently carry a test identity"
    )


def _packed_manifest(msix_path):
    with zipfile.ZipFile(msix_path) as z:
        return ET.fromstring(z.read("AppxManifest.xml"))


class TestStorePackagePacks:
    def test_produces_one_package_per_architecture(self, packed):
        """x64 + ARM64, two `.msix` files — what a Store release uploads."""
        missing = [a for a, p in packed.items() if not os.path.exists(p)]
        assert not missing, (
            f"build produced no package for {missing}; a Store release ships both "
            f"desktop architectures (got: {[p for p in packed.values() if os.path.exists(p)]})"
        )

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_packed_manifest_declares_its_own_architecture(self, packed, arch):
        """The per-arch token really is substituted per arch.

        One template producing two packages is only correct if the two disagree on
        exactly this attribute — a copy/paste bug that stamped both as `x64` would
        ship an ARM64 payload that Windows refuses to run on ARM64 devices.
        """
        identity = _packed_manifest(packed[arch]).find(f"{{{PKG_NS}}}Identity")
        assert identity is not None
        assert identity.get("ProcessorArchitecture") == arch, (
            f"{os.path.basename(packed[arch])} declares ProcessorArchitecture="
            f'"{identity.get("ProcessorArchitecture")}", want "{arch}"'
        )

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_packed_manifest_has_no_surviving_tokens(self, packed, arch):
        """Nothing `@@…@@` reaches a shipped package."""
        with zipfile.ZipFile(packed[arch]) as z:
            text = z.read("AppxManifest.xml").decode("utf-8")
        assert "@@" not in text, (
            f"{os.path.basename(packed[arch])} ships an unsubstituted token: "
            f"{text[text.index('@@'):text.index('@@') + 40]!r}"
        )

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_packed_manifest_carries_the_supplied_identity(self, packed, arch):
        """Partner Center's assigned Name/Publisher flow through untouched."""
        identity = _packed_manifest(packed[arch]).find(f"{{{PKG_NS}}}Identity")
        assert identity.get("Name") == TEST_IDENTITY
        assert identity.get("Publisher") == TEST_PUBLISHER

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_package_contains_its_payload(self, packed, arch):
        """A full MSIX carries its binaries — the whole reason it can be Store-signed."""
        with zipfile.ZipFile(packed[arch]) as z:
            names = {n.replace("\\", "/") for n in z.namelist()}
        for rel in STUB_PAYLOAD:
            assert rel.replace("\\", "/") in names, (
                f"{rel} is missing from {os.path.basename(packed[arch])} — the "
                f"package contains {sorted(n for n in names if not n.startswith('Assets/'))}"
            )
        assert any(n.startswith("Assets/") for n in names), (
            "no Assets/ in the package — Properties/Logo would fail to resolve (0x80073CF6)"
        )

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_store_packages_declare_no_write_virtualization_opt_out(self, packed, arch):
        """The Store channel packs the template as it is: the direct channel's
        opt-out never reaches an upload."""
        root = _packed_manifest(packed[arch])
        names = {c.get("Name") for c in root.find(f"{{{PKG_NS}}}Capabilities")}
        assert names == {"runFullTrust"}, names
        with zipfile.ZipFile(packed[arch]) as z:
            text = z.read("AppxManifest.xml").decode("utf-8")
        assert "FileSystemWriteVirtualization" not in text
        assert root.find(f"{{{PKG_NS}}}Properties/{{{PKG_NS}}}DisplayName").text == "Fauna Social"

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_package_ships_no_debug_symbols(self, packed, arch):
        """`.pdb` files are staged next to the binaries but must not be shipped.

        They are dead weight in a package every user downloads, and they carry
        build-machine source paths and full symbol names for no user benefit.
        The publish step stages them, so the packer is the one place that can
        drop them for BOTH the Store package and every future consumer of
        `_stage`.
        """
        with zipfile.ZipFile(packed[arch]) as z:
            pdbs = [n for n in z.namelist() if n.lower().endswith(".pdb")]
        assert pdbs == [], (
            f"{os.path.basename(packed[arch])} ships debug symbols: {pdbs}"
        )


# ── The direct-download channel (installers/windows.md § Direct download) ────────
#
# One packer call packs both arch packages on the direct channel and bundles them
# into Fauna.msixbundle. `makeappx` schema-validates what XML parsing cannot: the
# write-virtualization block the packer inserts (two namespaces, the Windows 11
# scoped form and the blanket switch) and the bundle over the two packages.

DIRECT_IDENTITY = "FaunaSocial.FaunaDesktop"
VIRT_NS = "http://schemas.microsoft.com/appx/manifest/virtualization/windows10"
DESKTOP6_NS = "http://schemas.microsoft.com/appx/manifest/desktop/windows10/6"
BUNDLE_NS = "http://schemas.microsoft.com/appx/2013/bundle"


def _run_packer(*args):
    repo = _repo_root()
    script = os.path.join(repo, "scripts", "build-store-package.py")
    r = subprocess.run([sys.executable, script, *args],
                       capture_output=True, text=True, timeout=600, cwd=repo)
    if r.returncode != 0:
        if "not found under the Windows SDK" in (r.stdout + r.stderr):
            pytest.skip("Windows SDK (makeappx.exe) not installed on this box")
        pytest.fail(f"build-store-package.py {' '.join(args)} failed (rc={r.returncode}):\n"
                    f"stdout: {r.stdout[-4000:]}\nstderr: {r.stderr[-4000:]}")
    return r


@pytest.fixture(scope="module")
def direct_out(tmp_path_factory, stub_payload_root):
    """The direct channel packed over the stub payload: the out dir it wrote."""
    out_dir = tmp_path_factory.mktemp("direct-out")
    _run_packer("--channel", "direct", "--payload-root", str(stub_payload_root),
                "--publisher", TEST_PUBLISHER, "--out-dir", str(out_dir))
    return out_dir


def _product_version():
    import re
    with open(os.path.join(_repo_root(), "apps", "fauna-windows", "installer", "Package.wxs"),
              encoding="utf-8") as f:
        return re.search(r'\bVersion="(\d+\.\d+\.\d+)"', f.read()).group(1) + ".0"


def _bundle_manifest(bundle):
    with zipfile.ZipFile(bundle) as z:
        return ET.fromstring(z.read("AppxMetadata/AppxBundleManifest.xml"))


def _inner(bundle, arch):
    """The inner package of `arch`, as the bundle manifest names it: (name, zip)."""
    import io
    root = _bundle_manifest(bundle)
    (fname,) = [p.get("FileName") for p in root.iter(f"{{{BUNDLE_NS}}}Package")
                if p.get("Type") == "application" and p.get("Architecture") == arch]
    with zipfile.ZipFile(bundle) as z:
        return fname, zipfile.ZipFile(io.BytesIO(z.read(fname)))


class TestDirectPackagePacks:
    def test_produces_one_bundle_of_both_architectures(self, direct_out):
        bundle = direct_out / "Fauna.msixbundle"
        assert bundle.exists(), f"no Fauna.msixbundle in {sorted(p.name for p in direct_out.iterdir())}"
        root = _bundle_manifest(bundle)
        arches = sorted(p.get("Architecture") for p in root.iter(f"{{{BUNDLE_NS}}}Package")
                        if p.get("Type") == "application")
        assert arches == ["arm64", "x64"], arches

    def test_the_bundle_carries_the_direct_identity_at_the_product_version(self, direct_out):
        identity = _bundle_manifest(direct_out / "Fauna.msixbundle").find(f"{{{BUNDLE_NS}}}Identity")
        assert identity.get("Name") == DIRECT_IDENTITY
        assert identity.get("Publisher") == TEST_PUBLISHER
        assert identity.get("Version") == _product_version(), (
            "the bundle's version is the product version (§ Direct download, 2), never "
            "makeappx's clock-derived default"
        )

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_each_inner_package_is_the_direct_channels(self, direct_out, arch):
        _, pkg = _inner(direct_out / "Fauna.msixbundle", arch)
        text = pkg.read("AppxManifest.xml").decode("utf-8")
        assert "@@" not in text, "an unsubstituted token reached the package"
        root = ET.fromstring(text.encode("utf-8"))
        identity = root.find(f"{{{PKG_NS}}}Identity")
        assert (identity.get("Name"), identity.get("ProcessorArchitecture")) == (DIRECT_IDENTITY, arch)
        assert root.find(f"{{{PKG_NS}}}Properties/{{{PKG_NS}}}DisplayName").text == "Fauna"
        names = {c.get("Name") for c in root.find(f"{{{PKG_NS}}}Capabilities")}
        assert names == {"runFullTrust", "unvirtualizedResources"}, names
        props = root.find(f"{{{PKG_NS}}}Properties")
        assert (props.find(f"{{{DESKTOP6_NS}}}FileSystemWriteVirtualization").text or "").strip() == "disabled"
        excluded = [(e.text or "").strip() for e in props.iter(f"{{{VIRT_NS}}}ExcludedDirectory")]
        assert excluded == [r"$(KnownFolder:LocalAppData)\Fauna"], excluded

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_each_inner_package_carries_its_payload_and_no_symbols(self, direct_out, arch):
        _, pkg = _inner(direct_out / "Fauna.msixbundle", arch)
        names = {n.replace("\\", "/") for n in pkg.namelist()}
        for rel in STUB_PAYLOAD:
            assert rel.replace("\\", "/") in names, f"{rel} is missing from the {arch} package"
        assert not [n for n in names if n.lower().endswith(".pdb")]

    def test_the_store_and_direct_channels_share_no_staging(self, tmp_path, stub_payload_root):
        """Both channels packed into one out dir: the Store's staged layout still
        describes the Store package. The 2026-08-23 clobber (a build reaching into
        another build's loose layout) must not come back between channels."""
        _run_packer("--arch", "arm64", "--payload-root", str(stub_payload_root),
                    "--identity-name", TEST_IDENTITY, "--publisher", TEST_PUBLISHER,
                    "--out-dir", str(tmp_path))
        _run_packer("--channel", "direct", "--payload-root", str(stub_payload_root),
                    "--publisher", TEST_PUBLISHER, "--out-dir", str(tmp_path))
        staged = ET.parse(tmp_path / "store-stage" / "arm64" / "AppxManifest.xml").getroot()
        assert staged.find(f"{{{PKG_NS}}}Identity").get("Name") == TEST_IDENTITY
        assert (tmp_path / "Fauna-Store-arm64.msix").exists()
        assert (tmp_path / "Fauna.msixbundle").exists()

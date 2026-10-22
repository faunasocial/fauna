"""Structural validation of the Microsoft **Store** package (full MSIX).

Pure parsing of the checked-in manifest template plus in-process calls into
`scripts/build-store-package.py` — no MSI, no driver, no nest, no Windows API and
no packer. Runs on every machine (the couplings it guards are fleet-wide), which is
why there is no `skipif(sys.platform != "win32")` here. The *packing* half — does
`makeappx` accept this manifest, and are two arch packages actually produced — is
`test_store_package_pack.py` (tier_3, win-only).

Background — why a SECOND MSIX exists beside the sparse one
-----------------------------------------------------------
The sparse package (`test_sparse_package.py`) is a manifest-only identity carrier
for the MSI-installed shell extension: its payload lives outside the package. That
shape **cannot be Store-distributed**. The Store channel therefore ships a *full*
MSIX carrying its binaries inside, which is what earns Store re-signing (no
certificate to manage, no SmartScreen). See
`docs/goal/architecture/installers/windows.md` § Store distribution (MSIX).

The two packages are disjoint identities serving disjoint installs; neither
replaces the other.

The load-bearing invariants pinned here
---------------------------------------
1. **Identity stays templated.** `Name` *and* `Publisher` are assigned by Partner
   Center at name-reservation time and the manifest must match them exactly, so
   both remain build-time tokens — hard-coding either would weld the package to one
   Store reservation. `ProcessorArchitecture` is a token too: a full MSIX carrying
   native binaries must declare the arch it carries, and we ship two.
2. **No 9th CLSID** — same invariant as the sparse package: the Store package is a
   third *registration surface* for the one root `IExplorerCommand` implementation,
   so the expected GUID is read out of `ShellExt.wxs`, never restated here.
3. **The feature subset is the ratified one.** Desktop App + `fauna://` + sync agent
   + context menu are IN; **services are OUT** — an MSIX service can only run as
   localSystem/localService/networkService, which would abandon the ratified
   `NT SERVICE\\FaunaNest` virtual-account least-privilege shape. A service
   declaration sneaking into this manifest is a design regression, not a typo.
4. **Data continuity is scoped, not blanket.** `%LocalAppData%\\Fauna` is excluded
   from write virtualization so Store and MSI clients see one state; the exclusion
   must stay *scoped to that directory* rather than switching AppData virtualization
   off wholesale.
5. **Content is internal.** No `AllowExternalContent` — that is the sparse package's
   mechanism and is precisely what disqualifies it from the Store.
"""

import importlib.util
import os
import re
import struct
import subprocess
import sys
import xml.etree.ElementTree as ET
import zipfile

import pytest

pytestmark = pytest.mark.tier_1


NS = {
    "pkg": "http://schemas.microsoft.com/appx/manifest/foundation/windows10",
    "uap": "http://schemas.microsoft.com/appx/manifest/uap/windows10",
    "uap10": "http://schemas.microsoft.com/appx/manifest/uap/windows10/10",
    "rescap": "http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities",
    "desktop": "http://schemas.microsoft.com/appx/manifest/desktop/windows10",
    "desktop4": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/4",
    "desktop5": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/5",
    "desktop6": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/6",
    "com": "http://schemas.microsoft.com/appx/manifest/com/windows10",
    "virtualization": "http://schemas.microsoft.com/appx/manifest/virtualization/windows10",
}

# Same floor as the sparse package: MSIX packaging itself. The Win11 DEFAULT context
# menu only renders from 22000, but every other feature in this package (app,
# fauna://, startupTask) works below that, and a Windows 10 box still gets the
# legacy menu — so the floor is the packaging floor, not the menu's.
MIN_OS_VERSION = "10.0.19041.0"

# `runFullTrust` alone. `unvirtualizedResources` was dropped 2026-08-23 (user
# directive): it bought MSI-coexistence and MSI->Store migration only, and with
# no MSI installs in the field it served nobody while costing the submission its
# one unusual restricted capability. See installers/windows.md § Data continuity.
EXPECTED_CAPABILITIES = {"runFullTrust"}


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    ).stdout.strip()
    if sys.platform == "win32" and out.startswith("/"):
        out = out[1].upper() + ":" + out[2:]
    return os.path.normpath(out)


def _installer_dir():
    return os.path.join(_repo_root(), "apps", "fauna-windows", "installer")


def _template_path():
    return os.path.join(_installer_dir(), "store", "AppxManifest.xml.in")


@pytest.fixture(scope="module")
def template_text():
    path = _template_path()
    assert os.path.exists(path), f"store manifest template missing: {path}"
    with open(path, encoding="utf-8") as f:
        return f.read()


@pytest.fixture(scope="module")
def manifest():
    # Parsed from the FILE, not from `template_text`: ElementTree refuses a str that
    # carries an `encoding=` declaration.
    return ET.parse(_template_path()).getroot()


@pytest.fixture(scope="module")
def wxs_root_clsid():
    """The root IExplorerCommand CLSID, read from ShellExt.wxs (never restated).

    The MSI writes this GUID as `*\\shell\\Fauna`'s `ExplorerCommandHandler`; the
    sparse package reuses it; the Store package must too. Drift between the three
    is exactly the bug this reads-from-source fixture exists to catch.
    """
    wxs = os.path.join(_installer_dir(), "ShellExt.wxs")
    with open(wxs, encoding="utf-8") as f:
        text = f.read()
    m = re.search(
        r'Name="ExplorerCommandHandler"\s+Type="string"\s+Value="\{([0-9a-fA-F-]+)\}"',
        text,
    )
    assert m, "could not find the ExplorerCommandHandler CLSID in ShellExt.wxs"
    return m.group(1).lower()


@pytest.fixture(scope="module")
def builder():
    """`scripts/build-store-package.py` imported as a module.

    Importing (rather than grepping the source) means these assertions exercise the
    real functions the build runs — a renamed constant fails loudly instead of
    silently passing a regex.
    """
    path = os.path.join(_repo_root(), "scripts", "build-store-package.py")
    assert os.path.exists(path), f"store build script missing: {path}"
    spec = importlib.util.spec_from_file_location("build_store_package", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class TestStorePackageManifestTemplate:
    def test_all_build_time_tokens_are_present(self, template_text):
        """Every value the build substitutes must actually appear in the template.

        The packer hard-fails on an unsubstituted `@@…@@`, but a token that is
        *absent* fails the other way — silently shipping a hard-coded value. Both
        halves are checked (here, and by the builder's own missing-token guard).
        """
        for token in ("@@IdentityName@@", "@@Publisher@@",
                      "@@PackageVersion@@", "@@ProcessorArchitecture@@"):
            assert token in template_text, (
                f"{token} missing from the store AppxManifest.xml.in — every "
                "Partner-Center-assigned or per-arch value must stay a build parameter"
            )

    def test_identity_is_entirely_templated(self, manifest):
        """`Identity` carries no literal: Partner Center assigns Name + Publisher.

        Reserving the app name mints `Identity/@Name` and `Publisher` (`CN=<GUID>`),
        and the manifest must match them *exactly* or the Store rejects the upload.
        Hard-coding either would turn "we got our reservation" into a code change.
        """
        identity = manifest.find("pkg:Identity", NS)
        assert identity is not None, "manifest has no <Identity>"
        assert identity.get("Name") == "@@IdentityName@@", (
            f'Identity/@Name is "{identity.get("Name")}" — must stay the literal token '
            '"@@IdentityName@@" (Partner Center assigns it at name reservation)'
        )
        assert identity.get("Publisher") == "@@Publisher@@", (
            f'Identity/@Publisher is "{identity.get("Publisher")}" — must stay '
            '"@@Publisher@@" (Partner Center assigns CN=<GUID>; the Store re-signs)'
        )
        assert identity.get("Version") == "@@PackageVersion@@"
        assert identity.get("ProcessorArchitecture") == "@@ProcessorArchitecture@@", (
            f'Identity/@ProcessorArchitecture is "{identity.get("ProcessorArchitecture")}" '
            '— must stay "@@ProcessorArchitecture@@": a full MSIX carries native '
            "binaries, so each of the two packages declares the arch it carries"
        )

    def test_content_is_internal_no_external_location(self, manifest):
        """No `AllowExternalContent` — that is what disqualifies sparse from the Store.

        A Store package must carry its payload. This is the single structural
        difference from the sparse manifest, and getting it wrong produces a package
        the Store silently refuses.
        """
        el = manifest.find("pkg:Properties/uap10:AllowExternalContent", NS)
        assert el is None, (
            "store manifest declares uap10:AllowExternalContent — the Store cannot "
            "distribute an external-location package; its payload must be internal"
        )

    def test_reuses_the_existing_root_clsid_no_ninth_guid(self, manifest, wxs_root_clsid):
        """Every CLSID named here is the *existing* root context-menu CLSID."""
        com_ids = [e.get("Id") for e in manifest.iterfind(".//com:Class", NS)]
        verb_clsids = [e.get("Clsid") for e in manifest.iterfind(".//desktop5:Verb", NS)]
        assert com_ids, "manifest declares no com:Class — the COM server is unregistered"
        assert verb_clsids, "manifest declares no desktop5:Verb — no context menu"

        named = {g.strip("{}").lower() for g in com_ids + verb_clsids}
        assert named == {wxs_root_clsid}, (
            f"store package names CLSIDs {sorted(named)}, but must reuse ONLY the root "
            f"IExplorerCommand CLSID from ShellExt.wxs ({wxs_root_clsid}) — no 9th GUID"
        )

    def test_com_class_path_is_the_bare_shell_dll(self, manifest):
        """`com:Class/@Path` is the BARE DLL name, unlike the sparse package's.

        Path 1's version-stamped filename exists because an MSI cannot replace a DLL
        Explorer holds open. Inside an MSIX there is no such lock to dodge — the
        whole package is replaced atomically — so the packaged copy ships under its
        plain build-output name. Stamping it here would couple the Store package to
        an MSI-only workaround and break on every version bump.
        """
        cls = manifest.find(".//com:Class", NS)
        assert cls is not None
        assert cls.get("Path") == "fauna_shell.dll", (
            f'com:Class/@Path is "{cls.get("Path")}" — inside a full MSIX it must be '
            'the bare "fauna_shell.dll" (no Path-1 version stamp, no template token)'
        )

    def test_registers_the_verb_for_files_and_folders(self, manifest):
        """The menu must appear on files, folders, and folder backgrounds."""
        types = {e.get("Type") for e in manifest.iterfind(".//desktop5:ItemType", NS)}
        assert {"*", "Directory", "Directory\\Background"} <= types, (
            f"store package registers item types {sorted(types)}; want at least "
            "'*', 'Directory' and 'Directory\\\\Background'"
        )

    def test_declares_the_fauna_protocol(self, manifest):
        """`fauna://` is a manifest extension here, not an HKLM Classes key."""
        protocols = {e.get("Name") for e in manifest.iterfind(".//uap:Protocol", NS)}
        assert "fauna" in protocols, (
            f"store package declares protocols {sorted(protocols)} — the fauna:// "
            "handler is part of the ratified Store feature subset"
        )

    def test_startup_task_launches_the_sync_agent_enabled(self, manifest):
        """`windows.startupTask` replaces the MSI's HKLM Run key.

        A Store app cannot write the Run key, so the per-user sync agent's logon
        launch is a manifest extension. `Enabled="true"` is honoured for full-trust
        desktop apps once the app has been launched once; the user keeps control via
        Task Manager's Startup tab.
        """
        tasks = manifest.findall(".//desktop:Extension[@Category='windows.startupTask']", NS)
        assert len(tasks) == 1, (
            f"expected exactly one windows.startupTask extension, found {len(tasks)} — "
            "the per-user sync agent is the only thing that runs at logon"
        )
        ext = tasks[0]
        assert ext.get("Executable") == "fauna-sync-agent.exe", (
            f'startupTask Executable is "{ext.get("Executable")}" — must be the '
            "package-relative fauna-sync-agent.exe the MSI's Run key launches"
        )
        assert ext.get("EntryPoint") == "Windows.FullTrustApplication"
        task = ext.find("desktop:StartupTask", NS)
        assert task is not None, "windows.startupTask extension has no <StartupTask>"
        assert task.get("Enabled") == "true", (
            "StartupTask/@Enabled must be true — sync starting itself at logon is the "
            "works-out-of-the-box invariant; the user disables it in Task Manager"
        )
        assert task.get("TaskId"), "StartupTask needs a stable TaskId"

    def test_declares_no_services(self, manifest):
        """Nest and Bridge are permanently MSI-only — no service may appear here.

        MSIX services run only as localSystem/localService/networkService, with no
        virtual-account support, so shipping either service Store-side would abandon
        the ratified `NT SERVICE\\FaunaNest` least-privilege shape. This asserts the
        *design*, not an accident: a service element here means someone re-litigated
        a ratified decision inside a manifest.
        """
        services = manifest.findall(".//desktop6:Service", NS)
        assert not services, (
            f"store manifest declares {len(services)} desktop6:Service element(s) — "
            "Nest and Bridge are MSI-only by ratified design (MSIX services cannot "
            "use virtual accounts)"
        )
        categories = {e.get("Category") for e in manifest.iter() if e.get("Category")}
        assert "windows.service" not in categories, (
            "store manifest declares a windows.service extension — services are "
            "MSI-only by ratified design"
        )

    def test_capability_set_is_exactly_the_ratified_one(self, manifest):
        """Exactly `runFullTrust`, nothing more.

        It is a *restricted* capability needing a Store-submission justification,
        but it is the standard desktop-bridge one every full-trust Win32 Store app
        declares. Anything BEYOND it is an unusual justification a human has to
        write and certification has to stop and read — which is exactly why
        `unvirtualizedResources` was dropped. An extra capability appearing here
        silently re-adds that cost.
        """
        caps = manifest.find("pkg:Capabilities", NS)
        assert caps is not None, "manifest has no <Capabilities>"
        names = {c.get("Name") for c in caps}
        assert names == EXPECTED_CAPABILITIES, (
            f"store package declares capabilities {sorted(names)}; want exactly "
            f"{sorted(EXPECTED_CAPABILITIES)} — every extra one needs its own "
            "certification justification"
        )

    def test_declares_no_write_virtualization_opt_out(self, manifest):
        r"""The package accepts AppData write virtualization. Do not re-add an opt-out.

        Until 2026-08-23 this package unvirtualized `%LocalAppData%\Fauna` via
        Windows 11 flexible virtualization, so a Store install and an MSI install
        would see one `mls.db` / sync DB / drafts. That bought exactly two things —
        coexistence with MSI-installed components, and MSI->Store migration — and
        the user dropped both on 2026-08-23: there are no MSI installs in the field
        to coexist with or migrate from.

        What it COST was the submission's one unusual restricted capability, i.e.
        the thing certification stops to read a hand-written justification for.

        Nothing breaks on a Store-only box: the app, the sync agent and the shell
        extension all run under package identity, so they share one virtualized
        view. The one path that would have escaped it — `extract_icons()` handing
        Explorer a literal `%LOCALAPPDATA%\Fauna\icons` path — is inert here,
        because its only consumer is the icon overlay handler (`overlay.rs`) and an
        MSIX package cannot register one (it needs an HKLM key).

        Re-adding either form is a goal-doc decision (§ Data continuity), not a
        manifest edit — and it needs a fresh certification justification.
        """
        excluded = [
            (e.text or "").strip()
            for e in manifest.iterfind(
                "pkg:Properties/virtualization:FileSystemWriteVirtualization"
                "/virtualization:ExcludedDirectories/virtualization:ExcludedDirectory",
                NS,
            )
        ]
        assert excluded == [], (
            f"store package excludes {excluded} from write virtualization. That "
            "requires the unvirtualizedResources restricted capability, dropped "
            "2026-08-23 — the two would-be beneficiaries (MSI coexistence, "
            "MSI->Store migration) have no users"
        )

        blanket = manifest.find(
            "pkg:Properties/desktop6:FileSystemWriteVirtualization", NS
        )
        assert blanket is None, (
            "store package declares the pre-Windows-11 blanket "
            "desktop6:FileSystemWriteVirtualization switch. That switch unvirtualizes "
            "ALL of AppData and needs the same restricted capability this package "
            "deliberately no longer declares (installers/windows.md § Data continuity)."
        )

    def test_listing_name_matches_the_reservation_app_name_does_not(self, manifest):
        """The two DisplayNames differ ON PURPOSE — do not "fix" the inconsistency.

        `Fauna` was unavailable at Partner Center reservation (another publisher
        holds it), so the product is reserved as `Fauna Social`. Two fields follow,
        and only one is forced:

        * `Properties/DisplayName` **must** equal a name reserved for this product —
          Partner Center validates it at package upload and rejects a mismatch. So
          it reads `Fauna Social`, whatever we might prefer.
        * `Application/VisualElements/@DisplayName` is the Start-menu / taskbar /
          Alt+Tab label. It is NOT reservation-validated, and it deliberately stays
          `Fauna` — priority #1: a Store name-availability accident must not leak a
          naming divergence into app UI, where Fauna is "Fauna" on all seven apps.

        This test exists because the divergence reads like a typo. Unifying either
        way costs something real: raise the app label to "Fauna Social" and Windows
        becomes the one app with a different name; lower the listing name to "Fauna"
        and the upload is rejected. Authority:
        `docs/goal/architecture/installers/windows.md` § Identity & coexistence.
        """
        listing = manifest.find("pkg:Properties/pkg:DisplayName", NS)
        assert listing is not None, "manifest has no Properties/DisplayName"
        assert listing.text == "Fauna Social", (
            f"Properties/DisplayName is {listing.text!r}, but Partner Center only "
            "accepts a name reserved for this product — the reservation is "
            '"Fauna Social" (`Fauna` was taken). A mismatch fails at upload, not '
            "at build, so this is the only place it can be caught cheaply."
        )

        ve = manifest.find(".//uap:VisualElements", NS)
        assert ve is not None, "manifest has no uap:VisualElements"
        assert ve.get("DisplayName") == "Fauna", (
            f"VisualElements/@DisplayName is {ve.get('DisplayName')!r} — the app's "
            'own Start-menu label must stay "Fauna" to match the other six apps. '
            "It is not reservation-validated, so it does not have to follow the "
            "listing name, and priority #1 says it must not."
        )

    def test_publisher_display_name_matches_the_enrolled_publisher(self, manifest):
        """`PublisherDisplayName` must equal the Partner Center publisher name.

        Unlike Identity/@Publisher (a `CN=<GUID>` assigned at enrollment, and a
        build token) this one is a human-readable string the Store checks against
        the enrolled account: "Fauna Social". It is a literal rather than a token
        because it belongs to the *account*, not to a channel — the dev loop and
        the Store upload carry the same publisher name.
        """
        pdn = manifest.find("pkg:Properties/pkg:PublisherDisplayName", NS)
        assert pdn is not None, "manifest has no Properties/PublisherDisplayName"
        assert pdn.text == "Fauna Social", (
            f"PublisherDisplayName is {pdn.text!r}; Partner Center enrolled this "
            'account as "Fauna Social" and validates the field at upload'
        )

    def test_publishes_a_real_app_list_entry(self, manifest):
        """Unlike the sparse package, THIS package is the app.

        The sparse manifest sets `AppListEntry="none"` because the MSI already
        installs the real Start-menu shortcut. A Store install has no MSI, so
        suppressing the entry here would ship an app with no way to launch it.
        """
        ve = manifest.find(".//uap:VisualElements", NS)
        assert ve is not None, "manifest has no uap:VisualElements"
        assert ve.get("AppListEntry") != "none", (
            'store package sets AppListEntry="none" — that is the sparse identity '
            "carrier's setting; a Store install would then have no Start-menu entry"
        )

    def test_app_is_a_full_trust_win32_executable(self, manifest):
        """The WinUI app runs full-trust from inside the package."""
        app = manifest.find("pkg:Applications/pkg:Application", NS)
        assert app is not None, "manifest declares no <Application>"
        assert app.get("Executable") == "App\\FaunaApp.exe", (
            f'Application/@Executable is "{app.get("Executable")}" — must mirror the '
            "MSI's App\\FaunaApp.exe layout so both channels stage identically"
        )
        assert app.get("EntryPoint") == "Windows.FullTrustApplication"

    def test_targets_the_msix_packaging_floor(self, manifest):
        tdf = manifest.find("pkg:Dependencies/pkg:TargetDeviceFamily", NS)
        assert tdf is not None, "manifest declares no TargetDeviceFamily"
        assert tdf.get("MinVersion") == MIN_OS_VERSION, (
            f'TargetDeviceFamily/@MinVersion is {tdf.get("MinVersion")}, want '
            f"{MIN_OS_VERSION} (the MSIX packaging floor)"
        )


class TestStorePackageBuildScript:
    """In-process assertions on `scripts/build-store-package.py`'s real functions."""

    def test_builds_exactly_x64_and_arm64(self, builder):
        """Two packages per release — the Store's two desktop architectures.

        x86 is unsupported product-wide (§ Platform Support), and a `neutral`
        package would claim to run everywhere while carrying one arch's binaries.
        """
        assert tuple(builder.ARCHES) == ("x64", "arm64"), (
            f"build script targets {builder.ARCHES}; want exactly ('x64', 'arm64')"
        )

    def test_version_is_read_from_package_wxs_never_restated(self, builder):
        """One home for the product version: the MSI's `Package.wxs`.

        A second copy here would drift, and MSIX rejects re-registering an unchanged
        version (0x80073CF9), so a stale copy fails the dev loop in a way that looks
        like a packaging bug.
        """
        with open(os.path.join(_installer_dir(), "Package.wxs"), encoding="utf-8") as f:
            m = re.search(r'\bVersion="(\d+\.\d+\.\d+)"', f.read())
        assert m, "could not read Version from Package.wxs"
        assert builder._product_version() == f"{m.group(1)}.0"

    def test_packs_with_path_validation_enabled(self, builder):
        """No `/nv`: the Store package's content is internal, so paths MUST validate.

        The sparse build passes `/nv` because its manifest references files outside
        the package. Carrying that flag over would disable exactly the check that
        catches a payload we forgot to stage — the failure would then surface as a
        broken install on a user's machine instead of a red build.
        """
        # Inject the tool name: resolving the real SDK is win-only, but the
        # "no /nv" claim is about this script and must be guarded fleet-wide.
        argv = builder._pack_argv("some-pack-dir", "some-out.msix", tool="makeappx")
        assert "/nv" not in argv, (
            f"makeappx argv contains /nv ({argv}) — path validation must run for a "
            "full package; /nv is the sparse package's external-content escape hatch"
        )
        assert "pack" in argv and "some-pack-dir" in argv and "some-out.msix" in argv

    def test_refuses_to_ship_an_unsubstituted_token(self, builder):
        """The packer hard-fails on a surviving `@@…@@`, per the sparse convention."""
        with pytest.raises(SystemExit):
            builder.render_manifest(
                template="<Package Name='@@IdentityName@@' X='@@NeverSubstituted@@'/>",
                identity_name="Whatever.Fauna",
                publisher="CN=x",
                version="0.1.1.0",
                arch="x64",
            )


# ── Grading a packed upload (`build-store-package.py --grade`) ────────────────────
#
# What the Store-upload workflow runs on each package before it hands it over, and
# what a hand build runs too (installers/windows.md § Store distribution (MSIX)).
# The packer SETS identity from its arguments; the grade reads the PACKAGE back and
# compares it with what the caller says a real upload carries. Synthetic MSIX zips
# and synthetic PE images, so the grades are proven on every machine; the real
# payload is graded by the workflow's own job.

GRADE_IDENTITY = "FaunaSocialTest.FaunaGradeTest"
GRADE_PUBLISHER = "CN=00000000-0000-0000-0000-000000000000"
_MACHINE = {"x64": 0x8664, "arm64": 0xAA64, "x86": 0x14C}


def _pe(machine, cor_flags=None, chpe=False):
    """A minimal PE32+ image: one section, a CLR header when `cor_flags` is set, and a
    load config carrying CHPE metadata (an ARM64X hybrid image) when `chpe` is set."""
    opt_size = 112 + 16 * 8
    data = bytearray(0x400)
    data[0:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x40)
    data[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<HH", data, 0x44, machine, 1)            # Machine, NumberOfSections
    struct.pack_into("<H", data, 0x44 + 16, opt_size)          # SizeOfOptionalHeader
    opt = 0x58
    struct.pack_into("<H", data, opt, 0x20B)                   # PE32+
    dirs = opt + 112
    struct.pack_into("<I", data, dirs - 4, 16)                 # NumberOfRvaAndSizes
    if cor_flags is not None:
        struct.pack_into("<II", data, dirs + 14 * 8, 0x1000, 72)   # CLR runtime header
        struct.pack_into("<I", data, 0x200 + 16, cor_flags)        # its Flags
    if chpe:
        struct.pack_into("<II", data, dirs + 10 * 8, 0x1080, 0xD0)     # load config
        struct.pack_into("<I", data, 0x280, 0xD0)                      # its Size
        struct.pack_into("<Q", data, 0x280 + 0xC8, 0x18000A288)        # CHPEMetadataPointer
    struct.pack_into("<IIII", data, opt + opt_size + 8, 0x200, 0x1000, 0x200, 0x200)
    return bytes(data)


def _manifest_xml(name=GRADE_IDENTITY, publisher=GRADE_PUBLISHER, version="0.1.1.0",
                  arch="x64", capabilities=("runFullTrust",)):
    caps = "".join(f'<rescap:Capability Name="{c}"/>' for c in capabilities)
    return (
        '<?xml version="1.0" encoding="utf-8"?>'
        f'<Package xmlns="{NS["pkg"]}" xmlns:rescap="{NS["rescap"]}">'
        f'<Identity Name="{name}" Publisher="{publisher}" Version="{version}" '
        f'ProcessorArchitecture="{arch}"/>'
        f"<Capabilities>{caps}</Capabilities></Package>"
    )


def _msix(tmp_path, arch="x64", manifest=None, extra=None):
    """A package-shaped zip carrying the four payload binaries built for `arch`."""
    pe = _pe(_MACHINE[arch])
    entries = {
        "AppxManifest.xml": manifest if manifest is not None else _manifest_xml(arch=arch),
        "App/FaunaApp.exe": pe,
        "App/fauna_ffi.dll": pe,
        "fauna-sync-agent.exe": pe,
        "fauna_shell.dll": pe,
    }
    entries.update(extra or {})
    path = tmp_path / f"Fauna-Store-{arch}.msix"
    with zipfile.ZipFile(path, "w") as z:
        for name, body in entries.items():
            z.writestr(name, body)
    return str(path)


class TestStorePackageGrade:
    def _grade(self, builder, path, arch="x64", version="0.1.1.0"):
        return builder.grade_package(path, arch, GRADE_IDENTITY, GRADE_PUBLISHER, version)

    @pytest.mark.parametrize("arch", ["x64", "arm64"])
    def test_a_well_formed_package_grades_clean(self, builder, tmp_path, arch):
        assert self._grade(builder, _msix(tmp_path, arch=arch), arch=arch) == []

    @pytest.mark.parametrize("field,value", [
        ("name", "FaunaSocial.FaunaDev"),
        ("publisher", "CN=Fauna Dev"),
        ("version", "0.1.0.0"),
        ("arch", "arm64"),
    ])
    def test_an_identity_that_is_not_the_uploads_is_refused(self, builder, tmp_path,
                                                           field, value):
        problems = self._grade(builder, _msix(tmp_path, manifest=_manifest_xml(**{field: value})))
        assert any("identity" in p.lower() for p in problems), problems

    @pytest.mark.parametrize("caps", [("runFullTrust", "unvirtualizedResources"), ()])
    def test_run_full_trust_is_the_only_capability(self, builder, tmp_path, caps):
        problems = self._grade(builder, _msix(tmp_path, manifest=_manifest_xml(capabilities=caps)))
        assert any("capabilit" in p.lower() for p in problems), problems

    def test_a_pe_of_another_arch_is_refused(self, builder, tmp_path):
        """An x64 binary inside the arm64 package: the slip the 2026-10-10 hand builds
        graded by eye. The grade reads every PE's machine type."""
        path = _msix(tmp_path, arch="arm64", extra={"App/stray.dll": _pe(_MACHINE["x64"])})
        problems = self._grade(builder, path, arch="arm64")
        assert any("App/stray.dll" in p for p in problems), problems

    def test_an_arm64x_hybrid_image_ships_in_the_x64_package(self, builder, tmp_path):
        """The Windows App SDK's x64 runtime carries ARM64X images (ARM64 machine type
        plus CHPE metadata in the load config, e.g. Microsoft.Windows.Workloads.
        Resources_ec.dll) for x64 processes on ARM64 Windows; the 0.1.5 Store dry run
        refused one. A plain ARM64 image in the x64 package is still a stray."""
        hybrid = _msix(tmp_path, extra={"App/Hybrid_ec.dll": _pe(_MACHINE["arm64"], chpe=True)})
        assert self._grade(builder, hybrid) == []
        (tmp_path / "plain").mkdir()
        plain = _msix(tmp_path / "plain", extra={"App/stray.dll": _pe(_MACHINE["arm64"])})
        assert any("App/stray.dll" in p for p in self._grade(builder, plain))

    def test_an_il_only_managed_assembly_is_arch_neutral(self, builder, tmp_path):
        """AnyCPU .NET assemblies carry the i386 machine type plus the ILONLY flag;
        they run on either arch, so they are not a stray binary."""
        path = _msix(tmp_path, arch="arm64",
                     extra={"App/Managed.dll": _pe(_MACHINE["x86"], cor_flags=0x1)})
        assert self._grade(builder, path, arch="arm64") == []

    def test_a_32bit_required_managed_assembly_is_refused(self, builder, tmp_path):
        path = _msix(tmp_path, extra={"App/X86Only.dll": _pe(_MACHINE["x86"], cor_flags=0x3)})
        assert any("App/X86Only.dll" in p for p in self._grade(builder, path))

    def test_a_native_x86_binary_is_refused(self, builder, tmp_path):
        path = _msix(tmp_path, extra={"App/helper.exe": _pe(_MACHINE["x86"])})
        assert any("App/helper.exe" in p for p in self._grade(builder, path))

    def test_a_binary_that_is_not_a_pe_is_refused(self, builder, tmp_path):
        path = _msix(tmp_path, extra={"App/broken.dll": b"not a portable executable"})
        assert any("App/broken.dll" in p for p in self._grade(builder, path))

    def test_debug_symbols_are_refused(self, builder, tmp_path):
        path = _msix(tmp_path, extra={"App/FaunaApp.pdb": b"symbols"})
        assert any(".pdb" in p for p in self._grade(builder, path))

    def test_the_automation_surface_is_refused(self, builder, tmp_path):
        """The packer refuses a test-flavour payload at staging; the grade re-reads
        the PACKAGE, so one packed with the opt-out still cannot pass."""
        app = _pe(_MACHINE["x86"], cor_flags=0x1) + b"FaunaApp.Testing\x00"
        path = _msix(tmp_path, extra={"App/FaunaApp.dll": app})
        assert any("automation surface" in p for p in self._grade(builder, path))


class TestStorePackageFlavourGrade:
    """The Store package is the `store-safe` flavour (installers/windows.md § Store
    distribution, the flavour paragraph, owner ruling 2026-10-14): its native core
    carries no payments plane, and zaps and peer-to-peer sharing go with it. The
    grade reads the packaged fauna_ffi.dll for the names `just
    windows-store-safe-check` scans in its native column."""

    def _grade(self, builder, path, flavour):
        return builder.grade_package(path, "x64", GRADE_IDENTITY, GRADE_PUBLISHER,
                                     "0.1.1.0", flavour=flavour)

    def _ffi(self, *carried):
        return _pe(_MACHINE["x64"]) + b"".join(s + b"\x00" for s in carried)

    def test_the_store_flavour_is_the_default(self, builder, tmp_path):
        """A grade that names no flavour grades the ruled one, so a hand re-run
        cannot pass a full package by omission."""
        path = _msix(tmp_path, extra={"App/fauna_ffi.dll": self._ffi(b"fauna.payments.invoice")})
        problems = builder.grade_package(path, "x64", GRADE_IDENTITY, GRADE_PUBLISHER, "0.1.1.0")
        assert any("fauna.payments." in p for p in problems), problems

    def test_an_excised_core_grades_clean(self, builder, tmp_path):
        path = _msix(tmp_path, extra={"App/fauna_ffi.dll": self._ffi(b"fauna.feed.post")})
        assert self._grade(builder, path, "store-safe") == []

    @pytest.mark.parametrize("carried", [
        b"fauna.payments.invoice", b"fauna.tips.send", b"fauna.nostr.zap_signers.list",
        b"uniffi_fauna_ffi_fn_func_offline_share_begin", b"FfiSharePlaneView",
    ])
    def test_a_core_carrying_an_excised_plane_is_refused(self, builder, tmp_path, carried):
        path = _msix(tmp_path, extra={"App/fauna_ffi.dll": self._ffi(carried)})
        problems = self._grade(builder, path, "store-safe")
        assert any("App/fauna_ffi.dll" in p and "store-safe" in p for p in problems), problems

    def test_a_package_without_its_core_is_refused(self, builder, tmp_path):
        """No fauna_ffi.dll is an empty scan, which every absence check would read
        as a clean excision."""
        path = _msix(tmp_path)
        with zipfile.ZipFile(path) as z:
            kept = {n: z.read(n) for n in z.namelist() if n != "App/fauna_ffi.dll"}
        with zipfile.ZipFile(path, "w") as z:
            for name, body in kept.items():
                z.writestr(name, body)
        problems = self._grade(builder, path, "store-safe")
        assert any("App/fauna_ffi.dll" in p and "missing" in p for p in problems), problems

    def test_the_full_flavour_keeps_the_payments_plane(self, builder, tmp_path):
        path = _msix(tmp_path, extra={"App/fauna_ffi.dll": self._ffi(b"fauna.payments.invoice")})
        assert self._grade(builder, path, "full") == []

    def test_an_unknown_flavour_is_refused(self, builder, tmp_path):
        with pytest.raises(ValueError):
            self._grade(builder, _msix(tmp_path), "kids-safe")

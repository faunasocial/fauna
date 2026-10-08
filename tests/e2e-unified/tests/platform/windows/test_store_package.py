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
import subprocess
import sys
import xml.etree.ElementTree as ET

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

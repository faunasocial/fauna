#!/usr/bin/env python3
"""Build (and optionally sign) the Fauna full MSIX — the **Microsoft Store** package,
or, on the direct channel, the **direct-download** bundle.

    just windows-store-package arm64 --identity-name … --publisher …   # what an upload runs
    python scripts/build-store-package.py --arch arm64        # pack an already-staged payload
    python scripts/build-store-package.py --arch arm64 --self-sign
    python scripts/build-store-package.py --grade <msix> --arch arm64 --identity-name … --publisher …
    just windows-direct-package                               # the direct download, dev-signed
    python scripts/build-store-package.py --channel direct --self-sign
    python scripts/build-store-package.py --channel direct --grade build/installer/Fauna.msixbundle
    python scripts/build-store-package.py --channel direct --arch arm64 --identity-name … \
        --payload-root <payload> --stage-only <dir>          # a loose layout, for -Register

`--grade` reads a finished package back and checks it is a Store upload (identity,
capabilities, every PE's arch, no symbols, no automation surface, its three licence
files at the root, and its store
flavour's excision in its native core: for `store-no-payments`, the Store's and the
default, no payments or zaps plane and the peer-to-peer sharing plane present; for
`store-safe`, none of the three); the public `release-store-windows.yml` workflow
grades every package it builds with it.

**Two channels, one template, one packer** (installers/windows.md § Direct download
(MSIX app + nest MSI)). `--channel direct` changes exactly five things: the identity
name (`FaunaSocial.FaunaDesktop`), the publisher (the signing certificate's subject,
recorded in installer/PackageIdentity.props; the dev DN for a dev build),
`Properties/DisplayName` (`Fauna`), the write-virtualization block (the opt-out for
`%LocalAppData%\\Fauna` and its `unvirtualizedResources` capability, inserted here,
never in the Store manifest) and the flavour (`full`). It then bundles the x64 and
the arm64 package into one `Fauna.msixbundle` (`makeappx bundle`), which
`--self-sign` signs for the dev loop. The Store upload stays two unsigned `.msix`.

This script PACKS. The payload it packs — the self-contained WinUI publish, the sync
agent and the shell extension, all in their shipped flavour — is built and staged by
`just windows-store-package`, which then calls this script. The MSI test helper
(`tests/e2e-unified/helpers/windows_installer.py::build_msi`) also stages a payload, but a deliberately
TEST-flavoured one (the e2e agent compiled in), so this script refuses it unless a
caller passes `--allow-test-surface` (§ the automation-surface check below).

The package carries its binaries inside, which is what earns Store re-signing: free,
no certificate to manage, no SmartScreen warning — and it is the app's one package
identity on Windows (installers/windows.md § Package identity for FaunaApp.exe; the
manifest-only sparse identity package an MSI once registered retired 2026-11-16).
Design of record: docs/goal/architecture/installers/windows.md § Store distribution
(MSIX).

Two consequences of "content is internal" show up directly below:

  * **No `/nv`.** MakeAppx's path validation is disabled only for a manifest that
    references files deliberately absent from the package. Here every referenced
    path must resolve, so validation is exactly the check that catches a payload we
    forgot to stage — leaving it on is the point.
  * **One package per architecture.** A full MSIX carries native binaries, so it
    must declare the arch it carries; a `neutral` package would claim to run
    everywhere while shipping one arch's code. A Store release uploads both.

Identity is assigned by Partner Center at name reservation and the Store rejects any
mismatch, so `--identity-name`/`--publisher` are required parameters of a *real*
build. Their defaults here are obvious dev placeholders, never a reservation.

Version is READ FROM `Package.wxs`, never restated: one home for the product
version, and MSIX refuses to re-register an unchanged version (0x80073CF9).
"""

import argparse
import glob
import io
import os
import re
import shutil
import struct
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import _windows_package_identity  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
INSTALLER = os.path.join(REPO, "apps", "fauna-windows", "installer")
STORE = os.path.join(INSTALLER, "store")
# Brand assets have ONE generator (the appx-logo renderer, which writes them once and
# commits them) and one home, beside the template that names them. Both channels pack
# the same PNGs — a second copy would fork the brand mark the moment either is
# regenerated.
ASSETS = os.path.join(STORE, "Assets")

# The Store's two desktop architectures. x86 is unsupported product-wide
# (installers/windows.md § Platform Support).
ARCHES = ("x64", "arm64")

# Dev placeholders, NOT the reservation. The Partner Center identity (reserved
# 2026-08-22, installers/windows.md § Identity & coexistence) is its own value — a
# build that ships passes it explicitly as the identity name and publisher; it is
# never the MSI's signing DN. Only the DEV publisher is shared with the MSI channel,
# read from its one build-time home (installer/PackageIdentity.props).
DEV_IDENTITY_NAME = "FaunaSocial.FaunaDev"
DEV_PUBLISHER = _windows_package_identity.publisher()
DEV_CERT_FRIENDLY = "Fauna Store Package Dev"
# The throwaway password of the dev-loop PFX (`--self-sign`). Not a secret: the
# cert signs local test registrations only and is re-exported on every run.
DEV_PFX_PASSWORD = "fauna-store-pack"

# ── The two channels (installers/windows.md § Direct download, *One template, one
# packer, two channels*) ────────────────────────────────────────────────────────
CHANNELS = ("store", "direct")
# The direct-download package's identity name: a name of its own, permanent from
# the first signed release (§ Direct download, 5). Not a reservation, so it is the
# direct channel's default for dev and production builds alike.
DIRECT_IDENTITY_NAME = "FaunaSocial.FaunaDesktop"
# Properties/DisplayName. The Store's must equal its Partner Center reservation
# (`Fauna` was taken); the direct package has none and carries the app's own name.
DISPLAY_NAME = {"store": "Fauna Social", "direct": "Fauna"}
# What the direct package's file names are, beside the Store's Fauna-Store-<arch>.msix.
DIRECT_BUNDLE = "Fauna.msixbundle"


def _direct_package_name(arch):
    return f"Fauna-Direct-{arch}.msix"


# The direct channel's write-virtualization opt-out (§ Direct download, 5, *Data*):
# user data stays in the REAL %LocalAppData%\Fauna, so a forced uninstall (a
# changed publisher string) loses nothing and the unpackaged terminal app and agent
# see the same state. Microsoft's flexible-virtualization page gives two forms, and
# the package declares both, because it installs from 10.0.19041:
#   * Windows 11 and later honour the scoped `virtualization:` exclusion and it
#     takes precedence there, so only this one directory is unvirtualized;
#   * earlier Windows ignores it and honours the blanket `desktop6:` switch, which
#     unvirtualizes all of AppData for the package (registry writes stay
#     virtualized: no requirement reaches the registry).
# Both need `unvirtualizedResources` (supported from Windows 10 1903, below the
# package's floor). Inserted as text at fixed anchors, so the template's comments and
# prefixes survive and the Store manifest is the template's own text.
DIRECT_EXCLUDED_DIRECTORY = r"$(KnownFolder:LocalAppData)\Fauna"
_DESKTOP6_NS = "http://schemas.microsoft.com/appx/manifest/desktop/windows10/6"
_VIRTUALIZATION_NS = "http://schemas.microsoft.com/appx/manifest/virtualization/windows10"
_DIRECT_PROPERTIES = (
    "\n"
    "    <!-- The direct download keeps user data in the real %LocalAppData%\\Fauna\n"
    "         (installers/windows.md section Direct download, 5). Windows 11 honours\n"
    "         the scoped exclusion and it takes precedence there; earlier Windows\n"
    "         ignores it and honours the blanket file-system switch. -->\n"
    "    <desktop6:FileSystemWriteVirtualization>disabled</desktop6:FileSystemWriteVirtualization>\n"
    "    <virtualization:FileSystemWriteVirtualization>\n"
    "      <virtualization:ExcludedDirectories>\n"
    f"        <virtualization:ExcludedDirectory>{DIRECT_EXCLUDED_DIRECTORY}</virtualization:ExcludedDirectory>\n"
    "      </virtualization:ExcludedDirectories>\n"
    "    </virtualization:FileSystemWriteVirtualization>"
)
_DIRECT_CAPABILITY = '    <rescap:Capability Name="unvirtualizedResources" />\n'


def _insert_once(text, anchor, insert, where):
    """`text` with `insert` placed before the one `anchor`; exit when there is not
    exactly one, so a template that lost its anchor cannot ship without the block."""
    if text.count(anchor) != 1:
        sys.exit(f"error: store/AppxManifest.xml.in must hold exactly one {anchor.strip()!r} "
                 f"for the direct channel's {where}, found {text.count(anchor)}")
    return text.replace(anchor, insert + anchor, 1)


def _declare_unvirtualized_data(manifest_text):
    """The direct channel's write-virtualization opt-out, inserted into a rendered
    manifest: the two namespaces on <Package> (and in IgnorableNamespaces, so a
    Windows version that does not know one ignores it), the two Properties
    elements, the capability."""
    found = re.findall(r'\n([ \t]*)IgnorableNamespaces="([^"]*)"', manifest_text)
    if len(found) != 1:
        sys.exit("error: store/AppxManifest.xml.in must hold exactly one IgnorableNamespaces "
                 f"attribute for the direct channel's namespaces, found {len(found)}")
    indent, prefixes = found[0]
    manifest_text = manifest_text.replace(
        f'\n{indent}IgnorableNamespaces="{prefixes}"',
        f'\n{indent}xmlns:desktop6="{_DESKTOP6_NS}"'
        f'\n{indent}xmlns:virtualization="{_VIRTUALIZATION_NS}"'
        f'\n{indent}IgnorableNamespaces="{prefixes} desktop6 virtualization"', 1)
    manifest_text = _insert_once(manifest_text, "\n  </Properties>", _DIRECT_PROPERTIES,
                                 "write-virtualization opt-out")
    return _insert_once(manifest_text, "  </Capabilities>", _DIRECT_CAPABILITY,
                        "unvirtualizedResources capability")


def resolve_publisher(channel, publisher, production=False, props=None):
    """The Identity/@Publisher a build of `channel` packs with.

    A dev build: `publisher` when given, else the dev DN (PackageIdentity.props).
    `production` names the direct channel's recorded publisher and nothing else
    (§ Direct download, 5): it refuses while the property is empty, beside another
    publisher, and on the Store channel, whose production identity is the Partner
    Center reservation passed as arguments."""
    if not production:
        return publisher or DEV_PUBLISHER
    if channel != "direct":
        sys.exit("error: --production names the direct channel's recorded publisher; a "
                 "Store build's production identity is the Partner Center reservation, "
                 "passed as --identity-name and --publisher")
    if publisher:
        sys.exit("error: a production direct build packs with the publisher recorded in "
                 "installer/PackageIdentity.props, never one passed with --publisher")
    return _windows_package_identity.require_direct_production_publisher(
        props or _windows_package_identity.PROPS)

# Package-relative payload the manifest references. Each entry is
# (path inside the package, path inside --payload-root).
PAYLOAD = (
    (os.path.join("App"), "App"),
    ("fauna-sync-agent.exe", "fauna-sync-agent.exe"),
    # shell-ext/Cargo.toml overrides [lib] name = "fauna_shell", so the build output
    # is fauna_shell.dll. Unlike the MSI's copy this is NOT version-stamped:
    # Path 1's stamped filename exists to dodge an MSI's inability to replace a DLL
    # Explorer holds open, and an MSIX is replaced atomically.
    ("fauna_shell.dll", "fauna_shell.dll"),
) + tuple((name, name) for name in (
    # The three licence files every shipped binary carries, at the package root
    # (docs/goal/architecture/shipped-notices.md § Per channel, the Microsoft Store
    # row). `just windows-store-package` stages them; the notices file is
    # dependency-authored text this script copies and never opens.
    "LICENSE-MIT.txt", "LICENSE-APACHE.txt", "THIRD-PARTY-NOTICES.txt",
))
NOTICE_FILES = PAYLOAD[-3:]


def _sdk_tool(name):
    """Newest Windows SDK <name>.exe for this host arch."""
    arch = "arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "x64"
    pat = rf"C:\Program Files (x86)\Windows Kits\10\bin\*\{arch}\{name}.exe"
    hits = sorted(glob.glob(pat))
    if not hits:
        sys.exit(f"error: {name}.exe not found under the Windows SDK ({pat})")
    return hits[-1]


def _product_version(package_wxs=None):
    """MSI ProductVersion (Package.wxs) → 4-part MSIX version.

    `package_wxs` names another tree's Package.wxs: the public Store workflow grades
    with the workflow commit's packer a package built from an older recorded commit,
    whose version is that commit's."""
    with open(package_wxs or os.path.join(INSTALLER, "Package.wxs"), encoding="utf-8") as f:
        m = re.search(r'\bVersion="(\d+\.\d+\.\d+)"', f.read())
    if not m:
        sys.exit("error: could not read Version from Package.wxs")
    return f"{m.group(1)}.0"


def _default_payload_root(arch):
    """Where `just windows-store-package` stages the shipped-flavour payload.

    Deliberately NOT the MSI's `build/installer/stage/<arch>/`: the only thing that
    writes there locally is `windows_installer.py::build_msi`, which compiles the e2e
    automation surface in on purpose. Defaulting to it once meant a bare run of this
    script packaged the test agent for upload.
    """
    return os.path.join(REPO, "build", "installer", "store-payload", arch)


def render_manifest(template, identity_name, publisher, version, arch, channel="store"):
    """Substitute the five build-time tokens; hard-fail on anything left over. On the
    direct channel, then insert its write-virtualization opt-out.

    Both directions are guarded: a token that is *missing* from the template would
    silently ship a hard-coded value, and a token that *survives* would ship a
    literal `@@…@@` into a package. The Store manifest is the template's own text
    with the token values in place, nothing added.
    """
    if channel not in CHANNELS:
        raise ValueError(f"channel must be one of {CHANNELS}, got {channel!r}")
    for token, value in (
        ("@@IdentityName@@", identity_name),
        ("@@Publisher@@", publisher),
        ("@@DisplayName@@", DISPLAY_NAME[channel]),
        ("@@PackageVersion@@", version),
        ("@@ProcessorArchitecture@@", arch),
    ):
        if token not in template:
            sys.exit(f"error: token {token} missing from store/AppxManifest.xml.in")
        template = template.replace(token, value)
    leftover = re.search(r"@@\w+@@", template)
    if leftover:
        sys.exit(f"error: unsubstituted token remains: {leftover.group(0)}")
    if channel == "direct":
        template = _declare_unvirtualized_data(template)
    return template


def _pack_argv(pack_dir, out_path, tool=None):
    """MakeAppx argv. Deliberately NO `/nv` — see the module docstring.

    `tool` is injectable so the flag-shape assertion stays fleet-wide. Resolving
    the Windows SDK is a win-only act, but "this script never passes /nv" is a
    claim about THIS FILE, and a guard that only runs on one of three machines
    is a guard that mostly does not run.
    """
    return [tool or _sdk_tool("makeappx"), "pack", "/o", "/d", pack_dir, "/p", out_path]


def _bundle_argv(packages_dir, out_path, version, tool=None):
    """MakeAppx argv bundling every package in `packages_dir` (the direct channel's
    two arch packages, alone in a directory of their own).

    `/bv` sets the bundle's version: without it MakeAppx derives one from the clock,
    and an update installs only over a lower version, so the bundle carries the
    product version (§ Direct download, 2). No `/nv`, as for a pack.
    """
    return [tool or _sdk_tool("makeappx"), "bundle", "/o", "/bv", version,
            "/d", packages_dir, "/p", out_path]


# Staged beside the binaries by the MSBuild publish, never shipped: symbols are
# dead weight in a package every user downloads, and they carry build-machine
# source paths and full symbol names for no user benefit. Excluded here rather
# than in the publish step so the payload tree stays debuggable on the box that
# built it. Pinned by test_store_package_pack.py::test_package_ships_no_debug_symbols.
_NOT_SHIPPED = shutil.ignore_patterns("*.pdb")

# Files the package cannot run without, beyond PAYLOAD's top-level entries. The
# app's native core is the one that can go missing silently: FaunaApp.csproj
# includes runtimes\<rid>\native\fauna_ffi.dll only `Condition="Exists(...)"`, so a
# publish that ran before the FFI was staged SUCCEEDS — and the app it produces
# dies at its first P/Invoke.
REQUIRED_FILES = (
    os.path.join("App", "FaunaApp.exe"),
    os.path.join("App", "fauna_ffi.dll"),
)

# The e2e automation surface, by what it leaves in our own two binaries (convention
# 15, docs/goal/architecture/e2e-automation-surface-gating.md: the surface is
# compiled out of release artifacts and verified absent by a strings scan). Both
# markers were measured on real Windows builds, 2026-10-07 — present in the test
# flavour, absent in production:
#   * FaunaApp.dll — the namespace `FaunaApp.Testing` in the metadata string heap.
#     Everything in it (TestAgent, PaintedErrorObserver) is `#if DEBUG ||
#     FAUNA_E2E_AGENT` whole: a Debug assembly carries it, a Release publish does
#     not. NOT a bare `TestAgent` substring — production ships an ungated member
#     named `StageImageFromTestAgent`, and the heap may store `TestAgent` as that
#     string's shared tail. NOT `ForTest` either: ungated helpers such as
#     `DevicesMachineHost.NotifyListenersForTest` ship in production.
#   * fauna_ffi.dll — an EXPORT named `*for_test*` (the UniFFI seam surface): 135
#     under `windows-ffi-test`, 0 under `windows-ffi`. Read from the export table,
#     never from raw bytes: the production dll still carries seam NAMES as data
#     (4 of them), which are not a surface.
# Scoped to our own files on purpose: the self-contained publish carries hundreds
# of runtime and Windows App SDK binaries whose strings are not ours to grade.
_APP_ASSEMBLY = os.path.join("App", "FaunaApp.dll")
_APP_TEST_NAMESPACE = b"FaunaApp.Testing\x00"
_FFI_DLL = os.path.join("App", "fauna_ffi.dll")


def _pe_layout(data):
    """`(machine, data_directory(index) -> rva, offset(rva) -> file offset)` of a PE
    image, or None when `data` is not one. Raises struct.error/ValueError on a
    truncated or inconsistent image.

    Standard library only — the packer ships publicly and must not need a binary
    toolchain to read its own payload.
    """
    if data[:2] != b"MZ":
        return None
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        return None
    coff = pe + 4
    machine, nsect = struct.unpack_from("<HH", data, coff)
    (opt_size,) = struct.unpack_from("<H", data, coff + 16)
    opt = coff + 20
    (magic,) = struct.unpack_from("<H", data, opt)
    dirs = opt + (112 if magic == 0x20B else 96)            # PE32+ : PE32
    (ndirs,) = struct.unpack_from("<I", data, dirs - 4)
    sections = [struct.unpack_from("<IIII", data, opt + opt_size + 40 * i + 8)
                for i in range(nsect)]                      # vsize, rva, raw size, raw ptr

    def directory(index):
        return struct.unpack_from("<I", data, dirs + 8 * index)[0] if index < ndirs else 0

    def offset(rva):
        for vsize, va, raw_size, raw_ptr in sections:
            if va <= rva < va + max(vsize, raw_size):
                return raw_ptr + rva - va
        raise ValueError(f"rva {rva:#x} outside every section")

    return machine, directory, offset


def pe_export_names(path):
    """The export names of a PE image; [] when `path` is not a PE with an export table."""
    try:
        with open(path, "rb") as f:
            data = f.read()
        layout = _pe_layout(data)
        if layout is None:
            return []
        _, directory, offset = layout
        exp_rva = directory(0)
        if not exp_rva:
            return []
        exp = offset(exp_rva)
        nnames, names_rva = struct.unpack_from("<I", data, exp + 24)[0], \
            struct.unpack_from("<I", data, exp + 32)[0]
        table = offset(names_rva) if nnames else 0
        names = []
        for i in range(nnames):
            start = offset(struct.unpack_from("<I", data, table + 4 * i)[0])
            names.append(data[start:data.index(b"\0", start)].decode("ascii", "replace"))
        return names
    except (OSError, struct.error, ValueError):
        return []


def automation_surface_hits(payload_root):
    """Every place the e2e automation surface shows in the payload's own binaries."""
    hits = []
    app = os.path.join(payload_root, _APP_ASSEMBLY)
    if os.path.isfile(app):
        with open(app, "rb") as f:
            if _APP_TEST_NAMESPACE in f.read():
                hits.append("App/FaunaApp.dll carries the gated FaunaApp.Testing namespace "
                            "(TestAgent compiled in)")
    seams = [n for n in pe_export_names(os.path.join(payload_root, _FFI_DLL))
             if "for_test" in n.lower()]
    if seams:
        hits.append(f"App/fauna_ffi.dll exports {len(seams)} test seam(s), e.g. {seams[0]}")
    return hits


# ── Grading a packed upload ──────────────────────────────────────────────────────
#
# `--grade` reads a finished .msix back and checks what a Store upload must be. The
# packer SETS identity from its arguments; the grade compares the package with what
# the caller states independently, so a wrong argument or a wrong payload cannot
# reach the Store looking fine. The Store-upload workflow runs it on every package
# it builds, and a hand build runs it too (installers/windows.md § Store
# distribution (MSIX)).

# IMAGE_FILE_MACHINE_* per Store arch.
_PE_MACHINE = {"x64": 0x8664, "arm64": 0xAA64}
_PE_MACHINE_I386 = 0x14C
_CLR_DIRECTORY = 14
_LOAD_CONFIG_DIRECTORY = 10
_LOAD_CONFIG_CHPE_METADATA = 0xC8   # IMAGE_LOAD_CONFIG_DIRECTORY64.CHPEMetadataPointer
_COMIMAGE_FLAGS_ILONLY = 0x1
_COMIMAGE_FLAGS_32BITREQUIRED = 0x2
# Every capability a Store upload declares. Owner of the ruling: installers/windows.md
# § Data continuity (`unvirtualizedResources` dropped 2026-08-23); the template test
# pins the same set on the source side. The direct package adds the one its
# write-virtualization opt-out needs (§ Direct download, 5).
STORE_CAPABILITIES = {"runFullTrust"}
CHANNEL_CAPABILITIES = {"store": STORE_CAPABILITIES,
                        "direct": STORE_CAPABILITIES | {"unvirtualizedResources"}}
_MANIFEST_NS = "{http://schemas.microsoft.com/appx/manifest/foundation/windows10}"
_DESKTOP6 = "{" + _DESKTOP6_NS + "}"
_VIRTUALIZATION = "{" + _VIRTUALIZATION_NS + "}"
_BUNDLE_NS = "{http://schemas.microsoft.com/appx/2013/bundle}"

# The Store package's flavour (installers/windows.md § Store distribution, the
# flavour paragraph; dynamic-features.md § The App-Store escape hatch -> *The store
# flavors*): `store-no-payments` -- the payments plane and zaps compiled away,
# peer-to-peer sharing kept -- from the first version whose recorded commit carries
# the Windows leg; `store-safe` (sharing gone too, owner ruling 2026-10-14) for a
# commit older than that. `full` is the default-feature build every direct download
# carries (installers/windows.md § Direct download, 4): the direct package's one
# flavour, every plane present. What a core carries or must not is the native
# column of `just windows-store-safe-check` (its PAY_KIND_PATS and P2P_NATIVE_PATS):
# the kind strings and the UniFFI names of each plane, all plain UTF-8 in a Rust
# cdylib. STORE_FLAVOUR is the default of `--flavour` and of the public Store
# workflow's `flavour` input (test_public_workflows_shape.py holds the two together).
PAYMENTS_NATIVE = (b"fauna.payments.", b"fauna.tips.", b"fauna.nostr.zap_signers.")
P2P_SHARE_NATIVE = (
    b"uniffi_fauna_ffi_fn_func_offline_share_", b"FfiCeremonySeat",
    b"share_plane_view", b"FfiSharePlaneView",
)
STORE_SAFE_ABSENT = PAYMENTS_NATIVE + P2P_SHARE_NATIVE
# flavour -> (markers its core must NOT carry, markers it MUST carry). A flavour that
# keeps a plane asserts it present: absence alone would pass a core that fell back to
# store-safe, the flavour store-no-payments exists to differ from -- and a full core
# that fell back to a store flavour (`just windows-store-safe-check`'s `default`
# column asserts every one of the seven present).
FLAVOUR_CORE = {
    "store-no-payments": (PAYMENTS_NATIVE, P2P_SHARE_NATIVE),
    "store-safe": (STORE_SAFE_ABSENT, ()),
    "full": ((), STORE_SAFE_ABSENT),
}
FLAVOURS = tuple(FLAVOUR_CORE)
STORE_FLAVOUR = "store-no-payments"
# Each channel's default flavour, and the flavours it may be graded as: the Store's
# store flavours (and `full`, should a ruling return the Store to it), the direct
# package's `full` alone.
CHANNEL_FLAVOUR = {"store": STORE_FLAVOUR, "direct": "full"}
CHANNEL_FLAVOURS = {"store": FLAVOURS, "direct": ("full",)}


def pe_arch_problem(data, arch):
    """Why the PE image `data` cannot ship in the `arch` package, or None.

    A native image must carry the package's machine type. A managed AnyCPU assembly
    (i386 machine type, a CLR header flagged ILONLY and not 32BITREQUIRED) runs on
    either arch, so it passes whatever the package's arch. An ARM64X hybrid image (ARM64
    machine type with CHPE metadata in its load config) carries ARM64EC code for x64
    processes on ARM64 Windows, so it ships in the x64 package -- the Windows App SDK's
    x64 runtime carries such images (Microsoft.Windows.Workloads.Resources_ec.dll).
    """
    try:
        layout = _pe_layout(data)
        if layout is None:
            return "is not a PE image"
        machine, directory, offset = layout
        if machine == _PE_MACHINE[arch]:
            return None
        if arch == "x64" and machine == _PE_MACHINE["arm64"]:
            lc_rva = directory(_LOAD_CONFIG_DIRECTORY)
            if lc_rva:
                lc = offset(lc_rva)
                (lc_size,) = struct.unpack_from("<I", data, lc)
                if lc_size >= _LOAD_CONFIG_CHPE_METADATA + 8 and \
                        struct.unpack_from("<Q", data, lc + _LOAD_CONFIG_CHPE_METADATA)[0]:
                    return None
        clr_rva = directory(_CLR_DIRECTORY)
        if machine == _PE_MACHINE_I386 and clr_rva:
            (flags,) = struct.unpack_from("<I", data, offset(clr_rva) + 16)
            if flags & _COMIMAGE_FLAGS_ILONLY and not flags & _COMIMAGE_FLAGS_32BITREQUIRED:
                return None
        return f"has machine type {machine:#06x}, not {arch}'s {_PE_MACHINE[arch]:#06x}"
    except (struct.error, ValueError) as e:
        return f"is a malformed PE image ({e})"


def _virtualization_problems(root, channel):
    """Why the packaged manifest's write-virtualization declarations are not the
    `channel`'s: none for the Store (§ Data continuity), exactly the opt-out for
    %LocalAppData%\\Fauna in both forms for the direct package (§ Direct download, 5)."""
    props = root.find(f"{_MANIFEST_NS}Properties")
    props = props if props is not None else []
    blanket = [(e.text or "").strip() for e in props if e.tag == f"{_DESKTOP6}FileSystemWriteVirtualization"]
    scoped = [e for e in props if e.tag == f"{_VIRTUALIZATION}FileSystemWriteVirtualization"]
    excluded = [(d.text or "").strip() for e in scoped
                for d in e.iter(f"{_VIRTUALIZATION}ExcludedDirectory")]
    registry = [e.tag for e in props if e.tag.endswith("}RegistryWriteVirtualization")]
    problems = []
    if registry:
        problems.append("Properties declares registry write virtualization, which no "
                        "channel opts out of")
    if channel == "store":
        if blanket or scoped:
            problems.append("Properties declares a write-virtualization opt-out; a Store "
                            "upload declares none (installers/windows.md § Data continuity)")
        return problems
    if blanket != ["disabled"]:
        problems.append(f"desktop6:FileSystemWriteVirtualization is {blanket}, the direct "
                        "package declares it once, `disabled`, for the Windows versions "
                        "before 11")
    if len(scoped) != 1 or excluded != [DIRECT_EXCLUDED_DIRECTORY]:
        problems.append(f"virtualization:FileSystemWriteVirtualization excludes {excluded}, "
                        f"the direct package excludes exactly [{DIRECT_EXCLUDED_DIRECTORY!r}]")
    return problems


def grade_package(msix_path, arch, identity_name, publisher, version, flavour=None,
                  channel="store"):
    """Every reason the package at `msix_path` (a path or an open binary file) is not
    a valid `arch` package of the `channel` -- a Store upload, or a direct-download
    package -- of the `flavour` build (default: the channel's)."""
    import tempfile
    import xml.etree.ElementTree as ET
    import zipfile

    if channel not in CHANNELS:
        raise ValueError(f"channel must be one of {CHANNELS}, got {channel!r}")
    if flavour is None:
        flavour = CHANNEL_FLAVOUR[channel]
    if flavour not in CHANNEL_FLAVOURS[channel]:
        raise ValueError(f"a {channel} package's flavour is one of {CHANNEL_FLAVOURS[channel]}, "
                         f"got {flavour!r}")
    what = "an upload" if channel == "store" else "a direct package"
    problems = []
    with zipfile.ZipFile(msix_path) as z:
        names = z.namelist()
        try:
            root = ET.fromstring(z.read("AppxManifest.xml"))
        except (KeyError, ET.ParseError) as e:
            return [f"AppxManifest.xml is missing or unreadable ({e})"]

        identity = root.find(f"{_MANIFEST_NS}Identity")
        want = {"Name": identity_name, "Publisher": publisher, "Version": version,
                "ProcessorArchitecture": arch}
        got = {k: (identity.get(k) if identity is not None else None) for k in want}
        if got != want:
            problems.append(f"identity is {got}, {what} carries {want}")

        display = root.find(f"{_MANIFEST_NS}Properties/{_MANIFEST_NS}DisplayName")
        display = display.text if display is not None else None
        if display != DISPLAY_NAME[channel]:
            problems.append(f"Properties/DisplayName is {display!r}, {what} names itself "
                            f"{DISPLAY_NAME[channel]!r}")

        caps = root.find(f"{_MANIFEST_NS}Capabilities")
        declared = {c.get("Name") for c in (caps if caps is not None else ())}
        if declared != CHANNEL_CAPABILITIES[channel]:
            problems.append(f"capabilities are {sorted(declared)}, {what} declares "
                            f"exactly {sorted(CHANNEL_CAPABILITIES[channel])}")

        problems += _virtualization_problems(root, channel)

        # Present and non-empty, read by size only: the notices file is
        # dependency-authored text (shipped-notices.md § How the notices file is
        # produced).
        sizes = {i.filename: i.file_size for i in z.infolist()}
        for inside, _ in NOTICE_FILES:
            if not sizes.get(inside):
                problems.append(f"{inside} is {'empty' if inside in sizes else 'missing'} at the "
                                "package root; every shipped package carries the two licence texts "
                                "and its notices file")

        for name in names:
            lower = name.lower()
            if lower.endswith(".pdb"):
                problems.append(f"{name}: debug symbols (.pdb) never ship")
            elif lower.endswith((".exe", ".dll")):
                why = pe_arch_problem(z.read(name), arch)
                if why:
                    problems.append(f"{name} {why}")

        # The automation-surface scan reads files on disk; give it ours.
        with tempfile.TemporaryDirectory() as tmp:
            for rel in (_APP_ASSEMBLY, _FFI_DLL):
                member = rel.replace(os.sep, "/")
                if member in names:
                    dst = os.path.join(tmp, rel)
                    os.makedirs(os.path.dirname(dst), exist_ok=True)
                    with open(dst, "wb") as f:
                        f.write(z.read(member))
            problems += [f"automation surface: {hit}" for hit in automation_surface_hits(tmp)]

        absent, present = FLAVOUR_CORE[flavour]
        if absent or present:
            ffi = _FFI_DLL.replace(os.sep, "/")
            if ffi not in names:
                # An absent core is an empty scan, which reads as a clean excision.
                problems.append(f"{ffi} is missing, so the {flavour} excision cannot be graded")
            else:
                data = z.read(ffi)
                for marker in absent:
                    n = data.count(marker)
                    if n:
                        problems.append(f"{ffi} carries {n} '{marker.decode()}' occurrence(s); "
                                        f"a {flavour} core carries none of the planes it excises")
                for marker in present:
                    if marker not in data:
                        problems.append(f"{ffi} carries no '{marker.decode()}'; a {flavour} core "
                                        "keeps that plane, so this one fell back to another "
                                        "flavour")
    return problems


def grade_bundle(bundle_path, identity_name, publisher, version, flavour=None,
                 channel="direct"):
    """Every reason the bundle at `bundle_path` is not the direct download's
    `Fauna.msixbundle`: its identity at the product version, exactly one
    application package per arch of ARCHES, and each of them graded as a direct
    package of its arch (a finding inside one is prefixed with its file name)."""
    import xml.etree.ElementTree as ET
    import zipfile

    if channel != "direct":
        raise ValueError("only the direct download is a bundle; a Store upload is one "
                         "package per arch")
    problems = []
    with zipfile.ZipFile(bundle_path) as z:
        names = set(z.namelist())
        try:
            root = ET.fromstring(z.read("AppxMetadata/AppxBundleManifest.xml"))
        except (KeyError, ET.ParseError) as e:
            return [f"AppxMetadata/AppxBundleManifest.xml is missing or unreadable ({e})"]
        identity = root.find(f"{_BUNDLE_NS}Identity")
        want = {"Name": identity_name, "Publisher": publisher, "Version": version}
        got = {k: (identity.get(k) if identity is not None else None) for k in want}
        if got != want:
            problems.append(f"bundle identity is {got}, the direct download carries {want}")

        by_arch = {}
        for package in root.iter(f"{_BUNDLE_NS}Package"):
            if package.get("Type") == "application":
                by_arch.setdefault(package.get("Architecture"), []).append(package.get("FileName"))
        for arch in ARCHES:
            if len(by_arch.get(arch, [])) != 1:
                problems.append(f"the bundle lists {len(by_arch.get(arch, []))} {arch} "
                                "package(s), the direct download carries exactly one")
        for arch in sorted(set(by_arch) - set(ARCHES)):
            problems.append(f"the bundle lists a {arch} package; it carries {list(ARCHES)} only")
        for arch in ARCHES:
            for fname in by_arch.get(arch, [])[:1]:
                if fname not in names:
                    problems.append(f"{fname} is listed in the bundle manifest and missing from the bundle")
                    continue
                inner = grade_package(io.BytesIO(z.read(fname)), arch, identity_name, publisher,
                                      version, flavour=flavour, channel="direct")
                problems += [f"{fname}: {p}" for p in inner]
    return problems


def _stage(pack_dir, payload_root, manifest_text, allow_test_surface=False):
    """Lay out exactly what goes into the package."""
    missing = [rel for rel in REQUIRED_FILES
               if not os.path.isfile(os.path.join(payload_root, rel))]
    if missing:
        sys.exit("error: payload is missing what the app cannot run without:\n  " +
                 "\n  ".join(os.path.join(payload_root, rel) for rel in missing) +
                 "\n(`just windows-store-package` builds and stages the whole payload)")
    hits = automation_surface_hits(payload_root)
    if hits and not allow_test_surface:
        sys.exit("error: the payload carries the e2e automation surface — a TEST-flavour\n"
                 "build, never an upload (convention 15):\n  " + "\n  ".join(hits) +
                 "\nBuild the shipped flavour with `just windows-store-package`. A dev or\n"
                 "test package that really wants it passes --allow-test-surface.")

    shutil.rmtree(pack_dir, ignore_errors=True)
    os.makedirs(pack_dir, exist_ok=True)

    with open(os.path.join(pack_dir, "AppxManifest.xml"), "w", encoding="utf-8") as f:
        f.write(manifest_text)

    if not os.path.isdir(ASSETS):
        sys.exit(f"error: brand assets missing: {ASSETS}\n"
                 "       they are checked in — restore them from version control")
    shutil.copytree(ASSETS, os.path.join(pack_dir, "Assets"))

    missing = []
    for inside, outside in PAYLOAD:
        src = os.path.join(payload_root, outside)
        dst = os.path.join(pack_dir, inside)
        if os.path.isdir(src):
            shutil.copytree(src, dst, ignore=_NOT_SHIPPED)
        elif os.path.isfile(src):
            os.makedirs(os.path.dirname(dst) or pack_dir, exist_ok=True)
            shutil.copy2(src, dst)
        else:
            missing.append(src)
    if missing:
        sys.exit("error: payload missing from --payload-root:\n  " +
                 "\n  ".join(missing) +
                 "\n(`just windows-store-package` builds and stages the whole payload)")


def _self_sign(out_path, publisher):
    """Dev-only. The Store signs real uploads; this exists for local registration."""
    pfx = os.path.join(REPO, "build", "store-dev.pfx")
    # NB: no backtick line-continuations — they get mangled when this is handed to
    # powershell through Git Bash. One statement per line.
    ps = "; ".join([
        "$ErrorActionPreference = 'Stop'",
        f"$subject = '{publisher}'",
        "$cert = Get-ChildItem Cert:\\CurrentUser\\My | "
        "Where-Object { $_.Subject -eq $subject } | Select-Object -First 1",
        "if (-not $cert) { $cert = New-SelfSignedCertificate -Type Custom -Subject $subject "
        "-KeyUsage DigitalSignature "
        f"-FriendlyName '{DEV_CERT_FRIENDLY}' "
        "-CertStoreLocation 'Cert:\\CurrentUser\\My' "
        "-TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3','2.5.29.19={text}') }",
        "Write-Host ('dev cert ' + $cert.Thumbprint)",
        f"$pw = ConvertTo-SecureString -String '{DEV_PFX_PASSWORD}' -Force -AsPlainText",
        f"Export-PfxCertificate -Cert $cert -FilePath '{pfx}' -Password $pw | Out-Null",
    ])
    # `pwsh` (7), NOT `powershell` (5.1): Windows PowerShell on win-arm64 has no Cert:
    # drive, so every certificate step silently fails there.
    subprocess.run(["pwsh", "-NoProfile", "-Command", ps], check=True)
    subprocess.run(
        [_sdk_tool("signtool"), "sign", "/fd", "SHA256", "/f", pfx, "/p", DEV_PFX_PASSWORD,
         out_path],
        check=True,
    )
    return pfx


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--channel", choices=CHANNELS, default="store",
                    help="`store` (the default): one Fauna-Store-<arch>.msix per arch, "
                         "unsigned, for the Store; `direct`: the direct-download "
                         f"{DIRECT_BUNDLE}, both arches in one bundle, identity "
                         f"{DIRECT_IDENTITY_NAME}, the write-virtualization opt-out for "
                         "%%LocalAppData%%\\Fauna (installers/windows.md § Direct download)")
    ap.add_argument("--arch", default=",".join(ARCHES),
                    help=f"comma-separated subset of {','.join(ARCHES)} "
                         "(default: both, which is what a Store release uploads; the "
                         "direct channel always packs both)")
    ap.add_argument("--identity-name", default=None,
                    help="Identity/@Name — for the Store, assigned by Partner Center at "
                         f"name reservation (default placeholder: {DEV_IDENTITY_NAME!r}); "
                         f"for the direct channel, {DIRECT_IDENTITY_NAME!r} (the default)")
    ap.add_argument("--publisher", default=None,
                    help="Identity/@Publisher — Partner Center's CN=<GUID>, or the "
                         "dev signing cert's exact Subject DN "
                         f"(default: {DEV_PUBLISHER!r})")
    ap.add_argument("--production", action="store_true",
                    help="direct channel only: pack (or grade) with the production "
                         "publisher recorded in installer/PackageIdentity.props "
                         f"({_windows_package_identity.DIRECT_PRODUCTION_PROPERTY}); refused "
                         "while it is empty, with --publisher, and with --self-sign")
    ap.add_argument("--payload-root", default=None,
                    help="staged payload dir, every arch's (default: "
                         "build/installer/store-payload/<arch>, where "
                         "`just windows-store-package` stages it)")
    ap.add_argument("--out-dir", default=os.path.join(REPO, "build", "installer"))
    ap.add_argument("--self-sign", action="store_true",
                    help="create/reuse a dev self-signed cert and sign each package, or "
                         "the direct channel's bundle (dev loop only — Store uploads are "
                         "unsigned)")
    ap.add_argument("--allow-test-surface", action="store_true",
                    help="pack a payload that carries the e2e automation surface "
                         "(the MSI test helper's stage). Dev/test packages only — "
                         "NEVER an upload; without it such a payload is refused")
    ap.add_argument("--grade", metavar="MSIX", default=None,
                    help="grade an already-packed package instead of building one: "
                         "identity (the identity name and publisher passed here, the "
                         "Package.wxs version, the one --arch), Properties/DisplayName, "
                         "the channel's capabilities (runFullTrust alone for the Store, "
                         "with unvirtualizedResources for the direct channel) and "
                         "write-virtualization declarations, every PE built for that "
                         "arch, no .pdb, no automation surface, and the --flavour's "
                         f"planes. On the direct channel, a {DIRECT_BUNDLE} is graded "
                         "whole: its identity, one package per arch, each graded. Exit 1 "
                         "on any finding")
    ap.add_argument("--flavour", choices=FLAVOURS, default=None,
                    help="with --grade: the build the package must be. "
                         f"`{STORE_FLAVOUR}` (the Store's, its default) refuses a "
                         "fauna_ffi.dll carrying the payments or zaps plane and one "
                         "missing the peer-to-peer sharing plane; `store-safe` (a "
                         "recorded commit older than the Windows store-no-payments "
                         "leg) refuses all three planes; `full` (the direct channel's, "
                         "its default and only one) refuses a core missing any of them")
    ap.add_argument("--package-wxs", default=None,
                    help="with --grade: the Package.wxs whose version the package must "
                         "carry (default: this tree's)")
    ap.add_argument("--stage-only", metavar="DIR", default=None,
                    help="render the channel's manifest and lay out ONE --arch's package "
                         "in DIR from --payload-root, then stop: no pack, no signature, "
                         "no bundle. For a loose `Add-AppxPackage -Register "
                         "DIR/AppxManifest.xml` under Developer Mode (the dev-box "
                         "shell-extension observation harness)")
    args = ap.parse_args()

    channel = args.channel
    arches = [a.strip() for a in args.arch.split(",") if a.strip()]
    unknown = [a for a in arches if a not in ARCHES]
    if unknown:
        sys.exit(f"error: unknown arch {unknown}; supported: {list(ARCHES)}")
    if args.production and args.self_sign:
        sys.exit("error: a --production build is never --self-sign'ed: a self-signed "
                 "certificate for the recorded production subject would be a certificate "
                 "in the organisation's name that nobody issued")
    if args.flavour and args.flavour not in CHANNEL_FLAVOURS[channel]:
        sys.exit(f"error: a {channel} package's flavour is one of "
                 f"{list(CHANNEL_FLAVOURS[channel])}, got {args.flavour!r}")
    if args.stage_only and (args.grade or args.self_sign or len(arches) != 1):
        sys.exit("error: --stage-only lays out one package and stops: name its one --arch, "
                 "and leave out --grade and --self-sign")
    bundle_grade = bool(args.grade) and args.grade.lower().endswith(".msixbundle")
    package_grade = bool(args.grade) and not bundle_grade
    if channel == "direct" and sorted(arches) != sorted(ARCHES) and not (
            package_grade or args.stage_only):
        sys.exit(f"error: the direct download is one bundle of both arches {list(ARCHES)}; "
                 f"--arch {args.arch} names a subset")

    identity_name = args.identity_name or (
        DIRECT_IDENTITY_NAME if channel == "direct" else DEV_IDENTITY_NAME)
    publisher = resolve_publisher(channel, args.publisher, production=args.production)
    flavour = args.flavour or CHANNEL_FLAVOUR[channel]

    version = _product_version(args.package_wxs if args.grade else None)
    if args.grade:
        if bundle_grade:
            if channel != "direct":
                sys.exit("error: only the direct download is a bundle; grade it with "
                         "--channel direct")
            problems = grade_bundle(args.grade, identity_name, publisher, version,
                                    flavour=flavour)
            subject = f"the {flavour} direct-download bundle ({' + '.join(ARCHES)})"
        else:
            if len(arches) != 1:
                sys.exit("error: --grade grades one package; name its one --arch")
            problems = grade_package(args.grade, arches[0], identity_name, publisher,
                                     version, flavour=flavour, channel=channel)
            subject = (f"a {arches[0]} {flavour} "
                       + ("upload" if channel == "store" else "direct-download package"))
        for p in problems:
            print(f"GRADE FAIL: {p}")
        if problems:
            return 1
        print(f"GRADE OK: {args.grade} is {subject} of {identity_name} {version}, "
              f"publisher {publisher}")
        return 0

    with open(os.path.join(STORE, "AppxManifest.xml.in"), encoding="utf-8") as f:
        template = f.read()

    print(f"channel  : {channel}")
    print(f"version  : {version}   (from Package.wxs)")
    print(f"identity : {identity_name}")
    print(f"publisher: {publisher}" + ("   (production, PackageIdentity.props)"
                                       if args.production else ""))

    if args.stage_only:
        arch = arches[0]
        payload_root = args.payload_root or _default_payload_root(arch)
        _stage(args.stage_only, payload_root,
               render_manifest(template, identity_name, publisher, version, arch,
                               channel=channel),
               allow_test_surface=args.allow_test_surface)
        print(f"staged   : {args.stage_only}   ({arch}; Add-AppxPackage -Register its "
              "AppxManifest.xml)")
        return 0

    os.makedirs(args.out_dir, exist_ok=True)
    # The direct channel's per-arch packages go alone into a directory of their
    # own, which `makeappx bundle` reads whole; it is emptied first, so a bundle
    # never carries a package a previous run left behind.
    packages_dir = os.path.join(args.out_dir, "direct-packages")
    if channel == "direct":
        shutil.rmtree(packages_dir, ignore_errors=True)
        os.makedirs(packages_dir)
    built = []
    for arch in arches:
        payload_root = args.payload_root or _default_payload_root(arch)
        # Staging lives UNDER --out-dir, not at a fixed repo path, one directory per
        # channel. `_stage` rmtree's this directory, so a fixed path meant any caller
        # that redirected only its --out-dir still silently wiped and rewrote the
        # shared loose layout with its own identity. That is not hypothetical:
        # test_store_package_pack.py did exactly that to the developer's staged
        # Store layout, leaving a correct .msix beside a layout describing a FAKE
        # identity — and `Add-AppxPackage -Register` consumes the layout, so a
        # human registering "the Store package" got the test one, silently.
        # Deriving it here makes isolation automatic for every caller instead of
        # something each one must remember; the channel in the name keeps a direct
        # build from rewriting the Store's layout the same way.
        pack_dir = os.path.join(args.out_dir, f"{channel}-stage", arch)
        if channel == "store":
            out_path = os.path.join(args.out_dir, f"Fauna-Store-{arch}.msix")
        else:
            out_path = os.path.join(packages_dir, _direct_package_name(arch))

        print(f"\n── {arch} ──")
        print(f"payload  : {payload_root}")
        print(f"staged   : {pack_dir}   (Add-AppxPackage -Register this)")
        _stage(pack_dir, payload_root,
               render_manifest(template, identity_name, publisher, version, arch,
                               channel=channel),
               allow_test_surface=args.allow_test_surface)
        subprocess.run(_pack_argv(pack_dir, out_path), check=True)
        print(f"packed   : {out_path}")

        if args.self_sign and channel == "store":
            pfx = _self_sign(out_path, publisher)
            print(f"signed   : {out_path}")
        built.append(out_path)

    if channel == "direct":
        # The bundle alone is signed: its signature covers the packages inside it.
        bundle = os.path.join(args.out_dir, DIRECT_BUNDLE)
        subprocess.run(_bundle_argv(packages_dir, bundle, version), check=True)
        print(f"\nbundled  : {bundle}")
        built = [bundle]
        if args.self_sign:
            pfx = _self_sign(bundle, publisher)
            print(f"signed   : {bundle}")

    if not args.self_sign:
        if channel == "store":
            print("\nNot signed — which is correct for a Store upload: the Store signs.")
        else:
            print("\nNot signed: the direct download is signed by the release workflow's "
                  "signing leg, with a certificate in the organisation's own name.")
        print("For local registration, re-run with the self-sign flag.")
        return 0

    print("\nTo install (ELEVATED PowerShell — trusting a cert machine-wide needs admin):")
    print(f"  Import-PfxCertificate -FilePath '{pfx}' "
          f"-CertStoreLocation Cert:\\LocalMachine\\TrustedPeople "
          f"-Password (ConvertTo-SecureString '{DEV_PFX_PASSWORD}' -AsPlainText -Force)")
    for path in built:
        print(f"  Add-AppxPackage -Path '{path}'")
    print("\n⚠ A dev-signed package is a DIFFERENT PackageFamilyName than the "
          "Store-signed one\n  (Publisher feeds the PFN hash). Remove-AppxPackage the "
          "dev one; never expect\n  an in-place upgrade across the identity swap.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

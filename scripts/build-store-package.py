#!/usr/bin/env python3
"""Build (and optionally sign) the Fauna **Microsoft Store** package — a full MSIX.

    just windows-store-package arm64 --identity-name … --publisher …   # what an upload runs
    python scripts/build-store-package.py --arch arm64        # pack an already-staged payload
    python scripts/build-store-package.py --arch arm64 --self-sign

This script PACKS. The payload it packs — the self-contained WinUI publish, the sync
agent and the shell extension, all in their shipped flavour — is built and staged by
`just windows-store-package`, which then calls this script. The MSI test helper
(`test_installer.py::_build_msi`) also stages a payload, but a deliberately
TEST-flavoured one (the e2e agent compiled in), so this script refuses it unless a
caller passes `--allow-test-surface` (§ the automation-surface check below).

Sibling of `build-sparse-package.py`, same conventions — but a *different kind of
package*. The sparse one is a manifest-only identity carrier whose payload lives
outside it (`uap10:AllowExternalContent`); that shape cannot be Store-distributed.
This one carries its binaries inside, which is what earns Store re-signing: free, no
certificate to manage, no SmartScreen warning. Design of record:
docs/goal/architecture/installers/windows.md § Store distribution (MSIX).

Two consequences of "content is internal" show up directly below:

  * **No `/nv`.** The sparse build disables MakeAppx path validation because its
    manifest references files that are deliberately absent from the package. Here
    every referenced path must resolve, so validation is exactly the check that
    catches a payload we forgot to stage — leaving it on is the point.
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
# commits them) and one home, beside the sparse manifest. Both MSIX packages pack the
# same PNGs — a second copy here
# would fork the brand mark the moment either is regenerated.
ASSETS = os.path.join(INSTALLER, "sparse", "Assets")

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

# Package-relative payload the manifest references. Each entry is
# (path inside the package, path inside --payload-root).
PAYLOAD = (
    (os.path.join("App"), "App"),
    ("fauna-sync-agent.exe", "fauna-sync-agent.exe"),
    # shell-ext/Cargo.toml overrides [lib] name = "fauna_shell", so the build output
    # is fauna_shell.dll. Unlike the MSI/sparse pair this is NOT version-stamped:
    # Path 1's stamped filename exists to dodge an MSI's inability to replace a DLL
    # Explorer holds open, and an MSIX is replaced atomically.
    ("fauna_shell.dll", "fauna_shell.dll"),
)


def _sdk_tool(name):
    """Newest Windows SDK <name>.exe for this host arch."""
    arch = "arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "x64"
    pat = rf"C:\Program Files (x86)\Windows Kits\10\bin\*\{arch}\{name}.exe"
    hits = sorted(glob.glob(pat))
    if not hits:
        sys.exit(f"error: {name}.exe not found under the Windows SDK ({pat})")
    return hits[-1]


def _product_version():
    """MSI ProductVersion (Package.wxs) → 4-part MSIX version."""
    with open(os.path.join(INSTALLER, "Package.wxs"), encoding="utf-8") as f:
        m = re.search(r'\bVersion="(\d+\.\d+\.\d+)"', f.read())
    if not m:
        sys.exit("error: could not read Version from Package.wxs")
    return f"{m.group(1)}.0"


def _default_payload_root(arch):
    """Where `just windows-store-package` stages the shipped-flavour payload.

    Deliberately NOT the MSI's `build/installer/stage/<arch>/`: the only thing that
    writes there locally is `test_installer.py::_build_msi`, which compiles the e2e
    automation surface in on purpose. Defaulting to it once meant a bare run of this
    script packaged the test agent for upload.
    """
    return os.path.join(REPO, "build", "installer", "store-payload", arch)


def render_manifest(template, identity_name, publisher, version, arch):
    """Substitute the four build-time tokens; hard-fail on anything left over.

    Both directions are guarded: a token that is *missing* from the template would
    silently ship a hard-coded value, and a token that *survives* would ship a
    literal `@@…@@` into a package.
    """
    for token, value in (
        ("@@IdentityName@@", identity_name),
        ("@@Publisher@@", publisher),
        ("@@PackageVersion@@", version),
        ("@@ProcessorArchitecture@@", arch),
    ):
        if token not in template:
            sys.exit(f"error: token {token} missing from store/AppxManifest.xml.in")
        template = template.replace(token, value)
    leftover = re.search(r"@@\w+@@", template)
    if leftover:
        sys.exit(f"error: unsubstituted token remains: {leftover.group(0)}")
    return template


def _pack_argv(pack_dir, out_path, tool=None):
    """MakeAppx argv. Deliberately NO `/nv` — see the module docstring.

    `tool` is injectable so the flag-shape assertion stays fleet-wide. Resolving
    the Windows SDK is a win-only act, but "this script never passes /nv" is a
    claim about THIS FILE, and a guard that only runs on one of three machines
    is a guard that mostly does not run.
    """
    return [tool or _sdk_tool("makeappx"), "pack", "/o", "/d", pack_dir, "/p", out_path]


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


def pe_export_names(path):
    """The export names of a PE image; [] when `path` is not a PE with an export table.

    Standard library only — the packer ships publicly and must not need a binary
    toolchain to read its own payload.
    """
    try:
        with open(path, "rb") as f:
            data = f.read()
        if data[:2] != b"MZ":
            return []
        pe = struct.unpack_from("<I", data, 0x3C)[0]
        if data[pe:pe + 4] != b"PE\0\0":
            return []
        coff = pe + 4
        (nsect,) = struct.unpack_from("<H", data, coff + 2)
        (opt_size,) = struct.unpack_from("<H", data, coff + 16)
        opt = coff + 20
        (magic,) = struct.unpack_from("<H", data, opt)
        dirs = opt + (112 if magic == 0x20B else 96)        # PE32+ : PE32
        (ndirs,) = struct.unpack_from("<I", data, dirs - 4)
        if ndirs < 1:
            return []
        (exp_rva,) = struct.unpack_from("<I", data, dirs)
        if not exp_rva:
            return []
        sections = [struct.unpack_from("<IIII", data, opt + opt_size + 40 * i + 8)
                    for i in range(nsect)]                  # vsize, rva, raw size, raw ptr

        def offset(rva):
            for vsize, va, raw_size, raw_ptr in sections:
                if va <= rva < va + max(vsize, raw_size):
                    return raw_ptr + rva - va
            raise ValueError(f"rva {rva:#x} outside every section")

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
    ap.add_argument("--arch", default=",".join(ARCHES),
                    help=f"comma-separated subset of {','.join(ARCHES)} "
                         "(default: both, which is what a Store release uploads)")
    ap.add_argument("--identity-name", default=DEV_IDENTITY_NAME,
                    help="Identity/@Name — assigned by Partner Center at name "
                         f"reservation (default placeholder: {DEV_IDENTITY_NAME!r})")
    ap.add_argument("--publisher", default=DEV_PUBLISHER,
                    help="Identity/@Publisher — Partner Center's CN=<GUID>, or the "
                         "dev signing cert's exact Subject DN "
                         f"(default: {DEV_PUBLISHER!r})")
    ap.add_argument("--payload-root", default=None,
                    help="staged payload dir (default: build/installer/stage/<arch>)")
    ap.add_argument("--out-dir", default=os.path.join(REPO, "build", "installer"))
    ap.add_argument("--self-sign", action="store_true",
                    help="create/reuse a dev self-signed cert and sign each package "
                         "(dev loop only — Store uploads are unsigned)")
    ap.add_argument("--allow-test-surface", action="store_true",
                    help="pack a payload that carries the e2e automation surface "
                         "(the MSI test helper's stage). Dev/test packages only — "
                         "NEVER an upload; without it such a payload is refused")
    args = ap.parse_args()

    arches = [a.strip() for a in args.arch.split(",") if a.strip()]
    unknown = [a for a in arches if a not in ARCHES]
    if unknown:
        sys.exit(f"error: unknown arch {unknown}; supported: {list(ARCHES)}")

    version = _product_version()
    with open(os.path.join(STORE, "AppxManifest.xml.in"), encoding="utf-8") as f:
        template = f.read()

    print(f"version  : {version}   (from Package.wxs)")
    print(f"identity : {args.identity_name}")
    print(f"publisher: {args.publisher}")

    os.makedirs(args.out_dir, exist_ok=True)
    built = []
    for arch in arches:
        payload_root = args.payload_root or _default_payload_root(arch)
        # Staging lives UNDER --out-dir, not at a fixed repo path. `_stage`
        # rmtree's this directory, so a fixed path meant any caller that
        # redirected only its --out-dir still silently wiped and rewrote the
        # shared loose layout with its own identity. That is not hypothetical:
        # test_store_package_pack.py did exactly that to the developer's staged
        # Store layout, leaving a correct .msix beside a layout describing a FAKE
        # identity — and `Add-AppxPackage -Register` consumes the layout, so a
        # human registering "the Store package" got the test one, silently.
        # Deriving it here makes isolation automatic for every caller instead of
        # something each one must remember.
        pack_dir = os.path.join(args.out_dir, "store-stage", arch)
        out_path = os.path.join(args.out_dir, f"Fauna-Store-{arch}.msix")

        print(f"\n── {arch} ──")
        print(f"payload  : {payload_root}")
        print(f"staged   : {pack_dir}   (Add-AppxPackage -Register this)")
        _stage(pack_dir, payload_root,
               render_manifest(template, args.identity_name, args.publisher,
                               version, arch),
               allow_test_surface=args.allow_test_surface)
        subprocess.run(_pack_argv(pack_dir, out_path), check=True)
        print(f"packed   : {out_path}")

        if args.self_sign:
            pfx = _self_sign(out_path, args.publisher)
            print(f"signed   : {out_path}")
        built.append(out_path)

    if not args.self_sign:
        print("\nNot signed — which is correct for a Store upload: the Store signs.")
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

#!/usr/bin/env python3
"""Sign the unsigned iOS store app bundle by bundle and package the `.ipa`.

Owner: docs/goal/architecture/installers/store-review.md § The store legs,
*How the iOS store app is signed*; the identifiers and profiles:
installers/ios.md § Signing.

The App Store build is archived with signing OFF in a job that holds no
credential; this signer, run in the approved `sign` job (and by
`just ios-store-upload` on macOS), is the only thing that ever signs it. It
compiles nothing and runs nothing from the built tree but reads two of its
files: the Xcode project (which entitlements file each bundle's target names in
its Release configuration) and those entitlements files.

  ios-store-sign.py --app <unsigned Fauna-iOS.app> --project <Fauna.xcodeproj>
                    --identity <certificate name> --profile <p.mobileprovision>...
                    --out <Fauna-iOS.ipa> [--keychain <kc>] [--version V --build N]
                    [--entitlements-out <dir>] [--signatures <archive>/Signatures]

For each of the four bundles, the extensions first and the app last:

  1. its profile is the one `--profile` whose application identifier is
     `<team>.<the bundle's CFBundleIdentifier>`; it is refused when expired,
     when it lists devices or provisions all devices, when its team is not the
     identity's, or when its DeveloperCertificates do not hold the certificate
     of the identity about to sign;
  2. its entitlements are COMPUTED, never typed: the keys of the file the
     bundle's target names in its Release configuration (`CODE_SIGN_ENTITLEMENTS`;
     a target naming none claims none), plus the identity keys of
     PROFILE_IDENTITY_KEYS read from the profile. A tracked key the profile does
     not grant with that value is a red before anything is signed — never dropped;
  3. the profile is copied to `embedded.mobileprovision` and the bundle signed
     with `codesign --force` over that set — never `--deep`.

Any other nested code in the app (a framework, a dylib, a fifth bundle) is a
refusal: there is none today, and a signer that skipped one would ship it
signed by nobody. The `.ipa` is `Payload/Fauna-iOS.app` (and the archive's
`Signatures/` when given), zipped by `ditto`.
Every bundle is then read back off the package: the signature verifies
strictly, the authority is the identity, the embedded profile is the one
chosen, the signed entitlements equal the computed set, and the version and
build are the release's when given.

One `ios-store-sign: <step> OK|FAILED — …` line per step; exit 0 signed, 1 a
refusal or a failed step, 2 a usage error. Pinned by
tests/e2e-unified/tests/test_ios_store_sign.py (the computation, the refusals,
the project map) and, against the exporter, by
tests/e2e-unified/tests/artifact/test_ios_archive.py's signed leg.
"""
from __future__ import annotations

import argparse
import fnmatch
import hashlib
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

#: The bundles of the store app, by path under `Payload/`, in SIGNING order:
#: the extensions first, the app last (the app's signature seals theirs).
BUNDLES = (
    "Fauna-iOS.app/PlugIns/Fauna-iOS-FileProvider.appex",
    "Fauna-iOS.app/PlugIns/Fauna-iOS-FileProviderUI.appex",
    "Fauna-iOS.app/PlugIns/Fauna-iOS-Widget.appex",
    "Fauna-iOS.app",
)

#: The keys a store signature carries out of the bundle's own profile, beside
#: the tracked file's: what Xcode's App Store export stamps (measured against
#: it by test_ios_archive.py's signed leg, which holds the two equal).
PROFILE_IDENTITY_KEYS = (
    "application-identifier",
    "com.apple.developer.team-identifier",
    "get-task-allow",
    "beta-reports-active",
)

#: Nested code the signer refuses to leave unsigned: a code bundle of these kinds
#: under the app that is not one of BUNDLES, or any Mach-O file that is not one
#: of their main executables. A resource bundle (SwiftPM's `<Pkg>_<Target>.bundle`)
#: holds no code and is sealed by the app's own signature, as Xcode leaves it.
_NESTED_CODE_SUFFIXES = (".app", ".appex", ".framework", ".xpc")
_MACHO_MAGICS = {bytes.fromhex(m) for m in ("feedface", "feedfacf", "cefaedfe", "cffaedfe", "cafebabe", "bebafeca")}


class Refusal(Exception):
    """A red the signer states before or instead of signing."""


def say(msg: str) -> None:
    print(f"ios-store-sign: {msg}", flush=True)


# --- the Xcode project: an OpenStep plist, read without Apple's tools --------

_TOKEN = re.compile(r'\s*(?:(//[^\n]*)|(/\*.*?\*/)|("(?:[^"\\]|\\.)*")|([{}()=;,])|([^\s{}()=;,"]+))', re.S)


def _tokens(text: str):
    pos = 0
    while pos < len(text):
        m = _TOKEN.match(text, pos)
        if not m:
            if text[pos:].strip():
                raise Refusal(f"project FAILED — unreadable project text at offset {pos}")
            return
        pos = m.end()
        if m.group(1) or m.group(2):
            continue
        if m.group(3) is not None:
            raw = m.group(3)[1:-1]
            yield ("str", re.sub(r'\\(.)', lambda e: {"n": "\n", "t": "\t"}.get(e.group(1), e.group(1)), raw))
        elif m.group(4):
            yield ("sym", m.group(4))
        elif m.group(5):
            yield ("str", m.group(5))


def parse_openstep(text: str):
    """The value of an old-style (OpenStep) property list such as a project.pbxproj."""
    toks = list(_tokens(text))
    i = 0

    def value():
        nonlocal i
        kind, tok = toks[i]
        i += 1
        if kind == "str":
            return tok
        if tok == "{":
            out = {}
            while toks[i] != ("sym", "}"):
                key = value()
                if toks[i] != ("sym", "="):
                    raise Refusal(f"project FAILED — expected '=' after {key!r}")
                i += 1
                out[key] = value()
                if toks[i] != ("sym", ";"):
                    raise Refusal(f"project FAILED — expected ';' after {key!r}")
                i += 1
            i += 1
            return out
        if tok == "(":
            out = []
            while toks[i] != ("sym", ")"):
                out.append(value())
                if toks[i] == ("sym", ","):
                    i += 1
            i += 1
            return out
        raise Refusal(f"project FAILED — unexpected {tok!r}")

    return value()


def release_entitlements_files(pbxproj_text: str) -> dict[str, str | None]:
    """Each product's entitlements file in its Release configuration, by product name.

    The value is the path relative to the project's directory, or None for a
    target naming none. The device-SDK conditional, when set, wins over the bare key.
    """
    objects = parse_openstep(pbxproj_text)["objects"]
    out: dict[str, str | None] = {}
    for obj in objects.values():
        if obj.get("isa") != "XCBuildConfiguration" or obj.get("name") != "Release":
            continue
        settings = obj.get("buildSettings", {})
        name = settings.get("PRODUCT_NAME")
        if not name:
            continue
        if name in out:
            raise Refusal(f"project FAILED — two Release configurations name the product {name!r}")
        out[name] = settings.get("CODE_SIGN_ENTITLEMENTS[sdk=iphoneos*]") or settings.get("CODE_SIGN_ENTITLEMENTS")
    return out


# --- the computation ---------------------------------------------------------

def _granted(value, granted) -> bool:
    """Does a profile value `granted` grant the tracked value `value`?"""
    if value == granted:
        return True
    if isinstance(granted, list):
        items = value if isinstance(value, list) else [value]
        return all(
            any(item == g or (isinstance(g, str) and isinstance(item, str) and g.endswith("*")
                              and fnmatch.fnmatchcase(item, g)) for g in granted)
            for item in items
        )
    return False


def compute_entitlements(tracked: dict, profile_entitlements: dict) -> dict:
    """The entitlements a store signature of one bundle carries.

    The tracked file's keys, each refused unless the profile grants that value,
    plus PROFILE_IDENTITY_KEYS from the profile.
    """
    out: dict = {}
    for key, value in tracked.items():
        if key not in profile_entitlements:
            raise Refusal(f"entitlements FAILED — the tracked file claims {key!r}, which the profile does not grant")
        if not _granted(value, profile_entitlements[key]):
            raise Refusal(
                f"entitlements FAILED — the tracked file claims {key!r} = {value!r}, "
                f"the profile grants {profile_entitlements[key]!r}"
            )
        out[key] = value
    for key in PROFILE_IDENTITY_KEYS:
        if key in profile_entitlements:
            if key in out and out[key] != profile_entitlements[key]:
                raise Refusal(f"entitlements FAILED — the tracked file sets the identity key {key!r} itself")
            out[key] = profile_entitlements[key]
    for key in ("application-identifier", "com.apple.developer.team-identifier"):
        if key not in out:
            raise Refusal(f"entitlements FAILED — the profile carries no {key!r}")
    return out


def check_profile(profile: dict, bundle_id: str, cert_sha1: str, now: datetime) -> str:
    """Refuse a profile that may not sign `bundle_id` with that certificate; return the team."""
    teams = profile.get("TeamIdentifier", [])
    if len(teams) != 1:
        raise Refusal(f"profile FAILED — {profile.get('Name')!r} names teams {teams}")
    team = teams[0]
    name = profile.get("Name")
    if profile.get("Entitlements", {}).get("application-identifier") != f"{team}.{bundle_id}":
        raise Refusal(f"profile FAILED — {name!r} is for {profile.get('Entitlements', {}).get('application-identifier')!r}, not {team}.{bundle_id}")
    expiry = profile.get("ExpirationDate")
    if not isinstance(expiry, datetime) or expiry.replace(tzinfo=None) <= now.replace(tzinfo=None):
        raise Refusal(f"profile FAILED — {name!r} expired {expiry}")
    if "ProvisionedDevices" in profile or profile.get("ProvisionsAllDevices"):
        raise Refusal(f"profile FAILED — {name!r} lists devices: not an App Store profile")
    certs = [hashlib.sha1(der).hexdigest().upper() for der in profile.get("DeveloperCertificates", [])]
    if cert_sha1.upper() not in certs:
        raise Refusal(f"profile FAILED — {name!r} does not carry the signing identity's certificate {cert_sha1}")
    return team


# --- the tools ---------------------------------------------------------------

def _run(*cmd: str, check: bool = True) -> subprocess.CompletedProcess:
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if check and proc.returncode != 0:
        raise Refusal(f"{cmd[0]} FAILED — {' '.join(cmd[:3])} …: {(proc.stderr or proc.stdout).strip()[-600:]}")
    return proc


def decode_profile(path: Path) -> dict:
    out = subprocess.run(["security", "cms", "-D", "-i", str(path)], capture_output=True)
    if out.returncode != 0:
        raise Refusal(f"profile FAILED — {path} does not decode")
    return plistlib.loads(out.stdout)


def identity_sha1(identity: str, keychain: str | None) -> str:
    """The one valid code-signing identity of that name, by its certificate's SHA-1."""
    cmd = ["security", "find-identity", "-v", "-p", "codesigning"] + ([keychain] if keychain else [])
    found = re.findall(r'^\s*\d+\)\s+([0-9A-F]{40})\s+"([^"]+)"', _run(*cmd).stdout, re.M)
    shas = sorted({sha for sha, name in found if name == identity})
    if len(shas) != 1:
        raise Refusal(f"identity FAILED — {len(shas)} valid identities named {identity!r} (needs exactly one)")
    return shas[0]


def signed_entitlements(bundle: Path) -> dict:
    out = _run("codesign", "-d", "--entitlements", "-", "--xml", str(bundle)).stdout
    return plistlib.loads(out.encode()) if out.strip() else {}


def _main_executable(bundle: Path) -> Path | None:
    try:
        name = plistlib.loads((bundle / "Info.plist").read_bytes()).get("CFBundleExecutable")
    except (OSError, plistlib.InvalidFileException):
        return None
    return bundle / name if name else None


def nested_code_outside(app: Path) -> list[str]:
    """Code under the app that BUNDLES does not sign: a code bundle, or a stray Mach-O."""
    known = {Path(rel).relative_to("Fauna-iOS.app") for rel in BUNDLES if rel != "Fauna-iOS.app"}
    mains = {_main_executable(app / k) for k in known} | {_main_executable(app)}
    extra = []
    for path in app.rglob("*"):
        rel = path.relative_to(app)
        if rel in known:
            continue
        if path.name.endswith(_NESTED_CODE_SUFFIXES):
            extra.append(str(rel))
        elif path.is_file() and not path.is_symlink() and path not in mains:
            with path.open("rb") as f:
                if f.read(4) in _MACHO_MAGICS:
                    extra.append(str(rel))
    return sorted(extra)


# --- the run -----------------------------------------------------------------

def sign(args: argparse.Namespace) -> None:
    app_src = Path(args.app)
    if app_src.name != "Fauna-iOS.app" or not (app_src / "Info.plist").is_file():
        raise Refusal(f"input FAILED — {app_src} is not a Fauna-iOS.app bundle")
    pbxproj = Path(args.project) / "project.pbxproj"
    project_dir = Path(args.project).parent
    ent_files = release_entitlements_files(pbxproj.read_text(encoding="utf-8"))

    sha1 = identity_sha1(args.identity, args.keychain)
    say(f"identity OK — {args.identity} ({sha1[:8]}…)")
    profiles = [(Path(p), decode_profile(Path(p))) for p in args.profile]
    now = datetime.now(timezone.utc)

    work = Path(tempfile.mkdtemp(prefix="ios-store-sign-"))
    try:
        package = work / "package"
        payload = package / "Payload"
        payload.mkdir(parents=True)
        if args.signatures:
            # The archive's `Signatures/` (Xcode's record of each linked
            # xcframework's own signature) rides at the package's top level, as
            # Xcode's export carries it.
            shutil.copytree(args.signatures, package / "Signatures", symlinks=True)
        app = payload / "Fauna-iOS.app"
        shutil.copytree(app_src, app, symlinks=True)
        extra = nested_code_outside(app)
        if extra:
            raise Refusal(f"input FAILED — nested code the signer does not know: {extra}")

        plan = []
        for rel in BUNDLES:
            bundle = payload / rel
            if not (bundle / "Info.plist").is_file():
                raise Refusal(f"input FAILED — {rel} is missing")
            info = plistlib.loads((bundle / "Info.plist").read_bytes())
            bundle_id = info["CFBundleIdentifier"]
            if args.version and info.get("CFBundleShortVersionString") != args.version:
                raise Refusal(f"version FAILED — {rel} is {info.get('CFBundleShortVersionString')}, not {args.version}")
            if args.build and info.get("CFBundleVersion") != args.build:
                raise Refusal(f"version FAILED — {rel} is build {info.get('CFBundleVersion')}, not {args.build}")
            # An expired copy beside its renewal is ignored (Xcode's profile
            # directory keeps both); among the live ones exactly one may match.
            matches = [(p, prof) for p, prof in profiles
                       if prof.get("Entitlements", {}).get("application-identifier", "").split(".", 1)[-1] == bundle_id]
            live = [(p, prof) for p, prof in matches
                    if isinstance(prof.get("ExpirationDate"), datetime)
                    and prof["ExpirationDate"].replace(tzinfo=None) > now.replace(tzinfo=None)]
            if len(live) != 1:
                expired = f" ({len(matches) - len(live)} expired)" if len(matches) > len(live) else ""
                raise Refusal(f"profile FAILED — {len(live)} live profiles are for {bundle_id}{expired} (needs exactly one)")
            matches = live
            ppath, prof = matches[0]
            check_profile(prof, bundle_id, sha1, now)
            stem = Path(rel).stem
            if stem not in ent_files:
                raise Refusal(f"project FAILED — no Release configuration builds the product {stem!r}")
            ent_rel = ent_files[stem]
            tracked = plistlib.loads((project_dir / ent_rel).read_bytes()) if ent_rel else {}
            computed = compute_entitlements(tracked, prof["Entitlements"])
            plan.append((rel, bundle, ppath, prof, computed))
        say(f"plan OK — {len(plan)} bundles, each with its profile and computed entitlements")

        ent_dir = Path(args.entitlements_out) if args.entitlements_out else work / "entitlements"
        ent_dir.mkdir(parents=True, exist_ok=True)
        for rel, bundle, ppath, prof, computed in plan:
            shutil.copyfile(ppath, bundle / "embedded.mobileprovision")
            ent_file = ent_dir / f"{Path(rel).stem}.entitlements"
            ent_file.write_bytes(plistlib.dumps(computed))
            cmd = ["codesign", "--force", "--sign", sha1, "--generate-entitlement-der",
                   "--entitlements", str(ent_file)]
            if args.keychain:
                cmd += ["--keychain", args.keychain]
            _run(*cmd, str(bundle))
            say(f"sign OK — {rel} with {prof['Name']!r}")

        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.unlink(missing_ok=True)
        _run("ditto", "-c", "-k", "--norsrc", "--noextattr", "--noacl", str(package), str(out))
        say(f"package OK — {out}")

        # Read back off the package, not the staging copy.
        check = work / "readback"
        _run("ditto", "-x", "-k", str(out), str(check))
        for rel, _bundle, ppath, prof, computed in plan:
            b = check / "Payload" / rel
            _run("codesign", "--verify", "--strict", str(b))
            info = _run("codesign", "-dvv", str(b)).stderr
            if f"Authority={args.identity}" not in info:
                raise Refusal(f"readback FAILED — {rel} is not signed by {args.identity}")
            if (b / "embedded.mobileprovision").read_bytes() != ppath.read_bytes():
                raise Refusal(f"readback FAILED — {rel} does not embed {prof['Name']!r}")
            got = signed_entitlements(b)
            if got != computed:
                raise Refusal(f"readback FAILED — {rel} signed entitlements {got}, computed {computed}")
        _run("codesign", "--verify", "--deep", "--strict", str(check / "Payload" / "Fauna-iOS.app"))
        sha = hashlib.sha256(out.read_bytes()).hexdigest()
        say(f"readback OK — {len(plan)} bundles verify, signed by {args.identity}, entitlements as computed; sha256 {sha}")
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--app", required=True)
    ap.add_argument("--project", required=True, help="the Fauna.xcodeproj of the built commit")
    ap.add_argument("--identity", required=True)
    ap.add_argument("--profile", action="append", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--keychain")
    ap.add_argument("--version")
    ap.add_argument("--build")
    ap.add_argument("--entitlements-out")
    ap.add_argument("--signatures", help="the archive's Signatures/ directory, packaged beside Payload/")
    args = ap.parse_args(argv)
    try:
        sign(args)
    except Refusal as e:
        print(f"ios-store-sign: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

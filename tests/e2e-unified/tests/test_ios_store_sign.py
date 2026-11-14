"""The iOS store signer computes what it signs and refuses what it must.

`installers/store-review.md` § The store legs, *How the iOS store app is
signed*: the App Store build is archived unsigned and signed bundle by bundle
by `scripts/ios-store-sign.py` in the approved job. A bundle's entitlements are
computed — the tracked file its target names in Release, plus the identity keys
of its own profile — and a tracked key the profile does not grant is a red
before anything is signed. A profile that is expired, lists devices, names
another bundle or lacks the signing certificate is refused.

Static-only (tier_1): the real project file and synthetic profiles; nothing is
signed. The signed result is held against Xcode's export by
tests/artifact/test_ios_archive.py's signed leg on macOS.
"""
from __future__ import annotations

import hashlib
import importlib.util
import plistlib
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

REPO = Path(__file__).resolve().parents[3]
_spec = importlib.util.spec_from_file_location("ios_store_sign", REPO / "scripts" / "ios-store-sign.py")
sign = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sign)

TEAM = "7457N3M72H"
CERT = b"certificate-der-bytes"
CERT_SHA1 = hashlib.sha1(CERT).hexdigest().upper()
NOW = datetime(2026, 11, 12, tzinfo=timezone.utc)


def _profile(bundle_id: str, **over) -> dict:
    prof = {
        "Name": f"Fauna AppStore {bundle_id}",
        "TeamIdentifier": [TEAM],
        "ExpirationDate": datetime(2027, 8, 22, 17, 20, 21),
        "DeveloperCertificates": [CERT],
        "Entitlements": {
            "application-identifier": f"{TEAM}.{bundle_id}",
            "com.apple.developer.team-identifier": TEAM,
            "get-task-allow": False,
            "beta-reports-active": True,
            "keychain-access-groups": [f"{TEAM}.*", "com.apple.token"],
            "com.apple.security.application-groups": ["group.social.fauna.shared"],
            "aps-environment": "production",
            "com.apple.developer.devicecheck.appattest-environment": ["development", "production"],
        },
    }
    prof.update(over)
    return prof


def test_the_project_map_names_each_store_bundles_release_entitlements():
    """The four bundles' entitlements files, read off the real project as the signer reads them."""
    files = sign.release_entitlements_files(
        (REPO / "apps/fauna-apple/Fauna.xcodeproj/project.pbxproj").read_text(encoding="utf-8"))
    got = {Path(rel).stem: files[Path(rel).stem] for rel in sign.BUNDLES}
    assert got == {
        "Fauna-iOS": "Fauna-iOS/Resources/Fauna-iOS.entitlements",
        "Fauna-iOS-FileProvider": "Fauna-FileProvider/Fauna-FileProvider-iOS.entitlements",
        "Fauna-iOS-Widget": "Fauna-Widget/Fauna-Widget-iOS.entitlements",
        "Fauna-iOS-FileProviderUI": None,
    }
    for rel in got.values():
        if rel:
            assert (REPO / "apps/fauna-apple" / rel).is_file(), rel


def test_the_signing_order_puts_the_app_last():
    assert sign.BUNDLES[-1] == "Fauna-iOS.app"
    assert all(rel.endswith(".appex") for rel in sign.BUNDLES[:-1])


def test_the_app_entitlements_are_the_tracked_keys_plus_the_profiles_identity():
    tracked = plistlib.loads((REPO / "apps/fauna-apple/Fauna-iOS/Resources/Fauna-iOS.entitlements").read_bytes())
    prof = _profile("social.fauna.fauna", **{})
    prof["Entitlements"]["com.apple.developer.declared-age-range"] = True
    got = sign.compute_entitlements(tracked, prof["Entitlements"])
    assert got == {
        **tracked,
        "application-identifier": f"{TEAM}.social.fauna.fauna",
        "com.apple.developer.team-identifier": TEAM,
        "get-task-allow": False,
        "beta-reports-active": True,
    }
    # A list value in the profile grants one of its members.
    assert got["com.apple.developer.devicecheck.appattest-environment"] == "production"


def test_a_bundle_naming_no_file_claims_only_its_identity():
    got = sign.compute_entitlements({}, _profile("social.fauna.fauna.FileProviderUI")["Entitlements"])
    assert set(got) == set(sign.PROFILE_IDENTITY_KEYS)


def test_a_tracked_key_the_profile_does_not_grant_is_a_red_never_dropped():
    ents = _profile("social.fauna.fauna.Widget")["Entitlements"]
    with pytest.raises(sign.Refusal, match="does not grant"):
        sign.compute_entitlements({"com.apple.developer.declared-age-range": True}, ents)
    with pytest.raises(sign.Refusal, match="profile grants"):
        sign.compute_entitlements({"com.apple.security.application-groups": ["group.other"]}, ents)
    with pytest.raises(sign.Refusal, match="profile grants"):
        sign.compute_entitlements({"aps-environment": "development"}, ents)


def test_a_wildcard_grant_covers_a_matching_value_only():
    ents = _profile("social.fauna.fauna")["Entitlements"]
    got = sign.compute_entitlements({"keychain-access-groups": [f"{TEAM}.social.fauna.fauna"]}, ents)
    assert got["keychain-access-groups"] == [f"{TEAM}.social.fauna.fauna"]
    with pytest.raises(sign.Refusal):
        sign.compute_entitlements({"keychain-access-groups": ["OTHERTEAM.x"]}, ents)


def test_a_good_profile_passes():
    assert sign.check_profile(_profile("social.fauna.fauna"), "social.fauna.fauna", CERT_SHA1, NOW) == TEAM


@pytest.mark.parametrize("over, why", [
    ({"ExpirationDate": datetime(2026, 11, 1)}, "expired"),
    ({"ProvisionedDevices": ["00008110-0001"]}, "lists devices"),
    ({"ProvisionsAllDevices": True}, "lists devices"),
    ({"DeveloperCertificates": [b"another-certificate"]}, "certificate"),
    ({"TeamIdentifier": ["OTHERTEAM1"]}, "is for"),
])
def test_a_profile_is_refused(over, why):
    with pytest.raises(sign.Refusal, match=why):
        sign.check_profile(_profile("social.fauna.fauna", **over), "social.fauna.fauna", CERT_SHA1, NOW)


def test_a_profile_for_another_bundle_is_refused():
    with pytest.raises(sign.Refusal, match="is for"):
        sign.check_profile(_profile("social.fauna.fauna.Widget"), "social.fauna.fauna", CERT_SHA1, NOW)


def test_nested_code_the_signer_does_not_know_is_found(tmp_path):
    app = tmp_path / "Fauna-iOS.app"
    (app / "PlugIns" / "Fauna-iOS-Widget.appex" / "Frameworks" / "Inner.framework").mkdir(parents=True)
    (app / "Frameworks").mkdir()
    (app / "Frameworks" / "libx.dylib").write_bytes(b"")
    (app / "Frameworks" / "libx.dylib").write_bytes(bytes.fromhex("cffaedfe") + b"\0" * 12)
    (app / "Info.plist").write_bytes(plistlib.dumps({"CFBundleExecutable": "Fauna-iOS"}))
    (app / "Fauna-iOS").write_bytes(bytes.fromhex("cffaedfe") + b"\0" * 12)
    # A SwiftPM resource bundle holds no code: the app's signature seals it.
    (app / "FaunaApple_FaunaiOSLib.bundle").mkdir()
    (app / "FaunaApple_FaunaiOSLib.bundle" / "Localizable.strings").write_text("x")
    assert sign.nested_code_outside(app) == [
        "Frameworks/libx.dylib",
        "PlugIns/Fauna-iOS-Widget.appex/Frameworks/Inner.framework",
    ]
    (app / "FaunaApple_FaunaiOSLib.bundle" / "stray").write_bytes(bytes.fromhex("cafebabe"))
    assert "FaunaApple_FaunaiOSLib.bundle/stray" in sign.nested_code_outside(app)


def test_the_signer_never_signs_deep():
    code = "\n".join(l for l in (REPO / "scripts/ios-store-sign.py").read_text().splitlines()
                     if not l.lstrip().startswith("#"))
    sign_calls = [l for l in code.splitlines() if '"codesign", "--force"' in l]
    assert sign_calls and not any("--deep" in l for l in sign_calls)

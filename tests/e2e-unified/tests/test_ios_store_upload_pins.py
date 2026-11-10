"""The iOS store upload recipe keeps the shape installers/ios.md § Upload mechanics names.

`just ios-store-upload <version> [export|upload]` (`scripts/ios-store-upload.sh`)
is the one command a session runs to build — and upload — the App Store build
from the recorded public commit. Its run needs the distribution identity, the
four App Store profiles and the App Store Connect key, which live on one macOS
box alone; these pins run on every machine, so nobody can quietly widen it into
a build that talks to Apple's servers more than the upload itself, or that
ships the payments surfaces the store build excises.

Static-only: it reads repository files, builds nothing and launches nothing
(tier_1).
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts" / "ios-store-upload.sh"
JUSTFILE = ROOT / "justfile"
TRACKED_OPTIONS = "apps/fauna-apple/ExportOptions-AppStore.plist"


def _code() -> str:
    """The script with its comment lines removed, so a pin reads what runs."""
    lines = SCRIPT.read_text(encoding="utf-8").splitlines()
    return "\n".join(l for l in lines if not l.lstrip().startswith("#"))


def test_the_just_recipe_runs_the_script_on_macos_only():
    text = JUSTFILE.read_text(encoding="utf-8")
    m = re.search(r"\[macos\]\nios-store-upload version mode=\"export\":\n(.*?)\n\n", text, re.S)
    assert m, "justfile: no `[macos] ios-store-upload version mode=\"export\":` recipe"
    assert 'scripts/ios-store-upload.sh "{{version}}" "{{mode}}"' in m.group(1), m.group(1)


def test_the_source_is_a_clone_of_the_public_repository_at_the_version_tag():
    code = _code()
    assert 'PUBLIC_REMOTE="https://github.com/faunasocial/fauna.git"' in code
    assert re.search(r'git clone .*--branch "\$TAG" "\$PUBLIC_REMOTE" "\$CLONE"', code), (
        "the source tree must be a clone of the public repository at the tag "
        "(release-integrity.md § Release signing)"
    )
    assert 'TAG="v${VERSION}"' in code
    assert "describe --tags --exact-match HEAD" in code, "a non-tag checkout must be refused"
    assert "status --porcelain" in code, "a dirty checkout must be refused"
    assert "MARKETING_VERSION" in code, "a tree of another version must be refused"


def test_the_archive_is_the_store_safe_flavor_with_both_excision_flags():
    code = _code()
    assert "apple-ffi-store-safe" in code
    assert "-D FAUNA_EXCISE_PAYMENTS" in code and "-D FAUNA_EXCISE_P2P_SHARE" in code
    assert '"OTHER_SWIFT_FLAGS=${EXCISE_FLAGS}"' in code
    assert "-configuration Release" in code


def test_the_export_uses_the_tracked_app_store_connect_options():
    code = _code()
    assert f'EXPORT_OPTIONS="${{CLONE}}/{TRACKED_OPTIONS}"' in code
    tracked = (ROOT / TRACKED_OPTIONS).read_text(encoding="utf-8")
    assert "<string>app-store-connect</string>" in tracked


def test_the_upload_goes_only_through_a_scratch_options_copy():
    code = _code()
    assert 'UPLOAD_OPTIONS="${OUT}/ExportOptions-upload.plist"' in code
    assert 'cp "$EXPORT_OPTIONS" "$UPLOAD_OPTIONS"' in code
    for edit in re.findall(r"plutil -replace [^\n]*", code):
        assert edit.endswith('"$UPLOAD_OPTIONS"'), (
            f"only the scratch copy may be edited, never the tracked export-only "
            f"options: {edit}"
        )
    assert 'plutil -replace destination -string upload "$UPLOAD_OPTIONS"' in code
    assert 'plutil -replace uploadSymbols -bool YES "$UPLOAD_OPTIONS"' in code
    upload_calls = re.findall(r"xcodebuild -exportArchive.*?>\"\$LOGS/(\w+)\.log\"", code, re.S)
    assert upload_calls == ["export", "upload"], upload_calls
    upload = code.split('-exportPath "${OUT}/upload"', 1)[1].split(">", 1)[0]
    assert '-exportOptionsPlist "$UPLOAD_OPTIONS"' in upload
    assert "-authenticationKeyPath" in upload and "-authenticationKeyIssuerID" in upload


def test_the_key_is_read_as_ios_export_compliance_reads_it():
    code = _code()
    for var in ("FAUNA_NOTARY_KEY_FILE", "FAUNA_NOTARY_KEY_ID", "FAUNA_NOTARY_ISSUER"):
        assert var in code
    assert '"${HOME}/.config/fauna/asc"' in code
    assert not re.search(r"AuthKey_[A-Z0-9]{4,}|authenticationKeyID [A-Z0-9]{4,}", code), (
        "no key id may be typed into the script; it is read off the file name"
    )


def test_nothing_may_create_or_refresh_a_profile():
    assert "allowProvisioningUpdates" not in _code(), (
        "the build signs manually from the profiles on disk; "
        "-allowProvisioningUpdates would let xcodebuild contact Apple to make one"
    )


def test_the_store_review_record_is_printed_never_written():
    code = _code()
    lines = [l for l in code.splitlines() if "store-review" in l]
    assert any("just store-review app-store ${VERSION} built" in l for l in lines)
    assert any("just store-review app-store ${VERSION} uploaded" in l for l in lines)
    assert "scripts/store-review.py" not in code, (
        "the record is written from the console reading, never by the build "
        "(store-review.md § The record → The writing rule)"
    )
    for l in lines:
        assert l.startswith("  just store-review"), f"store-review must only be printed: {l!r}"

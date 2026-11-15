"""The iOS store upload recipe keeps the shape installers/ios.md § Upload mechanics names.

`just ios-store-upload <version> [export|upload|build|sign]`
(`scripts/ios-store-upload.sh`) is the one command that builds — and uploads —
the App Store build from the recorded public commit, in the two halves the
release workflow calls (installers/store-review.md § The store legs, *How the
iOS store app is signed*): a BUILD half that archives unsigned and holds no
credential, and a SIGN half that runs the tree's own signer. Its run needs the
distribution identity, the four App Store profiles and the App Store Connect
key, which live on one macOS box and in the release Environment alone; these
pins run on every machine, so nobody can quietly widen it into a build that
signs with the exporter, talks to Apple's servers more than the upload itself,
or ships the payments surfaces the store build excises.

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


def _code() -> str:
    """The script with its comment lines removed, so a pin reads what runs."""
    lines = SCRIPT.read_text(encoding="utf-8").splitlines()
    return "\n".join(l for l in lines if not l.lstrip().startswith("#"))


def _function(name: str) -> str:
    m = re.search(rf"^{name}\(\) \{{\n(.*?)^\}}", _code(), re.S | re.M)
    assert m, f"no {name}() in {SCRIPT}"
    return m.group(1)


def test_the_just_recipe_runs_the_script_on_macos_only():
    text = JUSTFILE.read_text(encoding="utf-8")
    m = re.search(r"\[macos\]\nios-store-upload version mode=\"export\" \*args:\n(.*?)\n\n", text, re.S)
    assert m, "justfile: no `[macos] ios-store-upload version mode=\"export\" *args:` recipe"
    assert 'scripts/ios-store-upload.sh "{{version}}" "{{mode}}" {{args}}' in m.group(1), m.group(1)


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


def test_a_given_tree_must_be_clean():
    code = _code()
    tree = code.split('if [ -n "$TREE_ARG" ]; then', 1)[1].split("elif", 1)[0]
    assert "status --porcelain --untracked-files=no" in tree and "die 2" in tree


def test_the_archive_is_unsigned_and_the_store_safe_flavor_with_both_excision_flags():
    build = _function("build_half")
    assert "apple-ffi-store-safe" in build
    assert "CODE_SIGNING_ALLOWED=NO" in build and "CODE_SIGNING_REQUIRED=NO" in build
    assert '"OTHER_SWIFT_FLAGS=${EXCISE_FLAGS}"' in build
    assert "-configuration Release" in build
    code = _code()
    assert "-D FAUNA_EXCISE_PAYMENTS" in code and "-D FAUNA_EXCISE_P2P_SHARE" in code


def test_the_build_half_holds_no_credential_and_never_exports():
    build = _function("build_half")
    for word in ("IDENTITY", "KEY_FILE", "altool", "codesign", "ios-store-sign", "-exportArchive"):
        assert word not in build, f"the build half names {word!r}"


def test_the_exporter_is_off_the_path():
    assert "-exportArchive" not in _code(), (
        "the store app is signed by the tree's own signer, never the exporter "
        "(store-review.md § The store legs, How the iOS store app is signed)"
    )


def test_the_sign_half_runs_the_trees_signer():
    sign = _function("sign_half")
    assert 'scripts/ios-store-sign.py"' in sign
    assert '--project "$PROJECT"' in sign and '--identity "$IDENTITY"' in sign
    assert '--version "$VERSION"' in sign


def test_the_upload_is_apples_tool_on_the_signers_package():
    code = _code()
    upload = code.split("xcrun altool", 1)
    assert len(upload) == 2, "the upload is one `xcrun altool` call"
    assert upload[1].lstrip().startswith('--upload-package "$IPA"')
    assert code.count("xcrun altool") == 1


def test_the_key_is_read_as_ios_export_compliance_reads_it():
    code = _code()
    for var in ("FAUNA_ASC_STORE_KEY_FILE", "FAUNA_ASC_STORE_KEY_ID", "FAUNA_ASC_STORE_ISSUER",
                "FAUNA_NOTARY_KEY_FILE", "FAUNA_NOTARY_KEY_ID", "FAUNA_NOTARY_ISSUER"):
        assert var in code
    assert '"${HOME}/.config/fauna/asc"' in code
    assert not re.search(r"AuthKey_[A-Z0-9]{4,}|api-key [A-Z0-9]{4,}", code), (
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

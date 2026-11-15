"""tier_1: an unanswered keychain dialog at the photo-library venue's codesign is
an environment skip, not a red — and is met once a session.

Owner: ``drivers/macos.py`` § the photo-library venue — the stable signature
needs the login keychain's one-time "Always Allow" for ``codesign``, a person's
step at the screen (``just mac-photos-e2e-grant``). A run on a box that has not
had it is the box's property, not the app's (convention 7): it declares the skip
so the background tester holds no red that no commit can fix, and every later
launch in the session skips at once instead of raising the dialog again and
waiting the whole timeout for nobody.

``codesign`` is faked here: no keychain is touched.
"""

import subprocess

import pytest

from drivers import macos

pytestmark = [pytest.mark.tier_1]


def test_a_codesign_held_on_the_keychain_dialog_skips_as_environment_once(
    tmp_path, monkeypatch
):
    if not macos._MACOS_INFO_PLIST.exists():
        pytest.skip("the macOS Info.plist is not in this tree")
    binary = tmp_path / "FaunaMacOS"
    binary.write_bytes(b"\0")
    calls = []

    def fake_run(argv, *args, **kwargs):
        calls.append(argv)
        if argv[:2] == ["codesign", "--force"]:
            raise subprocess.TimeoutExpired(argv, kwargs.get("timeout"))
        return subprocess.CompletedProcess(argv, 1, "", "")

    monkeypatch.setattr(macos, "apple_development_identity", lambda: "Apple Development: x")
    monkeypatch.setattr(macos.subprocess, "run", fake_run)
    monkeypatch.setattr(macos, "_CODESIGN_UNGRANTED", False)

    with pytest.raises(pytest.skip.Exception) as first:
        macos.stage_photo_library_bundle(binary, stage_dir=tmp_path / "stage")
    assert first.value.msg.startswith("environment: ")
    assert "mac-photos-e2e-grant" in first.value.msg
    signs = sum(1 for argv in calls if argv[:2] == ["codesign", "--force"])
    assert signs == 1

    with pytest.raises(pytest.skip.Exception):
        macos.stage_photo_library_bundle(binary, stage_dir=tmp_path / "stage2")
    assert sum(1 for argv in calls if argv[:2] == ["codesign", "--force"]) == signs, (
        "a second launch in the session must not raise the dialog again"
    )

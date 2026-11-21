"""tier_1: ``helpers.owner_only`` reads "only its owner can open this" the way
the platform spells it — the mode bits on unix, a DACL naming the current user
alone on windows — the same rule the app's writer is held to
(``fauna_provisioning::ssh_door``'s ``assert_owner_only``,
``fauna_ipc::win_acl::is_owner_only``). A drive that read the unix mode on
windows refused every key the app wrote there: windows has no mode bits, so
``st_mode & 0o777`` reads ``0o666`` whatever the file's ACL says."""
from __future__ import annotations

import os
import re

import pytest

from helpers import owner_only

pytestmark = pytest.mark.tier_1


def test_a_fresh_file_is_not_owner_only_until_restricted(tmp_path):
    key = tmp_path / "fauna-door.example.org"
    key.write_text("k")
    if os.name == "posix":
        key.chmod(0o644)
    # Beside-control: under the temp dir a file inherits SYSTEM and
    # Administrators beside the user on windows; 0644 lets everyone read on unix.
    assert not owner_only.is_owner_only(key), owner_only.reading(key)
    owner_only.restrict_to_owner(key)
    assert owner_only.is_owner_only(key), owner_only.reading(key)


def test_a_restricted_directory_is_owner_only_at_its_own_mode(tmp_path):
    d = tmp_path / ".ssh"
    d.mkdir()
    owner_only.restrict_to_owner(d)
    assert owner_only.is_owner_only(d, unix_mode=0o700), owner_only.reading(d)


@pytest.mark.host_platform("linux", "macos")
def test_on_unix_the_mode_must_match_exactly(tmp_path):
    key = tmp_path / "k"
    key.write_text("k")
    key.chmod(0o400)
    assert not owner_only.is_owner_only(key), "0400 is not the 0600 the app writes"
    assert owner_only.reading(key) == "0o400"


@pytest.mark.host_platform("windows")
def test_on_windows_the_reading_is_the_dacl(tmp_path):
    key = tmp_path / "k"
    key.write_text("k")
    owner_only.restrict_to_owner(key)
    got = owner_only.reading(key)
    # Protected (P), one allow ACE of full access (FA) for one SID; the set
    # adds the auto-inherited flag (AI) itself.
    assert re.fullmatch(r"D:P(AI)?\(A;;FA;;;S-1-5-[0-9-]+\)", got), got

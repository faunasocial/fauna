"""tier_1: a text payload a test hands a seat lands on disk as its own UTF-8 bytes.

The succession witnesses write a payload ending in a newline through
`helpers.folder_content.atomic_write` (a bound folder) or the Media-page door's
scratch file, then compare what another seat downloads, and the size the nest's
change row records, against `payload.encode()`. Text mode on Windows writes
`\\r\\n` for each `\\n` and encodes in the box's locale, so the uploaded file was
one byte longer than the expectation: measured on the Windows background tester,
`test_folder_succession_member_reader` read 68 bytes for 67 and
`test_identity_succession_shared_folder_rekey` waited for a 90-byte change row
that was written as 91. Both writers therefore write bytes, and the bound
folder's read decodes bytes, on every box.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import helpers.succession_file_door as succession_file_door  # noqa: E402
from helpers.folder_content import atomic_write  # noqa: E402

pytestmark = pytest.mark.tier_1

PAYLOAD = "the owner's file, before the recovery — 0123\nsecond line\n"


def test_atomic_write_lands_a_str_as_its_utf8_bytes(tmp_path):
    path = tmp_path / "a.txt"
    atomic_write(path, PAYLOAD)
    assert path.read_bytes() == PAYLOAD.encode("utf-8")
    assert not list(tmp_path.glob("*.tmp-agentsync"))


def test_atomic_write_keeps_bytes_as_given(tmp_path):
    path = tmp_path / "b.bin"
    atomic_write(path, b"one\r\ntwo\n")
    assert path.read_bytes() == b"one\r\ntwo\n"


def test_the_media_page_door_uploads_the_payloads_own_bytes(tmp_path):
    uploaded: list[bytes] = []

    class _Door(succession_file_door.MediaPageDoor):
        def _upload(self, path):
            uploaded.append(path.read_bytes())
            return True

    door = _Door(app=None, filter_value="set", scratch=tmp_path / "scratch", seat="m")
    door.add("c.txt", PAYLOAD, await_landed=False)
    assert uploaded == [PAYLOAD.encode("utf-8")]


def test_the_bound_folder_read_does_not_fold_line_endings(tmp_path):
    crlf = tmp_path / "d.txt"
    crlf.write_bytes(b"one\r\n")
    assert succession_file_door._read_or_none(crlf) == "one\r\n"
    assert succession_file_door._read_or_none(tmp_path / "absent.txt") is None

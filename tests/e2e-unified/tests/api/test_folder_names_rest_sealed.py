"""File and folder names rest sealed on the nest — tier_3, api.

``docs/features/folders.md`` outcome 17 (``encryption-at-rest.md`` § Implementation
status today and § Carve-outs, *paths are content*): a folder the owner's app
created rests its name only as the owner's seal, a file's path rests only as
its seal, and a change record that arrives without its sealed path is refused
``path_seal_required``.

The set is created the way an owner's app creates one — the shared
``create_set``, which seals the name at create (``fauna_ffi.harness_create_set``)
— and is neither reserved (``__``) nor ``public``, the two names that lawfully
rest in the clear; an admin-created set keeps its plaintext name until its
owner's app seals it, so an unsealed create is no refusal to witness. Both
names are unique to this test, so a byte scan of the nest's database (main file
and WAL, where a fresh write rests until a checkpoint) finds them nowhere unless
the nest wrote them in the clear. The scan's control, in the share-links
witness's shape: each needle is real — the nest received both names (the set is
addressed by its name, the record carries its path) and the recorded row opens
to exactly the file's name.
"""

from __future__ import annotations

import secrets
from pathlib import Path

import pytest

from common.auth import (
    sealed_path,
    sync_changes_list,
    sync_changes_record,
    sync_register,
    user_create_folder,
)
from conftest import _make_user

pytestmark = pytest.mark.tier_3


def _nest_files(db_path: str) -> dict[str, bytes]:
    files = {}
    for suffix in ("", "-wal"):
        path = Path(db_path + suffix)
        if path.exists():
            files[path.name] = path.read_bytes()
    return files


@pytest.mark.feature("folders")
def test_file_and_folder_names_rest_sealed_and_an_unsealed_record_is_refused(nest_instance):
    nest = nest_instance
    owner = _make_user(nest)
    secret = bytes(owner["signing_key"]).hex()
    folder = f"holiday-{secrets.token_hex(6)}"
    file_name = f"itinerary-{secrets.token_hex(6)}.txt"

    device_id = secrets.token_hex(32)
    sync_register(nest["port"], secret_key=secret, device_id=device_id)
    user_create_folder(nest["port"], folder, secret_key=secret)
    sync_changes_record(
        nest["port"], secret_key=secret, folder=folder, device_id=device_id,
        path=file_name, manifest_hash=secrets.token_hex(32), size_bytes=42,
    )

    # Control: the nest holds both — the set it recorded into by name, and a
    # row whose seal is exactly the file's name under the owner's root.
    changes = sync_changes_list(nest["port"], secret_key=secret, folder=folder).get("changes", [])
    want = sealed_path(secret, file_name)
    rows = [c for c in changes if c.get("path_sealed") == want]
    assert len(rows) == 1, (
        f"control: the record must land as one row sealed to {file_name!r}; "
        f"got {[(c.get('path'), c.get('change_type')) for c in changes]!r}"
    )
    assert rows[0].get("path") is None, (
        f"a recorded row carries no plaintext path: {rows[0].get('path')!r}"
    )

    leaked = {
        needle: [f for f, body in _nest_files(nest["db_path"]).items() if needle.encode() in body]
        for needle in (folder, file_name)
    }
    leaked = {needle: files for needle, files in leaked.items() if files}
    assert not leaked, f"a name rests in the clear on the nest: {leaked!r}"

    # A record that carries no sealed path is refused, into the same set by
    # the same writer: the missing seal is the only thing that differs. The
    # signed record goes out through the FFI harness, whose error carries the
    # nest's code in its text.
    with pytest.raises(Exception) as refused:
        sync_changes_record(
            nest["port"], secret_key=secret, folder=folder, device_id=device_id,
            path=f"unsealed-{secrets.token_hex(4)}.txt", manifest_hash=secrets.token_hex(32),
            size_bytes=7, path_sealed=None,
        )
    assert "path_seal_required" in str(refused.value), (
        f"a sealless record must be refused path_seal_required: {refused.value!r}"
    )

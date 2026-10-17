"""tier_3 — what an archive import leaves on the nest, witnessed from outside.

`docs/features/import-your-social-archive.md` carries `[nest]` outcomes no app
can show, because each is a property of what the nest holds after an import,
whichever app ran it. Here the import is the shared ``ArchiveImportMachine``
itself, run with no app in the loop (``fauna_ffi.harness_archive_import``: the
machine the apps build, walked Source → Done over a generated Facebook export),
and everything after is read off the nest — its disk, its wire, its lookups.

* **44** — the nest cannot read the archive or anything the app worked out from
  it: it only holds them sealed (`archive-import.md` § Architectural rules —
  *the app parses; the nest stores sealed*).
* **45** — an imported post never links back to the original and carries
  nothing that identifies it on the other service (§ What each category becomes;
  § Compatibility → *Slice-3 rulings*, ruling 2: the envelope carries
  ``platform`` only).
* **49** — nobody can look the importer up by their identity on the other
  service (§ Linking and merging (phase two): *there is no nest-wide or
  federated directory of platform identities*). The doors probed are the two a
  stranger can ask a nest by identity: ``fauna.actor.by_handle`` (the account
  table by handle — WebFinger, served only with ActivityPub on, reads the very
  same table by username, so it adds no source) and ``fauna.search.query`` (the
  one free-text door).

One dedicated nest and one import serve all three: the disk scan needs a nest
no other test has written the fixture's names into.
"""

from __future__ import annotations

import sys
import uuid
from pathlib import Path

import cbor2
import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from common import create_actor_and_register
from tests.api import ws_api

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "fakes"))
from fake_facebook_archive import PUBLIC_TEXT, FakeFacebookArchive  # noqa: E402

pytestmark = pytest.mark.tier_3

#: The importer's identity on the other service, as the fixture's profile
#: carries it (`profile_v2.username` and the tail of `profile_uri`).
OTHER_SERVICE_USERNAME = "test.owner.fixture"
OTHER_SERVICE_PROFILE = "facebook.com/test.owner.fixture"
#: People and words that exist only inside the archive and what the app works
#: out from it — the friends list, a message thread, a friend's comment — and
#: are published nowhere by an import.
ARCHIVE_ONLY = ("Friend One", "Friend Two", "friendone_abc123", "See you there", "Nice one")


@pytest.fixture(scope="module")
def imported(request, nest_mode, tmp_path_factory):
    """A dedicated nest, an importer and a stranger on it, and one finished
    archive import. The owner's name and the public post carry a run-unique
    token, so both are findable by exactly this run."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "archive-import-promises-nest"
    )
    try:
        token = f"zq{uuid.uuid4().hex[:10]}"
        owner_name = f"Owner {token}"
        archive = FakeFacebookArchive(owner=owner_name, tag=f" {token}")
        zip_path = archive.write(tmp_path_factory.mktemp("archive") / "facebook-export.zip")

        def user() -> dict:
            return create_actor_and_register(
                nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
            )

        owner, stranger = user(), user()
        result = fauna_ffi.harness_archive_import(
            nest["url"], bytes(owner["signing_key"]), str(zip_path)
        )
        assert result["step"] == "Done", f"the import never finished: {result!r}"
        assert result["imported"] > 0 and result["folder"], result
        yield {
            "nest": nest, "owner": owner, "stranger": stranger, "token": token,
            "owner_name": owner_name, "public_text": f"{PUBLIC_TEXT} {token}", "result": result,
        }
    finally:
        cleanup()


def _files_holding(root: str, needle: bytes) -> list[str]:
    held = []
    for path in Path(root).rglob("*"):
        if not path.is_file():
            continue
        try:
            if needle in path.read_bytes():
                held.append(str(path))
        except FileNotFoundError:
            continue  # a `-shm`/`-wal` side file SQLite removed mid-scan
    return held


def _imported_posts(imp) -> list[tuple[str, bytes]]:
    """Every post the importer's own local feed lists, as the raw bytes the
    by-id read serves."""
    nest, owner = imp["nest"], imp["owner"]
    ids = [p["post_id"] for p in ws_api.local_feed_posts(nest["port"], owner, limit=100)]
    return [(pid, ws_api.get_post(nest["port"], owner, pid)) for pid in ids]


def _origins(value) -> list:
    """Every ``origin`` value anywhere in a decoded post envelope."""
    found = []
    if isinstance(value, dict):
        for key, inner in value.items():
            if key == "origin":
                found.append(inner)
            found.extend(_origins(inner))
    elif isinstance(value, list):
        for inner in value:
            found.extend(_origins(inner))
    elif isinstance(value, bytes):
        try:
            found.extend(_origins(cbor2.loads(value)))
        except Exception:
            pass  # opaque bytes (a signature, a sealed body), not a nested record
    return found


# ── 44: the nest holds the archive and what the app worked out only sealed ──


@pytest.mark.feature("import-your-social-archive")
def test_the_nest_holds_the_archive_and_what_the_app_worked_out_only_sealed(imported):
    """No byte the archive alone carries is readable anywhere on the nest's disk.

    The markers are the importer's name and other-service username (the
    profile), the friends list, a message thread and a friend's comment — the
    raw archive's content and the model files the app derives from it, which
    an import keeps only in the importer's archive folder. The public post the
    same import published IS readable on disk: that is the scan's control, so a
    scan that reads nothing cannot pass.
    """
    root = imported["nest"]["tmp_dir"]
    assert _files_holding(root, imported["public_text"].encode()), (
        "the public import must be on the nest's disk in the clear, or the scan reads nothing"
    )
    for marker in (imported["owner_name"], OTHER_SERVICE_USERNAME, *ARCHIVE_ONLY):
        held = _files_holding(root, marker.encode())
        assert not held, f"archive content {marker!r} is readable on the nest's disk: {held}"


# ── 45: an imported post carries nothing identifying it on the other service ──


@pytest.mark.feature("import-your-social-archive")
def test_an_imported_post_carries_nothing_that_identifies_it_on_the_other_service(imported):
    """Every imported post's envelope names its platform and nothing else: no
    permalink, no other-service id, no profile or member path from the export."""
    posts = _imported_posts(imported)
    origins = []
    for pid, raw in posts:
        found = _origins(cbor2.loads(raw))
        if not found:
            continue
        origins.extend(found)
        for origin in found:
            assert origin == {"platform": "facebook"}, f"post {pid} carries origin {origin!r}"
        for needle in (OTHER_SERVICE_USERNAME, "facebook.com", "your_facebook_activity", "Timeline_photos"):
            assert needle.encode() not in raw, f"post {pid} carries {needle!r} from the export"
    assert len(origins) >= imported["result"]["imported"] - 1, (
        f"the import reported {imported['result']['imported']} records but only "
        f"{len(origins)} listed posts carry an origin: {[pid for pid, _ in posts]}"
    )
    assert any(imported["public_text"].encode() in raw for _, raw in posts), (
        "the public import must be among the posts read, or the check read none of them"
    )


# ── 49: no nest keeps a directory of other-service identities ──


@pytest.mark.feature("import-your-social-archive")
def test_nobody_can_look_the_importer_up_by_their_identity_on_the_other_service(imported):
    """A stranger asks both by-identity doors — handle resolution and search —
    for the importer's other-service username, profile and name, and for the
    people in the archive; neither answers. Each door's control is the same
    lookup by what the nest DOES know (the importer's own handle, the public
    post's words), so an absence is a door that works, not one that is shut."""
    nest, owner, stranger = imported["nest"], imported["owner"], imported["stranger"]

    assert ws_api.actor_by_handle(nest["port"], owner["handle"])["actor_id"] == owner["actor_id_hex"]
    for handle in (OTHER_SERVICE_USERNAME, OTHER_SERVICE_USERNAME.replace(".", "")):
        with pytest.raises(RpcCallError) as unknown:
            ws_api.actor_by_handle(nest["port"], handle)
        assert unknown.value.code.startswith(("fauna.actor.", "fauna.handle.")), unknown.value.code

    assert ws_api.search(nest["port"], stranger, imported["token"]), (
        "search must find the public import by its words, or it answers nothing at all"
    )
    for query in (
        OTHER_SERVICE_USERNAME, OTHER_SERVICE_PROFILE, imported["owner_name"], "Friend One", "Friend Two",
    ):
        hits = ws_api.search(nest["port"], stranger, query)
        assert not hits, f"search for {query!r} answered a stranger with {hits!r}"

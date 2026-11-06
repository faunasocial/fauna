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

* **17** — an imported post is never sent on to another network nor shown to
  anonymous visitors from outside Fauna (§ Compatibility → *Slice-3 rulings*,
  ruling 1). It needs a nest with ActivityPub built in and a fediverse follower,
  so it runs its own import on its own nest (``archive_ap_nest``).

One dedicated nest and one import serve 44, 45 and 49: the disk scan needs a nest
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


# ── 17: an import is never sent on, nor shown to anonymous outside visitors ──


@pytest.fixture()
def archive_ap_nest(ap_binary, tmp_path_factory):
    """One nest with ActivityPub built in, on a free port."""
    from drivers.port_util import find_free_port
    from helpers.ap_nest import start_ap_nest

    port = find_free_port()
    nest = start_ap_nest(
        ap_binary, tmp_path_factory.mktemp("archive-import-ap"), port,
        f"127.0.0.1:{port}",
    )
    try:
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.mark.feature("import-your-social-archive")
def test_an_imported_post_is_never_sent_on_or_shown_to_anonymous_outside_visitors(
    archive_ap_nest, tmp_path_factory
):
    """An importer federated with a fediverse follower imports an archive, then
    writes one post of their own. Only that native post is queued for the
    follower; the published history an anonymous outside server reads lists
    and counts only it; and no imported post's note id resolves
    (`archive-import.md` § Compatibility → *Slice-3 rulings*, ruling 1 — the
    off-box predicate refuses every archive-platform ``source``).

    The push half rides a positive signal, never a clock: the import is
    finished before the native post is written, so by the time the native
    ``Create`` is delivered a job for any import would already be queued."""
    import json
    import time
    import urllib.error
    import urllib.request

    from helpers.ap_nest import AP_BRIDGE_ID, delivery_jobs_for, enable_ap
    from helpers.fake_follower import FakeFollower
    from drivers.port_util import find_free_port
    from tests.api.bare import sign_and_encode_post

    def anonymous_get(path: str) -> tuple[int, dict]:
        req = urllib.request.Request(
            f"{nest['url']}{path}", headers={"Accept": "application/activity+json"}
        )
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                return resp.status, json.loads(resp.read())
        except urllib.error.HTTPError as err:
            return err.code, {}

    nest = archive_ap_nest
    owner = create_actor_and_register(nest["port"], admin_signing_key=nest["admin"]["signing_key"])
    username, actor_url = enable_ap(nest, owner)
    ws_api.bridge_set_settings(nest["port"], owner, AP_BRIDGE_ID, {"backfill": True})

    with FakeFollower(find_free_port()) as follower:
        code = follower.post_signed(
            f"{nest['url']}/ap/users/{username}/inbox",
            {
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Follow",
                "id": f"{follower.actor_uri}/activities/follow-1",
                "actor": follower.actor_uri,
                "object": actor_url,
            },
        )
        assert code == 202, f"the inbox did not take the Follow: HTTP {code}"
        follower.wait_for("Accept")

        token = f"zq{uuid.uuid4().hex[:10]}"
        archive = FakeFacebookArchive(owner=f"Owner {token}", tag=f" {token}")
        zip_path = archive.write(tmp_path_factory.mktemp("archive") / "facebook-export.zip")
        result = fauna_ffi.harness_archive_import(
            nest["url"], bytes(owner["signing_key"]), str(zip_path)
        )
        assert result["step"] == "Done" and result["imported"] > 0, result
        imported_ids = [
            p["post_id"] for p in ws_api.local_feed_posts(nest["port"], owner, limit=100)
        ]
        assert imported_ids, "control: the import landed posts the owner can read"

        native_id = ws_api.create_post(
            nest["port"],
            owner,
            sign_and_encode_post(
                owner["signing_key"], int(time.time() * 1_000_000), f"written here {token}"
            ),
        )
        follower.wait_for("Create")  # control: the federation leg delivers

    native_note = f"{actor_url}/notes/{native_id}"
    queued = delivery_jobs_for(nest, follower.inbox_url)
    creates = [json.loads(a) for a in queued if json.loads(a)["type"] == "Create"]
    assert [a["object"]["id"] for a in creates] == [native_note], (
        f"only the native post may be sent on: {[a['object']['id'] for a in creates]}"
    )
    sent_on = [pid for pid in imported_ids if any(pid in a for a in queued)]
    assert not sent_on, f"imported posts queued for the fediverse: {sent_on}"

    status, root = anonymous_get(f"/ap/users/{username}/outbox")
    assert status == 200 and root["totalItems"] == 1, f"the outbox counts imports: {root}"
    _, page = anonymous_get(f"/ap/users/{username}/outbox?page=0")
    assert [i["object"]["id"] for i in page["orderedItems"]] == [native_note], page
    assert anonymous_get(f"/ap/users/{username}/notes/{native_id}")[0] == 200, (
        "control: the native post's note id resolves for an anonymous reader"
    )
    shown = [
        pid for pid in imported_ids
        if anonymous_get(f"/ap/users/{username}/notes/{pid}")[0] == 200
    ]
    assert not shown, f"imported posts resolve for an anonymous outside reader: {shown}"

"""The corpus a nest UPGRADED IN PLACE must keep — seeded, read and compared the
same way at every tier that witnesses ``upgrades-never-lose-data`` outcome 18.

Two witnesses share it: the tier_3 binary hop (``tests/test_nest_data_upgrade.py``:
the pinned nest binary seeds a data dir, the HEAD binary reopens it) and the tier_4
image hop (``tests/platform/docker/test_nest_image_upgrade.py``: the pinned released
image seeds a data volume, a later image reopens it). Both ask the same at-rest
question — does build N open build N−1's ``nest.db`` and blobs without losing
anything — so both must seed, read and judge the same corpus; a copy in each would
drift until one tier proved less than the other while reading the same.

A ``nest`` here is the ordinary nest handle (``url``, ``port``, ``admin``): the
standalone dict ``common.nest.start_nest`` returns, or a docker container's
equivalent whose port the caller marked TLS-serving (``common.auth.mark_tls_nest``),
so every call below takes the right scheme from the one place it lives.

**What "nothing lost" means, mechanically** (``lost``): every value a read returned
before the upgrade is returned, equal, by the same read after it. A reply may GROW —
a field the newer build adds, an extra row it derives — because additive evolution is
the contract (``version-compatibility.md`` § Dimension 1); a reply may never drop a
field, change a value, or lose a row. The corpus spans the stores a user cannot
recreate: members and their tier (admin choices), a custom tier, a feed with posts and
a like, contact edges both ways, an inbox reach choice, a folder with recorded files
and the device that wrote them, and a stored blob read back byte for byte.
"""

from __future__ import annotations

import json
import secrets
import urllib.request

from actions.api_actor import ApiActor
from common.auth import (
    _user_call,
    create_actor_and_register,
    ensure_tier,
    set_user_tier,
    sync_changes_list,
    sync_changes_record,
    sync_register,
    user_create_folder,
)
from helpers.blob_upload import download_blob, upload_blob
from tests.api import conv_api

FOLDER = "upgrade-keeps-this"
TIER = "upgrade-keeps-tier"
POSTS = ("first words", "a second thought", "and a third")
FILES = ("notes/today.md", "photos/one.jpg")
BLOB = b"a public blob the upgrade must keep, byte for byte\n"


def health(nest: dict) -> dict:
    """``/api/v1/health`` — whose ``commit`` names the build the process was stamped
    with, which is what each build-identity guard reads."""
    with urllib.request.urlopen(nest["url"] + "/api/v1/health", timeout=10) as resp:
        return json.load(resp)


def _admin_call(nest: dict, kind: str, payload: dict):
    from common.auth import _authed_call, _coerce_admin_signing_key

    return _authed_call(
        nest["url"], _coerce_admin_signing_key(nest["admin"]["signing_key"]), kind, payload
    )


def _actor_call(nest: dict, user: dict, kind: str, payload: dict):
    return _user_call(
        nest["port"], bytes(user["signing_key"]).hex(), kind, payload, nest["url"]
    )


def seed_the_pinned_build(nest: dict) -> dict:
    """Fill the previous build's nest through the ordinary wire. Returns the ids the
    reads need — never the expected values: those are whatever the previous build
    answered (``read_all``), so a test asserts the upgrade, not the seeding."""
    admin_sk = nest["admin"]["signing_key"]
    alice = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=admin_sk)
    bob = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=admin_sk)

    # Admin choices: a tier of the admin's own, and a member moved onto it.
    ensure_tier(nest["port"], admin_signing_key=admin_sk, name=TIER,
                base_url=nest["url"], max_feeds=7)
    set_user_tier(nest["port"], alice["actor_id_hex"], TIER,
                  admin_signing_key=admin_sk, base_url=nest["url"], label="alice")

    # Content: a feed, posts, and a like that bumps a stored counter.
    a = ApiActor(nest["url"], alice["token"], alice["actor_id_hex"],
                 bytes(alice["signing_key"]))
    b = ApiActor(nest["url"], bob["token"], bob["actor_id_hex"],
                 bytes(bob["signing_key"]))
    feed_id = a.create_feed("General")
    post_ids = [a.post_to_feed(feed_id, body) for body in POSTS]
    b.like_post(post_ids[0])

    # Relationships and a reach choice.
    a.accept_knock(bob["actor_id_hex"])
    a.confirm_contact(bob["actor_id_hex"])
    b.accept_knock(alice["actor_id_hex"])
    conv_api.set_inbox_mode(nest["port"], alice, "open")

    # A folder with files, recorded by a registered device.
    alice_sk = bytes(alice["signing_key"]).hex()
    device_id = secrets.token_hex(32)
    user_create_folder(nest["port"], FOLDER, secret_key=alice_sk, base_url=nest["url"])
    sync_register(nest["port"], secret_key=alice_sk, device_id=device_id,
                  label="alice's laptop", base_url=nest["url"])
    for i, path in enumerate(FILES):
        sync_changes_record(
            nest["port"], secret_key=alice_sk, folder=FOLDER, device_id=device_id,
            path=path, manifest_hash=secrets.token_hex(32), size_bytes=100 + i,
            base_url=nest["url"])

    # Stored bytes.
    blob_hash = upload_blob(nest["port"], alice["token"], BLOB,
                            audience_class="PublicPost", mime="text/plain")
    return {"alice": alice, "bob": bob, "feed_id": feed_id, "blob_hash": blob_hash}


def read_all(nest: dict, seeded: dict) -> dict:
    """Every read the corpus answers to, keyed by what it is. The same function runs
    against the previous build and the upgraded one."""
    alice, bob = seeded["alice"], seeded["bob"]
    alice_sk = bytes(alice["signing_key"]).hex()
    return {
        # The rows, not the reply's `total`: that counts every users row, nest-internal
        # principals included, so it grows whenever a newer build adds one — an added
        # row the additive contract allows, and `lost` would read the changed count as
        # a lost value (measured 2026-11-13: the HEAD build adds one bare principal on
        # its first boot over the pinned build's data dir, every seeded row held: the
        # materializer holder's audit-only row, new since 0.1.5 —
        # `bins/fauna-nest/src/materializer/holder.rs`, `docs/goal/architecture/
        # capability-grants.md` § The materialization grant, part (b)).
        "admin users": _admin_call(nest, "fauna.admin.users.list", {})["users"],
        "admin tiers": _admin_call(nest, "fauna.admin.tiers.list", {}),
        "feed posts": _actor_call(
            nest, alice, "fauna.feed.posts", {"feed_id": seeded["feed_id"]}),
        "alice contacts": _actor_call(nest, alice, "fauna.contacts.list", {}),
        "bob contacts": _actor_call(nest, bob, "fauna.contacts.list", {}),
        "alice inbox mode": _actor_call(nest, alice, "fauna.inbox.mode.get", {}),
        "alice folders": _actor_call(nest, alice, "fauna.folders.list", {}),
        "alice devices": _actor_call(nest, alice, "fauna.sync.devices.list", {}),
        "folder files": sync_changes_list(
            nest["port"], secret_key=alice_sk, folder=FOLDER, base_url=nest["url"]),
        "blob bytes": download_blob(nest["port"], seeded["blob_hash"]),
    }


def _matches(before, after) -> bool:
    return not lost(before, after, "")


def lost(before, after, path: str = "") -> list[str]:
    """Where ``after`` fails to hold everything ``before`` held — empty when nothing
    was lost. Dicts: every key, with a holding value (added keys are fine). Lists:
    every element held by a distinct element (order and added rows are fine — a list
    is a set of rows here). Anything else: equal."""
    if isinstance(before, dict):
        if not isinstance(after, dict):
            return [f"{path or '<root>'}: was a map, now {after!r}"]
        out = []
        for key, value in before.items():
            if key not in after:
                out.append(f"{path}.{key}: dropped (was {value!r})")
            else:
                out.extend(lost(value, after[key], f"{path}.{key}"))
        return out
    if isinstance(before, list):
        if not isinstance(after, list):
            return [f"{path or '<root>'}: was a list, now {after!r}"]
        unclaimed = list(after)
        out = []
        for i, row in enumerate(before):
            hit = next((j for j, cand in enumerate(unclaimed) if _matches(row, cand)), None)
            if hit is None:
                out.append(f"{path}[{i}]: no row after the upgrade holds {row!r}")
            else:
                unclaimed.pop(hit)
        return out
    if before != after:
        return [f"{path or '<root>'}: {before!r} → {after!r}"]
    return []


def assert_nothing_lost(before: dict, after: dict, hop: str) -> None:
    """Outcome 18's assertion, the same at every tier: the corpus was really there
    before the upgrade (else "nothing lost" holds vacuously — a read that answered
    empty holds trivially), and every read still holds after it. ``hop`` names the
    two builds for the failure message."""
    feed_bodies = json.dumps(before["feed posts"], default=repr)
    for body in POSTS:
        assert body in feed_bodies, (
            f"the previous build never served the seeded post {body!r}: "
            f"{before['feed posts']!r}")
    assert len(before["folder files"]["changes"]) >= len(FILES), before["folder files"]
    assert before["blob bytes"] == BLOB

    where = {name: lost(before[name], after[name], "") for name in before}
    where = {name: lines for name, lines in where.items() if lines}
    assert not where, (
        f"the upgrade lost data the previous build held ({hop}):\n"
        + "\n".join(f"  {name}: {line}" for name, lines in where.items() for line in lines)
    )

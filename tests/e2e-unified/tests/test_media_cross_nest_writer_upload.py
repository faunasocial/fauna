"""tier_3 e2e: **a writer member on another nest uploads into a set homed on the
owner's nest, and deletes it again; a reader member is refused** — the
writer door's end-to-end witness, tui-first.

The owners: ``docs/goal/ui/media.md`` § Cross-nest shared sets, the *Upload*
paragraph (a writer member's upload seals under the set's content keys, POSTs
its chunks and manifest DIRECT to the home nest under the write token its own
nest obtains, and relays its record through its own nest; a reader is refused
with ``media.error_read_only_shared_folder``; a writer's delete is the same
relayed record, a tombstone), ``docs/goal/ui/folders.md`` § Sharing a folder
(the owner's Reader | Writer pick on ``folder-share-role-select``), and
``docs/goal/behavior/file-sync.md`` § Multi-writer shared sets (owner-pays
metering — the read the bytes-rest assertion leans on).

**What only this test proves.** The door is pinned at tier_1 in shared Rust
(``fauna-media-machine``'s ``media_lifecycle.rs``) against an in-process nest
double. Here both seats are real apps, each signed in on its own handled TLS
nest (``cross_nest_folder_seats``), and nothing drives the write but the
member's Media page:

1. The owner creates two sets in the app. Both keep their bodies on the home
   nest (the default residency — a metadata-only set would refuse the write
   before the grant is ever consulted). The owner shares set W with WRITER and
   set R with READER, from the picker, to ``member@<member nest>``; the member
   accepts both knocks in the app.
2. **Writer upload.** The member selects W's ``foreign:<channel>`` option on
   ``media-folder-filter`` and uploads a file from the Media page. The member's
   scope lists it, and the owner's home nest records it in W. **The bytes rest
   on the HOME nest**, read three ways: the owner opens the file in their own
   app and downloads the member's exact plaintext (the owner binds no folder,
   so no device of theirs holds it and the home nest's store is the only
   source); the owner's storage meter on the home nest grows by at least the
   file's size (owner-pays); and the member's meter on their own nest does not
   move.
3. **Reader refusal.** In R's scope the member's upload answers
   ``media.error_read_only_shared_folder`` on ``error-message``, and the home
   nest records nothing in R.
4. **Writer delete.** Back in W's scope the member deletes the file from the
   detail; the owner's listing of W drops it.

Two seats, two nests, one machine, no operator (convention 16). Exclusion class
(8), as the other cross-nest tests: the home nest dials the member's nest, which
docker mode refuses as non-global.
"""

from __future__ import annotations

import secrets

import pytest

from actions.media import MediaActions
from common.auth import _user_call
from drivers.http_bridge import SelectOptionNotOffered
from helpers.folder_share import (
    channel_id_for_group_hex,
    share_cross_nest_through_owner_ui,
)
from helpers.set_names import item_in_set
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

# Not audio/video: every app reads it back through the cross-app download.
_FILE_NAME = "member-written-report.pdf"
_REFUSED_NAME = "reader-must-not-land.pdf"
# Distinctive and multi-kilobyte, so a truncated or wrong-key read cannot match
# and the meter delta is well clear of any metadata-sized noise.
_FILE_BYTES = b"%PDF-bytes-a-writer-member-sealed-for-the-home-nest " * 512

# Named ceilings, polled to a deadline; a green run pays only what it uses
# (convention 14).
_KEYPACKAGE_S = 60.0
_ACCEPT_S = 180.0
_SCOPE_S = 180.0
_RECORD_S = 180.0
_DOWNLOAD_S = 120.0
_REFUSAL_S = 60.0


def _wait_member_keypackage(seats) -> None:
    """The member's login-time KeyPackage publish is best-effort async, and each
    of the owner's shares consumes one over the home→member relay."""
    from tests.api import conv_api

    nest, member = seats.member_nest, seats.member
    wait_until(
        lambda: conv_api.keypackage_count(
            nest["port"], member, member["actor_id_hex"], scheme="https"
        ) >= 2,
        _KEYPACKAGE_S,
        interval=1.0,
        diagnose=lambda: (
            "the member never published two KeyPackages on their own nest, so the "
            "owner's two shares cannot both admit them"
        ),
    )


def _secret(actor) -> str:
    return actor["signing_key"].encode().hex()


def _owner_listed(seats, set_name: str) -> int:
    """How many items the HOME nest lists in ``set_name`` for its owner — a
    read of the nest's own record, never a mutation (convention 8). A count, not
    names: the plaintext path is scrubbed from the listing (the label rides
    sealed), and the member's scope and the owner's app read the names."""
    nest = seats.owner_nest
    items = _user_call(
        nest["port"], _secret(seats.owner), "fauna.media.list",
        {"limit": 1000, "cursor_version": 2}, nest["url"],
    ).get("items", [])
    return sum(1 for item in items if item_in_set(item, set_name))


def _storage_used(nest, actor) -> int:
    """``actor``'s own transparency read on ``nest`` — ``fauna.account.get``'s
    quota block."""
    reply = _user_call(nest["port"], _secret(actor), "fauna.account.get", {}, nest["url"])
    return int(reply["quota"]["storage"]["used_bytes"])


def _error_text(driver) -> str:
    return driver.get_text("error-message") if driver.is_visible("error-message") else ""


def _scope_names(media: MediaActions, value: str) -> list[str] | None:
    """Re-enter Media, select ``value``, and return the names it lists once the
    page says it loaded (rows, or the empty state); ``None`` while the option is
    not offered or the page has not loaded."""
    media.reenter()
    try:
        # One attempt: every caller polls this to its own deadline, so an
        # option the page has not offered yet (a share still being accepted,
        # a custody record not yet read) is "not yet", never a failure here.
        media.set_filter(value, offer_timeout=0)
    except SelectOptionNotOffered:
        return None
    if not media.wait_for_loaded():
        return None
    return media.item_names()


@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_a_writer_member_on_another_nest_uploads_and_deletes_and_a_reader_is_refused(
    cross_nest_folder_seats, tmp_path
):
    """A writer member's Media upload into a set homed on the owner's nest rests
    there and lists for the owner; a reader member's is refused on
    ``error-message``; the writer's delete drops it from the owner's listing."""
    seats = cross_nest_folder_seats
    owner_app, member_app = seats.owner_app, seats.member_app
    owner_nest, member_nest = seats.owner_nest, seats.member_nest
    member_driver = member_app.driver

    _wait_member_keypackage(seats)

    # ── 1. Two sets, one WRITER share and one READER share ───────────────────
    tag = secrets.token_hex(4)
    writer_set, reader_set = f"xnwrite-{tag}", f"xnread-{tag}"
    ob = owner_app.backups
    for name in (writer_set, reader_set):
        ob.navigate_folders()
        ob.create_folder_via_wizard(name)

    qualified = f"{seats.member['handle']}@{member_nest['authority']}"
    share_cross_nest_through_owner_ui(owner_app, writer_set, qualified, role="writer")
    share_cross_nest_through_owner_ui(owner_app, reader_set, qualified, role="reader")

    mb = member_app.backups
    mb.navigate_devices()
    mb.navigate_folders()
    count = mb.wait_for_pending_shares(2, timeout=_ACCEPT_S)
    assert count == 2, (
        "the two cross-nest shares should each knock once on the member's app; "
        f"count={count} error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(1) == 1, (
        f"accepting the first knock should leave one; error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    pending = mb.wait_for_pending_shares(0)
    assert pending == 0, (
        f"accepting should consume both knocks; still {pending} pending "
        f"error={member_app.error_text()!r}"
    )

    from tests.api import conv_api

    def _scope_of(set_name: str) -> str:
        group = conv_api.folder_group_id_hex(
            owner_nest["port"], seats.owner, set_name, scheme="https"
        )
        assert group, f"the share must bind {set_name!r} to an MLS group on the home nest"
        return MediaActions.foreign_set_value(channel_id_for_group_hex(group))

    writer_scope, reader_scope = _scope_of(writer_set), _scope_of(reader_set)
    media = MediaActions(member_driver)

    # Both scopes are offered before anything is written: the step-2 and step-3
    # verdicts below then cannot be an unaccepted share in disguise.
    wait_until(
        lambda: _scope_names(media, writer_scope) is not None
        and _scope_names(media, reader_scope) is not None,
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            f"the member's Media page never offered both foreign scopes; "
            f"options={media.folder_filter_labels()!r} "
            f"error={_error_text(member_driver)!r}"
        ),
    )

    owner_used_before = _storage_used(owner_nest, seats.owner)
    member_used_before = _storage_used(member_nest, seats.member)

    # ── 2. The writer uploads into W from the Media page ─────────────────────
    upload_path = tmp_path / _FILE_NAME
    upload_path.write_bytes(_FILE_BYTES)
    assert _scope_names(media, writer_scope) is not None, "W's scope was offered a moment ago"
    media.upload_file(str(upload_path))

    names = wait_until(
        lambda: (lambda n: n if n and _FILE_NAME in n else None)(
            _scope_names(media, writer_scope)
        ),
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            f"the writer's upload of {_FILE_NAME!r} never listed in the foreign scope "
            f"{writer_scope!r}; names={media.item_names()!r} "
            f"error={_error_text(member_driver)!r}"
        ),
    )
    assert not _error_text(member_driver), (
        f"a writer's upload leaves no error banner; it reads {_error_text(member_driver)!r}"
    )
    wait_until(
        lambda: _owner_listed(seats, writer_set) == 1,
        _RECORD_S,
        interval=1.0,
        diagnose=lambda: (
            f"the home nest never recorded {_FILE_NAME!r} in {writer_set!r}; it lists "
            f"{_owner_listed(seats, writer_set)!r}"
        ),
    )

    # The bytes rest on the HOME nest: the owner's own app downloads them, and
    # no device of the owner's holds the file (they bound no folder).
    owner_media = MediaActions(owner_app.driver)
    owner_names = wait_until(
        lambda: (lambda n: n if n and _FILE_NAME in n else None)(
            _scope_names(owner_media, writer_set)
        ),
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            f"the owner's Media page never listed {_FILE_NAME!r} in {writer_set!r}; "
            f"names={owner_media.item_names()!r} error={_error_text(owner_app.driver)!r}"
        ),
    )
    owner_media.open_item_detail(owner_names.index(_FILE_NAME))
    try:
        got = owner_media.download_open_item(timeout=_DOWNLOAD_S)
    except Exception as exc:
        pytest.fail(
            f"[owner] the download of the member's {_FILE_NAME!r} produced no file: "
            f"{exc!r}. The writer door must have put its chunks and manifest on the "
            f"home nest's store; error-message: "
            f"{_error_text(owner_app.driver) or '(none)'}"
        )
    assert got == _FILE_BYTES, (
        f"[owner] the home nest's copy is not the member's plaintext "
        f"({len(got)} bytes vs {len(_FILE_BYTES)})"
    )
    owner_media.close_detail()

    owner_used_after = _storage_used(owner_nest, seats.owner)
    member_used_after = _storage_used(member_nest, seats.member)
    assert owner_used_after - owner_used_before >= len(_FILE_BYTES), (
        "a writer member's upload is owner-pays on the home nest: the owner's meter "
        f"should grow by at least {len(_FILE_BYTES)} bytes; it went "
        f"{owner_used_before} → {owner_used_after}"
    )
    assert member_used_after == member_used_before, (
        "the member's own nest keeps none of a foreign set's bytes, so the member's "
        f"meter there must not move; it went {member_used_before} → {member_used_after}"
    )

    # ── 3. The reader is refused in R's scope ────────────────────────────────
    refused_path = tmp_path / _REFUSED_NAME
    refused_path.write_bytes(_FILE_BYTES)
    assert _scope_names(media, reader_scope) is not None, "R's scope was offered a moment ago"
    media.upload_file(str(refused_path))
    refused = S.media.error_read_only_shared_folder
    wait_until(
        lambda: refused in _error_text(member_driver) or None,
        _REFUSAL_S,
        interval=0.5,
        diagnose=lambda: (
            "a reader member's upload into the foreign scope should answer "
            f"{refused!r} on error-message; it reads {_error_text(member_driver)!r}"
        ),
    )
    assert _owner_listed(seats, reader_set) == 0, (
        f"a refused reader upload must reach nothing on the home nest; "
        f"{reader_set!r} lists {_owner_listed(seats, reader_set)} item(s)"
    )

    # ── 4. The writer deletes the file from the detail ───────────────────────
    names = wait_until(
        lambda: (lambda n: n if n and _FILE_NAME in n else None)(
            _scope_names(media, writer_scope)
        ),
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            f"the writer's scope stopped listing {_FILE_NAME!r} before the delete; "
            f"names={media.item_names()!r} error={_error_text(member_driver)!r}"
        ),
    )
    media.open_item_detail(names.index(_FILE_NAME))
    media.delete_open_item()
    wait_until(
        lambda: _owner_listed(seats, writer_set) == 0,
        _RECORD_S,
        interval=1.0,
        diagnose=lambda: (
            f"the writer's delete never dropped {_FILE_NAME!r} from the home nest's "
            f"listing of {writer_set!r}; it lists {_owner_listed(seats, writer_set)!r} "
            f"error={_error_text(member_driver)!r}"
        ),
    )
    # Wrapped in a tuple: the scope is EMPTY once its one file is deleted, and
    # wait_until reads a bare ``[]`` as "not yet" (it polls for a truthy value).
    (gone,) = wait_until(
        lambda: (lambda n: (n,) if n is not None and _FILE_NAME not in n else None)(
            _scope_names(media, writer_scope)
        ),
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            f"after the delete the writer's own scope still lists {_FILE_NAME!r}; "
            f"names={media.item_names()!r} error={_error_text(member_driver)!r}"
        ),
    )
    assert _FILE_NAME not in gone

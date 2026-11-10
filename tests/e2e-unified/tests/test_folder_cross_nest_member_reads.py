"""tier_3 e2e: **a member on another nest opens a metadata-only folder's file**,
served from the owner's device through the owner's nest, and loses it when the
owner removes them. This is share-a-folder outcome 25, witnessed tui-first.

The owners: ``docs/goal/behavior/relay-serving.md`` § Relay serving → *A member
on another nest* (step (5) is the member door this test exercises; ``file-sync.md``
§ Relay serving routes to it), ``docs/goal/ui/media.md`` § Cross-nest shared sets
(the foreign browse scope: the ``foreign:<channel>`` option on
``media-folder-filter``, the download through the member door, and the removal
re-entering the scope empty with ``media.error_foreign_removed``), and
``federation-shared-folders.md`` § Cross-nest shared folders + channel append
(the custody record the share-accept writes).

**What only this test proves.** Every other cross-nest folder e2e keeps one side
headless (``test_folder_cross_nest_foreign_row.py`` mints the Welcome through API
calls, so its recipient has no content-key custody behind it, and
``test_folder_cross_nest_share_picker.py``'s Bob never comes online). Here both
seats are real apps, each signed in on its own nest (``cross_nest_folder_seats``):

1. The owner, on the set's home nest, creates the set in the app. As setup, the
   set is made metadata-only *before* any engine first sees it, as
   ``test_filesync_metadata_only_relay.py`` does, so the nest never stores its
   bytes and the relay is the only way they can move.
2. The owner shares it from the picker to ``member@<member nest>`` (outcome 21's
   gesture). The member, to whom the owner is a stranger, accepts the knock in
   the app, and the accept writes the ``ForeignFolder`` custody record.
3. The owner binds a folder and writes a file. Their agent records it, keeps the
   bytes on disk and announces the folder, so the home nest has a seat to ask.
4. The member selects the foreign set on the Media page's folder filter and
   downloads the file. The member's own nest mints the read token, the home nest
   relays each chunk from the owner's agent, and the member's custody opens it.
   **The decrypted bytes are the assertion.**
5. The owner removes the member in the app. The member's next read of the scope
   comes back empty, and ``error-message`` names the removal.

Two seats, two nests, one machine, no operator (convention 16). Exclusion class
(8), as the other cross-nest tests: the home nest dials the member's nest, which
docker mode refuses as non-global.
"""

from __future__ import annotations

import secrets
import time

import pytest

from actions.media import MediaActions
from common.auth import _user_call, set_folder_residency, sync_status
from helpers.folder_content import (
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
)
from helpers.folder_share import (
    SHARE_ROSTER_S,
    channel_id_for_group_hex,
    roster_index,
    share_cross_nest_through_owner_ui,
)
from helpers.set_names import item_in_set
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

# Not audio/video: every app reads through the cross-app download.
_FILE_NAME = "relayed-report.pdf"
# Distinctive and multi-kilobyte, so a truncated or wrong-key read cannot match.
_FILE_BYTES = b"%PDF-bytes-that-live-on-the-owners-device " * 512

# Named ceilings, polled to a deadline; a green run pays only what it uses
# (convention 14).
_KEYPACKAGE_S = 60.0
_RECORD_S = 180.0
_SCOPE_S = 180.0
_DOWNLOAD_S = 120.0


def _wait_member_keypackage(seats) -> None:
    """The member's login-time KeyPackage publish is best-effort async, and the
    owner's share consumes one over the home→member relay. Poll the
    non-destructive count on the member's own nest."""
    from tests.api import conv_api

    nest, member = seats.member_nest, seats.member
    wait_until(
        lambda: conv_api.keypackage_count(
            nest["port"], member, member["actor_id_hex"], scheme="https"
        ) > 0,
        _KEYPACKAGE_S,
        interval=1.0,
        diagnose=lambda: (
            "the member never published a KeyPackage on their own nest, so the "
            "owner's share cannot admit them to the set's MLS group"
        ),
    )


def _owner_listed(seats, set_name: str) -> list:
    nest = seats.owner_nest
    items = _user_call(
        nest["port"], seats.owner["signing_key"].encode().hex(), "fauna.media.list",
        {"limit": 1000, "cursor_version": 2}, nest["url"],
    ).get("items", [])
    return [item for item in items if item_in_set(item, set_name)]


def _member_scope_names(media: MediaActions, value: str) -> list[str] | None:
    """Re-enter Media, select the foreign scope, and return the names it lists
    once the page says it loaded (rows, or the empty state)."""
    media.reenter()
    media.set_filter(value)
    if not media.wait_for_loaded():
        return None
    return media.item_names()


def _error_text(driver) -> str:
    return driver.get_text("error-message") if driver.is_visible("error-message") else ""


@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_a_member_on_another_nest_opens_a_metadata_only_file_and_loses_it_when_removed(
    cross_nest_folder_seats, tmp_path, request
):
    """A member on another nest reads a metadata-only set's file relayed from
    the owner's device, and after the owner removes them the scope comes back
    empty with the removal named on ``error-message``."""
    seats = cross_nest_folder_seats
    owner_app, member_app = seats.owner_app, seats.member_app
    owner_nest, member_nest = seats.owner_nest, seats.member_nest
    owner_secret = seats.owner["signing_key"].encode().hex()
    member_driver = member_app.driver
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))

    _wait_member_keypackage(seats)

    # ── 1. The owner creates the set; setup makes it metadata-only ───────────
    set_name = f"xnrelay-{secrets.token_hex(4)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    reply = set_folder_residency(
        owner_nest["port"], set_name, secret_key=owner_secret,
        residency="metadata_only", base_url=owner_nest["url"],
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"

    # ── 2. The owner shares it with the member on the other nest ──────────────
    qualified = f"{seats.member['handle']}@{member_nest['authority']}"
    share_cross_nest_through_owner_ui(owner_app, set_name, qualified)

    # The member accepts the knock (the owner is a stranger to them).
    mb = member_app.backups
    mb.navigate_devices()
    mb.navigate_folders()
    count = mb.wait_for_pending_shares(1)
    assert count == 1, (
        "the cross-nest share should knock exactly once on the member's app; "
        f"count={count} error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    pending = mb.wait_for_pending_shares(0)
    assert pending == 0, (
        f"accepting should consume the knock; still {pending} pending "
        f"error={member_app.error_text()!r}"
    )

    from tests.api import conv_api

    group_id_hex = conv_api.folder_group_id_hex(
        owner_nest["port"], seats.owner, set_name, scheme="https"
    )
    assert group_id_hex, "the share must bind the set to an MLS group on the home nest"
    scope = MediaActions.foreign_set_value(channel_id_for_group_hex(group_id_hex))

    # ── 3. The owner binds a folder and writes a file ─────────────────────────
    owner_folder = tmp_path / "owner-bound"
    owner_folder.mkdir()
    bind_location_under_set(owner_app, set_name, owner_folder, seat="owner")
    atomic_write(owner_folder / _FILE_NAME, _FILE_BYTES)
    await_agent_upload(owner_app, _FILE_NAME, seat="owner")
    wait_until(
        lambda: len(_owner_listed(seats, set_name)) == 1,
        _RECORD_S,
        interval=1.0,
        diagnose=lambda: f"the home nest never recorded {_FILE_NAME!r} for the owner",
    )
    # The causal barrier for the member's read: the home nest counts the owner's
    # agent as a holder only once its announce is admitted.
    wait_until(
        lambda: sync_status(
            owner_nest["port"], secret_key=owner_secret, folder=set_name,
            base_url=owner_nest["url"],
        ).get("source_online") is True,
        _RECORD_S,
        interval=1.0,
        diagnose=lambda: (
            "the owner's agent never announced the folder, so the home nest has no "
            "device to relay its bytes from\n" + agent_diagnosis(owner_app, "owner")
        ),
    )

    # ── 4. The member opens it from the foreign scope ─────────────────────────
    media = MediaActions(member_driver)
    names = wait_until(
        lambda: (lambda n: n if n and _FILE_NAME in n else None)(
            _member_scope_names(media, scope)
        ),
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            f"the member's foreign scope {scope!r} never listed {_FILE_NAME!r}; "
            f"options={media.folder_filter_labels()!r} names={media.item_names()!r} "
            f"error={_error_text(member_driver)!r}"
        ),
    )
    media.open_item_detail(names.index(_FILE_NAME))
    assert media.wait_for_version_count(1) >= 1, (
        f"the foreign item's head never loaded as a version, so the download "
        f"cannot paint; error={_error_text(member_driver)!r}"
    )
    try:
        got = media.download_open_item(timeout=_DOWNLOAD_S)
    except Exception as exc:
        pytest.fail(
            f"[member] the download of {_FILE_NAME!r} produced no file: {exc!r}. "
            "The set is metadata-only, so its bytes reach the member only by the "
            "home nest relaying them from the owner's agent through the member "
            f"door. error-message: {_error_text(member_driver) or '(none)'}\n"
            + agent_diagnosis(owner_app, "owner")
        )
    assert got == _FILE_BYTES, (
        f"[member] the downloaded bytes are not the owner's plaintext "
        f"({len(got)} bytes vs {len(_FILE_BYTES)})"
    )
    assert not _error_text(member_driver), "a successful download leaves no error banner"
    media.close_detail()

    # ── 5. The owner removes the member; the member loses the set ─────────────
    ob.navigate_folders()
    row = ob.open_folder(set_name)
    # The owner's nest holds no `users` row for the member; their own nest named
    # them on the share's relay, so the roster reads the qualified handle the
    # owner typed — never the short-id fallback (nest.md row 4509).
    index = roster_index(ob, qualified, row=row)
    assert ob.member_handle(index, row=row) == qualified, (
        f"the cross-nest member is labelled {ob.member_handle(index, row=row)!r}, "
        f"not the qualified handle {qualified!r} their own nest announced"
    )
    ob.remove_shared_member(index, row=row)
    wait_until(
        lambda: ob.shared_member_count() == 0 or None,
        SHARE_ROSTER_S,
        diagnose=lambda: f"the removal never landed; error={owner_app.error_text()!r}",
    )

    removed = S.media.error_foreign_removed

    def _refused():
        names = _member_scope_names(media, scope)
        error = _error_text(member_driver)
        return (names, error) if removed in error else None

    names, error = wait_until(
        _refused,
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            "after the removal the member's foreign scope never named the removal "
            f"on error-message; error={_error_text(member_driver)!r} "
            f"names={media.item_names()!r} options={media.folder_filter_labels()!r}"
        ),
    )
    assert _FILE_NAME not in (names or []), (
        f"a removed member's scope must re-read empty; it lists {names!r}"
    )

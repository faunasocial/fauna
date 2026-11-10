"""tier_3 e2e: **a member on another nest reads a shared file's whole version
history** in the Media item detail, not just its current head.

The owners: ``docs/goal/ui/media.md`` § Cross-nest shared sets (*Version
history and restore across nests*: the detail's history is the home nest's,
read through the member's own nest's relay of ``fauna.files.versions.list``)
and ``federation-shared-folders.md`` § Cross-nest shared folders + channel
append (``fauna.federation.folder.versions.list``, the federated twin under the
member gate).

**What only this test proves.** The shared Rust is pinned by tier_1 tests over
fakes (``media_lifecycle.rs``, ``row_judge``) and the nest's twin by a
conformance test over a direct federation dial. Here the whole chain runs: the
member's app asks its OWN nest, which relays to the home nest, whose rows the
member's app judges under the foreign set's custody. A break between the
pieces (the relay arm, the federated kind's registration, the judge's binding
of a relayed version) shows as a one-row history.

The seats are ``test_folder_cross_nest_member_reads.py``'s
(``cross_nest_folder_seats``: two real apps, each signed in on its own nest):

1. The owner, on the set's home nest, creates the set in the app and shares it
   from the picker to ``member@<member nest>``; the member accepts the knock.
2. The owner binds a folder and writes the same file twice, so the home nest
   records two versions of one path.
3. The member selects the foreign set on the Media page's folder filter and
   opens the file's detail. **Two ``file-version-item`` rows are the assertion.**

Two seats, two nests, one machine, no operator (convention 16). Exclusion class
(8), as the other cross-nest tests: the home nest dials the member's nest, which
docker mode refuses as non-global.
"""

from __future__ import annotations

import secrets

import pytest

from actions.media import MediaActions
from common.auth import _user_call
from helpers import budgets
from helpers.folder_content import (
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
)
from helpers.folder_share import channel_id_for_group_hex
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

_FILE_NAME = "history-notes.pdf"
_FIRST = b"%PDF-the-first-version " * 256
_SECOND = b"%PDF-the-second-version, edited " * 256

# Named ceilings, polled to a deadline (convention 14).
_KEYPACKAGE_S = 60.0
_RECORD_S = 180.0
_SCOPE_S = 180.0


def _wait_member_keypackage(seats) -> None:
    """The owner's share consumes a member KeyPackage over the relay; the
    member's login-time publish is best-effort async."""
    from tests.api import conv_api

    nest, member = seats.member_nest, seats.member
    wait_until(
        lambda: conv_api.keypackage_count(
            nest["port"], member, member["actor_id_hex"], scheme="https"
        ) > 0,
        _KEYPACKAGE_S,
        interval=1.0,
        diagnose=lambda: "the member never published a KeyPackage on their own nest",
    )


def _owner_version_count(seats) -> int:
    """The owner's own read of the file's history on the home nest — the causal
    barrier that both versions are recorded before the member looks."""
    import blake3

    nest = seats.owner_nest
    reply = _user_call(
        nest["port"], seats.owner["signing_key"].encode().hex(),
        "fauna.files.versions.list",
        {"path_hash": blake3.blake3(_FILE_NAME.encode()).digest()},
        nest["url"],
    )
    return len(reply.get("versions", []))


def _error_text(driver) -> str:
    return driver.get_text("error-message") if driver.is_visible("error-message") else ""


@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_a_member_on_another_nest_reads_a_shared_files_whole_history(
    cross_nest_folder_seats, tmp_path, request
):
    """The member's foreign item detail lists both versions the home nest
    holds, read through the member's own nest."""
    seats = cross_nest_folder_seats
    owner_app, member_app = seats.owner_app, seats.member_app
    owner_nest, member_nest = seats.owner_nest, seats.member_nest
    member_driver = member_app.driver
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))

    _wait_member_keypackage(seats)

    # ── 1. The owner creates the set and shares it across nests ──────────────
    set_name = f"xnhist-{secrets.token_hex(4)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    qualified = f"{seats.member['handle']}@{member_nest['authority']}"
    row = ob.find_and_expand_folder(set_name)
    if not ob.share_button_visible():
        ob.expand_folder(row)
    ob.open_share_dialog()
    ob.share_recipient_typed(qualified)
    wait_until(
        lambda: ob.shared_member_count() == 1 or None,
        budgets.CROSS_NEST_S,
        diagnose=lambda: (
            f"sharing with {qualified!r} never put them on the owner's roster; "
            f"count={ob.shared_member_count()} error={owner_app.error_text()!r}"
        ),
    )
    mb = member_app.backups
    mb.navigate_devices()
    mb.navigate_folders()
    assert mb.wait_for_pending_shares(1) == 1, (
        f"the cross-nest share should knock once; error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, "accepting should consume the knock"

    from tests.api import conv_api

    group_id_hex = conv_api.folder_group_id_hex(
        owner_nest["port"], seats.owner, set_name, scheme="https"
    )
    assert group_id_hex, "the share must bind the set to an MLS group on the home nest"
    scope = MediaActions.foreign_set_value(channel_id_for_group_hex(group_id_hex))

    # ── 2. The owner writes the file twice ────────────────────────────────────
    owner_folder = tmp_path / "owner-bound"
    owner_folder.mkdir()
    bind_location_under_set(owner_app, set_name, owner_folder, seat="owner")
    atomic_write(owner_folder / _FILE_NAME, _FIRST)
    await_agent_upload(owner_app, _FILE_NAME, seat="owner")
    wait_until(
        lambda: _owner_version_count(seats) >= 1,
        _RECORD_S,
        interval=1.0,
        diagnose=lambda: f"the home nest never recorded {_FILE_NAME!r}",
    )
    atomic_write(owner_folder / _FILE_NAME, _SECOND)
    wait_until(
        lambda: _owner_version_count(seats) >= 2,
        _RECORD_S,
        interval=1.0,
        diagnose=lambda: (
            f"the home nest never recorded the edit of {_FILE_NAME!r} as a second "
            f"version; versions={_owner_version_count(seats)}\n"
            + agent_diagnosis(owner_app, "owner")
        ),
    )

    # ── 3. The member reads the whole history from the foreign scope ──────────
    media = MediaActions(member_driver)

    def _history_count():
        media.reenter()
        media.set_filter(scope)
        if not media.wait_for_loaded():
            return None
        names = media.item_names()
        if _FILE_NAME not in names:
            return None
        media.open_item_detail(names.index(_FILE_NAME))
        count = media.wait_for_version_count(2)
        media.close_detail()
        return count if count >= 2 else None

    count = wait_until(
        _history_count,
        _SCOPE_S,
        interval=2.0,
        diagnose=lambda: (
            f"the member's foreign item detail never listed both versions of "
            f"{_FILE_NAME!r}; versions={media.version_count()} "
            f"names={media.item_names()!r} error={_error_text(member_driver)!r}"
        ),
    )
    assert count == 2, f"the home nest holds two versions; the detail lists {count}"
    assert not _error_text(member_driver), "reading the history leaves no error banner"

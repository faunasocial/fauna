"""The Conversations kind entry of the Backups page takes a conversation
snapshot, and the local restore picker on the same page offers it.

The conversation twin of
``test_backups.py::test_a_mail_snapshot_taken_from_its_kind_entry_is_offered_for_restore``
(snapshots outcome 14 names conversations beside mail). It lives in its own
file because it needs a real conversation on the nest — a channel the user
belongs to, which only the real MLS backend makes — and the
``real_conversations`` marker that opens that backend is session-wide
(``conftest.py::_apply_real_conversations_env``): such tests run in their own
invocation, so the marker never reaches the backups page tests beside the mail
one.

tui first (``testing.md`` § Default app and nest mode), android and web
lifted; the other apps add their marker as their lift lands.
"""

from __future__ import annotations

import pytest

from helpers.waiting import wait_until
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.real_conversations,
]

# The Conversations kind entry's selector value: its i18n label key, the
# locale-stable selection key the shared machine mints
# (`fauna_backups_machine::KIND_ENTRIES`).
CONV_KIND_ENTRY = "conversations.list.title"


def _conv_snapshot_ids_from_nest(nest_instance, owner) -> set[int]:
    """The ids the nest's own `conv` list serves the owner — every channel the
    owner belongs to (the split verdict, convention 5, for which row a create
    added)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with client:
        rows = client.call("fauna.filesync.snapshot.list", {"message_kind": "conv"})["rows"]
    return {row["id"] for row in rows}


@pytest.mark.feature("snapshots")
def test_a_conversation_snapshot_taken_from_its_kind_entry_is_offered_for_restore(
    logged_in_app, nest_instance, test_user
):
    """`ui/backups.md` § Snapshot-list shape → *Kind entries*: after the folders
    the selector lists one entry per message kind the nest snapshots,
    Conversations among them; create on it is
    `SnapshotsClient::create_message_kind("conv", None)` — the batched form
    over every channel the user belongs to — and its rows restore from the
    Restore section below, so the picker there must list one without a page
    re-entry.

    Arrangement: one real 1:1 conversation with one message, sent from the app
    (the send registers the user on the channel, which is what the batched
    create walks). Barriers are causal (convention 14): the nest's own list
    says which ids the create added, and the page is then waited on for one of
    them, in the snapshot list and in the restore picker.
    """
    app = logged_in_app
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    app.conversations.enable_real_faunamls()
    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    app.conversations.real_resolve_send_new(
        peer["actor_id_hex"], "a line worth keeping in a snapshot"
    )

    before = _conv_snapshot_ids_from_nest(nest_instance, test_user)
    app.backups.navigate()

    labels = app.driver.option_texts("backup-folder-selector")
    assert labels is None or S.conversations.list.title in labels, (
        f"the selector must offer the Conversations entry under its existing "
        f"label; options painted: {labels!r}"
    )

    app.backups.select_folder(CONV_KIND_ENTRY)
    if before:
        app.backups.wait_for_snapshot_row(max(before))
    else:
        app.backups.wait_for_no_snapshots()
    app.backups.create_snapshot()

    added = _conv_snapshot_ids_from_nest(nest_instance, test_user) - before
    assert added, (
        "snapshot-create-button on the Conversations entry added no conversation "
        "snapshot on the nest (the user's channel was not pinned). "
        f"error-message: {app.error_text()!r}"
    )
    snapshot_id = max(added)
    app.backups.wait_for_snapshot_row(snapshot_id)

    marker = f"(#{snapshot_id})"
    wait_until(
        lambda: any(marker in option for option in (
            app.driver.option_texts("restore-snapshot-select") or []
        )),
        30,
        diagnose=lambda: (
            f"restore-snapshot-select never offered conversation snapshot "
            f"{snapshot_id}: {app.driver.option_texts('restore-snapshot-select')!r} "
            f"error={app.error_text()!r} "
            f"{app.driver.diagnose('restore-snapshot-select')}"
        ),
    )

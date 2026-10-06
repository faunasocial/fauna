"""tier_3 — ordinary-folder destination coverage: the folders page's
destination-places section (`folder-destination-*`; IDs user-approved
2026-08-20, built with tui leading — `backup-destinations.md` § Ordinary-folder
coverage — destination places).

The journey a user walks: enroll a backup destination on the Backups page,
then give one ordinary folder a destination place from the folder's own
expanded row — and take it away again. MUTATIONS are UI-driven (convention 8:
the attach select + button and the detach button are clicked, never a raw
`attach_folder` call); the folder and the destination-nest registration are
fixture setup (the documented carve-out — enrolling itself is UI-driven too,
via the Backups page, since that IS part of this journey); VERIFICATION reads
the nest's ground truth through `fauna.backup.destination.list` — the same
coverage read the sweep and the custodian pull consume, so what this asserts
is what the mirror actually runs on. The byte-level end of the chain (attach →
one nest sweep → the destination holds the folder's manifests + chunks +
custody rows; detach → per-path tombstones under the grace window) is pinned
by the two-nest Rust tier_3
`bins/fauna-nest/tests/nest_backup_coordinator.rs::a_source_nest_mirrors_a_covered_folder_and_detach_tears_it_down`,
which drives a real federation handshake — re-proving the bytes here would
re-implement that harness in Python for no new coverage.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import register_user
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui is the lead app; the other six join as their trickle-down legs land
    # — the marker list is the parity ledger, as in `test_folder_place_editor.py`.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    # android landed the FFI face but its own e2e
    # parametrization stays unverified — android tier_3 is host-emulator-gated,
    # the standing constraint every android e2e leg carries.
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _coverage(client, destination_label_hint: str) -> tuple[str, list[dict]]:
    """The (destination_id, covered_folders) of the owner's one destination."""
    with client:
        reply = client.call("fauna.backup.destination.list", {})
    assert reply["destinations"], f"a destination must be enrolled: {reply!r}"
    dest = reply["destinations"][0]
    assert destination_label_hint  # the hint is for the failure message only
    return dest["destination_id"], dest.get("covered_folders", [])


def _remove_the_only_destination(backups) -> None:
    """Finalizer: remove the destination this test enrolled, if it is still there.

    Best-effort on purpose — a test that already failed keeps its own diagnosis
    rather than trading it for a teardown error (`test_offline_gate.py`'s
    `_dismiss_remove_modal` posture). On a green run the test's last step has
    already removed it, so this finds nothing to do.
    """
    try:
        backups.navigate()
        if backups.destination_count() == 1:
            backups.remove_destination(0)
            backups.wait_for_destination_count(0)
    except Exception:
        pass


@pytest.mark.feature("backup-destinations-and-restore")
def test_folder_destination_place_attach_and_detach(
    logged_in_app, nest_instance, second_nest, test_user, request
):
    app = logged_in_app
    b = app.backups
    b.require_destination_management_supported()

    # Authorize the owner on the destination nest (idempotent across the
    # session-scoped nest — the `test_backup_destination_crud` shape).
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass

    # Enroll the destination through the Backups page — the same UI door a
    # user takes, and the prerequisite the folders-page section needs (with
    # no enrolled destination it deliberately does not paint).
    b.navigate()
    enrolled_here = b.destination_count() == 0
    if enrolled_here:
        b.add_destination(second_nest["url"], name="Offsite")
        b.wait_for_destination_count(1)
        # `test_user` is session-scoped, so a destination left behind is every
        # later module's input: `test_nest_trust.py` enrolls its own and counts
        # exactly one. The last step removes it; this
        # covers a run that fails before getting there. A destination this
        # test did not enroll is not its to remove.
        request.addfinalizer(lambda: _remove_the_only_destination(b))

    # Fixture: one ordinary folder via the wizard.
    name = f"covered-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)

    # ── Attach, through the folder's own expanded row. ──
    row = b.find_and_expand_folder(name)
    app.driver.wait_for("folder-destination-attach-select", timeout=15.0)
    client = _user_client(nest_instance, test_user)
    destination_id, covered_before = _coverage(client, "Offsite")
    folder_ids_before = {c["folder_id"] for c in covered_before}

    app.driver.select("folder-destination-attach-select", destination_id)
    app.driver.click("folder-destination-attach-button")

    # The section repaints from the nest's answer: the attached row appears.
    app.driver.wait_for("folder-destination-row", timeout=15.0)
    assert "Offsite" in app.driver.get_text("folder-destination-row"), (
        f"the attached row carries the destination's display name; "
        f"error={app.error_text()!r}"
    )
    assert app.driver.is_visible(
        "folder-destination-detach-button", scope="folder-destination-row[0]"
    )

    # Nest ground truth: exactly one new coverage row, whose folder_set is the
    # canonical `__folder/<source-nest-hex>/<folder-id>` name.
    _, covered_after = _coverage(client, "Offsite")
    new = [c for c in covered_after if c["folder_id"] not in folder_ids_before]
    assert len(new) == 1, f"one new coverage row: {covered_after!r}"
    folder_set = new[0]["folder_set"]
    assert folder_set.startswith("__folder/") and folder_set.endswith(
        f"/{new[0]['folder_id']}"
    ), folder_set

    # The coverage survives a collapse + re-expand (nest truth, not a local
    # flip): the row is still painted, and no attach select remains for the
    # one-and-only destination.
    b.navigate_folders()
    # Some clients (e.g. linux, whose destination-places section repaints
    # in place and never rebuilds the folder list on its own) leave the
    # expander OPEN across a `navigate_folders()` that changes no nav state
    # — collapse it explicitly so the following `find_and_expand_folder`
    # genuinely re-expands rather than toggling an already-open row SHUT
    # (`test_folder_place_editor.py`'s idiom). A client whose own repaint
    # already re-collapsed the row (e.g. one that rebuilds the list on every
    # observer tick) skips the extra toggle.
    if app.driver.is_visible("folder-destination-row"):
        b.expand_folder(row)  # collapse (the expander is a toggle)
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-destination-row", timeout=15.0)
    assert app.driver.is_absent("folder-destination-attach-select"), (
        "every enrolled destination is attached, so nothing is attachable"
    )

    # ── Detach, through the row's own button. ──
    app.driver.click(
        "folder-destination-detach-button", scope="folder-destination-row[0]"
    )
    wait_until(
        lambda: not app.driver.is_visible("folder-destination-row"),
        15.0,
        diagnose=lambda: (
            f"rows={app.driver.count('folder-destination-row')} "
            f"error={app.error_text()!r}"
        ),
    )
    _, covered_final = _coverage(client, "Offsite")
    assert {c["folder_id"] for c in covered_final} == folder_ids_before, (
        f"the coverage row is gone from the nest: {covered_final!r}"
    )
    # The destination itself is untouched — a per-folder detach is never a
    # destination removal.
    b.navigate()
    assert b.destination_count() == 1

    # Leave the session actor as found: remove the destination this test
    # enrolled, and wait for it to go, so a remove that does not take is this
    # test's red rather than a later module's wrong count.
    if enrolled_here:
        b.remove_destination(0)
        b.wait_for_destination_count(0)

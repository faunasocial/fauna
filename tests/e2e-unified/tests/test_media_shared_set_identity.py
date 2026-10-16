"""tier_3 — the Media page addresses a set by its row, not its name
(`docs/goal/ui/media.md` § Encryption at rest → *The filter and the upload
target address the row*, ruled 2026-10-11).

Before the ruling a folder shared with you under the name of one of your own
merged into it: one `media-folder-filter` option, both sets' files behind it,
and no way to upload into the shared one at all. This module drives the
sentence a user lives, through two real tui seats on one handled nest:

1. the OWNER creates a folder and shares it with the MEMBER as a **writer**;
   the member accepts, then creates a folder of their own with the SAME name;
2. the member's Media filter offers two options — their own set by name, the
   shared one by its row and labelled with its owner (clauses (a)/(b));
3. with the shared set chosen, the member uploads a file: it lands in the
   owner's set — the owner's Media lists it — and not in the member's own
   same-named set (clauses (d)/(e));
4. the owner takes write access away; the member's next upload into the shared
   set is refused with its reason on `error-message` before anything is sent,
   and the owner sees nothing new (clause (d)).

Each step reads its own observable (convention 6): the painted option labels,
the select's value membership (the driver refuses an unoffered value), each
seat's own item list, and the refusal's words.
"""

import secrets

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # tui leads (`media.md` § (f)); web and linux follow in the Linux host
    # trickle-down batch; macos/ios/windows/android add their marker and seat
    # pair in theirs.
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
]


def _scoped_names(app, value: str) -> list[str]:
    """The item names the Media page lists with the filter on ``value``,
    re-entering first so the page reads the nest afresh."""
    app.media.reenter()
    app.media.ensure_loaded()
    app.media.set_filter(value)
    return app.media.item_names()


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [("tui", "tui"), ("web", "web"), ("linux", "linux")],
    indirect=True,
)
@pytest.mark.real_conversations
# Two apps + a real MLS share/accept + custody ingest before the member can seal
# into the shared set. A ceiling, not an expectation.
@pytest.mark.timeout(900)
@pytest.mark.feature("share-a-folder")
def test_a_same_named_shared_set_is_its_own_option_and_upload_target(
    folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    from tests.api import conv_api

    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app

    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30.0,
        diagnose=lambda: "the member never published a KeyPackage the owner could admit",
    )

    # ── 1. Owner shares a writer set; member accepts and makes a namesake ──
    set_name = f"photos-{secrets.token_hex(3)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    owner_row = ob.find_and_expand_folder(set_name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])
    wait_until(
        lambda: ob.shared_member_count() == 1,
        20.0,
        diagnose=lambda: f"the share never landed; error={owner_app.error_text()!r}",
    )
    ob.set_member_access("writer", 0, row=owner_row)
    wait_until(
        lambda: ob.member_access(0, row=owner_row) == "writer",
        15.0,
        diagnose=lambda: f"the writer grant never persisted; error={owner_app.error_text()!r}",
    )
    shared_id = conv_api.folder_row_id(nest["port"], owner, set_name)
    assert shared_id is not None, "the owner's set must have a row on the nest"
    shared_value = member_app.media.shared_set_value(shared_id)

    mb = member_app.backups
    mb.navigate_folders()
    assert mb.wait_for_pending_shares(1) == 1, (
        f"[member] the share should wait as one pending knock; error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, (
        f"[member] accepting should consume the knock; error={member_app.error_text()!r}"
    )

    def _member_view() -> str:
        """Ground truth for a red: the member's Folders rows as painted, and
        the member's own `fauna.folders.list` rows (role / id / access)."""
        try:
            mb.navigate_folders()
            titles = mb.folder_titles()
        except Exception as exc:  # diagnosis only — never mask the real failure
            titles = f"<unreadable: {exc}>"
        rows = conv_api.folder_rows(nest["port"], member)
        return f"member folders painted={titles!r}; member folders.list={rows!r}"

    # ── 2. Two options: own by name, shared by row, labelled by owner ──────
    shared_label = f"{set_name} ({owner['handle']})"
    mm = member_app.media

    def _offered(*wanted: str):
        mm.reenter()
        mm.ensure_loaded()
        labels = mm.folder_filter_labels() or []
        return all(w in labels for w in wanted)

    # The join and the custody ingest, then the next Media read: generous on
    # purpose (convention 14). The shared set alone first — the baseline that
    # makes "the namesake split it" mean something.
    wait_until(
        lambda: _offered(shared_label),
        150.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] media-folder-filter should offer the shared {shared_label!r}; "
            f"painted {mm.folder_filter_labels()!r}; "
            f"error={member_app.error_text()!r}; " + _member_view()
        ),
    )

    # Now the member's own folder of the same name: a second option, never a
    # merge into the first.
    mb.navigate_folders()
    mb.create_folder_via_wizard(set_name, await_row=False)
    wait_until(
        lambda: _offered(set_name, shared_label),
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] media-folder-filter should offer {set_name!r} and "
            f"{shared_label!r}; painted {mm.folder_filter_labels()!r}; "
            f"error={member_app.error_text()!r}; " + _member_view()
        ),
    )
    labels = mm.folder_filter_labels() or []
    assert labels.count(set_name) == 1 and labels.count(shared_label) == 1, (
        f"a same-named pair is exactly two options, never merged: {labels!r}"
    )

    if app_name(member_app.driver) == "web":
        # Steps 1–2 are web's pin of the identity-addressed options; the writer
        # member's browser upload into the shared set fails at the chunk POST
        # (`TypeError: Failed to fetch`), a gap this pin found.
        skip_unbuilt(
            member_app.driver,
            surface="a writer member's upload into a shared set",
            detail="the browser's chunk upload POST fails before any HTTP status",
            tracked="",
        )

    # ── 3. Upload into the shared set lands in the OWNER's set ─────────────
    into_shared = tmp_path / f"into-shared-{secrets.token_hex(3)}.txt"
    into_shared.write_text(f"for the owner's folder — {secrets.token_hex(8)}\n")

    om = owner_app.media
    attempts: list[str] = []

    def _landed_in_owners_set():
        # Where the file lands is read on the OWNER's Media page — the set it
        # is in. The member seals under the shared set's content key, which its
        # client ingests off the receive loop after the join; until then the
        # upload is refused at seal time (fail closed) and a retry is the
        # user's move.
        if into_shared.name in _scoped_names(owner_app, set_name):
            return True
        mm.reenter()
        mm.ensure_loaded()
        mm.set_filter(shared_value)
        mm.upload_file(str(into_shared))
        mm.wait_for_error(timeout=3.0)
        attempts.append(member_app.error_text())
        return False

    wait_until(
        _landed_in_owners_set,
        240.0,
        interval=3.0,
        diagnose=lambda: (
            f"[owner] the member's upload into the shared set never reached the "
            f"owner's set; owner lists {om.item_names()!r}; the member's upload "
            f"attempts answered {attempts!r}"
        ),
    )
    assert into_shared.name not in _scoped_names(member_app, set_name), (
        "the member's OWN same-named set must not list a file uploaded into the shared one"
    )
    # The member reads its own upload back in the shared set: its row judge
    # resolves each listed set by the row the item names, so beside the own
    # namesake the shared row is judged under the SHARED set's nonce lineage
    # and roster, never the own set's (`set-addressing.md` § The row form).
    wait_until(
        lambda: into_shared.name in _scoped_names(member_app, shared_value),
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the shared set should list the member's own upload "
            f"{into_shared.name!r}; lists {mm.item_names()!r}; "
            f"error={member_app.error_text()!r}; " + _member_view()
        ),
    )

    # ── 4. Demoted to reader: the upload is refused with its reason ────────
    ob.navigate_folders()
    owner_row = ob.find_and_expand_folder(set_name)
    if ob.shared_member_count() == 0:
        ob.expand_folder(owner_row)  # the toggle closed a row left open
    ob.set_member_access("reader", 0, row=owner_row)
    wait_until(
        lambda: ob.member_access(0, row=owner_row) == "reader",
        15.0,
        diagnose=lambda: f"the demotion never persisted; error={owner_app.error_text()!r}",
    )

    refused = tmp_path / f"refused-{secrets.token_hex(3)}.txt"
    refused.write_text(f"must never land — {secrets.token_hex(8)}\n")

    def _refused_with_reason():
        mm.reenter()
        mm.ensure_loaded()
        mm.set_filter(shared_value)
        mm.upload_file(str(refused))
        return mm.wait_for_error(timeout=5.0) and (
            member_app.error_text() == S.media.error_read_only_shared_folder
        )

    wait_until(
        _refused_with_reason,
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] an upload into a set shared to read must be refused with "
            f"its reason; error={member_app.error_text()!r}"
        ),
    )
    assert refused.name not in _scoped_names(owner_app, set_name), (
        "a refused upload must never reach the owner's set"
    )


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [("tui", "tui")],
    indirect=True,
)
@pytest.mark.real_conversations
# Two apps + a real MLS share/accept + the join before the shared row is
# offered. A ceiling, not an expectation.
@pytest.mark.timeout(600)
@pytest.mark.feature("share-a-folder")
def test_a_namesake_made_before_the_accept_does_not_hide_the_shared_set(
    folder_share_owner_app, folder_share_recipient_app
):
    """The order the first test does not walk: the member already owns a
    folder of the name when the owner's share of a same-named set arrives.
    Accepting it must still put the shared row on the member's Folders page
    and in `media-folder-filter`, beside the own one (`folders.md` § Sharing a
    folder; `media.md` § Encryption at rest, clause (c))."""
    from tests.api import conv_api

    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app

    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30.0,
        diagnose=lambda: "the member never published a KeyPackage the owner could admit",
    )

    # ── 1. The member's own folder first ───────────────────────────────────
    set_name = f"photos-{secrets.token_hex(3)}"
    mb = member_app.backups
    mb.navigate_folders()
    mb.create_folder_via_wizard(set_name)

    # ── 2. The owner shares a same-named set; the member accepts ───────────
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    ob.find_and_expand_folder(set_name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])
    wait_until(
        lambda: ob.shared_member_count() == 1,
        20.0,
        diagnose=lambda: f"the share never landed; error={owner_app.error_text()!r}",
    )

    mb.navigate_folders()
    assert mb.wait_for_pending_shares(1) == 1, (
        f"[member] the share should wait as one pending knock; error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, (
        f"[member] accepting should consume the knock; error={member_app.error_text()!r}"
    )

    # ── 3. Both rows on Folders, both options on Media ─────────────────────
    def _folder_rows_named() -> int:
        mb.navigate_folders()
        return sum(1 for t in mb.folder_titles() if set_name in t)

    def _member_view() -> str:
        try:
            mb.navigate_folders()
            titles = mb.folder_titles()
        except Exception as exc:  # diagnosis only — never mask the real failure
            titles = f"<unreadable: {exc}>"
        rows = conv_api.folder_rows(nest["port"], member)
        return f"member folders painted={titles!r}; member folders.list={rows!r}"

    wait_until(
        lambda: _folder_rows_named() == 2,
        150.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the Folders page should list the own and the shared "
            f"{set_name!r}; error={member_app.error_text()!r}; " + _member_view()
        ),
    )

    shared_label = f"{set_name} ({owner['handle']})"
    mm = member_app.media

    def _offered() -> bool:
        mm.reenter()
        mm.ensure_loaded()
        labels = mm.folder_filter_labels() or []
        return set_name in labels and shared_label in labels

    wait_until(
        _offered,
        150.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] media-folder-filter should offer {set_name!r} and "
            f"{shared_label!r}; painted {mm.folder_filter_labels()!r}; "
            f"error={member_app.error_text()!r}; " + _member_view()
        ),
    )
    labels = mm.folder_filter_labels() or []
    assert labels.count(set_name) == 1 and labels.count(shared_label) == 1, (
        f"a same-named pair is exactly two options, never merged: {labels!r}"
    )

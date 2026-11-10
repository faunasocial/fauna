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
from pathlib import Path

import pytest

from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # tui leads (`media.md` § (f)); web and linux follow in the Linux host
    # trickle-down batch; macos and ios (one shared FaunaKit change) in the
    # macOS one; windows in the Windows one; android adds its marker in its own.
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]

# A real >300×300 image, so the uploading device's producer renders a tile.
FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def _scoped_names(app, value: str) -> list[str]:
    """The item names the Media page lists with the filter on ``value``,
    re-entering first so the page reads the nest afresh."""
    app.media.reenter()
    app.media.ensure_loaded()
    app.media.set_filter(value)
    return app.media.item_names()


def _share_with_member(owner_app, member_app, nest, owner, member, set_name, *, writer):
    """The preamble every test here walks: the OWNER creates ``set_name`` and
    shares it with the MEMBER (raised to writer when ``writer``), and the
    member accepts the knock. Returns the owner's set row id — the identity
    the member's Media page addresses the shared set by."""
    from tests.api import conv_api

    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30.0,
        diagnose=lambda: "the member never published a KeyPackage the owner could admit",
    )
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
    if writer:
        ob.set_member_access("writer", 0, row=owner_row)
        wait_until(
            lambda: ob.member_access(0, row=owner_row) == "writer",
            15.0,
            diagnose=lambda: f"the writer grant never persisted; error={owner_app.error_text()!r}",
        )
    shared_id = conv_api.folder_row_id(nest["port"], owner, set_name)
    assert shared_id is not None, "the owner's set must have a row on the nest"

    mb = member_app.backups
    mb.navigate_folders()
    assert mb.wait_for_pending_shares(1) == 1, (
        f"[member] the share should wait as one pending knock; error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, (
        f"[member] accepting should consume the knock; error={member_app.error_text()!r}"
    )
    return shared_id


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [
        ("tui", "tui"),
        ("web", "web"),
        ("linux", "linux"),
        pytest.param("macos", "macos", marks=pytest.mark.macos),
        pytest.param("ios", "ios", marks=pytest.mark.ios),
        # A cross-app pair: a tui owner granting to an Apple member.
        pytest.param("tui", "macos", marks=pytest.mark.macos),
        pytest.param("tui", "ios", marks=pytest.mark.ios),
        pytest.param("windows", "windows", marks=pytest.mark.windows),
        # android is the MEMBER here (a tui owner granting to the phone): its
        # filter and sort are `TokenSelect`s, so `set_filter(shared_value)` finds
        # the option by token. Not run on a device yet (venue down).
        pytest.param("tui", "android", marks=pytest.mark.android),
    ],
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

    # ── 1. Owner shares a writer set; member accepts and makes a namesake ──
    set_name = f"photos-{secrets.token_hex(3)}"
    shared_id = _share_with_member(
        owner_app, member_app, nest, owner, member, set_name, writer=True
    )
    shared_value = member_app.media.shared_set_value(shared_id)
    ob = owner_app.backups
    mb = member_app.backups

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

    # ── 3b. A picture uploaded into the shared set paints on BOTH seats ───
    # A thumbnail rests as its file rests (`media.md` § Encryption at rest →
    # *A thumbnail rests as its file rests*): the member's upload seals the
    # tile under the shared set's content key, as its file, so the OWNER paints
    # the picture the member's device produced — and the member paints it too.
    # Until 2026-10-22 a shared set carried no tile at all.
    image = FIXTURE_IMAGE.name
    assert FIXTURE_IMAGE.exists(), f"missing image fixture: {FIXTURE_IMAGE}"
    mm.reenter()
    mm.ensure_loaded()
    mm.set_filter(shared_value)
    mm.upload_file(str(FIXTURE_IMAGE))

    def _paints_the_picture(app, value: str) -> bool:
        if image not in _scoped_names(app, value):
            return False
        return app.media.thumbnail_kind(app.media.index_of(image)) == "picture"

    wait_until(
        lambda: _paints_the_picture(owner_app, set_name),
        120.0,
        interval=3.0,
        diagnose=lambda: (
            f"[owner] the member's picture in the shared set should paint on the "
            f"owner's Media page; owner lists {om.item_names()!r}; "
            f"error={owner_app.error_text()!r}"
        ),
    )
    wait_until(
        lambda: _paints_the_picture(member_app, shared_value),
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the member's own picture should paint in the shared set; "
            f"lists {mm.item_names()!r}; error={member_app.error_text()!r}"
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
    [
        ("tui", "tui"),
        ("web", "web"),
        ("linux", "linux"),
        pytest.param("macos", "macos", marks=pytest.mark.macos),
        pytest.param("ios", "ios", marks=pytest.mark.ios),
        pytest.param("tui", "ios", marks=pytest.mark.ios),
        pytest.param("windows", "windows", marks=pytest.mark.windows),
    ],
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

    # ── 1. The member's own folder first ───────────────────────────────────
    set_name = f"photos-{secrets.token_hex(3)}"
    mb = member_app.backups
    mb.navigate_folders()
    mb.create_folder_via_wizard(set_name)

    # ── 2. The owner shares a same-named set; the member accepts ───────────
    _share_with_member(owner_app, member_app, nest, owner, member, set_name, writer=False)

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


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    # tui leads (outcome 26 of `docs/features/share-a-folder.md`); the other
    # apps add their pair as they are witnessed.
    [("tui", "tui")],
    indirect=True,
)
@pytest.mark.real_conversations
# Two apps + a real MLS share/accept + the join, then two detail surfaces and a
# delete on each side. A ceiling, not an expectation.
@pytest.mark.timeout(900)
@pytest.mark.feature("share-a-folder")
def test_delete_versions_and_restore_act_on_the_chosen_row_never_the_namesake(
    folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """Outcome 26: beside an own folder of the same name, the version list,
    a restore and a delete on the SHARED folder's file act on that row alone
    (`set-addressing.md` § The row form; the doors resolve the row the opened
    item names, never its name). Both sets hold a file of the same path, so a
    door that resolved by name would read, re-point or tombstone the wrong one
    — each step reads both rows back on the member's Media page, and the
    owner's own list, as its observable."""
    from tests.api import conv_api

    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app

    # ── 1. A writer share, accepted; then the member's namesake ────────────
    set_name = f"photos-{secrets.token_hex(3)}"
    shared_id = _share_with_member(
        owner_app, member_app, nest, owner, member, set_name, writer=True
    )
    mm = member_app.media
    om = owner_app.media
    shared_value = mm.shared_set_value(shared_id)
    mb = member_app.backups
    mb.navigate_folders()
    mb.create_folder_via_wizard(set_name, await_row=False)

    def _member_view() -> str:
        try:
            mb.navigate_folders()
            titles = mb.folder_titles()
        except Exception as exc:  # diagnosis only — never mask the real failure
            titles = f"<unreadable: {exc}>"
        rows = conv_api.folder_rows(nest["port"], member)
        return f"member folders painted={titles!r}; member folders.list={rows!r}"

    # The Folders list shows both rows, never one merged.
    def _folder_rows_named() -> int:
        mb.navigate_folders()
        return sum(1 for t in mb.folder_titles() if set_name in t)

    wait_until(
        lambda: _folder_rows_named() == 2,
        150.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the Folders page should list the own and the shared "
            f"{set_name!r}; error={member_app.error_text()!r}; " + _member_view()
        ),
    )

    # ── 2. One path, three records: two in the shared set, one in the own ──
    # The owner records v1 then v2 of `same.txt` into its set; the member
    # records its own `same.txt` into the namesake. Distinct sizes make every
    # row's head readable off `media-item-size` / `file-version-size`.
    same = "same.txt"
    owned = tmp_path / "owner" / same
    owned.parent.mkdir()
    owned.write_bytes(b"v" * 1200)
    om.navigate()
    om.ensure_loaded()
    om.set_filter(set_name)
    om.upload_file(str(owned))
    wait_until(
        lambda: same in _scoped_names(owner_app, set_name),
        30.0,
        interval=1.0,
        diagnose=lambda: (
            f"[owner] {set_name!r} should list {same!r}; lists {om.item_names()!r}; "
            f"error={owner_app.error_text()!r}"
        ),
    )
    idx = om.index_of(same)
    size_v1 = om.item_size(idx)
    owned.write_bytes(b"w" * 48_000)
    om.upload_file(str(owned))
    size_v2 = om.wait_for_item_size_change(idx, size_v1, timeout=30.0)
    assert size_v2 != size_v1, (
        f"[owner] v2 of {same!r} should change the head size ({size_v1!r}); "
        f"error={owner_app.error_text()!r}"
    )

    mine = tmp_path / "member" / same
    mine.parent.mkdir()
    mine.write_bytes(b"m" * 9_000)
    mm.navigate()
    mm.ensure_loaded()
    mm.set_filter(set_name)
    mm.upload_file(str(mine))
    wait_until(
        lambda: same in _scoped_names(member_app, set_name),
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the own {set_name!r} should list {same!r}; lists "
            f"{mm.item_names()!r}; error={member_app.error_text()!r}"
        ),
    )
    size_mine = mm.item_size(mm.index_of(same))
    assert size_mine not in (size_v1, size_v2), (
        f"the fixture sizes must tell the rows apart: own {size_mine!r}, "
        f"shared {size_v1!r}/{size_v2!r}"
    )

    # The shared row lists the owner's file at its v2 head once the member's
    # client holds the set's key (the join's custody ingest; generous on
    # purpose, convention 14).
    def _shared_head() -> str | None:
        if same not in _scoped_names(member_app, shared_value):
            return None
        return mm.item_size(mm.index_of(same))

    wait_until(
        lambda: _shared_head() == size_v2,
        150.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the shared set should list {same!r} at {size_v2!r}; lists "
            f"{mm.item_names()!r} (head {_shared_head()!r}); "
            f"error={member_app.error_text()!r}; " + _member_view()
        ),
    )

    def _own_detail() -> tuple[int, str]:
        """The own namesake's `same.txt` detail: (version count, v0's size)."""
        assert same in _scoped_names(member_app, set_name), (
            f"[member] the own {set_name!r} lost {same!r}; lists {mm.item_names()!r}"
        )
        mm.open_item_detail(mm.index_of(same))
        try:
            count = mm.wait_for_version_count(1)
            return count, mm.version_size(0) if count else ""
        finally:
            mm.close_detail()

    # ── 3. The version list names the chosen row ───────────────────────────
    _scoped_names(member_app, shared_value)
    mm.open_item_detail(mm.index_of(same))
    versions = mm.wait_for_version_count(2, timeout=30.0)
    assert versions == 2, (
        f"[member] the shared {same!r} has two records — a list read by name "
        f"would show the own namesake's one; got {versions}; "
        f"error={member_app.error_text()!r}"
    )
    assert (mm.version_size(0), mm.version_size(1)) == (size_v1, size_v2), (
        f"[member] the shared versions should be the owner's {size_v1!r} → "
        f"{size_v2!r}; got {mm.version_size(0)!r} → {mm.version_size(1)!r}"
    )

    # ── 4. A restore re-points the chosen row, never the namesake ──────────
    mm.restore_version(0)
    versions = mm.wait_for_version_count(3, timeout=30.0)
    assert versions == 3, (
        f"[member] restoring v1 should append a version (2 → 3), got {versions}; "
        f"error={member_app.error_text()!r}"
    )
    assert mm.version_size(2) == size_v1, (
        f"[member] the restored head should carry v1's {size_v1!r}, got "
        f"{mm.version_size(2)!r}"
    )
    mm.close_detail()
    # The owner reads the re-point in its own set: the restore went to the
    # owner's row.
    wait_until(
        lambda: same in _scoped_names(owner_app, set_name)
        and om.item_size(om.index_of(same)) == size_v1,
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[owner] the member's restore should re-point the owner's {same!r} "
            f"to {size_v1!r}; owner lists {om.item_names()!r}; "
            f"error={owner_app.error_text()!r}"
        ),
    )
    assert _own_detail() == (1, size_mine), (
        f"[member] the own namesake's {same!r} must keep its one record at "
        f"{size_mine!r} after a restore in the shared set"
    )

    # ── 5. A delete tombstones the chosen row, never the namesake ──────────
    assert same in _scoped_names(member_app, shared_value), (
        "precondition: the shared set still lists the file"
    )
    mm.open_item_detail(mm.index_of(same))
    mm.delete_open_item()
    # Each poll re-filters, and a re-filter refreshes the page and clears its
    # banner — so the banner is read BEFORE each re-filter and any error kept,
    # or a refused delete would time out reading `error=''` (convention 6).
    delete_errors: list[str] = []

    def _shared_delete_landed() -> bool:
        seen = member_app.error_text()
        if seen and seen not in delete_errors:
            delete_errors.append(seen)
        return same not in _scoped_names(member_app, shared_value)

    wait_until(
        _shared_delete_landed,
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] deleting the shared {same!r} should drop it from the "
            f"shared set; lists {mm.item_names()!r}; errors seen after the "
            f"delete={delete_errors!r}; error now={member_app.error_text()!r}"
        ),
    )
    wait_until(
        lambda: same not in _scoped_names(owner_app, set_name),
        60.0,
        interval=2.0,
        diagnose=lambda: (
            f"[owner] the member's delete in the shared set should tombstone the "
            f"owner's {same!r}; owner lists {om.item_names()!r}"
        ),
    )
    assert _own_detail() == (1, size_mine), (
        f"[member] the own namesake's {same!r} must survive a delete in the "
        f"shared set, at its one record of {size_mine!r}"
    )

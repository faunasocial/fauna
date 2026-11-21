"""The Media page offers a reader of a shared folder only what a reader can do:
in a set shared to read, delete and restore-an-earlier-version are not shown,
and upload is blocked with its reason beside it before anything is pressed.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot; what they
can never do here is not shown). The page's instance:
``docs/goal/ui/media.md`` § What this person may write — the upload answer
(``MediaMachine::upload_possibility``, blocked with
``media.error_read_only_shared_folder`` in a set the caller may only read —
the person can lift it by picking another folder, so it stays visible) and the
write answer (``MediaMachine::write_possibility``, absent there — a reader can
never write, so never sees delete or restore).

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: the journey makes the precondition unmet on its own page and
fails when the control is enabled, when its reason is not within eyeshot, or
when a control this person can never use is shown.

**The owner's own view is the control.** The same file, at two versions, is
opened on the owner's seat first: there delete and restore are shown, so their
absence on the reader's seat is the reader's, not a page that paints neither.

The first read's *still loading* / *failed to load* arm of the upload answer
is a window a journey could only race (convention 14); the machine tests own it.
"""

from __future__ import annotations

import secrets

import pytest

from drivers.http_bridge import SelectOptionNotOffered
from helpers.app_surface import require_surface
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_media_shared_set_identity import _share_with_member

pytestmark = [pytest.mark.tier_3]


def _scoped_names(app, value: str) -> list[str]:
    """The item names Media lists with the filter on ``value``, re-entering
    first so the page reads the nest afresh."""
    app.media.reenter()
    app.media.ensure_loaded()
    app.media.set_filter(value)
    return app.media.item_names()


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [
        pytest.param("tui", "tui", marks=pytest.mark.tui),
        pytest.param("web", "web", marks=pytest.mark.web),
        pytest.param("linux", "linux", marks=pytest.mark.linux),
        pytest.param("macos", "macos", marks=pytest.mark.macos),
        pytest.param("ios", "ios", marks=pytest.mark.ios),
        # android is the READER here (a tui owner sharing to the phone).
        pytest.param("tui", "android", marks=pytest.mark.android),
    ],
    indirect=True,
)
@pytest.mark.real_conversations
# Two apps + a real MLS share/accept + custody ingest before the reader can
# read the shared set. A ceiling, not an expectation.
@pytest.mark.timeout(900)
@pytest.mark.feature("media")
def test_a_reader_is_not_offered_delete_or_restore_and_upload_says_why(
    folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app
    member_driver = member_app.driver
    require_surface(member_driver, "blocked-reason-read")

    # ── 1. The owner records two versions of one file in a fresh set ───────
    set_name = f"readonly-{secrets.token_hex(3)}"
    shared_id = _share_with_member(
        owner_app, member_app, nest, owner, member, set_name, writer=False
    )
    om = owner_app.media
    name = "possible.txt"
    path = tmp_path / name
    path.write_bytes(b"a" * 1500)
    om.navigate()
    om.ensure_loaded()
    om.set_filter(set_name)
    om.upload_file(str(path))
    wait_until(
        lambda: name in _scoped_names(owner_app, set_name),
        30.0,
        interval=1.0,
        diagnose=lambda: (
            f"[owner] {set_name!r} should list {name!r}; lists {om.item_names()!r}; "
            f"error={owner_app.error_text()!r}"
        ),
    )
    idx = om.index_of(name)
    size_v1 = om.item_size(idx)
    path.write_bytes(b"b" * 40_000)
    om.upload_file(str(path))
    size_v2 = om.wait_for_item_size_change(idx, size_v1, timeout=30.0)
    assert size_v2 != size_v1, (
        f"[owner] v2 of {name!r} should change the head size; error={owner_app.error_text()!r}"
    )

    # The control: the owner, who may write, is offered both.
    om.open_item_detail(om.index_of(name))
    try:
        assert om.wait_for_version_count(2, timeout=30.0) == 2, (
            f"[owner] {name!r} should list two versions; error={owner_app.error_text()!r}"
        )
        od = owner_app.driver
        assert not od.is_absent("media-delete-button") and od.is_enabled("media-delete-button"), (
            "[owner] the owner may write, so delete must be offered — without it the "
            f"reader's absence below proves nothing: {od.diagnose('media-delete-button', attrs=('enabled',))}"
        )
        assert od.count("file-version-restore-button") >= 1, (
            "[owner] an earlier version must offer restore to the owner: "
            f"{od.diagnose('file-version-restore-button', attrs=('enabled',))}"
        )
    finally:
        om.close_detail()

    # ── 2. The reader opens the same file in the shared set ────────────────
    mm = member_app.media
    shared_value = mm.shared_set_value(shared_id)
    def _reader_head() -> str | None:
        """The shared file's head size on the reader's seat, ``None`` while the
        share's option or the file is not offered yet."""
        mm.reenter()
        try:
            # One attempt: the wait below polls to its own deadline.
            mm.set_filter(shared_value, offer_timeout=0)
        except SelectOptionNotOffered:
            return None
        if not mm.wait_for_loaded() or name not in mm.item_names():
            return None
        return mm.item_size(mm.index_of(name))

    # The join, the custody ingest, then a Media read that lists the head:
    # generous on purpose (convention 14).
    wait_until(
        lambda: _reader_head() == size_v2,
        150.0,
        interval=2.0,
        diagnose=lambda: (
            f"[reader] the shared set should list {name!r} at {size_v2!r}; lists "
            f"{mm.item_names()!r}; error={member_app.error_text()!r}"
        ),
    )

    # Upload: shown, not live, its reason beside it — before anything is pressed.
    reason = S.media.error_read_only_shared_folder
    for element in ("upload-button", "file-upload"):
        diag = member_driver.diagnose(element, attrs=("enabled", "reason", "title"))
        assert not member_driver.is_absent(element), (
            f"[reader] {element} must stay visible in a read-only set — picking another "
            f"folder lifts the block (media.md § What this person may write, rule 1): {diag}"
        )
        assert not member_driver.is_enabled(element), (
            f"[reader] {element} is live in a set shared to read: {diag}"
        )
    painted = member_driver.blocked_reason("upload-button")
    assert painted == reason, (
        f"[reader] the reason beside upload-button should read {reason!r}, painted "
        f"{painted!r}: {member_driver.diagnose('upload-button', attrs=('enabled', 'reason', 'title'))}"
    )

    # Delete and restore: never shown to a reader.
    mm.open_item_detail(mm.index_of(name))
    try:
        assert mm.wait_for_version_count(2, timeout=30.0) == 2, (
            f"[reader] the shared {name!r} should list the owner's two versions; "
            f"error={member_app.error_text()!r}"
        )
        assert member_driver.is_absent("media-delete-button"), (
            "[reader] a reader can never delete in a set shared to read, so delete must "
            f"not be shown: {member_driver.diagnose('media-delete-button', attrs=('enabled', 'reason'))}"
        )
        assert member_driver.count("file-version-restore-button") == 0, (
            "[reader] a reader can never restore an earlier version here, so no "
            "restore may be shown: "
            f"{member_driver.diagnose('file-version-restore-button', attrs=('enabled', 'reason'))}"
        )
    finally:
        mm.close_detail()

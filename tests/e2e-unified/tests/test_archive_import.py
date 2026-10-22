"""E2E coverage for the archive-import wizard: a person brings their
Facebook posts into Fauna from the app (docs/goal/behavior/archive-import.md
§ Testing → tier_3; UX/IDs: ui.yaml `archive-import`, user-approved
2026-09-08). tui is the lead app.

tier_3 and it earns the tier: real tui, real shared-Rust parser + machine, a
real nest, real folder writes, real signed posts. The only fabricated thing is
the archive itself, which is what § Parser contract rule 7 requires.

Every mutation goes through the app UI (convention 8). The two headless calls
are precondition setup (a second actor's subscribe) and black-box verification
(a stranger's key-blob read is refused) — both outside the rule.

**A gated import shows its TEASER on the list card.** An imported post whose
original audience was Friends / Only me is authored as a gated post, and a
gated post's public body is the neutral placeholder `"<Platform> import"`
(`run.rs::preview_of`) — never any of the post's own text, on anyone's card,
the author's included (`ui/feed.md` § Encryption at rest; the plaintext lands
in the list only after this reader unseals it on the detail, which
`FeedManager::unlocked_bodies` then re-folds onto every rebuilt list). So the
journeys below read the imported text off the DETAIL and treat its absence
from the list as the security property it is, rather than as a missing post.
"""

from __future__ import annotations

import os
import re
import sys
import uuid
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from actions.archive_import import ArchiveImportActions
from actions.backups import BackupsActions
from actions.contacts import ContactsActions
from common.auth import create_actor_and_register
from helpers import budgets
from helpers.e2e_session import login_as
from helpers.waiting import wait_until

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fakes"))
from fake_facebook_archive import (  # noqa: E402
    BAD_RECORD_REASON,
    ArchivePostSpec,
    FRIENDS_TEXT,
    FRIENDS_TS,
    NEVER_IMPORTED_TEXT,
    ONLY_ME_TEXT,
    PUBLIC_TEXT,
    PUBLIC_TS,
    REPLY_EVENTS,
    FakeFacebookArchive,
    write_html_export,
    write_unrelated_zip,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

ARCHIVE_GLYPH = "📦"  # SourceGlyph::Archive — the badge every archive-origin post carries
FOLLOWERS_TIER = "followers"  # fauna_core::subscription::FOLLOWERS_TIER
OWNER_ONLY_TIER = "only-me"  # fauna_core::subscription::OWNER_ONLY_TIER
TEASER = "Facebook import"  # run.rs::preview_of(Platform::Facebook)
# `PostSummary.timestamp` is epoch MILLIS at the client boundary
# (libs/fauna-feed/src/snapshot.rs: "`FeedPostItem.created_at` is micros; the
# manager divides by 1000"); the fixture's stamps are epoch SECONDS.
MILLIS_PER_SECOND = 1_000
# 3 posts + 1 album; the event skips unless the nest's calendar is enabled.
IMPORTED_CARDS = 4
# PUBLIC_TS (2021-01-07T06:13:20Z) as the Scope step's bare UTC day.
PUBLIC_DAY = "2021-01-07"
# The people the fixture's comments, reactions, friends list and thread name.
ARCHIVE_PEOPLE = ("Friend One", "Friend Two")


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def archive_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest: every journey mints a fresh actor and imports
    into it, and dedup spans every same-platform archive folder of an account
    (§ Storage), so isolation here is what keeps test order non-load-bearing."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "archive-import-nest")
    yield nest
    cleanup()


def _fresh_actor(nest):
    return create_actor_and_register(
        nest["port"], base_url=nest["url"],
        admin_signing_key=_as_signing_key(nest["admin"]["signing_key"]),
    )


def _login(app, nest, user, handle: str) -> None:
    login_as(app, nest, user, handle=handle, device_id="test-device-archive")


def _cards_with_badge(driver, tier: str) -> list[int]:
    """Every post-card index whose `gated-post-badge` names ``tier``.

    The gate tier is what identifies an imported post whose body is still
    sealed to this reader — its text is the neutral teaser, and three of the
    four imported cards carry the same one, so the badge is the only
    discriminator a list read has (scoped queries, never global counts —
    convention 1)."""
    out = []
    for k in range(driver.count("post-card")):
        scope = f"post-card[{k}]"
        if driver.count("gated-post-badge", scope=scope) and (
            driver.get_text("gated-post-badge", scope=scope) == tier
        ):
            out.append(k)
    return out


def _the_followers_card(app) -> int:
    """The one card gated to the followers tier — the Friends-audience post."""
    cards = wait_until(
        lambda: _cards_with_badge(app.driver, FOLLOWERS_TIER),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"no card carries a {FOLLOWERS_TIER!r} gated-post-badge; "
            f"{app.driver.count('post-card')} cards rendered, "
            f"error-message={app.error_text()!r}"
        ),
    )
    assert len(cards) == 1, f"exactly one Friends-audience post was imported, got cards {cards}"
    return cards[0]


def _unseal_on_detail(app, index: int, text: str) -> None:
    """Open the card's detail and wait for the full body — the entitled
    reader's unseal (`fauna_feed::FeedManager::unlock_gated_post`)."""
    app.feed.open_post_detail(index)
    app.driver.wait_for("feed-post-detail-body")
    assert wait_until(
        lambda: text in app.feed.post_detail_body(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the gated import never unsealed: {app.feed.post_detail_body()!r}; "
            f"error-message={app.error_text()!r}"
        ),
    )


def _walk_to_confirm(wizard: ArchiveImportActions, zip_path: Path, recorded_audiences: int | None = 3) -> None:
    """Source → Archive → Scope → Confirm, as a user does it. The Scope step's
    audience line is checked against ``recorded_audiences`` (the default
    fixture's three posts and albums with one), unless that is ``None``."""
    wizard.navigate()
    assert wizard.is_page_visible(), f"page error: {wizard.error_text()!r}"
    wizard.select_source("Facebook")
    wizard.next()
    wizard.set_archive_path(str(zip_path))
    wizard.open_archive()
    summary = wait_until(
        lambda: wizard.archive_summary() or None,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the archive never indexed; error was {wizard.error_text()!r}",
    )
    assert summary.startswith("Facebook · Test Owner"), summary
    wizard.next()
    assert wait_until(lambda: wizard.category_labels(), budgets.UI_SETTLE_S,
                      diagnose=lambda: "the Scope step listed no categories")
    assert wizard.category_state("Posts") == "on", "posts default to selected"
    assert wizard.category_state("Friends") == "kept", "friends are kept in the archive, never selectable"
    if recorded_audiences is not None:
        assert wizard.audience_summary().startswith(
            f"{recorded_audiences} posts and albums have a recorded audience"
        ), wizard.audience_summary()
    wizard.next()
    assert wait_until(lambda: wizard.confirm_summary() or None, budgets.UI_SETTLE_S,
                      diagnose=lambda: "no confirm summary")


def _wait_done(wizard: ArchiveImportActions) -> str:
    return wait_until(
        lambda: wizard.done_summary() or None,
        budgets.RPC_ROUNDTRIP_S * 3,
        diagnose=lambda: f"the import never reached Done; progress {wizard.progress_summary()!r}, "
        f"skips {wizard.error_log()!r}, error {wizard.error_text()!r}",
    )


def _wait_imported_cards(app) -> int:
    return wait_until(
        lambda: app.driver.count("post-card") == IMPORTED_CARDS or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the author's feed should list the {IMPORTED_CARDS} imported records, "
            f"got {app.driver.count('post-card')}; error-message={app.error_text()!r}"
        ),
    )


@pytest.mark.feature("import-your-social-archive")
def test_the_wizard_is_reachable_and_opens_on_the_source_step(logged_in_app):
    wizard = ArchiveImportActions(logged_in_app.driver)
    wizard.navigate()
    assert wizard.is_page_visible(), f"page error: {wizard.error_text()!r}"


@pytest.mark.feature("import-your-social-archive")
def test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates(app, request, archive_nest, tmp_path):
    """§ Goal item 1: own posts at their original dates and audiences, badged;
    § Storage: the archive rests in a folder the user can see."""
    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-owner"))
    # The archive also carries a record the parser cannot read, the files
    # Fauna never imports (ads, logins, saved items, liked pages) and a
    # section from a newer export layout.
    zip_path = FakeFacebookArchive(tag=" " + _unique("#walk"), bad_record=True).write(tmp_path / "facebook.zip")

    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, zip_path)
    wizard.start()
    done = _wait_done(wizard)
    # 3 posts + 1 album; events skip unless the nest's calendar is enabled.
    # The unreadable record never stopped the run (§ Parser contract rule 1),
    # nor did the newer layout's section (rule 2).
    assert done.startswith("4 imported"), done
    skipped = int(re.search(r"(\d+) skipped", done).group(1))
    assert skipped >= 1, f"the unreadable record is counted as skipped: {done!r}"
    wizard.review_skipped()
    assert wait_until(
        lambda: BAD_RECORD_REASON in wizard.error_log(),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the skip log does not list the unreadable record: {wizard.error_log()!r}",
    )
    folder = wizard.folder_link_text()
    assert "Facebook archive" in folder, folder
    folder_name = folder.split(": ", 1)[1]

    feed = app.feed
    app.driver.navigate_to("feed")
    assert feed.wait_for_post_text(PUBLIC_TEXT), f"error-message={app.error_text()!r}"
    _wait_imported_cards(app)
    park = feed.wait_for_post_state_by_text(PUBLIC_TEXT)
    assert park is not None
    # Backdated: the state row's timestamp is the ARCHIVE's instant, not now.
    assert park["timestamp"] == PUBLIC_TS * MILLIS_PER_SECOND, park["timestamp"]
    i = feed.post_index_by_text(PUBLIC_TEXT)
    assert app.driver.count("protocol-badge", scope=f"post-card[{i}]") == 1
    assert app.driver.get_text("protocol-badge", scope=f"post-card[{i}]") == ARCHIVE_GLYPH

    # The friends-only post is gated to the followers tier; only-me and the
    # audience-less album to the reserved owner-only one. Neither body is on a
    # card — a gated import's public preview is the neutral teaser, and putting
    # the user's own words there would be the leak § Audience mapping forbids.
    assert not feed.post_text_visible(FRIENDS_TEXT), "a friends-only import must not publish its text"
    assert not feed.post_text_visible(ONLY_ME_TEXT), "an only-me import must not publish its text"
    assert len(_cards_with_badge(app.driver, OWNER_ONLY_TIER)) == 2, (
        "the only-me note and the audience-less album both rest at the owner-only tier"
    )
    j = _the_followers_card(app)
    assert i < j, "the feed sorts newest first: the 2021 post sits above the 2020 one"
    assert TEASER in feed.post_text(j), feed.post_text(j)
    # Ads, logins, saved items and liked pages became nothing.
    assert not any(NEVER_IMPORTED_TEXT in feed.post_text(k) for k in range(feed.post_count()))

    # The author unseals their own friends-only import through custody, and the
    # unsealed body then carries the archive's own instant.
    _unseal_on_detail(app, j, FRIENDS_TEXT)
    app.driver.navigate_to("feed")
    friends_row = wait_until(
        lambda: feed.post_state_by_text(FRIENDS_TEXT),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the unsealed body never re-folded onto the list",
    )
    assert friends_row["timestamp"] == FRIENDS_TS * MILLIS_PER_SECOND, friends_row["timestamp"]

    folders = BackupsActions(app.driver)
    folders.navigate_folders()
    titles = wait_until(
        lambda: [folders.folder_title(k) for k in range(folders.folder_count())] or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the Folders page listed nothing",
    )
    assert any(folder_name in t for t in titles), (folder_name, titles)

    # Nobody named in the archive became a contact (§ Architectural rules).
    contacts = ContactsActions(app.driver)
    contacts.navigate()
    names = contacts.contact_names()
    assert not any(person in name for person in ARCHIVE_PEOPLE for name in names), names


@pytest.mark.feature("import-your-social-archive")
def test_a_restart_mid_import_resumes_from_the_folder(app, request, archive_nest, tmp_path):
    """§ The wizard and its machine step 5: a real restart resumes from the
    folder. The pause is a causal anchor armed BEFORE Start (convention 14):
    the machine pauses itself after exactly one settled record."""
    owner = _fresh_actor(archive_nest)
    handle = _unique("e2e-archive-restart")
    _login(app, archive_nest, owner, handle=handle)
    zip_path = FakeFacebookArchive(tag=" " + _unique("#restart")).write(tmp_path / "facebook.zip")

    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, zip_path)
    wizard.arm_pause_after(1)
    wizard.start()
    paused = wait_until(
        lambda: (s := wizard.progress_summary()).startswith("Paused") and s or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the run never paused; progress {wizard.progress_summary()!r}",
    )
    assert "1 of" in paused, paused
    # Step 5: elapsed time and, one record watched, an estimate of the rest.
    assert " elapsed · about " in paused and paused.endswith(" left"), paused
    # Per-kind progress and the overall bar.
    assert 0.0 < wizard.progress_fraction() < 1.0, wizard.progress_fraction()
    rows = dict(wizard.category_progress())
    assert rows.get("Posts", "").startswith("1 of 3"), rows

    # The restart: tear the app down and launch it again (the driver's own
    # relaunch), then sign the same actor back in. Nothing about the IMPORT is
    # held in the app — the folder is the session — but the app's own
    # client-local store must survive, because a restart is the same installed
    # app coming back on the same machine, not a fresh profile. Without the pin
    # the driver relaunches on a fresh XDG base and the app mints a SECOND sync
    # device, which the free tier's `max_devices` cap then refuses: the resumed
    # run died on `fauna.sync.device_limit_exceeded` at "1 of 5" (measured
    # 2026-09-09). A real restart re-registers the device id it already holds,
    # which that quota never rejects.
    assert app.driver.preserve_state_across_relaunch(), (
        "this journey needs the driver to pin its client-local store across the "
        "relaunch (tui/linux do); a fresh store makes the app a new device"
    )
    assert app.driver.recover(), "the driver could not relaunch the app"
    _login(app, archive_nest, owner, handle=handle)
    wizard = ArchiveImportActions(app.driver)
    wizard.navigate()
    resumed = wait_until(
        lambda: (s := wizard.progress_summary()).startswith("Paused") and s or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"hydrate did not land on the paused import; error {wizard.error_text()!r}",
    )
    assert "1 of" in resumed, resumed
    # A fresh process has watched nothing yet: elapsed, and no estimate.
    assert " elapsed · estimating the time left" in resumed, resumed
    wizard.resume()
    done = _wait_done(wizard)
    assert done.startswith("4 imported"), done

    feed = app.feed
    app.driver.navigate_to("feed")
    assert feed.wait_for_post_text(PUBLIC_TEXT), f"error-message={app.error_text()!r}"
    _wait_imported_cards(app)
    # Each record exactly once: the resume re-authored nothing.
    assert sum(1 for k in range(feed.post_count()) if PUBLIC_TEXT in feed.post_text(k)) == 1
    assert len(_cards_with_badge(app.driver, FOLLOWERS_TIER)) == 1
    assert len(_cards_with_badge(app.driver, OWNER_ONLY_TIER)) == 2


def _another_device(request, nest):
    """One MORE seat of the app under test, launched fresh beside ``app`` — its
    own empty client-local store, so it is a new device of whoever signs in,
    not the same device coming back. Returns ``(seat_app, driver)``; the caller
    tears the driver down."""
    from conftest import _build_app_config
    from actions import ActionLayer
    from drivers import create_driver

    driver = create_driver("tui")
    driver.launch(_build_app_config("tui", nest, request))
    return ActionLayer(driver), driver


@pytest.mark.feature("import-your-social-archive")
def test_an_import_started_on_one_device_is_finished_from_another(app, request, archive_nest, tmp_path):
    """§ The wizard and its machine step 5: the folder is the session, so a
    real restart resumes from it on ANY device — cross-device, not
    same-process. Device A starts the import and is paused by a causal anchor
    armed before Start (convention 14); device B is a second seat of the same
    account with a fresh store (never `preserve_state_across_relaunch`), so
    nothing it knows of the import came from A's process. It finds the paused
    import, resumes it from the folder and finishes it, and every record lands
    exactly once.

    The free tier's `max_devices` is 3 (`admin.md` § 2 Users → *Device
    enforcement*), so the second seat's own sync device fits beside A's. Two
    devices resuming one folder at once have no lease (archive-import.md
    § Implementation status today); A stays paused throughout."""
    owner = _fresh_actor(archive_nest)
    handle = _unique("e2e-archive-handover")
    _login(app, archive_nest, owner, handle=handle)
    zip_path = FakeFacebookArchive(tag=" " + _unique("#handover")).write(tmp_path / "facebook.zip")

    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, zip_path)
    wizard.arm_pause_after(1)
    wizard.start()
    paused = wait_until(
        lambda: (s := wizard.progress_summary()).startswith("Paused") and s or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the run never paused on device A; progress {wizard.progress_summary()!r}",
    )
    assert "1 of" in paused, paused

    seat_b, driver_b = _another_device(request, archive_nest)
    try:
        login_as(seat_b, archive_nest, owner, handle=handle, device_id="test-device-archive-b")
        wizard_b = ArchiveImportActions(driver_b)
        wizard_b.navigate()
        found = wait_until(
            lambda: (s := wizard_b.progress_summary()).startswith("Paused") and s or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                f"device B never found the import device A paused; progress "
                f"{wizard_b.progress_summary()!r}, error {wizard_b.error_text()!r}"
            ),
        )
        assert "1 of" in found, f"device B must pick up at A's checkpoint: {found!r}"
        wizard_b.resume()
        done = _wait_done(wizard_b)
        assert done.startswith("4 imported"), done

        feed = seat_b.feed
        driver_b.navigate_to("feed")
        assert feed.wait_for_post_text(PUBLIC_TEXT), f"error-message={seat_b.error_text()!r}"
        _wait_imported_cards(seat_b)
        # Each record exactly once: B re-authored nothing A had already landed.
        assert sum(1 for k in range(feed.post_count()) if PUBLIC_TEXT in feed.post_text(k)) == 1
        assert len(_cards_with_badge(driver_b, FOLLOWERS_TIER)) == 1
        assert len(_cards_with_badge(driver_b, OWNER_ONLY_TIER)) == 2
    finally:
        try:
            driver_b.screenshot("teardown-archive-import-device-b")
        except Exception:
            pass
        driver_b.teardown()


@pytest.mark.feature("import-your-social-archive")
def test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not(app, request, archive_nest, tmp_path):
    """§ Audience mapping: Friends → the followers tier. The import provisioned
    that tier; an approved follower unseals the post, a stranger cannot."""
    owner = _fresh_actor(archive_nest)
    follower = _fresh_actor(archive_nest)
    stranger = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-author"))
    zip_path = FakeFacebookArchive(tag=" " + _unique("#friends")).write(tmp_path / "facebook.zip")
    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, zip_path)
    wizard.start()
    assert _wait_done(wizard).startswith("4 imported")

    # Follower: headless subscribe (precondition), the author approves through
    # the UI when the tier queues requests.
    sub = ApiActor(archive_nest["url"], follower["token"], follower["actor_id_hex"], bytes(follower["signing_key"]))
    reply = sub.subscribe(owner["actor_id_bytes"], FOLLOWERS_TIER)
    if reply.get("outcome") == "queued":
        subs = app.subscriptions
        subs.navigate()
        subs.open_tiers_tab()
        subs.refresh()
        assert subs.wait_for_pending_request(1), subs.error_text()
        subs.approve_first_request()
        assert subs.wait_for_subscriber(1), subs.error_text()
    else:
        assert reply.get("outcome") in ("approved", "active"), reply

    _login(app, archive_nest, follower, handle=_unique("e2e-archive-follower"))
    feed = app.feed
    assert feed.wait_post_count(1) >= 1, f"error-message={app.error_text()!r}"
    # The follower sees the teaser, never the friends-only text, until they
    # open the detail — where the minted KeyBlob entry unseals it.
    assert not feed.post_text_visible(FRIENDS_TEXT)
    j = _the_followers_card(app)
    assert TEASER in feed.post_text(j), feed.post_text(j)
    _unseal_on_detail(app, j, FRIENDS_TEXT)

    # Stranger: never subscribed — no card with the text, and the key blob is refused.
    _login(app, archive_nest, stranger, handle=_unique("e2e-archive-stranger"))
    assert feed.wait_for_post_text_absent(FRIENDS_TEXT)
    outsider = ApiActor(archive_nest["url"], stranger["token"], stranger["actor_id_hex"], bytes(stranger["signing_key"]))
    with pytest.raises(Exception):
        outsider.subscription_key_blob(owner["actor_id_bytes"], FOLLOWERS_TIER)


def _selected_record_count(wizard: ArchiveImportActions) -> int:
    """What the Scope step's selected rows add up to — `Posts (3)` and so on."""
    total = 0
    for label in wizard.category_labels():
        if wizard.category_state(label) == "on":
            total += int(re.search(r"\((\d+)\)", label).group(1))
    return total


def _open_refused(wizard: ArchiveImportActions, app, zip_file: Path, reason: str) -> None:
    """Opening ``zip_file`` is refused for ``reason`` on the Archive step."""
    wizard.set_archive_path(str(zip_file))
    wizard.open_archive()
    assert wait_until(
        lambda: reason in wizard.error_text(),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"{zip_file.name} was not refused for {reason!r}: error {wizard.error_text()!r}, "
            f"summary {wizard.archive_summary()!r}"
        ),
    )
    assert wizard.archive_summary() == "", wizard.archive_summary()
    assert app.driver.is_visible("archive-import-archive-path"), (
        f"the refusal left the Archive step: source picker visible="
        f"{app.driver.is_visible('archive-import-source-picker')}, "
        f"scope visible={app.driver.is_visible('archive-import-scope-date-from')}"
    )


def _refused_on_scope(wizard: ArchiveImportActions, app, reason: str) -> None:
    """Next was refused for ``reason``, and Scope is still showing."""
    assert wait_until(
        lambda: reason in wizard.error_text(),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"Next was not refused for {reason!r}: error {wizard.error_text()!r}, "
            f"confirm summary {wizard.confirm_summary()!r}"
        ),
    )
    assert app.driver.is_visible("archive-import-scope-date-from"), "the wizard left the Scope step"
    assert wizard.confirm_summary() == "", wizard.confirm_summary()


@pytest.mark.feature("import-your-social-archive")
def test_the_wizard_guards_each_step_and_writes_nothing_before_start(
    app, request, archive_nest, tmp_path, tui_opened_urls
):
    """§ The wizard and its machine, steps 1–4, and § Parser contract: the
    export page is linked and the link opens it through the app's OS opener
    (the session's fake, `tui_opened_urls`), a wrong file is refused naming what to request, a
    malformed or inverted date range stops the wizard, the Confirm step
    totals what was chosen — and walking all of it and backing out writes
    nothing: the Start that follows makes the account's one archive folder."""
    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-guards"))
    wizard = ArchiveImportActions(app.driver)
    wizard.navigate()
    assert wizard.is_page_visible(), f"page error: {wizard.error_text()!r}"

    # Step 1 links to the chosen platform's own export page, and activating the
    # link hands exactly that address to the OS opener.
    for platform, host in (("Instagram", "instagram.com"), ("Facebook", "facebook.com")):
        wizard.select_source(platform)
        link_text = wait_until(
            lambda: (h := wizard.source_export_link()) and host in h and h or None,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"step 1 does not link to {host}: {wizard.source_export_link()!r}",
        )
        address = re.search(rf"https://\S*{re.escape(host)}/\S+", link_text)
        assert address, link_text
        assert platform.lower() in wizard.source_help().lower(), wizard.source_help()
        opened_before = len(tui_opened_urls())
        wizard.open_source_export_link()
        opened = wait_until(
            lambda: tui_opened_urls()[opened_before:] or None,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"activating the {platform} link opened nothing: {tui_opened_urls()!r}",
        )
        assert opened == [address.group(0)], (opened, link_text)
    wizard.next()
    app.driver.wait_for("archive-import-archive-path")

    # Step 2 refuses a zip no supported service exported, and an HTML export
    # with a message naming the format to request; each refusal leaves the
    # Archive step showing, ready for another file.
    for zip_file, reason in (
        (write_unrelated_zip(tmp_path / "unrelated.zip"), "not a Facebook or Instagram export"),
        (write_html_export(tmp_path / "facebook-html.zip"), "request the JSON format"),
    ):
        _open_refused(wizard, app, zip_file, reason)

    zip_path = FakeFacebookArchive(tag=" " + _unique("#guards")).write(tmp_path / "facebook.zip")
    wizard.set_archive_path(str(zip_path))
    wizard.open_archive()
    assert wait_until(lambda: wizard.archive_summary() or None, budgets.UI_SETTLE_S,
                      diagnose=lambda: f"the archive never indexed; error was {wizard.error_text()!r}")
    wizard.next()
    assert wait_until(lambda: wizard.category_labels(), budgets.UI_SETTLE_S,
                      diagnose=lambda: "the Scope step listed no categories")

    # Step 3: a mistyped date, and a start after the end, each stop the wizard.
    wizard.set_date_from("2021-13-45")
    wizard.next()
    _refused_on_scope(wizard, app, "YYYY-MM-DD")
    wizard.set_date_from("2021-02-01")
    wizard.set_date_to("2021-01-01")
    wizard.next()
    _refused_on_scope(wizard, app, "after the until date")
    wizard.set_date_from("")
    wizard.set_date_to("")

    # Choose the kinds: albums off, and the Confirm step totals what is left.
    wizard.toggle_category("Albums")
    assert wait_until(lambda: wizard.category_state("Albums") == "off", budgets.UI_SETTLE_S,
                      diagnose=lambda: "the Albums row did not switch off")
    expected = _selected_record_count(wizard)
    wizard.next()
    confirm = wait_until(lambda: wizard.confirm_summary() or None, budgets.UI_SETTLE_S,
                         diagnose=lambda: f"no confirm summary; error {wizard.error_text()!r}")
    assert confirm.startswith(f"{expected} records · about "), (expected, confirm)

    # Back out before Start, then come forward and start: had the first visit
    # to Confirm written anything, Start would find its folder name taken and
    # name the archive's folder `… -2` (§ Storage — one folder per import), and
    # the Folders page would list two. One folder, plainly named, is the
    # observable that nothing was written before Start — and a non-empty list
    # is what proves the page loaded.
    wizard.back()
    assert wait_until(lambda: wizard.category_labels(), budgets.UI_SETTLE_S,
                      diagnose=lambda: "Back did not return to the Scope step")
    wizard.next()
    assert wait_until(lambda: wizard.confirm_summary() or None, budgets.UI_SETTLE_S,
                      diagnose=lambda: f"no confirm summary; error {wizard.error_text()!r}")
    wizard.start()
    _wait_done(wizard)
    folder_name = wizard.folder_link_text().split(": ", 1)[1]
    assert re.fullmatch(r"Facebook archive \d{4}-\d{2}-\d{2}", folder_name), folder_name
    folders = BackupsActions(app.driver)
    folders.navigate_folders()
    titles = wait_until(
        lambda: [folders.folder_title(k) for k in range(folders.folder_count())] or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the Folders page listed nothing, not even the import's folder",
    )
    assert sum("archive" in t.lower() for t in titles) == 1, titles


@pytest.mark.feature("import-your-social-archive")
def test_a_one_day_only_me_import_takes_that_whole_day_visible_to_the_owner_alone(app, request, archive_nest, tmp_path):
    """§ The wizard and its machine step 3: a date range names whole UTC days
    at both ends, and the audience mode can put everything at only-me. The
    range is the single day of the 06:13 public post — an end that stopped at
    the day's midnight would import nothing."""
    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-oneday"))
    zip_path = FakeFacebookArchive(tag=" " + _unique("#oneday")).write(tmp_path / "facebook.zip")
    wizard = ArchiveImportActions(app.driver)
    wizard.navigate()
    assert wizard.is_page_visible(), f"page error: {wizard.error_text()!r}"
    wizard.select_source("Facebook")
    wizard.next()
    wizard.set_archive_path(str(zip_path))
    wizard.open_archive()
    assert wait_until(lambda: wizard.archive_summary() or None, budgets.UI_SETTLE_S,
                      diagnose=lambda: f"the archive never indexed; error was {wizard.error_text()!r}")
    wizard.next()
    assert wait_until(lambda: wizard.category_labels(), budgets.UI_SETTLE_S,
                      diagnose=lambda: "the Scope step listed no categories")
    wizard.select_audience_mode("Only me")
    assert wait_until(lambda: wizard.audience_summary() == "Everything will be visible only to you.",
                      budgets.UI_SETTLE_S,
                      diagnose=lambda: f"the only-me mode is not stated: {wizard.audience_summary()!r}")
    wizard.set_date_from(PUBLIC_DAY)
    wizard.set_date_to(PUBLIC_DAY)
    wizard.next()
    assert wait_until(lambda: wizard.confirm_summary() or None, budgets.UI_SETTLE_S,
                      diagnose=lambda: f"no confirm summary; error {wizard.error_text()!r}")
    wizard.start()
    done = _wait_done(wizard)
    assert done.startswith("1 imported"), done

    feed = app.feed
    app.driver.navigate_to("feed")
    owner_only = wait_until(
        lambda: _cards_with_badge(app.driver, OWNER_ONLY_TIER) or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"no owner-only card; {app.driver.count('post-card')} cards, error {app.error_text()!r}",
    )
    assert app.driver.count("post-card") == 1, "only the one post of that day was imported"
    assert owner_only == [0], owner_only
    # Public on Facebook, only-me here: its words are not on the card.
    assert not feed.post_text_visible(PUBLIC_TEXT)
    _unseal_on_detail(app, 0, PUBLIC_TEXT)


def _wait_progress(wizard: ArchiveImportActions, state: str, settled: int) -> str:
    """The Progress summary once it reads ``<state> · <settled> of …``."""
    want = f"{state} · {settled} of "
    return wait_until(
        lambda: (s := wizard.progress_summary()).startswith(want) and s or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"progress never read {want!r}: {wizard.progress_summary()!r}, "
        f"error {wizard.error_text()!r}",
    )


@pytest.mark.feature("import-your-social-archive")
def test_the_user_pauses_resumes_and_cancels_and_what_arrived_stays(app, request, archive_nest, tmp_path):
    """§ The wizard and its machine step 5: pause / resume / cancel, and cancel
    keeps what landed. The run is HELD (still running) after one record by a
    causal anchor armed before Start (convention 14), so the user's own Pause
    and Cancel clicks land on a running import rather than racing its end."""
    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-pause"))
    zip_path = FakeFacebookArchive(tag=" " + _unique("#pause")).write(tmp_path / "facebook.zip")

    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, zip_path)
    wizard.arm_hold_after(1)
    wizard.start()
    _wait_progress(wizard, "Importing", 1)
    wizard.pause()
    _wait_progress(wizard, "Paused", 1)

    # Resume carries on from where it stopped; held again one record later.
    wizard.arm_hold_after(1)
    wizard.resume()
    _wait_progress(wizard, "Importing", 2)
    wizard.cancel()
    _wait_progress(wizard, "Cancelled", 2)
    # A cancelled import is an end state: Back opens a fresh wizard rather
    # than the cancelled run.
    wizard.back()
    assert wizard.is_page_visible(), f"Back did not open a fresh wizard; error {wizard.error_text()!r}"

    # The two records that arrived stay; nothing after the cancel landed.
    app.driver.navigate_to("feed")
    assert wait_until(
        lambda: app.driver.count("post-card") == 2 or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the cancelled import should leave its 2 records, "
        f"got {app.driver.count('post-card')} cards; error-message={app.error_text()!r}",
    )


def _import_to_done(wizard: ArchiveImportActions, zip_path: Path, recorded_audiences: int | None = 3) -> tuple[str, str]:
    """Walk ``zip_path`` through to Done; returns (done summary, folder name)."""
    _walk_to_confirm(wizard, zip_path, recorded_audiences)
    wizard.start()
    done = _wait_done(wizard)
    return done, wizard.folder_link_text().split(": ", 1)[1]


def _archive_folder_titles(app) -> list[str]:
    folders = BackupsActions(app.driver)
    folders.navigate_folders()
    titles = wait_until(
        lambda: [folders.folder_title(k) for k in range(folders.folder_count())] or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the Folders page listed nothing",
    )
    return [t for t in titles if "archive" in t.lower()]


@pytest.mark.feature("import-your-social-archive")
def test_importing_again_adds_only_what_is_new_into_a_folder_of_its_own(app, request, archive_nest, tmp_path):
    """§ Storage: re-imports are continuous through dedup — a newer export of
    the same platform lands in a NEW folder, and an entity already imported is
    skipped, so only what is new arrives; the same archive again adds nothing."""
    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-again"))
    tag = " " + _unique("#again")
    older = FakeFacebookArchive(tag=tag)
    newer_text = "A newer post"
    newer = FakeFacebookArchive(
        tag=tag, posts=[*older.posts, ArchivePostSpec(newer_text, PUBLIC_TS + 86_400 * 200, "Public")],
    )

    wizard = ArchiveImportActions(app.driver)
    done, first_folder = _import_to_done(wizard, older.write(tmp_path / "facebook-1.zip"))
    assert done.startswith("4 imported"), done
    wizard.back()  # Done → a fresh wizard

    done, second_folder = _import_to_done(wizard, newer.write(tmp_path / "facebook-2.zip"), recorded_audiences=4)
    assert done.startswith("1 imported"), f"only the newer post is new: {done!r}"
    wizard.review_skipped()
    assert wait_until(
        lambda: "already imported" in wizard.error_log(),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the skip log does not say why: {wizard.error_log()!r}",
    )
    assert second_folder != first_folder, (first_folder, second_folder)
    wizard.back()

    # The same archive once more: nothing new at all.
    done, third_folder = _import_to_done(wizard, newer.write(tmp_path / "facebook-3.zip"), recorded_audiences=4)
    assert done.startswith("0 imported"), done

    feed = app.feed
    app.driver.navigate_to("feed")
    assert feed.wait_for_post_text(newer_text + tag), f"error-message={app.error_text()!r}"
    assert wait_until(
        lambda: app.driver.count("post-card") == IMPORTED_CARDS + 1 or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"three imports should leave {IMPORTED_CARDS + 1} cards, "
        f"got {app.driver.count('post-card')}; error-message={app.error_text()!r}",
    )
    assert sum(PUBLIC_TEXT in feed.post_text(k) for k in range(feed.post_count())) == 1

    # One folder per import: a newer export never overwrote an older one.
    titles = _archive_folder_titles(app)
    for name in (first_folder, second_folder, third_folder):
        assert sum(name == t or t.startswith(name + " ") for t in titles) >= 1, (name, titles)
    assert len(titles) == 3, titles


@pytest.mark.feature("import-your-social-archive")
def test_an_imported_post_can_be_liked_replied_to_and_reposted_by_the_owner_and_others(
    app, request, archive_nest, tmp_path,
):
    """§ The post origin field: an imported post is native (`SourceKind::
    is_native`), so its like, reply and repost are signed Fauna posts, on the
    owner's app and on a follower's alike."""
    owner = _fresh_actor(archive_nest)
    follower = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-interact"))
    zip_path = FakeFacebookArchive(tag=" " + _unique("#interact")).write(tmp_path / "facebook.zip")
    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, zip_path)
    wizard.start()
    assert _wait_done(wizard).startswith("4 imported")

    sub = ApiActor(archive_nest["url"], follower["token"], follower["actor_id_hex"], bytes(follower["signing_key"]))
    reply = sub.subscribe(owner["actor_id_bytes"], FOLLOWERS_TIER)
    if reply.get("outcome") == "queued":
        subs = app.subscriptions
        subs.navigate()
        subs.open_tiers_tab()
        subs.refresh()
        assert subs.wait_for_pending_request(1), subs.error_text()
        subs.approve_first_request()
        assert subs.wait_for_subscriber(1), subs.error_text()
    else:
        assert reply.get("outcome") in ("approved", "active"), reply

    feed = app.feed
    app.driver.navigate_to("feed")
    row = feed.wait_for_post_state_by_text(PUBLIC_TEXT)
    assert row is not None and row.get("post_id"), f"error-message={app.error_text()!r}"
    # Addressed by id from here on: once the owner reposts it, the repost row
    # folds the original's text too, and a text read can land on either.
    post_id = row["post_id"]
    for n, (who, handle) in enumerate(((owner, None), (follower, _unique("e2e-archive-fan"))), start=1):
        if handle:
            _login(app, archive_nest, who, handle=handle)
            assert wait_until(
                lambda: feed.post_state_by_id(post_id),
                budgets.RPC_ROUNDTRIP_S,
                diagnose=lambda: f"the follower's feed never listed the imported post; "
                f"error-message={app.error_text()!r}",
            )
        feed.like_post(feed.post_index_by_id(post_id))
        assert feed.wait_for_interaction_count_by_id(post_id, "like", n) == n, (
            f"like #{n} on the imported post did not land; error={app.error_text()!r}"
        )
        feed.reply_post(_unique(f"reply-{n}"), index=feed.post_index_by_id(post_id))
        assert feed.wait_for_interaction_count_by_id(post_id, "reply", n) == n, (
            f"reply #{n} on the imported post did not land; error={app.error_text()!r}"
        )
        feed.return_to_feed_page()
        feed.repost_post(feed.post_index_by_id(post_id))
        # The tap does not re-read an imported post's counts — the nest's
        # interact door refuses every non-`fauna` native token
        # (`FeedManager::repost`, ruling 3) — so the count is read after the
        # next natural reload: leave the feed and come back.
        ArchiveImportActions(app.driver).navigate()
        feed.return_to_feed_page()
        assert feed.wait_for_interaction_count_by_id(post_id, "repost", n) == n, (
            f"repost #{n} of the imported post did not land; error={app.error_text()!r}"
        )


# The fixture's one event (`FakeFacebookArchive._events`): joined, at
# 1_630_000_000 = 2021-08-26T17:46:40Z, at "Fixture Park".
EVENT_TITLE = "Fixture Picnic"
EVENT_PLACE = "Fixture Park"
EVENT_DAY = "2021-08-26"
# Title → the owner's own reply as their `attendee-status` reads it (the
# English `events.rsvp.*` labels): the joined picnic plus `reply_events`'
# declined / interested / hosted ones. Hosted reads Going — the owner hosts
# and attends; what sets it apart is that only it names the owner organizer.
OWN_REPLIES = {
    EVENT_TITLE: "Going",
    REPLY_EVENTS["declined"][0]: "Declined",
    REPLY_EVENTS["interested"][0]: "Interested",
    REPLY_EVENTS["hosted"][0]: "Going",
}


@pytest.mark.feature("import-your-social-archive")
def test_imported_events_land_in_the_calendar_at_their_times_visible_to_the_owner_alone(
    app, request, archive_nest, tmp_path,
):
    """§ What each category becomes, the events row: own events become calendar
    events with title, time, place and the owner's own RSVP, owner only. The
    calendar needs the owner's mail key and a calendar to write into — the
    precondition the `calendar_backend` fixture names, made here through the
    same app pages."""
    owner = _fresh_actor(archive_nest)
    other = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-events"))
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    app.events.navigate()
    app.events.create_calendar(_unique("archive-cal"))

    zip_path = FakeFacebookArchive(tag=" " + _unique("#events"), reply_events=True).write(
        tmp_path / "facebook.zip",
    )
    wizard = ArchiveImportActions(app.driver)
    done, _folder = _import_to_done(wizard, zip_path)
    assert done.startswith(f"{IMPORTED_CARDS + len(OWN_REPLIES)} imported"), (
        f"the events land with the posts: {done!r}"
    )

    app.events.navigate()
    listed = app.events.wait_for_listed(list(OWN_REPLIES))
    assert set(OWN_REPLIES) <= set(listed), f"an imported event is not in the calendar: {listed}"
    app.events.open_detail_by_summary(EVENT_TITLE)
    assert app.events.detail_time_text().startswith(f"{EVENT_DAY}T"), app.events.detail_time_text()
    assert EVENT_PLACE in app.events.detail_location_text(), app.events.detail_location_text()

    # The owner's own reply: their one roster row, its status the answer the
    # archive recorded — a declined event must never read as one they go to.
    # Back to the list between events: an open detail stays open across a
    # card click, so each one is left the way the user leaves it.
    for title, reply in OWN_REPLIES.items():
        app.events.back_from_detail()
        app.events.wait_for_listed([title])
        app.events.open_detail_by_summary(title)
        statuses = app.events.attendee_statuses(title)
        assert statuses == [reply], f"{title!r}: own reply {statuses!r}, expected [{reply!r}]"

    # Owner only: another account of the same nest, calendar working, never
    # sees it.
    _login(app, archive_nest, other, handle=_unique("e2e-archive-neighbour"))
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    probe = _unique("neighbour-event")
    app.events.navigate()
    cal = _unique("neighbour-cal")
    app.events.create_calendar(cal)
    app.events.select_calendar(cal)
    app.events.create_event(probe, "2031-01-01T10:00", "2031-01-01T11:00")
    app.events.navigate()
    shown = app.events.wait_for_listed([probe], absent=[EVENT_TITLE])
    assert probe in shown and EVENT_TITLE not in shown, shown


MIB = 1024 * 1024
PADDING_MIB = 3
_UNIT_MIB = {"B": 1 / MIB, "KB": 1 / 1024, "MB": 1.0, "GB": 1024.0, "TB": 1024.0 * 1024}


def _storage_used_mib(app) -> float:
    """The Status page's `quota-storage` USED half, in MiB (the shared
    `fauna_core::format::byte_size` rendering: one decimal, binary units)."""
    app.settings._navigate_subpage("status")
    app.driver.wait_for("quota-section", timeout=budgets.RPC_ROUNDTRIP_S)
    text = app.settings.quota_storage()
    m = re.search(r"([\d.,]+)\s*(B|KB|MB|GB|TB)\b", text)
    assert m, f"quota-storage carries no size: {text!r}"
    return float(m.group(1).replace(",", "")) * _UNIT_MIB[m.group(2)]


@pytest.mark.feature("import-your-social-archive")
def test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts(
    app, request, archive_nest, tmp_path,
):
    """§ Storage: the archive folder is quota-metered and backed up like any
    folder, `raw/` holds the untouched export, and deleting the folder deletes
    the archive, never the posts it produced."""
    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-folder"))
    # Incompressible padding no parser reads: only raw/ carries it, so the
    # archive's own weight is what moves the storage allowance.
    zip_path = FakeFacebookArchive(
        tag=" " + _unique("#folder"), padding=os.urandom(PADDING_MIB * MIB),
    ).write(tmp_path / "facebook.zip")
    before = _storage_used_mib(app)
    assert before < PADDING_MIB, f"a fresh account starts near empty: {before} MiB"
    wizard = ArchiveImportActions(app.driver)
    done, folder_name = _import_to_done(wizard, zip_path)
    assert done.startswith("4 imported"), done

    # The folder counts toward the storage allowance like any folder.
    after = wait_until(
        lambda: (used := _storage_used_mib(app)) >= before + PADDING_MIB and used or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"storage used did not grow by the archive's {PADDING_MIB} MiB: "
        f"{before} MiB before, {_storage_used_mib(app)} MiB now",
    )
    assert after >= before + PADDING_MIB

    # Backed up like any folder: a snapshot of it, taken from the Backups page.
    backups = BackupsActions(app.driver)
    backups.navigate()
    backups.select_folder(folder_name)
    backups.create_snapshot()
    backups.open_snapshot(0)
    files = backups.wait_for_snapshot_files(min_count=2)
    # The snapshot holds the archive exactly as chosen: download every file
    # (the rows carry no per-file id) and read back the raw zip by its name.
    for k in range(files):
        backups.download_file(index=k)
    raw = backups.wait_for_downloaded_file(zip_path.name)
    assert raw == zip_path.read_bytes(), (
        f"raw/ must hold the untouched export: {len(raw)} bytes vs {zip_path.stat().st_size}"
    )

    # Deleting the folder removes the archive and leaves every post.
    backups.navigate_folders()
    backups.find_and_expand_folder(folder_name)
    backups.delete_folder()
    backups.confirm_folder_delete()
    backups.wait_for_folder_row_gone(folder_name)
    app.driver.navigate_to("feed")
    assert app.feed.wait_for_post_text(PUBLIC_TEXT), f"error-message={app.error_text()!r}"
    _wait_imported_cards(app)


@pytest.mark.feature("import-your-social-archive")
def test_the_archive_stays_on_the_nest_and_no_device_holds_a_copy(app, request, archive_nest, tmp_path):
    """§ Storage — the archive folder: the nest place holds the content and no
    device place is bound, so no device mirrors it.

    A zero is only a witness behind a positive barrier (convention 14). The
    importing device's own writes are the barrier: every change record the
    import writes carries this device's sync id (`rpc_glue.rs`, the import's
    `ChunkStore`), so the folder's device activity names this device — the
    device exists, wrote the whole archive, and still holds no place in it.
    On the expanded row, the device-activity list and the places list arrive
    in ONE fold (tui's `FolderRosterLoaded`, the expand-time read), so an
    activity row painted for this folder proves its places were read too."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.set_names import addressed

    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-nest-only"))
    zip_path = FakeFacebookArchive(tag=" " + _unique("#nestonly")).write(tmp_path / "facebook.zip")
    wizard = ArchiveImportActions(app.driver)
    done, folder_name = _import_to_done(wizard, zip_path)
    assert done.startswith("4 imported"), done

    # The nest's ground truth (black-box verification, convention 5): the
    # importing device wrote into the folder, and no device holds a place in it.
    with WsRpcAdminClient(
        archive_nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    ) as client:
        writers = client.call("fauna.folders.devices", addressed(folder_name))["devices"]
        seats = client.call("fauna.folders.members.list", addressed(folder_name))["members"]
    assert writers, f"the import's writes should name the device that made them: {writers!r}"
    assert seats == [], (
        f"the archive folder must bind no device place — nothing mirrors it down: {seats!r}"
    )

    # The row says the same, read only once its expand-time read has landed.
    folders = BackupsActions(app.driver)
    folders.navigate_folders()
    folders.find_and_expand_folder(folder_name)
    wait_until(
        lambda: folders.device_activity_item_count() >= 1 or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the expanded archive row never painted the importing device's activity "
            f"(the barrier); error {app.error_text()!r}"
        ),
    )
    assert app.driver.count("folder-place-row") == 0, (
        f"the archive folder's row lists a device place: "
        f"{app.driver.get_texts('folder-place-row')!r}"
    )


@pytest.mark.feature("import-your-social-archive")
def test_a_record_with_too_many_pictures_is_noted_in_the_skip_list_with_how_many_were_dropped(
    app, request, archive_nest, tmp_path,
):
    """§ Parser contract: a record naming more media than the parser takes
    (`MAX_RECORD_MEDIA_REFS`, 512) is truncated, never silently — the skip log
    says how many were dropped, and the post still lands."""
    owner = _fresh_actor(archive_nest)
    _login(app, archive_nest, owner, handle=_unique("e2e-archive-pictures"))
    tag = " " + _unique("#pictures")
    crowded = "A very crowded album post"
    archive = FakeFacebookArchive(tag=tag, posts=[
        ArchivePostSpec(crowded, PUBLIC_TS, "Public", photo=b"fixture-photo-1", photo_refs=514),
    ])
    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, archive.write(tmp_path / "facebook.zip"), recorded_audiences=None)
    wizard.start()
    _wait_done(wizard)
    wizard.review_skipped()
    assert wait_until(
        lambda: "imported the first 512 and skipped 2" in wizard.error_log(),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the skip log does not report the dropped pictures: {wizard.error_log()!r}",
    )
    app.driver.navigate_to("feed")
    assert app.feed.wait_for_post_text(crowded + tag), f"error-message={app.error_text()!r}"

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

import sys
import uuid
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from actions.archive_import import ArchiveImportActions
from actions.backups import BackupsActions
from common.auth import create_actor_and_register
from helpers import budgets
from helpers.e2e_session import login_as
from helpers.waiting import wait_until

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fakes"))
from fake_facebook_archive import (  # noqa: E402
    FRIENDS_TEXT,
    FRIENDS_TS,
    ONLY_ME_TEXT,
    PUBLIC_TEXT,
    PUBLIC_TS,
    FakeFacebookArchive,
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


def _walk_to_confirm(wizard: ArchiveImportActions, zip_path: Path) -> None:
    """Source → Archive → Scope → Confirm, as a user does it."""
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
    assert wizard.audience_summary().startswith("3 posts and albums have a recorded audience"), wizard.audience_summary()
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
    zip_path = FakeFacebookArchive(tag=" " + _unique("#walk")).write(tmp_path / "facebook.zip")

    wizard = ArchiveImportActions(app.driver)
    _walk_to_confirm(wizard, zip_path)
    wizard.start()
    done = _wait_done(wizard)
    # 3 posts + 1 album; events skip unless the nest's calendar is enabled.
    assert done.startswith("4 imported"), done
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

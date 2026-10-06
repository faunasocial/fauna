import time
import uuid

import pytest

from helpers.app_surface import app_name, skip_if_no_local_search_arm, skip_unbuilt
from helpers.search_journeys import LOCAL_INDEX_S, reset_search_page
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# tui led (2026-08-10); linux/android/web landed 2026-08-14/15; macOS/iOS joined 2026-08-25. windows closes the set — last of 7 apps.
#
# ⚠ This tuple answers "does this app ACT on a search result's navigation
# target?" — nothing more. Two of the three legs below additionally need a
# *result of that class to exist*, which is a different question with a
# different answer on web, on the phone seats, and on windows (no local search
# arm registered yet — see `skip_if_no_local_search_arm`).
#
# The former `_SEARCH_NAV_POST_APPS` split is RETIRED (2026-08-25). It existed
# because apple's Contact/File legs timed out at zero results while Post was
# green, which read as a local-search-index content-coverage gap. The real
# cause was neither the index nor the routing: apple's e2e login builds no
# `ConversationsSession` — and therefore registers no index arm and no builder
# — unless a selected test carries `real_conversations` (see the two marked
# tests below). Post was green throughout because posts come from the nest arm
# (`content_fts`), never the local index.
_SEARCH_NAV_APPS = ("tui", "linux", "android", "web", "macos", "ios", "windows")


def test_navigate_to_search(logged_in_app):
    """Navigate to search page and verify the query field is visible."""
    logged_in_app.search.navigate()
    assert logged_in_app.driver.is_visible("search-query-field"), (
        f"search query field should be visible: {logged_in_app.driver.diagnose('search-query-field')}"
    )
    assert logged_in_app.driver.is_visible("search-submit-button"), (
        f"search submit button should be visible: {logged_in_app.driver.diagnose('search-submit-button')}"
    )


@pytest.mark.feature("search")
def test_search_and_clear(logged_in_app):
    """Execute a search query and clear it."""
    logged_in_app.search.navigate()
    logged_in_app.search.query("test query")
    # After searching, either results or no-results should appear
    assert (logged_in_app.search.has_results()
            or logged_in_app.search.has_no_results()), (
        "after a query, either results or the no-results state should render: "
        f"results-view={logged_in_app.driver.diagnose('search-results-view')} "
        f"no-results={logged_in_app.driver.diagnose('search-no-results')} "
        f"error={logged_in_app.error_text()!r}"
    )
    logged_in_app.search.clear()
    assert logged_in_app.driver.is_visible("search-query-field"), (
        f"query field should remain after clear: {logged_in_app.driver.diagnose('search-query-field')}"
    )


@pytest.mark.feature("search")
def test_search_no_results(logged_in_app):
    """Search for nonsense and verify no-results state."""
    logged_in_app.search.navigate()
    logged_in_app.search.query("zzz_nonexistent_query_xyz_12345")
    assert logged_in_app.search.has_no_results(), (
        "a nonsense query should render the no-results state: "
        f"no-results={logged_in_app.driver.diagnose('search-no-results')} "
        f"results-view={logged_in_app.driver.diagnose('search-results-view')}"
    )


@pytest.mark.feature("search")
def test_search_load_more_hidden_when_under_limit(logged_in_app):
    """The load-more button should stay hidden when the result count is below
    the page limit — a partial page means the server has no more rows."""
    logged_in_app.search.navigate()
    # Query for something that returns 0 results (below the 50-row limit).
    logged_in_app.search.query("zzz_loadmore_nothing_xyz_87654")
    assert logged_in_app.driver.is_absent("search-load-more-button"), (
        "load-more should stay hidden for a below-limit (0-result) query: "
        f"{logged_in_app.driver.diagnose('search-load-more-button')}"
    )


@pytest.mark.feature("search")
def test_search_load_more_button_appears_at_limit(logged_in_app, nest_instance, test_user):
    """When search returns ≥ the limit (50), the load-more button becomes
    visible; clicking it bumps the limit and re-fires the search with more
    results (API has no cursor, so the client paginates by growing limit)."""
    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    signing_key = test_user["signing_key"]
    port = nest_instance["port"]

    # Publish 55 posts with a unique needle so this test can't collide with
    # other sessions' seeded data. 55 > 50 (INITIAL_LIMIT) exercises the
    # button. Posts ride the fauna.posts.create WS-RPC kind (the
    # POST /api/v1/posts HTTP twin was deleted by the WS-RPC migration).
    needle = f"loadmore{uuid.uuid4().hex[:10]}"
    now_us = int(time.time() * 1_000_000)
    for i in range(55):
        body = f"{needle} entry {i}"
        post_bytes = sign_and_encode_post(
            signing_key, now_us + i, body, tags=[],
        )
        ws_api.create_post(port, test_user, post_bytes)

    logged_in_app.search.navigate()
    logged_in_app.search.query(needle)
    time.sleep(1)  # search fire-and-forget; wait for results to populate.

    # Sanity: verify the posts are actually searchable via the API before
    # asserting on UI state. Uses the fauna.search.query WS-RPC kind (the
    # GET /api/v1/search HTTP twin was deleted in the WS-RPC search migration).
    api_hits = ws_api.search(port, test_user, needle, limit=100)
    assert len(api_hits) >= 55, (
        f"Nest search API returned {len(api_hits)} hits for {needle!r} — "
        "FTS indexing or publish may have failed"
    )

    before = logged_in_app.search.result_count()
    assert before >= 50, (
        f"First page should be full (≥50); got {before} UI rows. "
        f"API returned {len(api_hits)} — likely UI-render or driver issue."
    )
    # The first page hits the limit, so load-more should become visible.
    assert logged_in_app.driver.is_visible("search-load-more-button"), \
        "Load-more button should appear when result count reaches the page limit"

    logged_in_app.driver.click("search-load-more-button")
    # Give the client a moment to issue the bumped-limit request and render.
    time.sleep(2)

    after = logged_in_app.search.result_count()
    assert after > before, \
        f"Expected more results after load-more click; before={before} after={after}"


@pytest.mark.feature("search")
def test_activating_a_post_search_result_navigates_to_its_post_detail(
    logged_in_app, nest_instance, test_user
):
    """Row 18: activating a search result
    row must navigate to its typed `SearchNav` target — tui first (the
    lead-app rule; `ui/search.md` § User actions — "search-result-item[i] |
    Open destination"). A post row's `content_id` **is** the post id (the
    ratified wire contract, `ui/search.md` § The page's wire surface), so
    activation should land directly on that post's detail view.
    """
    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    if app_name(logged_in_app.driver) not in _SEARCH_NAV_APPS:
        skip_unbuilt(
            logged_in_app.driver,
            surface="search-result-item activation (SearchNav)",
            detail="no app but tui/linux/android/web/macos/ios acts on a result row's navigation target yet",
            tracked="",
        )

    signing_key = test_user["signing_key"]
    port = nest_instance["port"]

    needle = f"searchnav{uuid.uuid4().hex[:10]}"
    now_us = int(time.time() * 1_000_000)
    post_bytes = sign_and_encode_post(
        signing_key, now_us, f"{needle} landed the row", tags=[],
    )
    post_id = ws_api.create_post(port, test_user, post_bytes)

    logged_in_app.search.navigate()
    logged_in_app.search.query(needle)

    # Deadline-poll rather than a fixed sleep on top of query()'s own (convention
    # 14) — the result count is the latency-independent state that actually
    # matters, and content_fts indexing has no fixed settle time to guess at.
    found = wait_until(
        lambda: logged_in_app.search.result_count() >= 1,
        30.0,
        diagnose=lambda: (
            f"expected at least one result for {needle!r}: "
            f"{logged_in_app.driver.diagnose('search-result-item')}"
        ),
    )
    assert found

    logged_in_app.search.open_result(index=0)

    # Deadline-polled (convention 14), not a bare assert: the dialog is meant
    # to appear immediately (every app is expected to switch to it — even
    # showing a loading placeholder — before the deep-link resolve completes,
    # not gate the switch on the round trip), but "immediately" still crosses
    # the accessibility tree / GTK main-loop under load, so a same-tick assert
    # is a wall-clock bet this convention forbids. A generous ceiling a
    # healthy run never pays.
    dialog_appeared = wait_until(
        logged_in_app.feed.post_detail_visible,
        15.0,
        diagnose=lambda: (
            f"activating the search result (post_id={post_id}) should open the "
            f"post detail view: {logged_in_app.driver.diagnose('feed-post-detail-dialog')}"
        ),
    )
    assert dialog_appeared

    # ...and the dialog must show the post's REAL body, not an empty shell.
    # This post was created for this test and never scrolled to, so it is not
    # in the loaded feed snapshot: `feed::Action::OpenPostDetail` was built for
    # a post-card click, where the post is always already loaded by
    # construction, and a search hit is the first caller that can name one the
    # feed never fetched. Until row 29 landed the deep-link fetch
    # (`FeedManager::resolve_post` → the snapshot's deep-link slot), this
    # assertion was the red: the dialog opened blank. Deadline-polled because
    # the fetch is one async round trip after the navigation lands.
    body_shows_the_post = wait_until(
        lambda: needle in (logged_in_app.feed.post_detail_body() or ""),
        30.0,
        diagnose=lambda: (
            f"expected the deep-linked post's body to contain {needle!r}: "
            f"{logged_in_app.driver.diagnose('feed-post-detail-body')}"
        ),
    )
    assert body_shows_the_post


# `real_conversations` is what gives an APPLE seat a local search arm at all
# under e2e. macOS/iOS build a `ConversationsSession`
# on the e2e `set_state` login ONLY under `FAUNA_E2E_REAL_CONVERSATIONS`
# (`FaunaMacApp.swift`'s `if FaunaE2E.realConversations`; the ordinary login path
# builds it unconditionally), and the index arm + builder are created inside that
# factory (`FfiNestClient::conversations_session` → `index_arm`,
# `set_index_builder_launcher`). Without it `attach_local_search_index` returns
# false forever and nothing publishes segments either — zero local rows for the
# whole run, which is exactly how this leg read as a content-coverage gap.
# Per-test and NOT module-level, following
# `test_conversations_attachments_outbound.py`: `_apply_real_conversations_env`
# is session-wide, so a module mark would tag every other test in this file too.
# ⚠ Run these two in their own pytest invocation — the flag reaches every native
# app collected in the same session and would flip mock-inject DM tests to real.
#
# The three private-index result legs below (Contact card, deleted card, File
# media detail) are MARKED for the columns a single-seat run can drive them on
# (feature-catalog.md § Cell semantics, the marked-witness rule, 2026-09-26):
# ios and android query a local arm but never BUILD one (`skip_if_seat_builds_no_index`
# — a phone renders segments a desktop seat published), so a single phone seat
# can never be handed a Contact/File hit; that is a limit of this test's
# shape, not a behaviour the phones lack, and their witness is a phone-shaped
# twin with a desktop seat, as `test_search_outcomes.py`'s
# `_a_desktop_seat_indexed` journey already does for outcome 6. iOS's twins
# close this module; android's pairing awaits a desktop seat on the emulator's
# host. web STAYS in the set on purpose: it
# has no local arm by design, so the helper's `declared_absence` there is the
# real thing — the run's confirmation of the page's own absence
# (`private-search-index` declares it page-level; `search` outcomes 3 and 4
# await the user's approval of the per-outcome form).
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.real_conversations
@pytest.mark.feature("search", "private-search-index")
def test_activating_a_contact_search_result_navigates_to_its_card(
    app, dedicated_mail_nest, request
):
    """Row 31: activating a search result row
    for a `Contact` hit must navigate to that contact's card through the real
    app UI. Row 18 built the leg (`SearchNav::Contact` →
    `contacts::Action::OpenCardByUid` → the shared
    `CardDavClient::locate_card_by_uid_hash` resolve, joining the index's
    `uid_hash` identity to the destination's `card_id`) and covered it with
    unit + scripted-requester tests, but — same as the Post leg before row 29
    — nothing had driven it through a live app session. Treat
    `test_activating_a_post_search_result_navigates_to_its_post_detail` (which
    found the real gap) as the precedent this leg could still be hiding
    one behind.
    """
    from tests.test_addressbook import _seed_mda_sealed_card

    if app_name(app.driver) not in _SEARCH_NAV_APPS:
        skip_unbuilt(
            app.driver,
            surface="search-result-item activation (SearchNav::Contact)",
            detail="no app but tui/linux/android/web/macos/ios acts on a Contact search result's navigation target yet",
            tracked="",
        )
    skip_if_no_local_search_arm(app.driver, content_class="Contact")

    handle = dedicated_mail_nest
    fn, tel, email, _client, _book, _uid = _seed_mda_sealed_card(app, handle, request)
    needle = fn.rsplit(" ", 1)[-1]  # the unique timestamp suffix _seed_mda_sealed_card mints

    app.search.navigate()

    # The contacts corpus stages on the arm's attach walk, so the poll window
    # must clear the same periodic-flush backstop
    # test_search_local_index.py documents (60s), with margin — mirrors that
    # test's query_and_check shape: a local-index hit needs a re-query per
    # tick, not just a wait on a result that already landed.
    def query_and_check():
        app.search.query(needle)
        return app.search.result_count() >= 1

    found = wait_until(
        query_and_check,
        90.0,
        diagnose=lambda: (
            f"expected at least one Contact result for {needle!r}: "
            f"{app.driver.diagnose('search-result-item')}"
        ),
    )
    assert found

    app.search.open_result(index=0)

    # OpenCardByUid resolves the row's uid_hash to a card_id over the wire
    # (`locate_card_by_uid_hash`), so the detail pane fills in a beat after
    # navigation — poll rather than read once, same reasoning as the Post
    # leg's deep-link body assertion above.
    def detail_fn_or_empty():
        return app.contacts.detail_fn() if app.driver.is_visible("vcard-detail-fn") else ""

    # The deep link crosses three surfaces before the card can paint — the
    # destination page, the Address Book segment, then the located card — and
    # each one failing looks identical at `vcard-detail-fn` alone (absent, no
    # error). Report all three so a red says WHICH link broke instead of only
    # that the chain did (convention 6).
    opened = wait_until(
        lambda: fn in detail_fn_or_empty(),
        30.0,
        diagnose=lambda: (
            f"activating the Contact search result (fn={fn!r}) should open its "
            f"card: {app.driver.diagnose('vcard-detail-fn')} "
            f"error={app.error_text()!r} "
            f"| destination page: {app.driver.diagnose('contacts-view')} "
            f"| address-book segment: {app.driver.diagnose('addressbook-item')} "
            f"| card rows: {app.driver.diagnose('vcard-card')}"
        ),
    )
    assert opened


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.real_conversations
@pytest.mark.feature("search")
def test_activating_a_contact_search_result_for_a_deleted_card_surfaces_error(
    app, dedicated_mail_nest, request
):
    """Row 388: the DROPPED case (`ui/search.md` § Where
    logic lives → Result navigation (deep link), the Contact bullet) — a card
    deleted between being indexed and being clicked. tui and apple already
    surface `error-message` rather than a silently-repainted picker; this
    pins the same for linux/android/web.

    The deep-link resolve happens SERVER-side at activation time
    (`locate_card_by_uid_hash`), so deleting the card via the scripted MUA
    right after it is indexed — before activating the still-displayed search
    result row — deterministically reproduces the DROPPED outcome: no local
    re-query races an index refresh, because none is needed. The click
    itself is what asks the nest, fresh, and the nest has nothing to answer.
    """
    from tests.test_addressbook import _seed_mda_sealed_card

    if app_name(app.driver) not in _SEARCH_NAV_APPS:
        skip_unbuilt(
            app.driver,
            surface="search-result-item activation (SearchNav::Contact)",
            detail="no app but tui/linux/android/web/macos/ios acts on a Contact search result's navigation target yet",
            tracked="",
        )
    skip_if_no_local_search_arm(app.driver, content_class="Contact")

    handle = dedicated_mail_nest
    fn, tel, email, client, book, uid = _seed_mda_sealed_card(app, handle, request)
    needle = fn.rsplit(" ", 1)[-1]  # the unique timestamp suffix _seed_mda_sealed_card mints

    app.search.navigate()

    def query_and_check():
        app.search.query(needle)
        return app.search.result_count() >= 1

    found = wait_until(
        query_and_check,
        90.0,
        diagnose=lambda: (
            f"expected at least one Contact result for {needle!r}: "
            f"{app.driver.diagnose('search-result-item')}"
        ),
    )
    assert found

    # Delete the card server-side WITHOUT re-querying search — the displayed
    # row still points at `uid`, and the resolve behind activating it is
    # fresh (never index-served), so this is deterministic latency-independent
    # state, never a race against index-refresh timing.
    client.delete_card(book, uid)

    app.search.open_result(index=0)

    # A short, i18n-generation-immune fragment of the ratified `card_not_found`
    # copy ("That contact is no longer in your address books.") — the exact
    # literal would break the moment the copy is edited for tone, which is
    # not this test's job to pin.
    not_found_fragment = "no longer in your address books"

    def error_or_empty():
        return app.error_text() if app.has_error() else ""

    surfaced = wait_until(
        lambda: not_found_fragment in error_or_empty(),
        30.0,
        diagnose=lambda: (
            f"activating the Contact search result for a since-deleted card "
            f"should surface error-message, not a silent repaint: "
            f"error={app.error_text()!r} "
            f"| destination page: {app.driver.diagnose('contacts-view')} "
            f"| address-book segment: {app.driver.diagnose('addressbook-item')}"
        ),
    )
    assert surfaced

    # The book picker still repaints — the DROPPED case must not regress that
    # half (the own success criterion).
    assert app.driver.is_visible("addressbook-item"), (
        "the address-book picker rows must still land even though the "
        "specific card did not"
    )


# Same apple local-arm gate as the Contact leg above — see that comment.
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.web
@pytest.mark.real_conversations
@pytest.mark.feature("search", "private-search-index")
def test_activating_a_file_search_result_navigates_to_its_media_detail(
    seeded_media_app, tmp_path
):
    """Row 31: activating a search result row
    for a `File` hit must navigate to that file's media detail through the
    real app UI — and must do so ACROSS the page's active folder filter, the
    stronger assertion that would catch a regression from the raw aggregate to
    the rendered/filtered view (the `MediaMachine::locate_file` reads
    `s.raw`, never `snapshot().items`; mutation-verified at the Rust layer by
    `a_file_deep_link_opens_its_detail_across_the_active_filter`, but nothing
    had driven the same property through a live app session before this).
    """
    from tests.test_media import FIXTURE_IMAGE

    app, plan = seeded_media_app

    if app_name(app.driver) not in _SEARCH_NAV_APPS:
        skip_unbuilt(
            app.driver,
            surface="search-result-item activation (SearchNav::File)",
            detail="no app but tui/linux/android/web/macos/ios acts on a File search result's navigation target yet",
            tracked="",
        )
    # Red on web for the identical structural reason as the Contact leg above —
    # found by the same read, before anyone spent a run discovering it.
    skip_if_no_local_search_arm(app.driver, content_class="File")

    # Upload into the fixture's single-folder — a clean pre/post count, same
    # target-set selection as test_media_upload_into_selected_set.
    target_set, seeded_paths = next(
        ((name, paths) for name, paths in plan.items() if len(paths) == 1),
        (None, None),
    )
    assert target_set is not None, f"fixture should seed a single-folder; plan={plan!r}"
    other_set = next(name for name in plan if name != target_set)

    app.media.navigate()
    app.media.set_filter(target_set)
    pre = app.media.wait_for_item_count(len(seeded_paths))
    assert pre == len(seeded_paths), (
        f"the seeded set {target_set!r} should show its {len(seeded_paths)} item "
        f"before upload, got {pre}; error={app.error_text()!r}"
    )

    needle = f"searchnavfile{uuid.uuid4().hex[:10]}"
    picked_name = f"{needle}.png"
    picked = tmp_path / picked_name
    picked.write_bytes(FIXTURE_IMAGE.read_bytes())
    app.media.upload_file(str(picked))

    after = app.media.wait_for_item_count(len(seeded_paths) + 1)
    assert after == len(seeded_paths) + 1, (
        f"upload should add one item to {target_set!r} ({len(seeded_paths)} -> "
        f"{len(seeded_paths) + 1}), got {after}; error={app.error_text()!r}"
    )

    # Switch the active filter to the OTHER set before searching — the
    # uploaded file must resolve despite not being the currently-filtered set.
    app.media.set_filter(other_set)
    app.media.wait_for_item_count(len(plan[other_set]))

    app.search.navigate()

    def query_and_check():
        app.search.query(needle)
        return app.search.result_count() >= 1

    found = wait_until(
        query_and_check,
        90.0,
        diagnose=lambda: (
            f"expected at least one File result for {needle!r}: "
            f"{app.driver.diagnose('search-result-item')}"
        ),
    )
    assert found

    app.search.open_result(index=0)

    def detail_name_or_empty():
        return app.media.detail_name() if app.driver.is_visible("media-item-detail") else ""

    opened = wait_until(
        lambda: detail_name_or_empty() == picked_name,
        15.0,
        diagnose=lambda: (
            f"activating the File search result (name={picked_name!r}) should open "
            f"its media detail across the active filter ({other_set!r}): "
            f"{app.driver.diagnose('media-item-detail')} error={app.error_text()!r}"
        ),
    )
    assert opened


# ─── The phone twins: a desktop seat indexes, the phone opens the result ───
#
# The three private-index legs above in the phone's real shape (`search`
# outcomes 3 and 4 on ios; feature-catalog.md § Implementation status today, the
# 2026-09-26 paragraph). A phone builds no index of its own
# (`content-index.md` § Build vs. query), so the Contact / File the phone seeded
# is indexed by a DESKTOP seat of the SAME account (`alice_builder_seats`,
# launched as the actor the phone is running — a dedicated nest's admin for the
# card, the dedicated seeded actor for the file); the desktop walks the address
# book / `fauna.media.list` at its own launch and publishes, and the phone reads
# the synced replica. The desktop launches only AFTER the seed, so its first walk
# already holds it — no delay, no second launch. Everything after the phone's
# first hit is the leg's own journey, asserted on the phone.


def _a_desktop_seat_indexes(seats, needle, *, what, user, nest):
    """Launch a desktop seat of `user` on `nest` and wait until ITS search finds
    `needle` — the seat has walked the corpus and staged the hit, so the segment
    it publishes carries it. Returns the seat's app, for a failure message."""
    desktop, _ = seats.launch(user=user, nest=nest)
    desktop.search.navigate()

    def desktop_finds():
        desktop.search.query(needle)
        return desktop.search.result_count() >= 1

    assert wait_until(
        desktop_finds,
        LOCAL_INDEX_S,
        diagnose=lambda: (
            f"the desktop seat never indexed the phone's {what} {needle!r}: "
            f"{desktop.driver.diagnose('search-result-item')} "
            f"error={desktop.error_text()!r}"
        ),
    )
    return desktop


def _the_phone_finds(phone, needle, *, what):
    """Poll the phone's search until the synced index hands it `needle` — a
    re-query per tick, as the legs above do."""
    phone.search.navigate()

    def phone_finds():
        phone.search.query(needle)
        return phone.search.result_count() >= 1

    assert wait_until(
        phone_finds,
        LOCAL_INDEX_S,
        diagnose=lambda: (
            f"the phone never found the {what} {needle!r} in the index the "
            f"desktop published: {phone.driver.diagnose('search-result-item')} "
            f"error={phone.error_text()!r}"
        ),
    )


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("search", "private-search-index")
def test_a_phone_opens_a_contact_result_a_desktop_seat_indexed(
    app, dedicated_mail_nest, request, alice_builder_seats
):
    """`search` outcome 3 (Contact) on a phone — the Contact leg above, with the
    card indexed by a desktop seat of the dedicated nest's admin, the identity
    the phone is running. Activating the phone's row must open the card on the
    phone (`SearchNav::Contact` → `locate_card_by_uid_hash`)."""
    from tests.test_addressbook import _seed_mda_sealed_card

    phone = app
    handle = dedicated_mail_nest
    fn, _tel, _email, _client, _book, _uid = _seed_mda_sealed_card(
        phone, handle, request
    )
    needle = fn.rsplit(" ", 1)[-1]
    try:
        _a_desktop_seat_indexes(
            alice_builder_seats, needle, what="card",
            user=handle.nest["admin"], nest=handle.nest,
        )
        _the_phone_finds(phone, needle, what="card")
        phone.search.open_result(index=0)

        def detail_fn_or_empty():
            return (
                phone.contacts.detail_fn()
                if phone.driver.is_visible("vcard-detail-fn") else ""
            )

        assert wait_until(
            lambda: fn in detail_fn_or_empty(),
            30.0,
            diagnose=lambda: (
                f"activating the phone's Contact result (fn={fn!r}) should open "
                f"its card: {phone.driver.diagnose('vcard-detail-fn')} "
                f"error={phone.error_text()!r} "
                f"| destination page: {phone.driver.diagnose('contacts-view')} "
                f"| address-book segment: {phone.driver.diagnose('addressbook-item')} "
                f"| card rows: {phone.driver.diagnose('vcard-card')}"
            ),
        )
    finally:
        reset_search_page(phone)


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("search")
def test_a_phone_result_for_a_card_deleted_since_a_desktop_seat_indexed_it_surfaces_error(
    app, dedicated_mail_nest, request, alice_builder_seats
):
    """`search` outcome 4 on a phone — the DROPPED leg above: the card a desktop
    seat indexed is deleted server-side after the phone's row shows, so the
    fresh resolve behind activating that row has nothing to answer and the
    phone must surface `error-message`, not a silent repaint. Witnesses outcome
    3's Contact navigation too: the same activation, the other branch of the
    resolve."""
    from tests.test_addressbook import _seed_mda_sealed_card

    phone = app
    handle = dedicated_mail_nest
    fn, _tel, _email, client, book, uid = _seed_mda_sealed_card(
        phone, handle, request
    )
    needle = fn.rsplit(" ", 1)[-1]
    try:
        _a_desktop_seat_indexes(
            alice_builder_seats, needle, what="card",
            user=handle.nest["admin"], nest=handle.nest,
        )
        _the_phone_finds(phone, needle, what="card")

        # Delete WITHOUT re-querying — the phone's displayed row still points at
        # `uid`, and the resolve behind activating it is fresh, never
        # index-served (the leg above's reasoning).
        client.delete_card(book, uid)
        phone.search.open_result(index=0)

        not_found_fragment = "no longer in your address books"

        def error_or_empty():
            return phone.error_text() if phone.has_error() else ""

        assert wait_until(
            lambda: not_found_fragment in error_or_empty(),
            30.0,
            diagnose=lambda: (
                f"activating the phone's Contact result for a since-deleted card "
                f"should surface error-message, not a silent repaint: "
                f"error={phone.error_text()!r} "
                f"| destination page: {phone.driver.diagnose('contacts-view')} "
                f"| address-book segment: {phone.driver.diagnose('addressbook-item')}"
            ),
        )
        assert phone.driver.is_visible("addressbook-item"), (
            "the address-book picker rows must still land even though the "
            "specific card did not"
        )
    finally:
        reset_search_page(phone)


@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.feature("search", "private-search-index")
def test_a_phone_opens_a_file_result_a_desktop_seat_indexed(
    seeded_media_app_and_user, nest_instance, alice_builder_seats, tmp_path
):
    """`search` outcome 3 (File) on a phone — the File leg above, including its
    stronger half: the phone's media detail opens ACROSS the page's active
    folder filter. The phone uploads the file; a desktop seat of the same
    dedicated actor walks `fauna.media.list` at its launch and indexes it."""
    from tests.test_media import FIXTURE_IMAGE

    phone, plan, user = seeded_media_app_and_user

    target_set, seeded_paths = next(
        ((name, paths) for name, paths in plan.items() if len(paths) == 1),
        (None, None),
    )
    assert target_set is not None, f"fixture should seed a single-folder; plan={plan!r}"
    other_set = next(name for name in plan if name != target_set)

    phone.media.navigate()
    phone.media.set_filter(target_set)
    pre = phone.media.wait_for_item_count(len(seeded_paths))
    assert pre == len(seeded_paths), (
        f"the seeded set {target_set!r} should show its {len(seeded_paths)} item "
        f"before upload, got {pre}; error={phone.error_text()!r}"
    )

    needle = f"phonesearchfile{uuid.uuid4().hex[:10]}"
    picked_name = f"{needle}.png"
    picked = tmp_path / picked_name
    picked.write_bytes(FIXTURE_IMAGE.read_bytes())
    phone.media.upload_file(str(picked))
    after = phone.media.wait_for_item_count(len(seeded_paths) + 1)
    assert after == len(seeded_paths) + 1, (
        f"upload should add one item to {target_set!r}, got {after}; "
        f"error={phone.error_text()!r}"
    )

    phone.media.set_filter(other_set)
    phone.media.wait_for_item_count(len(plan[other_set]))

    try:
        _a_desktop_seat_indexes(
            alice_builder_seats, needle, what="file",
            user=user, nest=nest_instance,
        )
        _the_phone_finds(phone, needle, what="file")
        phone.search.open_result(index=0)

        def detail_name_or_empty():
            return (
                phone.media.detail_name()
                if phone.driver.is_visible("media-item-detail") else ""
            )

        assert wait_until(
            lambda: detail_name_or_empty() == picked_name,
            15.0,
            diagnose=lambda: (
                f"activating the phone's File result (name={picked_name!r}) "
                f"should open its media detail across the active filter "
                f"({other_set!r}): {phone.driver.diagnose('media-item-detail')} "
                f"error={phone.error_text()!r}"
            ),
        )
    finally:
        reset_search_page(phone)

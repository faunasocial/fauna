import time
import urllib.error
import urllib.request
import uuid
import pytest
from pathlib import Path

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers import budgets
from helpers.blob_upload import upload_blob
from helpers.waiting import await_feed_reload_after, feed_reload_baseline, wait_until
from helpers.png_chunks import EXIF_GPS_CANARY, PNG_MAGIC
from helpers.png_chunks import png_chunks as _png_chunks
from helpers.png_chunks import png_with_chunk as _png_with_chunk
from helpers.png_chunks import png_with_exif_chunk as _png_with_exif_chunk
from tests.api import ws_api
from tests.api.bare import media_item, sign_and_encode_post

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


def _unique(prefix: str) -> str:
    """Generate a unique post text to avoid test interference."""
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.feature("feed-compose")
def test_create_post(logged_in_app):
    """Create a post and verify it appears in the feed."""
    text = _unique("create-post")
    logged_in_app.feed.create_post(text=text)
    assert logged_in_app.feed.first_post_text() == text, (
        f"created post {text!r} should be first in feed; got "
        f"{logged_in_app.feed.first_post_text()!r} error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("feed-interactions")
def test_feed_quote_button_present(logged_in_app):
    """Every post-card's interaction bar carries the universal `feed-quote-button`
    (icon + quote count, hidden at 0) — ratified 2026-06-27 (feed.md § Interaction
    bar): quote is universal on all seven apps, firing `fauna.posts.interact`
    action=quote (a repost with commentary). This is the cross-app gate for the
    Phase-2 interaction-count fan-out; it goes green per client as each app's leg
    lands (web reference + windows green; linux/android/apple legs in flight)."""
    text = _unique("quote-btn")
    logged_in_app.feed.create_post(text=text)
    assert logged_in_app.feed.quote_button_count() >= 1, (
        f"post-card should carry a feed-quote-button; got "
        f"{logged_in_app.feed.quote_button_count()} buttons for "
        f"post_count={logged_in_app.feed.post_count()} error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("feed-compose")
def test_compose_attach_affordance_offered(logged_in_app):
    """The composer offers a usable attach affordance (`compose-file`).

    feed.md § User actions: "Click `compose-file` -> Open file picker. Client
    glue (pickers are platform-native); staged-file metadata + validation are
    shared." Every app must therefore *offer* the affordance, whatever panel
    it opens.

    Guards the regression closed 2026-07-20: iOS shipped this button
    `.disabled(true)` behind an empty closure, and macOS's `NSOpenPanel` had a
    `// future: handle attachment` OK-branch that silently dropped the chosen
    file — so no real user could attach an image on either apple app, while
    every sibling client could. Both were invisible to the suite because the
    e2e attach path is the `compose.file` state-injection command, which
    bypasses the button entirely; the injected path stayed green throughout.
    That is the gap this reads directly.
    """
    state = logged_in_app.feed.attach_affordance_state()
    assert state["visible"] and state["enabled"], (
        f"composer must offer a usable `compose-file` attach affordance; got "
        f"{state} error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("feed-read")
def test_post_body_renders_markdown(logged_in_app):
    """The feed body is painted through the shared `RenderDocument` walker (D6 —
    render-model.md § D6: `PostSummary.document`, the SAME semantic model the
    Conversations page renders), not as flat text. A `**bold**` body therefore
    renders the word `bold` with the `**` markers consumed by the renderer — a flat
    string render would leave the literal asterisks in `feed-post-text`. Proves the
    feed body walks the document on every app that adopted it (linux + web here;
    windows/apple legs run on their machines)."""
    suffix = uuid.uuid4().hex[:8]
    logged_in_app.feed.create_post(text=f"**bold{suffix}**")
    # Deadline-poll rather than reading index 0 once (convention 14): the new post
    # reaching the top of the re-queried feed and its body being decoded for render
    # are both asynchronous and neither is ordered with respect to `create_post`.
    # Reading straight after it passed only by winning that race, and failed by
    # reading the PREVIOUS post — a valid body, so the failure read as a render bug.
    assert logged_in_app.feed.wait_for_first_post_text(f"bold{suffix}"), (
        f"the composed post never became the first rendered post: "
        f"feed-post-text {logged_in_app.feed.first_post_text()!r} "
        f"error={logged_in_app.error_text()!r}"
    )
    rendered = logged_in_app.feed.first_post_text()
    assert "**" not in rendered, (
        f"feed body rendered literal markdown — not walked as a document: {rendered!r}"
    )


@pytest.mark.web
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("feed-read")
def test_post_detail_opens(logged_in_app):
    """Clicking a post-card opens the post-detail dialog showing the author + the
    post body (ui.yaml feed transition `click post-card → post_detail`). Every
    app implements post_detail; web lifted it 2026-06-03 (it was the lone holdout);
    tui lifted it in M3 slice G close-out (`post-card` is a real `Gesture::Feed`
    button there, not just a scope marker, so the click drives the same one-door
    dispatch every other actuation does).

    web+tui+windows+linux+macos+ios today; android carries the same
    `feed-post-detail-*` testids but its `post-card` open-gesture clickability is
    unverified in its driver — promoting it (adding its mark) is a follow-on once
    confirmed there.

    macos+ios joined 2026-09-20: both already registered the whole-card open with
    the in-process automation registry — `.automationActivate(Ids.postCard,
    value: { post.body }) { openPostCard(post) }` in `MacFeedDetailView` and
    `FeedListView` — so the registry invokes the same `openPostCard` a real
    `.onTapGesture` runs, and the promotion was the run, not a code change. (iOS
    reaches detail through `selectedPost` + `.navigationDestination(item:)`
    rather than a pushed `NavigationLink` label, for the reason its own comment
    records: a tap-driven link label does not push when invoked via the
    registry.)

    linux joined 2026-09-19: `post-card` is a `gtk::ListBoxRow`, and the agent's
    click on a row emits `row-activated` on its `ListBox` — the same handler a
    real activation runs (`views/feed/post_list.rs`, `connect_row_activated`).

    windows joined 2026-07-19: `post-card`'s DataTemplate root is now a transparent
    `Button` (`FeedPage.xaml`, PostCard_Click) instead of the bare `Grid` that
    exposed no UIA invoke pattern (so the old FlaUI physical-click fallback landed
    but never fired the open — RED). Unlike the event-card
    precedent, the card's own interactive children (feed-post-actions-button,
    load-remote-content-button, the like/reply/repost/quote bar, muted-reveal) stay
    NESTED inside the Button — the feed e2e scopes several of them under
    `post-card[i]` (a scoped lookup is confined to the post-card element's subtree),
    and each keeps its own InvokePattern so FlaUI still drives it directly."""
    text = _unique("post-detail")
    logged_in_app.feed.create_post(text=text)
    logged_in_app.feed.open_post_detail(0)
    assert logged_in_app.feed.post_detail_visible(), (
        f"post-detail dialog did not open. error: {logged_in_app.error_text()!r}"
    )
    assert text in logged_in_app.feed.post_detail_body(), (
        f"detail body {logged_in_app.feed.post_detail_body()!r} missing post text {text!r}"
    )
    assert logged_in_app.feed.post_detail_author().strip() != "", "detail author empty"


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.feature("feed-interactions")
def test_interaction_bar_has_quote_and_all_buttons(logged_in_app):
    """Every post-card carries the four interaction buttons — like, reply,
    repost, quote — rendered as icon + count (no word labels), with
    `feed-quote-button` universal on all seven apps (feed.md § Interaction bar,
    ratified 2026-06-27). The counts are read from
    PostSummary.{like,reply,repost,quote}_count and hidden at 0; that
    deterministic count-render/hide-at-0 contract is pinned by the linux Rust
    unit tests `interaction_button_*` in post_list.rs (tui: `an_interaction_count_is_hidden_at_zero`
    in `feed/mod.rs`).

    Scoped to web + linux + tui + macos — the apps that have landed the quote
    button + counts. macOS added 2026-07-18: the four buttons were already built
    (`InteractionBar.swift`) but only carried a bare `.accessibilityIdentifier`
    (invisible to the in-process automation registry — the same class of gap
    this codebase hits repeatedly); now registered via `.automationActivate`.
    iOS shares the same `InteractionBar` component but is NOT yet promoted —
    unverified this session (no `--client ios` run). Promote iOS / windows /
    android as those legs land (NEXT-{apple,windows,android}-feed-interaction-counts)."""
    logged_in_app.feed.create_post(text=_unique("interaction-bar"))
    for btn in (
        "feed-reply-button",
        "feed-repost-button",
        "feed-quote-button",
        "feed-like-button",
    ):
        assert logged_in_app.feed.interaction_button_count(btn) >= 1, (
            f"{btn} should render on the post card "
            f"(post_count={logged_in_app.feed.post_count()} "
            f"error={logged_in_app.error_text()!r})"
        )


@pytest.mark.macos
@pytest.mark.ios
# tui NOT a candidate (the tui-excluding-marker audit, 2026-08-21):
# genuinely apple-only mechanism, per this test's own docstring below — it
# exists because apple's lazy feed `List` doesn't register `post-card` child
# elements in-process, forcing a snapshot-state read instead of an element
# read. tui already covers the same four counts via a real element read in
# `test_interaction_bar_buttons_render` above (tui-marked).
def test_interaction_counts_in_snapshot(logged_in_app):
    """The feed card binds the four shared interaction counts
    (``PostSummary.{like,reply,repost,quote}_count``, ratified 2026-06-27 —
    feed.md § Interaction bar) from the FeedManager snapshot. A freshly-created
    post has no activity, so all four are 0 — the icon-only, count-hidden-at-0
    baseline the bar renders (the count appears only once > 0). This proves the
    counts flow snapshot → client on apple; the non-zero increment *values* are
    tier_3-proven by ``tests/api/test_engagement_counts.py``.

    apple-scoped (macos/ios): apple reads the feed from the in-process snapshot
    state because the lazy feed ``List`` doesn't register its ``post-card`` child
    elements in-process (the same harness gap that excludes apple from
    ``test_feed_unverified_source.py``); the other apps' count legs land with
    their own element-read assertions (feed.md § Implementation status today →
    the per-app UI legs)."""
    text = _unique("counts")
    logged_in_app.feed.create_post(text=text)
    # wait_for_post_state_by_text, not a single read: create_post's own completion
    # criterion is a UI-element wait, which can settle a tick before the SEPARATE
    # state-protocol dump reflects the same post under load (e2e-conventions.md
    # § point 14).
    row = logged_in_app.feed.wait_for_post_state_by_text(text)
    counts = None if row is None else {
        "like": row.get("like_count"),
        "reply": row.get("reply_count"),
        "repost": row.get("repost_count"),
        "quote": row.get("quote_count"),
    }
    assert counts == {"like": 0, "reply": 0, "repost": 0, "quote": 0}, (
        f"fresh post {text!r} should bind all four interaction counts as 0; got "
        f"{counts!r} error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("feed-interactions")
def test_like_moves_its_own_count(logged_in_app):
    """Tapping ``feed-like-button`` moves **that post's own** like count on the
    tapping app, with no unrelated refresh in between.

    This is the arm ``tests/api/test_engagement_counts.py`` cannot cover: that
    one proves the nest counts correctly over the raw wire, and passes happily
    while every app throws the result away. Here the only thing that happens
    between the two reads is the user's tap, so a green run means the app
    actually surfaced it.

    The count is read from the shared snapshot
    (``PostSummary.like_count`` — feed.md § Interaction bar), which is what all
    seven apps render; the assertion is therefore app-agnostic. A repeat tap
    would NOT move it again (the nest's like counter is idempotent per
    (actor, post)), so the test taps exactly once.
    """
    text = _unique("like-count")
    logged_in_app.feed.create_post(text=text)
    # Deadline-poll, not a single read: create_post's own completion criterion is a
    # UI-element wait (first_post_text), which can settle a tick before the SEPARATE
    # state-protocol dump reflects the same post — a single-shot read races that gap
    # under load (e2e-conventions.md § point 14; the SAME race wait_for_interaction_count
    # below already guards against).
    before = logged_in_app.feed.wait_for_interaction_count(text, "like", 0, timeout=15)
    assert before == 0, (
        f"fresh post {text!r} should start at like_count 0; got {before!r} "
        f"error={logged_in_app.error_text()!r}"
    )

    index = logged_in_app.feed.post_index_by_text(text)
    logged_in_app.feed.like_post(index)

    after = logged_in_app.feed.wait_for_interaction_count(text, "like", 1)
    assert after == 1, (
        f"tapping feed-like-button on {text!r} should move its own like count "
        f"0 → 1 on the tapping app; got {after!r} "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("feed-interactions")
def test_quote_creates_a_post_and_moves_its_own_count(logged_in_app):
    """Tapping ``feed-quote-button`` **creates a quote post** and moves the
    target's own ``quote_count``.

    This is the witness whose absence let the whole composing half of the
    interaction bar ship dead. Every surrounding artifact read as covered — the
    button exists on all seven apps, ``tests/api/test_engagement_counts.py`` is
    green with ``("Quote", "quote_count")`` in its parametrize list, and
    feed.md described the behaviour in the present tense — but nothing pressed
    the button. It turned out ``fauna.posts.interact``'s native arm creates
    nothing for reply/repost/quote, so all three were no-ops (and reply
    additionally *discarded* typed text). See feed.md § Implementation status
    today.

    The distinction that makes this test different from
    ``test_like_moves_its_own_count``: a like is *recorded* against the target,
    a quote is *composed* — the count moves only because a new post referencing
    the target lands (``record_reference_engagements``). So an app that fires
    the old ``interact(id, "quote")`` call passes every count-binding assertion
    and fails this one.

    Marker-scoped to the apps whose quote button is wired to the composer; the
    four remaining app legs are tracked in the apps work queue. The
    counter bump is awaited inline in the nest's create handler
    (``routes.rs`` — before ``fauna.posts.create`` returns), so the count is
    already moved when the manager reads it back; the deadline poll below
    covers app-side render latency only.
    """
    text = _unique("quote-count")
    logged_in_app.feed.create_post(text=text)
    before = logged_in_app.feed.wait_for_interaction_count(text, "quote", 0, timeout=15)
    assert before == 0, (
        f"fresh post {text!r} should start at quote_count 0; got {before!r} "
        f"error={logged_in_app.error_text()!r}"
    )

    index = logged_in_app.feed.post_index_by_text(text)
    logged_in_app.feed.quote_post(index)

    after = logged_in_app.feed.wait_for_interaction_count(text, "quote", 1)
    assert after == 1, (
        f"tapping feed-quote-button on {text!r} must COMPOSE a quote post and "
        f"move its quote count 0 → 1; got {after!r}. A count stuck at 0 means "
        f"the app is still calling interact(id, 'quote'), which creates nothing "
        f"on a native post. error={logged_in_app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("feed-interactions")
def test_reply_creates_a_post_and_moves_its_own_count(logged_in_app):
    """Tapping ``feed-reply-button``, typing text, and submitting **creates a
    reply post carrying the typed body** and moves the target's own
    ``reply_count`` — the twin of
    :func:`test_quote_creates_a_post_and_moves_its_own_count`.

    Reply is the more severe half of the composing-half defect
    (feed.md § Implementation status today): an app still calling
    ``interact(id, "reply", body)`` doesn't just fail to move the count — it
    silently DISCARDS the user's typed text (the nest's native arm never reads
    ``body`` on that path), so this test asserts both the count AND that the
    created post's body is the exact text that was typed, not just that
    *something* moved. Reply/quote deliberately do NOT reload the feed window
    (re-ranking it under the user's finger) — only the target's counter is
    folded in locally — so, mirroring
    :func:`test_repost_toggle_creates_and_removes_the_callers_repost`, a
    throwaway ``create_post`` reloads the window before the body-landed check.

    Marker-scoped to the apps whose reply button opens a real compose dialog
    onto ``FeedManager::reply`` (the same set quote is scoped to): linux was
    always wired (``build_reply_dialog`` → ``client.rs``'s ``m.reply(...)``)
    but had no marker here until this pass caught the gap; tui had no compose
    surface at all until this pass; windows was the
    remaining app still calling the raw discarding ``interact`` door, fixed
    this pass. android's ``feed-reply-button``
    opens a real ``feed-reply-dialog`` and its ``feed-quote-button`` composes
    in one tap (2026-10-05); android stays unmarked only until a recorded run
    on its e2e venue earns the marker.
    """
    text = _unique("reply-count")
    logged_in_app.feed.create_post(text=text)
    before = logged_in_app.feed.wait_for_interaction_count(text, "reply", 0, timeout=15)
    assert before == 0, (
        f"fresh post {text!r} should start at reply_count 0; got {before!r} "
        f"error={logged_in_app.error_text()!r}"
    )

    reply_text = _unique("reply-body")
    index = logged_in_app.feed.post_index_by_text(text)
    logged_in_app.feed.reply_post(reply_text, index)

    after = logged_in_app.feed.wait_for_interaction_count(text, "reply", 1)
    assert after == 1, (
        f"replying to {text!r} must COMPOSE a reply post and move its reply "
        f"count 0 → 1; got {after!r}. A count stuck at 0 means the app is "
        f"still calling interact(id, 'reply'), which creates nothing on a "
        f"native post. error={logged_in_app.error_text()!r}"
    )

    # Composing reloads the window (repost-toggle test's established pattern).
    logged_in_app.feed.create_post(text=_unique("reply-reload-marker"))
    assert logged_in_app.feed.wait_for_post_text(reply_text), (
        f"the reply's typed body {reply_text!r} never appeared in the feed "
        f"after a reload — a count that moved without the body landing means "
        f"interact() (or something else) discarded the typed text. "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("feed-interactions")
def test_like_toggle_records_and_reverses_the_callers_like(logged_in_app):
    """The like TOGGLE end to end (feed.md § Interaction bar → Repost, which
    ratifies the ``viewer_liked`` carrier): a first tap records the like and
    lights the button, a second tap UN-likes and reverses the count.

    **Why this is a separate test from**
    ``test_like_moves_its_own_count``: that one is unmarked and runs on all
    seven apps, asserting the half every app has — a tapped ♥ moves its own
    count. The reverse half exists only where the button is wired to
    ``FeedManager::like``, so widening the universal test would red on the
    app still firing bare ``interact(id, "like")`` rather than report an
    honest per-app gap (convention 7). The markers here name the apps whose leg
    has landed — android's UI already calls the manager's own toggle
    (``FeedVM.like`` → ``FfiFeedManager.like``); it was excluded only by the
    id-keyed state-read helpers' android gap, closed by the ``data.feed.posts``
    state dump.

    **The gap this closes.** The nest has shipped ``unlike`` since the counters
    landed, but its like arm is *idempotent per (actor, post)* — a second tap
    on the old one-way call moved nothing — and ``PostSummary`` carried no
    viewer state to route off until the per-viewer pair was ratified
    2026-08-10. So on every app a like was permanent: nothing in the product
    could reach ``unlike`` at all.

    Both directions ride the same interact door on the same post id (a like is
    *recorded*, not composed — § User actions' two-verb split), so unlike the
    repost toggle no row joins or leaves the window. Reads are id-keyed anyway,
    matching the repost witness: text-keyed reads are ambiguous the moment any
    embed fold puts one post's text inside another's plaintext.
    """
    text = _unique("like-toggle")
    logged_in_app.feed.create_post(text=text)
    row = logged_in_app.feed.wait_for_post_state_by_text(text)
    assert row is not None and row.get("post_id"), (
        f"fresh post {text!r} should be readable from feed state; "
        f"error={logged_in_app.error_text()!r}"
    )
    post_id = row["post_id"]
    assert row.get("like_count") == 0 and not row.get("viewer_liked"), (
        f"fresh post should start un-liked with like_count 0; got {row!r}"
    )

    # Toggle ON.
    index = logged_in_app.feed.post_index_by_id(post_id)
    logged_in_app.feed.like_post(index)
    after = logged_in_app.feed.wait_for_interaction_count_by_id(post_id, "like", 1)
    assert after == 1, (
        f"tapping feed-like-button on {text!r} must move its like count 0 → 1; "
        f"got {after!r}. error={logged_in_app.error_text()!r}; "
        f"{logged_in_app.feed.like_landing_diagnosis(index, post_id)}"
    )
    row = logged_in_app.feed.post_state_by_id(post_id)
    assert row and row.get("viewer_liked"), (
        f"after liking, the row must carry viewer_liked — the toggle's state, "
        f"and what routes the next tap to unlike; got {row!r}"
    )

    # Toggle OFF — the half that was unreachable from every app's UI.
    logged_in_app.feed.like_post(logged_in_app.feed.post_index_by_id(post_id))
    back = logged_in_app.feed.wait_for_interaction_count_by_id(post_id, "like", 0)
    assert back == 0, (
        f"a second tap must UN-like and reverse the count 1 → 0; got {back!r}. "
        f"A count stuck at 1 means the app is still calling "
        f"interact(id, 'like'), whose nest arm is idempotent per (actor, post) "
        f"and moves nothing on a repeat tap. error={logged_in_app.error_text()!r}"
    )
    row = logged_in_app.feed.post_state_by_id(post_id)
    assert row and not row.get("viewer_liked"), (
        f"after un-liking, viewer_liked must clear; got {row!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("feed-interactions")
def test_repost_toggle_creates_and_removes_the_callers_repost(logged_in_app):
    """The repost TOGGLE end to end (feed.md § Interaction bar → Repost,
    ratified 2026-08-10): a first tap COMPOSES the caller's empty-body
    ``Reference::Repost`` post — moving the target's ``repost_count`` and
    filling the ``viewer_repost_id`` projection — a reload renders the repost
    ROW (the ``reposted_post_id`` carrier), and a second tap UN-reposts,
    reversing the counter and dropping the row. Before 2026-08-10 every half
    of this was dark: no client composed a repost, ``query_feed`` never
    projected the target (a bridged repost painted as a blank card), and the
    nest's fully-built ``unrepost`` was unreachable by construction because
    ``PostSummary`` carried no viewer state to name the caller's own repost.

    Assertions are state-reads keyed by post id, not text: once the repost row
    shares the window with its original, the embed fold puts the ORIGINAL's
    text into the repost row's plaintext, so text-keyed reads can match either
    row (and the repost row's own counters are 0 — a text-keyed poll for 0
    could return before the original's counter actually reversed). The repost
    row is asserted via BOTH the state carrier and its on-screen
    ``repost-attribution`` marker (id user-approved 2026-08-11).

    Marker-scoped to all seven apps: android's repost button is also wired to
    ``FeedManager::repost`` (``FeedVM.repost``) and paints ``repost-attribution``
    (``Ids.REPOST_ATTRIBUTION``) — it was excluded only by the id-keyed
    state-read helpers' android gap (``data.feed.posts`` was hard-coded
    empty), closed.
    """
    text = _unique("repost-toggle")
    logged_in_app.feed.create_post(text=text)
    row = logged_in_app.feed.wait_for_post_state_by_text(text)
    assert row is not None and row.get("post_id"), (
        f"fresh post {text!r} should be readable from feed state; "
        f"error={logged_in_app.error_text()!r}"
    )
    original_id = row["post_id"]
    assert row.get("repost_count") == 0 and not row.get("viewer_repost_id"), (
        f"fresh post should start un-reposted with repost_count 0; got {row!r}"
    )

    # Toggle ON — the only card in the window, so index 0 is unambiguous.
    logged_in_app.feed.repost_post(logged_in_app.feed.post_index_by_text(text))
    after = logged_in_app.feed.wait_for_interaction_count_by_id(
        original_id, "repost", 1
    )
    assert after == 1, (
        f"tapping feed-repost-button on {text!r} must COMPOSE a repost post "
        f"and move its repost count 0 → 1; got {after!r}. A count stuck at 0 "
        f"means the app is still calling interact(id, 'repost'), which creates "
        f"nothing on a native post. error={logged_in_app.error_text()!r}"
    )
    row = logged_in_app.feed.post_state_by_id(original_id)
    assert row and row.get("viewer_repost_id"), (
        f"after reposting, the original's row must carry viewer_repost_id — "
        f"unrepost's argument, and the toggle's state; got {row!r}"
    )

    # A reload (composing reloads the window) serves the repost ROW itself,
    # now projected fresh by the nest: the carrier + viewer pair survive a
    # round trip, not just the manager's local fold.
    logged_in_app.feed.create_post(text=_unique("repost-reload-marker"))
    repost_row = logged_in_app.feed.wait_for_repost_row_by_target(original_id)
    assert repost_row is not None, (
        f"after a reload the caller's repost row must render, carrying "
        f"reposted_post_id={original_id!r} (the render carrier); feed state "
        f"holds no such row. error={logged_in_app.error_text()!r}"
    )
    # ...and it says so ON SCREEN. `repost-attribution` (id user-approved
    # 2026-08-11) is the element that distinguishes a repost card from an
    # empty-commentary quote card; before it, this could only be inferred from
    # the state dump, which is not what the user sees.
    repost_index = logged_in_app.feed.post_index_by_id(repost_row["post_id"])
    assert logged_in_app.feed.repost_attribution_count(repost_index) == 1, (
        f"the repost row (card {repost_index}) must paint one "
        f"repost-attribution marker; the original's own card must paint none. "
        f"error={logged_in_app.error_text()!r}"
    )
    assert logged_in_app.feed.repost_attribution_count(
        logged_in_app.feed.post_index_by_id(original_id)
    ) == 0, "an ordinary post's card must not claim to be a repost"
    row = logged_in_app.feed.post_state_by_id(original_id)
    assert row and row.get("viewer_repost_id") == repost_row.get("post_id"), (
        f"the nest's viewer_repost_id projection must name the caller's own "
        f"repost post; original row {row!r}, repost row {repost_row!r}"
    )

    # Toggle OFF — aimed by id (the text-keyed index could hit the repost row).
    index = logged_in_app.feed.post_index_by_id(original_id)
    assert index >= 0, f"original {original_id!r} left the window unexpectedly"
    logged_in_app.feed.repost_post(index)
    back = logged_in_app.feed.wait_for_interaction_count_by_id(
        original_id, "repost", 0
    )
    assert back == 0, (
        f"a second tap must UN-repost (the toggle's off direction) and reverse "
        f"the count 1 → 0; got {back!r}. error={logged_in_app.error_text()!r}"
    )
    row = logged_in_app.feed.post_state_by_id(original_id)
    assert row and not row.get("viewer_repost_id"), (
        f"after unreposting, viewer_repost_id must clear; got {row!r}"
    )
    assert logged_in_app.feed.repost_row_state_by_target(original_id) is None, (
        "the caller's just-deleted repost row must leave the window"
    )


# How long a label repaint may take to follow the state it paints. A ceiling for
# a broken surface, never a subject (convention 14): the poll returns on the
# first read that shows the new state.
LABEL_REPAINT_BUDGET_S = 15.0


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.feature("feed-interactions")
def test_a_count_shows_no_number_until_the_post_has_activity(logged_in_app):
    """feed.md § Interaction bar: each button is "icon + interaction count … the
    **count is hidden when 0** (a clean icon-only button until the post has
    activity)". Read off the buttons a user sees, not the snapshot: the snapshot
    carries a 0 either way, and hiding it is precisely what the button does
    with it.

    A fresh post's four buttons each paint an icon and no digit; one like
    gives the like button its number and leaves the other three bare.
    """
    feed = logged_in_app.feed
    text = _unique("count-hidden")
    feed.create_post(text=text)
    row = feed.wait_for_post_state_by_text(text)
    assert row is not None and row.get("post_id"), (
        f"fresh post {text!r} should be readable from feed state; "
        f"error={logged_in_app.error_text()!r}"
    )
    post_id = row["post_id"]
    index = feed.post_index_by_id(post_id)

    for button in feed.INTERACTION_BUTTONS:
        label = feed.interaction_button_label(button, index)
        assert label.strip(), (
            f"{button} on a fresh post must still paint its icon; got {label!r}"
        )
        assert not any(ch.isdigit() for ch in label), (
            f"{button} on a post with no activity must show no number; got {label!r}"
        )

    logged_in_app.driver.click("feed-like-button", scope=f"post-card[{index}]")
    assert feed.wait_for_interaction_count_by_id(post_id, "like", 1) == 1, (
        f"the like never landed on {text!r}; error={logged_in_app.error_text()!r}"
    )

    def like_label_shows_one():
        at = feed.post_index_by_id(post_id)
        label = feed.interaction_button_label("feed-like-button", at)
        return label if "1" in label else None

    wait_until(
        like_label_shows_one,
        LABEL_REPAINT_BUDGET_S,
        diagnose=lambda: (
            "the like button never showed its count: "
            f"{feed.interaction_button_label('feed-like-button', feed.post_index_by_id(post_id))!r}"
        ),
    )
    index = feed.post_index_by_id(post_id)
    for button in ("feed-reply-button", "feed-repost-button", "feed-quote-button"):
        label = feed.interaction_button_label(button, index)
        assert not any(ch.isdigit() for ch in label), (
            f"a like is not {button}'s activity — it must still show no number; "
            f"got {label!r}"
        )


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.feature("feed-interactions")
def test_a_repost_names_the_reposter_shows_the_original_and_opens_it(
    logged_in_app, nest_instance
):
    """feed.md § Interaction bar → Repost: a repost row "renders as
    **attribution + the shipped `quoted-post` embed of the original**" and
    "**activating the card opens the ORIGINAL's `post_detail`**, never the empty
    repost's".

    The original is someone else's, so the three halves are each told apart
    from their wrong answer: the card must name ME (the reposter), not the
    original's author; the embed must carry the original's words (the repost
    itself has none); and the detail it opens must be the original's — its
    author and its words — not the empty repost post. Authors are compared in
    whatever shape this app paints them (tui: the hex actor id), never assumed.
    """
    app = logged_in_app
    feed = app.feed

    # How this app names ME: the author line of a post of my own.
    mine = _unique("repost-reposter")
    feed.create_post(text=mine)
    my_row = feed.wait_for_post_state_by_text(mine)
    assert my_row is not None and my_row.get("post_id"), (
        f"own post {mine!r} should be readable from feed state; "
        f"error={app.error_text()!r}"
    )
    me = feed.post_author(feed.post_index_by_id(my_row["post_id"]))
    assert me.strip(), "the app should name the author of my own post"

    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    original_text = _unique("repost-original")
    ApiActor(
        nest_instance["url"], other["token"], other["actor_id_hex"],
        bytes(other["signing_key"]),
    ).post_to_feed("", original_text)
    # No app live-reloads on another actor's out-of-band post; re-select the
    # feed the way a user would (the `test_feed_post_delete.py` pattern).
    feed.open_feed("General")
    row = feed.wait_for_post_state_by_text(original_text)
    assert row is not None and row.get("post_id"), (
        f"the other actor's post {original_text!r} should reach the feed; "
        f"error={app.error_text()!r}"
    )
    original_id = row["post_id"]
    them = feed.post_author(feed.post_index_by_id(original_id))
    assert them.strip() and them != me, (
        f"the original's author ({them!r}) must read differently from mine "
        f"({me!r}) for this test to tell reposter from author"
    )

    feed.repost_post(feed.post_index_by_id(original_id))
    assert feed.wait_for_interaction_count_by_id(original_id, "repost", 1) == 1, (
        f"the repost never landed; error={app.error_text()!r}"
    )
    # Composing reloads the window, which serves the repost ROW itself.
    feed.create_post(text=_unique("repost-reload-marker"))
    repost_row = feed.wait_for_repost_row_by_target(original_id)
    assert repost_row is not None, (
        f"after a reload the repost row naming {original_id!r} must render; "
        f"error={app.error_text()!r}"
    )
    at = feed.post_index_by_id(repost_row["post_id"])

    assert feed.repost_attribution_count(at) == 1, (
        f"the repost card (card {at}) must say it is a repost"
    )
    assert feed.post_author(at) == me, (
        f"the repost card must name the reposter ({me!r}), not the original's "
        f"author ({them!r}); got {feed.post_author(at)!r}"
    )
    # The embed folds in asynchronously once the row has landed (the manager
    # resolves the original, then re-emits), so poll for it — a single read
    # right after the row appears races the fold (convention 14).
    wait_until(
        lambda: original_text in (feed.quoted_post_text(at) or ""),
        LABEL_REPAINT_BUDGET_S,
        diagnose=lambda: (
            f"the repost card must show the original inside it; quoted-post "
            f"text={feed.quoted_post_text(at)!r} error={app.error_text()!r}"
        ),
    )

    feed.open_post_detail(at)
    wait_until(
        lambda: feed.post_detail_visible() and original_text in feed.post_detail_body(),
        LABEL_REPAINT_BUDGET_S,
        diagnose=lambda: (
            f"visible={feed.post_detail_visible()} "
            f"body={feed.post_detail_body() if feed.post_detail_visible() else None!r} "
            f"error={app.error_text()!r}"
        ),
    )
    assert feed.post_detail_author() == them, (
        f"opening the repost must open the ORIGINAL — author {them!r} — not "
        f"the repost; detail author={feed.post_detail_author()!r}"
    )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("trending")
def test_trending_feed_selection(logged_in_app):
    """Selecting the built-in Trending feed-selector entry (`feed-trending-item`)
    queries the virtual `fauna.feed.trending.posts` read (trending.md § The
    Trending feed) and renders its public posts. A just-created public post
    surfaces in the Trending feed — the scored read serves every public post
    (hot ones first; a fresh post scores 0 and sorts by recency, so it lands at
    the top among the un-engaged). Proves the client's Trending selector → shared
    `FeedManager.select_trending_feed` → `fauna.feed.trending.posts` wire →
    render path end to end.

    Rollout `linux → windows → web → apple` (apple
    mirrors linux/web's row on both FeedListView/MacFeedListView, driven by the
    shared `FfiFeedManager.selectTrendingFeed()` face); android follows (blocked
    fleet-wide on the android bridge's e2e access, not this surface — Robolectric-
    proven per trending.md § Implementation status). The trending SCORING/ordering
    with real cross-nest engagement is tier_3-proven by
    `tests/api/test_trending_federation.py`; this leg proves the per-app
    selector wiring the nest side has been waiting on. **apple leg is
    compile-verified only (swift-test 220/220 + mac-debug), not yet e2e-run** —
    landed under machine contention (3 concurrent mac sessions, load 8). **Windows landed**
    (Task 1 of the windows personalization lift): its own `feed-trending-item`
    Button above LocalFeedButton (mirrors web/linux's above-Local ordering), wired
    through `FeedViewModel.SelectTrendingFeedAsync()` → the same
    `FfiFeedManager.select_trending_feed()` face; the reload/reconnect path
    (`FeedViewModel.ReloadAsync`) was fixed to re-select the current SOURCE
    (Trending vs. a normal feed) rather than unconditionally re-selecting
    `SelectedFeedId` — the same selection-preservation bug android/apple hit,
    reproduced here since `SelectedFeedId` is `null` both for the local feed and
    while Trending is selected.

    ⚠ **The gated post is the whole test.** Until 2026-08-10 this test created
    the viewer's OWN public post and asserted it was visible after selecting
    Trending — which the LOCAL feed serves just as happily (`query_local_feed_core`
    scopes by nothing but the spam guard), so it pinned "selecting Trending does
    not break the feed" and nothing more. Mutation-measured on tui 2026-07-29:
    replacing `select_trending_feed()` with `select_feed(None)` — i.e. the exact
    bug three apps shipped, staying on Local — left it PASSING in 46.71s. The
    only discriminating UI observable is that **Trending is public-posts-only**
    (`trending.md` § Implementation status today: `query_feed_scored_public`
    adds `content_meta.gated_tier IS NULL`), so the fixture now seeds a *gated*
    post beside the public one and requires it to LEAVE. A green assertion is
    not coverage (testing.md § point 7's sibling)."""
    feed = logged_in_app.feed
    public_text = _unique("trending-public")
    gated_preview = _unique("trending-gated")

    feed.create_post(text=public_text)
    # `sell_post`, not `create_gated_post`: the sell path auto-mints its own
    # degenerate unlock tier as part of the submit (`prepare_sell_post`), so the
    # fixture needs no pre-existing subscription tier — and it is built on all 7
    # apps (`monetization.md` § Per-post pay-to-unlock).
    feed.sell_post("the sealed body, buyers only.", gated_preview, "$3")

    # ── Precondition, on the LOCAL feed: BOTH posts are visible here. ─────────
    # Without this the negative assertion below could pass vacuously — a gated
    # post that never rendered in the first place also "leaves".
    assert feed.wait_for_post_text(gated_preview), (
        f"the gated post {gated_preview!r} should be visible in the LOCAL feed "
        f"before Trending is selected (the local read has no gated filter); "
        f"post_count={feed.post_count()} error={logged_in_app.error_text()!r}"
    )
    assert feed.wait_for_post_text(public_text), (
        f"public post {public_text!r} should be visible in the local feed; "
        f"post_count={feed.post_count()} error={logged_in_app.error_text()!r}"
    )

    feed.select_trending()

    # ── The discriminator: Trending is public-posts-only, so the gated post ───
    # must LEAVE. Under the mutation this test exists to catch (stay on Local)
    # it never does, and the wait burns its budget into a RED.
    assert feed.wait_for_post_text_absent(gated_preview), (
        f"gated post {gated_preview!r} is STILL rendered after selecting "
        f"feed-trending-item — the client is showing the LOCAL feed, not the "
        f"public-posts-only Trending read (trending.md § Implementation status "
        f"today); post_count={feed.post_count()} "
        f"error={logged_in_app.error_text()!r}"
    )
    assert feed.wait_for_post_text(public_text), (
        f"public post {public_text!r} should surface in the Trending feed after "
        f"selecting feed-trending-item; post_count={feed.post_count()} "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("trending")
# The re-pull's barrier is the new post painting at the TOP of Trending, which
# holds only where nobody else posts: on a populated box a post with no
# engagement ranks below a full first page.
@pytest.mark.harness_box
def test_trending_selection_survives_a_feed_re_pull(logged_in_app):
    """A re-pull of the feed keeps a Trending viewer ON Trending.

    This is the regression test for the bug class fixed, and it
    could not be written before the gated-post observable above existed — which
    is precisely why the bug survived six apps' rollouts and was re-shipped every
    time. The shape is always the same (`trending.md` § Implementation status
    today, the ⚠ paragraph): a refresh / reconnect / remount / post-interaction
    path re-selects `select_feed(snapshot().selected_feed)`, which resolves to
    **Local** while Trending is selected, because `selected_feed` is `None` for
    both. The shared `FeedManager::refresh_current_feed` exists to carry that
    branch once; an app that re-derives it re-derives the bug.

    **Composing is the re-pull**, and `create_post` blocks until the new post is
    painted — a real causal barrier (convention 14), so the negative assertion
    reads settled state without a settle-sleep. The second post is public, so it
    lands in Trending and in Local alike: it moves the feed without telling the
    test which feed answered. The gated post remains the only discriminator."""
    feed = logged_in_app.feed
    public_text = _unique("trending-keep-public")
    gated_preview = _unique("trending-keep-gated")

    feed.create_post(text=public_text)
    feed.sell_post("the sealed body, buyers only.", gated_preview, "$3")
    assert feed.wait_for_post_text(gated_preview), (
        f"the gated post {gated_preview!r} should be visible in the LOCAL feed "
        f"first; post_count={feed.post_count()} "
        f"error={logged_in_app.error_text()!r}"
    )

    feed.select_trending()
    assert feed.wait_for_post_text_absent(gated_preview), (
        f"gated post {gated_preview!r} still rendered after selecting Trending "
        f"— the selection never took, so this test cannot say anything about "
        f"whether a re-pull PRESERVES it; post_count={feed.post_count()} "
        f"error={logged_in_app.error_text()!r}"
    )

    # The re-pull. Returns only once the new post is painted, so the read below
    # is of settled post-re-query state.
    feed.create_post(text=_unique("trending-keep-second"))

    assert not feed.post_text_visible(gated_preview), (
        f"the gated post {gated_preview!r} came BACK after a compose re-pulled "
        f"the feed — this client dropped a Trending viewer into Local, the "
        f"`select_feed(selected_feed)` bug `refresh_current_feed` exists to "
        f"prevent (trending.md § Implementation status today); "
        f"post_count={feed.post_count()} error={logged_in_app.error_text()!r}"
    )


def _trending_ranks(feed, *texts: str) -> list[int | None]:
    """Each text's position in the list the app holds for the selected feed —
    muted posts included (they stay in the list, collapsed) — or ``None`` when
    the loaded window does not carry it."""
    bodies = feed.model_post_bodies() or []
    ranks: list[int | None] = []
    for text in texts:
        ranks.append(next((i for i, body in enumerate(bodies) if text in body), None))
    return ranks


def _liked_above_plain_on_trending(logged_in_app, api_actor_peer, label: str,
                                   liked_word: str = ""):
    """Seed the Trending discriminator: two public posts, the NEWER one
    un-engaged, the older one liked by another user on this nest. One like gives
    the older post a trend score, so on Trending it ranks ABOVE the newer one —
    the reverse of their recency order, which only the trend score can produce.
    ``liked_word`` goes into the liked post's body only. Returns
    ``(liked, plain, ranks)`` once Trending shows that order."""
    feed = logged_in_app.feed
    liked = f"{_unique(f'trending-{label}-liked')} {liked_word}".strip()
    plain = _unique(f"trending-{label}-plain")
    feed.create_post(text=liked)
    feed.create_post(text=plain)
    row = feed.wait_for_post_state_by_text(liked)
    assert row, f"post {liked!r} never reached the app's state; error={logged_in_app.error_text()!r}"
    api_actor_peer.like_post(row["post_id"])

    feed.select_trending()
    ranks = wait_until(
        lambda: (lambda r: r if None not in r and r[0] < r[1] else None)(
            _trending_ranks(feed, liked, plain)
        ),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"a liked post should rank above a newer un-engaged one on Trending "
            f"before any factor of the user's applies; ranks(liked, plain)="
            f"{_trending_ranks(feed, liked, plain)} error={logged_in_app.error_text()!r}"
        ),
    )
    return liked, plain, ranks


def _sunk_below(feed, sunk: str, above: str):
    """``True`` once ``above`` is loaded and ``sunk`` sits below it or has left
    the loaded window — a post sunk under every unscored public post can fall
    off the first page on a busy nest, which is still sunk."""
    sunk_rank, above_rank = _trending_ranks(feed, sunk, above)
    return above_rank is not None and (sunk_rank is None or sunk_rank > above_rank)


@pytest.mark.feature("trending")
def test_a_global_factor_still_sinks_a_post_on_trending(
    logged_in_app, nest_instance, test_user, api_actor_peer, request,
):
    """Trending composes the user's GLOBAL factor set on top of the trend score
    (`trending.md` § The Trending feed: `[(trending, 1000)]` plus the caller's
    global factors, nest-side), so a post the user's own global factor sinks
    stays sunk there. The factor is set the way a user sets it — `engagement`
    weighted strongly negative, applied to all feeds from the create-feed form —
    and it sinks exactly the liked post, whose trend score had lifted it above a
    newer un-engaged one."""
    port = nest_instance["port"]
    pre_existing = ws_api.feed_factors_get(port, test_user)
    request.addfinalizer(lambda: ws_api.feed_factors_set(port, test_user, pre_existing))
    feed = logged_in_app.feed
    liked, plain, _ = _liked_above_plain_on_trending(
        logged_in_app, api_actor_peer, "global-factor",
    )

    # One like scores 48‰ on trending and 1/51 on engagement, so ×-5 engagement
    # (-98) outweighs it: the liked post's key goes below the plain post's 0.
    feed.create_feed_with_factor(
        name=_unique("sink-engaged-everywhere"), factor_label="engagement",
        weight="-5.0", global_scope=True,
    )
    by_factor = {f["factor"]: f["weight_permille"] for f in ws_api.feed_factors_get(port, test_user)}
    assert by_factor.get("engagement") == -5000, (
        f"the global engagement weight did not land: {by_factor!r}; "
        f"error={logged_in_app.error_text()!r}"
    )

    feed.select_trending()
    assert wait_until(
        lambda: _sunk_below(feed, liked, plain),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            "the user's global factor must still sink a post on Trending; "
            f"ranks(liked, plain)={_trending_ranks(feed, liked, plain)} "
            f"error={logged_in_app.error_text()!r}"
        ),
    )


@pytest.mark.feature("trending")
def test_a_muted_word_still_sinks_a_post_on_trending(logged_in_app, api_actor_peer):
    """A muted word sinks a post on Trending too (`trending.md` § The Trending
    feed: sealed factors — muted keywords among them — compose client-side on
    top of the nest-served key, with no special path for Trending). The muted
    post is the liked one, which the trend score had lifted above a newer
    un-engaged post, so it must fall below it — and collapse, as a muted post
    does everywhere."""
    app = logged_in_app
    feed = app.feed
    muted = app.muted_words
    # One unbroken word, so it matches as a whole word and nothing else does.
    term = f"zzmute{uuid.uuid4().hex[:8]}"
    liked, plain, _ = _liked_above_plain_on_trending(
        app, api_actor_peer, "muted-word", liked_word=term,
    )

    muted.navigate()
    before = muted.row_count()
    muted.add(term)
    try:
        assert muted.wait_for_row_count(before + 1), f"muted term {term!r} did not persist"
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        feed.select_trending()
        assert wait_until(
            lambda: _sunk_below(feed, liked, plain),
            budgets.UI_SETTLE_S,
            diagnose=lambda: (
                "a muted word must still sink a post on Trending; "
                f"ranks(liked, plain)={_trending_ranks(feed, liked, plain)} "
                f"error={app.error_text()!r}"
            ),
        )
        assert not feed.post_text_visible(liked), (
            f"the muted post {liked!r} must collapse on Trending, not paint its body; "
            f"error={app.error_text()!r}"
        )
    finally:
        muted.navigate()
        words = muted.words()
        if term in words:
            muted.remove(words.index(term))


@pytest.mark.feature("feed-compose")
def test_create_multiple_posts(logged_in_app):
    """Create two posts and verify both are on the feed.

    Asserted on the test's OWN posts, never on the count: every app caps the
    feed at its first page (50 cards), so on an account whose feed is already
    full — the run-scoped `test_user` late in a long run, any account on a
    populated box — two new posts displace two old ones and the count never
    moves."""
    feed = logged_in_app.feed
    first, second = _unique("multi-1"), _unique("multi-2")
    feed.create_post(text=first)
    feed.create_post(text=second)
    for text in (first, second):
        assert feed.wait_for_post_text(text), (
            f"post {text!r} should be on the feed after two creates; "
            f"post_count={feed.post_count()} error={logged_in_app.error_text()!r}"
        )


# How long a triggered reload may take to START (reach its parked fetch) and,
# once released, to COMMIT. Ceilings for a broken surface, never subjects
# (convention 14): each poll returns on the first read that shows the state.
RELOAD_PARK_BUDGET_S = 30.0


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.feature("feed-read")
def test_a_refresh_keeps_the_posts_on_screen_and_only_a_switch_clears_them(
    logged_in_app,
):
    """feed.md § The read model: "A REFRESH keeps the posts on screen until the
    new page lands; only a SWITCH clears them up front … A **switch** (a
    different `selected_feed`, Trending, a changed `search_query`) clears the
    list immediately … A **refresh of the query already on screen** … must NOT
    clear."

    "Until the new page lands" is a moment, and nothing can hold it open
    without a clock except holding the fetch itself: the manager's test-only
    reload hold lets the reload publish the list it decided on and then parks
    it before the fetch. While it is parked (``feed_reloads``: started past
    committed), the test reads what the page paints:

    * a REFRESH — leaving the Feed page and coming back to it, the user's own
      way to re-pull the same query (a page ENTRY re-pulls on every app; only
      tui also re-pulls on a tap of the tab it is already on) — still shows
      the post;
    * a SWITCH — typing a search, which changes the query — shows none.

    Both are released and allowed to land before the test leaves, and the
    search is cleared, so the shared session's feed is as it was.
    """
    app = logged_in_app
    feed = app.feed
    text = _unique("refresh-keeps")
    feed.create_post(text=text)
    assert feed.wait_for_post_text(text), (
        f"post {text!r} never reached the feed; error={app.error_text()!r}"
    )
    on_screen = feed.post_card_count()

    def parked():
        return feed.reload_in_flight()

    # ── A refresh of the feed on screen: its posts stay while it is in flight.
    # Leave first and arm the hold only then, so the reload it parks is the one
    # the way back starts (`FeedActions.reenter` — the entry is what re-pulls).
    app.driver.navigate_to("conversations")
    baseline = feed_reload_baseline(app.driver)
    feed.hold_next_reload_for_test()
    try:
        feed.navigate()
        wait_until(
            parked,
            RELOAD_PARK_BUDGET_S,
            diagnose=lambda: (
                "re-entering the Feed page started no reload the hold could park; "
                f"baseline={baseline!r} "
                f"feed_reloads={app.driver.get_state('feed_reloads')!r}"
            ),
        )
        assert feed.post_text_visible(text), (
            "a refresh of the feed on screen must keep its posts until the new "
            "page lands; the post left the page while the refresh was in flight"
        )
        assert feed.post_card_count() == on_screen, (
            f"a refresh in flight must keep every card ({on_screen}); the page "
            f"paints {feed.post_card_count()}"
        )
    finally:
        feed.release_held_reload_for_test()
    await_feed_reload_after(
        app.driver, baseline, budget_s=RELOAD_PARK_BUDGET_S,
        what="the released refresh",
    )
    assert feed.post_text_visible(text), "the refreshed page must still hold the post"

    # ── A switch — a changed search query — clears the list up front.
    # Every write to the search field re-queries (an unchanged one as a
    # refresh), and `clear_and_type` is two writes, so the one-shot hold would
    # go to the clear and the search would run free. Empty the field first,
    # unheld, so the ONE write the hold is armed for is the search itself.
    app.driver.clear_and_type("feed-search-field", "")
    baseline = feed_reload_baseline(app.driver)
    feed.hold_next_reload_for_test()
    try:
        app.driver.type_text("feed-search-field", _unique("matches-nothing"))
        wait_until(
            parked,
            RELOAD_PARK_BUDGET_S,
            diagnose=lambda: (
                "typing a search started no reload the hold could park; "
                f"baseline={baseline!r} "
                f"feed_reloads={app.driver.get_state('feed_reloads')!r}"
            ),
        )
        assert feed.post_card_count() == 0, (
            "a switch must clear the list up front — the previous query's posts "
            f"under the new query are a lie; the page still paints "
            f"{feed.post_card_count()} card(s)"
        )
    finally:
        feed.release_held_reload_for_test()
    await_feed_reload_after(
        app.driver, baseline, budget_s=RELOAD_PARK_BUDGET_S,
        what="the released search",
    )
    feed.clear_feed_search()
    assert feed.wait_for_post_text(text), (
        "clearing the search must bring the feed back"
    )


@pytest.mark.feature("feed-read")
def test_post_ordering_newest_first(logged_in_app):
    """Post 3 messages, verify newest appears first (at top of feed)."""
    t1 = _unique("order-first")
    t2 = _unique("order-second")
    t3 = _unique("order-third")
    logged_in_app.feed.create_post(text=t1)
    logged_in_app.feed.create_post(text=t2)
    logged_in_app.feed.create_post(text=t3)
    # Wait for the last-composed post to arrive rather than sleeping a fixed second
    # (convention 14 — the `sleep(1)` this replaces asserted on whatever had landed
    # by then, so under load it read a half-populated feed).
    assert logged_in_app.feed.wait_for_post_text(t3), (
        f"the third composed post never rendered; error={logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('post-card')}"
    )

    # Assert the RELATIVE order of the three posts this test composed, not absolute
    # indices 0/1/2. Absolute indices silently encode "the feed is empty", which is
    # false: `test_user`/`nest_instance` are session-scoped, so every earlier test in
    # this file has left its own posts above these three. That made the assertion a
    # test-ORDER dependency — it passed only while enough earlier tests had failed to
    # leave the feed short, and started failing the moment an earlier test was fixed
    # and contributed one more post.
    i3 = logged_in_app.feed.post_index_by_text(t3)
    i2 = logged_in_app.feed.post_index_by_text(t2)
    i1 = logged_in_app.feed.post_index_by_text(t1)
    assert i3 >= 0 and i2 >= 0 and i1 >= 0, (
        f"all three composed posts must render; indices t3={i3} t2={i2} t1={i1} "
        f"error={logged_in_app.error_text()!r}"
    )
    assert i3 < i2 < i1, (
        f"posts must be ordered newest-first: expected index({t3!r}) < index({t2!r}) "
        f"< index({t1!r}); got {i3} < {i2} < {i1}"
    )


@pytest.mark.feature("feed-compose")
def test_post_with_tags(logged_in_app):
    """Post with tags, verify tag chips render."""
    text = _unique("tagged")
    logged_in_app.feed.create_post_with_tags(text=text, tags="rust, svelte, wasm")
    tags = logged_in_app.feed.post_tags_by_text(text)
    assert len(tags) == 3, f"3 tag chips expected; got {len(tags)}: {tags!r}"
    assert "#rust" in tags, f"#rust expected among the rendered chips; got {tags!r}"
    assert "#svelte" in tags, f"#svelte expected among the rendered chips; got {tags!r}"
    assert "#wasm" in tags, f"#wasm expected among the rendered chips; got {tags!r}"


@pytest.mark.feature("feed-images-and-video")
def test_post_with_image(logged_in_app):
    """Post with image attachment, verify image renders.

    WASM is initialized when the feed page loads (ensureWasm() in onMount),
    so by the time feed-view is visible, crypto signing for blob upload works.
    """
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    text = _unique("image")
    logged_in_app.feed.create_post_with_image(text=text, image_path=str(TEST_IMAGE))
    assert logged_in_app.feed.post_has_image_by_text(text), (
        f"post {text!r} should render its attached image (image-RENDER path; "
        f"a red here on apple is the known post_has_image_by_text gap): "
        f"post_count={logged_in_app.feed.post_count()} error={logged_in_app.error_text()!r}"
    )
    blob_hash = logged_in_app.feed.post_image_blob_hash_by_text(text)
    assert len(blob_hash) == 64, f"expected a 64-hex blob hash, got {blob_hash!r}"

    # The positive fetch → paint path, not just a registered element: THIS post's
    # `post-image` shows decoded bytes. `post_image_painted_by_text` says how each
    # app reads that, and declares the apps that cannot yet.
    assert logged_in_app.feed.post_image_painted_by_text(text), (
        f"post {text!r} should PAINT its image, not just register a placeholder; "
        f"paint={logged_in_app.feed.post_image_paint_report()} "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("feed-images-and-video")
def test_post_with_image_and_tags(logged_in_app):
    """Post with image + tags, verify both render together.

    WASM is initialized when the feed page loads (ensureWasm() in onMount),
    so by the time feed-view is visible, crypto signing for blob upload works.
    """
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    text = _unique("sunset")
    logged_in_app.feed.create_post_with_image_and_tags(
        text=text, tags="photography, nature", image_path=str(TEST_IMAGE),
    )
    assert logged_in_app.feed.post_has_image_by_text(text), (
        f"post {text!r} should render its attached image alongside tags "
        f"(image-RENDER path; known apple gap if red): "
        f"post_count={logged_in_app.feed.post_count()} error={logged_in_app.error_text()!r}"
    )
    tags = logged_in_app.feed.post_tags_by_text(text)
    assert len(tags) == 2, f"2 tag chips expected; got {len(tags)}: {tags!r}"
    assert "#photography" in tags, f"#photography expected among chips; got {tags!r}"
    assert "#nature" in tags, f"#nature expected among chips; got {tags!r}"


@pytest.mark.feature("feed-images-and-video")
def test_post_has_image_by_text_is_scoped_to_the_named_post(logged_in_app):
    """`post_has_image_by_text` answers about the post it is asked about, not
    the feed as a whole — the check must be scoped
    to `post-card[i]`, never a global `post-image` count that answers `True`
    the moment ANY post on the feed carries an image.

    Creates the imaged post FIRST, then a plain text post, so a
    global-count implementation (`True` as soon as one image exists
    anywhere) would pass the positive assertion below for the wrong reason;
    asking about the plain post's own text is what pins the scoping down.
    """
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    imaged_text = _unique("has-image")
    plain_text = _unique("no-image")
    logged_in_app.feed.create_post_with_image(text=imaged_text, image_path=str(TEST_IMAGE))
    assert logged_in_app.feed.post_has_image_by_text(imaged_text), (
        f"the imaged post {imaged_text!r} should report an image"
    )
    logged_in_app.feed.create_post(text=plain_text)
    assert not logged_in_app.feed.post_has_image_by_text(plain_text, timeout=3), (
        f"post {plain_text!r} carries no attachment, but post_has_image_by_text "
        f"answered True — it is reading the feed's image count instead of this "
        f"post's own render"
    )


@pytest.mark.feature("feed-images-and-video")
def test_post_image_uploads_via_sidecar_and_round_trips(logged_in_app, nest_instance):
    """The composer attachment uploads through the multipart UploadSidecar wire
    shape (fauna-media `process_and_seal`), not the legacy raw-bytes POST.

    This is a tier_3 integration guard for the *full* attach→post→render→fetch
    flow, landed by the structured-MediaItem
    compose + client-side media-hash decode work. The upload-sidecar wire bytes
    themselves are additionally pinned by the Rust tests cited below.

    Linux posts are public, so the blob is the `PublicPost` audience: stored
    plaintext (public-post media is signed plaintext, no key —
    `encryption-at-rest.md` Media row) and served with an image MIME. We assert
    it round-trips via the public `GET /api/v1/blob/{hash}`: the stored bytes
    are a plaintext PNG (NOT AEAD-sealed garble, and not a 404 from a rejected
    upload), which proves the real nest accepted the client's multipart framing
    end-to-end. (The precise multipart+sidecar wire bytes are pinned by the
    Rust wiremock test in `libs/fauna-nest-http/tests/content_round_trip.rs`
    and the encrypted-mode per-class verifier by the nest's
    `bins/fauna-nest/tests/blob_ingest_sidecar.rs`.)
    """
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    text = _unique("image-roundtrip")
    logged_in_app.feed.create_post_with_image(text=text, image_path=str(TEST_IMAGE))
    assert logged_in_app.feed.post_has_image_by_text(text), (
        f"post {text!r} should render its attached image before the blob round-trip "
        f"(image-RENDER path; known apple gap if red): "
        f"post_count={logged_in_app.feed.post_count()} error={logged_in_app.error_text()!r}"
    )
    blob_hash = logged_in_app.feed.post_image_blob_hash_by_text(text)
    assert len(blob_hash) == 64, f"expected a 64-hex blob hash, got {blob_hash!r}"

    # GET /api/v1/blob/{hash} is public (no bearer).
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/blob/{blob_hash}", method="GET"
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        body = resp.read()
        content_type = resp.headers.get("Content-Type", "")

    assert body[:8] == PNG_MAGIC, "blob is not stored as a plaintext PNG (PublicPost passthrough)"
    assert content_type.startswith("image/"), f"unexpected Content-Type: {content_type!r}"



@pytest.mark.feature("feed-images-and-video")
def test_post_image_strips_exif_gps_before_the_nest_stores_it(
    logged_in_app, nest_instance, tmp_path
):
    """A GPS-tagged photo posted through the composer reaches the nest already
    stripped: the bytes ``GET /api/v1/blob/{hash}`` serves back carry no ``eXIf``
    chunk and no trace of the canary that rode inside it.

    **Why this test and not the ones that already exist.** The EXIF strip is
    `fauna_media::process_media`, and whether a *shipped artifact* contains it is
    decided by Cargo feature unification across the whole dependency graph — with
    the `process_media` feature absent the pipeline is an identity passthrough:
    the upload still succeeds, the post still renders, and the photo keeps its
    GPS in bytes the nest then serves from a blob GET that is unauthenticated by
    design and has no DELETE. Two witnesses already guard pieces of that:
    `fauna-media`'s own EXIF canaries (`libs/fauna-media/tests/process_test.rs`)
    witness the *function*, in a crate where the feature is unconditional; a
    merge gate witnesses the *artifact*, by
    resolving each shipped binary's feature set. Both ultimately trust that `process_media`
    is what runs on the compose path. This one does not: it drives the real
    composer and reads the real served bytes, so it survives a refactor of the
    feature graph — or of the pipeline — entirely (`ui/media.md` § Encryption at
    rest; `principles.md` § The user always controls their data).

    **Why the served blob is a witness of the CLIENT.** The nest never strips:
    "it cannot read the bytes … media processing is uploader-side only"
    (`ui/media.md` § Encryption at rest → *Today's reality*), and
    `bins/fauna-nest/src/blob_routes.rs::upload_blob` stores the multipart
    `bytes` part verbatim. So EXIF-freedom here can only have happened in the app
    under test.

    An ungated post is the `PublicPost` audience, which rests **plaintext** by
    design (public-post media is signed, not sealed), so these bytes are readable
    without a key — which is exactly why their metadata is worth asserting on.

    **Non-vacuity.** The fixture is checked to carry the canary in a real `eXIf`
    chunk *before* it is handed to the composer; without that check a builder
    that silently produced a plain PNG would make this pass forever.

    The producer is shared Rust (`process_and_seal` on every native app,
    `sealComposeAttachment` on web), so this is the same contract on all seven
    apps rather than a tui-specific guard.
    """
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")

    tagged = _png_with_exif_chunk(TEST_IMAGE.read_bytes(), EXIF_GPS_CANARY)
    assert EXIF_GPS_CANARY in tagged, (
        "fixture builder must embed the canary, else the strip assertion is vacuous"
    )
    assert any(ctype == b"eXIf" for ctype, _ in _png_chunks(tagged)), (
        "fixture builder must write a real eXIf chunk, else the strip assertion "
        "is vacuous"
    )
    tagged_path = tmp_path / "gps-tagged.png"
    tagged_path.write_bytes(tagged)

    text = _unique("exif-strip")
    logged_in_app.feed.create_post_with_image(text=text, image_path=str(tagged_path))
    assert logged_in_app.feed.post_has_image_by_text(text), (
        f"post {text!r} should render its attached image before the strip check: "
        f"post_count={logged_in_app.feed.post_count()} "
        f"error={logged_in_app.error_text()!r}"
    )
    blob_hash = logged_in_app.feed.post_image_blob_hash_by_text(text)
    assert len(blob_hash) == 64, f"expected a 64-hex blob hash, got {blob_hash!r}"

    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/blob/{blob_hash}", method="GET"
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        served = resp.read()

    assert served[:8] == PNG_MAGIC, (
        f"served blob is not a plaintext PNG, so the assertions below would be "
        f"reading something other than the photo: first bytes={served[:8]!r}"
    )

    metadata_chunks = [
        ctype.decode("ascii", "replace")
        for ctype, _ in _png_chunks(served)
        if ctype in (b"eXIf", b"tEXt", b"iTXt", b"zTXt")
    ]
    assert not metadata_chunks, (
        f"the nest is serving metadata chunks {metadata_chunks} on a photo posted "
        f"through the composer — the uploader-side EXIF strip did not run in this "
        f"artifact — fauna-media's `process_media` feature is the usual cause"
        f""
    )
    assert EXIF_GPS_CANARY not in served, (
        "the photo's GPS canary survived into the bytes the nest serves from its "
        "public, undeletable blob endpoint — this is the privacy baseline, not a "
        "cosmetic strip"
    )

    # Last, because the two assertions above are the headline and should be the
    # message a red prints: these guard the *identity* of the bytes they read.
    # Without them the test would also pass if the app had uploaded a
    # re-encoded image, or if the primary hash had resolved to the derived
    # thumbnail — both EXIF-free for reasons that have nothing to do with the
    # strip. IHDR carries width/height/bit-depth/colour-type, so matching it
    # against the fixture's says "same photo"; the size decrease says something
    # was actually removed on the way rather than nothing having been there.
    def _ihdr(png: bytes) -> bytes:
        return next(data for ctype, data in _png_chunks(png) if ctype == b"IHDR")

    assert _ihdr(served) == _ihdr(tagged), (
        f"the served blob is a different image from the one attached — its IHDR "
        f"geometry is {_ihdr(served)!r}, the fixture's {_ihdr(tagged)!r} — so the "
        f"assertions above said nothing about the strip"
    )
    assert len(served) < len(tagged), (
        f"the served blob is not smaller than the tagged fixture "
        f"({len(served)} >= {len(tagged)} bytes) — nothing was removed on the way"
    )


@pytest.mark.feature("feed-images-and-video")
def test_post_image_thumbnail_is_uploaded_and_served(logged_in_app, nest_instance):
    """A feed image attached through the composer POSTs its derived thumbnail as
    a SECOND blob, so the nest's ``?thumb=1`` lookup serves the thumbnail rather
    than falling through to the full-size original.

    The producer is shared Rust (``fauna_media::process_and_seal`` renders the
    JPEG thumbnail; ``UploadPayload::into_multipart_parts`` hands the client both
    multipart pairs), so this asserts the same contract on every app.

    Why the assertion is shaped as "different AND smaller": the nest's thumb
    branch (``bins/fauna-nest/src/blob_routes.rs``) *degrades silently* — when
    the recorded ``thumbnail_hash`` names a blob that is not in the store it
    serves the original bytes and 200s. So a client that derives a thumbnail,
    stamps its hash on the primary sidecar, and then drops the bytes looks
    completely healthy on a status check. Only comparing the two payloads
    catches it. Web did exactly that until 2026-07-22
    (``WasmUploadPayload`` dropped ``payload.thumbnail`` while keeping the hash),
    which is the regression this test pins.

    ``media.md`` § Implementation status — "PublicPost feed-image producer".
    """
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    text = _unique("image-thumb")
    logged_in_app.feed.create_post_with_image(text=text, image_path=str(TEST_IMAGE))
    assert logged_in_app.feed.post_has_image_by_text(text), (
        f"post {text!r} should render its attached image before the thumbnail check: "
        f"post_count={logged_in_app.feed.post_count()} error={logged_in_app.error_text()!r}"
    )
    blob_hash = logged_in_app.feed.post_image_blob_hash_by_text(text)
    assert len(blob_hash) == 64, f"expected a 64-hex blob hash, got {blob_hash!r}"

    def _get(url: str) -> bytes:
        with urllib.request.urlopen(
            urllib.request.Request(url, method="GET"), timeout=10
        ) as resp:
            return resp.read()

    base = f"{nest_instance['url']}/api/v1/blob/{blob_hash}"
    full = _get(base)
    thumb = _get(f"{base}?thumb=1")

    assert thumb != full, (
        "?thumb=1 served the full-size original — the client stamped a "
        "thumbnail_hash on the primary sidecar but never POSTed the thumbnail "
        "blob, so the nest's thumb lookup fell through (this is the silent "
        f"degradation described above). full={len(full)}B thumb={len(thumb)}B"
    )
    assert len(thumb) < len(full), (
        f"the thumbnail ({len(thumb)}B) should be smaller than the "
        f"{len(full)}B original"
    )
    # The shared producer renders thumbnails as JPEG (`render_thumbnail`), and
    # the nest serves the thumb branch as image/jpeg.
    assert thumb[:3] == b"\xff\xd8\xff", (
        f"thumbnail is not a JPEG; first bytes={thumb[:8]!r}"
    )


# Lives in the repo-root `tests/fixtures/` (NOT `tests/e2e-unified/fixtures/`
# — a different directory from the rest of this file's fixtures).
C2PA_TEST_IMAGE = FIXTURE_DIR.parent.parent / "fixtures" / "c2pa-signed.png"


@pytest.mark.web
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("feed-images-and-video")
def test_post_image_shows_c2pa_badge_when_uploaded_with_provenance(logged_in_app, nest_instance):
    """Uploading a C2PA-signed image through the app's own composer makes the
    ``c2pa-badge`` reachable on that post's card.

    Web is both the uploader (browser-side detection at upload,
    ``$lib/c2pa.ts::readProvenanceFromBytes``) and the viewer
    (``C2paImage.svelte``'s own re-check, ``readProvenanceFromUrl``) for the
    same image. tui, linux, apple (macos/ios), and windows upload through the
    same shared `fauna-media` `c2pa-detect` pipeline every native app uses
    (`media.md` § State & data shape), and view by HEADing/GETting the blob
    for the `x-c2pa` wire hint (`fauna_nest_http::NestContentApi::head_has_c2pa`
    on tui/linux, apple's own `APIClient.hasC2paAssertion(hash:)`, windows'
    `BlobImageLoader.LoadWithC2paAsync` reading it off the same authenticated
    GET the post image itself needs — the android `checkBlobC2pa`
    reachable-badge pattern) — a different mechanism per app, the same
    observable badge.

    Web + tui + linux + macos + ios + windows (``@pytest.mark.web`` /
    ``@pytest.mark.tui`` / ``@pytest.mark.linux`` / ``@pytest.mark.macos`` /
    ``@pytest.mark.ios`` / ``@pytest.mark.windows``): every native app already
    computes a real ``has_c2pa`` on upload via `fauna-media`'s `c2pa-detect`
    (`media.md` § State & data shape); web was the one app left running the
    stub until 2026-07-30 (`media.md` § C2PA provenance → *Upload-side
    ``has_c2pa`` population on web*), and tui/linux/macos/ios/windows had no
    `c2pa-badge` wiring on this surface at all until their own builds
    (`ui/media.md` § C2PA provenance, per-app bullets). android computes
    ``has_c2pa`` correctly too but does not (yet) show a *reachable* badge on
    this exact surface — see the per-app notes on the `c2pa-badge` component in
    ui.yaml.

    Mutation proof (non-vacuity): a plain, unsigned image (``test-image.png``,
    used by ``test_post_with_image`` etc.) must NOT show the badge — otherwise
    this would just be asserting "an image post renders something".
    """
    if not C2PA_TEST_IMAGE.exists():
        pytest.skip("c2pa-signed.png fixture not found")
    app = logged_in_app

    plain_text = _unique("plain-image")
    app.feed.create_post_with_image(text=plain_text, image_path=str(TEST_IMAGE))
    assert app.feed.post_has_image_by_text(plain_text), (
        f"post {plain_text!r} should render its attached image: "
        f"error={app.error_text()!r}"
    )
    assert app.driver.is_absent("c2pa-badge", scope="post-card[0]"), (
        "a plain unsigned image must NOT show a c2pa-badge — the badge "
        "would be vacuously true otherwise"
    )

    signed_text = _unique("c2pa-signed")
    app.feed.create_post_with_image(text=signed_text, image_path=str(C2PA_TEST_IMAGE))
    assert app.feed.post_has_image_by_text(signed_text), (
        f"post {signed_text!r} should render its attached image before the "
        f"c2pa-badge check: error={app.error_text()!r}"
    )
    blob_hash = app.feed.post_image_blob_hash_by_text(signed_text)
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/blob/{blob_hash}", method="GET"
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        c2pa_header = resp.headers.get("x-c2pa", "")
    assert c2pa_header == "true", (
        f"wire-level check: the nest should serve x-c2pa: true for a "
        f"c2pa-signed upload (upload-side has_c2pa fix); got {c2pa_header!r}"
    )

    deadline = time.monotonic() + 15
    badge_visible = False
    while time.monotonic() < deadline:
        # The new post prepends to the feed, so the just-created post is
        # post-card[0] (mirrors the plain-image negative check above).
        if app.driver.is_visible("c2pa-badge", scope="post-card[0]"):
            badge_visible = True
            break
        time.sleep(0.5)
    assert badge_visible, (
        "uploading a C2PA-signed image through web compose should surface "
        f"c2pa-badge on its own post card: {app.driver.diagnose('c2pa-badge')} "
        f"error={app.error_text()!r}"
    )


def _blob_c2pa_header(nest_url: str, blob_hash: str) -> str:
    """The raw ``x-c2pa`` header the nest serves for one blob."""
    req = urllib.request.Request(f"{nest_url}/api/v1/blob/{blob_hash}", method="GET")
    with urllib.request.urlopen(req, timeout=10) as resp:
        return resp.headers.get("x-c2pa", "")


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("feed-images-and-video")
def test_a_forged_c2pa_assertion_paints_no_provenance_badge(
    logged_in_app, nest_instance, test_user
):
    """A public image uploaded with ``has_c2pa=true`` and **no manifest in the
    bytes** renders no ``c2pa-badge`` — while a genuinely signed image uploaded
    the same way still does.

    **The defect this pins.** ``has_c2pa`` is the uploader's own assertion: the
    nest stores it for a public-post blob without ever inspecting the bytes
    (``bins/fauna-nest/src/storage/sealed.rs`` — the ``PublicPost`` ingest arm
    checks length and MIME shape only) and serves it back as ``x-c2pa``. An app
    that paints its provenance badge from that header alone shows *this image
    carries provenance* on the strength of a claim anyone can make with a
    modified client or a raw multipart POST. ``docs/goal/ui/media.md`` §
    C2PA provenance has always ruled the other way — "viewing apps parse the
    manifest from the unsealed bytes and correct any displayed badge against
    ground truth" — and this is that rule's witness.

    **Why both halves, and why the API.** The two posts differ in exactly one
    input: the bytes. Same uploader, same ``has_c2pa=true`` sidecar, same
    ``x-c2pa: true`` on the wire (asserted below before either badge is read),
    same post shape, same feed. So a badge on one and not the other can only
    have come from a byte-level verdict — and a client that painted from the
    header would light up both, which is the mutation this pair rejects. The
    forgery goes through ``POST /api/v1/blob`` + ``fauna.posts.create`` rather
    than the composer because that is what the attacker *is*: our own composer
    computes ``has_c2pa`` honestly on-device (``fauna-media``'s ``c2pa-detect``
    pipeline), so the lie is not expressible through the app UI. The journey
    under test is the **viewer's** — the badge read — and that stays entirely in
    the app (e2e-conventions.md conventions 5 and 8).

    **tui, macos, ios and windows so far.** tui is the lead app and the first leg
    of the correction (`ui/media.md` § C2PA provenance, per-app bullets); macos
    and ios followed together, as one shared `FaunaKit` change; windows takes
    its verdict in `PostMediaOpen` (`FaunaApp.Core`) over the bytes the image
    load already holds. linux and android paint from the header still, and web
    has its own JS re-parse whose leg is untested here. Each app's mark joins
    this test as its leg lands — the badge IDs and the assertions are already
    app-agnostic.
    """
    if not C2PA_TEST_IMAGE.exists():
        pytest.skip("c2pa-signed.png fixture not found")
    app = logged_in_app
    port = nest_instance["port"]
    nest_url = nest_instance["url"]

    # A plain PNG with no C2PA manifest, made unique per run by an ancillary
    # chunk: blob storage is content-addressed and `put_blob_metadata` is an
    # INSERT OR IGNORE, so a hash some earlier test already uploaded would keep
    # ITS `has_c2pa` and the forgery below would silently not take.
    marker = f"forged-c2pa-{uuid.uuid4().hex}".encode()
    plain_png = _png_with_chunk(TEST_IMAGE.read_bytes(), b"tEXt", b"fauna\x00" + marker)
    signed_png = C2PA_TEST_IMAGE.read_bytes()

    # Both uploaded claiming provenance. Only the second one is telling the
    # truth, and the nest cannot tell the difference — that is the premise.
    forged_hash = upload_blob(
        port, test_user["token"], plain_png,
        audience_class="PublicPost", mime="image/png", has_c2pa=True,
    )
    honest_hash = upload_blob(
        port, test_user["token"], signed_png,
        audience_class="PublicPost", mime="image/png", has_c2pa=True,
    )
    assert forged_hash != honest_hash, "the two uploads must be different blobs"

    # Wire-level: the claim really did reach the header on BOTH. Without this
    # the negative assertion below would pass for the boring reason that the
    # forgery never landed.
    assert _blob_c2pa_header(nest_url, forged_hash) == "true", (
        "the nest must serve x-c2pa: true for the forged upload — it stores the "
        "uploader's has_c2pa unchecked, which is what makes the viewer-side "
        "correction necessary in the first place"
    )
    assert _blob_c2pa_header(nest_url, honest_hash) == "true", (
        "the genuinely signed upload must also serve x-c2pa: true"
    )

    forged_text = _unique("forged-provenance")
    honest_text = _unique("honest-provenance")
    now_us = int(time.time() * 1_000_000)
    for offset, (text, blob_hash, data) in enumerate(
        ((forged_text, forged_hash, plain_png), (honest_text, honest_hash, signed_png))
    ):
        ws_api.create_post(
            port,
            test_user,
            sign_and_encode_post(
                test_user["signing_key"],
                now_us + offset,
                text,
                media_items=[media_item(blob_hash, "image/png", len(data))],
            ),
        )

    # Both posts were created behind the app's back, so the feed has to be
    # re-queried: a plain `navigate()` is a no-op on an app already showing it.
    app.feed.reenter()
    for text in (forged_text, honest_text):
        assert app.feed.wait_for_post_text(text), (
            f"post {text!r} should reach the feed: error={app.error_text()!r}"
        )
        assert app.feed.post_has_image_by_text(text), (
            f"post {text!r} should render its attached image before any badge "
            f"is read — a card with no image would make both badge assertions "
            f"vacuous: error={app.error_text()!r}"
        )

    honest_index = app.feed.post_index_by_text(honest_text)
    forged_index = app.feed.post_index_by_text(forged_text)
    assert honest_index >= 0 and forged_index >= 0, (
        f"both cards must be locatable: honest={honest_index} forged={forged_index}"
    )

    # The positive half first: the badge is reachable on this surface at all,
    # so "no badge" below means the verdict said no — not that the feature is
    # missing, the build dropped `c2pa-detect`, or the card never rendered.
    deadline = time.monotonic() + 15
    honest_badge = False
    while time.monotonic() < deadline:
        if app.driver.is_visible("c2pa-badge", scope=f"post-card[{honest_index}]"):
            honest_badge = True
            break
        time.sleep(0.5)
    assert honest_badge, (
        "a genuinely C2PA-signed image must still show c2pa-badge: "
        f"{app.driver.diagnose('c2pa-badge')} error={app.error_text()!r}"
    )

    # …and the forged one must not, on a feed where the check has demonstrably
    # already run to completion for a sibling card. The read is
    # ``is_visible_scrolled``, not a bare ``is_visible``: windows reads UIA
    # ``IsOffscreen``, and the forged card sits BELOW the honest one, so a badge
    # painted there but scrolled out of the viewport reads "not visible" and a
    # bare negative passes vacuously (measured 2026-09-21: this test stayed green
    # against a build that painted the badge from the header alone). Scrolling the
    # card in first turns "absent" and "one scroll away" into different answers;
    # it never masks a genuinely absent badge.
    assert not app.driver.is_visible_scrolled(
        "c2pa-badge", scope=f"post-card[{forged_index}]"
    ), (
        "an image with no C2PA manifest must NOT show a provenance badge, "
        "however loudly its uploader asserted has_c2pa — the badge is the "
        "viewer's verdict over the bytes, not the uploader's word "
        f"(ui/media.md § C2PA provenance): {app.driver.diagnose('c2pa-badge')}"
    )


@pytest.mark.feature("feed-images-and-video")
def test_post_image_survives_a_blob_gc_sweep(logged_in_app, nest_instance):
    """A picture attached through the composer is still served after the nest's
    blob garbage collector runs with no grace period.

    The composer uploads the photo through `POST /api/v1/blob`, which records
    the blob's `blob_metadata` row and nothing else; the only thing that names
    the blob afterwards is the signed post. Until 2026-09-08 the GC's reference
    walk had no post-derived source, so every photo on every post was swept on
    the first pass past the 30-minute grace and the post rendered a 404 image
    forever (`docs/goal/behavior/backup-restore.md` § 9 step 2, the *live
    signed records* arm). No e2e nest lives long enough to see a 6-hourly
    sweep, so this drives the sweep directly: `fauna.admin.gc` with
    `grace_period_secs: 0` — "reclaim everything unreferenced right now" —
    against the real nest the real app just posted to, then re-fetches the
    picture the post names. The mutant proof (removing the arm reds this)
    lives in the nest's `gc.rs` unit tests; this is the user-shaped witness.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    text = _unique("image-gc")
    logged_in_app.feed.create_post_with_image(text=text, image_path=str(TEST_IMAGE))
    assert logged_in_app.feed.post_has_image_by_text(text), (
        f"post {text!r} should render its attached image before the sweep: "
        f"post_count={logged_in_app.feed.post_count()} error={logged_in_app.error_text()!r}"
    )
    blob_hash = logged_in_app.feed.post_image_blob_hash_by_text(text)
    assert len(blob_hash) == 64, f"expected a 64-hex blob hash, got {blob_hash!r}"

    admin = nest_instance["admin"]
    admin_ws = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        reply = admin_ws.call("fauna.admin.gc", {"grace_period_secs": 0, "dry_run": False})
    assert reply.get("dry_run") is False, f"the sweep must really run, got {reply!r}"

    # GET /api/v1/blob/{hash} is public (no bearer). A swept blob is a 404.
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/blob/{blob_hash}", method="GET"
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = resp.read()
    except urllib.error.HTTPError as e:
        raise AssertionError(
            f"the GC swept a live post's photo: GET /api/v1/blob/{blob_hash} -> "
            f"{e.code} after fauna.admin.gc replied {reply!r}"
        ) from e
    assert body[:8] == PNG_MAGIC, "the served bytes are not the posted PNG"
    assert logged_in_app.feed.post_has_image_by_text(text), (
        f"post {text!r} lost its image after the sweep: error={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("feed-images-and-video")
def test_a_picture_is_uploaded_when_you_post_so_writing_time_never_loses_it(
    logged_in_app, nest_instance,
):
    """A picture attached while writing is still on the post however long the
    writing took (`docs/goal/ui/media.md` § Encryption at rest: every composer
    holds the picked bytes locally and uploads at submit).

    The nest pins a blob only once a stored post names it; until then only the
    creation grace stands between an upload and the sweep, so a composer that
    uploaded at pick would lose the photo of any post written for longer than
    the grace — silently, since the publish itself succeeds. Waiting out the
    grace is not a test, so this runs the sweep with NO grace between the attach
    and the post: `fauna.admin.gc` with `grace_period_secs: 0` reclaims every
    unpinned blob, which is exactly what the grace ending mid-write does. The
    picture must then upload with the post and paint.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    app = logged_in_app
    text = _unique("image-written-slowly")
    admin = nest_instance["admin"]
    sweeps: list[dict] = []

    def sweep_while_writing() -> None:
        admin_ws = WsRpcAdminClient(
            nest_instance["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            sweeps.append(
                admin_ws.call("fauna.admin.gc", {"grace_period_secs": 0, "dry_run": False})
            )

    app.feed.create_post_with_image(
        text=text, image_path=str(TEST_IMAGE), before_submit=sweep_while_writing,
    )
    assert sweeps and sweeps[0].get("dry_run") is False, (
        f"the sweep between attach and post must really run, got {sweeps!r}"
    )
    assert app.feed.post_image_painted_by_text(text), (
        f"post {text!r} was written across a no-grace sweep and its picture did "
        f"not paint — an upload at attach time was swept before the post named it: "
        f"{app.feed.post_image_paint_report()} error={app.error_text()!r}"
    )
    blob_hash = app.feed.post_image_blob_hash_by_text(text)
    with urllib.request.urlopen(
        f"{nest_instance['url']}/api/v1/blob/{blob_hash}", timeout=10
    ) as resp:
        assert resp.read()[:8] == PNG_MAGIC, "the served bytes are not the posted PNG"

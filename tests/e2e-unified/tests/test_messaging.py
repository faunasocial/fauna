"""Conversations list chrome + new-thread compose, on the unified
(post-convergence) conversations model.

These tests previously exercised the pre-convergence UI — a `new-dm-button`
modal DM dialog and a separate Groups page (`group-item`, `group-message`,
`view-mode-toggle`). That model was removed per
`docs/goal/ui/conversations.md` ("Don't reintroduce a groups page. Group
threads live on this page with flavor MlsGroup"), so the tests are rewritten
against the canonical `ConversationsActions` layer + canonical ui.yaml IDs.

Coverage that moved elsewhere:
- MLS group create / membership / rename → `test_thread_membership.py`,
  `test_thread_rename.py`.
- Recipient resolution + multi-recipient group compose → `test_recipient_picker.py`.
- Per-rail message send/display → the rails track
  (tracked internally); linux's thread compose
  `on_send` is a no-op until that lands, so a UI-send assertion can't be green
  yet and is not asserted here.

Clients still on the old model (web/android, mid-convergence) fail these
canonical assertions until they converge — that is the intended signal.
"""
import re
import secrets

import pytest

from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


def test_navigate_to_conversations(logged_in_app):
    """Conversations page loads with the canonical new-conversation-button."""
    logged_in_app.conversations.navigate()
    assert logged_in_app.driver.is_visible("new-conversation-button"), (
        "conversations page should load with the new-conversation-button: "
        f"{logged_in_app.driver.diagnose('new-conversation-button')}"
    )


@pytest.mark.feature("find-a-conversation")
def test_conversation_search_visible(logged_in_app):
    """The conversation search box is visible on the list pane."""
    logged_in_app.conversations.navigate()
    assert logged_in_app.driver.is_visible("conversation-search-box"), (
        "the conversation list pane should show the search box: "
        f"{logged_in_app.driver.diagnose('conversation-search-box')}"
    )


def test_conversation_sort_button(logged_in_app):
    """The conversation sort toggle is visible on the list pane."""
    logged_in_app.conversations.navigate()
    assert logged_in_app.driver.is_visible("conversation-sort"), (
        "the conversation list pane should show the sort toggle: "
        f"{logged_in_app.driver.diagnose('conversation-sort')}"
    )


# The ratified cycle (`conversations.md` § Where logic lives -> Thread-list sort cycle), in
# the serde spelling `data.conversation_sort` publishes.
_SORT_CYCLE = {"LatestActivity": "OldestFirst", "OldestFirst": "Unread", "Unread": "LatestActivity"}


@pytest.mark.feature("find-a-conversation")
def test_conversation_sort_button_cycles_three_orders_without_error(logged_in_app):
    """Each tap advances latest-activity -> oldest-first -> unread -> latest-activity
    (`docs/goal/ui/conversations.md` § Where logic lives -> Thread-list sort cycle), and the
    rows land in the order the tap selected: newest first, then oldest first, then unread
    threads first (newest first within each group), and three taps return home.

    Until 2026-09-21 this tapped three times and asserted only "no error", which a button
    that did nothing at all satisfied. Three threads of known age are seeded and the list
    is filtered to them, so the rows read are exactly this test's and their order is
    fully determined. Which order is active comes from ``data.conversation_sort``.

    **Exactly one thread is left unread — the oldest.** All three arrive unread, and with
    every thread unread (as with none) the unread order renders exactly like
    latest-activity, so a witness that seeded nothing further would pass without proving
    unread-first at all. The two newer threads are therefore read the way a user reads
    them — opened through the list (``conversations.md`` § State & data shape -> *When a
    thread is read*) — which makes the unread order ``(0, 2, 1)``: the one ordering of
    these rows that neither of the other two orders produces.
    """
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    tag = "sort" + secrets.token_hex(3)
    subjects = [f"{tag} first", f"{tag} second", f"{tag} third"]  # oldest -> newest
    seeded = [
        conv.inject_and_resolve_thread(
            rail="Smtp", sender=f"s{i}-{tag}@host.test", subject=s, body=f"{s} arrived"
        )
        for i, s in enumerate(subjects)
    ]
    for thread_id in seeded[1:]:
        conv.open_thread_by_id(thread_id)

    def unread_by_age() -> tuple:
        counts = {t.thread_id: t.unread_count for t in conv.list_threads()}
        return tuple(counts.get(thread_id, 0) for thread_id in seeded)

    wait_until(
        lambda: unread_by_age() == (1, 0, 0),
        UI_SETTLE_S,
        diagnose=lambda: "only the oldest seeded thread should still be unread — the two "
        f"newer ones were opened; unread counts oldest -> newest are {unread_by_age()}",
    )
    conv.navigate()
    conv.search_conversations(tag)
    wait_until(
        lambda: driver.count("conversation-item") == 3,
        UI_SETTLE_S,
        diagnose=lambda: f"the three seeded threads never filtered in: "
        f"{driver.diagnose('conversation-item')}",
    )

    def rendered() -> tuple:
        """The seeded threads' ages (0 = oldest), in the order the rows show them —
        read off each row's own snippet (``dm-subject`` scoped within its
        ``conversation-item``, the per-row text every app ids), which carries the
        subject the body repeats."""
        rows = []
        for i in range(driver.count("conversation-item")):
            text = driver.get_text("dm-subject", scope=f"conversation-item[{i}]")
            rows.append(next((n for n, s in enumerate(subjects) if s in text), text))
        return tuple(rows)

    def expected(order: str) -> tuple:
        if order == "LatestActivity":
            return (2, 1, 0)
        if order == "OldestFirst":
            return (0, 1, 2)
        return (0, 2, 1)

    def active():
        return driver.get_state("data.conversation_sort")

    start = active()
    assert start in _SORT_CYCLE, (
        f"the app publishes no active sort order (data.conversation_sort={start!r}); "
        "the shared `fauna_conversations::state_json::conversation_sort_json` is the one "
        "line an app's state builder needs"
    )
    order = start
    for _ in range(3):
        driver.click("conversation-sort")
        order = _SORT_CYCLE[order]
        wait_until(
            lambda: active() == order and rendered() == expected(order),
            UI_SETTLE_S,
            diagnose=lambda: f"after a tap the list should be in {order} order "
            f"{expected(order)} (ages, 0 = oldest); it reports {active()!r} and shows "
            f"{rendered()}",
        )
        assert not logged_in_app.has_error(), (
            "cycling conversation-sort should not raise: "
            f"error={logged_in_app.error_text()!r} "
            f"{logged_in_app.driver.diagnose('conversation-sort')}"
        )
    assert order == start, "three taps must return the list to the order it started in"
    conv.search_conversations("")


@pytest.mark.feature("find-a-conversation")
def test_conversation_row_shows_unread_until_its_thread_is_opened(logged_in_app):
    """A list row carries ``dm-unread-indicator`` while its thread holds a message nobody
    has opened it over, and loses it once the thread is opened — and opening one thread
    reads no other (``docs/goal/ui/conversations.md`` § State & data shape -> *When a
    thread is read*).

    Two threads are seeded and the list is filtered to them. The indicator is read scoped
    within each thread's own ``conversation-item`` (it is absent, not hidden, on a read
    row, so an unscoped index would silently name a different row), and the row is found
    by the subject its snippet repeats, never by position. Its text is not asserted: the
    spec element is a dot, and an app may paint the count on it.
    """
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    tag = "unread" + secrets.token_hex(3)
    opened_subject, control_subject = f"{tag} opened", f"{tag} control"
    opened = conv.inject_and_resolve_thread(
        rail="Smtp", sender=f"o-{tag}@host.test", subject=opened_subject,
        body=f"{opened_subject} arrived",
    )
    control = conv.inject_and_resolve_thread(
        rail="Smtp", sender=f"c-{tag}@host.test", subject=control_subject,
        body=f"{control_subject} arrived",
    )

    def show_the_two_rows():
        conv.navigate()
        conv.search_conversations(tag)
        wait_until(
            lambda: driver.count("conversation-item") == 2,
            UI_SETTLE_S,
            diagnose=lambda: "the two seeded threads never filtered in: "
            f"{driver.diagnose('conversation-item')}",
        )

    def indicators() -> dict:
        """subject -> whether that thread's row paints the indicator."""
        out = {}
        for i in range(driver.count("conversation-item")):
            scope = f"conversation-item[{i}]"
            text = driver.get_text("dm-subject", scope=scope)
            for subject in (opened_subject, control_subject):
                if subject in text:
                    out[subject] = driver.count("dm-unread-indicator", scope=scope) > 0
        return out

    def unread() -> dict:
        counts = {t.thread_id: t.unread_count for t in conv.list_threads()}
        return {"opened": counts.get(opened), "control": counts.get(control)}

    show_the_two_rows()
    wait_until(
        lambda: indicators() == {opened_subject: True, control_subject: True},
        UI_SETTLE_S,
        diagnose=lambda: "both seeded threads arrived unopened, so both rows should paint "
        f"dm-unread-indicator; rows paint {indicators()}, shared state says {unread()} "
        f"{driver.diagnose('dm-unread-indicator')}",
    )

    conv.open_thread_by_id(opened)
    show_the_two_rows()
    wait_until(
        lambda: indicators() == {opened_subject: False, control_subject: True},
        UI_SETTLE_S,
        diagnose=lambda: "opening a thread should clear its row's dm-unread-indicator and "
        f"leave the other row's alone; rows paint {indicators()}, shared state says "
        f"{unread()} {driver.diagnose('dm-unread-indicator')}",
    )
    assert unread() == {"opened": 0, "control": 1}, (
        f"the published unread counts disagree with the rows: {unread()}"
    )
    conv.search_conversations("")


def test_conversation_item_count(logged_in_app):
    """conversation-item is queryable (the list may legitimately be empty)."""
    logged_in_app.conversations.navigate()
    assert logged_in_app.driver.count("conversation-item") >= 0, (
        "conversation-item should be queryable (count never negative): "
        f"{logged_in_app.driver.diagnose('conversation-item')}"
    )


@pytest.mark.feature("conversations")
def test_new_conversation_compose_opens(logged_in_app):
    """new-conversation-button opens the in-pane recipient picker + compose
    (no modal dialog, per conversations.md § Layout & flow)."""
    logged_in_app.conversations.navigate()
    logged_in_app.driver.click("new-conversation-button")
    logged_in_app.driver.wait_for("recipient-picker-input")
    logged_in_app.driver.wait_for("dm-text-field")
    logged_in_app.driver.wait_for("dm-send-button")


@pytest.mark.feature("message-formatting")
def test_markdown_toolbar_visible(logged_in_app):
    """The markdown toolbar renders inside the compose bar, revealed by the
    canonical new-conversation-button (in-pane compose)."""
    logged_in_app.conversations.navigate()
    logged_in_app.driver.click("new-conversation-button")
    logged_in_app.driver.wait_for("markdown-bold-button")


@pytest.mark.feature("find-a-conversation")
def test_conversation_row_shows_when_it_last_moved(logged_in_app):
    """A list row carries ``conversation-item-timestamp`` — its thread's last-activity
    time through the shared ``conversation_timestamp_display`` bucketer
    (``docs/goal/ui/conversations.md`` § Layout & flow; ``value-formatting.md``
    § Conversation timestamp: "apps must not hand-roll it").

    The seeded thread's only message is stamped at "now", so the row lands in the
    today bucket and reads the shared 24h local clock ``HH:MM`` whatever the host
    timezone. The time is read scoped within the seeded thread's own
    ``conversation-item``, found by the subject its snippet repeats, never by position.
    """
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    tag = "moved" + secrets.token_hex(3)
    subject = f"{tag} seeded"
    conv.inject_and_resolve_thread(
        rail="Smtp", sender=f"m-{tag}@host.test", subject=subject,
        body=f"{subject} arrived",
    )

    conv.navigate()
    conv.search_conversations(tag)
    wait_until(
        lambda: driver.count("conversation-item") == 1,
        UI_SETTLE_S,
        diagnose=lambda: "the seeded thread never filtered in: "
        f"{driver.diagnose('conversation-item')}",
    )
    scope = "conversation-item[0]"
    assert subject in driver.get_text("dm-subject", scope=scope), (
        f"the one filtered row is not the seeded thread: {driver.diagnose('dm-subject')}"
    )
    assert driver.count("conversation-item-timestamp", scope=scope) == 1, (
        "the row should paint its last-activity time as conversation-item-timestamp: "
        f"{driver.diagnose('conversation-item-timestamp')}"
    )
    ts = driver.get_text("conversation-item-timestamp", scope=scope).strip()
    assert re.fullmatch(r"\d{2}:\d{2}", ts), (
        f"row timestamp {ts!r} is not the shared today-bucket HH:MM clock (the app "
        "must format ThreadSummary.last_activity_ms through conversation_timestamp_display)"
    )

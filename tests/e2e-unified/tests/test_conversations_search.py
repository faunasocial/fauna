"""Conversation thread-list search filters the list.

Goal doc: ``conversations.md`` § User actions — ``conversation-search-box``
"Filter list" → ``manager.set_search_query(text)``, and § Where logic lives
(thread-list sort + search filtering live in the manager's ``snapshot()``).

The filter lives in shared Rust: ``ConversationsManager::snapshot()`` filters
``threads`` by the active query — a case-insensitive substring over each
thread's ``label`` + ``snippet`` — so every app renders the already-filtered
list (priority #3). The *logic* is covered by the shared-Rust unit test
``manager_integration_tests::set_search_query_filters_thread_list_by_label_and_snippet``;
this e2e proves each app wires its ``conversation-search-box`` →
``set_search_query`` → re-render correctly (the wiring a unit test can't reach).

Marked for the apps whose leg has landed (linux wires the box; web/android
are dumb renderers of ``snapshot().threads``; windows drives the box →
``set_search_query`` and renders the manager-filtered ``snapshot().threads``,
dropping its old client-side ``.Where()`` filter). apple adds its marker when its
leg lands — the same staged pattern the other conversation lifts use.

Seeded via the ``inject_inbound_for_test`` seam (subject → ``ThreadSummary.label``,
body → ``snippet``) — no mail-enabled login required.
"""

import secrets

import pytest

from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.android, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios, pytest.mark.tui]


# Every needle this test searches for is namespaced by a per-run TAG, and every thread
# it seeds is keyed by it. That is what makes the exact match counts below sound: the
# thread list is SESSION-scoped and accumulates every other test's threads, so a bare
# needle ("q4", "budget") would match whatever else happened to run first, and a bare
# `count(...) == 3` would assert on the whole accumulated list rather than on this
# test's own three threads. (`testing.md` § Cross-app e2e conventions: scoped
# queries, never global counts.)
TAG = "srch" + secrets.token_hex(3)


def _seed_three_threads(conv):
    # subject → ThreadSummary.label, body → ThreadSummary.snippet.
    return [
        conv.inject_and_resolve_thread(
            rail="Smtp",
            sender=f"alice-{TAG}@host.test",
            subject=f"{TAG}q4 {TAG}budget",
            body="spreadsheet attached",
        ),
        conv.inject_and_resolve_thread(
            rail="Smtp",
            sender=f"bob-{TAG}@host.test",
            subject=f"{TAG}lunch plans",
            body="tacos at noon",
        ),
        conv.inject_and_resolve_thread(
            rail="Smtp",
            sender=f"carol-{TAG}@host.test",
            subject=f"{TAG}vacation",
            body=f"see the {TAG}budget doc",
        ),
    ]


@pytest.mark.feature("find-a-conversation")
def test_search_filters_thread_list(logged_in_app):
    """Typing in ``conversation-search-box`` filters the visible thread list to
    threads whose label or snippet contains the query (case-insensitive);
    clearing it restores the full list."""
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    seeded = _seed_three_threads(conv)
    conv.navigate()

    # Our three threads are present — asserted by IDENTITY, since the list also holds
    # every thread earlier tests in this run left behind. The unfiltered total is the
    # baseline the cleared query must restore; it is not 3.
    wait_until(
        lambda: {t.thread_id for t in conv.list_threads()} >= set(seeded),
        UI_SETTLE_S,
        diagnose=lambda: "the three seeded threads never rendered before filtering",
    )
    unfiltered = driver.count("conversation-item")

    # Matches a LABEL only: our q4 token → just the budget thread.
    conv.search_conversations(f"{TAG}q4")
    wait_until(
        lambda: driver.count("conversation-item") == 1,
        UI_SETTLE_S,
        diagnose=lambda: "label match never filtered to one thread",
    )

    # Matches LABEL + SNIPPET: our budget token hits the first thread's label and the
    # third thread's "see the ... doc" snippet → two threads.
    conv.search_conversations(f"{TAG}budget")
    wait_until(
        lambda: driver.count("conversation-item") == 2,
        UI_SETTLE_S,
        diagnose=lambda: "label+snippet match never filtered to two threads",
    )

    # Clearing the box restores the full (unfiltered) list.
    conv.search_conversations("")
    wait_until(
        lambda: driver.count("conversation-item") == unfiltered,
        UI_SETTLE_S,
        diagnose=lambda: "cleared query never restored the full thread list",
    )


@pytest.mark.feature("find-a-conversation")
def test_search_finds_a_term_past_the_visible_snippet(logged_in_app):
    """Search matches the latest message's FULL text, so a word past what the row shows
    still finds the thread (``conversations.md`` § Where logic lives -> Thread-list sort +
    search filtering: "deliberately not the bounded ``snippet``, so a term past what the
    row shows still finds the thread").

    The body runs well past the snippet's 512-byte bound with the needle at its very
    end, and the test first proves the premise from the published row (the snippet does
    not contain the needle) — without that, a match could have come from the snippet and
    this would witness nothing the label/snippet test above does not.
    """
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    needle = f"{TAG}tailword"
    filler = " ".join(f"line{n:03d} of a long mail about the offsite plans" for n in range(40))
    # Opens with the bare TAG so the matching row's snippet names this run's thread;
    # the needle glues a suffix onto it, so the snippet never contains the needle.
    body = f"{TAG} {filler} {needle}"
    assert len(body.encode()) > 1024, "the body must run far past the snippet bound"
    thread_id = conv.inject_and_resolve_thread(
        rail="Smtp",
        sender=f"dana-{TAG}@host.test",
        subject=f"{TAG} offsite",
        body=body,
    )
    conv.navigate()
    row = next(t for t in conv.list_threads() if t.thread_id == thread_id)
    assert row.snippet and needle not in row.snippet, (
        f"premise: the needle must lie past the row's snippet, which reads {row.snippet!r}"
    )

    conv.search_conversations(needle)
    wait_until(
        lambda: driver.count("conversation-item") == 1,
        UI_SETTLE_S,
        diagnose=lambda: "a term past the visible snippet never found its thread: "
        f"{driver.diagnose('conversation-item')}",
    )
    shown = driver.get_text("dm-subject", scope="conversation-item[0]")
    assert shown.startswith(f"{TAG} line000"), (
        f"the one match must be the long mail's thread, not another: {shown!r}"
    )
    conv.search_conversations("")

"""tier_3: a feed page error surfaces through ``error_text()``/``has_error()``,
not just on screen (``docs/goal/architecture/e2e-conventions.md`` convention 2's
rider) — the apple leg.

Apple's page-level error mechanism is the shared FaunaKit ``ErrorBanner``: it
paints the message AND publishes it into ``AppMessages.error`` on
``.onAppear``, which is what `error_text()`'s state-protocol read
(``messages.error``, unconditionally served by both apple targets) actually
resolves. ``Fauna-iOS/Views/Feed/FeedListView.swift`` rendered its
``vm.errorMessage`` into a hand-rolled ``Text(...).accessibilityIdentifier(
"error-message")`` instead — correctly conditional (the id left the tree with
no error) but never routed through ``ErrorBanner``, so every feed error read
back as ``""`` to `error_text()` despite being genuinely on screen. Fixed by
swapping to ``ErrorBanner(message:)``, mirroring macOS's own
``MacFeedDetailView`` (which already used it for the same ``vm.errorMessage``
field).

There is no *product* path that fails a feed fetch on demand — a real failure
needs the nest's own ``fauna.feed.local.posts``/``fauna.feed.*.posts`` query to
error — so this drives ``FeedManager::inject_error_for_test``, the feed twin of
``ConversationsManager::inject_page_error_for_test`` (``feed_inject_error``
test-agent command). tui wired the same command 2026-08-21 (a marker-set audit found this test silently deselecting tui with no
recorded reason; the seam it needed already existed in shared Rust, so this
was a wiring gap, not a missing feature). Web followed in its catalog
trickle-down pass the same way: the page already derived ``error-message``
from ``FeedSnapshot.error``, and only the wasm seam (``injectErrorForTest``)
and the ``feed_inject_error`` command were missing.
"""

import time

import pytest

# Generous, named, and latency-independent (e2e-conventions.md convention 14):
# the assertion is on *state* — the label eventually carries the message —
# never on how fast a loaded box got there. A green run pays only the poll
# interval; the ceiling exists solely so a genuinely broken surface fails in
# bounded time rather than riding the 900s per-test timeout.
ERROR_SURFACE_BUDGET_S = 60.0


@pytest.mark.tier_3
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.feature("feed-read")
def test_feed_error_surfaces_on_error_text(logged_in_app):
    app = logged_in_app

    # Land on the feed page explicitly: not every app's post-login landing is
    # the feed (web's is Conversations), and the injected error only surfaces
    # on the page that renders the feed snapshot.
    app.feed.navigate()

    # A clean feed page surfaces no error.
    assert not app.driver.is_visible("error-message"), (
        "feed page must start with no error surfaced"
    )
    assert app.error_text() == "", (
        f"error_text() must start empty, got {app.error_text()!r}"
    )

    message = "nest rejected fauna.feed.local.posts (feed error test)"
    app.feed.inject_error_for_test(message=message)

    # The injection stamps `FeedSnapshot.error` and notifies observers; the
    # page's error element renders on the deferred UI-thread refresh the
    # command return races, so poll for it (condition-based waiting, per the
    # action layer) rather than assuming the very next read sees it.
    deadline = time.time() + ERROR_SURFACE_BUDGET_S
    while time.time() < deadline:
        if app.driver.is_visible("error-message"):
            break
        time.sleep(0.1)

    assert app.driver.is_visible("error-message"), (
        "an injected feed error must surface on the page error-message "
        f"element; it stayed hidden. error_text={app.error_text()!r}"
    )

    # The regression this test exists to pin: `error_text()` reads the state
    # protocol's `messages.error`, which apple serves unconditionally — a page
    # that paints its own error WITHOUT publishing into `AppMessages` looks
    # correct on screen (`is_visible` above passes) yet reads back as "" here,
    # exactly the gap `FeedListView`'s raw `Text` left.
    shown = app.error_text()
    assert message in shown, (
        f"error_text() must carry the injected message (got {shown!r}); a "
        "page-local error element that never publishes into AppMessages "
        "reads back empty even while genuinely on screen"
    )
    assert app.has_error(), "has_error() must agree with a non-empty error_text()"

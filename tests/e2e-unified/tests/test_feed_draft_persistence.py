"""tier_3: a half-written feed post survives an app restart — the ``posts`` rail
of draft-persistence v2 (``docs/goal/behavior/reserved-folders.md`` § Drafts
Sync + ``docs/goal/ui/feed.md`` § Persistence).

The sibling of ``test_conversations_draft_persistence.py``, one rail over. The
at-rest shape is proven tier_1 in ``fauna-feed`` (``drafts.rs``) and the nest
``__drafts`` plane tier_3 in ``tests/api/test_drafts_sync.py``; what THIS proves
is the *app wiring* end to end — a compose change reaches ``__drafts`` via the
debounced ``fauna.drafts.put``, and the next launch restores it via
``fauna.drafts.get`` into the composer the user actually looks at.

**All seven apps now.** tui is the lead app and landed first
(``apps/fauna-tui/src/feed/drafts.rs``); linux landed second
(``apps/fauna-linux/src/feed/drafts.rs``, 2026-08-19); android third
(``FeedManagerHost.kt`` + ``ApiClient.kt``, 2026-08-20); web fourth (``$lib/feed.ts`` + ``routes/feed/+page.svelte``,
2026-08-21); macos fifth (shared FaunaKit
``FeedVM.configure``, 2026-08-25) — the same
``FeedVM`` change covers iOS too, and iOS is e2e-proven from this pass on too
 (the code landed with macOS's; only the marker was
missing). The android leg is **build-verified only** (``compileDebugKotlin`` +
``compileDebugUnitTestKotlin`` on the primary dev VM) — the marker lands with
the code per android's standing constraint (android e2e needs the ``host``
emulator, which the primary dev VM does not have; the same shape as
``test_feed_tips.py``'s owed run). windows landed last (``FeedDraftsService``,
2026-08-26), closing
``feed.md``'s § Implementation status today gap — exactly how the
conversations twin grew to seven.

**Why the top-level composer and not a reply draft:** the posts rail is a single
slot by design (``feed.md`` § Persistence — the composer is one surface, so there
are no draft ids), and the reply composer is a transient dialog holding no
persisted state. The top-level compose bar is therefore the whole rail.

The restart uses the DEFAULT fresh-store relaunch, never
``preserve_state_across_relaunch()``: the draft has to come back *from the nest*,
and client-local state surviving the relaunch would make every assertion here
pass vacuously.

**RED-verified at introduction (2026-08-15), not merely green.** With the restore
half mutated out of the tui leg (``restore_on_launch`` dropping its
``manager.restore_drafts(bytes)``) this test fails at step 5 and *only* there —
both preconditions still pass, so the save-side assertion is independently
exercised — reporting ``expected '…restart 5193', got ''``. That is the shape a
regression here must produce: an EMPTY composer, never a stale value.
"""

import time

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    # The debounce itself must land this draft — see the marker's entry in
    # pytest.ini. Keeps a `drafts_autosave_window_ms` run from silently
    # recording this as a product red.
    pytest.mark.drafts_production_window,
]

#: The rail this test drives — one of the three frozen constants on the wire
#: (``fauna_protocol::drafts::DRAFT_RAILS``). Named once so a typo cannot make
#: the side-channel read a rail the app never writes and time out looking honest.
RAIL = "posts"


def _wait_compose_body(app, expected: str, timeout: float = 10.0) -> str:
    """Poll the composer body until it equals ``expected`` (or time out). The
    snapshot refresh after a mutator is async, so read condition-based rather
    than once. Returns the last value read, for the failure message."""
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = app.feed.compose_body_text()
        if last == expected:
            return last
        time.sleep(0.1)
    return last


def _wait_drafts_changed(node_url, actor_id, signing_key, baseline_blob, timeout: float = 20.0):
    """Poll the nest ``__drafts`` plane (via a fresh side-channel device for the
    same actor) until the debounced ``fauna.drafts.put`` lands — i.e. the blob
    appears or changes from ``baseline_blob``. Returns the observed blob, or
    ``None`` on timeout.

    Deterministic save-confirmation rather than a settle-sleep: the budget is a
    generous ceiling that a green run never pays (convention 14), and it anchors
    on the observable that *is* the persistence contract — the nest's own blob —
    instead of guessing how long the 1.5 s debounce plus a WS round trip takes on
    a loaded box."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    deadline = time.time() + timeout
    while time.time() < deadline:
        with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
            blob = dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")
        if blob is not None and blob != baseline_blob:
            return blob
        time.sleep(0.5)
    return None


@pytest.mark.feature("drafts-survive")
def test_feed_compose_draft_survives_app_restart(logged_in_app, nest_instance, test_user):
    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    draft_body = "unfinished post that must survive a restart 5193"

    # 0. Baseline the rail's blob so step 2 can tell "the save landed" from
    #    "something was already there".
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        baseline_blob = dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")

    # 1. Type a post and DON'T submit it. Each edit forwards to the shared
    #    FeedManager and schedules the debounced `fauna.drafts.put`.
    app.feed.navigate()
    app.feed.open_composer()
    app.driver.type_text("compose-text-field", draft_body)
    assert _wait_compose_body(app, draft_body) == draft_body, (
        "precondition: the draft must be typed into the composer before the restart"
    )

    # 2. Wait for the save to actually reach the nest before tearing the app
    #    down — a restart mid-put would drop the draft with nothing to restore,
    #    and the assertion at step 5 would then be testing the wrong thing.
    saved_blob = _wait_drafts_changed(node_url, actor_id, signing_key, baseline_blob)
    assert saved_blob is not None, (
        "precondition: the debounced fauna.drafts.put must reach the nest __drafts "
        f"plane at path={RAIL!r} before the restart (save-side persistence did not land)"
    )

    # 3. Force-quit + relaunch, replaying the same identity — the app restart.
    #    In-memory state dies here; only what reached the nest can come back.
    app.driver.hard_reload()

    # 4. Back to the feed. The relaunched session's post-auth hook builds a fresh
    #    FeedManager and its launch restore re-fetches the `__drafts` blob.
    app.feed.navigate()
    app.feed.open_composer()

    # 5. The restored draft must be in the composer. A *value* assertion, not a
    #    non-empty one: a stale draft, an empty composer and a placeholder all
    #    satisfy "non-empty", and this queue has been bitten by that class before.
    restored = _wait_compose_body(app, draft_body)
    assert restored == draft_body, (
        "the feed compose draft did not survive the app restart "
        f"(expected {draft_body!r}, got {restored!r}); error={app.error_text()!r}"
    )

    # 6. Discard the draft this test just proved is DURABLE — the cleanup is part
    #    of the test, not an afterthought. `__drafts` is per-actor nest state on
    #    the session-scoped `test_user`, so a draft left behind is restored into
    #    the NEXT test's composer on every app that wires the posts rail. That is
    #    not hypothetical: the conversations twin shipped exactly this leak and it
    #    broke `test_conversations_new_thread_cancel.py[tui]` with a composer
    #    holding the previous test's body.
    #
    #    Clearing the field is the right instrument, not a side-channel wipe: it
    #    is the user-facing discard, it goes through the UI like every other
    #    mutation here (e2e-conventions.md point 8), and there is no "delete a
    #    rail" RPC — the plane is `fauna.drafts.{get,put}` only.
    #
    #    Asserted, never best-effort: a cleanup that silently no-ops re-arms the
    #    exact cross-test leak this closes, and the next victim would fail far
    #    away with a baffling message instead of here with this one.
    app.driver.clear_and_type("compose-text-field", "")
    assert _wait_drafts_changed(node_url, actor_id, signing_key, saved_blob) is not None, (
        "cleanup: clearing the composer must persist the emptied draft to the nest "
        f"__drafts plane at path={RAIL!r}, or it leaks into the next test's composer. "
        "The assertion above already PASSED — the drafts feature works; this is the "
        "teardown failing, so look at the compose clear path on this app, not at "
        "draft persistence."
    )

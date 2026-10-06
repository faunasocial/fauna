"""tier_3 E2E: the apple leave-flush legs of `drafts-survive` outcome 5
(`docs/goal/behavior/reserved-folders.md` § The leave-flush promise).

Proves a REAL leave-time FLUSH (not merely the process exiting, and not the
debounce landing on its own): types a new-thread conversation draft and leaves
IMMEDIATELY — well inside the debounced `fauna.drafts.put`'s save window
(`test_conversations_draft_persistence.py` polls up to 12s for that debounce +
WS round trip to land) — so if the draft reaches the nest's `__drafts` plane at
all, it can only be via the forced flush the app's own leave door runs. The
linux/windows/tui/web twins are `test_linux_window_close_flush.py`,
`test_windows_window_close_flush.py`, `test_tui_quit_flush.py` and
`test_web_leave_flush.py`; this file is apple's.

**macOS's door is a real QUIT, not a window close.** Closing the window is
deliberately never a quit on macOS — the sync agent outlives the app session
and there is no close-to-tray setting
(`test_sync_agent_survives_macos_window_close.py`,
`docs/goal/architecture/apps/macos.md` § Sync) — so `performClose` never
reaches `AppDelegate.applicationShouldTerminate`, where the bounded leave-flush
gate lives. `driver.quit_app()` sends the real `NSApp.terminate(nil)` that ⌘Q
sends. This is the one place apple's door differs in KIND from linux's and
windows', and it differs because the platform does.

**The ios leg is sound only because of the window seam — do not remove it.**
iOS's door is the background transition, and the app keeps RUNNING after it, so
unlike every other app's leg there is no process exit to assert. Every other leg
is sound precisely because the process is GONE by the time the assertion runs:
the debounce provably cannot have fired. A bare 6s poll after
`enter_background()` was measured passing, failing, passing and failing across
four runs before this seam existed, and at least the passes could have been the
ordinary ~1.5s debounced autosave rather than the flush — a false green, which
is worse than a short cell. So the ios leg instead re-times the debounce PAST
the whole test (`@pytest.mark.drafts_autosave_window_ms`, the shared
`fauna_client_drafts::autosave_debounce` seam), which restores exactly the
property process death gives the others: any save at all is necessarily the
flush. A SHORTER poll is not the alternative — a bound under the production
1.5 s only trades a false green for a load-dependent false red,
which is the wall-clock dependence convention 14 forbids. The iOS product door
is `FaunaApp.applicationDidEnterBackground`, flushing all three rails, and the
driver affordance is `IosDriver.enter_background`.

Both legs drive the app's own real delegate callback, never a test-only flush
entry point — what they do not prove is that AppKit/UIKit invokes that callback,
which is Apple's contract, exactly as the linux twin does not prove that GTK
emits close-request.

tier_3: a real app + real nest binary.
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import APP_EXIT_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

# No app marker at file level: each leg carries its OWN (`macos` / `ios`), so
# `--app macos` selects only the quit leg and `--app ios` only the background
# leg. Same shape as `test_media_sync_state_badge.py`'s two legs under one
# outcome — and it is what keeps convention 7 satisfied without a single
# defensive in-test skip.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
]

#: ⚠ ONE BODY PER LEG, and they must differ. The two legs share this session's
#: nest AND actor, so whatever the first leg flushes is still sitting in the
#: `__drafts` conversations rail when the second computes its baseline. With one
#: shared constant the second leg's `blob != baseline_blob` can never become
#: true — its app loads the existing blob, types the identical body, and
#: `save_if_changed` correctly dedups against bytes it already holds. That is a
#: CORRECT no-op being read as a dead leave door: the leg passed solo and failed
#: under `--app ios,macos`, for a reason that has nothing to do with either app.
MACOS_DRAFT_BODY = "draft flushed by the real macOS quit door, not the debounce"
IOS_DRAFT_BODY = "draft flushed by the real iOS background door, not the debounce"


def _drafts_blob(node_url, actor_id, signing_key):
    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        return dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")


def _type_a_draft_then_leave_immediately(app, body: str) -> None:
    """Open the new-thread composer and type the draft. The caller leaves the
    app on the very next statement — no wait for the debounced autosave, which
    is the whole point of the witness."""
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", body)


@pytest.mark.macos
@pytest.mark.feature("drafts-survive")
def test_quit_forces_conversation_draft_flush_before_exit(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    baseline_blob = _drafts_blob(node_url, actor_id, signing_key)

    # 1. Type a new-thread draft, then quit IMMEDIATELY.
    _type_a_draft_then_leave_immediately(app, MACOS_DRAFT_BODY)

    # 2. The real quit: NSApp.terminate -> applicationShouldTerminate ->
    # bounded flush of all three drafts rails -> .terminateLater reply.
    app.driver.quit_app()
    assert app.driver.wait_app_exit(APP_EXIT_S), (
        f"app did not exit on quit_app(); still alive={app.driver.is_app_alive()}; "
        f"error={app.error_text()!r}"
    )

    # 3. The draft must be on the nest — reachable ONLY via the quit-time
    # forced flush at this point, since no debounce could have fired.
    blob = _drafts_blob(node_url, actor_id, signing_key)
    assert blob is not None and blob != baseline_blob, (
        "conversation draft was not on the nest after quit_app() — the "
        "applicationShouldTerminate gate's forced drafts flush did not land "
        "before the process exited. No debounce could have fired in this "
        "window, so this is the forced flush's own failure."
    )


#: What the ios leg re-times the autosave debounce to (`autosave_debounce`,
#: seeded by the `drafts_autosave_window_ms` marker). Ten minutes: the number
#: only has to be unmistakably longer than anything this test can spend — the
#: per-test ceiling is 900 s (convention 9) and the wait below is bounded by
#: `RPC_ROUNDTRIP_S` — so that a draft found on the nest cannot be the debounce
#: having fired. It is NOT a wall-clock assertion: nothing waits for it, and
#: making it larger still would change no outcome.
IOS_AUTOSAVE_WINDOW_MS = 600_000


@pytest.mark.ios
@pytest.mark.drafts_autosave_window_ms(IOS_AUTOSAVE_WINDOW_MS)
@pytest.mark.feature("drafts-survive")
def test_background_forces_conversation_draft_flush(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    baseline_blob = _drafts_blob(node_url, actor_id, signing_key)

    # 1. Type a new-thread draft. The autosave this arms is scheduled
    # `IOS_AUTOSAVE_WINDOW_MS` out, so it cannot be what lands anything below —
    # which is also what makes the precondition below FREE here, where on the
    # other legs it would be a window for the debounce to fire in.
    _type_a_draft_then_leave_immediately(app, IOS_DRAFT_BODY)

    # 1b. Precondition, and the reason this leg can say what broke: the flush
    # saves `manager.draftsSnapshotBytes()`, so typing that has not reached the
    # MODEL yet makes `saveIfChanged` a no-op and the door look broken when it
    # is not (the automation-write race that became registration rule 10). Split
    # the two failures rather than leaving one message to cover both.
    body = wait_until(
        lambda: app.conversations.compose_body_text() == IOS_DRAFT_BODY,
        UI_SETTLE_S,
        diagnose=lambda: (
            "the draft never reached the composer model, so nothing below is a "
            "verdict on the leave door: "
            f"compose_body_text()={app.conversations.compose_body_text()!r}"
        ),
    )
    assert body

    # 2. The real leave door: applicationDidEnterBackground -> the background
    # task extension's flush of all three drafts rails. The app stays ALIVE
    # afterwards — backgrounding is not an exit — so there is no process death
    # to assert and the outcome must be read off the nest instead.
    app.driver.enter_background()

    # 3. The draft must reach the nest, and at this window it can only be the
    # forced flush. Bounded poll, not a sleep: it returns the instant the put
    # lands (convention 14 — the assertion is on STATE, and the budget is a
    # ceiling a green run never spends).
    def flushed_blob():
        blob = _drafts_blob(node_url, actor_id, signing_key)
        return blob if blob is not None and blob != baseline_blob else None

    blob = wait_until(
        flushed_blob,
        RPC_ROUNDTRIP_S,
        # `is_app_alive` is a DESKTOP driver method — the ios driver has no such
        # affordance, and backgrounding is not an exit anyway, so liveness is not
        # the question here. Asking for it once turned the real timeout into an
        # AttributeError raised from inside `diagnose`, hiding the very message
        # this argument exists to print.
        diagnose=lambda: (
            "conversation draft never reached the nest after enter_background() "
            f"— the autosave debounce was re-timed to {IOS_AUTOSAVE_WINDOW_MS} ms "
            "for this launch, so nothing but applicationDidEnterBackground's "
            "forced flush could have landed it, and nothing did. First thing to "
            "check is whether the door took its own early exit: it needs "
            "`appDelegate.appState` AND at least one rail VM published on it, and "
            "both are wired in the scene's `.onAppear`. "
            f"error={app.error_text()!r}"
        ),
    )
    assert blob

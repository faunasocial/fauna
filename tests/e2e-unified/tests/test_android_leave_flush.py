"""tier_3 E2E: the android leave-flush leg of `drafts-survive` outcome 5
(`docs/goal/behavior/reserved-folders.md` § The leave-flush promise).

android's leave door is the process lifecycle's `ON_STOP` — switching away on a
phone, the common way a phone app is left. `ConversationsManagerHost.onStop`
forces the conversation draft's `saveIfChanged` right then, rather than waiting
for the debounced autosave; the OS may kill a backgrounded app at any moment.
The twin of `test_apple_leave_flush.py::test_background_forces_conversation_draft_flush`.

**Sound only because of the window seam — do not remove it.** Backgrounding
leaves the app RUNNING, so unlike the quit/close legs there is no process exit
to prove the debounce could not have fired. The leg instead re-times the
debounce PAST the whole test (`@pytest.mark.drafts_autosave_window_ms`, the
shared `fauna_client_drafts::autosave_debounce` seam, which android reaches
through the intent → `MainActivity` `Os.setenv` door): any save at all is then
necessarily the flush. See the apple twin for why a shorter poll is not the
alternative (convention 14).

The door is the real HOME key (`AndroidDriver.enter_background`), so the leg
drives the real `ON_STOP`, never a test-only flush entry point.

tier_3: a real app + real nest binary.
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

#: Ten minutes: unmistakably longer than anything this test can spend (the
#: per-test ceiling is 900 s, convention 9, and the wait below is bounded by
#: `RPC_ROUNDTRIP_S`). Not a wall-clock assertion — nothing waits for it.
ANDROID_AUTOSAVE_WINDOW_MS = 600_000

ANDROID_DRAFT_BODY = "draft flushed by the real android ON_STOP door, not the debounce"


def _drafts_blob(node_url, actor_id, signing_key):
    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        return dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")


@pytest.mark.drafts_autosave_window_ms(ANDROID_AUTOSAVE_WINDOW_MS)
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
    # `ANDROID_AUTOSAVE_WINDOW_MS` out, so it cannot be what lands anything below.
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", ANDROID_DRAFT_BODY)

    # 1b. Precondition: the flush saves the manager's drafts snapshot, so typing
    # that has not reached the MODEL yet makes `saveIfChanged` a no-op and the
    # door look broken when it is not. Split the two failures.
    assert wait_until(
        lambda: app.conversations.compose_body_text() == ANDROID_DRAFT_BODY,
        UI_SETTLE_S,
        diagnose=lambda: (
            "the draft never reached the composer model, so nothing below is a "
            "verdict on the leave door: "
            f"compose_body_text()={app.conversations.compose_body_text()!r}"
        ),
    )

    # 2. The real leave door: HOME -> ON_STOP -> ConversationsManagerHost.onStop.
    # The app stays ALIVE afterwards, so the outcome is read off the nest.
    app.driver.enter_background()

    # 3. The draft must reach the nest, and at this window it can only be the
    # forced flush. Bounded poll on STATE, not a sleep (convention 14).
    def flushed_blob():
        blob = _drafts_blob(node_url, actor_id, signing_key)
        return blob if blob is not None and blob != baseline_blob else None

    assert wait_until(
        flushed_blob,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "conversation draft never reached the nest after enter_background() "
            f"— the autosave debounce was re-timed to {ANDROID_AUTOSAVE_WINDOW_MS} "
            "ms for this launch, so nothing but ConversationsManagerHost.onStop's "
            "forced flush could have landed it, and nothing did. First check the "
            "debounce env actually reached the app (MainActivity's Os.setenv door) "
            f"and that onStop was registered. error={app.error_text()!r}"
        ),
    )

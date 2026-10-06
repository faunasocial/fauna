"""tier_3 E2E: the tui leave-flush leg of `drafts-survive` outcome 5
(`docs/goal/behavior/reserved-folders.md` § The leave-flush promise; row 481).

tui's leave door is the sidebar's `exit-tab` — a terminal window has no
titlebar close/Cmd+Q/Alt+F4, so this is the one in-app quit affordance a real
user drives (`architecture/apps/tui.md` § Sidebar quit row); it sets
`app.should_quit`, which ends the app's main loop and returns control to
`main()` — the exact point `flush_drafts_on_quit` runs, right before the
process exits on its own (no signal involved).

Proves a REAL quit-time FLUSH (not merely the process exiting): types a
new-thread conversation draft and quits IMMEDIATELY — well inside the
debounced `fauna.drafts.put`'s save window
(`test_conversations_draft_persistence.py` polls up to 12s for that debounce +
WS round trip to land) — so if the draft reaches the nest's `__drafts` plane
at all, it can only be via the forced flush `main.rs::flush_drafts_on_quit`
runs after the main loop ends, modeled on
`test_windows_window_close_flush.py` / `test_linux_window_close_flush.py`.

tier_3: a real app + real nest binary.
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import APP_EXIT_S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.real_conversations]


@pytest.mark.feature("drafts-survive")
def test_exit_tab_forces_conversation_draft_flush_before_exit(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    draft_body = "draft flushed by exit-tab, not the debounce"

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        baseline_blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")

    # 1. Type a new-thread draft, then quit IMMEDIATELY — no wait for the
    # debounced autosave.
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", draft_body)

    # 2. The real quit door: exit-tab -> should_quit -> main loop ends ->
    # flush_drafts_on_quit -> process exits on its own.
    app.driver.click("exit-tab")
    assert app.driver.wait_app_exit(APP_EXIT_S), (
        f"app did not exit after exit-tab; still alive={app.driver.is_app_alive()}; "
        f"error={app.error_text()!r}"
    )

    # 3. The draft must be on the nest — reachable ONLY via the quit-time
    # forced flush at this point, since no debounce could have fired.
    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")
    assert blob is not None and blob != baseline_blob, (
        "conversation draft was not on the nest after exit-tab — "
        "flush_drafts_on_quit did not land before the process exited"
    )

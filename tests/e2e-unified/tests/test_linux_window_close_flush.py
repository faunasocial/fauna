"""tier_3 E2E: the linux leave-flush leg of `drafts-survive` outcome 5
(`docs/goal/behavior/reserved-folders.md` § The leave-flush promise; row 481).

Proves a REAL quit-time FLUSH (not merely the process exiting): types a
new-thread conversation draft and closes the window IMMEDIATELY — well inside
the debounced `fauna.drafts.put`'s save window
(`test_conversations_draft_persistence.py` polls up to 12s for that debounce +
WS round trip to land) — so if the draft reaches the nest's `__drafts` plane
at all, it can only be via the forced flush the real `connect_close_request`
handler runs before the process exits (the linux twin of windows'
`TrayIconService.QuitApplication()` → `ConvDrafts.SaveNowAsync()`, modeled on
`test_windows_window_close_flush.py`).

Close-to-tray is flipped OFF first so `window_close()` reaches the real quit
path rather than hiding the window (default e2e launch already runs on a
private, watcher-less session bus per `drivers/linux.py::_wants_private_bus`,
so close already quits by default — flipping the toggle explicitly makes that
assumption an assertion rather than an inherited default, matching the windows
test's own discipline).

tier_3: a real app + real nest binary.
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import APP_EXIT_S

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.real_conversations]


@pytest.mark.feature("drafts-survive")
def test_window_close_forces_conversation_draft_flush_before_exit(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    draft_body = "draft flushed by real window_close, not the debounce"

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        baseline_blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")

    # 1. Close-to-tray OFF FIRST — a private watcher-less bus already makes
    # window_close() quit by default (`_wants_private_bus`), but pinning the
    # toggle explicitly (rather than relying on the inherited default) matches
    # the windows test's own discipline and guards against the toggle
    # defaulting ON for a fresh install (`test_tray_close_to_tray.py`'s
    # shape-A default).
    app.driver.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}}
    )
    app.driver.wait_for("close-to-tray-toggle", timeout=10)
    if app.driver.get_attr("close-to-tray-toggle", "state") == "true":
        app.driver.click("close-to-tray-toggle")
    assert app.driver.get_attr("close-to-tray-toggle", "state") != "true", (
        "close-to-tray must be OFF for window_close() to reach the real quit "
        f"path; error={app.error_text()!r}"
    )

    # 2. Type a new-thread draft, then close the window IMMEDIATELY — no wait
    # for the debounced autosave.
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", draft_body)

    # 3. The real quit: connect_close_request -> no tray host to hide to ->
    # forced drafts flush -> app.quit().
    app.driver.window_close()
    assert app.driver.wait_app_exit(APP_EXIT_S), (
        f"app did not exit on window close; still alive={app.driver.is_app_alive()}; "
        f"error={app.error_text()!r}"
    )

    # 4. The draft must be on the nest — reachable ONLY via the quit-time
    # forced flush at this point, since no debounce could have fired.
    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")
    assert blob is not None and blob != baseline_blob, (
        "conversation draft was not on the nest after window_close() — the "
        "close-request handler's forced drafts flush did not land before the "
        "process exited"
    )

"""tier_3 E2E: the web leave-flush leg of `drafts-survive` outcome 5
(`docs/goal/behavior/reserved-folders.md` § The leave-flush promise; row 481).

web's leave door is the tab-leave events the browser actually delivers —
`pagehide` / `visibilitychange` — and the goal doc says plainly that a browser
grants no reliable async work after either, so this leg is deliberately
best-effort (unlike the native legs' bounded-BLOCKING flush). This test
dispatches a real `pagehide` DOM event (the root layout's handler,
`+layout.svelte`, has no `visibilityState` guard on that branch, unlike the
`visibilitychange` branch which needs the page to actually be backgrounded) and
proves `$lib/conversations`'s `flushDraftsNow()` runs and lands on the nest —
modeled on `test_windows_window_close_flush.py` /
`test_linux_window_close_flush.py` / `test_tui_quit_flush.py`, adapted to the
one door a script can drive without tearing the page down (a real tab close
would end the driver session before the assertion could run).

tier_3: a real app + real nest binary.
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = [pytest.mark.tier_3, pytest.mark.web, pytest.mark.real_conversations]


@pytest.mark.feature("drafts-survive")
def test_pagehide_forces_conversation_draft_flush(logged_in_app, nest_instance, test_user):
    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    draft_body = "draft flushed by pagehide, not the debounce"

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        baseline_blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")

    # 1. Type a new-thread draft. Web's production debounce
    # (`autosaveDebounceMs()`) is short under the e2e agent
    # (`DRAFT_SAVE_DEBOUNCE_E2E_MS`), but we don't wait for it: dispatch
    # `pagehide` IMMEDIATELY so a landed blob can only be the forced flush.
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", draft_body)

    # 2. The real leave door: a `pagehide` DOM event, exactly what the browser
    # delivers on tab close / navigation away — `+layout.svelte`'s handler
    # calls `flushDraftsNow()` unconditionally on this event (no visibility
    # check, unlike the `visibilitychange` branch).
    app.driver.eval_js("window.dispatchEvent(new Event('pagehide'))")

    # 3. The draft must be on the nest — reachable ONLY via the forced
    # `pagehide` flush at this point, since no debounce could have fired yet.
    def _poll_blob():
        import time

        deadline = time.time() + 5.0
        while time.time() < deadline:
            with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
                blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")
            if blob is not None and blob != baseline_blob:
                return blob
            time.sleep(0.2)
        return None

    blob = _poll_blob()
    assert blob is not None, (
        "conversation draft was not on the nest after a pagehide event — "
        "flushDraftsNow() did not run or did not land; "
        f"error={app.error_text()!r}"
    )

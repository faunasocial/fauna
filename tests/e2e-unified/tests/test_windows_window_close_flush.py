"""FlaUI-bridge windows e2e test infra: window_close()/wait_app_exit() hooks.

Proves the graceful-quit path end-to-end: ``Actions.WindowClose`` posts a
real WM_CLOSE to the app's main window, running the real
``AppWindow.Closing`` handler — with Close-to-tray OFF, that reaches
``TrayIconService.QuitApplication()`` — never ``SessionManager.Quit()``'s
force-kill, which skips both close-to-tray and every quit-time flush.

Proof of a REAL quit-time FLUSH (not merely the process exiting): types a
new-thread conversation draft and closes the window IMMEDIATELY — well
inside the debounced ``fauna.drafts.put``'s save window
(``test_conversations_draft_persistence.py`` polls up to 12s for that
debounce + WS round trip to land) — so if the draft reaches the nest's
``__drafts`` plane at all, it can only be via
``ConversationsDraftsService.SaveNowAsync()``, the FORCED flush
``TrayIconService.QuitApplication()`` runs before exit (``App.xaml.cs``'s
own comment: "flush the latest compose body to the nest before exit so a
quit-within-the-debounce-window doesn't lose it"). Close-to-tray is flipped
OFF *before* typing the draft, so the only thing between typing and
``window_close()`` is the close call itself — no intervening navigation
that could let the debounce win the race first.

tier_3: a real app + real nest binary. ``real_conversations`` mirrors
``test_conversations_draft_persistence.py`` — the windows leg needs the
REAL ``ConversationsSession`` receive loop (the production drafts autosync
sits in the ``!FAUNA_E2E_BRIDGE`` block the e2e never reaches otherwise).
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.budgets import APP_EXIT_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.windows, pytest.mark.real_conversations]


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

    # 1. Close-to-tray OFF FIRST — it defaults ON (AppSettingsStore.
    # CloseToTray), so a plain window_close() would just hide the window and
    # never reach QuitApplication() at all. Doing this BEFORE typing the
    # draft means nothing but the close call itself sits between typing and
    # closing below — no extra navigation that could let the debounce win.
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

    # 2. Type a new-thread draft, then close the window IMMEDIATELY — no
    # wait for the debounced autosave.
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", draft_body)

    # 3. The real quit: WM_CLOSE -> AppWindow.Closing -> TrayIconService.
    # QuitApplication() -> App.FlushDraftRailsAsync().Wait(1500) (the
    # conversations rail's ConvDrafts.SaveNowAsync() among the three) -> exit.
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
        "conversation draft was not on the nest after window_close() — "
        "QuitApplication()'s ConvDrafts.SaveNowAsync() quit-time flush did "
        "not land before the process exited"
    )

    # 5. Discard the draft this test just proved is DURABLE — the cleanup is part
    #    of the test (test_conversations_draft_persistence.py's step 7 has the
    #    reasoning): `__drafts` is per-actor nest state on the session-scoped
    #    `test_user`, so a draft left here comes back in the next test's
    #    new-thread composer. Measured 2026-10-09: run after this file, that
    #    file's precondition read "draft flushed by real window_close, not the
    #    debouncedraft that must survive an app restart 4827". Relaunch as the
    #    same actor, wait for the launch restore to bring the draft back, then
    #    cancel it — the user-facing discard — and wait for the emptied set to
    #    reach the nest. Asserted, never best-effort, for the same reason.
    app.driver.hard_reload()
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    wait_until(
        lambda: app.conversations.compose_body_text() == draft_body,
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"cleanup: the relaunch never restored the draft to cancel; composer="
            f"{app.conversations.compose_body_text()!r} error={app.error_text()!r}"
        ),
    )
    app.driver.click("new-conversation-cancel")

    def _emptied_set_landed():
        with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
            return dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob") != blob

    wait_until(
        _emptied_set_landed,
        budgets.RPC_ROUNDTRIP_S,
        interval=0.5,
        diagnose=lambda: (
            "cleanup: cancelling the restored draft never reached the nest, so it "
            "leaks into the next test's composer. The flush assertion above already "
            "PASSED — look at new-conversation-cancel and the debounced save here."
        ),
    )

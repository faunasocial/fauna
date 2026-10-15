"""tier_3 E2E: signing out saves the draft still inside its autosave window, and
signing back in restores it as the user left it — the sign-out door of
`docs/goal/behavior/reserved-folders.md` § The leave-flush promise ("Signing
out, and switching to another account, is a leave door too, on every app").

**Why this door needs its own witness.** The quit/close/background doors
(`test_tui_quit_flush.py` and its twins) end the process, so the flush is the
last thing it does. Signing out does not end the process — it takes away the
SESSION the debounced save would ride. An app that drops its autosave loops (or
disposes its client) on sign-out loses the edit still inside the debounce, and
the next sign-in restores the draft from BEFORE it — whole, audience included.
That is how this surfaced: `test_feed_compose_in_flight.py`'s cleanup (empty the
composer, back to Public) was still inside the debounce when the per-test reset
signed windows out, so the next sign-in restored the previous test's sale and
the following post went out as one.

**The window seam makes the verdict the door's own.** The autosave debounce is
re-timed to `AUTOSAVE_WINDOW_MS` for this run (`drafts_autosave_window_ms`, the
shared `fauna_client_drafts::autosave_debounce` seam), so the debounce cannot
fire anywhere in this test: a draft found on the nest after the sign-out can
only have been put there by the sign-out's own flush. Without the seam this test
would race the production 1.5 s window — the walk to Settings alone can outlast
it — and pass for the wrong reason.

The sign-out is the user's own gesture (`sign-out-button` → `sign-out-confirm-button`,
`SettingsActions.sign_out`), never the harness's reset: linux's e2e reset
destroys its windows without passing through the sign-out branch that flushes,
so only the real gesture is the door a user crosses.

**Columns.** tui (its sign-out flushes in `session::sign_out`'s stop task,
before the client disconnects; green on Linux 2026-10-10) and windows
(`App.FlushDraftRailsAsync()` at `SignOutHandler`). linux is marked because it
owes this door, and it is RED on Linux 2026-10-10 — its only sign-out flush sits
in `connect_close_request`'s sign-out branch, which runs after the account
runtime's stop (the nest has already revoked the session) and after
`actor_scope::reset_actor_scoped_state` has cleared the feed manager, so the
save never lands. web, android,
macos and ios are not yet audited for this door; they are left unmarked, so
`just features-parity drafts-survive` reads them as gaps — web additionally
cannot take the window seam at all (`wasm32-unknown-unknown` has no process
environment), so its leg will need a door-shaped witness of its own.

tier_3: a real app + real nest binary.
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

#: What the autosave debounce is re-timed to for this run. Ten minutes, the same
#: value `test_apple_leave_flush.py` uses (two different values in one selection
#: are refused, so sharing it keeps the two co-selectable): it only has to be
#: unmistakably longer than this test can spend. Nothing waits for it — it is not
#: a wall-clock assertion.
AUTOSAVE_WINDOW_MS = 600_000

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.drafts_autosave_window_ms(AUTOSAVE_WINDOW_MS),
]

#: The rail this test drives — the feed composer's, the one the in-flight
#: incident leaked through (`fauna_protocol::drafts::DRAFT_RAILS`).
RAIL = "posts"


def _posts_blob(node_url, actor_id, signing_key):
    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        return dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")


def _await_blob_moved(app, node_url, actor_id, signing_key, before, what: str):
    """The rail's blob once it differs from ``before`` — bounded, returning the
    instant the put lands (convention 14)."""

    def moved():
        blob = _posts_blob(node_url, actor_id, signing_key)
        return blob if blob is not None and blob != before else None

    return wait_until(
        moved,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{what}: the {RAIL!r} drafts rail never changed on the nest after the "
            f"sign-out. The autosave debounce was re-timed to {AUTOSAVE_WINDOW_MS} ms "
            "for this launch, so only the sign-out's own flush could have saved it, "
            "and nothing did — the outgoing session's rails were torn down unsaved. "
            f"error={app.error_text()!r}"
        ),
    )


@pytest.mark.feature("drafts-survive")
def test_signing_out_saves_the_draft_inside_its_autosave_window(
    request, logged_in_app, nest_instance, test_user
):
    from conftest import _login_app_as

    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])
    draft_body = "half-written post that signing out must not lose 7731"

    baseline_blob = _posts_blob(node_url, actor_id, signing_key)

    # 1. Type a post and don't submit it. The autosave this arms is
    #    `AUTOSAVE_WINDOW_MS` out, so it saves nothing in this test.
    app.feed.navigate()
    app.feed.open_composer()
    app.driver.type_text("compose-text-field", draft_body)
    # Precondition: the text reached the composer MODEL. The flush saves the
    # manager's snapshot, so typing still in flight would make the door look
    # broken when it is not — split that failure from the door's own.
    wait_until(
        lambda: app.feed.compose_body_text() == draft_body,
        UI_SETTLE_S,
        diagnose=lambda: (
            "the draft never reached the composer, so nothing below is a verdict on "
            f"the sign-out door: compose_body_text()={app.feed.compose_body_text()!r}"
        ),
    )

    # 2. The real door: the user's own sign-out, inside the autosave window.
    app.settings.sign_out()

    # 3. The draft is on the nest — reachable only through the sign-out's flush.
    flushed_blob = _await_blob_moved(
        app, node_url, actor_id, signing_key, baseline_blob, "the sign-out flush"
    )

    # 4. Sign back in as the same user. The new session's launch restore fetches
    #    the rail from the nest.
    _login_app_as(app, request, nest_instance, test_user)
    app.feed.navigate()
    app.feed.open_composer()

    # 5. By VALUE: the draft as the user left it, never the one from before
    #    their last edit (which is what a torn-down-unsaved session restores).
    restored = wait_until(
        lambda: app.feed.compose_body_text() == draft_body,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the draft did not come back after signing in again "
            f"(expected {draft_body!r}, got {app.feed.compose_body_text()!r}); "
            f"error={app.error_text()!r}"
        ),
    )
    assert restored

    # 6. Cleanup, asserted: `__drafts` is per-actor state on the session-scoped
    #    `test_user`, so a draft left here is restored into the next test's
    #    composer. The debounce still cannot fire, so the emptied draft goes out
    #    through the same door this test proves.
    app.driver.clear_and_type("compose-text-field", "")
    wait_until(
        lambda: app.feed.compose_body_text() == "",
        UI_SETTLE_S,
        diagnose=lambda: f"cleanup: composer still holds {app.feed.compose_body_text()!r}",
    )
    app.settings.sign_out()
    _await_blob_moved(
        app, node_url, actor_id, signing_key, flushed_blob,
        "cleanup (the assertions above already PASSED — this is the emptied draft "
        "failing to persist, which would leak into the next test's composer)",
    )

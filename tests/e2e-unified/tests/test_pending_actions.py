"""The standing pending-actions section (`ui/settings.md` § Pending actions).

The three delayed verbs — handle change, account delete, snapshot delete —
schedule a **cancellable** action on the nest; until 2026-08-19 every app
discarded the reply, so the cancellation window the nest deliberately holds
was unreachable from any UI. This journey proves the whole loop through the
app's own controls (convention 8 — the mutations are UI actions):

1. the Account page's `pending-actions-section` is STANDING and hydrates to
   an honest reading (never asserted against the bare un-hydrated title —
   the section deliberately makes no "nothing scheduled" claim before its
   first list read lands);
2. scheduling a handle change through `new-handle` + `change-handle` makes a
   `pending-action-item` row appear, its description naming the requested
   handle and its `pending-action-execute-after` naming when it would apply;
3. `pending-action-cancel-button` is ONE CLICK (no confirm — cancelling is
   the safe direction) and removes the row;
4. the handle NEVER changed: the anonymous `fauna.actor.by_handle` still
   resolves the original handle to this actor, and the requested handle
   resolves to nothing. This is the sanctioned external black-box
   VERIFICATION (convention 8's carve-out) — the mutation path stays UI.

The cancel reply is the causal barrier for (4): the nest holds the change
until `execute_after` (a day away), so a cancelled action can never apply —
no settle-sleep, nothing timing-dependent (convention 14).

All 7 apps now build the section (tui led 2026-08-19; windows landed
last, 2026-09-07). `skip_unbuilt` stays as the standing
per-app guard (never a hand-maintained allowlist) — a client where the
surface later regresses or where a fresh app never lands it reports through
here rather than a silent false green.
"""

import uuid

import pytest

from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient
from helpers.app_surface import skip_unbuilt
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3


def _resolve_handle(nest_url: str, handle: str) -> str | None:
    """Anonymous `fauna.actor.by_handle` → actor-id hex, or None when the
    handle resolves to nothing (`fauna.actor.not_found`)."""
    with WsRpcAnonClient(nest_url) as client:
        try:
            return client.call("fauna.actor.by_handle", {"handle": handle})["actor_id"]
        except RpcCallError:
            return None


@pytest.mark.feature("account")
def test_a_scheduled_handle_change_is_visible_and_cancellable(
    logged_in_app, nest_instance, test_user
):
    d = logged_in_app.driver
    logged_in_app.settings._navigate_subpage("account")
    try:
        d.wait_for("pending-actions-section", timeout=10)
    except Exception:
        skip_unbuilt(
            d,
            surface="pending-actions-section",
            detail=(
                "the standing pending-actions section (settings.md § Pending "
                "actions) is not built on this app yet"
            ),
            tracked="",
        )

    # (1) The nav-edge hydrate lands: the container's text leaves the bare
    # un-hydrated title for one of the two loaded readings. The shared user is
    # session-scoped, so another test may legitimately have left a scheduled
    # row — assert hydration, not emptiness.
    bare_title = "Pending actions"
    wait_until(
        lambda: d.get_text("pending-actions-section") != bare_title,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
    )
    rows_before = d.count("pending-action-item")

    # (2) Schedule a handle change through the app's own form. The requested
    # handle is unique per run, so the row is findable by its description and
    # the not-found verification below can never collide with a real handle.
    original_handle = test_user["handle"]
    requested = f"pend-{uuid.uuid4().hex[:10]}"
    d.clear_and_type("new-handle", requested)
    d.click("change-handle")

    def our_row_index():
        for i in range(d.count("pending-action-item")):
            if requested in d.get_text("pending-action-description", index=i):
                return i + 1  # 1-based so index 0 is truthy for wait_until
        return 0

    index = (
        wait_until(
            our_row_index,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                f"{d.count('pending-action-item')} row(s); descriptions: "
                + repr(
                    [
                        d.get_text("pending-action-description", index=i)
                        for i in range(d.count("pending-action-item"))
                    ]
                )
            ),
        )
        - 1
    )
    assert d.count("pending-action-item") == rows_before + 1
    when = d.get_text("pending-action-execute-after", index=index)
    assert when, "the row must say when the change would apply"

    # (3) One click cancels — no confirm dialog to answer.
    d.click("pending-action-cancel-button", index=index)
    wait_until(
        lambda: our_row_index() == 0,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
    )
    assert d.count("pending-action-item") == rows_before

    # (4) The handle never changed — the cancel reply already landed (the
    # causal barrier), and the nest would not have executed a change scheduled
    # a day out regardless, so this read is latency-independent.
    assert _resolve_handle(nest_instance["url"], original_handle) == test_user[
        "actor_id_hex"
    ], "the original handle must still resolve to this actor"
    assert _resolve_handle(nest_instance["url"], requested) is None, (
        "a cancelled handle change must never take effect"
    )


@pytest.mark.feature("account")
def test_a_queued_account_delete_is_visible_and_cancellable(logged_in_app):
    """The second delayed verb feeds the same section: queueing the account
    delete (type-to-confirm, then the button — both UI) shows a cancellable
    'Delete this account' row, and one click takes it back. Cancelling is
    what keeps the shared session-scoped user alive for every later test —
    the nest holds the deletion for a multi-day window, so the account is
    never at risk inside this test's lifetime (and the cancel lands before
    the test ends)."""
    d = logged_in_app.driver
    logged_in_app.settings._navigate_subpage("account")
    try:
        d.wait_for("pending-actions-section", timeout=10)
    except Exception:
        skip_unbuilt(
            d,
            surface="pending-actions-section",
            detail=(
                "the standing pending-actions section (settings.md § Pending "
                "actions) is not built on this app yet"
            ),
            tracked="",
        )

    delete_description = "Delete this account"

    def delete_row_index():
        for i in range(d.count("pending-action-item")):
            if delete_description in d.get_text("pending-action-description", index=i):
                return i + 1
        return 0

    assert delete_row_index() == 0, (
        "no queued delete may pre-exist this test (a leftover would mean an "
        "earlier run's cancel never landed)"
    )

    logged_in_app.settings.type_delete_confirm("DELETE")
    logged_in_app.settings.confirm_delete_account()
    index = (
        wait_until(
            delete_row_index,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
        )
        - 1
    )

    d.click("pending-action-cancel-button", index=index)
    wait_until(
        lambda: delete_row_index() == 0,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
    )


@pytest.mark.feature("account")
def test_a_scheduled_handle_change_is_not_shown_as_your_handle(
    logged_in_app, nest_instance, test_user
):
    """Until a scheduled handle change takes effect, the app keeps showing the
    handle the user actually holds (`settings.md` § Pending actions: "never
    feed the echoed new value into local caches").

    `test_a_scheduled_handle_change_is_visible_and_cancellable` above proves
    the nest side — the handle never changed — but never reads what the APP
    shows the user as their handle, which is exactly where a client feeding
    the echoed value into its cache would lie. Where every app shows the
    user their own handle is the account switcher's `account-item-handle` on
    this same Account page; the Account page is re-entered after the
    schedule so a nav-edge refresh cannot hide a cache that the reply fed.
    The scheduled change is cancelled through the UI before the test ends,
    so the shared session user keeps its handle for every later test."""
    d = logged_in_app.driver
    settings = logged_in_app.settings
    settings._navigate_subpage("account")
    try:
        d.wait_for("pending-actions-section", timeout=10)
    except Exception:
        skip_unbuilt(
            d,
            surface="pending-actions-section",
            detail=(
                "the standing pending-actions section (settings.md § Pending "
                "actions) is not built on this app yet"
            ),
            tracked="",
        )

    original = test_user["handle"]
    original_local = original.split("@")[0]
    requested = f"pend-{uuid.uuid4().hex[:10]}"

    def shown_handles():
        return [
            d.get_text("account-item-handle", index=i)
            for i in range(d.count("account-item-handle"))
        ]

    # Precondition: the page shows the handle the user holds today — without
    # it the "not shown" assertion below could pass on a page that shows none.
    wait_until(
        lambda: any(original_local in h for h in shown_handles()),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"account-item-handle rows: {shown_handles()!r}",
    )

    def our_row_index():
        for i in range(d.count("pending-action-item")):
            if requested in d.get_text("pending-action-description", index=i):
                return i + 1
        return 0

    d.clear_and_type("new-handle", requested)
    d.click("change-handle")
    try:
        # The reply landed: its receipt row is on the page.
        wait_until(
            our_row_index,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
        )
        # Re-enter the Account page, then read what it calls the user's handle.
        settings._navigate_subpage("status")
        settings._navigate_subpage("account")
        wait_until(
            lambda: d.count("pending-action-item") > 0 and our_row_index(),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
        )
        shown = shown_handles()
        assert not any(requested in h for h in shown), (
            "a handle change that has not taken effect must never be shown as "
            f"the user's handle; account-item-handle rows read {shown!r}"
        )
        assert any(original_local in h for h in shown), (
            f"the page must still show the handle the user holds ({original!r}); "
            f"account-item-handle rows read {shown!r}"
        )
        session_handle = (d.get_state() or {}).get("session", {}).get("handle")
        if session_handle is not None:
            assert requested not in session_handle, (
                f"the session's handle took the scheduled value: {session_handle!r}"
            )
    finally:
        index = our_row_index()
        if index:
            d.click("pending-action-cancel-button", index=index - 1)
            wait_until(
                lambda: our_row_index() == 0,
                RPC_ROUNDTRIP_S,
                diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
            )


@pytest.mark.feature("account")
def test_an_admins_deletion_of_this_account_is_listed_and_cancellable(
    logged_in_app, nest_instance, test_user
):
    """The fourth row this section can carry is one the user did NOT schedule:
    an administrator's pending deletion of their account (`settings.md`
    § Pending actions, ruled 2026-09-24 — the target is one of the parties
    the cancel matrix admits, and this section is where they act). The
    admin's act is the other party's, made over the wire (`fauna.admin.users.delete`,
    the only creator of that action — no app schedules it); the user's own
    mutation, the cancel, is the UI's. The nest holds the deletion for seven
    days, so the shared session user is never at risk inside this test's
    lifetime — and the cancel lands before it ends."""
    from common.auth import _authed_call

    d = logged_in_app.driver
    logged_in_app.settings._navigate_subpage("account")
    try:
        d.wait_for("pending-actions-section", timeout=10)
    except Exception:
        skip_unbuilt(
            d,
            surface="pending-actions-section",
            detail=(
                "the standing pending-actions section (settings.md § Pending "
                "actions) is not built on this app yet"
            ),
            tracked="",
        )

    description = S.settings.pending_actions.admin_delete_user(
        target=test_user["actor_id_hex"]
    )

    def our_row_index():
        for i in range(d.count("pending-action-item")):
            if description in d.get_text("pending-action-description", index=i):
                return i + 1
        return 0

    assert our_row_index() == 0, "no admin deletion may pre-exist this test"

    scheduled = _authed_call(
        nest_instance["url"],
        nest_instance["admin"]["signing_key"],
        "fauna.admin.users.delete",
        {"actor_id": bytes.fromhex(test_user["actor_id_hex"])},
    )
    assert scheduled["status"] == "pending", scheduled
    action_id = scheduled["pending_action_id"]

    # Re-enter the page: the section hydrates at the nav edge.
    logged_in_app.settings._navigate_subpage("status")
    logged_in_app.settings._navigate_subpage("account")
    index = (
        wait_until(
            our_row_index,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                f"section text: {d.get_text('pending-actions-section')!r}; rows: "
                + repr(
                    [
                        d.get_text("pending-action-description", index=i)
                        for i in range(d.count("pending-action-item"))
                    ]
                )
            ),
        )
        - 1
    )
    d.click("pending-action-cancel-button", index=index)
    wait_until(
        lambda: our_row_index() == 0,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
    )
    detail = _authed_call(
        nest_instance["url"],
        nest_instance["admin"]["signing_key"],
        "fauna.pending_actions.get",
        {"id": action_id},
    )
    assert detail["status"] == "cancelled", (
        f"the target's cancel must land on the nest's record: {detail!r}"
    )

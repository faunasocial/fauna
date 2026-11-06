"""A conversation draft never crosses an identity switch — the one arc every
app's account-switcher module runs (drafts-survive outcome 16,
`conversation-drafts.md` § Persistence: "A restore the outgoing account started
fills nothing after an identity change").

The arc is the same on all 7 apps, so it lives here once; each
`test_account_switcher_<app>.py` launches its own app on a two-identity seed
(the user active, the admin second) and hands over the two app-specific
readings only it knows — how to bring the switcher up with both rows listed,
and how to tell that the incoming identity's session is working.

The steps: as the user, half-write a new message (confirmed on the user's
`__drafts` plane before switching, so the switch tests scoping, not the
debounce); switch to the admin → the composer does not hold it; half-write the
admin's own (confirmed on the admin's plane); switch back → the user's message
is there again; switch to the admin once more → the composer holds the ADMIN's
message. That last read is the barriered witness: a restore FILLS and never
overwrites (§ Persistence), so if a load the user's session started had landed
after the switch, it would have filled the slot first and the admin's own
restore could not replace it — the composer would hold the user's text.

Every mutation is a UI gesture; the drafts reads are the sanctioned side-channel
verification (convention 8's carve-out). Activating the admin may raise the
in-app re-auth prompt (`long-term-store.md` § Per-account re-auth — the app
flags its admin identity by default); the user confirms it, as they would.
"""

from __future__ import annotations

import secrets
from typing import Callable

from actions.conversations import ConversationsActions
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

SWITCHER_ITEM = "account-switcher-item"
ITEM_HANDLE = "account-item-handle"
REAUTH_PROMPT = "account-activate-reauth-prompt"
REAUTH_CONFIRM_BUTTON = "account-activate-reauth-confirm-button"


def _drafts_log(driver) -> str:
    """The app's own `drafts` log lines (convention 6 — the failure carries its
    cause), from a driver that exposes its launch's stderr; "" otherwise."""
    read = getattr(driver, "app_stderr_text", None)
    if read is None:
        return "(this driver exposes no app stderr)"
    lines = [line for line in (read() or "").splitlines() if "drafts" in line.lower()]
    return "\n".join(lines[-20:]) or "(no drafts lines)"


def assert_new_message_draft_never_crosses_identity(
    driver,
    nest_url: str,
    *,
    user: tuple[str, bytes],
    admin: tuple[str, bytes],
    show_switcher: Callable[[], None],
    await_session: Callable[[str, str], None],
) -> None:
    """Run the arc on an app already launched as the user.

    ``user`` and ``admin`` are ``(actor_id_hex, signing_key_bytes)``.
    ``show_switcher()`` brings the account switcher up with both rows listed;
    ``await_session(actor_hex, what)`` returns once the app works as that
    identity. The caller owns the driver's launch and teardown."""
    user_actor, user_sk = user
    admin_actor, admin_sk = admin
    conv = ConversationsActions(driver)
    # Fresh per call, never per process: the admin is shared by the whole run, so
    # a text another app's run of this arc already left on the admin's plane would
    # be restored on the switch, the app's save would dedup it as unchanged, and
    # the plane would never move.
    nonce = secrets.token_hex(4)
    # What each identity's half-write left on its plane, so a composer that
    # never settles can say whether the plane moved since (convention 6).
    rested: dict[str, object] = {}
    user_draft = f"a new message only the user is writing {nonce}"
    admin_draft = f"a new message only the admin is writing {nonce}"

    def drafts_blob(actor_hex, signing_key):
        with WsRpcAdminClient(
            nest_url, actor_id=bytes.fromhex(actor_hex), signing_key=signing_key,
        ) as dev:
            return dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")

    def open_new_message():
        conv.navigate()
        driver.click("new-conversation-button")
        driver.wait_for("dm-text-field", timeout=10.0)

    def half_write(text, actor_hex, signing_key):
        baseline = drafts_blob(actor_hex, signing_key)
        open_new_message()
        driver.clear_and_type("dm-text-field", text)
        wait_until(
            lambda: conv.compose_body_text() == text, UI_SETTLE_S,
            diagnose=lambda: f"composer reads {conv.compose_body_text()!r}",
        )
        wait_until(
            lambda: drafts_blob(actor_hex, signing_key) not in (None, baseline),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: "the draft never reached its identity's __drafts plane; "
            f"the app's drafts log: {_drafts_log(driver)}",
        )
        rested[actor_hex] = drafts_blob(actor_hex, signing_key)

    def switch_to(handle, actor, what):
        # The row is found by what it shows, never by a position the list may
        # reorder between switches: the admin's row is the one naming "admin",
        # the user's the other one.
        show_switcher()
        labels = [driver.get_text(ITEM_HANDLE, index=i) for i in range(2)]
        admin_rows = [i for i, label in enumerate(labels) if "admin" in (label or "")]
        assert len(admin_rows) == 1, f"exactly one switcher row should name the admin: {labels!r}"
        index = admin_rows[0] if handle == "admin" else 1 - admin_rows[0]
        driver.click(SWITCHER_ITEM, index=index)
        outcome = wait_until(
            lambda: "prompt" if driver.is_visible(REAUTH_PROMPT)
            else ("switched" if (driver.get_state() or {}).get("session", {}).get("actor_id") == actor
                  else None),
            APP_RELAUNCH_S,
            diagnose=lambda: f"{what}: neither the switch nor a re-auth prompt "
            f"(rows {labels!r}, clicked {index})",
        )
        if outcome == "prompt":
            driver.click(REAUTH_CONFIRM_BUTTON)
        await_session(actor, f"{what} (rows {labels!r}, clicked {index})")

    def composer_settles_on(text, who, actor_hex, signing_key):
        open_new_message()

        def plane():
            # A red with the plane moved is a save that overwrote it; one with
            # the plane intact is a restore that never filled the composer.
            now = drafts_blob(actor_hex, signing_key)
            if now == rested.get(actor_hex):
                return "unchanged since its half-write"
            return f"MOVED since its half-write (now {'absent' if now is None else 'another blob'})"

        wait_until(
            lambda: conv.compose_body_text() == text, RPC_ROUNDTRIP_S,
            diagnose=lambda: f"{who}'s new-message composer should hold {text!r}; "
            f"it reads {conv.compose_body_text()!r}; {who}'s __drafts plane is {plane()}; "
            f"the app's drafts log: {_drafts_log(driver)}",
        )

    await_session(user_actor, "the launch as the user")

    # (1) The user's half-written message, at rest on the nest.
    half_write(user_draft, user_actor, user_sk)

    # (2) Switch to the admin: the user's message is not here.
    switch_to("admin", admin_actor, "the switch to the admin")
    open_new_message()
    assert user_draft not in conv.compose_body_text(), (
        "the user's half-written message followed the device to the admin; "
        f"the admin's composer reads {conv.compose_body_text()!r}"
    )
    # The admin writes their own (replacing any earlier rest of theirs).
    half_write(admin_draft, admin_actor, admin_sk)

    # (3) Back to the user: theirs is there again (the positive control).
    switch_to("user", user_actor, "the switch back to the user")
    composer_settles_on(user_draft, "the user", user_actor, user_sk)

    # (4) The admin once more: the admin's own message, never the user's.
    switch_to("admin", admin_actor, "the second switch to the admin")
    composer_settles_on(admin_draft, "the admin", admin_actor, admin_sk)

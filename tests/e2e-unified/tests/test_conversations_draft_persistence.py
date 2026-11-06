"""tier_3: a fauna-native conversations compose draft survives an app restart
(draft-persistence v2 — ``docs/goal/behavior/reserved-folders.md`` § Drafts Sync +
``docs/goal/ui/conversations.md`` § Persistence).

The shared serialize/seal/restore is proven in ``fauna-conversations`` (tier_1)
and the nest ``__drafts`` plane in
``tests/e2e-unified/tests/api/test_drafts_sync.py`` (tier_3). This test proves the
*web app wiring*: the composer's in-progress draft is persisted to ``__drafts``
after a compose change (the debounced ``saveDrafts`` → ``fauna.drafts.put``) and
restored on the next app launch (``restoreDrafts`` → ``fauna.drafts.get``), so it
survives a page reload (the web "app restart") under the same identity.

Why the **new-thread** composer (not a per-thread reply draft): a per-thread draft
is keyed by ``ThreadId``, and the conversations thread store is in-memory — a
reload rebuilds threads from the nest, so a test-helper thread's id does not
survive. The new-thread compose is a single global draft slot
(``conversations.md`` § Persistence: "a half-written new message survives
switching") tied to no server thread, so it is the stable, representative draft
for a restart round-trip. ``start_new_conversation`` PRESERVES a stashed/restored
new-thread draft, so re-opening ``+`` after the reload shows the restored body.

**linux and tui joined 2026-08-04**, closing the roster at all seven apps. linux's
wiring (``apps/fauna-linux/src/conversations/drafts.rs``) had been live since
2026-06-21 and was simply never covered here — a *test* gap, not a client gap.
tui's leg was genuinely absent (``conversations.md`` § Implementation status
today's "Drafts v2" row read ``open``) and landed with this marker:
``apps/fauna-tui/src/conversations/drafts.rs``, the same shared ``DraftsSync``
over the same ``"conversations"`` rail. Neither needs ``real_conversations`` —
both run the real ``ConversationsSession`` for every e2e login by construction
(conftest ``_apply_real_conversations_env``) — but both are marked so the flag's
session-wide env stays consistent when the suite runs cross-app.

Marked ``web`` + ``macos`` + ``ios``: all three legs are wired today (the macOS
leg landed when apple wired the conversations drafts autosync into the in-process
e2e path — ``ConversationsVM.attachDraftsSync`` called from ``applySessionPatch``
— and
``test_apple_track_a_diag::test_drafts_save_restore_isolation_macos`` flipped to
``BOTH OK`` end-to-end). The other apps extend this test as their legs land
(one leg per client, per the goal-doc Implementation-status notes; ``--client``
deselects the unwired ones). The iOS leg flipped GREEN 2026-06-23: the
once-suspected iOS "new-thread composer-open
gap" does NOT reproduce — the pushed ``NewThreadComposeView`` fields
(``dm-text-field`` et al.) DO ``.onAppear``-register in-process (byte-identical
to the macOS control), so the compose chokepoint this test drives works on iOS.
"""

import secrets
import time

import pytest

# `real_conversations` makes the windows app launch the REAL ConversationsSession
# receive loop under e2e (FAUNA_E2E_REAL_CONVERSATIONS), where the drafts autosync is
# wired (App.StartE2eRealConversationsAsync) — the production drafts build sits in the
# !FAUNA_E2E_BRIDGE block the e2e never reaches, so the windows leg needs the real path.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.real_conversations,
    # The debounce itself must land this draft — see the marker's entry in
    # pytest.ini. Keeps a `drafts_autosave_window_ms` run from silently
    # recording this as a product red.
    pytest.mark.drafts_production_window,
]


@pytest.fixture
def drafts_actor(request, app, nest_instance):
    """``(app, user)`` logged in as a DEDICATED fresh actor, whose ``__drafts``
    plane is empty by construction.

    Both tests type into the new-thread composer and compare its body with what
    they typed, so a new-thread draft already resting on the actor is restored
    under the typing and the body comes back with it in front. On the shared
    session ``test_user`` that is not hypothetical: a tui run of the
    ``drafts-survive`` selection failed both preconditions reading
    ``"device B's half-written new messagedraft that must survive..."`` — the
    draft ``test_conversations_across_own_devices.py`` writes from its second
    device, restored here — and the second test then inherited the first's body
    too, because a failed test never reaches its own discard. Order decided
    green or red. A fresh actor makes the empty starting composer this module
    asserts against locally true, whatever any other test leaves behind, present
    or future (the same trade ``ungranted_app`` makes in conftest).
    """
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    # `verify_live_actor=True` by the barrier's own rule: the premise is a
    # dedicated actor, and a declined switch would type into the shared one's.
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app, user


def _wait_compose_body(app, expected: str, timeout: float = 5.0) -> str:
    """Poll the composer body until it equals ``expected`` (or time out). The
    snapshot refresh after a mutator is async, so read condition-based rather
    than once (e2e action-layer convention). Returns the last value read."""
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = app.conversations.compose_body_text()
        if last == expected:
            return last
        time.sleep(0.1)
    return last


def _wait_drafts_persisted(node_url, actor_id, signing_key, baseline_blob, timeout: float = 12.0):
    """Poll the nest ``__drafts`` plane (via a fresh side-channel device for the
    same actor) until the debounced ``fauna.drafts.put`` lands — i.e. the blob
    appears / changes from ``baseline_blob``. Returns the observed blob (or None
    on timeout). This is a *deterministic* save-confirmation: a fixed sleep races
    the async debounce + WS round-trip, which is fast enough on web but not on the
    slower clients (macOS tier_3 under load), so the reload would tear the app
    down before the save lands and there is nothing to restore. Reading the nest's
    own ``__drafts`` blob is also exactly the persistence contract under test."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    deadline = time.time() + timeout
    while time.time() < deadline:
        with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
            blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")
        if blob is not None and blob != baseline_blob:
            return blob
        time.sleep(0.5)
    return None


@pytest.mark.feature("drafts-survive")
def test_new_thread_compose_draft_survives_app_restart(drafts_actor, nest_instance):
    app, test_user = drafts_actor
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    draft_body = "draft that must survive an app restart 4827"

    # 0. Baseline the nest drafts blob so step 2 can detect the save landing.
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        baseline_blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")

    # 1. Open the new-thread composer and type a draft body. No recipient is
    #    needed — the compose bar renders in new-thread mode off the snapshot.
    #    Each edit live-forwards to the shared manager and schedules a debounced
    #    `fauna.drafts.put` (the web leg's save trigger, hooked at the page's
    #    `run()` UI-action chokepoint).
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", draft_body)
    typed = _wait_compose_body(app, draft_body)
    assert typed == draft_body, (
        "precondition: the draft body must be typed into the composer before reload "
        f"(got {typed!r}); error={app.error_text()!r}"
    )

    # 2. Wait for the debounced draft-save to actually land on the nest before we
    #    tear the app down — a reload mid-put would drop the draft with nothing to
    #    restore. Poll the `__drafts` plane (deterministic) rather than a fixed
    #    sleep that races the async round-trip on the slower clients.
    saved_blob = _wait_drafts_persisted(node_url, actor_id, signing_key, baseline_blob)
    assert saved_blob is not None, (
        "precondition: the debounced fauna.drafts.put must reach the nest __drafts "
        "plane before reload (save-side persistence did not land)"
    )

    # 3. Restart the app: a full page reload (the web equivalent of force-quit +
    #    relaunch). The Ed25519 identity persists in localStorage (`fauna_secret`),
    #    so the reloaded SPA logs back in as the same actor and its launch
    #    `restoreDrafts` fetches the persisted `__drafts` blob.
    app.driver.hard_reload()

    # 4. Re-open conversations and give the relaunched manager time to build +
    #    restore drafts (getConversationsManager awaits restoreDrafts; the layout
    #    receive poll builds it on the first authenticated tick). Restore MUST
    #    complete before we click `+` — start_new_conversation seeds a fresh empty
    #    composer only when no draft is stashed, so clicking before restore would
    #    create an empty draft the late restore then silently replaces underneath.
    app.conversations.navigate()
    time.sleep(2.0)

    # 5. Re-open the new-thread composer. start_new_conversation PRESERVES the
    #    restored new-thread draft (conversations.md § Persistence), so the body
    #    must reappear in the composer.
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)

    # 6. The restored draft body must show in the composer.
    restored = _wait_compose_body(app, draft_body)
    assert restored == draft_body, (
        "the new-thread compose draft did not survive the app restart "
        f"(expected {draft_body!r}, got {restored!r}); error={app.error_text()!r}"
    )

    # 7. Discard the draft this test just proved is DURABLE — the cleanup is part
    #    of the test, not an afterthought. `__drafts` is per-actor nest state; this
    #    module now runs on a dedicated actor (`drafts_actor`), but it ran on the
    #    session-scoped `test_user` for months, and a draft left behind there is
    #    restored into the NEXT test's composer on every app that wires drafts. That
    #    was not hypothetical: it broke `test_conversations_new_thread_cancel.py[tui]`,
    #    whose composer came up holding *this* test's body with its own typed on
    #    the end ("...restart 4827half-written draft to discard 7731"). The leak
    #    was latent for web/macos/windows the whole time — they wire drafts too —
    #    and only surfaced when the tui + linux legs joined this file.
    #
    #    Cancel is the correct instrument, not a side-channel wipe: it is the
    #    user-facing discard (`conversations.md` § Persistence — only an explicit
    #    cancel or a successful send clears the new-thread draft), it goes
    #    through the UI like every other mutation here (e2e-conventions.md
    #    point 8), and the nest REFUSES an empty `fauna.drafts.put` blob
    #    (`drafts_handlers.rs:119`), so there is no "clear it" RPC to call.
    #
    #    Asserted, never best-effort: a cleanup that silently no-ops re-arms the
    #    exact cross-test leak this closes, and the next victim would fail far
    #    away with a baffling message instead of here with this one.
    app.driver.click("new-conversation-cancel")
    assert _wait_drafts_persisted(node_url, actor_id, signing_key, saved_blob) is not None, (
        "cleanup: discarding the draft must persist the emptied draft set to the "
        "nest __drafts plane, or it leaks into the next test's composer. The "
        "assertion above already PASSED — the drafts feature works; this is the "
        "teardown failing, so look at new-conversation-cancel on this app, not at "
        "draft persistence."
    )


@pytest.mark.feature("drafts-survive")
def test_a_restored_drafts_recipient_is_checked_again_without_retyping(
    drafts_actor, nest_instance
):
    """``conversation-drafts.md`` § Persistence — *A restore that FILLS a
    recipient picker owes it a probe*: the picker's typed address rests, its
    probe cannot (it rests ``idle``), so the restore issues the probe itself.
    Type an address (probed, never committed as a chip) and a body, let the
    autosave land, restart; the reopened composer holds the address and its
    ``recipient-resolve-status`` leaves ``idle`` for ``resolved`` with no
    keystroke. A restore that skipped the probe leaves it ``idle`` for good —
    the red arm. No settle sleep: the restore fills an untouched composer
    whenever it lands, so the wait polls the picker itself (convention 14)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from tests.api import conv_api

    app, test_user = drafts_actor
    conv = app.conversations
    node_url = nest_instance["url"]
    port = nest_instance["port"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])
    # A same-nest peer the Fauna rail claims, addressed as the nest echoes its
    # handles (`test_recipient_picker.py::test_resolves_fauna_handle`'s recipe).
    peer = conv_api.reachable_peer(
        port, nest_instance["admin"]["signing_key"], test_user["actor_id_hex"]
    )
    served = conv_api.actor_by_handle(port, test_user, peer["handle"])
    address = f"{peer['handle']}@{served['domain']}"
    body = f"draft whose recipient is re-checked {secrets.token_hex(3)}"

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        baseline_blob = dev.call("fauna.drafts.get", {"path": "conversations"}).get("blob")

    state = conv.resolve_recipient(address)
    assert state == "resolved", f"precondition: the typed address resolves live ({state!r})"
    app.driver.type_text("dm-text-field", body)
    typed = _wait_compose_body(app, body)
    assert typed == body, f"precondition: the body is typed (got {typed!r}); error={app.error_text()!r}"
    saved_blob = _wait_drafts_persisted(node_url, actor_id, signing_key, baseline_blob)
    assert saved_blob is not None, (
        "precondition: the debounced fauna.drafts.put must reach the nest before the restart"
    )

    app.driver.hard_reload()
    conv.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("recipient-picker-input", timeout=10.0)

    def picker():
        return (
            app.driver.get_text("recipient-picker-input"),
            app.driver.get_attr("recipient-resolve-status", "state"),
        )

    deadline = time.time() + 30.0
    seen = picker()
    while time.time() < deadline and seen != (address, "resolved"):
        time.sleep(0.2)
        seen = picker()
    assert seen == (address, "resolved"), (
        "the restored draft's recipient should come back and be probed again with "
        f"no keystroke: (input, state) = {seen!r}; 'idle' with the address means "
        f"the restore never issued the probe. body={conv.compose_body_text()!r} "
        f"error={app.error_text()!r}"
    )
    assert _wait_compose_body(app, body) == body, "the body came back with it"

    # Discard the durable draft (the module's cleanup rule — the test above, step 7).
    app.driver.click("new-conversation-cancel")
    assert _wait_drafts_persisted(node_url, actor_id, signing_key, saved_blob) is not None, (
        "cleanup: the discarded draft must persist, or it leaks into the next test"
    )

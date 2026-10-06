"""Tier_3 proof that an own fauna-native MLS message survives an app restart
**immediately** after the send action returns — no settle gate.

The sibling ``test_conversations_history_survives_app_restart.py`` proves the
restore + render paths by *gating on the debounced replica autosave having
settled* before restarting. This file removes that gate, pinning the durability
contract itself (``docs/goal/behavior/devices.md`` § Durability rules, Rule 3 —
durable-before-done): **the send action completes only after the own message's
``history/<ch>`` replica slice is durably persisted**, so a quit at any moment
after the send UI action finishes can no longer lose the message.

Before that contract landed this was the user's actual loss window: an own
message sent within ``REPLICA_DEBOUNCE`` (1.5 s) of an app quit was permanently
lost to its author — a sender cannot MLS-decrypt its own application messages,
so the log re-walk can never rebuild it. The restart here happens straight
after the send bridge command returns (well inside 1.5 s), which is exactly the
window the debounced autosave cannot cover.

Windows-only as the durability contract's localization proof: windows' driver
preserves the launch's data dir with no opt-in, so the immediate restart is a
pure same-device quit. The restart-survival *outcome* is witnessed on every
other column by ``test_conversations_history_survives_restart_cross_app.py``
(the ``preserve_state_across_relaunch()`` pin); that sibling is settle-gated on
purpose, so the no-grace-period contract this file pins stays proven here.

Two red shapes this distinguishes (both are the same root gap):

* no FaunaMls thread returns at all — not even the (empty) ``history/<ch>``
  blob became durable before the quit;
* the thread lists but ``message_count`` is 0 / zero bubbles render — an empty
  mid-bootstrap slice landed and the post-append save never did.
"""

from __future__ import annotations

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.conversations_restart import wait_fauna_thread, wait_rendered_messages
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.windows,
    pytest.mark.real_conversations,
]

LINE = "durable the instant send returns"


def _fauna_threads(app):
    return [t for t in app.conversations.list_threads() if t.rail == "FaunaMls"]


@pytest.mark.feature("conversations")
def test_conversations_own_message_survives_an_immediate_restart(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    if not app.driver.is_windows():
        skip_unbuilt(
            app.driver,
            surface="a same-device-restart mls.db data-dir persistence proof",
            detail="only the windows recover() preserves the native mls.db "
            "data dir with no opt-in (a stable per-driver-instance data dir "
            "kept across relaunch); the other columns' restart-survival is "
            "witnessed by test_conversations_history_survives_restart_"
            "cross_app.py via the preserve_state_across_relaunch() pin",
        )

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    app.conversations.enable_real_faunamls()

    # An API-tier peer publishing real key packages so the MLS bootstrap can
    # fetch one; it never has to decrypt anything.
    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])

    # ── Send one own message on a fresh 1:1, then restart IMMEDIATELY. No
    # settle gate, no render polling first: everything between the send action
    # returning and the quit is time the debounced autosave could steal, and
    # the contract under test is that NO such grace period is needed. The
    # pre-send channel snapshot excludes threads restored from earlier tests in
    # this session (the session-scoped actor + nest are shared). ──
    pre_existing = {t.channel_id_hex for t in _fauna_threads(app) if t.channel_id_hex}
    app.conversations.real_resolve_send_new(peer["actor_id_hex"], LINE)
    channel_hex = next(
        (
            t.channel_id_hex
            for t in _fauna_threads(app)
            if t.channel_id_hex and t.channel_id_hex not in pre_existing
        ),
        None,
    )
    assert channel_hex, "the 1:1 thread should bind a channel after the first send"

    app.driver.hard_reload()
    app.conversations.enable_real_faunamls()

    restored = wait_fauna_thread(app, channel_hex)
    assert restored is not None, (
        f"after an immediate restart no FaunaMls thread on channel {channel_hex} "
        "came back — not even the history/<ch> slice became durable before the "
        "quit (devices.md § Durability rules, Rule 3: the send action must not "
        "complete before the own message is durable)"
    )
    assert restored.message_count >= 1, (
        f"the restored thread lists {restored.message_count} message(s), expected "
        ">= 1: the own message sent right before the restart was lost — the "
        "history slice restored without it, and a sender cannot MLS-decrypt its "
        "own application messages, so it is unrecoverable (the KNOWN-GAP loss)"
    )

    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    after = wait_rendered_messages(app, 1)
    assert after >= 1, (
        f"after the restart the thread rendered {after} message bubble(s), "
        f"expected >= 1 (the store has {restored.message_count})"
    )
    texts = [app.driver.get_text("dm-message-text", index=i) for i in range(after)]
    assert any(LINE in t for t in texts), f"own message missing from {texts}"

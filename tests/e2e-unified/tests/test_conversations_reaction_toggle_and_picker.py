"""tier_3 — taking a reaction back, and reacting with an emoji outside the quick set.

The two outcomes ``docs/features/reactions-and-message-delete.md`` 4 and 5 name,
witnessed on one real seat over a real FaunaMls thread. Owner of the promises:
``docs/goal/ui/conversations.md`` § Reactions & message delete — the pill is
tap-to-toggle, and ``dm-reaction-more-button`` reaches a fuller picker beyond
the six ``dm-reaction-option`` quick-set emoji.

**One seat is the honest shape here.** Both outcomes are about what *your own*
reaction does to *your own* view: you place it, you see it marked as yours, you
take it back. Nothing in either sentence is about a second engine agreeing, so
a second seat would buy only wall-clock. The cross-member half — a peer seeing
the pill — is already witnessed by
``test_conversations_reactions.py::test_react_cross_member_peer_sees_pill``, and
outcome 4's "gone on the peer's copy" rides the same toggle this file drives
through the same ``manager.toggle_reaction`` door.

**No app marker, deliberately.** Like ``test_fauna_mls_real_roundtrip.py``,
every step here is the shared action layer's (``react_to_message``,
``toggle_reaction_pill``, ``react_with_custom_emoji``), so the journey speaks
for whichever column the run selects rather than for a named few — the
difference the catalog's ``Gap.reach`` turns on
(``feature-catalog.md`` § Cell semantics). What each column still owes is a
*run* on its own machine, not a lift.

**The fuller picker is the one sanctioned per-app divergence** (``ui.yaml``'s
menu component): a native chooser where the platform has one (a
``GtkEmojiChooser`` on linux, the emoji2 ``EmojiPickerView`` on android), and
everywhere else a free-entry emoji field that re-uses the
``dm-reaction-more-button`` id as its input while open (tui, apple, web,
windows — ``ui/conversations.md`` § Reactions & message delete → *Rendering /
picker glue*). The platform-neutral
assertion is therefore the one the outcome states — *an emoji outside the quick
set ends up as a pill* — and every per-platform step to get there lives in
``actions/conversations.py``, never here (e2e rule 7).

Latency-independent throughout (convention 14): every wait polls for the pill
set to *become* what the act should make it, and no step sleeps a fixed span.
"""

from __future__ import annotations

import secrets
import time

import pytest

from common import create_actor_and_register
from helpers.budgets import RPC_ROUNDTRIP_S
from tests.api import conv_api
from tests.api.conv_api import mint_key_packages as _mint_keypackages

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

PILL_BUDGET_S = 30.0
"""For a reaction placed through the ⋯ menu to fold onto the bubble. The toggle
is local-first — the manager applies it to its own snapshot before the envelope
leaves — so this is far above any non-pathological render."""

OFF_QUICK_SET = "🦊"
"""An emoji the six-item quick set does not carry, which is the whole point of
outcome 5. Matches the fox the tui's own `Action` round-trip unit test uses."""

_QUICK_SET = ["👍", "❤️", "😂", "😮", "😢", "🙏"]


def _pill_emoji(app) -> list[str]:
    """Every ``dm-reaction-pill``'s emoji, in paint order.

    A pill's text is ``"{emoji} {count}"`` plus, on a reaction this seat
    placed, a trailing own-marker (the tui paints ``✓``; the GUI apps vary the
    fill). Splitting on whitespace and keeping the head is the one reading that
    is true of all of them, and it is what makes this file's assertions
    platform-neutral.
    """
    out = []
    for i in range(app.driver.count("dm-reaction-pill")):
        text = app.conversations.reaction_pill_name(index=i)
        out.append(text.split()[0] if text.split() else "")
    return out


def _wait_pills(app, predicate, what: str, budget_s: float = PILL_BUDGET_S) -> list[str]:
    """Poll the pill set until ``predicate`` holds, then return it."""
    deadline = time.time() + budget_s
    seen: list[str] = []
    while time.time() < deadline:
        try:
            seen = _pill_emoji(app)
            if predicate(seen):
                return seen
        except Exception:
            pass
        time.sleep(0.25)
    raise AssertionError(
        f"{what}: the pill set never satisfied it within {budget_s}s — last read {seen!r}\n"
        f"{app.driver.diagnose('dm-reaction-pill')}"
    )


def _own_thread_with_a_message(app, nest_instance, test_user) -> str:
    """Setup (convention 8 carve-out (b)): a real bound 1:1 carrying one message
    of this seat's own, which is what both outcomes react to. Its body carries a
    fresh token so an earlier run's thread on the session-scoped account can
    never be the one we pin."""
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    run = secrets.token_hex(4)
    body = f"react to me {run}"

    peer = create_actor_and_register(port, admin_signing_key=admin_sk)
    conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
    # Without the accept the Welcome is refused under the default `allow_knock`
    # inbox mode (`direct-messages.md` § Reach policy).
    conv_api.accept_contact(port, peer, test_user["actor_id_hex"])
    app.conversations.real_resolve_send_new(peer["actor_id_hex"], body)

    deadline = time.time() + RPC_ROUNDTRIP_S
    while time.time() < deadline:
        match = next(
            (
                t
                for t in app.conversations.list_threads()
                if t.rail == "FaunaMls" and body in (t.snippet or "")
            ),
            None,
        )
        if match is not None and match.channel_id_hex:
            app.conversations.open_thread_by_id(match.thread_id)
            return match.thread_id
        time.sleep(0.5)
    raise AssertionError(
        f"the 1:1 carrying {body!r} never surfaced its own echo within {RPC_ROUNDTRIP_S}s"
    )


@pytest.mark.feature("reactions-and-message-delete")
def test_tapping_your_own_reaction_takes_it_back(real_faunamls_app, nest_instance, test_user):
    """Outcome 4: react, see the pill marked as yours, tap it, the pill is gone.

    The pill's disappearance — not merely a count going to zero — is the
    observable, because the shared render drops a group with no reactions left
    rather than painting an empty one."""
    app = real_faunamls_app
    _own_thread_with_a_message(app, nest_instance, test_user)

    app.conversations.react_to_message(message_index=0, reaction_index=0)
    pills = _wait_pills(
        app,
        lambda seen: _QUICK_SET[0] in seen,
        "the quick-set reaction folding onto the bubble",
    )
    assert pills.count(_QUICK_SET[0]) == 1, f"one pill per emoji, not one per tap: {pills}"
    mine = app.conversations.reaction_pill_name(index=pills.index(_QUICK_SET[0]))
    assert mine.split()[0] == _QUICK_SET[0], mine
    assert "1" in mine, f"the pill counts the one reaction placed: {mine!r}"

    # The take-back: tapping the pill you placed toggles it off.
    app.conversations.toggle_reaction_pill(
        message_index=0, pill_index=pills.index(_QUICK_SET[0])
    )
    _wait_pills(
        app,
        lambda seen: _QUICK_SET[0] not in seen,
        "the tapped-back reaction leaving the bubble",
    )
    assert not app.has_error(), f"a take-back is never a refusal: {app.error_text()!r}"


@pytest.mark.feature("reactions-and-message-delete")
def test_the_fuller_picker_reacts_with_an_emoji_outside_the_quick_set(
    real_faunamls_app, nest_instance, test_user
):
    """Outcome 5: beyond the six quick reactions, a fuller picker reacts with any
    emoji — asserted as the outcome states it, on the pill rather than on any
    one platform's picker widget."""
    app = real_faunamls_app
    _own_thread_with_a_message(app, nest_instance, test_user)

    assert OFF_QUICK_SET not in _QUICK_SET, "the point of the outcome"
    app.conversations.react_with_custom_emoji(OFF_QUICK_SET, message_index=0)

    pills = _wait_pills(
        app,
        lambda seen: OFF_QUICK_SET in seen,
        f"the off-quick-set {OFF_QUICK_SET} folding onto the bubble",
    )
    assert not app.has_error(), f"the fuller picker is never a refusal: {app.error_text()!r}"
    assert set(pills) & set(_QUICK_SET) == set(), (
        f"only the emoji picked through the fuller picker is on the bubble: {pills}"
    )

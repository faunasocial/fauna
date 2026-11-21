"""A room offers each person only the controls their role can use: what a
plain member's role never gives them is not painted at all, and the one
control the owner can unblock — the walk-out — is painted dead with its reason
and the hand-over standing live above it.

Goal: ``docs/goal/ui/conversations.md`` § Errors & edge cases → *Controls this
person can never use* (applying ``docs/goal/ui/README.md`` § Offer only what is
possible, rules 2 and 6): every thread and room control gated by the viewer's
role answers the shared ``ThreadCapabilities::possibility(control, my_role)``;
**absent** is every ``false`` but one — a role only the owner or an admin
grants is nothing this person can make true from this app — and **blocked** is
the owner's walk-out, shown disabled with ``error.send.room_owner_cannot_leave``
as its reason, the hand-over controls live above it in the same editor. A
viewer the policy editor is absent for reads the room's settings as plain
lines where the controls would sit.

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this person can never
use is shown. So this journey fails on a member's control painted greyed — the
shape windows still paints (its trickle-down row has not landed), which is why
windows carries no mark here, while ``test_conversation_room_leave.py`` and
``test_conversation_room_roles.py`` accept either shape. The shared
predicate's machine tests (``libs/fauna-conversations/src/thread_controls.rs``)
and tui's walk invariant I8 are not that witness.

**The reason is read as painted.** ``PlatformDriver.blocked_reason`` answers
what the app shows beside the control, compared with the ``S.`` string of the
predicate's reason key, never a literal; an app whose driver cannot read it
yet takes the ``blocked-reason-read`` surface gate. A journey never presses a
disabled control (the test agent refuses it, convention 11): the owner's
press-refusal is ``leave_room``'s own backstop, pinned in shared Rust.

**Two seats, one nest** (``helpers.room_seats.room_seat_pair``) — the fewest
the room fixtures allow: the founder founds a community room with the member,
every step a gesture (convention 8), so the founder is its owner and the
member a plain member under the initial ``invite`` join rule. Nothing is
mutated past the founding; every wait is a deadline poll on a projected role
or a repaint (convention 14).

**The plain-line read.** A member reads the join rule, the history policy,
the home nest's read and each labeler row as plain lines where the controls
would sit (``room-join-rule-line``, ``room-history-policy-line``,
``room-nest-read-line``, ``room-labeler-line[i]``): each line is present on the
member's seat carrying the value the owner's control holds (the select's token
as the line's ``value`` attribute, the toggle's ``checked`` as the line's), and
none is painted on the owner's seat, where the controls stand. So the second
journey fails when a member is shown neither the control nor its line. It
takes the ``room-settings-plain-line-read`` surface gate: tui paints the ids,
the other apps tag their lines in their lift rows.
"""

from __future__ import annotations

import secrets

import pytest

from helpers import budgets
from helpers.app_surface import require_surface
from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MEMBERSHIP_BUDGET_S,
    found_community_room,
    index_of,
    room_app,
    room_seat_pair,
    seat_element,
    seat_story,
    thread_by_channel,
    wait_for,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads; the five others paint the shared answer too
    # (`ui/conversations.md` § *Controls this person can never use*,
    # implementation status). windows is deliberately unmarked: it still
    # paints these greyed, which this journey must fail.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
]

# The controls a plain member's role never gives them, each absent (never
# greyed): the invite, the policy editor (`ThreadControl::SetPolicy`), the
# admin switch and the hand-over.
POLICY_CONTROLS = (
    "room-join-rule-select",
    "room-history-policy-select",
    "room-nest-read-toggle",
    "room-settings-save-button",
)
MEMBER_ABSENT_IN_EDITOR = POLICY_CONTROLS + (
    "room-admin-toggle",
    "room-owner-transfer-button",
)
# What a member reads where the policy controls would sit, each painted
# exactly when its control is absent.
PLAIN_LINES = (
    "room-join-rule-line",
    "room-history-policy-line",
    "room-nest-read-line",
    "room-labeler-line",
)


def _diag(driver, element_id: str, scope: str | None = None) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'), scope=scope)}, "
        f"{driver.diagnose('error-message')}"
    )


def _blocked_with(driver, element_id: str, reason: str) -> bool:
    if driver.is_absent(element_id):
        return False
    if driver.is_enabled(element_id):
        return False
    return driver.blocked_reason(element_id) == reason


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it; the asserts after the poll name every part that failed
    (convention 6)."""
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason), budgets.UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which half failed
    diag = _diag(driver, element_id)
    assert not driver.is_absent(element_id), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id), (
        f"{what}: {element_id} is live while a room is never owner-less: {diag}"
    )
    painted = driver.blocked_reason(element_id)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _assert_absent(seat, element_id: str, *, what: str) -> None:
    """``element_id`` is not painted at all — a greyed control fails here."""
    d = seat.app.driver
    assert d.is_absent(element_id), (
        f"{what}: {element_id} is shown (enabled={d.is_enabled(element_id)}) to a "
        f"viewer whose role can never use it — it must not be painted at all "
        f"(ui/conversations.md § Controls this person can never use):\n"
        f"{seat_element(seat, element_id)}"
    )


@pytest.mark.feature("group-conversations")
def test_a_member_is_offered_no_control_their_role_lacks_and_the_owners_leave_names_the_hand_over(
    room_seat_pair, nest_instance
):
    """The owner's walk-out is painted dead with the hand-over sentence as its
    reason, the hand-over live above it in the same editor; a plain member's
    thread and editor paint none of the invite, policy, admin or hand-over
    controls, and the member's own walk-out is live."""
    founder, member = room_seat_pair
    require_surface(founder.app.driver, "blocked-reason-read")
    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello offer-possible {secrets.token_hex(4)}"
    )

    # ── the roles, read off each seat's projection ───────────────────────
    owner_view = wait_for(
        "the founder's projection naming both members, as Owner",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and member.actor_hex in t.participant_actor_ids
        and (t.room or {}).get("my_role") == "Owner",
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    member_view = wait_for(
        "the member's projection reading Member",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Member",
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    policy = (member_view.room or {}).get("policy") or {}
    assert policy.get("join_rule") == "Invite", (
        f"the founding's initial policy lets only the owner and admins invite, so the "
        f"member's invite is something their role lacks: {member_view.room}"
    )

    # ── (a) the owner: the walk-out blocked with its reason, the hand-over live ──
    od = founder.app.driver
    founder.app.conversations.open_thread_by_id(founder_thread)
    founder.app.conversations.open_room_settings()
    _assert_blocked_with(
        od, "room-leave-button", S.error.send.room_owner_cannot_leave,
        what="the owner's walk-out",
    )
    heir = index_of(owner_view, member.actor_hex)
    wait_until(
        lambda: _transfer_live(od, heir),
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the hand-over to the member is not live in the owner's editor, "
        "where the walk-out's reason sends them:\n"
        f"{seat_element(founder, 'room-owner-transfer-button')}",
    )
    # The policy editor is live for the owner — so its absence on the member's
    # seat below is the role's answer, never an editor that did not paint.
    for element_id in ("room-join-rule-select", "room-history-policy-select",
                       "room-settings-save-button"):
        assert od.is_enabled(element_id), (
            f"the owner's {element_id} must be live: {_diag(od, element_id)}"
        )

    # ── (b) the member: nothing their role lacks is painted ──────────────
    md = member.app.driver
    mconv = member.app.conversations
    mconv.open_thread_by_id(member_thread)
    wait_until(
        lambda: md.is_visible("thread-room-settings-button"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the member's room header never painted:\n{seat_story(member)}",
    )
    _assert_absent(member, "thread-add-participant-button",
                   what="a plain member under the invite join rule")

    mconv.open_room_settings()
    wait_until(
        mconv.room_leave_enabled,
        budgets.UI_SETTLE_S,
        diagnose=lambda: "a plain member's walk-out must be live: "
        f"{_diag(md, 'room-leave-button')}",
    )
    for element_id in MEMBER_ABSENT_IN_EDITOR:
        _assert_absent(member, element_id, what="a plain member's room editor")
    assert md.blocked_reason("room-leave-button") is None, (
        f"a live walk-out paints no reason: {_diag(md, 'room-leave-button')}"
    )


@pytest.mark.feature("group-conversations")
def test_a_member_reads_the_room_settings_as_plain_lines_where_the_controls_would_sit(
    room_seat_pair, nest_instance
):
    """A plain member's room editor paints, in place of each policy control
    their role lacks, a plain line carrying the value the owner's control
    holds; the owner's editor paints none of those lines beside its live
    controls. Fails when a member is shown neither the control nor its line."""
    founder, member = room_seat_pair
    require_surface(member.app.driver, "room-settings-plain-line-read")
    room_id, founder_thread, member_thread = found_community_room(
        founder, member, f"hello plain-lines {secrets.token_hex(4)}"
    )
    wait_for(
        "the founder's projection naming the member, as Owner",
        lambda: thread_by_channel(founder.app, room_id),
        lambda t: t is not None and member.actor_hex in t.participant_actor_ids
        and (t.room or {}).get("my_role") == "Owner",
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(founder),
    )
    wait_for(
        "the member's projection reading Member",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("my_role") == "Member",
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )

    # ── (a) the owner: the controls live, no plain line beside them ──────
    od = founder.app.driver
    founder.app.conversations.open_thread_by_id(founder_thread)
    founder.app.conversations.open_room_settings()
    wait_until(
        lambda: od.is_enabled("room-join-rule-select"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the owner's policy editor never went live: "
        f"{_diag(od, 'room-join-rule-select')}",
    )
    # What the owner's controls hold — the values the member's plain lines
    # must read — and none of those lines painted beside the owner's controls.
    owner_holds = {
        "room-join-rule-line": od.get_text("room-join-rule-select"),
        "room-history-policy-line": od.get_text("room-history-policy-select"),
    }
    owner_nest_read = (
        None if od.is_absent("room-nest-read-toggle")
        else od.get_attr("room-nest-read-toggle", "checked")
    )
    owner_labelers = [
        od.get_attr("room-labeler-toggle", "checked", index=i)
        for i in range(od.count("room-labeler-toggle"))
    ]
    for element_id in PLAIN_LINES:
        assert od.count(element_id) == 0, (
            f"the owner's editor paints {element_id} beside the control it stands in "
            f"for — a plain line is a member's read, never both:\n"
            f"{seat_element(founder, element_id)}"
        )

    # ── (b) the member reads the settings as plain lines ────────────────
    md = member.app.driver
    mconv = member.app.conversations
    mconv.open_thread_by_id(member_thread)
    wait_until(
        lambda: md.is_visible("thread-room-settings-button"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the member's room header never painted:\n{seat_story(member)}",
    )
    mconv.open_room_settings()
    for element_id, token in owner_holds.items():
        _assert_line(member, element_id, "value", token)
    if owner_nest_read is None:
        assert md.count("room-nest-read-line") == 0, (
            "the owner's editor carries no nest read, so the member's carries no "
            f"line for one:\n{seat_element(member, 'room-nest-read-line')}"
        )
    else:
        _assert_line(member, "room-nest-read-line", "checked", owner_nest_read)
    wait_until(
        lambda: md.count("room-labeler-line") == len(owner_labelers),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the member should read {len(owner_labelers)} labeler line(s), "
        f"one per row of the owner's set:\n{seat_element(member, 'room-labeler-line')}",
    )
    for i, checked in enumerate(owner_labelers):
        painted = md.get_attr("room-labeler-line", "checked", index=i)
        assert painted == checked, (
            f"the member's room-labeler-line[{i}] reads checked={painted!r}, the "
            f"owner's toggle holds {checked!r}:\n{seat_element(member, 'room-labeler-line')}"
        )

def _assert_line(seat, element_id: str, attr: str, expected: str | None) -> None:
    """``element_id`` is painted on ``seat`` as a plain line, non-empty, its
    ``attr`` the value the owner's control holds (convention 6: the failure
    names what was painted)."""
    d = seat.app.driver
    wait_until(
        lambda: d.count(element_id) == 1,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"a member must read {element_id} where the control would sit "
        f"(ui/conversations.md § Controls this person can never use):\n"
        f"{seat_element(seat, element_id)}",
    )
    assert d.get_text(element_id), f"{element_id} is painted empty:\n{seat_element(seat, element_id)}"
    painted = d.get_attr(element_id, attr)
    assert painted == expected, (
        f"{element_id}'s {attr} reads {painted!r}, the owner's control holds "
        f"{expected!r}:\n{seat_element(seat, element_id)}"
    )


def _transfer_disabled(driver, index: int) -> str | None:
    """``room-owner-transfer-button[index]``'s ``disabled`` attribute — the
    cross-app capability-gating read (``PlatformDriver.get_attr``), used
    because ``is_enabled`` takes no index — or ``"absent"`` when the row is not
    painted."""
    if driver.count("room-owner-transfer-button") <= index:
        return "absent"
    return driver.get_attr("room-owner-transfer-button", "disabled", index=index)


def _transfer_live(driver, index: int) -> bool:
    """Whether ``room-owner-transfer-button[index]`` is painted and live."""
    return _transfer_disabled(driver, index) in ("false", None)

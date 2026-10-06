"""Seats and waits for the room-model journeys (``conversation-rooms.md``).

A *seat* is one signed-in launch of the app under test on its own account —
the ``_seat`` shape ``test_offline_share_two_seat.py`` established: each
launch keeps the driver's DEFAULT per-launch world (convention 10), so N
seats are N installs of one app, never N accounts in one process. That is
the only honest shape for a room: the roles are enforced by each member's
OWN MLS engine refusing a commit its author may not make, and one process
holding every identity would prove nothing about who refused what.

The waits are deadline polls over latency-independent state (convention 14):
a thread bound to a channel, a snippet carrying a body, a participant count.
Each budget is far above any non-pathological version of the thing it
covers, and a green run pays only what it actually needs.
"""

from __future__ import annotations

import os
import time
from dataclasses import dataclass

import pytest

from conftest import (
    _apply_actuation_mode_env,
    _apply_r14_trust_env,
    _apply_real_conversations_env,
    _get_ios_seat_udid,
    get_available_apps,
)
from drivers import create_driver
from helpers.app_surface import skip_environment
from helpers.connection import wait_until_online
from tests.api import conv_api

ROOM_APPS = ("tui", "linux", "macos", "ios", "windows", "web", "android")
"""The apps the room journeys run on. tui leads (``conversation-rooms.md`` —
"rendered on tui first"); the six apps join through the batched trickle-down,
each arriving with its own marker AND an entry here
(``test_offline_share_two_seat.py``'s own lesson: a marker alone silently drove
tui underneath). linux joined with its leg's first tier_3 witness; its seats
take the same launch shape as tui's (the two-seat offline share already
drives linux through it). macos and ios joined with the apple leg: the same
shape, plus two launch facts ``launch_seat`` carries — the apple apps start the
real conversations session only under ``FAUNA_E2E_REAL_CONVERSATIONS``, and an
iOS seat needs a simulator device of its own (``_get_ios_seat_udid``). windows
joined with its leg: the same shape again (``seat_app_path`` falls through to
the ``windows_app_path`` fixture, and each ``WindowsBridgeDriver`` already mints
its own bridge, ``%LOCALAPPDATA%``, data dir and credential store per launch, so
three seats are three installs — ``test_offline_share_two_seat.py`` drives two
of them the same way), plus the strict-actuation opt-in below. web joined last:
a web seat is a browser page on the nest's own SPA (``seat_app_path`` answers
``None``), each launch its own bridge, Chromium and browser context — so three
seats are three installs here too — and it reads none of ``launch_seat``'s
environment, needing none of it (the comments there say why, fact by fact).
android joined with its leg: the same shape as
linux/tui — ``seat_app_path`` falls through to the ``android_app_path``
fixture, and each launch is its own install under the app-under-test's own
account, reading ``launch_seat``'s environment (the trust seed, the
real-conversations gate) exactly as tui/linux do."""

WELCOME_BUDGET_S = 150.0
"""For a Welcome to cross the nest, be applied by the newcomer's engine, and
the newcomer's thread to render bound to the channel — the same order of
budget the offline-share ceremony gives its admission."""

MESSAGE_BUDGET_S = 90.0
"""For an application message to cross the channel and fold into a peer's
thread (its snippet / ``message_count``)."""

MEMBERSHIP_BUDGET_S = 60.0
"""For a membership commit posted from the UI to reflect in the committer's
own participant count — one real nest round-trip plus the snapshot re-render."""

POLL_S = 0.5


@dataclass
class Seat:
    """One launched seat: the action layer and the actor it signed in as."""

    app: object
    actor: dict

    @property
    def actor_hex(self) -> str:
        return self.actor["actor_id_hex"]

    @property
    def handle(self) -> str:
        return self.actor["handle"]


def seat_app_path(request, app_name: str):
    """What ``launch_seat`` hands the driver as ``app_path``: the built binary
    for a native app (its ``<app>_app_path`` fixture, which builds rather than
    skips), the built ``.app`` for **ios** (``ios_setup``'s, the same
    simulator build the ``app`` fixture launches), and ``None`` for **web** — a
    web seat is a browser page on the nest's own SPA, so there is no binary to
    point at (the SPA itself is prebuilt at collection time off the ``[web]``
    parametrization). Every seat fixture resolves the path through here, so a
    new app joins in one place."""
    if app_name == "web":
        return None
    if app_name == "ios":
        return request.getfixturevalue("ios_setup")["app_path"]
    return request.getfixturevalue(f"{app_name}_app_path")


def _next_seat_ordinal(request) -> int:
    """This seat's position among the seats the current test has launched so
    far — 0 for the first. Only an iOS seat reads it: each concurrently-live
    iOS seat needs a simulator device of its own (``_get_ios_seat_udid``),
    where every other app isolates a launch by itself."""
    node = request.node
    ordinal = getattr(node, "_room_seat_ordinal", 0)
    node._room_seat_ordinal = ordinal + 1
    return ordinal


def launch_seat(nest: dict, app_path, app_name: str, request, *, actor: dict | None = None) -> Seat:
    """Register a fresh actor, launch ``app_name`` on it, and wait for the real
    FaunaMls backend to be live on that seat.

    ``actor`` seats an account that already exists on ``nest`` instead of
    registering one — a *handled* actor on a foreign federation peer
    (``provision_foreign_handled_actor``'s dict), so a seat on another nest
    can be addressed by ``handle@authority`` from a seat on the first
    (`test_conversation_room_community_cross_nest.py`). The dict carries the
    same ``signing_key`` / ``actor_id_hex`` / ``handle`` keys a registered
    actor's does."""
    from actions import ActionLayer
    from common.auth import create_actor_and_register

    config: dict = {"app_path": app_path, "url": nest["url"]}
    # A web seat is a browser page on the BUILT SPA, served by the same
    # CORS-adding proxy every web login goes through (`spa_url` →
    # `_serve_spa_proxy`, base `/app/`), and its session's `node_url` is that
    # proxy too, because the browser fetches relative to it. The nest's own URL
    # is wrong twice over: the nest serves no SPA, so a page pointed there
    # installs no `window.__fauna_callCommand` and the seat dies on its first
    # command (measured 2026-09-12 — three seats, `__fauna_callCommand not
    # installed`), and it sends no `Access-Control-Allow-Origin`, so the wasm
    # client's cross-origin WS-RPC would be refused. `conftest._login_app_as`
    # and `_build_app_config`'s web branch make exactly these two choices for
    # the `app` fixture's single seat.
    node_url = nest["url"]
    if app_name == "web":
        spa = request.getfixturevalue("spa_url")
        config["url"] = spa + "/app/"
        node_url = spa
    if app_name == "ios":
        ordinal = _next_seat_ordinal(request)
        udid = _get_ios_seat_udid(ordinal)
        if udid is None:
            skip_environment(f"could not allocate an iOS simulator for room seat {ordinal}")
        config["udid"] = udid

    if actor is None:
        actor = create_actor_and_register(
            nest["port"], admin_signing_key=nest["admin"]["signing_key"]
        )
    driver = create_driver(app_name)
    environment = {
        # The receive loop's backstop ticker; the barriers below never rely
        # on it (they anchor on receive cycles and folded commits), it only
        # keeps a green run cheap.
        "FAUNA_CONV_POLL_SECS": "2",
        "RUST_LOG": (
            f"info,fauna_{app_name}=debug,fauna_conversations=debug,fauna_mls=debug"
        ),
    }
    # The escrow-holder trust seed, default-on for every launch that reads an
    # environment (`_apply_r14_trust_env`'s docstring owns the why). web is its
    # declared absence — a browser page reads no environment, so nothing in
    # this dict reaches a web seat — and the room journeys run there anyway,
    # because nothing they assert rides the generation plane the seed unlocks:
    # a policy commit and the history slice are MLS records on the channel,
    # and the floor-roster report is a plain permission-gated RPC
    # (`fauna.conversations.room.roster_report`).
    _apply_r14_trust_env(environment, nest, request)
    # The launch-gate apps (macOS / iOS) start the REAL conversations session
    # only under `FAUNA_E2E_REAL_CONVERSATIONS` — every room journey carries
    # the `real_conversations` marker, so this sets it; tui, linux and web run
    # the real session for every login and read nothing here. The poll knob
    # below is web's too in spirit, and web needs no twin of it: its receive
    # rail already ticks every 2 s under the e2e agent
    # (`WebBridgeDriver.set_conv_poll_secs`).
    _apply_real_conversations_env(environment, request)
    # `--permissive-actuation` / `--actuation-log` reach these seats by the same
    # one path every other launch site uses, and then:
    #
    # **the room journeys ASSERT a refusal.** "A plain member's Remove is
    # greyed" is checked twice over — `is_enabled` reads false, AND driving it
    # anyway raises the automation gate's named 409 (convention 11), which is
    # the client side of "a Remove from a plain member is never authored". That
    # second assert needs refusal ON, and refusal is the DEFAULT on every room
    # app: tui, linux, macOS, iOS, web (Playwright waits for enabled, and the
    # web bridge names the refusal the same way — its `_actuate`) and windows,
    # whose FlaUI bridge flipped on 2026-09-14
    # (`flaui-bridge/ActuationGate.cs`, `WindowsRefusesDisabledActuationByDefault`).
    # Until that flip each windows seat opted in with `FAUNA_E2E_STRICT_ACTUATION`;
    # no seat needs to now.
    #
    # `--permissive-actuation` still WINS, so an enumerating sweep measures
    # instead of turning red — at the cost of the roles journey's refusal
    # assert, which is why `scripts/actuation-sweep-report.py` lists that
    # marker as DELIBERATE rather than as an offender.
    _apply_actuation_mode_env(environment, request)
    config["environment"] = environment
    driver.launch(config)
    driver.set_state(
        {
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": bytes(actor["signing_key"]).hex(),
                "actor_id": actor["actor_id_hex"],
                # A real 32-byte device id — apple's account-runtime host
                # hex-decodes this and fails by design on a wrong length
                # (`test_offline_share_two_seat.py::_seat` records the trap).
                "device_id": os.urandom(32).hex(),
            },
            "nav": {"stack": [{"view": "feed"}]},
        }
    )
    # The CONNECTION BARRIER, the same per-login precondition
    # `conftest._login_app_as` ends on — a seat is an app login like any other,
    # and this launch site was the one that skipped it. Every room gesture
    # below is an `OnlineOnly` affordance, and a seat whose transport is still
    # `"connecting"` when `set_state` returns races the WS handshake and loses
    # exactly when the box is loaded (conventions point 14). A no-op on an app
    # that does not publish the observable; `helpers/connection.py` owns both
    # rules.
    wait_until_online(driver)
    app = ActionLayer(driver)
    # A readiness poll since the real backend became unconditional at login
    # (`enable_real_faunamls`'s docstring): the seat's key-package pool —
    # every package advertising the room-policy extension — is uploaded by
    # the same activation, which is what lets a room born with this seat in
    # it be governed rather than legacy.
    app.conversations.enable_real_faunamls()
    return Seat(app=app, actor=actor)


def teardown_seats(seats: list[Seat]) -> None:
    for seat in seats:
        try:
            seat.app.driver.teardown()
        except Exception:
            pass


def _room_apps():
    available = get_available_apps()
    return [c for c in ROOM_APPS if c in available]


@pytest.fixture(params=_room_apps())
def room_app(request):
    """The app under test; its id lands in the test name (``[tui]``), which is
    what conftest's ``--app`` filter reads. Every seat shares the app. A test
    module adopts this fixture (and ``room_seats`` below) by importing the
    name — pytest registers fixtures from a module's globals."""
    return request.param


@pytest.fixture()
def room_seats(nest_instance, room_app, request):
    """Three same-app seats, three accounts, one nest — the shape every room
    journey with a governing act and a witness needs: an ``owner`` and two
    members. The two members accept the owner as a contact so the owner's
    Welcome is admitted under the default ``allow_knock`` inbox mode
    (``direct-messages.md`` § Reach policy). Yields ``(owner, second, third)``;
    each journey names the two members for its own story."""
    app_path = seat_app_path(request, room_app)
    port = nest_instance["port"]
    seats: list[Seat] = []
    try:
        owner = launch_seat(nest_instance, app_path, room_app, request)
        seats.append(owner)
        second = launch_seat(nest_instance, app_path, room_app, request)
        seats.append(second)
        third = launch_seat(nest_instance, app_path, room_app, request)
        seats.append(third)
        for member in (second, third):
            conv_api.accept_contact(port, member.actor, owner.actor_hex)
        yield owner, second, third
    finally:
        teardown_seats(seats)


@pytest.fixture()
def room_seat_pair(nest_instance, room_app, request):
    """Two same-app seats, two accounts, one nest — ``(founder, member)`` —
    for a journey whose story has no witness: a community room founded by one
    and joined by the other. The member accepts the founder as a contact,
    exactly as :func:`room_seats`' members do."""
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        founder = launch_seat(nest_instance, app_path, room_app, request)
        seats.append(founder)
        member = launch_seat(nest_instance, app_path, room_app, request)
        seats.append(member)
        conv_api.accept_contact(nest_instance["port"], member.actor, founder.actor_hex)
        yield founder, member
    finally:
        teardown_seats(seats)


def bootstrap_room(owner: Seat, second: Seat, third: Seat) -> tuple[str, str]:
    """Setup (convention 8 carve-out (b)): a REAL bound governed room of
    [owner, second, third], through the real-wire commands — the ordinary
    1:1 → add-a-person fork (``direct-messages.md`` § Group-Forked Threads)
    with every peer's key package advertising the policy extension, so the
    room is born governed with its creator as owner. Returns the owner's
    thread id and the channel hex every seat agrees on."""
    owner.app.conversations.real_resolve_send_new(second.actor_hex, "hi second")
    one = wait_for(
        "the 1:1's own echo",
        lambda: next(
            (
                t
                for t in owner.app.conversations.list_threads()
                if t.rail == "FaunaMls" and "hi second" in (t.snippet or "")
            ),
            None,
        ),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    before_ids = {t.thread_id for t in owner.app.conversations.list_threads()}
    owner.app.conversations.real_add(one.thread_id, third.actor_hex, third.handle)
    fresh = [
        t
        for t in owner.app.conversations.list_threads()
        if t.thread_id not in before_ids and t.flavor == "MlsGroup"
    ]
    assert len(fresh) == 1, (
        f"adding a third person to the 1:1 should fork exactly one group; got {len(fresh)}"
        f"\n{seat_story(owner)}"
    )
    group_id = fresh[0].thread_id
    # The first send bootstraps the MLS group over both peers' key packages —
    # with the policy in the group context, since every package advertises it.
    owner.app.conversations.real_send(group_id, "hi room")
    bound = wait_for(
        "the group binding to a channel",
        lambda: thread_by_id(owner.app, group_id),
        lambda t: t is not None and bool(t.channel_id_hex),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(owner),
    )
    return group_id, bound.channel_id_hex


def found_community_room(founder: Seat, member: Seat, first_text: str) -> tuple[str, str, str]:
    """Found a COMMUNITY room from the app and seat ``member`` in it — every
    step a user gesture (convention 8): the composer's home-nest toggle, the
    first send (which founds it through the room ceremony), the invitation
    row atop the member's list, accept; then the founder's device keys the
    member in with no gesture of its own. Returns ``(room_id, founder's
    thread id, member's thread id)`` with both seats keyed and the founder's
    thread open.

    Asserts the founding's own invariants on the way — the class statement
    follows the toggle before the first message, the header states the
    class, an accepted invitation stops standing — so a journey built on it
    fails AT the step that broke, never later on a symptom."""
    conv = founder.app.conversations
    conv.navigate()
    founder.app.driver.click("new-conversation-button")
    conv.add_recipient(member.actor_hex)
    assert conv.prospective_room_class() == "end-to-end", (
        "with the home nest out, a Fauna chip makes an end-to-end room"
    )
    assert not conv.home_nest_included()
    conv.toggle_home_nest()
    assert conv.home_nest_included(), seat_element(founder, "recipient-picker-home-nest-toggle")
    assert conv.prospective_room_class() == "community", (
        "the class statement follows the choice, before the first message: "
        f"{seat_element(founder, 'recipient-picker-class')}"
    )
    founder.app.driver.type_text("dm-text-field", first_text)
    founder.app.driver.click("dm-send-button")

    def founded():
        for t in founder.app.conversations.list_threads():
            room = t.room or {}
            if t.rail == "FaunaMls" and room.get("class") == "Community" and t.channel_id_hex:
                return t
        return None

    room = wait_for(
        "the founder's community room binding to its room id",
        founded,
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(founder)}\n{seat_log(founder, 'found', 'room')}",
    )
    room_id = room.channel_id_hex
    assert not founder.app.has_error(), founder.app.error_text()
    conv.open_thread_by_id(room.thread_id)
    assert conv.room_class() == "community", (
        f"the header states the class: {seat_element(founder, 'thread-room-class')}"
    )

    wait_for(
        "the founder's invitation standing on the member's list",
        lambda: member.app.conversations.room_invitation_count(),
        lambda n: n >= 1,
        WELCOME_BUDGET_S,
        diagnose=lambda: f"{seat_story(member)}\n{seat_log(member, 'invitation', 'room')}",
    )
    member.app.conversations.accept_room_invitation(0)
    joined = wait_for(
        "the accepted room opening on the member's seat",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None,
        MEMBERSHIP_BUDGET_S,
        diagnose=lambda: seat_story(member),
    )
    assert not member.app.has_error(), (
        f"accepting must not refuse: {member.app.error_text()!r}\n{seat_story(member)}"
    )
    assert member.app.conversations.room_invitation_count() == 0, (
        "an accepted invitation stops standing"
    )

    wait_for(
        "the member's seat being keyed in by the founder's device",
        lambda: thread_by_channel(member.app, room_id),
        lambda t: t is not None and (t.room or {}).get("awaiting_key") is False,
        WELCOME_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(member)}\n{seat_story(founder)}\n"
            f"{seat_log(founder, 'key-in', 'key_in', 'tend', 'mint')}"
        ),
    )
    return room_id, room.thread_id, joined.thread_id


def index_of(thread, actor_hex: str) -> int:
    """The chip index of ``actor_hex`` in ``thread`` — the position every
    index-parallel room element (``thread-member-chip[i]``,
    ``room-admin-toggle[i]``, ``room-owner-transfer-button[i]``) shares."""
    ids = thread.participant_actor_ids
    assert actor_hex in ids, (
        f"{actor_hex[:12]}… must be a listed participant; participant_actor_ids={ids}"
    )
    return ids.index(actor_hex)


def thread_by_id(app, thread_id: str):
    """The thread with ``thread_id`` in ``app``'s current snapshot, or None.
    Every pick in these journeys is by identity — a seat accumulates threads
    across a run, and "the last group" is whichever sorts last."""
    for t in app.conversations.list_threads():
        if t.thread_id == thread_id:
            return t
    return None


def thread_by_channel(app, channel_hex: str):
    """The thread bound to ``channel_hex``, or None. The channel id is the
    one identity every member's snapshot agrees on — ``thread_id`` is
    per-seat, minted locally when the Welcome is applied."""
    for t in app.conversations.list_threads():
        if t.channel_id_hex == channel_hex:
            return t
    return None


def wait_for(what: str, read, predicate, budget_s: float, *, diagnose=None):
    """Deadline-poll ``read()`` until ``predicate(value)`` holds; return the
    value. On the budget elapsing, fail naming ``what`` and the last value
    seen (plus ``diagnose()`` when given) — never a bare timeout."""
    deadline = time.time() + budget_s
    last = None
    while time.time() < deadline:
        last = read()
        if predicate(last):
            return last
        time.sleep(POLL_S)
    extra = f"\n{diagnose()}" if diagnose else ""
    raise AssertionError(
        f"{what} did not happen within {budget_s:.0f}s; last seen: {last!r}{extra}"
    )


def wait_thread_by_channel(seat: Seat, channel_hex: str, *, what: str):
    """Wait for ``seat`` to carry a thread bound to ``channel_hex`` — the
    newcomer-side observable of a Welcome applied."""
    return wait_for(
        what,
        lambda: thread_by_channel(seat.app, channel_hex),
        lambda t: t is not None,
        WELCOME_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )


def wait_snippet(seat: Seat, channel_hex: str, needle: str, *, what: str):
    """Wait for the thread bound to ``channel_hex`` on ``seat`` to carry
    ``needle`` in its snippet — the peer-side observable of a message that
    crossed the channel and decrypted here."""
    return wait_for(
        what,
        lambda: thread_by_channel(seat.app, channel_hex),
        lambda t: t is not None and needle in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )


def wait_room_role(seat: Seat, channel_hex: str, *, what: str):
    """Wait for the room's projection to carry the viewer's role — the detail
    derives it from the engine's group context, which can lag the snapshot's
    own thread row by a render."""
    return wait_for(
        what,
        lambda: thread_by_channel(seat.app, channel_hex),
        lambda t: t is not None and (t.room or {}).get("my_role") is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )


def seat_log(seat: Seat, *needles: str) -> str:
    """The lines of ``seat``'s own app log that mention any of ``needles``
    (case-insensitive) — the half of a failed room gesture only the app saw:
    whether the editor's Save ran, what each policy commit answered. The log
    dies with the seat at teardown, so a failure message is the one place it
    can survive (convention 6)."""
    try:
        log = seat.app.driver.app_stderr_text() or ""
    except Exception as e:  # pragma: no cover — diagnosis only
        return f"<app log unavailable: {e}>"
    wanted = [n.lower() for n in needles]
    lines = [ln for ln in log.splitlines() if any(n in ln.lower() for n in wanted)]
    return "\n".join(lines[-40:]) or "(no matching lines in the app log)"


def seat_element(seat: Seat, element_id: str) -> str:
    """What ``seat``'s automation surface says about one element: the driver's
    one-line diagnosis — with the live ``frame`` (window-space "x,y,w,h"), the
    only thing that tells a **rendered but unseeable** element (count>=1,
    visible=False: zero-sized, or pushed outside the window by a layout that
    ran out of room) apart from one that was never painted — plus, where the
    app publishes a registry dump (the in-process agents' ``tree()``), that
    dump's lines for this id, which carry the per-slot state (on-screen
    geometry, the off-screen votes) that tells "still presented" apart from
    "presented once, left attached"."""
    lines = [seat.app.driver.diagnose(element_id, attrs=("frame",))]
    try:
        tree = seat.app.driver.tree() or ""
    except Exception:  # pragma: no cover — not every driver dumps a registry
        tree = ""
    rows = tree.splitlines()
    for i, row in enumerate(rows):
        if element_id in row:
            lines.extend(rows[i : i + 4])
    return "\n".join(lines)


def seat_story(seat: Seat) -> str:
    """One seat's diagnosis: the page error, every thread row's room facts, and
    the tail of this seat's own log — so a failure names what that side
    believed AND what its receive rail was doing.

    The log half was added 2026-09-12, when all four journeys' first web run
    failed identically on a peer seat's Welcome never applying and the
    diagnosis said only ``error='' `` with no thread rows: true, and useless.
    An empty roster is the *symptom* of every possible cause here — a rail that
    never ticked, a manager that never built, a refused fetch — and only the
    app's log tells them apart (convention 6: a failure must diagnose itself).
    On web that log is the browser console ring
    (``WebBridgeDriver.app_stderr_text``)."""
    app = seat.app
    try:
        err = app.error_text()
    except Exception as e:  # pragma: no cover — diagnosis only
        err = f"<error_text unavailable: {e}>"
    rows = []
    try:
        for t in app.conversations.list_threads():
            rows.append(
                f"  {t.thread_id} rail={t.rail} flavor={t.flavor} ch={t.channel_id_hex} "
                f"n={t.message_count} p={t.participant_count} snippet={t.snippet!r} "
                f"room={t.room!r} caps={t.capabilities!r}"
            )
    except Exception as e:  # pragma: no cover — diagnosis only
        rows.append(f"  <list_threads unavailable: {e}>")
    rail = seat_log(
        seat, "welcome", "conv", "poll", "manager", "mls", "roster", "error", "warn"
    )
    return (
        f"seat {seat.handle} ({seat.actor_hex[:12]}…): error={err!r}\n"
        + "\n".join(rows)
        + f"\n-- {seat.handle}'s own log (conv / welcome / poll / manager / mls) --\n"
        + rail
    )

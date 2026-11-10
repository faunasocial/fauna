"""tier_3: **one custodian serves at a time, and the door follows the signed-in
app** — the cli's conversation tools work only while a Fauna app of the user's
runs signed in on the device; when the serving app signs out another signed-in
Fauna app on the device takes over, and with none signed in the tool answers
that Fauna must be opened (``docs/goal/behavior/conversations-oracle.md``
§ The custodian and the door: the door's endpoint is taken under a
kernel-arbitrated exclusive lock per (OS user, account), first-come, released
when the app signs out, "so a second signed-in app takes it over on its next
tick").

The catalog page ``docs/features/connected-apps.md`` maps this journey as
outcome 48: "The LLM's conversation tools work only while a Fauna app of yours
runs signed in on the device: with none running the tool answers that Fauna
must be opened, and when the serving app signs out another signed-in Fauna app
on the device takes over".

One journey, on the consent nest, with tui — or the linux or macOS app — as both apps
(the custodian shells the door is built on, ``conversations-oracle.md``
§ Implementation status today; web and android are the per-outcome absence
ask, so the journey does not name them):

- alice's first seat — the custodian of ``test_cli_conversations.py``, whose
  fixture this module adopts — approves the cli's typed code on its Connected
  apps page, and the cli lists her rooms through it;
- a **second** seat signs in as alice on the same device: a separate install
  (its own store, its own MLS device — the role lock's keying, § The custodian
  and the door), whose launch shares the custodian's runtime dir, the place the
  account's door is resolved from (``oracle-<fingerprint>.sock`` beside the
  agent's socket). The cli is still served — by the first seat, which holds the
  door's lock;
- the custodian signs out through its own Account page; the cli's next
  ``conversations_list`` is served again, now by the second seat (polled,
  convention 14: the hand-over is the second keeper's next tick, and the
  second seat's account plane converging the grant the first seat recorded);
- the second seat signs out too, and the cli's next call answers
  ``ConnectError::NoCustodian``'s sentence verbatim — the grant still stands
  (before a grant the tool says "call connect" instead), so the sentence is the
  door's absence and nothing else. That last answer is also the proof the
  middle one came from the second seat: the first seat is still running, signed
  out, and serves nothing.

**The shared door per host.** linux: both launches name one private
``XDG_RUNTIME_DIR`` (``drivers/tui.py`` and ``drivers/linux.py``: a
caller-passed one owns it). The
second seat's direct-spawned sync agent then finds the first's
``sync-agent.sock.lock`` held and exits as a duplicate
(``fauna_ipc::unix_transport::InstanceLock``), so both apps talk to the first
launch's agent — the production shape, one agent per OS user. The door does
not ride the agent. windows: the door is the per-SID pipe, shared by
construction, so nothing is passed. macOS (the app and tui): the door and the
agent socket sit under the launch's ``HOME``, so the second seat's launch takes
the custodian's ``HOME`` as its device home (``config["device_home"]``,
``drivers/macos.py`` and ``drivers/tui.py``) while its install home,
``CFFIXED_USER_HOME``, stays its own — FaunaKit's Application Support follows
it, and in an e2e build so does shared Rust's state base
(``fauna_core::platform_ids::macos_install_home``), so the account store and
its locks stay the second seat's: a separate install, never two instances of
one (``apps/account-scoping.md`` § Concurrent instances). iOS has no door
(``conversations-oracle.md`` § The custodian and the door).
"""

import time

import pytest

from common.auth import UNBINDING_MAX_DEVICES, set_tier_caps
from drivers.cli_mcp import CliMcpSession
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    consent_bridge,
    consent_nest,
    consent_spa_url,
)
from helpers.budgets import ORCHESTRATION_STEP_S, RPC_ROUNDTRIP_S
from helpers.client_metadata_server import (  # noqa: F401 — pytest fixtures adopted by import
    cli_metadata,
    consent_nest_env,
)
from helpers.room_seats import Seat, launch_seat, seat_app_path, seat_story, teardown_seats
from helpers.waiting import await_account_runtime_assembled
from tests.test_cli_conversations import (  # noqa: F401 — `custodian` is a fixture adopted by import
    _door_location,
    _until_served,
    custodian,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

#: ``fauna_ipc::oracle::ConnectError::NoCustodian``'s sentence, which the cli
#: relays verbatim (``libs/fauna-cli/src/door.rs``); a public text, marked
#: ``cli-answer-strings-public`` at its definition (``oracle.rs``).
NO_CUSTODIAN = "open Fauna on this device to let this app reach your conversations"

#: The hand-over's ceiling, never paid on a green run (convention 14): the
#: second keeper's next tick (``serve::DOOR_TICK``) plus its account plane
#: converging the grant the first seat recorded at the approve.
_HANDOVER_S = ORCHESTRATION_STEP_S


def _shared_door_launch(seat: Seat) -> dict:
    """The launch-config overrides that put a second seat's door where
    ``seat``'s is — linux: the custodian launch's private ``XDG_RUNTIME_DIR``
    (``_door_location``'s ``runtime_dir``); macOS (the app and tui): the
    custodian launch's ``HOME`` as the second seat's device home, its install
    home (``CFFIXED_USER_HOME``, which its account store and locks follow —
    ``fauna_core::platform_ids::macos_install_home``) staying its own;
    windows: nothing, the per-SID pipe is one name per (OS user, account)
    already."""
    where = _door_location(seat)
    if "runtime_dir" in where:
        return {"environment": {"XDG_RUNTIME_DIR": where["runtime_dir"]}}
    if "user_home" in where:
        return {"device_home": where["user_home"]}
    return {}


#: A name only this module uses: the reach scan merges public names, and
#: `helpers/skew_client.py` calls a `second_seat()` parameter — a fixture of that
#: name lifting the tier cap made every skew journey read as admin-mutating
#: (helpers/kind_reach.py; test_nest_mode_axis's global-admin pins).
@pytest.fixture
def door_second_seat(request, consent_nest, custodian):
    """A second install of the app under test signed in as alice — another signed-in Fauna app on
    the same device, launched after the custodian holds the door. alice's device
    cap is lifted first so the second device is admitted (the
    ``test_folder_deposit_adoption.py`` precondition — admin setup, not a user
    gesture)."""
    set_tier_caps(
        consent_nest["port"],
        admin_signing_key=consent_nest["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES,
        base_url=consent_nest["url"],
    )
    app_name = request.node.callspec.params["app"]
    seat = launch_seat(
        consent_nest,
        seat_app_path(request, app_name),
        app_name,
        request,
        actor=custodian.actor,
        launch_config=_shared_door_launch(custodian),
    )
    # The door's custodian reads the account store's meta rows and the grant
    # log through the runtime slot: wait for the store to be assembled, not
    # for a delay.
    await_account_runtime_assembled(seat.app.driver)
    yield seat
    teardown_seats([seat])


def _poll_list(cli: CliMcpSession, done, budget_s: float) -> dict:
    """``conversations_list`` until ``done(result)`` or the ceiling (convention
    14: the state is asserted by the caller, the loop only paces it)."""
    deadline = time.monotonic() + budget_s
    while True:
        result = cli.call("conversations_list")
        if done(result) or time.monotonic() >= deadline:
            return result
        time.sleep(1.0)  # sleep-ok: paces a deadline poll on the door's own answer, never asserted on


def _served(result: dict) -> bool:
    return not result["isError"] and "rooms" in (result.get("structuredContent") or {})


@pytest.mark.feature("connected-apps")
@pytest.mark.host_platform("linux", "macos", "windows")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
def test_the_door_moves_to_the_next_signed_in_app_and_closes_with_the_last(
    custodian, door_second_seat, consent_nest, cli_binary, tmp_path
):
    first, second = custodian, door_second_seat
    assert first.actor["actor_id_hex"] == second.actor["actor_id_hex"], (
        "both seats are one account, two installs"
    )

    cli = CliMcpSession(cli_binary, consent_nest["url"], tmp_path, **_door_location(first))
    try:
        cli.initialize()

        # ── The first seat approves the cli; the cli lists through it. ──
        started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
        assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
        code = started["structuredContent"]["user_code"]
        ca = first.app.connected_apps
        ca.navigate()
        ca.connect_with_code(code)
        index = ca.wait_for_request_code(code)
        assert index is not None, (
            f"typing {code} must open its card. error: {ca.current_error_text()!r}"
        )
        ca.approve_request(index)
        assert ca.current_error_text() == "", ca.current_error_text()

        listed = _until_served(cli, "conversations_list")
        assert _served(listed), (
            f"with both seats signed in, the cli is served (by the first, which "
            f"took the door first): {listed!r}\n{cli.diagnostics()}\n{seat_story(first)}"
        )

        # ── The serving app signs out; the second signed-in app takes over. ──
        first.app.settings.sign_out()
        taken_over = _poll_list(cli, _served, _HANDOVER_S)
        assert _served(taken_over), (
            f"after the serving app signed out, the other signed-in app serves the "
            f"door: {taken_over!r}\n{cli.diagnostics()}\n{seat_story(second)}"
        )

        # ── The last signed-in app signs out; the tool says to open Fauna. ──
        second.app.settings.sign_out()
        closed = _poll_list(cli, lambda r: NO_CUSTODIAN in cli.text(r), _HANDOVER_S)
        assert closed["isError"] and NO_CUSTODIAN in cli.text(closed), (
            f"with no signed-in Fauna app on the device the tool answers that Fauna "
            f"must be opened — the grant still standing, so not 'call connect': "
            f"{closed!r}\n{cli.diagnostics()}"
        )
        assert "call connect" not in cli.text(closed), cli.text(closed)
    finally:
        cli.close()

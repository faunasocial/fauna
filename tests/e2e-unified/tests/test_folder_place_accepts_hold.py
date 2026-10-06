"""tier_3 — a device told not to take changes stops receiving them, and picks
up exactly where it stopped when it is let take them again
(``local-folder-sync`` outcome 15; ``file-sync.md`` § Technical Flow, the
``accepts`` gate: "hold, never advance").

**Why one user on two devices.** A member of someone else's set has no roster
row and positively accepts by design (``SeatRead::Absent``, ``file-sync.md``
§ 4. Orchestrator Forwarding) — there is no place whose ``accepts`` could be
turned off. The flag belongs to a seat on the owner's own roster, which is one
user's other device: alice's device A (``logged_in_app``) and device B
(``alice_second_device`` — same identity, distinct device id, empty state).

**The flow it asserts end to end.** ``folder-place-accepts`` is clicked off for
B's seat in the Folders place editor → ``fauna.folders.places.set`` writes the
flags on the nest's ``folder_members`` row → B's engine re-reads the roster on
its next rescan tick (``config::accepts_from_seat`` inside
``SyncEngine::refresh_sync_mode``) → ``pull_remote_changes`` returns before
fetching and leaves the anchor where it was → files A authors meanwhile reach
the nest but not B → the box is clicked back on → the next tick re-installs
``accepts`` and the very next pull starts from the held anchor, so every file
authored in the gap arrives. A pull that had advanced the anchor while holding
would lose the middle of the sequence for good, which is why the journey
authors a SEQUENCE and asserts every member of it.

**Latency-independence (convention 14).** "It stopped receiving them" is only
meaningful once a pull has provably run after the files were on the nest and
declined. B's engine says exactly that, in its own log, once per declined pass
(``fauna_sync_engine::place_accepts``, turned on for every e2e launch by
``conftest._apply_place_accepts_log_env``) — the machine's account of the hold,
the way ``await_agent_upload`` reads the engine's own upload line. The journey
counts that line: once to know the gate is installed at all, and again after
A's uploads are confirmed, so the absence is asserted at a state-defined
moment rather than after a sleep. The flip back rides the 30 s
``FAUNA_E2E_RESCAN_MS`` harness cadence; the budget is a ceiling.

The mutation is UI-driven (convention 8): the flag is clicked. B's seat itself
is fixture setup through the same ``fauna.folders.places.set`` door the place
editor's own test seeds with — a second device gains a roster row only when
something enrolls it, and enrolling is not the behaviour under test.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import PLACE_SYNC, place_flags_payload
from conftest import PLACE_ACCEPTS_HELD_LINE
from helpers.folder_content import (
    agent_diagnosis as _agent_diagnosis,
)
from helpers.folder_content import (
    atomic_write as _atomic_write,
)
from helpers.folder_content import (
    await_agent_upload as _await_agent_upload,
)
from helpers.folder_content import (
    bind_location_under_set as _bind_location_under_set,
)
from helpers.set_names import addressed
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # tui is the lead app. linux, windows and macOS ride the per-app
    # trickle-down (web, ios and android are the page's declared absences);
    # each arrives with its marker AND an `_SUPPORTED_APPS` entry.
    pytest.mark.tui,
]

#: Apps whose leg has landed. Grow this — and the markers above — together.
_SUPPORTED_APPS = ("tui",)

#: Device B's id — `alice_second_device`'s own constant.
_DEVICE_B = "fedcba9876543210" * 4

# ── Named budgets (convention 14: generous ceilings, deadline polls) ──
# One same-account hydration: two agents + a nest round trip on the 30 s cadence.
_HYDRATION_S = 360.0
# A place-flag edit reaches a running seat within one rescan tick (30 s in e2e).
_FLAG_INSTALL_S = 240.0


def _read_or_none(path):
    try:
        return path.read_text()
    except (FileNotFoundError, OSError):
        return None


def _held_passes(app) -> int:
    """How many pulls B's engine has declined so far, off its own log."""
    return sum(
        1 for line in app.driver.app_stderr_text().splitlines() if PLACE_ACCEPTS_HELD_LINE in line
    )


@pytest.mark.real_conversations
# Documented-long (point 9): two GUI apps + two agents + two flag flips.
@pytest.mark.timeout(1800)
@pytest.mark.feature("local-folder-sync")
def test_a_device_told_not_to_take_changes_resumes_exactly_where_it_stopped(
    request, logged_in_app, nest_instance, test_user, tmp_path
):
    """`local-folder-sync` outcome 15, on one user's two devices."""
    from helpers.app_surface import app_name, skip_unbuilt

    device_a = logged_in_app
    if app_name(device_a.driver) not in _SUPPORTED_APPS:
        skip_unbuilt(
            device_a.driver,
            surface="the place-accepts hold journey",
            detail="this app's leg of the journey has not landed",
            tracked="docs/features/local-folder-sync.md outcome 15 (per-app trickle-down)",
        )

    device_b, _driver_b = request.getfixturevalue("alice_second_device")
    request.addfinalizer(lambda: print(_agent_diagnosis(device_b, "device B")))
    request.addfinalizer(lambda: print(_agent_diagnosis(device_a, "device A")))

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )

    def roster(name: str) -> list[dict]:
        with client:
            return client.call("fauna.folders.members.list", addressed(name))["members"]

    def seat_b(name: str) -> dict | None:
        return next((m for m in roster(name) if m["device_id"] == _DEVICE_B), None)

    # Device B registers itself at login; its seat can be enrolled once it has.
    def _b_registered() -> bool:
        with client:
            devices = client.call("fauna.sync.devices.list", {}).get("devices", [])
        return any(d.get("device_id") == _DEVICE_B for d in devices)

    wait_until(
        _b_registered,
        120,
        interval=1.0,
        diagnose=lambda: "[device B] never registered its device id with the nest at login",
    )

    # ── the set, B's seat on it, and both devices bound ──
    set_name = f"hold-{secrets.token_hex(4)}"
    a = device_a.backups
    a.navigate_folders()
    a.create_folder_via_wizard(set_name)
    with client:
        client.call(
            "fauna.folders.places.set",
            addressed(set_name, device_id=_DEVICE_B, flags=place_flags_payload(PLACE_SYNC)),
        )
    assert (seat_b(set_name) or {}).get("flags", {}).get("accepts") is True, (
        f"[device B] its seat must start out accepting: {roster(set_name)!r}"
    )

    folder_a = tmp_path / "device-a"
    folder_a.mkdir()
    _bind_location_under_set(device_a, set_name, folder_a, seat="device A")

    b = device_b.backups
    b.navigate_folders()

    def _set_listed_on_b() -> bool:
        titles = [b.folder_title(i) for i in range(b.folder_count())]
        if any(set_name in t for t in titles):
            return True
        b.navigate_devices()
        b.navigate_folders()
        return False

    wait_until(
        _set_listed_on_b,
        90,
        interval=1.0,
        diagnose=lambda: f"[device B] {set_name!r} never listed; error={device_b.error_text()!r}",
    )
    folder_b = tmp_path / "device-b"
    folder_b.mkdir()
    _bind_location_under_set(device_b, set_name, folder_b, seat="device B")

    # ── the anchor: the rail carries a file A → B while B accepts ──
    first = f"before-{secrets.token_hex(3)}.txt"
    _atomic_write(folder_a / first, f"accepted — {secrets.token_hex(8)}\n")
    _await_agent_upload(device_a, first, seat="device A")
    wait_until(
        lambda: _read_or_none(folder_b / first) is not None,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[device B] {first} never arrived while B accepted — the ordinary "
            f"path is broken, so the hold below would prove nothing.\n"
            + _agent_diagnosis(device_b, "device B")
        ),
    )

    # ── tell B not to take changes (the UI gesture) ──
    driver_a = device_a.driver

    def _click_b_accepts(expect_on: bool) -> None:
        a.navigate_folders()
        # `expand_folder` toggles and the row may already be open from the bind
        # above, so expand until the place editor is actually on screen.
        a.find_and_expand_folder_until(set_name, "folder-place-row")
        j = next(i for i, m in enumerate(roster(set_name)) if m["device_id"] == _DEVICE_B)
        scope = f"folder-place-row[{j}]"
        driver_a.click("folder-place-accepts", scope=scope)
        want = "on" if expect_on else "off"
        wait_until(
            lambda: driver_a.get_attr("folder-place-accepts", "state", scope=scope) == want,
            15.0,
            diagnose=lambda: f"[device A] the box never read {want!r}; error={device_a.error_text()!r}",
        )
        assert (seat_b(set_name) or {}).get("flags", {}).get("accepts") is expect_on, (
            f"[nest] B's seat flags did not follow the click: {seat_b(set_name)!r}"
        )

    _click_b_accepts(expect_on=False)

    # B's engine has installed the hold once it declines a pass.
    wait_until(
        lambda: _held_passes(device_b) >= 1,
        _FLAG_INSTALL_S,
        interval=2.0,
        diagnose=lambda: (
            "[device B] its engine never declined a pull after its place stopped "
            "accepting — the flag never reached the running seat.\n"
            + _agent_diagnosis(device_b, "device B")
        ),
    )

    # ── A authors a sequence; every file reaches the nest ──
    tag = secrets.token_hex(3)
    held = {
        f"held-{n}-{tag}.txt": f"authored while B held — {n} — {secrets.token_hex(8)}\n"
        for n in (1, 2, 3)
    }
    for name, body in held.items():
        _atomic_write(folder_a / name, body)
    for name in held:
        _await_agent_upload(device_a, name, seat="device A")

    # A pass that ran after every held file was on the nest, and declined.
    after_upload = _held_passes(device_b)
    wait_until(
        lambda: _held_passes(device_b) > after_upload,
        _FLAG_INSTALL_S,
        interval=2.0,
        diagnose=lambda: (
            "[device B] no pull was declined after the held files reached the nest.\n"
            + _agent_diagnosis(device_b, "device B")
        ),
    )
    arrived = [n for n in held if _read_or_none(folder_b / n) is not None]
    assert not arrived, (
        f"[device B] {arrived} arrived while its place did not accept changes — "
        "the hold did not hold"
    )

    # ── let B take changes again: every held file arrives ──
    _click_b_accepts(expect_on=True)
    for name, body in held.items():
        wait_until(
            lambda name=name, body=body: _read_or_none(folder_b / name) == body,
            _HYDRATION_S,
            interval=1.0,
            diagnose=lambda name=name: (
                f"[device B] {name} never arrived after its place accepted again — "
                "a pull that advanced the anchor while holding skips it for good "
                f"(present: {sorted(p.name for p in folder_b.iterdir())}).\n"
                + _agent_diagnosis(device_b, "device B")
            ),
        )

    assert not device_b.has_error(), f"[device B] error: {device_b.error_text()!r}"

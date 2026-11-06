"""A discarded driver retires its machine: throw-away launches on a shared
account leave no live grant behind.

Owner: ``docs/goal/architecture/e2e-launch-isolation.md`` § The convention —
convention 10, *A discarded driver retires its machine* (ruled 2026-10-13); the
gate it protects is ``fleet-scope-reclamation.md`` § Fleet-scope reclamation,
clause (1) — a walk mark counts at the retention gate while its key holds a live
grant on the account.

**What went wrong, measured.** A test that builds its own driver, signs it into
a SHARED actor (``helpers/shared_identity.py`` — ``test_user`` or the session
nest's admin) and tears it down used to walk away signed in. Its grant stayed
live, so its last walk mark kept counting and pinned the gate's watermark for
the rest of the run: every retire above it was withheld. In a 2026-10-13
reproduction over the first 122 modules of ``--app linux``, nine launches of
the account-switcher and add-account tests pinned the admin's watermark at
seq 285 inside two minutes, and the admin's fleet scope grew to 1,034 live rows
from 90 writers by a quarter of the way through — each later admin test's
sign-in leaving its ~25 rows unretired. The 2026-10-12 whole-suite sweep's
session account met the nest's 4096-entry cap the same way.

**What is counted.** Rows of the account that still carry a principal — the
exact set whose walk marks the gate counts (``retirable_through_seq`` admits a
mark only while a ``sync_devices`` row holds its key). A count, never a
duration (convention 14): read once each teardown has returned, behind the
sign-out's own barrier (the app acknowledges its reset only after the
enrollment's retirement round trip).

⚠ **A DEDICATED actor registered as shared for the test's own duration** —
never ``test_user`` itself, whose roster is the whole session's. ``test_user``
is still requested: its fixture lifts the device cap on the tier every test
user shares, so each discarded machine can hold its own row.
"""

from __future__ import annotations

import pytest

from helpers import enrollment
from helpers.app_surface import app_name, require_surface
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

#: Discarded launches per run. One would pin the defect; three make "one live
#: grant per discarded machine" read as a rate rather than a coincidence.
DISCARDS = 3


def _device_id(n: int) -> str:
    """A distinct forced device id per launch, so each discarded machine is its
    own row — one row per machine, as `helpers/fleet.py::SiblingSeats` does it.
    On one shared id every launch would re-grant the SAME row and a leak could
    never count past one."""
    return "d15ca4d0" * 7 + f"d15ca4{n:02x}"


def _launch_signed_in(app, request, nest_instance, user, n: int):
    """A fresh driver of the app under test, signed into ``user`` with its
    enrollment latched on a row that holds a grant. Returns ``(driver, row)``;
    the caller tears the driver down."""
    from actions import ActionLayer
    from conftest import _build_app_config
    from drivers import create_driver

    name = app_name(app.driver)
    require_surface(app.driver, "discarded-machine-teardown")
    driver = create_driver(name)
    driver.launch(_build_app_config(name, nest_instance, request))
    assert driver._discards_machine_at_teardown(), (
        f"{name} lists `discarded-machine-teardown` but its launch kept no "
        "per-launch principal slot, so its teardown discards no machine"
    )
    seat = ActionLayer(driver)
    enrollment.sign_in(seat, request, nest_instance, user, device_id=_device_id(n))
    _writer, row, _roster = enrollment.await_enrollment(seat, nest_instance["url"], user)
    granted = enrollment.principals(nest_instance["url"], user)
    assert granted.get(row), (
        f"launch {n}: the enrollment latched on row {row} but the nest lists it with no "
        f"grant ({granted}) — the arrangement this test counts never happened"
    )
    return driver, row


def test_discarded_machines_leave_no_live_grant_on_a_shared_account(
    app, request, nest_instance, test_user
):
    """N throw-away launches signed into a shared account, each torn down: after
    every teardown no row of the account carries a principal — none of them is
    left counting at the fleet scope's retention gate."""
    from conftest import _make_user
    from helpers import shared_identity

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    shared_identity.remember_shared_actor(
        user["actor_id_hex"], nest_url=nest_url, signing_key=user["signing_key"]
    )
    try:
        for n in range(1, DISCARDS + 1):
            driver, row = _launch_signed_in(app, request, nest_instance, user, n)
            driver.teardown()
            seen: dict = {}

            def no_live_grant():
                listed = enrollment.principals(nest_url, user)
                seen.clear()
                seen.update(listed)
                return not any(listed.values())

            wait_until(
                no_live_grant,
                enrollment.RETIREMENT_VISIBLE_S,
                interval=1.0,
                diagnose=lambda: (
                    f"discard {n} of {DISCARDS}: after the teardown the account still lists "
                    f"rows holding a grant: { {d: p for d, p in seen.items() if p} } (this "
                    f"launch's row was {row}). Each one is a walk mark the fleet scope's "
                    "retention gate keeps counting, so every later retire above it is "
                    "withheld. The driver's teardown did not retire the machine — "
                    "drivers/http_bridge.py::_retire_discarded_machine; grep the run's "
                    "output for \"[e2e] discarded machine\"."
                ),
            )
    finally:
        shared_identity.forget_shared_actor(user["actor_id_hex"])


#: For a live machine's own pump to retire what the discarded ones left on the
#: fleet scope: its assembly, its first full pass (publish → reconcile → the
#: reclamation pass behind its own published copies), and a poked pass if the
#: first ran before the last discard's rows were listed. A green run pays only
#: what one pass costs.
DEPARTED_ROWS_BUDGET_S = 120.0

#: A full pump pass is expensive: the pokes are spaced, the reads are not.
POKE_SPACING_S = 10.0


def test_discarded_machines_leave_no_fleet_entry_behind(app, request, nest_instance, test_user):
    """N throw-away launches on a shared account, then one that stays: once the
    live machine's pump has run, no writer without a live grant holds a live
    entry in the account's fleet scope — the count stays flat however many
    machines came and went (`fleet-scope-reclamation.md` § *Fleet-scope
    reclamation*; measured 2026-10-30: each departed machine kept its copy of
    the escrow target until clause (3)(k), and a whole-suite sweep's session
    account met the 4096-entry cap that way). Read at rest from the nest's
    `nest.db`: the entries are sealed, so what is counted is (writer, grant),
    never a kind. A count, never a duration (convention 14)."""
    import time

    from conftest import _make_user
    from helpers import fleet_scope_census, shared_identity
    from helpers.waiting import account_pump_role, poke_account_pump

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    actor = bytes.fromhex(user["actor_id_hex"])
    shared_identity.remember_shared_actor(
        user["actor_id_hex"], nest_url=nest_url, signing_key=user["signing_key"]
    )
    live = None
    try:
        for n in range(1, DISCARDS + 1):
            driver, _row = _launch_signed_in(app, request, nest_instance, user, n)
            driver.teardown()
            wait_until(
                lambda: not any(enrollment.principals(nest_url, user).values()),
                enrollment.RETIREMENT_VISIBLE_S,
                interval=1.0,
                diagnose=lambda n=n: f"discard {n}: a grant outlived the teardown",
            )
        live, _row = _launch_signed_in(app, request, nest_instance, user, DISCARDS + 1)
        seen: dict = {}
        last_poke = [0.0]

        def departed_rows() -> list:
            conn = fleet_scope_census._open(nest_instance["db_path"])
            try:
                census = fleet_scope_census._read(conn, actor)
            finally:
                conn.close()
            seen["census"] = census
            return [w for w in (census or {}).get("writers", []) if not w[2]]

        def flat() -> bool:
            if not departed_rows():
                return True
            if account_pump_role(live) == (True, True) and (
                time.monotonic() - last_poke[0] > POKE_SPACING_S
            ):
                poke_account_pump(live)
                last_poke[0] = time.monotonic()
            return False

        wait_until(
            flat,
            DEPARTED_ROWS_BUDGET_S,
            interval=1.0,
            diagnose=lambda: (
                f"after {DISCARDS} discarded machines and one live one, writers holding no "
                f"grant still hold live fleet entries: "
                f"{[(w[:16], n) for w, n, _g in departed_rows()]} (census {seen.get('census')}). "
                "Each is an entry no pass retires, so the scope grows by one per machine "
                "until its 4096-entry cap — the live machine's reclamation pass should "
                "have retired them behind its own published copies "
                "(libs/fauna-account-plane/src/generation_reclaim.rs, step 4b)."
            ),
        )
    finally:
        if live is not None:
            live.teardown()
        shared_identity.forget_shared_actor(user["actor_id_hex"])


def test_a_wedge_recovery_leaves_no_live_grant_on_a_shared_account(
    app, request, nest_instance, test_user
):
    """The app stops answering, so the `app` fixture relaunches it in place of the
    reset that would have signed it out. Measured 2026-10-31: the relaunch signs
    nothing out, the fresh launch's reset has nothing to sign out, and the row kept
    its grant until the driver's teardown, its mark idle at the retention gate. The
    recovery finishes that sign-out through the app: the row stays, its grant
    cleared, which only the app's own sign-out does — a row deleted nest-side would
    leave a fleet member every healer keeps writing wrap cells for. And the machine
    keeps no slot to restore for it. A count, never a duration (convention 14): the
    sign-out has returned when the recovery does."""
    from conftest import _make_user
    from helpers import shared_identity

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    shared_identity.remember_shared_actor(
        user["actor_id_hex"], nest_url=nest_url, signing_key=user["signing_key"]
    )
    driver = None
    try:
        driver, row = _launch_signed_in(app, request, nest_instance, user, 1)
        assert driver.recover_from_wedged_reset(), "the relaunch did not come back"
        listed = enrollment.principals(nest_url, user)
        assert row in listed and listed[row] is None, (
            f"after a wedge recovery the roster reads {listed}; this launch's row {row} "
            "should be there with its grant cleared, the mark of the app's own sign-out. "
            "A live grant is a walk mark the retention gate keeps counting, and a missing "
            "row means the sign-out failed and only the nest half ran — "
            "drivers/http_bridge.py::recover_from_wedged_reset; grep the run's output for "
            '"[e2e] wedged reset".'
        )
        assert user["actor_id_hex"] not in getattr(driver, "_principal_slot_vault", {}), (
            "the recovery removed the row but the vault still holds its slot, so the next "
            "sign-in would restore a credential the nest has forgotten"
        )
    finally:
        if driver is not None:
            driver.teardown()
        shared_identity.forget_shared_actor(user["actor_id_hex"])


def test_a_relaunch_resumes_its_writer_for_a_session_naming_no_actor(
    app, request, nest_instance, test_user
):
    """A relaunch is the same machine (convention 10), including for a session
    patch that carries only the secret, as the admin logins do. Measured
    2026-10-31: the slot restore keyed on `actor_id` alone, so every such relaunch
    stranded a machine as a live fleet member and the next sign-in minted another.
    The same writer before and after is the count: one machine, not two."""
    from conftest import _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    driver, row = _launch_signed_in(app, request, nest_instance, user, 1)
    try:
        from actions import ActionLayer

        seat = ActionLayer(driver)
        writer, _row, _roster = enrollment.await_enrollment(seat, nest_url, user)
        assert driver.recover(), "the relaunch did not come back"
        driver.set_state(
            {
                "session": {
                    "authenticated": True,
                    "node_url": nest_url,
                    "secret_hex": user["signing_key"].encode().hex(),
                    "device_id": row,
                }
            }
        )
        after, _row, _roster = enrollment.await_enrollment(seat, nest_url, user)
        assert after == writer, (
            f"the relaunch minted writer {after} over the machine's {writer}: the slot "
            "was not restored for a session that names no actor_id, so the old machine "
            "stays a fleet member — drivers/http_bridge.py::_restore_principal_slot"
        )
    finally:
        driver.teardown()


def test_a_discarded_machine_on_a_dedicated_account_keeps_its_grant(
    app, request, nest_instance, test_user
):
    """The scope's other edge: a dedicated actor's machine is left exactly as its
    test stopped it. Journeys that stop a device to model a LOST one, and then
    remove it through the UI (`test_device_member_removal.py`), rest on this."""
    from conftest import _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    driver, row = _launch_signed_in(app, request, nest_instance, user, 1)
    driver.teardown()
    listed = enrollment.principals(nest_url, user)
    assert listed.get(row), (
        f"a dedicated account's discarded machine lost its grant (row {row}: {listed}). "
        "Retirement is for SHARED actors only — a dedicated actor's test decides what "
        "its stopped device means, and a lost-device journey removes it itself."
    )


def test_a_harness_seat_leaves_no_walker_at_the_gate(nest_instance):
    """N app-less writes through the harness's account seat leave no machine on
    the account: a seat lives for one call, so it is a discarded machine, and its
    grant and walk mark must go with it (`e2e-launch-isolation.md` convention 10;
    `libs/fauna-ffi/src/cabi/harness.rs`, `Seat::stop`). Measured 2026-10-30: each
    `user_create_folder`, mail enable or topic write on the session account left a
    live grant with a mark that never moved again, every later retire was withheld
    behind it, and the account met the 4096-entry cap at about half a sweep — the
    `scope_full` a whole-suite run then logged 203 times. Read at rest from the
    nest's `nest.db`. A count, never a duration (convention 14)."""
    import secrets

    from common.auth import user_create_folder
    from conftest import _make_user
    from helpers import fleet_scope_census

    user = _make_user(nest_instance)
    secret_hex = user["signing_key"].encode().hex()
    for _ in range(DISCARDS):
        user_create_folder(
            nest_instance["port"],
            f"seat-{secrets.token_hex(4)}",
            secret_key=secret_hex,
            base_url=nest_instance["url"],
        )

    conn = fleet_scope_census._open(nest_instance["db_path"])
    try:
        census = fleet_scope_census._read(conn, bytes.fromhex(user["actor_id_hex"]))
    finally:
        conn.close()
    counted = [(w[:16], mark) for w, mark, granted in census["walkers"] if granted]
    assert not counted, (
        f"{DISCARDS} harness seats left counted walkers on the account's fleet scope: "
        f"{counted} (census {census}). A walker holding a live grant keeps the retention "
        "gate's watermark at its last mark, so every retire above it is withheld."
    )

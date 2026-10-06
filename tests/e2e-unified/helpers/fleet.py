"""Fleet-plane helpers for journeys that need a SECOND enrolled device of one
account and an observation path onto the account-data plane's device set.

Lifted out of ``test_crash_recovery_journeys.py`` (the kill-between-the-legs
journey, 2026-09-20) when ``test_device_member_removal.py`` needed the same
sibling seat, the same ``device_set_state`` reader and the same merge barrier
— one home, never two copies (priority #2). Owner of the plane these observe:
``docs/goal/architecture/account-data-taxonomy.md`` § The generation machinery
→ *Fleet-scope reclamation*, clause (4).

Every wait here is a deadline poll on plane STATE (convention 14): a poked pump
pass where this process holds the pump, and a read of the effect either way.
"""
from __future__ import annotations

import json

from helpers.waiting import wait_until

#: The second seat's enrolment becoming a VERIFIED fleet member at seat one —
#: a real plane merge of its `device-set` + `device-endpoints` rows, which is
#: what makes a removal resolve to a target at all.
FLEET_MEMBER_VISIBLE_S = 240.0


def fleet_id_hex(writer_seed_hex: str) -> str:
    """The fleet id a store writer key answers to: `hex32(writer pubkey)`.

    The credential slot holds the writer key as a 32-byte ed25519 SEED in hex
    (`principal_bundle::load_writer_key`), while every plane row, every
    `RemovalFacts` member and `device_set_state`'s own key are the PUBLIC half
    (`account_runtime.rs`'s `writer_key.verifying_key().to_bytes()`). Derived
    here rather than read back through a new app seam: the derivation IS the
    definition, and a seam would be one more automation surface to gate for a
    value the harness can already compute.
    """
    from nacl.signing import SigningKey

    return bytes(SigningKey(bytes.fromhex(writer_seed_hex)).verify_key).hex()


def device_set_state(driver, device_id_hex: str) -> dict:
    """This app's own account-runtime read of `device_id_hex`'s
    `fauna.state.device-set` plane row — ``{"found": …, "state": …}``.

    The on-demand reader `fauna_client_account_runtime::device_set_state_json`,
    published through each seat's command dispatcher (convention 15: compiled
    out of release artifacts, never a runtime security boundary). An app that
    does not answer it returns ``None``, which :func:`require_device_set_reader`
    turns into that app's declared skip.
    """
    raw = driver.call_command("device_set_state", {"device_id_hex": device_id_hex})
    # The app stashes the reader's value as a JSON *string* (tui's
    # `CommandResult::machine_result`), but a bridge whose state blob types the
    # slot as a JSON value hands it back already decoded — tui does. Accept
    # both rather than pinning one bridge's serialization here.
    if isinstance(raw, str):
        return json.loads(raw)
    return raw if isinstance(raw, dict) else {}


def require_device_set_reader(driver) -> None:
    """Skip an app that hosts an account runtime but publishes no device-set
    reader — convention 7 mode 5, never a bare skip.

    tui wired it 2026-09-16; linux, android, macOS, iOS and windows followed
    2026-09-21 (macOS and iOS share `FaunaKit`'s `DeviceSetStateTestCommand`),
    and web 2026-09-30 (the same reader, lifted to the wasm-capable
    `fauna_account_plane::account_driver::e2e_readers`), so every app that hosts an account runtime now answers it and a journey
    turns on for each app the moment its arm lands rather than needing a second
    copy per app (priority #1). A built reader always answers a JSON object —
    at minimum ``{"found": false}`` — so ``None`` is unambiguous.
    """
    from helpers.app_surface import skip_unbuilt

    if driver.call_command("device_set_state", {"device_id_hex": "00" * 32}) is None:
        skip_unbuilt(
            driver,
            surface="the `device_set_state` account-data-plane reader",
            detail=(
                "this app hosts an account-store runtime but publishes no "
                "`device_set_state` command, so a fleet-scope `Removed` row "
                "cannot be observed here. Every app that hosts one answers it "
                "over the shared `fauna_client_account_runtime::device_set_state_json`, "
                "so a dispatcher that no longer does is a regression, which "
                "`test_account_runtime_pump.py`'s reader test fails on rather "
                "than skips"
            ),
            tracked="docs/goal/architecture/e2e-automation-surface-gating.md",
        )


def poked_pass(driver, *, what: str, budget_s: float = 90.0) -> None:
    """One full account-pump pass, begun after this call and observed finished —
    when this process is the pump HOLDER; otherwise a no-op.

    The pump's reconcile runs once per FULL pass, so every assertion about what
    it did wants a pass barrier rather than a settle-sleep (convention 14). The
    poke only spends the production cadence — it is not a user gesture, and
    nothing here drives a removal a second time.

    ⚠ The holder is often the co-located sync agent rather than the app
    (`helpers/waiting.py`'s role table), and a runtime that lost the role runs
    no pass at all — so poking it and waiting would be waiting on something
    nothing guarantees. Every caller wraps this in a poll on the EFFECT itself,
    which is the barrier that holds either way (convergence reaches a
    non-holder through the shared store), so the non-holder branch simply
    returns and lets that poll do the work.
    """
    from helpers.waiting import (
        account_pump_cycles,
        account_pump_role,
        await_pump_cycle_after,
        poke_account_pump,
    )

    if account_pump_role(driver) != (True, True):
        return
    cycles = account_pump_cycles(driver)
    if cycles is None:
        return
    poke_account_pump(driver)
    await_pump_cycle_after(driver, cycles[0], budget_s=budget_s, what=what)


def await_fleet_member(remover, sibling, fleet_id_hex: str, *, budget_s: float) -> None:
    """Barrier: `fleet_id_hex` is a VERIFIED member of the remover's own merged
    fleet view.

    A removal resolves its targets from client-held plane state alone
    (`fleet_removal::removal_facts` — deliberately never the nest's roster), so
    a click before the sibling's `device-set`/`device-endpoints` rows have
    merged resolves to no target, stages nothing, and would leave a journey
    asserting a reconcile that had no intent to finish. Both seats are poked:
    the sibling has to PUBLISH its rows and the remover has to MERGE them, and
    neither half is the other's.
    """
    last: dict = {}

    def merged():
        poked_pass(sibling.driver, what="the sibling's plane publish")
        poked_pass(remover.driver, what="the remover's fleet-view merge")
        state = device_set_state(remover.driver, fleet_id_hex)
        last.clear()
        last.update(state)
        return state.get("found") and state.get("state") == "enrolled"

    wait_until(
        merged, budget_s, interval=1.0,
        diagnose=lambda: (
            f"the second seat never became a verified fleet member at seat one "
            f"(last device-set read: {last}). Its nest enrolment had already "
            f"latched, so the gap is the PLANE leg: seat two published no "
            f"`device-set`/`device-endpoints` rows, or seat one's pump never "
            f"merged them. Without it a removal resolves to no target and "
            f"stages no intent, and the journey would assert a reconcile that "
            f"had nothing to finish."
        ),
    )


class SiblingSeats:
    """Factory for SECOND enrolled devices on ``nest``, for the account a test
    signs seat one in as: ``seat = seats.launch(user=user)``; ``seats.teardown()``
    in the fixture's ``finally``. Pass ``user`` by keyword: a lone positional
    ``.launch(x)`` is the shape `test_r14_trust_seed_self_launch.py` reads as a
    raw driver launch and demands a seed for, while this wrapper already seeds
    through `_build_app_config`.

    A real second seat, not a seeded roster row. `enrollment.register_device`
    fills a roster slot the way a daemon does, but the row it leaves is not a
    fleet MEMBER — it publishes no `device-set` enrolment and no
    `device-endpoints` row statement, so `resolve_removal_targets` resolves it
    to nothing and the fleet leg never arms. Only an app with its own account
    runtime produces those rows.

    Always **tui**, whatever app seat one is: the sibling is fixture
    arrangement (the device being removed — it is stopped before the gesture),
    tui is the lightest real seat on every machine the suite runs on, and a
    per-app sibling would need a second simulator on ios and a second WinUI
    launch on windows for no assertion of its own. Isolation is the driver's
    (convention 10): the launch gets its own HOME/keyring/credential dir, hence
    its own writer key, its own fleet id and its own nest row.

    ``environment`` rides into the sibling's process env — the one legitimate
    use is a compile-gated `FAUNA_E2E_*` knob such as the stated-row override
    the member-removal journey plays the stolen device with (convention 15).
    """

    def __init__(self, request, nest: dict):
        from conftest import get_available_apps

        if "tui" not in get_available_apps():
            import pytest

            pytest.skip(
                "the fleet sibling seat is always a fauna-tui seat, and tui is not in "
                "this run's app set — pass `--app sweep` (or add `tui` to `--app`); "
                "`get_available_apps()` is the run's selection, not just what this "
                "machine can build"
            )
        self._request = request
        self._nest = nest
        self._drivers: list = []

    def launch(self, user: dict, *, environment: dict | None = None):
        from actions import ActionLayer
        from conftest import _build_app_config
        from drivers import create_driver
        from helpers import enrollment

        driver = create_driver("tui")
        self._drivers.append(driver)
        config = _build_app_config("tui", self._nest, self._request)
        if environment:
            config = dict(config)
            config["environment"] = {**config.get("environment", {}), **environment}
        driver.launch(config)
        seat = ActionLayer(driver)
        # A DISTINCT forced device id per sibling: the e2e session patch forces
        # every seat onto one login id (the one-row-per-machine shape), and a
        # sibling on the primary seat's id would enrol on the SAME row — the
        # "second device" this factory exists to make would be none. Same
        # identity, different device, as `alice_second_device` does it.
        sibling_device_id = "fedcba9876543210" * 3 + f"fedcba98765432{len(self._drivers):02x}"
        enrollment.sign_in(seat, self._request, self._nest, user, device_id=sibling_device_id)
        return seat

    def launched_drivers(self) -> list:
        """Every sibling driver launched so far — the on-failure log hook's view
        of seats no fixture value names (`helpers/app_log_section.py::launched_drivers`)."""
        return list(self._drivers)

    def teardown(self) -> None:
        for driver in self._drivers:
            try:
                driver.teardown()
            except Exception:
                pass
        self._drivers.clear()

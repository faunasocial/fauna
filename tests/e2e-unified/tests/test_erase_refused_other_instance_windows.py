"""tier_3 e2e: windows' unreadable-index start-over refuses while a SIBLING
instance serves the account, and paints the refusal as ``error-message``.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*An erase refuses while a sibling serves the account*: the floor's refusal
paints "on its launch page's ``error-message`` (convention 2)", with the
gesture's own control still showing. Windows' leg of the start-over journey
``test_erase_refused_other_instance.py`` witnesses for tui and linux.

**What this pins that the gate's own tests cannot.** ``StartOverBlocked`` is
pinned at its shared-Rust site. What only a real pair of windows seats
witnesses is the page's PAINT: that the refusal lands on ``error-message``
and not over the verdict text (``account-index-refusal-warning``), which
windows once overwrote, and that the confirm stays offered. Remove the paint
and ``error-message`` never reads the refusal.

**The world** is ``test_account_instance_lock_windows.py``'s shared install
(one credential store, one ``%LOCALAPPDATA%``, one data dir). The first seat
serves the account; the index is then made malformed in the shared store, so a
second plain launch lands on the unreadable-index surface.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); windows driver only.
"""
from __future__ import annotations

import json
import os
import shutil
import tempfile

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.registry_store import credential_store_path, read_store_map, read_store_slot
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

ERROR_MESSAGE = "error-message"
CREATE_IDENTITY = "create-identity-button"
INDEX_REFUSAL = "account-index-refusal-warning"
INDEX_RESET = "account-index-reset-button"
INDEX_RESET_CONFIRM = "account-index-reset-confirm-button"

#: Not JSON at all — the malformed verdict, whose floor is the start-over
#: (``test_account_index_unreadable_launch.MALFORMED_INDEX``).
MALFORMED_INDEX = "this is not an account index {{{"

#: Convention 14: generous ceilings on latency-independent state.
LAUNCH_BUDGET_S = 60.0
LOCAL_BUDGET_S = 30.0


def _shared_instance_world():
    """One throwaway install world for two seats to share — the shape
    ``test_account_instance_lock_windows._shared_instance_world`` documents
    (credential store, ``%LOCALAPPDATA%`` and data dir all shared)."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-win-erase-world-")
    local_appdata = os.path.join(base, "localappdata")
    return base, {
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-erase-world-{os.path.basename(base)}",
        "local_appdata": local_appdata,
        "data_dir": os.path.join(local_appdata, "Fauna"),
    }


def _corrupt_index(driver):
    """Replace the shared store's ``fauna/index`` with a malformed blob,
    atomically, keeping every other slot (the serving seat's secret included)."""
    path = credential_store_path(driver)
    assert path is not None, "the shared world must be file-backed"
    stored = read_store_map(driver)
    assert stored, f"the shared store at {path} is empty — nothing to corrupt"
    stored["fauna/index"] = MALFORMED_INDEX
    tmp = f"{path}.e2e-tmp"
    with open(tmp, "w") as f:
        json.dump(stored, f)
    os.replace(tmp, path)


def _errors(driver) -> list[str]:
    try:
        return [t for t in driver.get_texts(ERROR_MESSAGE) if t]
    except Exception:  # noqa: BLE001 — a surface with no error line reads as none
        return []


@pytest.mark.feature("upgrades-never-lose-data")
def test_windows_start_over_refuses_on_error_message_while_another_instance_serves(
    nest_instance, windows_app_path, request
):
    """The first seat serves U; the index is then made malformed, so a second
    plain launch lands on the unreadable-index surface. Its confirm is REFUSED:
    the shared ``index_malformed_reset_blocked_other_window`` line on
    ``error-message``, the verdict text untouched, the confirm still offered, U's
    secret on disk. With the first seat gone, the same confirm resets to a fresh
    install."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    u_actor = user["actor_id_hex"]
    u_secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": u_actor, "secret_hex": u_secret, "nest_url": nest_instance["url"],
          "device_id": "erase-u", "handle": "u"}],
        active=u_actor,
    )
    world_base, world = _shared_instance_world()

    first = create_driver("windows")
    first.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
    })
    second = None
    try:
        first.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == u_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=LAUNCH_BUDGET_S,
        )
        _corrupt_index(first)

        second = create_driver("windows")
        second.launch({
            "app_path": windows_app_path,
            "url": nest_instance["url"],
            "environment": _seeded_environment(request, nest_instance),
            **world,
        })
        second.wait_for(INDEX_REFUSAL, timeout=LAUNCH_BUDGET_S)
        assert second.get_text(INDEX_REFUSAL) == S.onboarding.launch.index_malformed, (
            f"the second launch must read the malformed verdict; "
            f"got {second.get_text(INDEX_REFUSAL)!r}"
        )
        second.click(INDEX_RESET)
        second.wait_for(INDEX_RESET_CONFIRM, timeout=LOCAL_BUDGET_S)

        # ── refused while the first seat serves U ────────────────────────────
        assert first.app_running(), (
            "the sibling must still be live, or the refusal below has no premise"
        )
        second.click(INDEX_RESET_CONFIRM)
        refusal = S.onboarding.launch.index_malformed_reset_blocked_other_window
        wait_until(
            lambda: refusal in _errors(second),
            LOCAL_BUDGET_S,
            diagnose=lambda: (
                f"error-message never read the shared refusal {refusal!r}; it reads "
                f"{_errors(second)!r}, the verdict text reads "
                f"{second.get_text(INDEX_REFUSAL)!r}"
            ),
        )
        assert second.get_text(INDEX_REFUSAL) == (
            S.onboarding.launch.index_malformed_reset_residual
        ), "the refusal must not overwrite the residual warning the confirm states"
        assert second.is_visible(INDEX_RESET_CONFIRM), (
            "a refused start-over keeps its confirm — closing the other window and "
            "confirming again is the whole remedy"
        )
        assert second.is_absent(CREATE_IDENTITY), (
            "a refused start-over must not land on a fresh install"
        )
        assert read_store_slot(second, f"fauna/{u_actor}/secret") == u_secret, (
            "a refused start-over must leave the served account's secret in place"
        )

        # ── goes through once the sibling is gone ────────────────────────────
        # Stop only the first: a default teardown sweeps every instance owning
        # the shared data dir, the second included.
        first.teardown(sweep_data_dir_peers=False)
        first = None

        def _through():
            if second.is_visible(CREATE_IDENTITY):
                return True
            # A press in the window where the dying sibling still holds its lock
            # is refused, correctly; the user's remedy is to confirm again.
            if refusal in _errors(second):
                second.click(INDEX_RESET_CONFIRM)
            return False

        wait_until(
            _through,
            LOCAL_BUDGET_S,
            diagnose=lambda: (
                "with no sibling serving, the start-over must reset to a fresh "
                f"install; error-message reads {_errors(second)!r}"
            ),
        )
    finally:
        if second is not None:
            second.teardown()
        if first is not None:
            first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)

"""tier_1: the discarded-machine retirement's web and android seams, with no
browser and no device.

Owner: ``docs/goal/architecture/e2e-launch-isolation.md`` § The convention —
convention 10, *A discarded driver retires its machine*. The journey pin is
``test_discarded_machine_retirement.py``, which runs the whole retirement
against a live nest. This file pins what that journey rests on and what a
machine with no android venue can still check: the two seams
``drivers/http_bridge.py::_retire_discarded_machine`` reads —
``_discards_machine_at_teardown`` (does this driver own its machine?) and
``_current_slots`` (which actors does the machine hold?) — as web and android
answer them. Neither app keeps a host-side principal file, so each reads its
store through the running app (``_attached_slots``).

  * web's machine is the BrowserContext: a primary driver or a twin page owns
    one and discards it at teardown; a same-context tab shares its creator's
    store and must never sign the creator's machine out.
  * android's store is the device's filesDir, which outlives the driver. A
    seeded launch is a store the CALLER wrote (``AndroidLaunchHarness``), whose
    relaunch is ``teardown()`` + an unseeded ``launch()`` resuming it — so once
    any launch was seeded, or a ``preserve_state_across_relaunch()`` pin is
    set, the teardown discards nothing.
  * an unpinned android ``recover()`` erases the store with an empty seed
    while every enrollment in it is still live nest-side, so it vaults the
    slots first, and the teardown's nest half still reaches them.

Pure state, no timing (convention 14).
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))

from common.cred_store import ACCOUNT_STORE_FOREIGN_PREFIX  # noqa: E402
from drivers.android import AndroidBridgeDriver  # noqa: E402
from drivers.http_bridge import HttpBridgeDriver  # noqa: E402
from drivers.web import WebBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

SHARED = "5a" * 32
OTHER = "0b" * 32
ROW = "d15ca4d0" * 8


def _slot_map(actor: str, row: str) -> dict[str, str]:
    """An account store holding one whole slot for ``actor`` latched on
    ``row``, plus registry residue that is no machine's principal."""
    return {
        actor: "writer-key",
        f"{actor}/principal_bundle": "bundle",
        f"{actor}/grant-registered": f"{row}:grant",
        "fauna/index": "{}",
    }


def _android(stored: dict[str, str] | None = None):
    driver = AndroidBridgeDriver()
    driver.credential_map = lambda: {
        f"{ACCOUNT_STORE_FOREIGN_PREFIX}{k}": v for k, v in (stored or {}).items()
    }
    return driver


def _android_launched(config: dict, stored=None):
    """A driver whose `launch()` recorded `config` the way the real one does
    — the ownership decision is the only part of launch under test."""
    driver = _android(stored)
    driver._launch_config = config
    driver._note_store_owner(config)
    return driver


# ── android: does the driver own its machine? ───────────────────────────────


def test_an_unseeded_android_launch_is_a_machine_the_driver_discards():
    assert _android_launched({})._discards_machine_at_teardown()


def test_a_seeded_android_launch_is_the_callers_store():
    # The launch harness's seed — even an empty one — is a store a later
    # unseeded launch of the same driver resumes.
    assert not _android_launched({"seed_credentials": {}})._discards_machine_at_teardown()


def test_a_seeded_store_stays_the_callers_across_an_unseeded_relaunch():
    driver = _android_launched({"seed_credentials": {"k": "v"}})
    # `AndroidLaunchHarness.relaunch` pops the seed and launches again.
    driver._note_store_owner({})
    assert not driver._discards_machine_at_teardown()


def test_a_pinned_android_store_is_kept():
    driver = _android_launched({})
    driver._record_relaunch_pin(driver._launch_config, ("preserve_store",))
    assert not driver._discards_machine_at_teardown()


def test_the_real_launch_records_the_seed_decision():
    # Source pin on the real `launch()` (which needs a device to run): it hands
    # every config it launches with to the ownership decision.
    import inspect

    assert "self._note_store_owner(config)" in inspect.getsource(AndroidBridgeDriver.launch)


# ── android: which actors does the machine hold? ────────────────────────────


def test_android_reads_whole_slots_through_the_bridge():
    driver = _android(_slot_map(SHARED, ROW))
    slots = driver._current_slots()
    assert set(slots) == {SHARED}
    assert slots[SHARED][f"{SHARED}/grant-registered"] == f"{ROW}:grant"
    assert "fauna/index" not in slots[SHARED]


def test_an_unreadable_android_store_names_nothing():
    driver = AndroidBridgeDriver()

    def gone():
        raise ConnectionError("bridge gone")

    driver.credential_map = gone
    assert driver._current_slots() is None


def test_an_unpinned_relaunch_vaults_the_slots_its_empty_seed_erases(monkeypatch):
    driver = _android_launched({}, _slot_map(SHARED, ROW))
    posted = []
    monkeypatch.setattr(driver, "_await_health", lambda *a, **k: True)
    monkeypatch.setattr(driver, "_delete", lambda *a, **k: None)
    monkeypatch.setattr(driver, "_mark_log_baseline", lambda: None)
    monkeypatch.setattr(driver, "_post", lambda path, body, *a, **k: posted.append(body))
    assert driver.recover()
    assert posted and posted[0]["seed_credentials"] == {}
    assert SHARED in driver._principal_slot_vault


def test_a_callers_store_vaults_nothing_at_relaunch(monkeypatch):
    driver = _android_launched({"seed_credentials": {}}, _slot_map(SHARED, ROW))
    monkeypatch.setattr(driver, "_await_health", lambda *a, **k: True)
    monkeypatch.setattr(driver, "_delete", lambda *a, **k: None)
    monkeypatch.setattr(driver, "_mark_log_baseline", lambda: None)
    monkeypatch.setattr(driver, "_post", lambda *a, **k: None)
    assert driver.recover()
    assert not getattr(driver, "_principal_slot_vault", {})


# ── web ─────────────────────────────────────────────────────────────────────


def _web(stored: dict[str, str]):
    driver = WebBridgeDriver()
    driver.eval_js = lambda script: dict(stored)
    return driver


def test_a_web_driver_owns_its_browser_context():
    assert _web({})._discards_machine_at_teardown()


def test_a_same_context_web_tab_discards_no_machine():
    tab = _web({})
    tab._shares_context = True
    assert not tab._discards_machine_at_teardown()


def test_open_same_context_tab_marks_the_tab_as_sharing(monkeypatch):
    primary = WebBridgeDriver()
    monkeypatch.setattr(primary, "_post", lambda *a, **k: {"page_id": 2})
    monkeypatch.setattr(WebBridgeDriver, "_inject_agent", lambda self: None)
    assert primary.open_same_context_tab()._shares_context
    assert not primary.open_twin_page()._shares_context


def test_web_reads_whole_slots_from_local_storage():
    slots = _web(_slot_map(SHARED, ROW))._current_slots()
    assert set(slots) == {SHARED}


# ── the shared retirement reads these seams ─────────────────────────────────


@pytest.mark.parametrize("make", [_android, _web], ids=["android", "web"])
def test_the_retirement_signs_out_and_asks_the_nest_about_each_shared_row(make, monkeypatch):
    """The base `_retire_discarded_machine` drives web and android through the
    same path as the file-backed drivers: one sign-out through the app, then
    one nest-side question per shared actor's latched row — a dedicated
    actor's slot is left alone."""
    from helpers import shared_identity

    stored = {**_slot_map(SHARED, ROW), **_slot_map(OTHER, "ee" * 32)}
    driver = make(stored)
    if isinstance(driver, AndroidBridgeDriver):
        driver._store_caller_owned = False
    resets, asked = [], []
    monkeypatch.setattr(shared_identity, "is_shared_actor", lambda a: a == SHARED)
    monkeypatch.setattr(driver, "_health_raw", lambda *a, **k: {"ready": True})
    monkeypatch.setattr(driver, "reset", lambda *a, **k: resets.append(k))
    monkeypatch.setattr(
        HttpBridgeDriver,
        "_remove_row_still_granted",
        staticmethod(lambda actor, slot: asked.append((actor, slot[f"{actor}/grant-registered"]))),
    )
    driver._retire_discarded_machine()
    assert len(resets) == 1
    assert asked == [(SHARED, f"{ROW}:grant")]

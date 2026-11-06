"""tier_1: the windows driver's one way onto the box's real Credential Manager.

Convention 10 keeps every windows launch on the shared store's file arm; convention
12's Windows arm is the sanctioned exception, for a witness whose subject is the row
Credential Manager really holds. ``use_real_keyring`` is that opt-in, and these pin
its fences rather than its use, because the box has one store per user: there is no
private daemon to fall back on, so a launch that got the namespace wrong would write
into the installed product's rows. They drive the real ``launch()`` with the bridge
and HTTP stubbed, so the assertions cover the env the app is actually started with.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from common.keyring import unique_namespace  # noqa: E402
from tests.test_windows_driver_data_dir_isolation import (  # noqa: E402
    _launch_env,
    _stubbed_driver,
)

pytestmark = [pytest.mark.tier_1, pytest.mark.windows]

NAMESPACE = unique_namespace("fauna-e2e-real-keyring-pin")
REAL_KEYRING = {"use_real_keyring": True, "keyring_app": NAMESPACE}


@pytest.fixture
def driver(monkeypatch):
    d = _stubbed_driver(monkeypatch)
    yield d
    d.teardown()


def test_a_real_keyring_launch_takes_the_file_store_off(driver):
    env = _launch_env(driver, REAL_KEYRING)
    assert "FAUNA_E2E_CREDENTIAL_DIR" not in env
    assert env["FAUNA_KEYRING_APP"] == NAMESPACE
    assert driver._resolved_keyring_app == NAMESPACE
    assert driver._resolved_credential_dir is None


def test_a_callers_environment_cannot_turn_the_file_store_back_on(driver):
    """The isolated-agent env names a credential dir for the agent's sake, and a
    leftover would quietly put the launch back on the file arm: the witness would
    then read nothing from the real store and prove nothing."""
    env = _launch_env(driver, {
        **REAL_KEYRING, "environment": {"FAUNA_E2E_CREDENTIAL_DIR": r"C:\elsewhere"},
    })
    assert "FAUNA_E2E_CREDENTIAL_DIR" not in env


def test_the_namespace_stays_the_callers_across_a_recover(driver):
    _launch_env(driver, REAL_KEYRING)
    assert driver.recover()
    env = driver._session_body["environment"]
    assert env["FAUNA_KEYRING_APP"] == NAMESPACE
    assert "FAUNA_E2E_CREDENTIAL_DIR" not in env


def test_no_slot_carry_touches_a_real_keyring_store(driver):
    """The carry writes files beside the app's store; here there is none, and the
    store is the test's to give back."""
    _launch_env(driver, REAL_KEYRING)
    assert driver._principal_slot_carry_live is False
    assert driver._discards_machine_at_teardown() is False
    assert driver._resolved_install_secret_store is None


@pytest.mark.parametrize("keyring_app", [
    None, "", "fauna-windows", "fauna-e2e-stable", "fauna-e2e-stable-NOT-HEX",
    "fauna-windows-0a1b2c3d",
])
def test_a_namespace_not_unique_to_the_launch_is_refused_before_anything_starts(
    monkeypatch, keyring_app
):
    d = _stubbed_driver(monkeypatch)
    started = []
    monkeypatch.setattr(d, "_start_bridge", lambda: started.append(True))
    with pytest.raises(RuntimeError, match="keyring_app of its own"):
        d.launch({"app_path": "FaunaApp.exe", "use_real_keyring": True, "keyring_app": keyring_app})
    assert not started
    assert d._session_body is None


@pytest.mark.parametrize("key, value", [
    ("credential_dir", r"C:\store"), ("seed_credentials", {"fauna/index": "{}"}),
])
def test_a_file_store_input_beside_the_real_keyring_is_refused(monkeypatch, key, value):
    """Either key means the test thinks a file is in play. Honouring it would be
    silent, and writing a seed needs a real row the harness never writes."""
    d = _stubbed_driver(monkeypatch)
    with pytest.raises(RuntimeError, match=key):
        d.launch({"app_path": "FaunaApp.exe", **REAL_KEYRING, key: value})
    assert d._session_body is None


def test_an_ordinary_launch_keeps_the_file_store(driver):
    env = _launch_env(driver, {"keyring_app": NAMESPACE})
    assert env["FAUNA_E2E_CREDENTIAL_DIR"] == driver._resolved_credential_dir

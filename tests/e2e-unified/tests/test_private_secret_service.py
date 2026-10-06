"""The private Secret Service a linux real-keyring test runs for itself
(`drivers/secret_service.py::PrivateSecretService`) — convention 10's
`use_real_keyring` carve-out in its 2026-09-15 shape (`e2e-conventions.md`
§ The conventions, point 10).

What is pinned, and why each line matters:

1. The installed `gnome-keyring-daemon` comes up on a private bus **with no
   desktop session** and its login collection **unlocked non-interactively** —
   the whole premise of running one per test.
2. An item written there is invisible to the box's ambient Secret Service, and
   the ambient daemon is never touched — the machine-wide hazard this closes
   (a harness-launched app crashed the desktop daemon, which restarted locked
   and took every other client's secrets with it).
3. A daemon that dies mid-test is restarted over the same keyring and loses
   nothing — the harness's `Restart=on-failure`, with the password systemd
   lacks. That is what lets a force-quit test's relaunch still read its slots.
4. `LibsecretCredStore` rides it end to end: seed → launch config carrying the
   bus → read back → sweep → close, and the driver refuses the mode without
   that bus.

tier_2: real daemons (`dbus-daemon`, `gnome-keyring-daemon`) and no product
binary — the same reasoning `test_harness_self_termination.py` gives. Gated on
the daemon being installed (`skip_environment`), which is the only precondition
the mechanism has.
"""

from __future__ import annotations

import os
import signal
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_2
sys.path.insert(0, str(Path(__file__).parent.parent))

from common.accounts import actor_id_hex
from common.cred_store import LibsecretCredStore, make_cred_store
from common.keyring import (
    ACCT_NODE_URL,
    ACCT_SECRET_KEY,
    XDG_SCHEMA,
    read_field,
    secret_service_available,
    unique_namespace,
)
from drivers.linux import _secret_service_bus_missing, _wants_private_bus
from helpers.app_surface import skip_environment

_posix_only = pytest.mark.skipif(os.name == "nt", reason="D-Bus + gnome-keyring are Linux-only")


@pytest.fixture
def service():
    if not secret_service_available():
        skip_environment("gnome-keyring-daemon + dbus-daemon are not installed")
    from drivers.secret_service import PrivateSecretService

    svc = PrivateSecretService().start()
    try:
        yield svc
    finally:
        svc.stop()


def _attrs(ns: str, account: str) -> dict:
    return {"application": ns, "account": account, "xdg:schema": XDG_SCHEMA}


def _read(svc, ns: str, account: str):
    import secretstorage

    conn = svc.connect()
    try:
        coll = secretstorage.get_default_collection(conn)
        hits = list(coll.search_items(_attrs(ns, account)))
        return hits[0].get_secret() if hits else None
    finally:
        conn.close()


@_posix_only
@pytest.mark.timeout(60)
def test_daemon_comes_up_unlocked_on_a_private_bus_with_no_desktop(service):
    import secretstorage

    assert service.address.startswith("unix:path="), service.address
    assert service.address != os.environ.get("DBUS_SESSION_BUS_ADDRESS")
    conn = service.connect()
    try:
        coll = secretstorage.get_default_collection(conn)
        assert not coll.is_locked(), "the login collection must be unlocked without a prompter"
        ns = unique_namespace("probe")
        coll.create_item("probe", _attrs(ns, ACCT_SECRET_KEY), b"hunter2", replace=True)
    finally:
        conn.close()
    assert _read(service, ns, ACCT_SECRET_KEY) == b"hunter2"
    assert service.daemon_exits == []


@_posix_only
@pytest.mark.timeout(60)
def test_an_item_on_the_private_service_never_reaches_the_ambient_keyring(service):
    """The channel is closed, not just the crash: a seed on the private daemon
    is not in the desktop keyring. Proven against the real ambient bus when the
    box has one; a box without one has no desktop keyring to leak into."""
    import secretstorage

    ns = unique_namespace("probe-leak")
    conn = service.connect()
    try:
        secretstorage.get_default_collection(conn).create_item(
            "probe", _attrs(ns, ACCT_SECRET_KEY), b"hunter2", replace=True
        )
    finally:
        conn.close()
    try:
        ambient = secretstorage.dbus_init()
        coll = secretstorage.get_any_collection(ambient)
    except Exception as e:  # no ambient bus / no Secret Service on it
        pytest.skip(f"no ambient Secret Service to prove non-leakage against ({type(e).__name__})")
    try:
        assert list(coll.search_items(_attrs(ns, ACCT_SECRET_KEY))) == [], (
            "a private-service seed leaked into the ambient keyring"
        )
    finally:
        ambient.close()


@_posix_only
@pytest.mark.timeout(60)
def test_a_daemon_that_dies_is_restarted_over_the_same_keyring_with_nothing_lost(service):
    """`ensure_running` is the harness's `Restart=on-failure` plus the password
    systemd lacks: SIGKILL the daemon (the shape of the ABRT the desktop one
    dies of), and the next connect brings it back with the item intact."""
    import secretstorage

    ns = unique_namespace("probe-restart")
    conn = service.connect()
    try:
        secretstorage.get_default_collection(conn).create_item(
            "probe", _attrs(ns, ACCT_SECRET_KEY), b"survives", replace=True
        )
    finally:
        conn.close()
    os.kill(service._daemon.pid, signal.SIGKILL)
    service._daemon.wait(timeout=10)

    assert service.ensure_running() is True
    assert service.daemon_exits == [-signal.SIGKILL]
    assert _read(service, ns, ACCT_SECRET_KEY) == b"survives"
    assert service.ensure_running() is False, "a live daemon is never restarted"


@_posix_only
@pytest.mark.timeout(60)
def test_stop_takes_the_daemon_and_the_bus_down_and_removes_the_keyring_dir():
    if not secret_service_available():
        skip_environment("gnome-keyring-daemon + dbus-daemon are not installed")
    from drivers.secret_service import PrivateSecretService

    svc = PrivateSecretService().start()
    daemon, state, sock = svc._daemon, svc._state, svc.address.removeprefix("unix:path=")
    assert os.path.isdir(state) and os.path.exists(sock)
    svc.stop()
    assert daemon.poll() is not None, "the daemon outlived stop()"
    assert not os.path.exists(state), "the throwaway keyring dir outlived stop()"
    assert not os.path.exists(sock), "the private bus socket outlived stop()"
    assert svc.address is None
    svc.stop()  # idempotent


@_posix_only
@pytest.mark.timeout(90)
def test_libsecret_cred_store_owns_a_private_service_end_to_end(tmp_path):
    """The store the harness tests actually use: its launch config carries the
    private bus (the driver's caller-owned-bus carve-out, and the only way the
    driver accepts `use_real_keyring`), the seed lands on that bus, and
    `close()` stops the daemon."""
    if not secret_service_available():
        skip_environment("gnome-keyring-daemon + dbus-daemon are not installed")

    store = make_cred_store("linux", tmp_path)
    assert isinstance(store, LibsecretCredStore)
    assert store.service is None, "the daemon starts on first use, not at construction"
    try:
        store.inject_identity(secret_hex="ab" * 32, node_url="https://nest.example")
        svc = store.service
        assert svc is not None and svc.address

        config = store.launch_config("/opt/fauna/fauna-desktop", "https://nest.example")
        assert config["use_real_keyring"] is True
        assert config["environment"]["DBUS_SESSION_BUS_ADDRESS"] == svc.address
        assert _wants_private_bus(config) is False, "the launch rides the store's bus"
        assert _secret_service_bus_missing(config) is False

        actor = actor_id_hex("ab" * 32)
        assert store.stored_accounts() == {
            "fauna/index",
            f"fauna/{actor}/secret",
            f"fauna/{actor}/nest_url",
            f"fauna/{actor}/device_id",
        }
        assert store.read_field(f"fauna/{actor}/secret") == "ab" * 32
        assert (
            read_field(svc.address, config["keyring_app"], f"fauna/{actor}/nest_url")
            == "https://nest.example"
        )

        store.clear()
        assert store.stored_accounts() == set()
        assert store.read_field(f"fauna/{actor}/secret") is None
    finally:
        store.close()
    assert store.service is None
    assert svc._daemon is None and svc.address is None
    store.close()  # idempotent


@_posix_only
@pytest.mark.timeout(90)
def test_both_attach_paths_read_the_launched_bus_not_a_daemon_of_their_own(tmp_path):
    """`attach_cred_store` and `attach_account_store` bind to the store an
    already-launched driver is using. In `use_real_keyring` mode that store is
    the private daemon on the launch's bus; an attach that started its OWN
    daemon would read an empty namespace — exactly how smoke G's "sign-in
    minted the account store" precondition failed on 2026-09-15."""
    from types import SimpleNamespace

    from common.cred_store import (
        account_store_namespace,
        attach_account_store,
        attach_cred_store,
    )

    if not secret_service_available():
        skip_environment("gnome-keyring-daemon + dbus-daemon are not installed")
    owner = make_cred_store("linux", tmp_path)
    try:
        owner.inject_identity(secret_hex="cd" * 32, node_url="https://nest.example")
        config = owner.launch_config("/opt/fauna/fauna-desktop", "https://nest.example")
        # What `drivers/linux.py::launch` records for a real-keyring launch.
        driver = SimpleNamespace(
            _launch_config=config,
            _resolved_credential_dir=None,
            _resolved_keyring_app=config["keyring_app"],
            _resolved_xdg_base=config["xdg_base"],
        )
        # The app's own writes — here, the account store's writer key — land on
        # the same daemon under the derived namespace.
        account_ns = account_store_namespace(config["keyring_app"])
        from common.keyring import inject_fields as _inject

        _inject(
            config["environment"]["DBUS_SESSION_BUS_ADDRESS"],
            account_ns,
            {ACCT_SECRET_KEY: "ef" * 32},
        )

        secret_slot = f"fauna/{actor_id_hex('cd' * 32)}/secret"
        cred = attach_cred_store("linux", driver)
        account = attach_account_store("linux", driver)
        assert cred.service is None and account.service is None, "attached, owning nothing"
        assert cred.read_field(secret_slot) == "cd" * 32
        assert ACCT_SECRET_KEY in account.stored_accounts(), (
            "the attached account store read a daemon other than the launch's"
        )
        cred.close()
        account.close()
        assert owner.read_field(secret_slot) == "cd" * 32, "closing an attached store stops nothing"

        driver._launch_config = {"use_real_keyring": True, "environment": {}}
        with pytest.raises(RuntimeError, match="no caller-owned bus"):
            attach_account_store("linux", driver)
    finally:
        owner.close()


@_posix_only
def test_the_driver_refuses_the_mode_without_a_caller_owned_bus():
    """Neither fallback is acceptable, so the refusal is a launch error naming
    the store that supplies the bus — pinned here beside the pure predicate's
    own test in `test_linux_bridge_env.py`."""
    from drivers import create_driver

    driver = create_driver("linux")
    with pytest.raises(RuntimeError, match="LibsecretCredStore"):
        driver.launch({"app_path": "/nonexistent", "use_real_keyring": True, "xdg_base": "/nonexistent"})

"""tier_1: the android launch harness — seed, launch env and relaunch, with no device.

Authority: `docs/goal/architecture/e2e-automation-surface-gating.md` § The
convention (android's automation surface is `BuildConfig.DEBUG`-gated; the
launch-env seeds cross into the process through `MainActivity`'s `Os.setenv`
door) and `testing.md` § Default app and nest mode → *Android's run venue*.

WHY THIS FILE EXISTS. The routing smoke's case L (the wrong-clock launch
witness, `connect-and-sign-in` outcome 9) runs through
`common.launch_harness.make_launch_harness`, which had no android branch — so
android could not join. The android harness is built now, but the run venue is
still the user's open decision, so no android e2e run can execute it yet. What
would stop the FIRST run on any venue is testable here on a machine with no
device at all:

  1. the identity seed rides the launch config as ``seed_credentials`` — the
     one door into the on-device credential file (the bridge writes it into the
     app's own ``filesDir``), and an EMPTY seed still crosses, so a fresh launch
     cannot inherit an earlier test's file;
  2. a relaunch is a force-quit + start that does NOT re-seed — re-writing the
     launch's seed would erase whatever the app itself wrote, and a survival
     case would pass on the harness's copy instead of the app's;
  3. a case's launch env is forwarded only when the android bridge actually
     carries that name into the app process, and REFUSED otherwise — an intent
     launch has no process environment, so a name the bridge does not read
     would be silently dropped and the case would pass on a launch that never
     saw it;
  4. every name the harness forwards is read by ``BridgeHttpServer``'s
     ``/session`` handler, put on the intent by ``launchApp``, and re-exported by
     ``MainActivity``'s ``Os.setenv`` door — a source-level pin, because nothing
     that runs on this machine executes the Kotlin.

The driver's own ``launch``/``teardown`` are replaced with recorders: what is
under test is the harness's config, not adb (``test_android_driver_adb.py``
owns that surface). Pure state, no timing (convention 14).
"""

import json
import os
import re
import sys
from pathlib import Path

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))

from common.accounts import actor_id_hex  # noqa: E402
from common.cred_store import AndroidCredStore, attach_cred_store, make_cred_store  # noqa: E402
from common.launch_harness import AndroidLaunchHarness, make_launch_harness  # noqa: E402
from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_ANDROID_APP = _REPO / "apps/fauna-android/app/src"
_BRIDGE_KT = _ANDROID_APP / "androidTest/java/com/fauna/app/bridge/BridgeHttpServer.kt"
_MAIN_ACTIVITY_KT = _ANDROID_APP / "main/java/com/fauna/app/MainActivity.kt"

SECRET = "11" * 32
NODE_URL = "http://127.0.0.1:13001"


@pytest.fixture
def launches(monkeypatch):
    """Record every ``(event, config)`` the android driver is handed.

    Returns a LIST rather than a driver for the reason
    `test_android_driver_adb.py`'s ``bridge_posts`` gives: a fixture handing
    back a driver instance tells convention 17's frame probe there is a live
    app frame to observe, and there is none here. Each config is snapshotted
    at the call, since the harness keeps mutating the one dict it relaunches
    from."""
    events = []

    def launch(self, config):
        events.append(("launch", {**config, "environment": dict(config.get("environment") or {})}))

    def teardown(self):
        events.append(("teardown", None))

    monkeypatch.setattr(AndroidBridgeDriver, "launch", launch)
    monkeypatch.setattr(AndroidBridgeDriver, "teardown", teardown)
    return events


def _harness(tmp_path, environment=None, trusted=None):
    extra = {"environment": environment} if environment else None

    def seed_trust(env, nest):
        # Stands in for conftest's `_trust_seeder(request)`: names the nest it
        # was handed, so a test can see the trust seed rode the same env dict.
        env["FAUNA_E2E_TRUST_NEST_IDENTITY"] = f"id-for-{nest['port']}"
        if trusted is not None:
            trusted.append(nest)

    return make_launch_harness(
        "android", tmp_path=tmp_path, app_path="/tmp/app-debug.apk",
        extra_launch_config=extra, seed_trust=seed_trust,
    )


def _launch_configs(events):
    return [cfg for kind, cfg in events if kind == "launch"]


# ── the builder ─────────────────────────────────────────────────────────────


def test_make_launch_harness_builds_an_android_harness(tmp_path):
    harness = _harness(tmp_path)
    assert isinstance(harness, AndroidLaunchHarness)
    assert harness.client == "android"
    assert isinstance(harness.store, AndroidCredStore)


def test_make_cred_store_answers_android(tmp_path):
    assert isinstance(make_cred_store("android", tmp_path), AndroidCredStore)


class _ReadBackDriver:
    """Stands in for a launched android driver's bridge read-back — the one
    host-side view of the on-device credential file."""

    def __init__(self, stored):
        self.stored = stored

    def credential_map(self):
        return dict(self.stored)


def test_attach_cred_store_reads_the_live_file_through_the_driver():
    # The read-back is what `attach_cred_store` used to refuse for android: the
    # file is on-device, so the attached store reads it over the bridge.
    driver = _ReadBackDriver({"fauna/index": "{}", "fauna/abc/secret": "11"})
    store = attach_cred_store("android", driver)
    assert isinstance(store, AndroidCredStore)
    assert store.stored_accounts() == {"fauna/index", "fauna/abc/secret"}
    assert store.read_map() == {"fauna/index": "{}", "fauna/abc/secret": "11"}


def test_attach_cred_store_reads_live_not_a_snapshot():
    driver = _ReadBackDriver({"fauna/index": "{}"})
    store = attach_cred_store("android", driver)
    driver.stored = {}
    assert store.stored_accounts() == set(), (
        "an attached store must read the device file at call time — a sign-out "
        "erase assertion reads it AFTER the erase"
    )


def test_an_unattached_android_store_still_refuses_to_read_back(tmp_path):
    # The launch-routing store holds a pending seed on the host; it has no
    # driver to read the device through, and must not answer from the seed.
    store = make_cred_store("android", tmp_path)
    store.inject_identity(secret_hex=SECRET, node_url=NODE_URL)
    with pytest.raises(NotImplementedError):
        store.stored_accounts()


def test_the_launch_names_the_test_apk_and_the_run_device(tmp_path, launches, monkeypatch):
    # Both are run facts conftest's own android config carries; a harness
    # launch that dropped them would install no bridge APK and talk to
    # whichever device happened to be attached.
    from helpers import android_device

    monkeypatch.setattr(android_device, "_DEVICE_SERIAL", "emulator-5554")
    _harness(tmp_path).launch(secret_hex=SECRET, node_url=NODE_URL, trust=None)
    (config,) = _launch_configs(launches)
    assert config["device_serial"] == "emulator-5554"
    # `TEST_APK` is spelled with forward slashes; the config carries the host's
    # own spelling of the joined path, backslashes on Windows.
    assert Path(config["test_apk"]).as_posix().endswith(android_device.TEST_APK)
    assert config["app_path"] == "/tmp/app-debug.apk"
    assert config["url"] == NODE_URL


# ── 1. the seed rides the launch config ─────────────────────────────────────


def test_the_identity_rides_as_seed_credentials_in_the_registry_shape(tmp_path, launches):
    _harness(tmp_path).launch(secret_hex=SECRET, node_url=NODE_URL, trust=None)
    (config,) = _launch_configs(launches)
    seed = config["seed_credentials"]
    actor = actor_id_hex(SECRET)
    assert seed[f"fauna/{actor}/secret"] == SECRET
    assert seed[f"fauna/{actor}/nest_url"] == NODE_URL
    assert seed[f"fauna/{actor}/device_id"]
    assert json.loads(seed["fauna/index"])["active"] == actor


def test_a_fresh_launch_still_sends_an_empty_seed(tmp_path, launches):
    # The on-device file outlives the test that wrote it (it is in the app's
    # own filesDir, and `adb install -r` keeps data). An absent seed would leave
    # the bridge forwarding that stale file, so "no identity" must be SENT.
    _harness(tmp_path).launch(secret_hex=None, node_url=NODE_URL, trust=None)
    (config,) = _launch_configs(launches)
    assert config["seed_credentials"] == {}


# ── 2. relaunch = force-quit + start, never a re-seed ───────────────────────


def test_relaunch_does_not_reseed(tmp_path, launches):
    harness = _harness(tmp_path)
    harness.launch(secret_hex=SECRET, node_url=NODE_URL, trust=None)
    harness.relaunch()
    kinds = [kind for kind, _ in launches]
    assert kinds == ["launch", "teardown", "launch"]
    first, second = _launch_configs(launches)
    assert first["seed_credentials"]
    assert "seed_credentials" not in second, (
        "the relaunch re-wrote the launch's seed over whatever the app stored "
        f"since: {second['seed_credentials']!r}"
    )


def test_relaunch_reseeds_trust_from_the_nest_each_time(tmp_path, launches):
    trusted = []
    harness = _harness(tmp_path, trusted=trusted)
    harness.launch(secret_hex=SECRET, node_url=NODE_URL, trust={"port": 13001})
    harness.relaunch()
    assert len(trusted) == 2
    for config in _launch_configs(launches):
        assert config["environment"]["FAUNA_E2E_TRUST_NEST_IDENTITY"] == "id-for-13001"


# ── 3. launch env: forwarded when carried, refused when not ─────────────────


def test_the_clock_seed_rides_the_launch_environment(tmp_path, launches):
    harness = _harness(tmp_path, environment={"FAUNA_E2E_CLOCK_OFFSET_SECS": "-21600"})
    harness.launch(secret_hex=SECRET, node_url=NODE_URL, trust={"port": 13001})
    harness.relaunch()
    for config in _launch_configs(launches):
        env = config["environment"]
        assert env["FAUNA_E2E_CLOCK_OFFSET_SECS"] == "-21600"
        # Beside the trust seed, not instead of it.
        assert env["FAUNA_E2E_TRUST_NEST_IDENTITY"] == "id-for-13001"


def test_a_launch_env_name_the_bridge_does_not_carry_is_refused(tmp_path):
    with pytest.raises(ValueError, match="FAUNA_E2E_NOT_A_SEED"):
        _harness(tmp_path, environment={"FAUNA_E2E_NOT_A_SEED": "1"})


# ── 4. every forwarded name crosses both Kotlin doors ───────────────────────


@pytest.mark.parametrize("name", sorted(AndroidLaunchHarness.ANDROID_LAUNCH_ENV))
def test_every_forwarded_name_reaches_the_process_environment(name):
    bridge = _BRIDGE_KT.read_text(encoding="utf-8")
    main = _MAIN_ACTIVITY_KT.read_text(encoding="utf-8")
    assert re.search(rf'env\.optString\("{name}"', bridge), (
        f"BridgeHttpServer's /session handler never reads {name} out of `environment`"
    )
    assert re.search(rf'intent\.putExtra\("{name}"', bridge), (
        f"AppLauncher.launchApp never puts {name} on the launch intent"
    )
    assert re.search(rf'intent\.getStringExtra\("{name}"\)', main) and re.search(
        rf'Os\.setenv\("{name}"', main
    ), f"MainActivity never re-exports {name} into the process environment"


# ── 5. the admin DNS seams cross both Kotlin doors ──────────────────────────
#
# Not a launch-harness name: the admin DNS journeys launch through conftest's
# app fixture, whose android `environment` the bridge's /session handler reads.
# The two keys travel as a key list (`DNS_E2E_ENV_KEYS`), the region seams'
# shape, so the pin reads the lists and the loops that carry them.

_DNS_SEAMS = ("FAUNA_DNS_PROVIDER_FAKE", "FAUNA_DNS_RECHECK_SECS")
_CONFTEST = Path(__file__).resolve().parents[1] / "conftest.py"


def _kotlin_key_list(source, name):
    match = re.search(rf"\b{name} = listOf\(([^)]*)\)", source)
    assert match, f"no `{name} = listOf(...)` key list"
    return set(re.findall(r'"([A-Z0-9_]+)"', match.group(1)))


def test_the_android_app_launch_env_sets_the_dns_seams():
    source = _CONFTEST.read_text(encoding="utf-8")
    start = source.index('elif app_name == "android":')
    block = source[start:source.index('config["environment"] = env', start)]
    for name in _DNS_SEAMS:
        assert re.search(rf'"{name}":\s*"', block), (
            f"conftest's android launch env never sets {name} — the admin DNS "
            "journeys would reach no fake provider / wait production's clock"
        )


def test_the_bridge_carries_the_dns_seams_onto_the_intent():
    bridge = _BRIDGE_KT.read_text(encoding="utf-8")
    assert _kotlin_key_list(bridge, "DNS_E2E_ENV_KEYS") >= set(_DNS_SEAMS)
    assert re.search(r"DNS_E2E_ENV_KEYS\.associateWith \{ env\.optString\(it", bridge), (
        "BridgeHttpServer's /session handler never reads the DNS seams out of `environment`"
    )
    assert re.search(r"dnsEnv: Map<String, String>", bridge) and re.search(
        r"for \(\(key, value\) in dnsEnv\) intent\.putExtra\(key, value\)", bridge
    ), "AppLauncher.launchApp never puts the DNS seams on the launch intent"


def test_main_activity_reexports_the_dns_seams_into_the_process_env():
    main = _MAIN_ACTIVITY_KT.read_text(encoding="utf-8")
    assert _kotlin_key_list(main, "DNS_E2E_ENV_KEYS") >= set(_DNS_SEAMS)
    assert re.search(
        r"for \(key in DNS_E2E_ENV_KEYS\) \{\s*intent\.getStringExtra\(key\)\?\.let \{"
        r"\s*try \{\s*android\.system\.Os\.setenv\(key, it, true\)",
        main,
    ), "MainActivity never re-exports the DNS seams through its Os.setenv door"


# ── 6. the ACME directory seed crosses both Kotlin doors ────────────────────
#
# `e2e-automation-surface-gating.md` § The ACME directory seed: the seed is read
# by `fauna_client_dns::e2e_acme_seam` via std::env, so on android it rides the
# DNS seams' key list — the whole seed, all three variables meaningful on the
# device (the name server is a reversed loopback port, *The name server's
# reach*).

_ACME_SEAMS = (
    "FAUNA_E2E_ACME_DIRECTORY_URL",
    "FAUNA_E2E_ACME_CA_PEM",
    "FAUNA_E2E_ACME_NAMESERVER",
)

def test_the_bridge_and_main_activity_carry_the_acme_seed():
    for path in (_BRIDGE_KT, _MAIN_ACTIVITY_KT):
        keys = _kotlin_key_list(path.read_text(encoding="utf-8"), "DNS_E2E_ENV_KEYS")
        assert keys >= set(_ACME_SEAMS), f"{path.name} drops part of the ACME seed"


def test_the_android_launch_reverses_pebbles_directory_port():
    # The device reaches pebble only as its own loopback through an `adb
    # reverse`, which the driver opens for every port in `nest_ports`.
    source = _CONFTEST.read_text(encoding="utf-8")
    start = source.index('config["nest_ports"] = [')
    block = source[start:source.index("]", start + len('config["nest_ports"] = ['))]
    assert "directory_port(env)" in block, (
        "conftest's android launch never hands pebble's directory port to the "
        "driver's reverse list, so the app's ACME order dies with a bare "
        "connection error"
    )
    assert "name_server_port(env)" in block, (
        "conftest's android launch never hands the fake name server's port to the "
        "driver's reverse list, so the propagation probe's TCP retry and the "
        "managed mirror's UPDATE reach nothing"
    )

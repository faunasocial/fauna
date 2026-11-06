"""The iOS simulator runtime the suite creates its devices on is pinned.

With two Xcode generations' runtimes installed side by side, "the first
available iOS runtime" is an accident of `simctl`'s ordering; the conftest
picks `IOS_SIM_RUNTIME_VERSION`, and only on a box without it the newest
available iOS runtime.
"""

import pytest

pytestmark = pytest.mark.tier_1

import conftest  # noqa: E402  (tests/e2e-unified is on sys.path via conftest)


def _rt(platform: str, version: str, available: bool = True) -> dict:
    return {
        "platform": platform,
        "name": f"{platform} {version}",
        "version": version,
        "isAvailable": available,
        "identifier": f"com.apple.CoreSimulator.SimRuntime.{platform}-{version.replace('.', '-')}",
    }


def test_the_pinned_runtime_wins_whatever_the_order():
    pinned = conftest.IOS_SIM_RUNTIME_VERSION
    runtimes = [_rt("iOS", "26.5"), _rt("watchOS", pinned), _rt("iOS", pinned), _rt("iOS", "99.0")]
    assert conftest._pick_ios_runtime(runtimes)["version"] == pinned
    assert conftest._pick_ios_runtime(list(reversed(runtimes)))["version"] == pinned


def test_without_the_pin_the_newest_available_ios_runtime_is_taken(capsys):
    runtimes = [_rt("iOS", "26.10"), _rt("iOS", "26.9"), _rt("iOS", "30.0", available=False),
                _rt("watchOS", "40.0")]
    picked = conftest._pick_ios_runtime(runtimes)
    assert picked["version"] == "26.10"
    assert "is not installed" in capsys.readouterr().out


def test_no_ios_runtime_is_none():
    assert conftest._pick_ios_runtime([_rt("watchOS", "27.0")]) is None


# The golden device every run clones its devices from is named for the pin, so
# a runtime move names a new one and the old is deleted (mac-simulator-hold.md
# § Per-core tokens, *The e2e lane's simulator boot*).

def test_the_golden_device_name_follows_the_pin():
    name = conftest._ios_golden_name(conftest.IOS_SIM_RUNTIME_VERSION, conftest.IOS_SIM_DEVICE_NAME)
    assert name.startswith(conftest.IOS_GOLDEN_PREFIX)
    assert name.endswith(f"{conftest.IOS_SIM_RUNTIME_VERSION}-"
                         + conftest.IOS_SIM_DEVICE_NAME.replace(" ", "-"))
    assert conftest._ios_golden_name("99.0", conftest.IOS_SIM_DEVICE_NAME) != name
    assert conftest._ios_golden_name(conftest.IOS_SIM_RUNTIME_VERSION, "iPhone Duo") != name


def test_a_golden_device_of_another_pin_is_stale_and_the_current_one_is_kept():
    current = conftest._ios_golden_name(conftest.IOS_SIM_RUNTIME_VERSION, conftest.IOS_SIM_DEVICE_NAME)
    old = conftest._ios_golden_name("26.5", conftest.IOS_SIM_DEVICE_NAME)
    devices = {"iOS-27-0": [
        {"name": current, "udid": "U-CUR", "state": "Shutdown", "isAvailable": True},
        {"name": "fauna-e2e-ios-123-1", "udid": "U-RUN", "state": "Booted", "isAvailable": True},
        {"name": "iPhone 17 Pro", "udid": "U-ROSTER", "state": "Shutdown", "isAvailable": True},
    ], "iOS-26-5": [
        {"name": old, "udid": "U-OLD", "state": "Shutdown", "isAvailable": True},
    ]}
    assert conftest._stale_ios_goldens(devices, current) == ["U-OLD"]
    assert conftest._find_ios_golden(devices, current) == ("U-CUR", "Shutdown")
    assert conftest._find_ios_golden(devices, old + "-x") is None


def test_the_golden_settle_rule_needs_consecutive_calm_windows():
    # cores per window: busy, calm x2, busy (resets the count), then six calm.
    per_window = [3.0, 0.1, 0.1, 1.0] + [0.1] * conftest._SIM_SETTLE_WINDOWS
    readings = [0.0]
    for c in per_window:
        readings.append(readings[-1] + c * conftest._SIM_SETTLE_SAMPLE_S)
    it = iter(readings)
    clock = [0.0]

    def sleep(s):
        clock[0] += s

    cores, waited, capped = conftest._wait_until_simulator_settled(
        "U", tree_cpu_s=lambda _u: next(it), sleep=sleep, clock=lambda: clock[0])
    assert not capped
    assert waited == len(per_window) * conftest._SIM_SETTLE_SAMPLE_S
    assert cores < conftest._SIM_SETTLED_CORES


def test_the_golden_settle_rule_is_capped():
    clock = [0.0]
    total = [0.0]

    def read(_u):
        total[0] += 4.0 * conftest._SIM_SETTLE_SAMPLE_S
        return total[0]

    def sleep(s):
        clock[0] += s

    _cores, waited, capped = conftest._wait_until_simulator_settled(
        "U", tree_cpu_s=read, sleep=sleep, clock=lambda: clock[0])
    assert capped and waited >= conftest._SIM_SETTLE_MAX_S

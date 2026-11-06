"""tier_3: iOS File Provider one-time OS enable — a real OS reading on a real row.

`docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding, *The
one-time OS enable*. No e2e launch holds a domain of its own, because the reconcile
is off under e2e (domains are device-global), and the harness has no injected
reading. So the render is witnessed here on the OS's own answer instead: on a
throwaway device, with the app that embeds the extension driven by the harness.

The first test registers a domain under the account's real scoped identifier before
the app starts, and a domain added on a device where Fauna was never enabled reads
`userEnabled=false`. The Folders page must then show the caption, without the button
(no door into the Files app is measured), with the switch still ON. Turning the
switch off takes the caption away and turning it on brings it back, because the rule
is over desired presences. The reconcile stays off throughout, so the domain is only
ever read.

The second test measures whether the enablement is per domain or provider-wide: a
plain domain first, then an always-enabled one, and whether the first one flips.

The domain add and the set create are fixture setup (testing conventions point 8);
the action under test is the app reading what the OS says.
"""

import secrets
import sys
from pathlib import Path

import pytest

import fauna_ffi
from actions import ActionLayer
import conftest
from conftest import IOS_SIM_DEVICE_NAME, IOS_SIM_RUNTIME_VERSION, _login_app_as
from drivers import create_driver
from helpers.waiting import wait_until
from tests.platform.ios.test_file_provider_eviction import (  # noqa: F401
    _device_runtime,
    _extension_diagnostics,
    _fauna,
    fauna_ios_fp_app,
    fp_device,
)

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="the iOS simulator runs on macOS only"),
    pytest.mark.tier_3,
]

_CAPTION = "Not shown in Files yet"


def _register_raw(udid: str, domain_id: str, *mode: str) -> None:
    """Add a domain under exactly `domain_id`. A fresh device has answered FP -2014
    in some runs and not in others (`test_file_provider_eviction._register`), so that
    one answer is waited out."""
    answers = []

    def answered() -> bool:
        answers.append(_fauna(udid, "register", domain_id, domain_id, *mode))
        return "Code=-2014" not in answers[-1]

    wait_until(
        answered, 120, interval=2.0,
        diagnose=lambda: f"{answers[-1]}\n{_extension_diagnostics(udid)}",
    )
    assert "register OK" in answers[-1], (
        f"the domain did not register: {answers[-1]}\n{_extension_diagnostics(udid)}"
    )


def _user_enabled(udid: str, domain_id: str) -> bool:
    """What `list` says the OS holds for one domain."""
    listed = _fauna(udid, "list")
    line = f"domain {domain_id} userEnabled="
    found = [row for row in listed.splitlines() if row.strip().startswith(line)]
    assert found, f"{domain_id} is not in the OS's list: {listed}"
    return found[-1].strip().endswith("true")


@pytest.mark.timeout(2400)
def test_a_domain_the_os_holds_disabled_captions_its_row(
    nest_instance, test_user, request, fp_device, fauna_ios_fp_app  # noqa: F811
):
    udid = fp_device
    print(f"[os-enable] runtime {_device_runtime(udid)}")
    seed = bytes(test_user["signing_key"])
    actor_hex = bytes(test_user["signing_key"].verify_key).hex()
    name = f"osenable-{secrets.token_hex(4)}"
    created = fauna_ffi.harness_create_set(nest_instance["url"], seed, {"name": name})
    domain_id = f"local%3A{created['id']}@{actor_hex}"

    _register_raw(udid, domain_id)
    assert _user_enabled(udid, domain_id) is False, (
        "a domain added on a device where Fauna was never enabled must read false"
    )

    environment: dict = {}
    conftest._apply_r14_trust_env(environment, nest_instance, request)
    driver = create_driver("ios")
    driver.launch(
        {
            "url": nest_instance["url"],
            "app_path": str(fauna_ios_fp_app),
            "udid": udid,
            "device_name": IOS_SIM_DEVICE_NAME,
            "platform_version": IOS_SIM_RUNTIME_VERSION,
            "keep_container": True,
            "environment": environment,
        }
    )
    try:
        app = ActionLayer(driver)
        _login_app_as(app, request, nest_instance, test_user)
        b = app.backups

        def enter_row() -> None:
            app.mail_settings.navigate()
            b.navigate_folders()
            b.find_and_expand_folder(name)

        b.navigate_folders()
        b.find_and_expand_folder(name)
        # Turning the switch on is the enrol gesture: the set joins the plan only
        # with this device's place, which a set born through the API has not got.
        assert b.on_demand_toggle_state() == "on"
        b.toggle_on_demand()
        enter_row()
        assert b.on_demand_os_enable_hint() is None, (
            "a set toggled off is not desired, so its row carries no notice"
        )
        b.toggle_on_demand()

        def caption_shown() -> bool:
            enter_row()
            return (b.on_demand_os_enable_hint() or "").startswith(_CAPTION)

        wait_until(
            caption_shown, 60, interval=1.0,
            diagnose=lambda: (
                f"{driver.diagnose('folder-on-demand-os-enable-hint')} "
                f"userEnabled={_user_enabled(udid, domain_id)} error={app.error_text()!r}"
            ),
        )
        print(f"[os-enable] caption: {b.on_demand_os_enable_hint()!r}")
        assert b.on_demand_toggle_state() == "on", "the notice never touches the switch"
        assert driver.is_absent("folder-on-demand-open-settings-button"), (
            "iOS has no measured door into the Files app"
        )
    finally:
        driver.teardown()

    assert "remove OK" in _fauna(udid, "remove", domain_id)


@pytest.mark.timeout(1800)
def test_enablement_is_measured_in_the_inverse_order(fp_device, fauna_ios_fp_app):  # noqa: F811
    udid = fp_device
    print(f"[os-enable] runtime {_device_runtime(udid)}")
    plain, always = "fauna-probe-plain", "fauna-probe-always"

    _register_raw(udid, plain)
    assert _user_enabled(udid, plain) is False
    _register_raw(udid, always, "always-enabled")
    assert _user_enabled(udid, always) is True
    print(f"[os-enable] after an always-enabled domain, plain reads {_user_enabled(udid, plain)}")

    assert "remove OK" in _fauna(udid, "remove", always)
    assert "remove OK" in _fauna(udid, "remove", plain)

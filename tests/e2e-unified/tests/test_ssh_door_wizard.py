"""tier_3: the SSH door, wizard side — the toggle reaches the box's cloud-init and
the key reaches this device's ``~/.ssh``.

`docs/goal/behavior/onboarding-provisioning.md` § 5: beside the mail-mode toggle
sits ``vps-config-ssh-door-toggle``, **default OFF**; turning it on reveals
``vps-config-ssh-door-warning``; on a successful run the provisioning page shows
``provisioning-ssh-door-command`` — on a desktop app ``ssh <domain>`` and the path
of the key the app wrote to ``~/.ssh``; on the apps with no ``~/.ssh`` (ios,
android, web — ``ui.yaml``'s ``platform_elements``) ``ssh -i fauna-<domain>
root@<domain>`` and ``provisioning-ssh-door-save-key-button``, which hands the key
file over once (a share sheet, a download — observed in ``driver.download_dir()``)
— either way with the one admitted address. `installers/vps.md` § The SSH door
owns what the box does with it: a key-only sshd whose drop-in is
``/etc/ssh/sshd_config.d/0-fauna-door.conf``, carrying the key the app minted, and
*One address*: at the claim the nest writes the claimant's address to the host
channel's ``ssh-door-source``, and its claim reply echoes it.

The run is the standard-path journey of ``test_provisioning_claims_a_real_nest``
(VPS + DNS at the fake cloud, the nest leg at a real never-claimed nest), so
``Succeeded`` is a real build-and-claim. The witnesses are on the far side of the
app: the cloud-init the fake provider RECEIVED on ``POST /servers``, the key
file on disk, read back by the system ``ssh-keygen``, and the ``ssh-door-source``
the real nest wrote into its maintenance dir (which the test gives it, as the
box's compose mounts one).

⚠ The key dir is pointed at the test's own scratch ``.ssh``
(``set_ssh_door_key_dir``) before the run on a desktop app: a tui under test keeps
the developer's real HOME on linux, and a door journey must never write the
developer's own ``~/.ssh``. A hand-over app gets no key dir — the key waits in the
machine for the save-key button.
"""
import json
import os
import shutil
import stat
import subprocess
from pathlib import Path

import pytest

from helpers.budgets import ORCHESTRATION_STEP_S, UI_SETTLE_S
from helpers.provisioning_drive import (
    overall,
    point_providers_at,
    seed_standard_path_run,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

DOOR_DROPIN = "/etc/ssh/sshd_config.d/0-fauna-door.conf"


def _hands_key_over(drv) -> bool:
    """The apps with no ``~/.ssh`` hand the door key over once
    (``provisioning-ssh-door-save-key-button``, ios/android/web in ``ui.yaml``)."""
    return drv.is_web() or drv.is_mobile()


def _created_user_data(fake_cloud) -> str:
    """The cloud-init of the one ``POST /servers`` the fake provider received."""
    creates = [
        req
        for req, _resp in fake_cloud._h.log
        if req.method == "POST" and req.path.endswith("/hetzner-cloud/servers")
    ]
    assert len(creates) == 1, f"expected one create_server, saw {len(creates)}"
    return json.loads(creates[0].get_data(as_text=True))["user_data"]


def _run_to_succeeded(app, fake_cloud, nest, *, door: bool, key_dir: str) -> None:
    drv = app.driver
    point_providers_at(app, fake_cloud, nest_base_url=nest["url"], nest=nest)
    seed_standard_path_run(app, claim_code=nest["claim_code"])
    if not _hands_key_over(drv):
        drv.call_machine_method("set_ssh_door_key_dir", json.dumps(key_dir))

    # Back to vps_config: the door is chosen there, by the admin's own gesture.
    app.click("provisioning-back-button")
    app.wait_for("vps-config-ssh-door-toggle", timeout=UI_SETTLE_S)
    assert drv.is_absent("vps-config-ssh-door-warning"), (
        "the door is off by default, so its warning must not show: "
        + drv.diagnose("vps-config-ssh-door-warning")
    )
    if door:
        app.click("vps-config-ssh-door-toggle")
        wait_until(
            lambda: drv.is_visible_scrolled("vps-config-ssh-door-warning"),
            UI_SETTLE_S,
            diagnose=lambda: "the warning never showed after the toggle: "
            + drv.diagnose("vps-config-ssh-door-warning"),
        )
    wait_until(lambda: app.is_enabled("vps-config-continue-button"), UI_SETTLE_S)
    app.click("vps-config-continue-button")
    app.wait_for("provisioning-start-button", timeout=UI_SETTLE_S)

    app.click("provisioning-start-button")
    last = {"snap": None}

    def succeeded():
        last["snap"] = drv.call_machine_method("provisioning_snapshot")
        return overall(app) == "Succeeded"

    wait_until(
        succeeded,
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "the run never reached Succeeded. snapshot="
        + json.dumps(last["snap"])
        + f" | error={app.error_text()!r}",
    )


def test_the_door_on_reaches_the_cloud_init_and_the_key_lands_in_dot_ssh(
    app, fake_cloud, provision_target_nest, tmp_path
):
    drv = app.driver
    ssh_dir = tmp_path / ".ssh"
    # The nest→host channel a VPS box mounts at `<data_dir>/maintenance`.
    maintenance = Path(provision_target_nest["db_path"]).parent / "maintenance"
    maintenance.mkdir(exist_ok=True)
    _run_to_succeeded(app, fake_cloud, provision_target_nest, door=True, key_dir=str(ssh_dir))

    user_data = _created_user_data(fake_cloud)
    assert DOOR_DROPIN in user_data, "the door box carries the key-only sshd drop-in"
    authorized = next(
        (ln.strip().lstrip("- ").strip() for ln in user_data.splitlines() if "ssh-ed25519 " in ln),
        None,
    )
    assert authorized, "the door box carries root's authorized key"

    readout = drv.call_machine_method("ssh_door_readout")
    assert isinstance(readout, dict), f"no door readout after a door run: {readout!r}"
    if _hands_key_over(drv):
        _assert_the_key_is_handed_over(app, readout, user_data, authorized)
        return
    key_path = readout["key_path"]
    assert key_path == str(ssh_dir / f"fauna-{readout['domain']}"), readout

    # The one admitted address is the one the nest handed the host at the claim.
    source = (maintenance / "ssh-door-source").read_text()
    assert source.startswith("address="), source
    address = source.strip().removeprefix("address=")
    assert readout["admitted_from"] == address, (readout, source)

    # The page shows it — the command, the admitted address and the key's path.
    app.wait_for("provisioning-ssh-door-command", timeout=UI_SETTLE_S)
    shown = drv.get_text("provisioning-ssh-door-command")
    assert f"ssh {readout['domain']}" in shown and key_path in shown, shown
    assert address in shown, shown

    # The key on disk is the private half of exactly the key the box admits.
    assert stat.S_IMODE(os.stat(key_path).st_mode) == 0o600
    config = (ssh_dir / "config").read_text()
    assert f"Host {readout['domain']}\n    User root\n" in config, config
    private = open(key_path).read()
    body = next(ln for ln in private.splitlines() if not ln.startswith("-----"))
    assert body not in user_data, "the private key must never reach the provider"
    if shutil.which("ssh-keygen"):
        derived = subprocess.run(
            ["ssh-keygen", "-y", "-f", key_path], capture_output=True, text=True, check=True
        ).stdout.split()
        assert derived[:2] == authorized.split()[:2], (derived, authorized)


def _assert_the_key_is_handed_over(app, readout, user_data, authorized) -> None:
    """ios/android/web: no key path, the ``ssh -i`` form, and the key file handed
    over once by the save-key button — the private half of the key the box admits."""
    drv = app.driver
    domain = readout["domain"]
    assert readout["key_path"] is None, readout
    assert readout["command"] == f"ssh -i fauna-{domain} root@{domain}", readout

    app.wait_for("provisioning-ssh-door-command", timeout=UI_SETTLE_S)
    shown = drv.get_text("provisioning-ssh-door-command")
    assert readout["command"] in shown and readout["fingerprint"] in shown, shown

    app.wait_for("provisioning-ssh-door-save-key-button", timeout=UI_SETTLE_S)
    app.click("provisioning-ssh-door-save-key-button")
    saved: dict = {}

    def arrived():
        d = drv.download_dir()
        path = os.path.join(d, f"fauna-{domain}") if d else None
        if path and os.path.isfile(path) and os.path.getsize(path) > 0:
            saved["path"] = path
            return True
        return False

    wait_until(
        arrived,
        UI_SETTLE_S,
        diagnose=lambda: f"no fauna-{domain} in download_dir {drv.download_dir()!r}: "
        + drv.diagnose("provisioning-ssh-door-save-key-button"),
    )
    private = open(saved["path"]).read()
    assert private.startswith("-----BEGIN OPENSSH"), private[:40]
    body = next(ln for ln in private.splitlines() if not ln.startswith("-----"))
    assert body not in user_data, "the private key must never reach the provider"
    # Once: the machine dropped its copy as it handed it over, so the button goes.
    wait_until(
        lambda: drv.count("provisioning-ssh-door-save-key-button") == 0,
        UI_SETTLE_S,
        diagnose=lambda: "the save-key button outlived its one hand-over: "
        + drv.diagnose("provisioning-ssh-door-save-key-button"),
    )
    if shutil.which("ssh-keygen"):
        os.chmod(saved["path"], 0o600)
        derived = subprocess.run(
            ["ssh-keygen", "-y", "-f", saved["path"]], capture_output=True, text=True, check=True
        ).stdout.split()
        assert derived[:2] == authorized.split()[:2], (derived, authorized)


def test_the_door_off_renders_no_door_and_writes_nothing(
    app, fake_cloud, provision_target_nest, tmp_path
):
    drv = app.driver
    ssh_dir = tmp_path / ".ssh"
    _run_to_succeeded(app, fake_cloud, provision_target_nest, door=False, key_dir=str(ssh_dir))

    user_data = _created_user_data(fake_cloud)
    assert DOOR_DROPIN not in user_data
    assert "ssh-ed25519" not in user_data
    assert "ssh_pwauth" not in user_data
    assert drv.call_machine_method("ssh_door_readout") is None
    assert drv.count("provisioning-ssh-door-command") == 0
    assert drv.count("provisioning-ssh-door-save-key-button") == 0
    assert not ssh_dir.exists(), "a door-less run writes nothing to ~/.ssh"

"""tier_4: the host reboot policy a provisioned box ships, run end-to-end.

admin-nest outcome 19 — *the server installs security updates by itself and
restarts itself when nobody is connected, or within a day at the latest* — is
the HOST's behaviour, not the nest binary's, so no api-tier nest can witness it:
cloud-init (``libs/fauna-provisioning/src/cloud_init.rs``) writes the
``unattended-upgrades`` configuration (its own reboot switched off) and the
``fauna-reboot-coordinator`` script + timer that own reboots.

This renders the user-data through the real ``build_cloud_init`` (the test-only
``render_cloud_init`` example), takes the coordinator script out of its
``write_files`` entry and runs it — the whole ``fauna_coordinator_main``, not
just the decision function the crate's unit tests already pin — as root in a
throwaway Debian container, where the absolute host paths it hard-codes
(``/run/reboot-required``, ``/opt/fauna/maintenance{,-host}``, ``/proc/uptime``)
can be planted. ``systemctl`` and ``logger`` are recording shims; everything
else (``stat``, ``date``, ``timeout``, ``grep``) is the image's own coreutils,
as on the Ubuntu host. Cases: a pending update with nobody connected reboots; a
pending update with live connections and a fresh ``reboot-required`` holds; the
same with ``reboot-required`` older than the 24 h ceiling reboots; nothing
pending holds even when idle.

No app driver and no nest: a ``nest`` witness. Authority:
``docs/goal/architecture/installers/vps.md`` § Host OS Maintenance (§ 2 — Reboot).
"""

import subprocess
import uuid

import pytest

from .helpers import (
    SCANNER_SIDECAR_IMAGE,
    ensure_image,
    fence_cpuset_args,
    remove_container,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    pytest.mark.feature("admin-nest"),
]

COORDINATOR_PATH = "/usr/local/sbin/fauna-reboot-coordinator"
# The coordinator's own constant (cloud_init.rs `CEILING_SECS`).
CEILING_SECS = 86400
# A stock Debian userland with GNU coreutils, already the harness's sidecar
# image — the coordinator needs nothing beyond a POSIX sh and coreutils.
HOST_IMAGE = SCANNER_SIDECAR_IMAGE


def _write_files(user_data: str) -> dict[str, str]:
    """``{path: content}`` for every ``write_files`` entry of the rendered
    user-data. `build_cloud_init` emits each as ``  - path: P`` /
    ``    content: |`` / the body indented six spaces / ``    permissions:``,
    so the block scalar is read back by that indent."""
    files: dict[str, str] = {}
    lines = user_data.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("  - path: ") and i + 1 < len(lines) \
                and lines[i + 1] == "    content: |":
            path = line[len("  - path: "):].strip()
            body = []
            i += 2
            while i < len(lines) and (lines[i].startswith("      ") or lines[i] == ""):
                body.append(lines[i][6:])
                i += 1
            files[path] = "\n".join(body).rstrip("\n") + "\n"
            continue
        i += 1
    return files


@pytest.fixture(scope="module")
def user_data(render_cloud_init_binary) -> str:
    r = subprocess.run([render_cloud_init_binary], capture_output=True, text=True,
                       timeout=60, check=True)
    assert r.stdout.startswith("#cloud-config"), r.stdout[:200]
    return r.stdout


@pytest.fixture(scope="module")
def shipped_files(user_data) -> dict[str, str]:
    return _write_files(user_data)


@pytest.fixture(scope="module")
def coordinator_script(shipped_files) -> str:
    script = shipped_files.get(COORDINATOR_PATH)
    assert script, f"no {COORDINATOR_PATH} write_files entry in the rendered user-data"
    assert script.startswith("#!/bin/sh\n")
    return script


def test_unattended_upgrades_on_with_its_own_reboot_off(user_data, shipped_files):
    """The live-apply half: cloud-init installs `unattended-upgrades`, turns its
    periodic run on and its own reboot off (the coordinator owns reboots), and
    enables the coordinator's timer."""
    assert "\n  - unattended-upgrades\n" in user_data
    auto = shipped_files["/etc/apt/apt.conf.d/20auto-upgrades"]
    assert 'APT::Periodic::Update-Package-Lists "1";' in auto
    assert 'APT::Periodic::Unattended-Upgrade "1";' in auto
    unattended = shipped_files["/etc/apt/apt.conf.d/50unattended-upgrades"]
    assert '"${distro_id}:${distro_codename}-security";' in unattended
    assert 'Unattended-Upgrade::Automatic-Reboot "false";' in unattended
    timer = shipped_files["/etc/systemd/system/fauna-reboot-coordinator.timer"]
    assert "OnUnitActiveSec=15min" in timer
    assert f"ExecStart={COORDINATOR_PATH}" in \
        shipped_files["/etc/systemd/system/fauna-reboot-coordinator.service"]
    assert "systemctl enable --now fauna-reboot-coordinator.timer" in user_data


# The shims record each call, one line per invocation, so a hold is "no line".
_SHIM = '#!/bin/sh\nprintf "%s\\n" "$*" >> /tmp/{name}.calls\n'


@pytest.fixture()
def host(coordinator_script, tmp_path):
    """A throwaway root 'host' with the shipped coordinator installed at its
    real path, the two maintenance dirs cloud-init's runcmd creates, and
    recording `systemctl`/`logger` shims. Yields an ``exec(sh_snippet)``."""
    ensure_image(HOST_IMAGE)
    name = f"fauna-reboot-coordinator-{uuid.uuid4().hex[:10]}"
    remove_container(name)
    subprocess.run(
        ["docker", "run", "-d", *fence_cpuset_args(), "--network", "none",
         "--name", name, "--entrypoint", "sleep", HOST_IMAGE, "infinity"],
        capture_output=True, text=True, timeout=60, check=True,
    )
    try:
        script = tmp_path / "fauna-reboot-coordinator"
        script.write_text(coordinator_script)
        subprocess.run(["docker", "cp", str(script), f"{name}:{COORDINATOR_PATH}"],
                       capture_output=True, text=True, timeout=30, check=True)

        def exec_sh(snippet: str) -> subprocess.CompletedProcess:
            return subprocess.run(["docker", "exec", name, "sh", "-c", snippet],
                                  capture_output=True, text=True, timeout=60)

        setup = exec_sh(
            f"chmod 0755 {COORDINATOR_PATH}"
            " && mkdir -p /opt/fauna/maintenance /opt/fauna/maintenance-host"
            + "".join(
                f" && printf '%s' '{_SHIM.format(name=tool)}' > /usr/local/bin/{tool}"
                f" && chmod 0755 /usr/local/bin/{tool}"
                for tool in ("systemctl", "logger")
            )
            + " && command -v systemctl && command -v logger"
        )
        assert setup.returncode == 0, setup.stderr
        assert setup.stdout.split() == ["/usr/local/bin/systemctl",
                                        "/usr/local/bin/logger"], setup.stdout
        yield exec_sh
    finally:
        remove_container(name)


def _plant(exec_sh, *, pending: bool, pending_age_s: int, connections: int):
    """Plant the host's view: the nest's readiness file (container-owned dir)
    and, when `pending`, `/run/reboot-required` dated `pending_age_s` ago —
    its mtime is the coordinator's ceiling clock."""
    snippet = (
        f"printf 'ready=true\\nconnection_count={connections}\\n'"
        " > /opt/fauna/maintenance/nest-readiness && rm -f /run/reboot-required"
    )
    if pending:
        snippet += (
            " && touch /run/reboot-required"
            f" && touch -d \"@$(( $(date +%s) - {pending_age_s} ))\" /run/reboot-required"
        )
    r = exec_sh(snippet)
    assert r.returncode == 0, r.stderr


def _run_coordinator(exec_sh) -> tuple[list[str], dict[str, str], str]:
    """One timer firing: run the shipped coordinator, return the `systemctl`
    calls it made, the host-status it published, and the logger lines."""
    run = exec_sh(COORDINATOR_PATH)
    assert run.returncode == 0, f"coordinator exited {run.returncode}:\n{run.stderr}"
    calls = exec_sh("cat /tmp/systemctl.calls 2>/dev/null || true").stdout.splitlines()
    logged = exec_sh("cat /tmp/logger.calls 2>/dev/null || true").stdout
    status_text = exec_sh("cat /opt/fauna/maintenance-host/host-status").stdout
    status = dict(line.split("=", 1) for line in status_text.splitlines() if "=" in line)
    return calls, status, logged


@pytest.mark.parametrize(
    "pending,pending_age_s,connections,expect_reboot",
    [
        # (a) idle: nothing can be in flight, so a pending update reboots now.
        (True, 60, 0, True),
        # (b) users connected, the update pending for a minute: hold.
        (True, 60, 3, False),
        # (c) users connected, but pending past the 24 h ceiling: reboot.
        (True, CEILING_SECS + 3600, 3, True),
        # nothing pending: no reboot, however idle.
        (False, 0, 0, False),
    ],
    ids=["idle-reboots", "busy-fresh-holds", "busy-past-ceiling-reboots",
         "nothing-pending-holds"],
)
def test_coordinator_reboots_idle_or_at_the_ceiling(
    host, pending, pending_age_s, connections, expect_reboot,
):
    _plant(host, pending=pending, pending_age_s=pending_age_s, connections=connections)
    calls, status, logged = _run_coordinator(host)

    # The host status it publishes for the nest reads the same planted state:
    # the pending flag, and the ceiling clock taken from reboot-required's mtime.
    assert status.get("reboot_pending") == ("true" if pending else "false"), status
    if pending:
        mtime = int(host("stat -c %Y /run/reboot-required").stdout.strip())
        assert status.get("reboot_deferred_since") == str(mtime), status
    else:
        assert status.get("reboot_deferred_since") == "", status

    if expect_reboot:
        assert calls == ["reboot"], f"expected one `systemctl reboot`, got {calls}"
        assert f"connection_count={connections}" in logged, logged
    else:
        assert calls == [], f"expected a hold, got systemctl calls {calls}"

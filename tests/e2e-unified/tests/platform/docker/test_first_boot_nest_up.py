"""tier_4: the first-boot unit a provisioned box ships, its retry loop run for real.

A provisioned box turns sshd off before it brings the nest up, so a pull that
fails at first boot — the channel's tag not yet published, a registry outage, a
transient network error — must never strand it: cloud-init
(``libs/fauna-provisioning/src/cloud_init.rs``) writes ``fauna-nest-up`` and the
``fauna-nest.service`` unit that runs it, and the script retries ``docker
compose up -d`` until it succeeds.

This renders the user-data through the real ``build_cloud_init`` (the test-only
``render_cloud_init`` example), takes the script out of its ``write_files``
entry and runs it at its real path as root in a throwaway Debian container,
against a stub ``docker`` that fails N times and then succeeds; ``logger`` is a
recording shim and the interval is shortened through the script's test-only
``FAUNA_NEST_UP_INTERVAL_S``. The attempt count, the logged retries and the
exit status are asserted — no wall-clock dependence.

No app driver and no nest: a ``nest`` witness. Authority:
``docs/goal/architecture/installers/vps.md`` § What Happens Behind the Scenes,
step 3.
"""

import subprocess
import uuid

import pytest

from .helpers import (
    SCANNER_SIDECAR_IMAGE,
    cloud_init_write_files,
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

NEST_UP_PATH = "/usr/local/sbin/fauna-nest-up"
UNIT_PATH = "/etc/systemd/system/fauna-nest.service"
COMPOSE_ARGS = "compose -f /opt/fauna/docker-compose.yml up -d"
# A stock Debian userland (dash as /bin/sh, as on the Ubuntu host), already the
# harness's sidecar image.
HOST_IMAGE = SCANNER_SIDECAR_IMAGE


@pytest.fixture(scope="module")
def user_data(render_cloud_init_binary) -> str:
    r = subprocess.run([render_cloud_init_binary], capture_output=True, text=True,
                       timeout=60, check=True)
    assert r.stdout.startswith("#cloud-config"), r.stdout[:200]
    return r.stdout


@pytest.fixture(scope="module")
def shipped_files(user_data) -> dict[str, str]:
    return cloud_init_write_files(user_data)


def test_runcmd_starts_the_unit_instead_of_the_compose(user_data, shipped_files):
    """runcmd never runs the compose once by itself: it enables the unit and
    starts it without blocking cloud-init, and the unit runs the script."""
    runcmd = user_data.split("\nruncmd:\n", 1)[1]
    assert "docker compose" not in runcmd, runcmd
    assert runcmd.rstrip().endswith(
        "  - systemctl enable fauna-nest.service\n"
        "  - systemctl start --no-block fauna-nest.service"
    ), runcmd
    unit = shipped_files[UNIT_PATH]
    assert f"ExecStart={NEST_UP_PATH}" in unit
    assert "WantedBy=multi-user.target" in unit
    assert shipped_files[NEST_UP_PATH].startswith("#!/bin/sh\n")


# Each shim records its argv, one line per call. The `docker` stub fails until
# it has been called `{succeed_on}` times.
_LOGGER = '#!/bin/sh\nprintf "%s\\n" "$*" >> /tmp/logger.calls\n'
_DOCKER = ('#!/bin/sh\nprintf "%s\\n" "$*" >> /tmp/docker.calls\n'
           '[ "$(wc -l < /tmp/docker.calls)" -ge {succeed_on} ]\n')


@pytest.fixture()
def host(shipped_files, tmp_path):
    """A throwaway root 'host' with the shipped script at its real path and
    recording `logger` shim. Yields an ``exec(sh_snippet)``."""
    ensure_image(HOST_IMAGE)
    name = f"fauna-nest-up-{uuid.uuid4().hex[:10]}"
    remove_container(name)
    subprocess.run(
        ["docker", "run", "-d", *fence_cpuset_args(), "--network", "none",
         "--name", name, "--entrypoint", "sleep", HOST_IMAGE, "infinity"],
        capture_output=True, text=True, timeout=60, check=True,
    )
    try:
        script = tmp_path / "fauna-nest-up"
        script.write_text(shipped_files[NEST_UP_PATH])
        subprocess.run(["docker", "cp", str(script), f"{name}:{NEST_UP_PATH}"],
                       capture_output=True, text=True, timeout=30, check=True)

        def exec_sh(snippet: str, env: dict[str, str] | None = None):
            flags = [a for k, v in (env or {}).items() for a in ("-e", f"{k}={v}")]
            return subprocess.run(["docker", "exec", *flags, name, "sh", "-c", snippet],
                                  capture_output=True, text=True, timeout=60)

        setup = exec_sh(
            f"chmod 0755 {NEST_UP_PATH}"
            f" && printf '%s' '{_LOGGER}' > /usr/local/bin/logger"
            " && chmod 0755 /usr/local/bin/logger && command -v logger"
        )
        assert setup.returncode == 0, setup.stderr
        assert setup.stdout.split() == ["/usr/local/bin/logger"], setup.stdout
        yield exec_sh
    finally:
        remove_container(name)


@pytest.mark.parametrize("failures", [0, 3], ids=["pulls-first-time", "retries-until-pull"])
def test_nest_up_retries_until_compose_succeeds(host, failures):
    plant = host(
        f"printf '%s' '{_DOCKER.format(succeed_on=failures + 1)}' > /usr/local/bin/docker"
        " && chmod 0755 /usr/local/bin/docker && command -v docker"
    )
    assert plant.returncode == 0, plant.stderr
    assert plant.stdout.strip() == "/usr/local/bin/docker", plant.stdout

    run = host(NEST_UP_PATH, env={"FAUNA_NEST_UP_INTERVAL_S": "0"})
    assert run.returncode == 0, f"fauna-nest-up exited {run.returncode}:\n{run.stderr}"

    attempts = host("cat /tmp/docker.calls").stdout.splitlines()
    assert attempts == [COMPOSE_ARGS] * (failures + 1), attempts
    logged = host("cat /tmp/logger.calls 2>/dev/null || true").stdout.splitlines()
    assert logged == ["-t fauna-nest-up compose up failed; retrying in 0 s"] * failures, logged

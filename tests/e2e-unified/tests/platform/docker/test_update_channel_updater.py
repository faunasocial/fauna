"""tier_4 — the Fauna updater against the real nest image and the real engine.

`docs/goal/architecture/nest/update-channel.md` § Tests (the tier_4 bullet) and
§ The updater. Every piece of the updater is unit-tested against fakes (a
wiremock registry, a recording engine, inspect fixtures); this module is where
the real ones meet: the image the tier_4 suite built, a real Docker daemon, and
a registry the daemon really pulls from.

**The bench.** A harness registry on loopback (`helpers/oci_registry.py`) holds
the built image re-published under several version labels — the same layers, a
rewritten config, so each label is a distinct build (its own image id) without
building anything. A nest container runs one of them as
``127.0.0.1:<port>/faunasocial/nest:test``, labelled ``social.fauna.update=true``
with its own volume and the ``test`` seed, exactly as the bundles create it. The
updater runs as the bundles run it — a container of the image, the Docker socket
its only mount — except that it is the ``test-hooks`` build
(`updater-e2e.Dockerfile`), which reads that registry instead of ghcr.io and
manages its ``faunasocial/nest``; it shares the host network so it reaches the
loopback registry, and reads the nest's health at its bridge address as it does
in the cloud bundle.

**What it witnesses**, in order (one bench, so the order is the story):

* **Current** — the channel's head is the very build the box runs: nothing is
  re-created.
* **Hold** — the channel's head is OLDER than the running build: nothing is
  re-created, and the log names both versions.
* **Apply** — a newer build appears: the nest is re-created under its own name,
  with its volume, environment and labels, on the new build; the old container
  and its WS clients get the nest's 1001 drain inside the stop grace period;
  the admin's claim survives on the volume.
* **A channel change from the app** — ``fauna.admin.update_channel.set`` to Dev
  is picked up on the updater's next minute and Dev's head is applied.
* **Reconcile** — a re-create cut short after the old container was stepped
  aside and stopped (the state a crash leaves) is put back at the next start.

The Hold → Current → Apply steps restart the updater between them: a registry
check is due only hourly or on a channel change (`Checks::due`), and a fresh
updater checks at once — the same thing a box's restart does.
"""

import json
import secrets
import ssl
import subprocess
import threading
import time
import urllib.request
from pathlib import Path

import pytest

from .helpers import (
    admin_ws,
    authed_ws,
    claim_admin_api,
    docker_build,
    fence_cpuset_args,
    find_free_port,
    get_repo_root,
    read_close_code,
    remove_container,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    pytest.mark.self_contained_docker,
    pytest.mark.feature("admin-nest"),
]

UPDATER_IMAGE = "fauna-updater-e2e:local"
UPDATE_LABEL = "social.fauna.update"
#: The bundles' `stop_grace_period`, which the successor must keep.
STOP_TIMEOUT = 15
#: Bounded waits for a state, never a fixed sleep (convention 14). The updater
#: polls the nest every minute; a channel change lands on the next poll.
CHECK_TIMEOUT = 120.0

RUNNING, OLDER, NEWER, DEV_HEAD = "0.5.0", "0.4.0", "0.6.0", "0.7.0"


def _docker(*args, timeout=120, check=True) -> str:
    result = subprocess.run(
        ["docker", *args], capture_output=True, text=True, timeout=timeout
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"docker {' '.join(args)} failed:\n{result.stderr[-2000:]}")
    return result.stdout.strip()


def _inspect(name: str) -> dict | None:
    out = _docker("container", "inspect", name, check=False)
    found = json.loads(out) if out.startswith("[") else []
    return found[0] if found else None


def _build_updater_image(base: str) -> str:
    """Layer the `test-hooks` updater onto the built nest image. Only the
    updater's crate graph compiles; held in a machine-wide build slot where the
    fleet tooling exists, like every compile on a dev box."""
    repo = get_repo_root()
    dockerfile = Path(__file__).with_name("updater-e2e.Dockerfile")
    slot = repo / "scripts" / "build-slot.py"
    prefix = (
        ["uv", "run", "--no-project", "python", str(slot), "--pool", "build", "--"]
        if slot.exists()
        else []
    )
    result = subprocess.run(
        [*prefix, "docker", "buildx", "build", "-f", str(dockerfile),
         "--build-arg", f"BASE_IMAGE={base}", "-t", UPDATER_IMAGE, "--load", str(repo)],
        capture_output=True, text=True, timeout=1800,
    )
    if result.returncode != 0:
        raise RuntimeError(f"updater e2e image build failed:\n{result.stderr[-3000:]}")
    return UPDATER_IMAGE


class Bench:
    """The registry, the nest container and the updater container."""

    def __init__(self, registry, published: dict):
        self.registry = registry
        self.published = published
        self.repository = registry.repository
        suffix = secrets.token_hex(3)
        self.name = f"fauna-upd-nest-{suffix}"
        self.volume = f"fauna-upd-data-{suffix}"
        self.updater = f"fauna-upd-updater-{suffix}"
        self.port = find_free_port()
        self.claim_code = "UPD8TE"
        self.admin: dict | None = None

    # ── the nest ─────────────────────────────────────────────────────────────

    def start_nest(self) -> None:
        digest = self.published[RUNNING].index
        _docker("pull", f"{self.repository}@{digest}", timeout=600)
        _docker("tag", f"{self.repository}@{digest}", f"{self.repository}:test")
        remove_container(self.name)
        _docker(
            "run", "-d", *fence_cpuset_args(),
            "--name", self.name,
            "--label", f"{UPDATE_LABEL}=true",
            "--stop-timeout", str(STOP_TIMEOUT),
            "-p", f"127.0.0.1:{self.port}:3000",
            "-v", f"{self.volume}:/data",
            "-e", "FAUNA_PORT=3000",
            "-e", "FAUNA_MODE=public",
            "-e", f"FAUNA_CLAIM_CODE={self.claim_code}",
            "-e", "FAUNA_IMAGE_TAG=test",
            f"{self.repository}:test",
        )
        wait_for_health(self.port, self.name)
        self.admin = claim_admin_api(self.port, self.claim_code)

    @property
    def nest(self) -> dict:
        return {"url": f"https://127.0.0.1:{self.port}", "port": self.port, "admin": self.admin}

    def health(self) -> dict:
        ctx = ssl._create_unverified_context()
        url = f"https://127.0.0.1:{self.port}/api/v1/health"
        with urllib.request.urlopen(url, timeout=10, context=ctx) as resp:
            return json.loads(resp.read())

    def container(self) -> dict:
        found = _inspect(self.name)
        assert found is not None, f"no container named {self.name}"
        return found

    # ── the updater ──────────────────────────────────────────────────────────

    def start_updater(self) -> None:
        self.stop_updater()
        _docker(
            "run", "-d", *fence_cpuset_args(),
            "--name", self.updater,
            "--network", "host",
            "-v", "/var/run/docker.sock:/var/run/docker.sock",
            "-e", f"FAUNA_E2E_UPDATE_REGISTRY_ORIGIN={self.registry.origin}",
            "--entrypoint", "/usr/local/bin/fauna-updater-e2e",
            UPDATER_IMAGE,
        )

    def stop_updater(self) -> None:
        remove_container(self.updater)

    def updater_log(self) -> str:
        result = subprocess.run(
            ["docker", "logs", self.updater], capture_output=True, text=True, timeout=30
        )
        return result.stdout + result.stderr

    def await_log(self, needle: str, timeout: float = CHECK_TIMEOUT) -> str:
        """Wait for a line containing ``needle`` in the updater's log (state,
        not time: the deadline only bounds a wedged run). Returns the log."""
        deadline = time.monotonic() + timeout
        while True:
            log = self.updater_log()
            if needle in log:
                return log
            if "check: " in log and "re-create" in log:
                pytest.fail(f"the updater's re-create failed:\n{log[-4000:]}")
            if time.monotonic() > deadline:
                pytest.fail(
                    f"the updater never logged {needle!r} within {timeout}s:\n{log[-4000:]}\n"
                    f"registry requests: {self.registry.requests[-20:]}"
                )
            time.sleep(1.0)  # sleep-ok: the poll interval of a deadline-bounded wait for a log line

    def cleanup(self) -> None:
        self.stop_updater()
        remove_container(self.name)
        remove_container(f"{self.name}-fauna-updater-old")
        _docker("volume", "rm", "-f", self.volume, check=False)
        images = _docker("image", "ls", "--format", "{{.Repository}}:{{.Tag}}",
                         self.repository, check=False)
        for ref in images.splitlines():
            _docker("image", "rm", ref, check=False)
        for p in self.published.values():
            _docker("image", "rm", p.config, check=False)


@pytest.fixture(scope="module")
def bench(tmp_path_factory):
    from helpers.oci_registry import OciRegistry, SavedImage

    base = docker_build(get_repo_root())
    _build_updater_image(base)
    saved = SavedImage(base, tmp_path_factory.mktemp("saved-nest-image"))
    registry = OciRegistry().start()
    published = {v: registry.from_image(saved, v) for v in (RUNNING, OLDER, NEWER, DEV_HEAD)}
    registry.tag("test", published[RUNNING].index)
    b = Bench(registry, published)
    try:
        b.start_nest()
        yield b
    finally:
        b.cleanup()
        registry.stop()


def test_current_when_the_channel_head_is_the_running_build(bench):
    before = bench.container()["Id"]
    assert bench.container()["Image"] == bench.published[RUNNING].config
    bench.start_updater()
    try:
        bench.await_log(f"{bench.name}: current on test")
    finally:
        bench.stop_updater()
    assert bench.container()["Id"] == before, "a current box must not be re-created"


def test_hold_when_the_channel_head_is_older_than_the_running_build(bench):
    before = bench.container()["Id"]
    bench.registry.tag("test", bench.published[OLDER].index)
    bench.start_updater()
    try:
        bench.await_log(
            f"{bench.name}: following test from its next build that is at least "
            f"{RUNNING}; its head is {OLDER}"
        )
    finally:
        bench.stop_updater()
    container = bench.container()
    assert container["Id"] == before, "a held box must not be re-created"
    assert container["Image"] == bench.published[RUNNING].config, "never a downgrade"


def test_apply_recreates_the_nest_on_the_newer_build_with_its_configuration(bench):
    old = bench.container()
    admin = bench.admin
    ws = authed_ws(bench.port, bytes(admin["signing_key"].verify_key).hex(), admin["token"])
    close = {}
    reader = threading.Thread(
        target=lambda: close.setdefault("code", read_close_code(ws, timeout=CHECK_TIMEOUT)),
        daemon=True,
    )
    reader.start()
    bench.registry.tag("test", bench.published[NEWER].index)
    bench.start_updater()
    try:
        bench.await_log(f"{bench.name}: now runs {bench.published[NEWER].index}")
        reader.join(timeout=CHECK_TIMEOUT)
    finally:
        bench.stop_updater()
        ws.close()

    assert close.get("code") == 1001, (
        f"the old nest must drain its WS clients with 1001 inside the {STOP_TIMEOUT}s "
        f"grace period; got {close.get('code')!r} (None: a bare drop — the stop "
        "outran the drain)"
    )
    new = bench.container()
    assert new["Id"] != old["Id"], "Apply must re-create the container"
    assert new["Image"] == bench.published[NEWER].config, "the successor runs the newer build"
    assert new["Config"]["Image"] == f"{bench.repository}:test"
    assert new["Config"]["Labels"].get(UPDATE_LABEL) == "true"
    assert new["Config"]["StopTimeout"] == STOP_TIMEOUT
    for env in ("FAUNA_IMAGE_TAG=test", f"FAUNA_CLAIM_CODE={bench.claim_code}", "FAUNA_MODE=public"):
        assert env in new["Config"]["Env"], (env, new["Config"]["Env"])
    mounts = {(m.get("Name"), m["Destination"]) for m in new["Mounts"]}
    assert (bench.volume, "/data") in mounts, mounts
    assert new["HostConfig"]["PortBindings"] == old["HostConfig"]["PortBindings"]
    assert _inspect(f"{bench.name}-fauna-updater-old") is None, "the old container is removed"

    wait_for_health(bench.port, bench.name)
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(bench.nest["url"]) as anon:
        status = anon.call("fauna.setup.status", {})
    assert status.get("claimed") is True, (
        f"the admin's claim must survive the re-create on its volume: {status}"
    )


def test_a_channel_change_from_the_app_is_applied_on_the_next_poll(bench):
    health = bench.health()
    assert "update_channel" in health, (
        f"the image under test predates the update channel (its health is {health}); "
        "this witness needs an image built from a tree carrying it"
    )
    bench.registry.tag("dev", bench.published[DEV_HEAD].index)
    bench.start_updater()
    try:
        bench.await_log(f"{bench.name}: current on test")
        with admin_ws(bench.nest) as client:
            assert client.call("fauna.admin.update_channel.set", {"channel": "dev"})["ok"] is True
        bench.await_log(f"{bench.name}: applying dev {bench.published[DEV_HEAD].index}")
        bench.await_log(f"{bench.name}: now runs {bench.published[DEV_HEAD].index}")
    finally:
        bench.stop_updater()
    assert bench.container()["Image"] == bench.published[DEV_HEAD].config
    wait_for_health(bench.port, bench.name)
    assert bench.health()["update_channel"] == "dev", "the admin's choice survives the re-create"


def test_a_recreate_cut_short_is_put_back_at_the_next_start(bench):
    """The state a crash between the stop and the successor's start leaves —
    the container stepped aside under ``-fauna-updater-old`` and stopped, no
    successor — produced directly rather than by racing a kill against the
    re-create, so the witness does not depend on timing."""
    before = bench.container()
    aside = f"{bench.name}-fauna-updater-old"
    _docker("rename", bench.name, aside)
    _docker("stop", "-t", str(STOP_TIMEOUT), aside, timeout=60)
    assert _inspect(bench.name) is None
    bench.start_updater()
    try:
        bench.await_log(f"{aside}: an unfinished re-create — putting it back")
    finally:
        bench.stop_updater()
    after = bench.container()
    assert after["Id"] == before["Id"], "the very container is put back"
    assert after["State"]["Running"] is True
    assert _inspect(aside) is None
    wait_for_health(bench.port, bench.name)

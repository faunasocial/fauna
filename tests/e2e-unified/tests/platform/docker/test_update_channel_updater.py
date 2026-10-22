"""tier_4 — the Fauna updater against the real nest image and the real engine.

`docs/goal/architecture/nest/update-channel.md` § Tests (the tier_4 bullet) and
§ The updater. Every piece of the updater is unit-tested against fakes (a
wiremock registry, a recording engine, inspect fixtures); this module is where
the real ones meet: the image the tier_4 suite built, a real Docker daemon, and
a registry the daemon really pulls from.

**The bench.** A harness registry on loopback (`helpers/oci_registry.py`) holds
the built image — with the e2e updater layered on, so every build the nest moves
to carries it — re-published under several version labels — the same layers, a
rewritten config, so each label is a distinct build (its own image id) without
building anything. A nest container runs one of them as
``localhost:<port>/faunasocial/nest:test``, labelled ``social.fauna.update=true``
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
  re-created, and the log names both versions. The report (§ What the updater
  reports): the updater writes ``decision.hold`` into the nest container's
  ``/var/lib/fauna-updater`` — root-owned ``0755``, visible inside the running
  container (no tmpfs swallowing the write) — and the bench admin reads it as
  ``updater:decision.hold`` through ``fauna.admin.logs``.
* **A pull failure** — the registry serves a head whose layer blob is gone: the
  box stays on its build, and ``updater:apply.pull_failed`` reaches the ring.
* **Apply** — a newer build appears: the nest is re-created under its own name,
  with its volume, environment and labels, on the new build; the old container
  and its WS clients get the nest's 1001 drain inside the stop grace period;
  the admin's claim survives on the volume; the successor's ring carries
  ``updater:apply.completed``, written into it after its start.
* **A channel change from the app** — ``fauna.admin.update_channel.set`` to Dev
  is picked up on the updater's next minute and Dev's head is applied.
* **Reconcile** — a re-create cut short after the old container was stepped
  aside and stopped (the state a crash leaves) is put back at the next start.
* **The self-update hand-off** — an updater run FROM the managed repository
  (the bundles' shape: the nest's own image and tag, its entrypoint overridden,
  ``restart: unless-stopped``) moves itself to the build it just moved the nest
  to: one updater is left, under its name, on the new image, and the old image
  is gone. Then the same with the hand-off killed between the successor's
  create and its start (the ``test-hooks`` seam
  ``FAUNA_E2E_HANDOFF_EXIT_AFTER_CREATE``): the engine restarts the predecessor,
  which starts the successor, which retires it — the same end state.

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
#: A newer build whose layer blob the registry never serves.
BROKEN = "0.5.5"
#: The builds the two hand-off witnesses move the box to.
HANDED_ON, AFTER_KILL = "0.8.0", "0.9.0"


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


def _image_exists(image_id: str) -> bool:
    return subprocess.run(
        ["docker", "image", "inspect", image_id], capture_output=True, timeout=30
    ).returncode == 0


def _containerd_store() -> bool:
    """Whether the engine keeps images in the containerd snapshotter store (the
    default for a fresh install) rather than a legacy graph driver."""
    status = _docker("info", "--format", "{{json .DriverStatus}}", check=False)
    return "io.containerd.snapshotter" in status


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

    def __init__(self, registry, published: dict, saved=None):
        self.registry = registry
        self.saved = saved
        self.published = published
        self.repository = registry.repository
        suffix = secrets.token_hex(3)
        self.name = f"fauna-upd-nest-{suffix}"
        self.volume = f"fauna-upd-data-{suffix}"
        self.updater = f"fauna-upd-updater-{suffix}"
        self.port = find_free_port()
        self.claim_code = "UPD8TE"
        self.admin: dict | None = None
        self.containerd_store = _containerd_store()

    def image_id(self, version: str) -> str:
        """The engine's image id for a published build. The two image stores
        name an image differently: a graph driver by its config digest, the
        containerd store by the digest it was pulled by — here the index. The
        updater never relies on either (it matches by repository digest); only
        this bench's assertions do. `start_nest` checks the rule against the
        engine, so a store that names images some third way fails there."""
        published = self.published[version]
        return published.index if self.containerd_store else published.config

    # ── the nest ─────────────────────────────────────────────────────────────

    def start_nest(self) -> None:
        digest = self.published[RUNNING].index
        _docker("pull", f"{self.repository}@{digest}", timeout=600)
        _docker("tag", f"{self.repository}@{digest}", f"{self.repository}:test")
        engine_id = _docker("image", "inspect", "--format", "{{.Id}}", f"{self.repository}@{digest}")
        assert engine_id == self.image_id(RUNNING), (
            f"the engine names the pulled build {engine_id}, not the "
            f"{'index' if self.containerd_store else 'config'} digest "
            f"{self.image_id(RUNNING)} `image_id` expects for this image store "
            f"(containerd store: {self.containerd_store})"
        )
        remove_container(self.name)
        _docker(
            "run", "-d", *fence_cpuset_args(),
            "--name", self.name,
            "--label", f"{UPDATE_LABEL}=true",
            "--stop-timeout", str(STOP_TIMEOUT),
            "-p", f"127.0.0.1:{self.port}:443",
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
        """The nest's health body. A read is retried while the connection is
        reset: after a re-create the SNI router restarts once, when it meets
        the sidecar token the new nest minted (`docker/s6/fauna-sni-router/run`)."""
        from helpers.waiting import wait_until

        ctx = ssl._create_unverified_context()
        url = f"https://127.0.0.1:{self.port}/api/v1/health"
        last: dict = {}

        def _read():
            try:
                with urllib.request.urlopen(url, timeout=10, context=ctx) as resp:
                    return json.loads(resp.read())
            except OSError as e:
                last["error"] = repr(e)
                return None

        return wait_until(_read, 60, interval=1,
                          diagnose=lambda: f"health never readable: {last.get('error')}")

    def container(self) -> dict:
        found = _inspect(self.name)
        assert found is not None, f"no container named {self.name}"
        return found

    # ── the updater ──────────────────────────────────────────────────────────

    def start_updater(self, managed: bool = False, env: dict | None = None) -> str:
        """Run the updater; returns its container id. ``managed``: from the
        managed repository at the nest's own reference, restarted by the engine
        like the bundles' — the shape that hands itself off; otherwise from
        ``UPDATER_IMAGE``, which the hand-off leaves alone."""
        self.stop_updater()
        extra = [arg for k, v in (env or {}).items() for arg in ("-e", f"{k}={v}")]
        return _docker(
            "run", "-d", *fence_cpuset_args(),
            "--name", self.updater,
            "--network", "host",
            *(["--restart", "unless-stopped"] if managed else []),
            "-v", "/var/run/docker.sock:/var/run/docker.sock",
            *(arg for k, v in self.registry.env.items() for arg in ("-e", f"{k}={v}")),
            *extra,
            "--entrypoint", "/usr/local/bin/fauna-updater-e2e",
            f"{self.repository}:test" if managed else UPDATER_IMAGE,
        )

    def stop_updater(self) -> None:
        remove_container(self.updater)
        remove_container(f"{self.updater}-fauna-updater-old")

    def point_heads_at(self, version: str) -> None:
        """Make ``version`` the head of both channels the box may follow (the
        channel-change witness leaves it on Dev, or on Test if that one could
        not run)."""
        for channel in ("test", "dev"):
            self.registry.tag(channel, self.published[version].index)

    def updater_names(self) -> list[str]:
        """Every container this bench's updater left — itself, a successor, a
        predecessor stepped aside."""
        names = _docker("ps", "-a", "--format", "{{.Names}}").splitlines()
        return sorted(n for n in names if n.startswith(self.updater))

    def await_one_updater(self, image: str, old_image: str) -> dict:
        """Wait for the hand-off's end state — one updater, under its name,
        running ``image``, and ``old_image`` removed — and return it (state,
        not time: the deadline only bounds a wedged run)."""
        deadline = time.monotonic() + CHECK_TIMEOUT
        while True:
            names = self.updater_names()
            found = _inspect(self.updater) if names == [self.updater] else None
            if (
                found is not None
                and found["State"]["Running"]
                and found["Image"] == image
                and not _image_exists(old_image)
            ):
                return found
            if time.monotonic() > deadline:
                logs = {
                    n: subprocess.run(["docker", "logs", n], capture_output=True,
                                      text=True, timeout=30).stderr[-3000:]
                    for n in names
                }
                pytest.fail(
                    f"no single updater on {image} with {old_image} gone within "
                    f"{CHECK_TIMEOUT}s: containers {names}, old image present "
                    f"{_image_exists(old_image)}; logs:\n{logs}"
                )
            time.sleep(1.0)  # sleep-ok: the poll interval of a deadline-bounded wait for a state

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

    def report(self) -> dict | None:
        """The report file as the running nest container holds it."""
        out = _docker("exec", self.name, "cat", "/var/lib/fauna-updater/report.json",
                      check=False)
        return json.loads(out) if out.startswith("{") else None

    def updater_entries(self) -> list[dict]:
        with admin_ws(self.nest) as client:
            entries = client.call("fauna.admin.logs", {})["entries"]
        return [e for e in entries if e["target"].startswith("updater:")]

    def await_entry(self, target: str, timeout: float = CHECK_TIMEOUT) -> dict:
        """Wait for ``target`` in the bench admin's ``fauna.admin.logs`` (the
        nest reads the report every ten seconds; the deadline only bounds a
        wedged run). Returns the entry."""
        deadline = time.monotonic() + timeout
        while True:
            try:
                entries = self.updater_entries()
            except Exception as e:  # a nest mid-restart: the next poll reads it
                entries = [{"target": f"(read failed: {e})"}]
            found = [e for e in entries if e.get("target") == target]
            if found:
                return found[-1]
            if time.monotonic() > deadline:
                pytest.fail(
                    f"{target} never reached fauna.admin.logs within {timeout}s: "
                    f"updater entries {entries}; the container's report "
                    f"{self.report()}; updater log:\n{self.updater_log()[-3000:]}"
                )
            time.sleep(1.0)  # sleep-ok: the poll interval of a deadline-bounded wait for a ring entry

    def cleanup(self) -> None:
        self.stop_updater()
        remove_container(self.name)
        remove_container(f"{self.name}-fauna-updater-old")
        _docker("volume", "rm", "-f", self.volume, check=False)
        images = _docker("image", "ls", "--format", "{{.Repository}}:{{.Tag}}",
                         self.repository, check=False)
        for ref in images.splitlines():
            _docker("image", "rm", ref, check=False)
        for version in self.published:
            _docker("image", "rm", self.image_id(version), check=False)


@pytest.fixture(scope="module")
def bench(tmp_path_factory):
    from helpers.oci_registry import OciRegistry, SavedImage

    base = docker_build(get_repo_root())
    # The builds are published from the updater image — the nest image plus the
    # e2e updater — so a hand-off finds the updater binary in the build it
    # moves to; the nest runs from them unchanged.
    saved = SavedImage(_build_updater_image(base), tmp_path_factory.mktemp("saved-nest-image"))
    registry = OciRegistry().start()
    versions = (RUNNING, OLDER, NEWER, DEV_HEAD, HANDED_ON, AFTER_KILL)
    published = {v: registry.from_image(saved, v) for v in versions}
    registry.tag("test", published[RUNNING].index)
    b = Bench(registry, published, saved)
    try:
        b.start_nest()
        yield b
    finally:
        b.cleanup()
        registry.stop()


def test_current_when_the_channel_head_is_the_running_build(bench):
    before = bench.container()["Id"]
    assert bench.container()["Image"] == bench.image_id(RUNNING)
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
    assert container["Image"] == bench.image_id(RUNNING), "never a downgrade"

    # The report reaches the ring through the nest's read ...
    entry = bench.await_entry("updater:decision.hold")
    assert entry["level"] == "Warn", entry
    assert RUNNING in entry["message"] and OLDER in entry["message"], entry
    # ... from the file written into the container's own layer, visible inside
    # it, in the root-owned directory the image creates.
    mode = _docker("exec", bench.name, "stat", "-c", "%U %G %a", "/var/lib/fauna-updater")
    assert mode == "root root 755", mode
    report = bench.report()
    assert report is not None and report["schema"] == 1, report
    assert any(e["event"] == "decision.hold" for e in report["events"]), report


def test_a_pull_of_a_head_whose_blob_is_gone_is_reported_and_moves_nothing(bench):
    before = bench.container()["Id"]
    broken = bench.registry.with_missing_layer(bench.saved, BROKEN)
    bench.registry.tag("test", broken.index)
    bench.start_updater()
    try:
        entry = bench.await_entry("updater:apply.pull_failed")
    finally:
        bench.stop_updater()
    assert entry["level"] == "Error", entry
    container = bench.container()
    assert container["Id"] == before, "a failed pull must leave the box as it was"
    assert container["Image"] == bench.image_id(RUNNING)


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
    assert new["Image"] == bench.image_id(NEWER), "the successor runs the newer build"
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
    entry = bench.await_entry("updater:apply.completed")
    assert bench.published[NEWER].index.removeprefix("sha256:")[:12] in entry["message"], entry


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
    assert bench.container()["Image"] == bench.image_id(DEV_HEAD)
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


def test_the_updater_hands_itself_off_to_the_build_it_moved_the_nest_to(bench):
    """After an Apply, the updater run from the managed repository moves itself
    to the nest's new build: one updater under its name on the new image id, no
    predecessor left aside, and the old image removed once nothing runs it."""
    old_image = bench.container()["Image"]
    target = bench.image_id(HANDED_ON)
    bench.point_heads_at(HANDED_ON)
    first = bench.start_updater(managed=True)
    try:
        updater = bench.await_one_updater(target, old_image)
    finally:
        bench.stop_updater()
    assert bench.container()["Image"] == target, "the nest moved first"
    assert updater["Id"] != first, "a successor, not the first updater, is left"
    assert updater["Config"]["Image"] == f"{bench.repository}:test"
    assert updater["Config"]["Entrypoint"] == ["/usr/local/bin/fauna-updater-e2e"]
    assert updater["HostConfig"]["RestartPolicy"]["Name"] == "unless-stopped"
    assert _inspect(f"{bench.updater}-fauna-updater-old") is None


def test_a_hand_off_killed_between_create_and_start_settles_to_one_updater(bench):
    """The updater exits right after creating its successor (the e2e seam, as a
    crash would); the engine restarts it aside, it starts the successor it finds
    created and never started, and the successor retires it — the same
    one-updater end state as an uninterrupted hand-off."""
    old_image = bench.container()["Image"]
    target = bench.image_id(AFTER_KILL)
    since = str(int(time.time()) - 1)
    bench.point_heads_at(AFTER_KILL)
    first = bench.start_updater(managed=True, env={"FAUNA_E2E_HANDOFF_EXIT_AFTER_CREATE": "1"})
    try:
        updater = bench.await_one_updater(target, old_image)
    finally:
        bench.stop_updater()
    assert bench.container()["Image"] == target, "the nest moved first"
    assert updater["Id"] != first
    # The kill really happened and the engine restarted the predecessor: its own
    # event stream shows the seam's exit, then a start.
    actions = _docker(
        "events", "--since", since, "--until", str(int(time.time()) + 1),
        "--filter", f"container={first}",
        "--format", '{{.Action}} {{index .Actor.Attributes "exitCode"}}',
    ).splitlines()
    actions = [a.strip() for a in actions]
    assert "die 1" in actions, f"the seam's exit is missing: {actions}"
    after_kill = [a.split()[0] for a in actions[actions.index("die 1") + 1:]]
    assert "start" in after_kill, f"the engine never restarted the predecessor: {actions}"
    assert _inspect(f"{bench.updater}-fauna-updater-old") is None

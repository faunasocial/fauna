"""The update channel as nest state — ``nest/update-channel.md`` § The channel as
nest state, on a real ``fauna-nest`` binary.

* **The seed.** A nest booted with ``FAUNA_IMAGE_TAG=test`` follows Test while no
  admin has chosen: ``/api/v1/health`` reports ``update_channel: "test"`` and
  ``fauna.admin.update_channel.get`` answers ``channel: "test"``. A tag that is
  no channel (``sha-…``) seeds none — the box is *pinned*: ``null`` on health,
  no ``channel`` on the read. No variable at all is a nest the image does not
  run (the Windows service, the macOS daemon, the Linux package): no channel
  applies, ``enactable: false`` on the read, ``set`` refused, no registry watch.
* **The admin's choice wins and survives.** ``set { dev }`` is reflected on the
  read and on health at once, and again after a restart that keeps the
  ``test`` seed in the environment — the row, not the seed, is in force.
* **An older nest.** A kind the router never registered answers
  ``fauna.protocol.unknown_kind`` and the connection stays usable — the answer
  an app reads as "this nest predates the choice", never a failed read.
* A non-admin can neither read nor set it, and an unknown channel id is refused.

``channel_head_version``: an e2e nest resolves it only against the harness
registry ``FAUNA_E2E_UPDATE_REGISTRY_ORIGIN`` names (`helpers/oci_registry.py`).
The two nests above name none, so their read leaves it absent — never reaching
the real registry; ``updater_head_nest`` names one whose ``test`` tag carries version
``9.9.9``, and its read answers that, then answers without it once the registry
has no head or is gone — the read never fails on the head.
"""

import json
import time
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

pytestmark = pytest.mark.tier_3


def _start(request, nest_mode, tmp_path_factory, label: str, tag: str):
    from conftest import _start_dedicated_nest

    return _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, label,
        extra_env={"FAUNA_IMAGE_TAG": tag},
    )


@pytest.fixture(scope="module")
def test_channel_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest installed from the ``test`` tag, which these tests
    restart — never the session nest."""
    nest, cleanup = _start(request, nest_mode, tmp_path_factory,
                           "update-channel-test", "test")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def pinned_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest installed from a commit tag — no channel."""
    nest, cleanup = _start(request, nest_mode, tmp_path_factory,
                           "update-channel-pinned", "sha-abc1234")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def head_registry():
    """The harness registry: its ``test`` tag at version ``9.9.9``, no ``dev``
    tag at all."""
    from helpers.oci_registry import OciRegistry

    with OciRegistry() as registry:
        registry.synthetic("test", "9.9.9")
        yield registry


@pytest.fixture(scope="module")
def updater_head_nest(request, nest_mode, tmp_path_factory, head_registry):
    """A dedicated nest on the ``test`` seed that reads its channel's head from
    the harness registry."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "update-channel-head",
        extra_env={
            "FAUNA_IMAGE_TAG": "test",
            **head_registry.env,
        },
    )
    try:
        yield nest
    finally:
        cleanup()


def _client(nest, key=None) -> WsRpcAdminClient:
    key = key or nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(key.verify_key), signing_key=bytes(key)
    )


def _health(nest) -> dict:
    with urllib.request.urlopen(nest["url"].rstrip("/") + "/api/v1/health") as resp:
        return json.loads(resp.read())


def _get(nest) -> dict:
    with _client(nest) as admin:
        return admin.call("fauna.admin.update_channel.get", {})


def _set(nest, channel: str) -> dict:
    with _client(nest) as admin:
        return admin.call("fauna.admin.update_channel.set", {"channel": channel})


@pytest.mark.feature("admin-nest")
def test_the_seed_is_followed_until_the_admin_chooses_and_the_choice_survives_a_restart(
    test_channel_nest,
):
    from common.nest import restart_nest

    nest = test_channel_nest
    health = _health(nest)
    assert health["update_channel"] == "test", health
    reply = _get(nest)
    assert reply["channel"] == "test", reply
    assert reply["running_version"] == health["version"], (reply, health)
    assert reply.get("channel_head_version") is None, reply

    assert _set(nest, "dev")["ok"] is True
    assert _get(nest)["channel"] == "dev"
    assert _health(nest)["update_channel"] == "dev", "health must follow the row at once"

    restart_nest(nest, graceful=True)
    assert _health(nest)["update_channel"] == "dev", (
        "the admin's row must win over the FAUNA_IMAGE_TAG=test seed after a restart"
    )
    assert _get(nest)["channel"] == "dev"


@pytest.mark.feature("admin-nest")
def test_a_non_channel_tag_leaves_the_box_pinned(pinned_nest):
    health = _health(pinned_nest)
    assert "update_channel" in health and health["update_channel"] is None, health
    reply = _get(pinned_nest)
    assert reply.get("channel") is None, reply
    assert reply["running_version"] == health["version"]


@pytest.mark.feature("admin-nest")
def test_an_unknown_kind_is_the_predates_the_choice_answer(pinned_nest):
    with _client(pinned_nest) as admin:
        with pytest.raises(RpcCallError) as err:
            admin.call("fauna.admin.update_channel.not_yet_minted", {})
        assert err.value.code == "fauna.protocol.unknown_kind", err.value
        # The connection survives the miss: the read an app retries works.
        assert "running_version" in admin.call("fauna.admin.update_channel.get", {})


@pytest.mark.feature("admin-nest")
def test_a_non_admin_cannot_read_or_set_it_and_an_unknown_channel_is_refused(pinned_nest):
    from common.auth import create_actor_and_register

    member = create_actor_and_register(
        pinned_nest["port"], admin_signing_key=pinned_nest["admin"]["signing_key"]
    )
    with _client(pinned_nest, member["signing_key"]) as user:
        for kind, params in (
            ("fauna.admin.update_channel.get", {}),
            ("fauna.admin.update_channel.set", {"channel": "dev"}),
        ):
            with pytest.raises(RpcCallError):
                user.call(kind, params)
    with _client(pinned_nest) as admin:
        with pytest.raises(RpcCallError):
            admin.call("fauna.admin.update_channel.set", {"channel": "latest"})
    assert _health(pinned_nest)["update_channel"] is None, (
        "a refused set must not have moved the box off its pin"
    )


@pytest.mark.feature("admin-nest")
@pytest.mark.standalone_only
def test_the_channel_head_version_is_read_from_the_registry_and_absent_without_it(
    updater_head_nest, head_registry,
):
    """``standalone_only``: the registry is a loopback listener this process
    owns, named to the nest through a ``test-hooks`` seam — only a locally
    spawned test-hooks nest reads it."""
    reply = _get(updater_head_nest)
    assert reply["channel"] == "test", reply
    assert reply.get("channel_head_version") == "9.9.9", (
        f"the read must carry the Test head's version label: {reply}; "
        f"the registry saw {head_registry.requests}"
    )
    assert "HEAD /v2/faunasocial/nest/manifests/test" in head_registry.requests, (
        head_registry.requests
    )

    # A channel with no head in the registry: absent, and the read answers.
    assert _set(updater_head_nest, "dev")["ok"] is True
    reply = _get(updater_head_nest)
    assert reply["channel"] == "dev", reply
    assert reply.get("channel_head_version") is None, reply

    # The registry gone: back on Test (a channel the five-minute cache no
    # longer holds), the head cannot be had — absent, and the read succeeds.
    head_registry.stop()
    assert _set(updater_head_nest, "test")["ok"] is True
    reply = _get(updater_head_nest)
    assert reply["channel"] == "test", reply
    assert reply["running_version"] == _health(updater_head_nest)["version"], reply
    assert reply.get("channel_head_version") is None, (
        f"an unreachable registry must leave the head absent: {reply}"
    )


# ── The updater's report ────────────────────────────────────────────────────


def _planted_report(path, events: list[tuple[int, str, str, str]], dropped: int = 0) -> None:
    """Write a report as the updater would: ``(seq, level, event, message)``."""
    now = int(time.time() * 1000)
    body = {
        "schema": 1,
        "dropped": dropped,
        "events": [
            {"seq": seq, "timestamp_ms": now, "level": level, "event": event, "message": message}
            for seq, level, event, message in events
        ],
    }
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(body))
    tmp.replace(path)


@pytest.fixture(scope="module")
def report_file(tmp_path_factory):
    path = tmp_path_factory.mktemp("updater-report") / "report.json"
    _planted_report(path, [
        (1, "warn", "decision.hold", "holding on test: the box runs 9.9.9"),
        (2, "error", "apply.pull_failed", "the pull of the head failed (not found)"),
    ])
    return path


@pytest.fixture(scope="module")
def updater_report_nest(request, nest_mode, tmp_path_factory, report_file):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "update-channel-report",
        extra_env={"FAUNA_IMAGE_TAG": "test", "FAUNA_E2E_UPDATER_REPORT_PATH": str(report_file)},
    )
    try:
        yield nest
    finally:
        cleanup()


def _logs(nest) -> list[dict]:
    with _client(nest) as admin:
        return admin.call("fauna.admin.logs", {})["entries"]


def _updater_targets(nest) -> list[str]:
    return [e["target"] for e in _logs(nest) if e["target"].startswith("updater:")]


def _until_targets(nest, wanted: set[str]) -> list[str]:
    from helpers.waiting import wait_until

    return wait_until(
        lambda: (t := _updater_targets(nest)) and wanted <= set(t) and t,
        60,
        diagnose=lambda: f"updater entries: {_updater_targets(nest)}",
    )


@pytest.mark.feature("admin-logs")
@pytest.mark.standalone_only
def test_a_planted_report_lands_as_updater_entries_and_again_after_a_restart(
    updater_report_nest, report_file,
):
    """``standalone_only``: the report path is a file this process writes,
    named to a locally spawned test-hooks nest through a seam."""
    from common.nest import restart_nest

    targets = _until_targets(updater_report_nest, {"updater:decision.hold", "updater:apply.pull_failed"})
    assert targets.count("updater:decision.hold") == 1, targets

    # A new event above the mark lands; the old ones are not landed twice.
    _planted_report(report_file, [
        (1, "warn", "decision.hold", "holding on test: the box runs 9.9.9"),
        (2, "error", "apply.pull_failed", "the pull of the head failed (not found)"),
        (3, "info", "check.registry_recovered", "the registry check succeeds again"),
    ])
    targets = _until_targets(updater_report_nest, {"updater:check.registry_recovered"})
    assert targets.count("updater:decision.hold") == 1, targets

    # A torn file is ignored, never a ring entry, and the next whole one reads.
    report_file.write_text('{"schema": 1, "dropped": 0, "events": [{"seq": 4')
    restart_nest(updater_report_nest, graceful=True)
    assert not _updater_targets(updater_report_nest), "a torn report must land nothing"
    _planted_report(report_file, [
        (1, "warn", "decision.hold", "holding on test: the box runs 9.9.9"),
        (3, "info", "check.registry_recovered", "the registry check succeeds again"),
    ])
    targets = _until_targets(updater_report_nest, {"updater:decision.hold", "updater:check.registry_recovered"})
    entry = next(e for e in _logs(updater_report_nest) if e["target"] == "updater:decision.hold")
    assert entry["level"] == "Warn", entry


@pytest.fixture(scope="module")
def overdue_registry():
    """A Dev head newer than any nest under test, of a commit no local build
    has."""
    from helpers.oci_registry import OciRegistry

    with OciRegistry() as registry:
        registry.synthetic("dev", "9.9.9", revision="0000000")
        yield registry


@pytest.fixture(scope="module")
def overdue_nest(request, nest_mode, tmp_path_factory, overdue_registry):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "update-channel-overdue",
        extra_env={
            "FAUNA_IMAGE_TAG": "dev",
            **overdue_registry.env,
            "FAUNA_E2E_UPDATE_OVERDUE_ALLOWANCE_S": "0",
        },
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def packaged_nest(request, nest_mode, tmp_path_factory, overdue_registry):
    """A nest the image does not run — no ``FAUNA_IMAGE_TAG`` in its
    environment, as the Windows service, the macOS daemon and the Linux package
    start it — pointed at the same harness registry and the zero allowance, so
    a watch that ran would show in the registry's request log within seconds."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "update-channel-packaged",
        extra_env={
            **overdue_registry.env,
            "FAUNA_E2E_UPDATE_OVERDUE_ALLOWANCE_S": "0",
        },
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.standalone_only
def test_a_nest_the_image_does_not_run_follows_no_channel_and_asks_the_registry_nothing(
    packaged_nest, overdue_registry,
):
    """``nest/update-channel.md`` § The channel as nest state → *Where the
    channel applies*: with no image tag in its environment the nest reports
    ``update_channel: null`` on health and ``enactable: false`` on the read,
    refuses ``set`` (a choice nothing could enact), and runs no registry watch —
    the harness registry sees no request from it, where the image-tagged nest of
    the test above asks within a second of boot. ``standalone_only`` as above
    (the Docker nest mode sets the tag itself)."""
    import time

    assert _health(packaged_nest)["update_channel"] is None
    reply = _get(packaged_nest)
    assert reply["enactable"] is False, reply
    assert "channel" not in reply, reply
    with pytest.raises(RpcCallError) as refused:
        _set(packaged_nest, "stable")
    assert "container image" in str(refused.value), refused.value
    assert _health(packaged_nest)["update_channel"] is None
    assert _get(packaged_nest)["enactable"] is False
    # The watch ticks every second under the zero allowance, and the admin read
    # above would have resolved a head had a channel been in force: a few
    # seconds of nothing is the pin.
    seen_before = len(overdue_registry.requests)
    time.sleep(5)  # sleep-ok: an absence window — a spawned watch would tick every second under the zero allowance, so five quiet seconds is the pin; nothing happens to anchor on
    assert len(overdue_registry.requests) == seen_before, overdue_registry.requests
    assert not any(
        e["target"] == "update_channel" and "update overdue" in e["message"]
        for e in _logs(packaged_nest)
    )


@pytest.mark.standalone_only
def test_an_unapplied_newer_head_is_said_overdue_once(overdue_nest, overdue_registry):
    """The Dev interval (a minute) plus a zero allowance passes with no updater
    to apply the head: the nest's own ring carries one ``update overdue``
    warning naming the head's version. ``standalone_only`` as above."""
    from helpers.waiting import wait_until

    def overdue():
        return [e for e in _logs(overdue_nest)
                if e["target"] == "update_channel" and "update overdue" in e["message"]]

    found = wait_until(
        overdue, 180,
        diagnose=lambda: f"registry saw {overdue_registry.requests}; "
                         f"get: {_get(overdue_nest)}",
    )
    assert found[0]["level"] == "Warn", found
    assert "9.9.9" in found[0]["message"], found
    assert not any(e["target"].startswith("updater:") for e in _logs(overdue_nest)), (
        "a nest with no updater report must carry no updater: entries"
    )
    # Once per head: the ticks after the warning add none.
    assert len(overdue()) == 1, overdue()

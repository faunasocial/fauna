"""tier_3: iOS File Provider eviction — does the OS let a materialized body go?

The iOS leg of the keep (`docs/goal/behavior/on-demand-files-apple.md` § Apple
File Provider binding, *An own record the nest took no bytes of is kept
downloaded*): in a metadata-only folder the device that wrote a file holds its
only body, and once the extension acks the record the OS is free to evict it.
On macOS a kept item declares the keep-downloaded content policy; iOS has no
such policy, so a file the gate does not keep advertises `allowsEvicting` and
a kept one leaves it out, which the SDK documents as never evicted. Driven on
a launched extension, headlessly, this test pins that a body that is not kept
goes (iOS 27 refuses any item without the capability):

  1. Seal one file into an owner-only folder on a nest this test may stop.
  2. Build the Debug **simulator** `Fauna-iOS` app (ad-hoc signed; its
     simulator entitlements carry the File Provider testing mode), install it
     on a throwaway device, `provision` the extension's capability with an
     enrolled machine principal (`harness_machine_principal` — the host keys
     the account plane's folder custody as the device it is a process of, and
     refuses to open the set without one), and `register <ref> <name>
     always-enabled` — without the testing mode a fresh device registers the
     domain disabled and nothing serves (FP -2011).
  3. `read` hydrates the file through `fetchContents` (the app is the only
     in-simulator reader: `simctl spawn` runs no `cat`).
  4. `evict` (`NSFileProviderManager.evictItem`, the programmatic *Remove
     Download*) is ACCEPTED, and the replica goes `SF_DATALESS` — read from
     the host, without entering the provider.
  5. The body re-opens from the nest; evicted again with the nest stopped, it
     does not: a body fetched from another holder stays evictable and is not
     kept.

The second test drives the own-record leg: a file written through the domain
into a metadata-only folder (the nest takes no bytes of it) and recorded is
kept, so it advertises no `allowsEvicting` and is REFUSED eviction
(`NSFileProviderErrorDomain` -2008, *non-evictable*), its body stays, and it
reads back with the nest stopped. Should the OS ever let one go, the staging
root still holds the recorded body and `fetchContents` reads it before the
network (*Read before the network*).

The third test pins the read path's silence (*a fetched body is hydrated, never
re-recorded*): with a live device grant, so that a spurious record would land
and count, ten first hydrates and ten re-fetches after eviction add no record
and no upload, and neither does removing the domain. Its positive control is
a write through the same domain, which does record.

Mutation carve-out (testing conventions point 8): the folder create, device
register and seal+upload are fixture setup; the action under test is the OS's
eviction, driven through `NSFileProviderManager` the way *Remove Download* is.
"""

import hashlib
import json
import os
import secrets
import subprocess
import sys
from pathlib import Path

import blake3
import cbor2
import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import _user_call, create_actor_and_register, set_folder_residency
from helpers.waiting import wait_until
from tests.platform.macos.test_file_provider_read_path import (
    _BACKUP_KEY_CONTEXT,
    _CONVERGENT_CHUNK_CONTEXT,
    _post_bytes,
)

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="the iOS simulator runs on macOS only"),
    pytest.mark.tier_3,
]

_APPLE_DIR = Path(__file__).resolve().parents[5] / "apps" / "fauna-apple"
_SIM_SLICE = _APPLE_DIR / "FaunaFFI.xcframework" / "ios-arm64-simulator"
_BUNDLE_ID = "social.fauna.fauna"
_SF_DATALESS = 0x40000000
_PROOF = "proof.txt"
_PROOF_CONTENT = b"fauna ios file provider eviction proof \xe2\x9c\x93\n"
_READS = 10


def _sim(*args: str, timeout: float = 120) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["xcrun", "simctl", *args], capture_output=True, text=True, timeout=timeout
    )


def _fauna(udid: str, *args: str) -> str:
    """Run one DEBUG `FileProviderTestCLI` verb in the installed app; its output."""
    proc = _sim(
        "launch", "--console-pty", "--terminate-running-process", udid, _BUNDLE_ID, *args,
        timeout=180,
    )
    out = (proc.stdout + proc.stderr).strip()
    print(f"$ Fauna {' '.join(args[:1])} … -> {out}")
    return out


@pytest.fixture(scope="session")
def fauna_ios_fp_app():
    """The Debug simulator `Fauna-iOS` app, built at collection time
    (`conftest._ensure_ios_fp_app_built`; here a replay of that build). Requires
    `just apple-ffi-test-sim` (or `apple-ffi-test`) to have built the xcframework's
    simulator slice; the test never triggers that build itself."""
    from conftest import _ensure_ios_fp_app_built

    if not _SIM_SLICE.exists():
        pytest.skip(
            f"{_SIM_SLICE} missing — run `just apple-ffi-test-sim` before this tier_3 test."
        )
    app = _ensure_ios_fp_app_built()
    assert app, "no Fauna-iOS app: this box has no xcodebuild"
    return Path(app)


@pytest.fixture
def fp_device(fauna_ios_fp_app):
    """A throwaway booted simulator with the app installed; deleted after.

    Registered with the run's lane note (`register_simulator`) so the fleet
    agent charges the device's boot — 11 to 23 cores for a minute — to this
    run rather than to the desktop, as it does for the conftest's own devices."""
    from conftest import register_simulator

    created = _sim("create", f"fauna-fp-evict-{os.getpid()}", "iPhone 17 Pro")
    assert created.returncode == 0, f"simctl create failed: {created.stderr}"
    udid = created.stdout.strip()
    register_simulator(udid)
    print(f"[fp_device] {udid}: {_device_runtime(udid)}")
    try:
        _sim("boot", udid)
        booted = _sim("bootstatus", udid, "-b", timeout=600)
        assert booted.returncode == 0, f"simulator never booted: {booted.stderr}"
        _sim(
            "spawn", udid, "log", "config", "--subsystem", "social.fauna.fileprovider",
            "--mode", "level:debug,persist:debug",
        )
        installed = _sim("install", udid, str(fauna_ios_fp_app), timeout=300)
        assert installed.returncode == 0, f"simctl install failed: {installed.stderr}"
        yield udid
    finally:
        _sim("shutdown", udid)
        _sim("delete", udid)


def _device_runtime(udid: str) -> str:
    """The runtime `simctl` gave a device, by its identifier."""
    listed = json.loads(_sim("list", "devices", "--json").stdout)["devices"]
    return next(
        (runtime for runtime, devices in listed.items() if any(d["udid"] == udid for d in devices)),
        "unknown",
    )


def _replica(udid: str, rel: str) -> Path | None:
    """The file as the OS keeps it in the domain's replica on the device."""
    cloud = Path.home() / "Library/Developer/CoreSimulator/Devices" / udid / "data/Library/CloudStorage"
    return next(iter(cloud.glob(f"*/{rel}")), None) if cloud.exists() else None


def _dataless(path: Path) -> bool:
    return bool(os.lstat(path).st_flags & _SF_DATALESS)


def _extension_diagnostics(udid: str) -> str:
    """The extension's own account of itself, for a failure message (convention
    6): its os log lines, and the tail of the engine's tracing file in the app
    group (`FileProvider/logs`, where the extension routes `tracing`)."""
    shown = _sim(
        "spawn", udid, "log", "show", "--info", "--debug", "--last", "5m", "--predicate",
        'subsystem == "social.fauna.fileprovider"',
    )
    lines = shown.stdout.strip().splitlines()[-30:]
    group = _sim("get_app_container", udid, _BUNDLE_ID, "groups").stdout
    for entry in group.splitlines():
        logs = Path(entry.split(None, 1)[-1].strip()) / "FileProvider" / "logs"
        for log in sorted(logs.glob("*"), key=lambda p: p.stat().st_mtime)[-1:]:
            lines += [f"--- {log.name}"] + log.read_text(errors="replace").splitlines()[-30:]
    return "\n".join(lines)


def _extension_tracing(udid: str) -> str:
    """The whole of the engine's tracing files in the app group."""
    group = _sim("get_app_container", udid, _BUNDLE_ID, "groups").stdout
    texts = []
    for entry in group.splitlines():
        logs = Path(entry.split(None, 1)[-1].strip()) / "FileProvider" / "logs"
        texts += [log.read_text(errors="replace") for log in sorted(logs.glob("*"))]
    return "\n".join(texts)


def _keep_logs(udid: str, into: Path) -> None:
    """Keep what the OS and the extension said, for reading after the device goes."""
    shown = _sim(
        "spawn", udid, "log", "show", "--info", "--debug", "--last", "30m", "--predicate",
        'process == "fileproviderd" OR subsystem == "social.fauna.fileprovider"',
        timeout=300,
    )
    (into / "fileproviderd.log").write_text(shown.stdout)
    (into / "extension-tracing.log").write_text(_extension_tracing(udid))
    print(f"[logs kept] {into}")


def _register(udid: str, folder_ref: str, set_name: str) -> None:
    """Add the set's domain. A fresh device has answered FP -2014
    (`applicationExtensionNotFound`, under -2001) in some runs and not in
    others, the app the same, so that one answer is waited out; any other is
    the verb's result."""
    answers = []

    def answered() -> bool:
        answers.append(_fauna(udid, "register", folder_ref, set_name, "always-enabled"))
        return "Code=-2014" not in answers[-1]

    wait_until(
        answered, 120, interval=2.0,
        diagnose=lambda: f"{answers[-1]}\n{_extension_diagnostics(udid)}",
    )
    assert "register OK" in answers[-1], (
        f"the domain did not register: {answers[-1]}\n{_extension_diagnostics(udid)}"
    )


def _read_ok(out: str) -> bool:
    return f"read OK: {_PROOF}" in out and hashlib.sha256(_PROOF_CONTENT).hexdigest() in out


@pytest.mark.timeout(3600)
@pytest.mark.feature("files-on-demand")
def test_file_provider_eviction_ios(stoppable_nest, fp_device):
    nest, udid = stoppable_nest, fp_device
    url, port = nest["url"], nest["port"]

    owner = create_actor_and_register(port, admin_signing_key=nest["admin"]["signing_key"])
    owner_seed = bytes(owner["signing_key"])
    backup_key = blake3.blake3(owner_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()
    content_key = blake3.blake3(backup_key, derive_key_context=_CONVERGENT_CHUNK_CONTEXT).digest()
    set_name = "fp-evict-" + secrets.token_hex(4)
    device_id = secrets.token_bytes(32)

    # The extension keys the account plane's folder custody as the enrolled
    # machine it is a process of — without one it refuses to open the set.
    # Enrolled before the set is born, so the set's custody is wrapped to it.
    writer_secret, device_authorization = fauna_ffi.harness_machine_principal(url, owner_seed)

    with WsRpcAdminClient(url, actor_id=owner["actor_id_bytes"], signing_key=owner_seed) as ws:
        created = fauna_ffi.harness_create_set(url, owner_seed, {"name": set_name})
        folder_ref = f"local:{created['id']}"
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
    manifest_bytes, chunks = fauna_ffi.seal_folder_file(_PROOF_CONTENT, content_key)
    for store_key, body in chunks:
        assert _post_bytes(port, "/api/v1/chunks", owner["token"], body) == store_key.hex()
    manifest_hash = _post_bytes(port, "/api/v1/manifests", owner["token"], manifest_bytes)
    fauna_ffi.harness_record_change(
        url,
        owner_seed,
        {
            "folder": set_name,
            "device_id": device_id.hex(),
            "path": _PROOF,
            "path_sealed": fauna_ffi.seal_path(owner_seed, _PROOF),
            "manifest_hash": manifest_hash,
            "size_bytes": len(_PROOF_CONTENT),
            "change_type": "create",
        },
    )

    assert "provision OK" in _fauna(
        udid, "provision", url, owner["actor_id_bytes"].hex(), device_id.hex(),
        "fp-ext", backup_key.hex(), owner["token"],
        writer_secret.hex(), device_authorization.hex(),
    )
    _register(udid, folder_ref, set_name)
    try:
        # Hydrate: the first open fetches the body through the extension.
        out = _fauna(udid, "read", folder_ref, set_name, _PROOF)
        assert _read_ok(out), (
            f"the first open did not return the sealed bytes: {out}\n{_extension_diagnostics(udid)}"
        )
        replica = _replica(udid, _PROOF)
        assert replica is not None, "no replica file under the device's CloudStorage"
        assert not _dataless(replica), f"{replica} still dataless after a read"

        # Evict: a body the gate does not keep advertises `allowsEvicting`.
        out = _fauna(udid, "evict", folder_ref, set_name, _PROOF)
        assert f"evict OK: {_PROOF}" in out, (
            f"the OS refused the eviction: {out}\n{_extension_diagnostics(udid)}"
        )
        assert _dataless(replica), f"{replica} still holds its body after an accepted evict"

        # With the nest up the body comes back through fetchContents.
        out = _fauna(udid, "read", folder_ref, set_name, _PROOF)
        assert _read_ok(out) and "dataless-before=true" in out, (
            f"the evicted body did not re-open from the nest: {out}\n{_extension_diagnostics(udid)}"
        )

        # A body fetched from another holder stays evictable and is not kept:
        # evicted again with the nest gone, nothing on the device serves it.
        assert f"evict OK: {_PROOF}" in _fauna(udid, "evict", folder_ref, set_name, _PROOF)
        nest["stop"]()
        out = _fauna(udid, "read", folder_ref, set_name, _PROOF)
        assert not _read_ok(out), f"a fetched body re-opened with no holder answering: {out}"
    finally:
        _fauna(udid, "remove", folder_ref, set_name)
        _fauna(udid, "revoke")


@pytest.mark.timeout(3600)
@pytest.mark.feature("files-on-demand")
def test_file_provider_own_record_reopens_after_eviction_ios(stoppable_nest, fp_device):
    """An own record in a metadata-only folder is not let go by the OS and
    reads back with no holder answering (`relay-serving.md` § Relay serving,
    *A holder keeps what it wrote*)."""
    nest, udid = stoppable_nest, fp_device
    url, port = nest["url"], nest["port"]

    owner = create_actor_and_register(port, admin_signing_key=nest["admin"]["signing_key"])
    owner_seed = bytes(owner["signing_key"])
    backup_key = blake3.blake3(owner_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()
    set_name = "fp-keep-" + secrets.token_hex(4)
    device_id = secrets.token_bytes(32)
    rel = "own-record.txt"
    text = f"fauna ios own record {secrets.token_hex(8)}"
    digest = hashlib.sha256(text.encode()).hexdigest()

    writer_secret, device_authorization = fauna_ffi.harness_machine_principal(url, owner_seed)
    with WsRpcAdminClient(url, actor_id=owner["actor_id_bytes"], signing_key=owner_seed) as ws:
        created = fauna_ffi.harness_create_set(url, owner_seed, {"name": set_name})
        folder_ref = f"local:{created['id']}"
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
        # Every harness seat enrolls on one deterministic device row with a
        # fresh writer key, so `harness_create_set`'s seat displaced the
        # principal's grant there; the extension's records go by reference,
        # so its signer must be live on a row — this device's own, as an
        # enrolled machine's is.
        ws.call(
            "fauna.sync.device_grant.register",
            {"device_id": device_id.hex(), "authorization": cbor2.loads(device_authorization)},
        )
    # Metadata-only BEFORE the extension first sees the set, so the record
    # carries no bytes to the nest and this device holds the only body.
    reply = set_folder_residency(
        port, set_name, secret_key=owner_seed.hex(), residency="metadata_only", base_url=url
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"

    assert "provision OK" in _fauna(
        udid, "provision", url, owner["actor_id_bytes"].hex(), device_id.hex(),
        "fp-ext", backup_key.hex(), owner["token"],
        writer_secret.hex(), device_authorization.hex(),
    )
    _register(udid, folder_ref, set_name)

    def listed() -> list:
        return _user_call(
            port, owner_seed.hex(), "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url
        ).get("items", [])

    try:
        out = _fauna(udid, "write", folder_ref, set_name, rel, text)
        assert f"write OK: {rel}" in out and digest in out, f"the write failed: {out}"
        # The owner is fresh and holds this one set: one listed item is the record.
        wait_until(
            lambda: len(listed()) == 1, 120, interval=1.0,
            diagnose=lambda: (
                f"the extension never recorded {rel!r}: {listed()!r}\n"
                f"{_extension_diagnostics(udid)}"
            ),
        )
        replica = _replica(udid, rel)
        assert replica is not None, "no replica file under the device's CloudStorage"
        assert not _dataless(replica), f"{replica} dataless right after its own write"

        # A kept item advertises no `allowsEvicting`: *Remove Download* is
        # refused as non-evictable, and the body stays where it was.
        out = _fauna(udid, "evict", folder_ref, set_name, rel)
        assert f"evict REFUSED: {rel}" in out and "Code=-2008" in out, (
            f"the OS no longer refuses to evict an own record ({out}) — the iOS keep "
            "now rests on the staging root alone: assert the evicted body re-opens "
            f"with the nest stopped\n{_extension_diagnostics(udid)}"
        )
        assert not _dataless(replica), f"{replica} went dataless though its evict was refused"

        # No holder answers, and the device still serves its own record.
        nest["stop"]()
        out = _fauna(udid, "read", folder_ref, set_name, rel)
        assert f"read OK: {rel}" in out and digest in out, (
            f"an own record did not read back with the nest stopped: {out}\n"
            f"{_extension_diagnostics(udid)}"
        )
    finally:
        _fauna(udid, "remove", folder_ref, set_name)
        _fauna(udid, "revoke")


@pytest.mark.timeout(3600)
@pytest.mark.feature("files-on-demand")
def test_file_provider_read_of_a_fetched_body_records_nothing_ios(
    stoppable_nest, fp_device, tmp_path
):
    """Reading a body makes no record: nothing wrote to it, so neither hydrating
    it, re-fetching it after an eviction nor removing the domain may upload it
    (`on-demand-files-apple.md` § Apple File Provider binding)."""
    nest, udid = stoppable_nest, fp_device
    url, port = nest["url"], nest["port"]

    owner = create_actor_and_register(port, admin_signing_key=nest["admin"]["signing_key"])
    owner_seed = bytes(owner["signing_key"])
    backup_key = blake3.blake3(owner_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()
    content_key = blake3.blake3(backup_key, derive_key_context=_CONVERGENT_CHUNK_CONTEXT).digest()
    set_name = "fp-read-" + secrets.token_hex(4)
    device_id = secrets.token_bytes(32)
    bodies = {
        f"read-{i}.txt": f"fauna ios read {i} {secrets.token_hex(8)}\n".encode()
        for i in range(_READS)
    }

    writer_secret, device_authorization = fauna_ffi.harness_machine_principal(url, owner_seed)
    with WsRpcAdminClient(url, actor_id=owner["actor_id_bytes"], signing_key=owner_seed) as ws:
        created = fauna_ffi.harness_create_set(url, owner_seed, {"name": set_name})
        folder_id = created["id"]
        folder_ref = f"local:{folder_id}"
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
        # As in the own-record test: a live signer, so a record the extension
        # makes lands here and is counted rather than refused.
        ws.call(
            "fauna.sync.device_grant.register",
            {"device_id": device_id.hex(), "authorization": cbor2.loads(device_authorization)},
        )

    for rel, body in bodies.items():
        manifest_bytes, chunks = fauna_ffi.seal_folder_file(body, content_key)
        for store_key, chunk in chunks:
            assert _post_bytes(port, "/api/v1/chunks", owner["token"], chunk) == store_key.hex()
        manifest_hash = _post_bytes(port, "/api/v1/manifests", owner["token"], manifest_bytes)
        fauna_ffi.harness_record_change(
            url,
            owner_seed,
            {
                "folder": set_name,
                "device_id": device_id.hex(),
                "path": rel,
                "path_sealed": fauna_ffi.seal_path(owner_seed, rel),
                "manifest_hash": manifest_hash,
                "size_bytes": len(body),
                "change_type": "create",
            },
        )

    def records() -> int:
        with WsRpcAdminClient(url, actor_id=owner["actor_id_bytes"], signing_key=owner_seed) as ws:
            listed = ws.call("fauna.sync.changes.list", {"folder_id": folder_id, "since": 0})
        return len(listed["changes"])

    def uploads() -> int:
        return _extension_tracing(udid).count("file uploaded")

    seeded = records()
    assert seeded == _READS, f"{seeded} records seeded, expected {_READS}"

    def read(rel: str) -> None:
        out = _fauna(udid, "read", folder_ref, set_name, rel)
        assert (
            f"read OK: {rel}" in out
            and hashlib.sha256(bodies[rel]).hexdigest() in out
            and "dataless-before=true" in out
        ), f"the read of {rel} failed: {out}\n{_extension_diagnostics(udid)}"

    assert "provision OK" in _fauna(
        udid, "provision", url, owner["actor_id_bytes"].hex(), device_id.hex(),
        "fp-ext", backup_key.hex(), owner["token"],
        writer_secret.hex(), device_authorization.hex(),
    )
    _register(udid, folder_ref, set_name)
    removed = False
    try:
        for rel in bodies:
            read(rel)
        for rel in bodies:
            out = _fauna(udid, "evict", folder_ref, set_name, rel)
            assert f"evict OK: {rel}" in out, f"{out}\n{_extension_diagnostics(udid)}"
            read(rel)

        # The positive control: this domain's own write does record, so the
        # silence below is the read's and not a signer that cannot land.
        rel, text = "control.txt", f"fauna ios control {secrets.token_hex(8)}"
        assert f"write OK: {rel}" in _fauna(udid, "write", folder_ref, set_name, rel, text)
        wait_until(
            lambda: records() > seeded, 120, interval=1.0,
            diagnose=lambda: f"the control never recorded\n{_extension_diagnostics(udid)}",
        )
        assert records() == seeded + 1, "a read recorded alongside the control"
        assert uploads() == 1, f"a read uploaded\n{_extension_diagnostics(udid)}"

        # Domain removal with the nest up: a failed run of this suite reaches
        # it within seconds of a hydrate, so it is a read's last chance.
        assert "remove OK" in _fauna(udid, "remove", folder_ref, set_name)
        removed = True
        assert records() == seeded + 1, "removing the domain recorded"
        assert uploads() == 1, f"removing the domain uploaded\n{_extension_diagnostics(udid)}"
    finally:
        _keep_logs(udid, tmp_path)
        if not removed:
            _fauna(udid, "remove", folder_ref, set_name)
        _fauna(udid, "revoke")

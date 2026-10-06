"""tier_3: macOS File Provider read path — populate → enumerate → hydrate.

Proves the M2 read path of the Apple File Provider binding end-to-end against a
live ``fauna-nest`` binary, **headlessly**, the way macOS itself drives it
(`docs/goal/behavior/on-demand-files.md` § On-Demand Files → Apple File Provider
binding, which owns this milestone chain):

  1. Seal one small file into an **owner-only sync-type** folder on the nest —
     chunk-sealed under ``convergent_chunk_root(BackupKey::derive(seed))``, the
     exact key an owner-``BackupKey`` engine decrypts with (agent-verified; the
     BackupKey derivation is pinned against `libs/fauna-core::crypto` KAT).
  2. Provision the **app-dead** extension's shared-app-group-Keychain capability
     (owner ``BackupKey`` + a session bearer) via the Fauna FP host app's
     ``provision`` command — the same ``FileProviderCredentialStore`` the real
     macOS app will use at first domain creation (M2 slice 3b).
  3. Register an ``NSFileProviderManager`` domain whose identifier is the set's
     actor-scoped identity (``local:<id>@<actor-hex>`` for this own-nest row —
     the host app's ``register`` scopes the bare ref it is handed to the
     provisioned account exactly as the app's reconcile does; the appex
     recovers the ref for the host constructor, which refuses a bare name) and
     whose display name is the set name (so the appex builds an owner-only
     engine for that set).
  4. From **another process** (`ls`/`cat` on ``~/Library/CloudStorage/``), assert
     the file **enumerates** (placeholder row, populated from the nest's
     ``changes.list``) and **hydrates** (open → ``fetchContents`` → decrypt →
     original bytes). "Another process" is the user's ground truth — the appex
     never observes its own I/O.
  5. **Remote-change nudge** (`sync-engine-deployments.md` § Control Plane
     Principle → Remote-change nudge): record a
     SECOND file on the nest — simulating a peer device's upload landing after
     this domain already enumerated once — then drive ``Fauna signal
     <set-name>``, the exact call ``FaunaClient.swift``'s live push observer
     makes on a ``fauna.sync.changed`` push (``FileProviderDomains.signalChanged``).
     Assert the second file enumerates too, proving the full round trip a real
     push would drive: `signalEnumerator` -> the extension's `enumerateChanges`
     -> `host.refresh()` -> re-signal iff changed -> the OS re-lists. This is the
     one hop `refresh_from_nest`'s own bool/repoint logic (tier_1-tested in
     ``libs/fauna-sync-engine/src/populate_placeholders_test.rs``'s
     ``apply_refresh_fold_*`` tests) cannot reach on its own — the OS actually
     re-invoking the extension in response to a signal.

Two preconditions this test does NOT paper over (each fails with guidance):

  * ``just apple-ffi-host`` must have built ``FaunaFFI.xcframework`` (the xcodeproj
    links it; the test never triggers the racy lazy FFI rebuild).
  * macOS ships a newly-installed File Provider extension **DISABLED** — enable it
    ONCE in System Settings → General → Login Items & Extensions → Extensions (i)
    → "Fauna Extensions" → File Provider. It is per-extension and survives
    reinstall. Until enabled, enumeration fails ``NSFileProviderErrorDomainDisabled``
    (FP -2011) and this test times out with that guidance.

Mutation carve-out (testing conventions point 8): the folder create / device
register / seal+upload here are **fixture setup** (arranging the world), not the
action under test. The action under test is the FP read path, driven through the
OS the way a user's Finder/Files does — `ls`/`cat`, not an API shortcut.
"""

import os
import secrets
import shutil
import subprocess
import sys
import time
from pathlib import Path

import blake3
import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register


def _has_codesigning_identity() -> bool:
    """True iff this machine has a real code-signing identity. The app→appex
    capability rendezvous rides the shared app-group **data-protection** Keychain,
    which macOS only lets a *properly-signed* app claim: under ad-hoc signing (no
    Team ID) `SecItemAdd`/`SecItemCopyMatching` return `errSecMissingEntitlement`
    (-34018), so `provision` cannot complete and the appex fails closed before it
    can enumerate (`on-demand-files.md` § Apple File Provider binding → Headless-first
    testing). This end-to-end appex path is therefore **signing-gated**; the
    read-path *mechanism* is proven headlessly at the FFI seam instead (interim
    `cabi.rs` bridge — the meantime while the org's Apple Developer cert lands)."""
    try:
        out = subprocess.run(
            ["security", "find-identity", "-v", "-p", "codesigning"],
            capture_output=True, text=True, timeout=30,
        ).stdout
    except OSError:
        return False
    return "0 valid identities found" not in out and " 1) " in out


pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="macOS-only: File Provider extension"),
    pytest.mark.skipif(
        not _has_codesigning_identity(),
        reason="appex-driven FP tier_3 is signing-gated: the app-group data-protection "
        "Keychain rendezvous returns errSecMissingEntitlement (-34018) under ad-hoc signing "
        "(no Apple Developer cert). Read-path mechanism proven headlessly at the FFI seam instead.",
    ),
    pytest.mark.tier_3,
]

# BLAKE3 derive_key contexts — MUST match `libs/fauna-core/src/crypto.rs`
# (`BackupKey::derive` :37-40, `BackupKey::convergent_chunk_root` :84-86). The
# owner-only Sync seal key derives from the identity seed as
# content_key = derive_key(CHUNK_CTX, derive_key(BACKUP_CTX, seed)); the appex is
# handed the intermediate BackupKey and re-derives content_key internally.
_BACKUP_KEY_CONTEXT = "fauna backup encryption key 2026-03-12"
_CONVERGENT_CHUNK_CONTEXT = "fauna.backup.chunk.v1"

_APPLE_DIR = Path(__file__).resolve().parents[5] / "apps" / "fauna-apple"
_XCODEPROJ = _APPLE_DIR / "Fauna.xcodeproj"
_XCFRAMEWORK = _APPLE_DIR / "FaunaFFI.xcframework"
_APPEX_ENTITLEMENTS = _APPLE_DIR / "Fauna-FileProvider" / "Fauna-FileProvider.entitlements"
# The real app's entitlements (carry the macOS app group — Team-ID-prefixed
# `7457N3M72H.group.social.fauna.shared` since the 2026-08-23 TCC matrix — since
# slice 3b; the transient M0 host + its entitlements were deleted with that slice).
_HOST_ENTITLEMENTS = _APPLE_DIR / "Fauna-macOS" / "Fauna-macOS.entitlements"
_LSREGISTER = (
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/"
    "LaunchServices.framework/Support/lsregister"
)
_CLOUD = Path.home() / "Library" / "CloudStorage"
_PROOF_CONTENT = b"fauna file provider read-path proof \xe2\x9c\x93\n"
# A second file, recorded AFTER the domain has already enumerated once — the
# remote-change-nudge proof simulates a peer device's upload landing mid-session.
_PROOF_CONTENT_2 = b"fauna file provider remote-change nudge proof \xe2\x9c\x93\n"


def _post_bytes(port: int, route: str, token: str, data: bytes) -> str:
    """POST raw bytes to a chunk/manifest route (Bearer auth); return the hex
    content hash the nest echoes (mirrors the web-paywall fixture)."""
    import json
    import urllib.request

    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{route}",
        data=data,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/octet-stream",
        },
        method="POST",
    )
    resp = urllib.request.urlopen(req)
    return json.loads(resp.read())["hash"]


def _run_host(binary: str, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    """Invoke a Fauna FP host command (`provision`/`register`/`remove`/`revoke`)."""
    proc = subprocess.run([binary, *args], capture_output=True, text=True, timeout=60)
    if check and proc.returncode != 0:
        raise AssertionError(
            f"`Fauna {' '.join(args[:1])}` failed ({proc.returncode}): "
            f"{proc.stdout.strip()} {proc.stderr.strip()}"
        )
    return proc


def _safe_listdir(path: Path) -> list[str]:
    """`os.listdir` that swallows the transient errors a File Provider container
    raises while the extension is still spinning up (so the poll keeps trying)."""
    try:
        return os.listdir(path)
    except OSError:
        return []


def _wait_for(predicate, timeout: float, msg: str, interval: float = 1.0):
    """Poll `predicate` until truthy or `timeout` seconds elapse; assert `msg`."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = predicate()
        if last:
            return last
        time.sleep(interval)
    raise AssertionError(msg)


@pytest.fixture(scope="session")
def fauna_fp_host_app():
    """Build + ad-hoc-sign + install the Fauna FP host app (embeds the appex),
    once per session. Requires ``just apple-ffi-host`` to have built the host
    xcframework first — this fixture NEVER triggers the lazy FFI rebuild (which
    races concurrent `just apple-ffi` on the shared xcframework and blows the
    per-test timeout; memory `apple-ffi-e2e-build-race-and-timeout`)."""
    if not _XCFRAMEWORK.exists():
        pytest.skip(
            f"{_XCFRAMEWORK} missing — run `just apple-ffi-host` before this tier_3 test "
            "(it links the FFI xcframework into the appex)."
        )
    # The SAME derived-data tree `just mac-app` builds into: one xcodebuild cache
    # per checkout rather than two (configurations namespace themselves under
    # Build/Products/<Config>, so a Debug test build and a Release shipping build
    # coexist in it).
    derived = _APPLE_DIR / ".xcode-build"
    build = subprocess.run(
        [
            "xcodebuild",
            "-project",
            str(_XCODEPROJ),
            "-scheme",
            "Fauna",
            "-configuration",
            "Debug",
            "-derivedDataPath",
            str(derived),
            "ENABLE_DEBUG_DYLIB=NO",
            "CODE_SIGNING_ALLOWED=NO",
            "CODE_SIGNING_REQUIRED=NO",
            "build",
        ],
        capture_output=True,
        text=True,
        timeout=1200,
    )
    if build.returncode != 0:
        raise AssertionError(f"xcodebuild failed:\n{build.stdout[-3000:]}\n{build.stderr[-2000:]}")

    built_app = derived / "Build" / "Products" / "Debug" / "Fauna.app"
    app_dst = Path.home() / "Applications" / "Fauna.app"
    app_dst.parent.mkdir(parents=True, exist_ok=True)
    if app_dst.exists():
        shutil.rmtree(app_dst)
    subprocess.run(["cp", "-R", str(built_app), str(app_dst)], check=True, timeout=120)

    # Ad-hoc sign appex (inner) then app (outer) with their entitlements — the
    # app-group + sandbox capabilities are restricted, so xcodebuild is told not
    # to sign and we sign by hand (this machine has 0 identities; ad-hoc suffices).
    appex = app_dst / "Contents" / "PlugIns" / "Fauna-FileProvider.appex"
    subprocess.run(
        ["codesign", "--force", "--sign", "-", "--entitlements", str(_APPEX_ENTITLEMENTS), str(appex)],
        check=True,
        timeout=60,
    )
    subprocess.run(
        ["codesign", "--force", "--sign", "-", "--entitlements", str(_HOST_ENTITLEMENTS), str(app_dst)],
        check=True,
        timeout=60,
    )
    # Unregister any OTHER bundle claiming the app's bundle id first (a stale
    # /Applications/Fauna.app from an installer test, an old FaunaMacOS.app e2e
    # artifact, …): fileproviderd resolves the registering app BY BUNDLE ID via
    # LaunchServices, and an appex-less twin winning that resolution fails every
    # register with FP -2001/-2014 (applicationExtensionNotFound) — exactly what
    # the slice-3b bring-up hit on macOS (2026-07-19). `lsregister -u` is
    # reversible and does not touch the bundles themselves.
    bundle_id = "social.fauna.fauna"
    twins = subprocess.run(
        ["mdfind", f"kMDItemCFBundleIdentifier == '{bundle_id}'"],
        capture_output=True, text=True, timeout=60,
    )
    for twin in twins.stdout.splitlines():
        twin = twin.strip()
        if twin and Path(twin) != app_dst:
            subprocess.run([_LSREGISTER, "-u", twin], timeout=60)
    subprocess.run([_LSREGISTER, "-f", str(app_dst)], check=True, timeout=60)
    subprocess.run(["pluginkit", "-a", str(appex)], timeout=60)
    return str(app_dst / "Contents" / "MacOS" / "Fauna")


@pytest.mark.timeout(1200)
@pytest.mark.feature("files-on-demand")
def test_file_provider_read_path(nest_instance, fauna_fp_host_app):
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    fauna = fauna_fp_host_app

    set_name = "fp-read-" + secrets.token_hex(4)
    device_id = secrets.token_bytes(32)

    # ── Owner identity + the owner-only Sync-set seal keys (pure-Python KDF; the
    # BackupKey is validated against the crypto.rs known-answer vector below). ──
    # Guard the pure-Python KDF against the `crypto.rs` :305-317 known-answer
    # vector (seed=[0x01;32]) before trusting it — a drift would silently seal
    # every file under a key the appex engine cannot decrypt.
    assert (
        blake3.blake3(bytes([1]) * 32, derive_key_context=_BACKUP_KEY_CONTEXT).digest().hex()
        == "f39582d247fa3bb84a45224943d9f058b8650bed6d6640e3a69165a147383a14"
    )

    owner = create_actor_and_register(port, admin_signing_key=admin_sk)
    owner_seed = bytes(owner["signing_key"])  # 32-byte Ed25519 seed
    backup_key = blake3.blake3(owner_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()
    content_key = blake3.blake3(backup_key, derive_key_context=_CONVERGENT_CHUNK_CONTEXT).digest()

    # ── Fixture setup: create the owner-only folder custody-first (the
    # records below are signed under its nonce) + register a write-capable
    # device (needed for the changes.record write gate). ──
    with WsRpcAdminClient(url, actor_id=owner["actor_id_bytes"], signing_key=owner_seed) as ws:
        created = fauna_ffi.harness_create_set(
            url, owner_seed, {"name": set_name}
        )
        # The set's `FolderRef` wire string — an own-nest row always takes the
        # `Local(id)` arm (`FolderRef::to_wire`). The host app's `register`
        # scopes it to the provisioned account for the domain identifier.
        folder_ref = f"local:{created['id']}"
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )

    # ── Fixture setup: seal + upload one file the production chunk+record way.
    # Owner-only ⇒ pass the derived content_key and OMIT content_key_version. ──
    manifest_bytes, chunks = fauna_ffi.seal_folder_file(_PROOF_CONTENT, content_key)
    for store_key, body in chunks:
        got = _post_bytes(port, "/api/v1/chunks", owner["token"], body)
        assert got == store_key.hex(), f"chunk store-key mismatch: {got} != {store_key.hex()}"
    manifest_hash = _post_bytes(port, "/api/v1/manifests", owner["token"], manifest_bytes)
    # Signed (writer-signed change records): through the shared signer.
    fauna_ffi.harness_record_change(
        url,
        owner_seed,
        {
            "folder": set_name,
            "device_id": device_id.hex(),
            "path": "proof.txt",
            # The nest rests no plaintext path (path-sealing S9 flip): seal
            # through the real funnel, as `common.auth.sync_changes_record`
            # does — the extension's engine opens it with the BackupKey.
            "path_sealed": fauna_ffi.seal_path(owner_seed, "proof.txt"),
            "manifest_hash": manifest_hash,
            "size_bytes": len(_PROOF_CONTENT),
            "change_type": "create",
        },
    )

    before = set(os.listdir(_CLOUD)) if _CLOUD.exists() else set()
    try:
        # ── Provision the app-dead capability + register the domain
        # (identifier = the set's ref scoped to the provisioned account,
        # display name = the set name). ──
        _run_host(
            fauna, "provision", url, owner["actor_id_bytes"].hex(), device_id.hex(),
            "fp-ext", backup_key.hex(), owner["token"],
        )
        _run_host(fauna, "register", folder_ref, set_name)

        # ── ENUMERATE (another process): fileproviderd materializes the domain's
        # replica dir, and listing it drives the appex enumerate → the nest's
        # changes.list is folded into placeholder rows → proof.txt appears. ──
        _CLOUD.mkdir(parents=True, exist_ok=True)
        domain_dir = _wait_for(
            lambda: next((_CLOUD / d for d in set(os.listdir(_CLOUD)) - before), None),
            timeout=45,
            msg=f"no new domain directory appeared under {_CLOUD} after register — "
            "fileproviderd never created the replica.",
        )
        proof = domain_dir / "proof.txt"
        # Listing the container (not a single-path stat) is what drives the OS to
        # call the appex's `enumerator(for: .rootContainer)`.
        _wait_for(
            lambda: "proof.txt" in _safe_listdir(domain_dir),
            timeout=45,
            msg=f"proof.txt never enumerated in {domain_dir}. Most likely the Fauna File "
            "Provider extension is DISABLED — enable it once in System Settings → General → "
            "Login Items & Extensions → Extensions (i) → 'Fauna Extensions' (FP -2011).",
        )

        # ── HYDRATE (another process): open → fetchContents → decrypt under the
        # BackupKey → the original bytes. ──
        assert proof.read_bytes() == _PROOF_CONTENT, "hydrated bytes did not match the sealed file"

        # ── REMOTE-CHANGE NUDGE: a peer device's upload lands on the nest AFTER
        # this domain already enumerated once. Nothing re-pulls it until either
        # the rescan-cadence backstop (registered at 2s above) or a nudge — drive
        # the nudge explicitly via `Fauna signal`, the exact call FaunaClient.swift's
        # live push observer makes on a `fauna.sync.changed` push, so a fresh
        # `register` racing the rescan timer can't make this pass for the wrong
        # reason. ──
        manifest_bytes_2, chunks_2 = fauna_ffi.seal_folder_file(_PROOF_CONTENT_2, content_key)
        for store_key, body in chunks_2:
            got = _post_bytes(port, "/api/v1/chunks", owner["token"], body)
            assert got == store_key.hex(), f"chunk store-key mismatch: {got} != {store_key.hex()}"
        manifest_hash_2 = _post_bytes(port, "/api/v1/manifests", owner["token"], manifest_bytes_2)
        fauna_ffi.harness_record_change(
            url,
            owner_seed,
            {
                "folder": set_name,
                "device_id": device_id.hex(),
                "path": "proof2.txt",
                "path_sealed": fauna_ffi.seal_path(owner_seed, "proof2.txt"),
                "manifest_hash": manifest_hash_2,
                "size_bytes": len(_PROOF_CONTENT_2),
                "change_type": "create",
            },
        )

        _run_host(fauna, "signal", folder_ref, set_name)

        proof2 = domain_dir / "proof2.txt"
        _wait_for(
            lambda: "proof2.txt" in _safe_listdir(domain_dir),
            timeout=45,
            msg=f"proof2.txt never enumerated in {domain_dir} after `Fauna signal {folder_ref} {set_name}` — "
            "the remote-change nudge (signalEnumerator -> enumerateChanges -> host.refresh() -> "
            "re-signal) did not reach the OS's listing.",
        )
        assert proof2.read_bytes() == _PROOF_CONTENT_2, (
            "hydrated bytes for the nudged file did not match the sealed file"
        )
    finally:
        _run_host(fauna, "remove", folder_ref, set_name, check=False)
        _run_host(fauna, "revoke", check=False)

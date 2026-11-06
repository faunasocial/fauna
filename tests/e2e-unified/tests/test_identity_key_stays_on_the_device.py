"""The app never puts the identity key into a backup or a sync that leaves the
device.

`docs/features/identity-protected-on-this-device.md` outcome 4;
`docs/goal/architecture/apps/common.md` § Credential storage → *What a
device's own backup carries* — "the app never, by itself, puts the identity
secret into a backup or a sync that leaves the device", and the table's third
column, which is **travels sealed** for linux and for tui on a desktop
session: the secret rests in the person's Secret Service keyring, and a
home-directory backup carries it only as that keyring carries it.

The observables, per column this machine runs (linux, tui):

1. **Where it rests.** A real sign-in — import a registered key, the real
   handle check against a live nest, Continue — on a launch whose only store
   is a private ``gnome-keyring-daemon`` (convention 10's real-keyring
   carve-out; linux's ``use_real_keyring`` mode, tui's ``probed_store`` mode,
   neither with ``FAUNA_E2E_CREDENTIAL_DIR``). The secret is found in the
   keyring (the positive control: the search below would find it if it were
   anywhere it looks), and no file under the launch's XDG data or config dirs
   — what the person's own home backup carries in the clear — holds it in any
   encoding.
2. **What leaves.** The sync's far end is the nest: once the account runtime
   is assembled (the writer key, the principal bundle and the device's
   registration have all been sent), no file of the nest's state — the
   database, its WAL, every blob — holds the secret in any encoding, while
   the actor's PUBLIC key is found there (the control that the search reads
   the nest's real bytes).

android's column (**stays**) is the second test: the installed package's
backup and device-transfer posture read off the device, then the same
nest-side search. Where the secret rests on android (outcome 3) needs the
production store, which no ordinary launch uses, so it lives in convention 12's
venue: ``real_session/test_identity_rests_in_the_android_secure_prefs.py``.

ios's column (**stays**) is the third: a launch whose store runs on the seat
simulator's real keychain, the identity item's class and sync flag read back
(attributes only), then the same nest-side search.

macOS's column (**travels sealed**, on the legacy plane every Developer-ID
build is on today) is the fifth: a launch whose store runs on a keychain file
the harness owns, never the box's login keychain.

web's column (**travels sealed**) is the fourth: every place the origin keeps
bytes — the browser profile a backup or a profile sync carries — searched for
the secret once the seal is on, then the same nest-side search.

The person's own acts that move the key (export, the recovery kit, adding a
device) are not driven here — they are the rule's sanctioned exits. windows'
column, and tui's on windows (non-roaming persistence), read the box's real
Credential Manager, which no e2e launch may write (convention 10), so they live
in convention 12's venue: ``real_session/test_identity_rests_in_credential_manager.py``,
which imports this file's two searches.
"""

from __future__ import annotations

import base64
import contextlib
import hashlib
import json
import subprocess
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from helpers.admin_wire import admit_adult
from common.keyring import read_field, secret_service_available, unique_namespace
from common.launch_harness import sign_in_through_the_wizard
from conftest import _seeded_environment, get_available_apps
from drivers import create_driver
from helpers.app_surface import skip_environment
from tests.test_onboarding_launch_routing_smoke import smoke_handled_nest  # noqa: F401 (fixture)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.feature("identity-protected-on-this-device"),
]


@pytest.fixture(params=["linux", "tui"])
def keyring_client(request):
    """The two desktop columns whose store, on this machine, is the Secret
    Service; the client id lands in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip(f"{request.param} is not in this run's app set")
    if not secret_service_available():
        skip_environment("gnome-keyring-daemon + dbus-daemon are not installed")
    return request.param


@pytest.fixture
def secret_service():
    from drivers.secret_service import PrivateSecretService

    svc = PrivateSecretService().start()
    try:
        yield svc
    finally:
        svc.stop()


def _encodings(secret: bytes) -> list[bytes]:
    return [
        secret,
        secret.hex().encode(),
        secret.hex().upper().encode(),
        base64.b64encode(secret),
        base64.urlsafe_b64encode(secret),
        base64.b64encode(secret).rstrip(b"="),
        base64.urlsafe_b64encode(secret).rstrip(b"="),
    ]


def _files_holding(root: Path, needles: list[bytes]) -> list[str]:
    hits = []
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        with contextlib.suppress(OSError):
            data = path.read_bytes()
            if any(n in data for n in needles):
                hits.append(str(path.relative_to(root)))
    return sorted(hits)


def _launch_config(client, app_path, nest, xdg_base, namespace, bus, request) -> dict:
    environment = {**_seeded_environment(request, nest), "DBUS_SESSION_BUS_ADDRESS": bus}
    if client == "linux":
        # Real libsecret persistence, no file backend (`drivers/linux.py`
        # `build_launch_env`); the bus is the private daemon's.
        return {
            "app_path": app_path, "url": nest["url"], "use_real_keyring": True,
            "keyring_app": namespace, "xdg_base": str(xdg_base),
            "environment": environment,
        }
    # tui: the production store resolution, unforced, on the same bus — the
    # probe finds the private daemon usable and takes the keyring arm.
    return {
        "app_path": app_path, "url": nest["url"], "probed_store": True,
        "keyring_app": namespace, "xdg_base": str(xdg_base),
        "environment": environment,
    }


# The Secret Service on a private session bus is a linux box's (convention 7, the
# fourth skip kind); the environment skip in `keyring_client` stays for a linux box
# with no gnome-keyring or dbus daemon.
@pytest.mark.host_platform("linux")
def test_the_identity_key_rests_in_the_keyring_and_never_reaches_the_nest(
    keyring_client, smoke_handled_nest, secret_service, tmp_path, request  # noqa: F811
):
    client = keyring_client
    nest = smoke_handled_nest
    actor = admit_adult(nest, f"staysput{client}", domain=nest["authority"], token=True)
    secret = bytes(actor["signing_key"])
    public = SigningKey(secret).verify_key.encode()
    namespace = unique_namespace(f"fauna-e2e-stays-{client}")
    xdg_base = tmp_path / "xdg"
    xdg_base.mkdir()

    driver = create_driver(client)
    app_path = request.getfixturevalue(f"{client}_app_path")
    driver.launch(_launch_config(
        client, app_path, nest, xdg_base, namespace, secret_service.address, request
    ))
    try:
        sign_in_through_the_wizard(driver, actor, nest)

        # ── 1. Where it rests: the keyring, and no XDG file. ──────────────────
        stored = read_field(
            secret_service.address, namespace, f"fauna/{public.hex()}/secret"
        )
        assert stored is not None and any(
            n in stored.encode() for n in _encodings(secret)
        ), (
            "the identity secret is not in the keyring item the app writes it to "
            "— the positive control for the searches below failed"
        )
        local = [
            hit for sub in ("data", "config") if (xdg_base / sub).is_dir()
            for hit in (f"{sub}/{h}" for h in _files_holding(xdg_base / sub, _encodings(secret)))
        ]
        assert not local, (
            "the identity secret rests readable in the app's own files, which "
            f"a home-directory backup carries as they are: {local}"
        )

        # ── 2. What leaves: nothing of the nest's state holds it. ────────────
        nest_root = Path(nest["tmp_dir"])
        assert _files_holding(nest_root, [public, public.hex().encode()]), (
            "the actor's public key is nowhere in the nest's state — the search "
            "is not reading the nest's real bytes, so its finding below means nothing"
        )
        remote = _files_holding(nest_root, _encodings(secret))
        assert not remote, (
            f"the identity secret reached the nest, in: {remote} — an app path "
            "sent it off the device"
        )
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()


@pytest.fixture(params=["android"])
def android_client(request):
    """android-only, the client id in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("android is not in this run's app set")
    return request.param


@pytest.mark.android
def test_the_installed_android_package_keeps_the_identity_out_of_backup_and_off_the_nest(
    android_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """android's column: **stays** — the store's master key lives in the
    Android Keystore, and the store's file is kept out of cloud backup and out
    of device-to-device transfer (`apps/common.md` § Credential storage, the
    Android row).

    What travels is decided per PACKAGE, by the manifest the device installed:
    ``android:allowBackup`` for cloud backup, and since API 31
    ``android:dataExtractionRules`` for both cloud backup and device transfer
    (``allowBackup="false"`` alone does not stop the transfer on some devices).
    So after a real sign-in through the wizard, the posture is read from the
    device, never from the source tree (``CloudBackupPostureTest`` reads that):

    1. **The package manager's reading** — ``dumpsys package`` shows no
       ``ALLOW_BACKUP`` flag (``HAS_CODE`` is the control that the flags line
       was read at all).
    2. **The installed manifest** — the APK ``pm path`` names, pulled off the
       device: ``allowBackup`` is false, and ``dataExtractionRules`` names a
       rules file whose ``cloud-backup`` and ``device-transfer`` sections each
       exclude every domain and include none. A per-package rule covers every
       file under the app's data, whichever store the secret rests in — an e2e
       launch rests it in the ``FAUNA_E2E_CREDENTIAL_FILE`` backend
       (convention 10), production in the Keystore-sealed prefs file.
    3. **What leaves** — the linux/tui arm's nest-side search: no file of the
       nest's state holds the secret in any encoding, while the actor's public
       key is found there.
    """
    from common.launch_harness import make_launch_harness
    from conftest import _trust_seeder
    from helpers.android_package import backup_posture, dumpsys_flags

    nest = smoke_handled_nest
    actor = admit_adult(nest, "staysputandroid", domain=nest["authority"], token=True)
    secret = bytes(actor["signing_key"])
    public = SigningKey(secret).verify_key.encode()

    harness = make_launch_harness(
        "android", tmp_path=tmp_path,
        app_path=request.getfixturevalue("android_app_path"),
        seed_trust=_trust_seeder(request),
    )
    driver = harness.launch(node_url=nest["url"], trust=nest)
    try:
        sign_in_through_the_wizard(driver, actor, nest)

        # ── 1. The package manager's reading of the installed manifest. ──────
        flags = dumpsys_flags(driver.dumpsys_package())
        assert "HAS_CODE" in flags, (
            f"no flags line read from dumpsys package (got {sorted(flags)}) — the "
            "ALLOW_BACKUP check below would pass over nothing"
        )
        assert "ALLOW_BACKUP" not in flags, (
            f"the installed package allows backup: flags={sorted(flags)}"
        )

        # ── 2. The installed APK's backup and transfer rules. ────────────────
        posture = backup_posture(driver.pull_installed_apk(tmp_path))
        assert posture.allow_backup is False, (
            f"the installed manifest's allowBackup is {posture.allow_backup!r} "
            "(None = absent, which the platform reads as true)"
        )
        assert posture.rules_member is not None, (
            "the installed manifest names no dataExtractionRules — on API 31+ a "
            "device-to-device transfer then carries the app's files"
        )
        leaks = posture.sections_leaving_something()
        assert not leaks, (
            f"the installed data-extraction rules ({posture.rules_member}) let "
            f"domains travel: {leaks}"
        )

        # ── 3. What leaves: nothing of the nest's state holds it. ────────────
        nest_root = Path(nest["tmp_dir"])
        assert _files_holding(nest_root, [public, public.hex().encode()]), (
            "the actor's public key is nowhere in the nest's state — the search "
            "is not reading the nest's real bytes, so its finding below means nothing"
        )
        remote = _files_holding(nest_root, _encodings(secret))
        assert not remote, (
            f"the identity secret reached the nest, in: {remote} — an app path "
            "sent it off the device"
        )
    finally:
        harness.teardown()


@pytest.fixture(params=["ios"])
def ios_client(request):
    """ios-only, the client id in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("ios is not in this run's app set")
    return request.param


def _keychain_copies(driver, account: str) -> tuple[int, list[dict]]:
    """The class and sync flag of every copy this launch's real keychain holds
    of ``account`` — attributes only (``keychain_item_attributes``, FaunaKit's
    ``KeychainItemAttributesTestCommand``)."""
    raw = driver.call_command("keychain_item_attributes", {"account": account})
    assert raw, (
        "keychain_item_attributes answered nothing — the reader is not built "
        "into this app, or it refused (the TestAgent failure names why)"
    )
    report = json.loads(raw)
    return report["status"], report["copies"]


@pytest.mark.ios
def test_the_ios_identity_rests_device_bound_in_the_real_keychain_and_never_reaches_the_nest(
    ios_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """ios's column: **stays** — the store writes every row
    ``kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`` and non-synchronizable
    unless the person opts into the iCloud-Keychain backup
    (`apps/common.md` § Credential storage, the macOS / iOS row; mechanics
    `apps/ios.md` § Credential Storage). ``ThisDeviceOnly`` keeps the item out
    of iCloud Keychain and out of a device backup restored elsewhere.

    Which class the store *asks for* is pinned headlessly
    (``KeychainAccessibilityTests``); the fact only an e2e adds is that the
    item the REAL keychain holds after a real onboarding carries it. So this
    launch runs the store on the seat simulator's own keychain
    (``real_keychain`` → ``KeychainStore.e2eRealKeychain``, iOS only — the
    keychain is the seat's, not the box's, so convention 10 holds) and reads
    the identity row's attributes back, never its data.

    1. **Where it rests** — after a real sign-in through the wizard, the
       ``fauna/<actor>/secret`` row is exactly one copy, device-bound and not
       synchronizable.
    2. **What leaves** — the linux/tui arm's nest-side search: no file of the
       nest's state holds the secret in any encoding, while the actor's public
       key is found there.
    3. **Nothing outlives the test** — the reset sweeps the real row away.

    Outcome 3 is the same launch: the row holds the secret (a digest of the
    store's own read, the positive control), and no file of the app's data
    container holds it in any encoding, while the nest's authority and the
    actor's public key are found there (the control that the search reads the
    container's real bytes).

    macOS's column is the next test."""
    from common.launch_harness import make_launch_harness
    from conftest import _trust_seeder
    from helpers.app_launch import app_launch_config

    nest = smoke_handled_nest
    actor = admit_adult(nest, "staysputios", domain=nest["authority"], token=True)
    secret = bytes(actor["signing_key"])
    public = SigningKey(secret).verify_key.encode()
    account = f"fauna/{public.hex()}/secret"

    harness = make_launch_harness(
        "ios", tmp_path=tmp_path, **app_launch_config("ios", request),
        seed_trust=_trust_seeder(request),
        extra_launch_config={"real_keychain": True},
    )
    driver = harness.launch(node_url=nest["url"], trust=nest)
    swept = False
    try:
        status, copies = _keychain_copies(driver, account)
        assert copies == [], (
            f"the seat simulator's keychain already holds {account} before the "
            f"sign-in (status {status}, {copies}) — the launch's sweep did not run"
        )
        sign_in_through_the_wizard(driver, actor, nest)

        # ── 1. Where it rests: one device-bound, non-synchronizable copy. ─────
        status, copies = _keychain_copies(driver, account)
        assert status == 0 and copies, (
            f"no keychain item holds {account} after a real sign-in (status "
            f"{status}) — the store did not reach the real keychain, so nothing "
            "below would mean anything"
        )
        assert copies == [{
            "accessible": "kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly",
            "synchronizable": False,
        }], (
            f"the identity row rests as {copies} — expected exactly one "
            "device-bound, non-synchronizable copy"
        )
        report = json.loads(driver.call_command(
            "keychain_item_attributes", {"account": account, "digest": True}
        ))
        assert report.get("value_sha256") in {
            hashlib.sha256(e).hexdigest() for e in _encodings(secret)
        }, (
            f"the {account} row does not hold an encoding of the identity secret — "
            "the positive control for the search below failed"
        )
        container = driver.app_data_container()
        assert container, "the app's data container could not be located"
        container = Path(container)
        assert _files_holding(
            container, [public, public.hex().encode(), nest["authority"].encode()]
        ), (
            "neither the actor's public key nor the nest's authority is anywhere in "
            f"{container} — the search is not reading the container's real bytes, so "
            "its finding below means nothing"
        )
        local = _files_holding(container, _encodings(secret))
        assert not local, (
            "the identity secret rests readable outside the keychain, in the app's "
            f"data container: {local}"
        )

        # ── 2. What leaves: nothing of the nest's state holds it. ────────────
        nest_root = Path(nest["tmp_dir"])
        assert _files_holding(nest_root, [public, public.hex().encode()]), (
            "the actor's public key is nowhere in the nest's state — the search "
            "is not reading the nest's real bytes, so its finding below means nothing"
        )
        remote = _files_holding(nest_root, _encodings(secret))
        assert not remote, (
            f"the identity secret reached the nest, in: {remote} — an app path "
            "sent it off the device"
        )

        # ── 3. Nothing outlives the test: the reset sweeps the real row. ─────
        driver.reset()
        swept = True
        status, copies = _keychain_copies(driver, account)
        assert copies == [], (
            f"the reset left {account} in the seat simulator's keychain: {copies}"
        )
    finally:
        if not swept:
            with contextlib.suppress(Exception):
                driver.reset()
        harness.teardown()


@pytest.fixture(params=["macos"])
def macos_client(request):
    """macos-only, the client id in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("macos is not in this run's app set")
    return request.param


def _security(*args: str) -> subprocess.CompletedProcess:
    """``/usr/bin/security``, bounded: never ask it for an item's data (``-w``)
    — the item's ACL names only the app, so that read blocks on a dialog."""
    return subprocess.run(
        ["/usr/bin/security", *args], capture_output=True, text=True, timeout=30
    )


@pytest.mark.macos
def test_the_macos_identity_rests_sealed_in_a_keychain_file_and_never_reaches_the_nest(
    macos_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """macOS's column: **travels sealed** on the legacy plane, the plane every
    Developer-ID build is on until path A (`apps/common.md` § Credential
    storage, the macOS / iOS row): the row is an ordinary keychain item, kept
    out of iCloud Keychain and carried by a home-folder backup only inside a
    file sealed under that keychain's password. The legacy plane is a plane of
    FILES, so this launch is given its own (``real_keychain`` →
    ``Plane.legacyFile``) and the box's login keychain stays out of it
    (`apps/ios.md` § Credential Storage, the macOS witness seam).

    1. **Where it rests** — after a real sign-in through the wizard:
       ``security`` (another process, reading the file) finds the
       ``fauna/<actor>/secret`` row; the app's read of it digests to an
       encoding of the secret, the positive control for the search that
       follows, which finds the secret in no file of the launch's tree, logs
       and credential dir included; and the row is exactly one copy, with no
       sync flag and NO accessibility class — the platform does not honour a
       class on this plane, and that absence IS the travels-sealed reading.
    2. **What leaves** — the linux/tui arm's nest-side search.
    3. **Nothing outlives the test** — the reset sweeps the row, and the user's
       search list and login keychain never saw any of it.

    The device-bound class's read-back and the opt-in copy's outcomes on macOS
    need the data-protection plane, so they wait for path A, in convention 12's
    venue."""
    from common.launch_harness import make_launch_harness
    from conftest import _trust_seeder
    from helpers.app_launch import app_launch_config

    nest = smoke_handled_nest
    actor = admit_adult(nest, "staysputmacos", domain=nest["authority"], token=True)
    secret = bytes(actor["signing_key"])
    public = SigningKey(secret).verify_key.encode()
    account = f"fauna/{public.hex()}/secret"
    locate = ("-s", "social.fauna.account", "-a", account)
    search_list = _security("list-keychains", "-d", "user").stdout

    harness = make_launch_harness(
        "macos", tmp_path=tmp_path, **app_launch_config("macos", request),
        seed_trust=_trust_seeder(request),
        extra_launch_config={"real_keychain": True},
    )
    driver = harness.launch(node_url=nest["url"], trust=nest)
    swept = False
    try:
        keychain = driver.keychain_path
        assert keychain and Path(keychain).is_file(), (
            f"the launch made no keychain file ({keychain!r}) — this launch is not "
            "on the harness's keychain, so nothing below would mean anything"
        )
        status, copies = _keychain_copies(driver, account)
        assert copies == [], (
            f"the launch's keychain file already holds {account} (status {status}, "
            f"{copies}) — it is not the fresh file the launch made"
        )
        sign_in_through_the_wizard(driver, actor, nest)

        # ── 1. Where it rests: the file, sealed, and no other file readable. ──
        found = _security("find-generic-password", *locate, keychain)
        assert found.returncode == 0, (
            f"security finds no {account} in the harness's keychain file "
            f"(exit {found.returncode}: {found.stderr.strip()}) — the store did not "
            "reach it, so nothing below would mean anything"
        )
        report = json.loads(driver.call_command(
            "keychain_item_attributes", {"account": account, "digest": True}
        ))
        assert report.get("value_sha256") in {
            hashlib.sha256(e).hexdigest() for e in _encodings(secret)
        }, (
            f"the {account} row does not hold an encoding of the identity secret — "
            "the positive control for the searches below failed"
        )
        launch_tree = Path(driver._tmp_dir)
        assert _files_holding(launch_tree, [public, public.hex().encode()]), (
            "the actor's public key is nowhere in the launch's tree — the search "
            "is not reading the launch's real bytes, so its finding below means nothing"
        )
        local = _files_holding(launch_tree, _encodings(secret))
        assert not local, (
            "the identity secret rests readable outside the keychain, in files a "
            f"home-folder backup carries as they are: {local}"
        )
        assert report["copies"] == [{"accessible": "nil", "synchronizable": False}], (
            f"the identity row rests as {report['copies']} — expected exactly one "
            "copy, not synchronizable, and no accessibility class: on the legacy "
            "plane a class means the store is no longer on the plane this reads"
        )
        assert _security("find-generic-password", *locate).returncode != 0, (
            f"the default search list holds {account} — the store wrote the box's "
            "own keychain, which no e2e launch may"
        )

        # ── 2. What leaves: nothing of the nest's state holds it. ────────────
        nest_root = Path(nest["tmp_dir"])
        assert _files_holding(nest_root, [public, public.hex().encode()]), (
            "the actor's public key is nowhere in the nest's state — the search "
            "is not reading the nest's real bytes, so its finding below means nothing"
        )
        remote = _files_holding(nest_root, _encodings(secret))
        assert not remote, (
            f"the identity secret reached the nest, in: {remote} — an app path "
            "sent it off the device"
        )

        # ── 3. Nothing outlives the test: the reset sweeps the row. ──────────
        driver.reset()
        swept = True
        status, copies = _keychain_copies(driver, account)
        assert copies == [], f"the reset left {account} in the keychain file: {copies}"
        assert _security("list-keychains", "-d", "user").stdout == search_list, (
            "the user's keychain search list changed across the test"
        )
    finally:
        if not swept:
            with contextlib.suppress(Exception):
                driver.reset()
        harness.teardown()
    assert _security("list-keychains", "-d", "user").stdout == search_list, (
        "the launch's teardown left the user's keychain search list changed"
    )


@pytest.mark.web
def test_the_web_identity_rests_only_sealed_in_the_origin_and_never_reaches_the_nest(
    nest_instance, spa_url
):
    """web's column (**travels sealed**, `common.md` § Credential storage, the
    table's Web row): the browser profile is what a backup or a profile sync
    carries, so the observable is every place the origin keeps bytes —
    ``localStorage``, ``sessionStorage``, every IndexedDB database and every
    OPFS file, the account store's two homes included — and the secret must
    be in none of them in any encoding once the seal is on and the account
    runtime has assembled (the writer key, the principal bundle and the
    device's registration sent). The control is the actor's public key, found
    by the same search. Then the same nest-side search as the native columns.

    The test build keeps the plain arm unless a journey opts in
    (`web.md` § Identity store at rest → *Test builds*), so this opts in and
    seals a signed-in browser in place — the shape a shipped build has from
    its first launch."""
    from common import build_registry_seed, create_actor_and_register
    from helpers.waiting import await_account_runtime_assembled
    from helpers.web_store import origin_holdings
    from tests.test_account_switcher_web import _await_boot_landed
    from tests.test_web_sealed_identity_store import (
        CONFIRM_INPUT, OPT_IN_KEY, PASSPHRASE_INPUT, _await_unlocked, _submit,
    )

    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    secret = bytes(actor["signing_key"])
    public = SigningKey(secret).verify_key.encode()
    seed = build_registry_seed(
        [{"actor_id": actor["actor_id_hex"], "secret_hex": secret.hex(),
          "nest_url": spa_url, "device_id": "stays-web", "handle": "stays"}],
        active=actor["actor_id_hex"],
    )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/", "seed_credentials": seed})
    try:
        _await_boot_landed(driver)
        driver.eval_js(f"localStorage.setItem({OPT_IN_KEY!r}, '1')")
        driver.hard_reload()
        driver.wait_for(CONFIRM_INPUT, timeout=30)
        passphrase = "stays-on-this-device"
        _submit(driver, passphrase, confirm=passphrase)
        _await_unlocked(driver)
        await_account_runtime_assembled(driver)
        assert driver.eval_js("localStorage.getItem('fauna/seal') !== null"), (
            "the seal header is missing — the store was never sealed"
        )

        # ── 1. Where it rests: nowhere in the origin but sealed. ─────────────
        public_needles = [public, public.hex().encode()]
        secret_needles = _encodings(secret)
        held = origin_holdings(driver, public_needles + secret_needles)
        found = held["found"]
        assert any(found[n.hex()] for n in public_needles), (
            "the actor's public key is nowhere in the origin's storage — the "
            f"search is not reading the profile's real bytes ({held['places']} "
            f"places, {held['idb_values']} IndexedDB values, {held['opfs_files']} "
            "OPFS files), so its finding below means nothing"
        )
        local = sorted({w for n in secret_needles for w in found[n.hex()]})
        assert not local, (
            "the identity secret rests readable in the browser profile, which a "
            f"backup or a profile sync carries as it is: {local}"
        )

        # ── 2. What leaves: nothing of the nest's state holds it. ────────────
        nest_root = Path(nest_instance["tmp_dir"])
        assert _files_holding(nest_root, public_needles), (
            "the actor's public key is nowhere in the nest's state — the search "
            "is not reading the nest's real bytes, so its finding below means nothing"
        )
        remote = _files_holding(nest_root, secret_needles)
        assert not remote, (
            f"the identity secret reached the nest, in: {remote} — an app path "
            "sent it off the device"
        )
        assert driver.is_absent(PASSPHRASE_INPUT)
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()

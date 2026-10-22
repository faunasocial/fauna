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

The person's own acts that move the key (export, the recovery kit, adding a
device) are not driven here — they are the rule's sanctioned exits. The other
columns' observables are other machines' (iOS / macOS keychain classes,
windows' non-roaming persistence) or need the android device this machine
does not always have; those twins are captured as their own rows.
"""

from __future__ import annotations

import base64
import contextlib
import time
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from common.auth import register_handled_actor
from common.keyring import read_field, secret_service_available, unique_namespace
from conftest import _seeded_environment, get_available_apps
from drivers import create_driver
from helpers.app_surface import skip_environment
from helpers.waiting import await_account_runtime_assembled
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


def test_the_identity_key_rests_in_the_keyring_and_never_reaches_the_nest(
    keyring_client, smoke_handled_nest, secret_service, tmp_path, request  # noqa: F811
):
    from actions import ActionLayer
    from common.launch_harness import reached_authenticated_app

    client = keyring_client
    nest = smoke_handled_nest
    actor = register_handled_actor(
        nest["port"], handle=f"staysput{client}", domain=nest["authority"],
        base_url=nest["url"],
    )
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
        app = ActionLayer(driver)
        app.onboarding.navigate_to_status()
        app.onboarding.import_key(secret.hex())
        driver.wait_for("handle-input", timeout=60)
        app.onboarding.fill_handle(f"{actor['handle']}@{nest['authority']}")
        app.onboarding.run_handle_check(timeout=45)
        app.onboarding.submit_handle()
        # A terminal wizard step may stand between Continue and the app (the
        # NAT-mode confirm, a launch retry): clicking through is the person's
        # own next action; the loop exits on arrival (smoke K's shape).
        deadline = time.monotonic() + 120
        while True:
            try:
                reached_authenticated_app(driver, timeout=2)
                break
            except Exception:
                if time.monotonic() > deadline:
                    pytest.fail(
                        "a real sign-in never reached the main app; "
                        f"error={app.error_text()!r}"
                    )
            for btn in ("nat-mode-confirm-button", "launch-retry-button"):
                with contextlib.suppress(Exception):
                    if driver.is_visible(btn):
                        driver.click(btn)
        # Everything the sign-in sends — the device registration, the writer
        # key, the principal bundle — has been sent once the runtime is up.
        await_account_runtime_assembled(driver)

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

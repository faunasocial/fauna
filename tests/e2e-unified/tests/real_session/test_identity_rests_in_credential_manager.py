"""The identity key rests in the real Credential Manager, non-roaming, and never
reaches the nest.

`docs/features/identity-protected-on-this-device.md` outcomes 3 (the key is kept in
the device's own protected store, and the search below finds no unprotected copy on
disk) and 4;
`docs/goal/architecture/apps/common.md` § Credential storage — the Windows row
(**travels sealed**: every row written ``CRED_PERSIST_LOCAL_MACHINE``, per-user and
never roamed to the person's other PCs) and *What a device's own backup carries*;
mechanics `docs/goal/architecture/apps/windows.md` § Key storage. tui on windows
rides the same arm, so it is the same test.

Real-session category (convention 12's Windows arm), for one reason: the subject IS
the row Credential Manager holds, and Credential Manager is one set per user with no
private instance to run instead. linux's witness starts a private keyring daemon and
iOS's uses the seat simulator's own keychain; here the only store there is, is the
box's, so an ordinary launch must never reach it (convention 10) and this one must.
What the unit layer already pins (``SharedSecretBackendTests``: the name, type,
persistence and bytes of a row the backend writes) stops short of the fact only this
adds: the row a real onboarding leaves behind is that row.

Borrow and give back, by the namespace alone. Every launch here names a fresh
namespace of its own (the drivers refuse anything else,
`helpers/windows_credman.py`), and the fixture sweeps it whether the test passed or
not, failing loudly if a row stays: a leaked row counts against a per-user budget that
every program on the box shares. The venue's machine-wide lock is what makes the
start-of-test sweep of this file's stem safe, and that sweep is how a run killed
before its ``finally`` gives its rows back.

The launch relocates ``%LOCALAPPDATA%``, so the store's cross-process lock is a
per-launch file and these writes are not serialized against other Fauna writers on
the box. Credential Manager loses overlapping updates at about one in a hundred
(`apps/windows.md` § Key storage), so a one-off red here is a likely overlap: rerun
it first. The relocation stays: convention 10(c) records why un-relocating it is
destructive.

The observables:

1. **Where it rests.** After a real sign-in — import a registered key, the real handle
   check against a live nest, Continue — ``CredReadW`` reads the identity row back
   holding the secret (the positive control: a launch still on the file arm finds
   nothing here), a generic credential persisted ``LOCAL_MACHINE``, and so is every
   other row of the namespace, since ``ENTERPRISE`` would roam.
2. **Nothing beside it.** With the app stopped, so no open handle hides a file, no file
   under the launch's profile dirs holds the secret in any encoding, while the actor's
   public key is found there (the control that the search reads real bytes).
3. **What leaves.** The linux/tui arm's nest-side search: no file of the nest's state
   holds the secret in any encoding, while the actor's public key is found there.
"""

from __future__ import annotations

import contextlib
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from helpers.admin_wire import admit_adult
from common.keyring import unique_namespace
from common.launch_harness import sign_in_through_the_wizard
from conftest import _seeded_environment, get_available_apps
from drivers import create_driver
from helpers import windows_credman as credman
from tests.test_identity_key_stays_on_the_device import _encodings, _files_holding
from tests.test_onboarding_launch_routing_smoke import smoke_handled_nest  # noqa: F401 (fixture)

pytestmark = [
    # Credential Manager is a Windows store: the subject exists on that OS alone
    # (e2e-conventions.md convention 7, the fourth skip kind).
    pytest.mark.host_platform("windows"),
    pytest.mark.tier_3,
    pytest.mark.feature("identity-protected-on-this-device"),
]

#: Every namespace this file mints starts here, and nothing else on the box does.
STEM = "fauna-e2e-stays-credman"


@pytest.fixture(params=["windows", "tui"])
def credman_client(request):
    """The two columns whose store, on this machine, is Credential Manager; the client
    id lands in the test name for conftest's ``--app`` filter."""
    if request.param not in get_available_apps():
        pytest.skip(f"{request.param} is not in this run's app set")
    return request.param


@pytest.fixture
def namespace(credman_client):
    """A namespace of this launch's own, given back whatever the test did."""
    credman.sweep(f"{STEM}-")
    name = unique_namespace(f"{STEM}-{credman_client}")
    try:
        yield name
    finally:
        credman.sweep(name)


def _launch_config(client, app_path, nest, namespace, home, request) -> dict:
    environment = _seeded_environment(request, nest)
    if client == "windows":
        # Both roots pinned under `home`, as production lays them out, so they
        # outlive the driver: the search below runs with the app stopped.
        local_appdata = home / "localappdata"
        return {
            "app_path": app_path, "url": nest["url"], "use_real_keyring": True,
            "keyring_app": namespace, "local_appdata": str(local_appdata),
            "data_dir": str(local_appdata / "Fauna"), "environment": environment,
        }
    # tui: the production store resolution, unforced; on windows the probe finds
    # Credential Manager, and `LOCALAPPDATA` relocates under this pinned base.
    return {
        "app_path": app_path, "url": nest["url"], "probed_store": True,
        "keyring_app": namespace, "xdg_base": str(home), "environment": environment,
    }


def test_the_identity_key_rests_non_roaming_in_credential_manager_and_never_reaches_the_nest(
    credman_client, namespace, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    client = credman_client
    nest = smoke_handled_nest
    actor = admit_adult(nest, f"staysput{client}win", domain=nest["authority"], token=True)
    secret = bytes(actor["signing_key"])
    public = SigningKey(secret).verify_key.encode()
    home = tmp_path / "home"
    home.mkdir()
    target = f"{namespace}/fauna/{public.hex()}/secret"

    driver = create_driver(client)
    app_path = request.getfixturevalue(f"{client}_app_path")
    driver.launch(_launch_config(client, app_path, nest, namespace, home, request))
    try:
        sign_in_through_the_wizard(driver, actor, nest)

        # ── 1. Where it rests: the real store, on this machine alone. ─────────
        row = credman.read_row(target)
        assert row is not None and any(n in row.blob for n in _encodings(secret)), (
            f"Credential Manager's {target} does not hold the identity secret — the "
            "positive control for everything below failed, so this launch may never "
            f"have reached the real store (rows there: "
            f"{[r.target for r in credman.rows_under(namespace)]})"
        )
        assert row.type == credman.CRED_TYPE_GENERIC, f"unexpected row type: {row.type}"
        assert row.persist == credman.CRED_PERSIST_LOCAL_MACHINE, (
            f"the identity row is persisted {row.persist}, not LOCAL_MACHINE "
            f"({credman.CRED_PERSIST_LOCAL_MACHINE}); ENTERPRISE "
            f"({credman.CRED_PERSIST_ENTERPRISE}) roams to the person's other PCs"
        )
        roaming = [
            (r.target, r.persist) for r in credman.rows_under(namespace)
            if r.persist != credman.CRED_PERSIST_LOCAL_MACHINE
        ]
        assert not roaming, f"rows of this namespace that are not LOCAL_MACHINE: {roaming}"
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()

    # ── 2. Nothing beside it: the profile a backup would carry, app stopped. ──
    assert _files_holding(home, [public, public.hex().encode()]), (
        "the actor's public key is nowhere in the launch's profile — the search is "
        "not reading its real bytes, so its finding below means nothing"
    )
    local = _files_holding(home, _encodings(secret))
    assert not local, (
        f"the identity secret rests readable in the launch's profile, in: {local} — "
        "outside the store that keeps it out of a backup"
    )

    # ── 3. What leaves: nothing of the nest's state holds it. ─────────────────
    nest_root = Path(nest["tmp_dir"])
    assert _files_holding(nest_root, [public, public.hex().encode()]), (
        "the actor's public key is nowhere in the nest's state — the search is not "
        "reading the nest's real bytes, so its finding below means nothing"
    )
    remote = _files_holding(nest_root, _encodings(secret))
    assert not remote, (
        f"the identity secret reached the nest, in: {remote} — an app path sent it off "
        "the device"
    )

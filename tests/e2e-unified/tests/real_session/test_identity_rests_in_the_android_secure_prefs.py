"""The identity key rests sealed in android's production store, nowhere else on the
device, and never reaches the nest.

`docs/features/identity-protected-on-this-device.md` outcome 3 (the key is kept in
the device's own protected store, and the search below finds no unprotected copy);
`docs/goal/architecture/apps/common.md` § Credential storage — the Android row
(`EncryptedSharedPreferences` under a Keystore master key, **stays**); mechanics
`docs/goal/architecture/apps/android.md` § Secret Storage.

Real-session category (convention 12's Android arm), for one reason: the subject IS
the file the production store writes, and every ordinary android launch rests its
identity in the e2e file backend instead (convention 10), so no ordinary test can
observe it. The app needs no seam for this: the store is what it uses whenever the
launch intent names no credential file, and the driver's ``production_store`` launch
is what keeps that file absent. What the unit layer pins
(`test_android_driver_production_store.py`: no seed reaches the bridge, the store is
wiped and given back) stops short of the fact only this adds: the file a real
onboarding leaves behind is that store, and the secret is in nothing else.

Borrow and give back: the driver wipes the two encrypted stores before the launch
and after it, so a run killed before its teardown gives them back at the next. The
device is a singleton and its store one per package, which is why the category's
machine-wide lock is held, and why the guard refuses anything but an emulator
(`tests/real_session/conftest.py`). The outcome-4 column, the installed package's
backup posture, stays an ordinary test
(`test_identity_key_stays_on_the_device.py`), which this file's searches come from.

The observables:

1. **Where it rests.** After a real sign-in — import a registered key, the real
   handle check against a live nest, Continue — ``fauna_secure_prefs.xml`` exists
   with Tink's two keyset rows (what makes it the encrypted store, not a plain
   prefs file) and rows of the app's beside them, and the e2e credential file does
   not exist (the positive control: a launch still on the file arm fails here).
2. **Nothing beside it.** With the app stopped, so its databases are at rest, no file
   of the app's data directory holds the secret in any encoding, while the search
   does find the keyset's row name (the control that it reads real bytes).
3. **What leaves.** The linux/tui arm's nest-side search: no file of the nest's state
   holds the secret in any encoding, while the actor's public key is found there.
"""

from __future__ import annotations

import xml.etree.ElementTree as ElementTree
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from helpers.admin_wire import admit_adult
from common.launch_harness import make_launch_harness, sign_in_through_the_wizard
from conftest import _trust_seeder, get_available_apps
from tests.test_identity_key_stays_on_the_device import _encodings, _files_holding
from tests.test_onboarding_launch_routing_smoke import smoke_handled_nest  # noqa: F401 (fixture)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.feature("identity-protected-on-this-device"),
]

STORE = "shared_prefs/fauna_secure_prefs.xml"
E2E_CREDENTIAL_FILE = "files/e2e_credentials.json"

#: The two rows `EncryptedSharedPreferences` keeps beside the app's own: the Tink
#: keysets, themselves wrapped by the Keystore master key.
KEYSET_ROWS = frozenset({
    "__androidx_security_crypto_encrypted_prefs_key_keyset__",
    "__androidx_security_crypto_encrypted_prefs_value_keyset__",
})


@pytest.fixture(params=["android"])
def android_client(request):
    """android-only, the client id in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("android is not in this run's app set")
    return request.param


@pytest.mark.android
def test_the_android_identity_rests_only_in_the_encrypted_store_and_never_reaches_the_nest(
    android_client, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    nest = smoke_handled_nest
    actor = admit_adult(nest, "staysputandroidreal", domain=nest["authority"], token=True)
    secret = bytes(actor["signing_key"])
    public = SigningKey(secret).verify_key.encode()

    harness = make_launch_harness(
        "android", tmp_path=tmp_path,
        app_path=request.getfixturevalue("android_app_path"),
        seed_trust=_trust_seeder(request),
        extra_launch_config={"production_store": True},
    )
    driver = harness.launch(node_url=nest["url"], trust=nest)
    try:
        sign_in_through_the_wizard(driver, actor, nest)

        assert not driver.app_data_exists(E2E_CREDENTIAL_FILE), (
            f"{E2E_CREDENTIAL_FILE} exists, so this launch rested its identity in the "
            "e2e file backend and never reached the production store"
        )

        # With the app stopped: its databases are at rest, and no open handle hides
        # a page. The bridge is not needed for the rest of this test; adb reads it.
        driver.quit_app()
        data = driver.pull_app_data(tmp_path / "app-data")

        # ── 1. Where it rests: the encrypted store. ───────────────────────────
        store = data / STORE
        assert store.is_file(), (
            f"{STORE} does not exist after a real sign-in, so the identity rests "
            f"somewhere else (shared_prefs: {driver.app_data_listing('shared_prefs')})"
        )
        rows = {row.get("name") for row in ElementTree.parse(store).getroot()}
        assert KEYSET_ROWS <= rows, (
            f"{STORE} has no Tink keysets, so it is not the encrypted store: "
            f"{len(rows)} rows"
        )
        assert rows - KEYSET_ROWS, (
            f"{STORE} holds only its keysets, so the sign-in wrote nothing to it"
        )

        # ── 2. Nothing beside it: every byte the app rests. ───────────────────
        assert STORE in _files_holding(data, [min(KEYSET_ROWS).encode()]), (
            "the keyset row is not found in the store that was just parsed — the "
            "search is not reading real bytes, so its finding below means nothing"
        )
        local = _files_holding(data, _encodings(secret))
        assert not local, (
            f"the identity secret rests readable in the app's data, in: {local} — "
            "outside the store that keeps it sealed"
        )

        # ── 3. What leaves: nothing of the nest's state holds it. ─────────────
        nest_root = Path(nest["tmp_dir"])
        assert _files_holding(nest_root, [public, public.hex().encode()]), (
            "the actor's public key is nowhere in the nest's state — the search is "
            "not reading the nest's real bytes, so its finding below means nothing"
        )
        remote = _files_holding(nest_root, _encodings(secret))
        assert not remote, (
            f"the identity secret reached the nest, in: {remote} — an app path sent it "
            "off the device"
        )
    finally:
        harness.teardown()

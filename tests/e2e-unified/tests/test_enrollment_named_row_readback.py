"""tier_1 pins for the named-row barrier's per-app read-back —
no driver launch, no app, no nest.

`helpers/enrollment.await_named_row` is the causal barrier every un-forced
sign-in rests on: "this sign-in has registered its NAMED `sync_devices` row"
(`apps/sync-agent-credentials.md` § Implementation status today → *The derived
named-row id*; `e2e-conventions.md` convention 10). It answers that question
from two at-rest shapes — linux and tui read the account-scoped sqlite
`device.db`, macOS, iOS, windows, android and web a row in the app's own
credential store (web's is `localStorage`, android's an on-device file read over
the bridge) — and it must never answer from a slot that is not this actor's,
because a barrier latched on the wrong id greens the accrual it guards.

Three things are pinned, each red-on-regression against a specific way this can
go wrong:

  * **the apple arm reads the per-account slot alone**, like windows'. apple
    retired its pre-registry single slot — its session patch enrols the identity
    in the registry, so the id always lands in `fauna/{actor}/device_id`
    (`long-term-store.md` § Downgrade mirror + abandoned-append recovery). A
    native `device_id` row is never read: it would latch the barrier on whichever
    account was last active, greening the exact accrual the derivation exists to
    stop.
  * **android reads the same secret-store arm, over the bridge.** Its e2e
    credential file is on-device, and `GET /credentials` is the one host-side
    view of it; the account-store's rows ride the foreign seam into that same
    file, prefixed, as on iOS.
  * **no app is left without a reader**, so a driver that reaches the gate has
    not launched — a harness error, never a skip.
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers import create_driver  # noqa: E402
from helpers import enrollment  # noqa: E402

pytestmark = pytest.mark.tier_1

_ACTOR = "aa" * 32
_OTHER = "bb" * 32
_ID = "c3" * 32
_OTHER_ID = "d4" * 32
_SECRET = "11" * 32


def _index(active):
    return json.dumps({"active": active, "accounts": [], "schema_version": 1})


def _apple_driver(tmp_path, rows, client="macos"):
    """A constructed (never launched) apple driver whose store holds `rows` — the
    `_resolved_*` pair is the cross-driver contract `attach_cred_store`
    discovers the store through."""
    driver = create_driver(client)
    cred_dir = tmp_path / "credentials"
    cred_dir.mkdir(exist_ok=True)
    (cred_dir / "keychain.json").write_text(json.dumps(rows))
    driver._resolved_credential_dir = str(cred_dir)
    driver._resolved_keyring_app = "keychain"
    return driver


def test_the_apple_arm_reads_the_per_account_device_id_slot(tmp_path):
    driver = _apple_driver(tmp_path, {f"fauna/{_ACTOR}/device_id": _ID})

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own == _ID
    assert where["key"] == f"fauna/{_ACTOR}/device_id"


@pytest.mark.parametrize("client", ["macos", "ios"])
@pytest.mark.parametrize("with_index", [False, True])
def test_the_apple_arm_never_answers_from_a_legacy_slot(client, with_index, tmp_path):
    """apple retired its pre-registry single slot (the windows twin is
    `test_windows_never_answers_from_a_legacy_slot`): the un-migrated shape this arm
    once honoured — a native `device_id` row beside a `secret_key` row — is refused,
    with or without an index naming this actor active."""
    rows = {"device_id": _ID, "secret_key": _SECRET}
    if with_index:
        rows["fauna/index"] = _index(_ACTOR)
    driver = _apple_driver(tmp_path, rows, client=client)

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own is None
    assert "source" not in where


def test_the_apple_arm_never_answers_from_another_actors_slot(tmp_path):
    driver = _apple_driver(tmp_path, {f"fauna/{_OTHER}/device_id": _OTHER_ID})

    own, _ = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own is None


def test_the_apple_arm_diagnoses_the_slot_it_looked_for(tmp_path):
    """Convention 6: the failure reads itself. A barrier that timed out must say
    which key it watched and what the store held beside it."""
    driver = _apple_driver(tmp_path, {f"fauna/{_ACTOR}/secret": "seed"})

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own is None
    assert where["key"] == f"fauna/{_ACTOR}/device_id"
    assert where["rows"] == [f"fauna/{_ACTOR}/secret"]
    assert where["keychain_dir"] == driver._resolved_credential_dir


def test_the_apple_arm_reads_back_live(tmp_path):
    """The reader is polled: it must re-read the file, not a map bound once. The
    app writes the slot at its own pace behind the barrier."""
    driver = _apple_driver(tmp_path, {})
    read = enrollment._own_device_id_reader(driver, _ACTOR)
    assert read()[0] is None

    path = os.path.join(driver._resolved_credential_dir, "keychain.json")
    with open(path, "w") as f:
        json.dump({f"fauna/{_ACTOR}/device_id": _ID}, f)

    assert read()[0] == _ID


@pytest.mark.parametrize(
    "client,relpath",
    [
        ("linux", ("fauna", "sync", _ACTOR, "device.db")),
        ("tui", ("fauna-tui", _ACTOR, "device.db")),
    ],
)
def test_the_file_backed_arm_still_reads_the_device_db(client, relpath, tmp_path):
    """The other at-rest shape, unchanged by apple's arm landing beside it: the
    id is raw bytes in `device_identity`, and the barrier compares hex."""
    driver = create_driver(client)
    driver._xdg_config = str(tmp_path)
    path = os.path.join(str(tmp_path), *relpath)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with sqlite3.connect(path) as db:
        db.execute("CREATE TABLE device_identity (key TEXT PRIMARY KEY, value BLOB)")
        db.execute(
            "INSERT INTO device_identity VALUES ('device_id', ?)", (bytes.fromhex(_ID),)
        )

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own == _ID
    assert where["device_db"] == path


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_the_file_backed_arm_reports_an_absent_device_db(client, tmp_path):
    driver = create_driver(client)
    driver._xdg_config = str(tmp_path)

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own is None
    assert "read" in where


def _windows_driver(tmp_path, rows):
    """The windows twin of :func:`_apple_driver`, and deliberately the same two
    lines: windows' `CredentialStore.Logical` resolves to
    `FileSecretBackend(FAUNA_E2E_CREDENTIAL_DIR, FAUNA_KEYRING_APP)` under e2e,
    which is the very `_resolved_*` pair `attach_cred_store` discovers. The
    namespace differs from apple's fixed `keychain`; nothing else does."""
    driver = create_driver("windows")
    cred_dir = tmp_path / "credentials"
    cred_dir.mkdir(exist_ok=True)
    (cred_dir / "fauna-e2e.json").write_text(json.dumps(rows))
    driver._resolved_credential_dir = str(cred_dir)
    driver._resolved_keyring_app = "fauna-e2e"
    return driver


def test_windows_reads_the_same_secret_store_arm_as_apple(tmp_path):
    """windows has no arm of its own, and this is what says so: the id it
    persists through the shared `FfiAccountRegistry.DeviceIdForActor` lands
    under shared Rust's own logical key, in a store the same adapter reads. A
    windows-shaped copy of the apple arm would be a second spelling of one rule."""
    driver = _windows_driver(tmp_path, {f"fauna/{_ACTOR}/device_id": _ID})

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own == _ID
    assert where["key"] == f"fauna/{_ACTOR}/device_id"


def test_windows_never_answers_from_a_legacy_slot(tmp_path):
    """windows retired its pre-registry single slot: its session patch enrolls
    the identity in the registry, so the id always lands in the per-account slot
    (`long-term-store.md` § Downgrade mirror + abandoned-append recovery). The
    un-migrated legacy shape is refused here, exactly as on apple."""
    driver = _windows_driver(tmp_path, {"device_id": _ID, "secret_key": _SECRET})

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own is None
    assert "source" not in where


def _android_driver(rows):
    """A constructed (never launched) android driver whose bridge answers
    `rows` — `GET /credentials` (`AndroidBridgeDriver.credential_map`) is the one
    host-side view of the on-device e2e credential file, and `attach_cred_store`
    binds `AndroidCredStore` over it. A callable, so a test can change what the
    device holds between reads."""
    driver = create_driver("android")
    driver.credential_map = rows if callable(rows) else (lambda: dict(rows))
    return driver


def test_android_reads_the_same_secret_store_arm_as_apple():
    """android has no arm of its own either: `SecureStorage.deviceId` persists
    through the shared `FfiAccountRegistry.deviceIdForActor`, so the id lands
    under shared Rust's own logical key in the one e2e credential file, which
    the same adapter reads — over the bridge instead of a host path."""
    driver = _android_driver({f"fauna/{_ACTOR}/device_id": _ID})

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own == _ID
    assert where["key"] == f"fauna/{_ACTOR}/device_id"


def test_android_never_answers_from_another_actors_or_the_legacy_slot():
    """The per-account slot alone: the install-wide `legacy/device_id` mirror
    is handed to an actor only behind shared Rust's `holds_legacy_slot`, so an
    unguarded read would report one account's id for another."""
    driver = _android_driver(
        {
            f"fauna/{_OTHER}/device_id": _OTHER_ID,
            "legacy/device_id": _OTHER_ID,
            "fauna/index": _index(_ACTOR),
        }
    )

    own, where = enrollment._own_device_id_reader(driver, _ACTOR)()

    assert own is None
    assert "source" not in where


def test_android_reads_back_live():
    """Polled through the bridge on every read, never a map bound once."""
    held: dict = {}
    driver = _android_driver(lambda: dict(held))
    read = enrollment._own_device_id_reader(driver, _ACTOR)
    assert read()[0] is None

    held[f"fauna/{_ACTOR}/device_id"] = _ID

    assert read()[0] == _ID


def test_androids_account_store_is_the_foreign_seam_rows_of_the_same_file():
    """android has no native Rust keyring arm, so the `fauna-account-store`
    namespace rides the foreign seam into the one e2e credential file beside the
    identity rows, each key prefixed `fauna-account-store/` (iOS's shape). The
    attach strips the prefix and drops everything else, so `await_enrollment`
    reads the writer key and the latch exactly as on a file-of-its-own app —
    and a sign-out that erased the identity rows but left these is still seen."""
    from common.cred_store import attach_account_store

    driver = _android_driver(
        {
            "fauna/index": _index(_ACTOR),
            f"fauna/{_ACTOR}/secret": "seed",
            f"fauna/{_ACTOR}/device_id": _ID,
            f"fauna-account-store/{_ACTOR}": "writer",
            f"fauna-account-store/{_ACTOR}/grant-registered": f"{_ID}:enc",
        }
    )

    slot = attach_account_store("android", driver).read_map()

    assert slot == {_ACTOR: "writer", f"{_ACTOR}/grant-registered": f"{_ID}:enc"}
    assert enrollment.latched_row_in(slot, _ACTOR) == _ID


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_an_unlaunched_file_backed_driver_is_a_harness_error_not_a_skip(client):
    """Every app has a read-back arm, so the reader has no unbuilt surface left
    to declare: a linux/tui driver that recorded no config home has simply not
    launched, and skipping would hide that harness bug behind a green run."""
    driver = create_driver(client)

    with pytest.raises(RuntimeError, match="no config home"):
        enrollment._own_device_id_reader(driver, _ACTOR)

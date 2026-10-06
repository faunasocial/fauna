"""tier_1 unit tests for the relaunch-pin lifecycle on the bridge drivers —
no driver launch, no app, no nest.

Regression guard for the session-scoped `_driver_cache` pin leak: `_launch_config`
is a session-scoped dict, and `preserve_state_across_relaunch()` writes the three
client-local-store keys (`xdg_base` / `credential_dir` / `keyring_app`) into it so a
subsequent `recover()` reuses this launch's dirs. Nothing un-pinned them at the
per-test boundary, so once a factory-reset journey preserved, EVERY later test's
relaunch silently reused that store. The fix clears the pin at the top of
`reset()` (the per-test boundary), the native analogue of macOS clearing
`_preserved_cred_dir` in its own reset() (tracked internally).

Listed in conftest's _CLIENT_INDEPENDENT_FILES so it never acquires a client
parametrization.
"""

import json
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers import create_driver  # noqa: E402
from drivers.tui import build_launch_env  # noqa: E402

pytestmark = pytest.mark.tier_1

_PIN_KEYS = ("xdg_base", "credential_dir", "keyring_app")


def _driver_with_resolved_launch(client):
    """A constructed (never launched) bridge driver with the attributes
    `preserve_state_across_relaunch()` reads — as if a real `launch()` had run."""
    driver = create_driver(client)
    driver._launch_config = {"app_path": "/nonexistent"}
    driver._resolved_xdg_base = "/tmp/fauna-e2e-xdgbase"
    driver._resolved_credential_dir = "/tmp/fauna-e2e-creds"
    driver._resolved_keyring_app = "fauna-e2e-agent-12345"
    return driver


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_preserve_pins_the_three_store_keys(client):
    driver = _driver_with_resolved_launch(client)
    assert not any(k in driver._launch_config for k in _PIN_KEYS)

    assert driver.preserve_state_across_relaunch() is True
    for k in _PIN_KEYS:
        assert k in driver._launch_config, f"{client}: preserve() did not pin {k}"


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_clear_relaunch_pin_unpins_all_three_keys(client):
    driver = _driver_with_resolved_launch(client)
    driver.preserve_state_across_relaunch()

    driver._clear_relaunch_pin()

    for k in _PIN_KEYS:
        assert k not in driver._launch_config, (
            f"{client}: {k} leaked past the per-test un-pin — a prior journey's "
            "store would be reused by this relaunch"
        )


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_clear_relaunch_pin_is_idempotent_and_preserves_other_config(client):
    driver = _driver_with_resolved_launch(client)
    # No pin present, and an unrelated key that must survive the un-pin.
    driver._clear_relaunch_pin()
    assert driver._launch_config == {"app_path": "/nonexistent"}
    # And a later launch can re-pin cleanly (a preserving journey after a
    # non-preserving one).
    assert driver.preserve_state_across_relaunch() is True
    assert all(k in driver._launch_config for k in _PIN_KEYS)


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_reset_clears_the_relaunch_pin(client):
    """The per-test boundary itself un-pins: `reset()` calls `_clear_relaunch_pin()`
    before touching the app, so the next relaunch is fresh unless the test re-pins.

    Stub the two transport calls `reset()` makes so it settles immediately without
    a running app; the assertion is purely that the pin is gone afterward.
    """
    driver = _driver_with_resolved_launch(client)
    driver.preserve_state_across_relaunch()

    captured = {}

    def _fake_post(path, payload):
        captured["id"] = payload["id"]
        return {}

    def _fake_get(path):
        return {
            "last_command_id": captured.get("id"),
            "ready": True,
            "state": {"session": {"authenticated": False}},
        }

    driver._post = _fake_post
    driver._get = _fake_get
    driver.reset(timeout=5.0)

    for k in _PIN_KEYS:
        assert k not in driver._launch_config, (
            f"{client}: reset() did not un-pin {k}"
        )


# ── tui on macOS: the nest-identity pin store rests under HOME ──────────────
#
# tui installs its `DiskPinStore` at shared Rust's `install_scoped_trust_home()`
# (`session::install_disk_pin_store`): `$XDG_CONFIG_HOME/fauna` on linux — under
# the `xdg_base` the pin already keeps — but `<HOME>/Library/Application
# Support/Fauna/trust` on macOS, and `build_launch_env` gives every macOS launch
# its OWN throwaway HOME. So on macOS the pin has to name HOME as well, or a
# relaunch reads an empty trust dir: the seeded pin is gone, the app TOFU-pins
# afresh and enters as if nothing had changed
# (`test_nest_identity_pin.py::test_changed_nest_identity_warns_then_recovers`).
# Driven through the explicit `sys_platform` parameter, so the darwin arm is
# proven on every machine (convention 7).


def _tui_launch(driver, config, tmp_path, n, sys_platform):
    """The env + store-bookkeeping half of one tui `launch()`, in `launch()`'s
    own order: build the child env in this launch's fresh tmpdir, then record
    where the client-local store landed."""
    tmp = tmp_path / f"launch-{n}"
    tmp.mkdir()
    driver._launch_config = config
    env = build_launch_env(config, 40000 + n, str(tmp), sys_platform=sys_platform)
    driver._remember_launch_store(config, env, str(tmp), sys_platform=sys_platform)
    return env


def test_tui_preserve_keeps_the_macos_home_the_pin_store_rests_under(tmp_path):
    driver = create_driver("tui")
    config = {"app_path": "/nonexistent"}
    first = _tui_launch(driver, config, tmp_path, 1, "darwin")

    assert driver.preserve_state_across_relaunch() is True
    second = _tui_launch(driver, config, tmp_path, 2, "darwin")

    assert second["HOME"] == first["HOME"] == str(tmp_path / "launch-1" / "home"), (
        "the relaunch got a HOME of its own — the pin store under it "
        "(`<HOME>/Library/Application Support/Fauna/trust`) starts empty, so a "
        "seeded nest-identity pin never reaches the relaunched process"
    )
    assert second["CFFIXED_USER_HOME"] == first["CFFIXED_USER_HOME"]


def test_tui_unpinned_macos_relaunch_still_gets_a_fresh_home(tmp_path):
    """The default contract is unchanged: no preserve(), no carried HOME."""
    driver = create_driver("tui")
    config = {"app_path": "/nonexistent"}
    first = _tui_launch(driver, config, tmp_path, 1, "darwin")

    second = _tui_launch(driver, config, tmp_path, 2, "darwin")

    assert second["HOME"] != first["HOME"]


def test_tui_preserve_leaves_home_alone_off_macos(tmp_path):
    """Only darwin relocates HOME, so only darwin has one of the launch's own to
    pin: linux's pin store rides `XDG_CONFIG_HOME` (already pinned), and pinning a
    `home` there would be an unverified change to the seat with the long green
    record."""
    driver = create_driver("tui")
    config = {"app_path": "/nonexistent"}
    _tui_launch(driver, config, tmp_path, 1, "linux")

    assert driver.preserve_state_across_relaunch() is True

    assert "home" not in config


def test_tui_clear_relaunch_pin_unpins_the_macos_home(tmp_path):
    driver = create_driver("tui")
    config = {"app_path": "/nonexistent"}
    _tui_launch(driver, config, tmp_path, 1, "darwin")
    driver.preserve_state_across_relaunch()
    assert "home" in config

    driver._clear_relaunch_pin()

    assert "home" not in config, (
        "the pinned HOME leaked past the per-test un-pin — the next test's "
        "relaunch would read this journey's pin store"
    )


def test_tui_a_second_preserve_still_lets_the_reset_unpin_the_home(tmp_path):
    """`preserve()` twice in one journey (a relaunch between them re-resolves
    `home` from the very pin the first call wrote) must not forget that the pin
    is the driver's own — else the per-test reset leaves it in the session-scoped
    launch config."""
    driver = create_driver("tui")
    config = {"app_path": "/nonexistent"}
    _tui_launch(driver, config, tmp_path, 1, "darwin")
    driver.preserve_state_across_relaunch()
    _tui_launch(driver, config, tmp_path, 2, "darwin")
    driver.preserve_state_across_relaunch()

    driver._clear_relaunch_pin()

    assert "home" not in config


def test_tui_clear_relaunch_pin_never_unpins_a_home_the_caller_supplied(tmp_path):
    chosen = str(tmp_path / "caller-home")
    driver = create_driver("tui")
    config = {"app_path": "/nonexistent", "home": chosen}
    _tui_launch(driver, config, tmp_path, 1, "darwin")
    driver.preserve_state_across_relaunch()

    driver._clear_relaunch_pin()

    assert config["home"] == chosen


_ACTOR = "aa" * 32


def _session_block():
    return {
        "authenticated": True,
        "node_url": "http://127.0.0.1:1",
        "secret_hex": "11" * 32,
        "actor_id": _ACTOR,
        "handle": "restarter",
    }


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_pinned_hard_reload_waits_for_the_auto_login_instead_of_replaying(client):
    """With the store pinned, `hard_reload()` must NOT replay the login once the
    relaunched app's own auto-login lands — the replay raced that auto-login
    into a second concurrent same-actor login, and the loser's conversations
    engine hit the winner's role lock (`StateServedElsewhere`): a standing
    refusal that bricked the rail on tui and blanked the conversations page on
    linux. A real user's restart has no re-login at all.
    """
    driver = _driver_with_resolved_launch(client)
    driver.preserve_state_across_relaunch()
    driver._last_session = _session_block()
    driver.recover = lambda: True
    driver.get_state = lambda key: {
        "session.authenticated": True,
        "session.actor_id": _ACTOR,
    }[key]
    replays = []
    driver.set_state = replays.append

    driver.hard_reload()

    assert replays == [], (
        f"{client}: the auto-restored session was live, yet hard_reload still "
        "replayed the login — the double-login race this contract split removed"
    )


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_pinned_hard_reload_falls_back_to_the_replay_when_no_auto_login_lands(client):
    """The pin promises nothing about the credentials themselves: a store that
    did not carry them relaunches unauthenticated, and past the ceiling the
    unpinned contract (replay) must take over rather than leaving the app
    logged out."""
    driver = _driver_with_resolved_launch(client)
    driver.preserve_state_across_relaunch()
    driver.AUTO_RELOGIN_CEILING_S = 0.6  # shrink the named budget for the test
    driver._last_session = _session_block()
    driver.recover = lambda: True
    driver.get_state = lambda key: False
    replays = []
    driver.set_state = replays.append

    driver.hard_reload()

    assert len(replays) == 1 and replays[0]["session"] == _session_block(), (
        f"{client}: with no auto-login inside the ceiling the replay is owed"
    )


@pytest.mark.parametrize("client", ["linux", "tui"])
@pytest.mark.parametrize("pinned", [False, True], ids=["unpinned", "pinned"])
def test_reset_forgets_the_session_a_later_hard_reload_would_replay(client, pinned):
    """A session one test injected must not outlive the per-test `reset()`.

    The driver is session-scoped, so a cached `_last_session` used to cross into
    the next test: a test that never signed in and then hard-reloaded had the
    earlier test's login replayed over its own identity — at once unpinned, and
    after the auto-login ceiling when it had pinned its store to prove client-side
    durability. `reset()` returns the app to factory state, so nothing is left to
    replay.
    """
    driver = _driver_with_resolved_launch(client)
    captured = {}

    def _fake_post(path, payload):
        captured["id"] = payload["id"]
        return {}

    driver._post = _fake_post
    driver._get = lambda path: {
        "last_command_id": captured.get("id"),
        "ready": True,
        "state": {"session": {"authenticated": False}},
    }
    driver._last_session = _session_block()  # the previous test's sign-in

    driver.reset(timeout=5.0)

    if pinned:
        driver.preserve_state_across_relaunch()
        driver.AUTO_RELOGIN_CEILING_S = 0.6  # shrink the named budget for the test
    driver.recover = lambda: True
    driver.get_state = lambda key: False
    replays = []
    driver.set_state = replays.append

    driver.hard_reload()

    assert replays == [], (
        f"{client}: hard_reload replayed a session the per-test reset should have "
        "forgotten — this test never signed in, yet was logged in as another test's actor"
    )


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_unpinned_hard_reload_replays_the_login_immediately(client):
    """The default contract is unchanged: no pin, no wait — the relaunched
    process starts unauthenticated and the replay is the login."""
    driver = _driver_with_resolved_launch(client)
    driver._last_session = _session_block()
    driver.recover = lambda: True

    def _no_state_reads(key):
        raise AssertionError(
            "the unpinned path must not poll session state before replaying"
        )

    driver.get_state = _no_state_reads
    replays = []
    driver.set_state = replays.append

    driver.hard_reload()

    assert len(replays) == 1 and replays[0]["session"] == _session_block()


# ── The principal-slot carry (e2e-conventions.md convention 10) ─────────────
#
# A relaunch is the same machine restarting: the signed-in actor's store-
# principal slot (the `fauna-account-store` writer key + principal bundle)
# survives it, restored at that actor's first sign-in in the new launch. These
# drive the store half of `launch()` and the real `set_state` against tmp dirs,
# with the bridge transport stubbed — no app, no nest.

_ACTOR_B = "bb" * 32
_SLOT_SUFFIXES = ("device-auth", "backup-key", "generation-keys", "grant-registered")


def _slot(actor, tag):
    """A whole per-actor slot: the bare writer-key entry and its bundle."""
    entries = {actor: f"{tag}-writer"}
    entries.update({f"{actor}/{suffix}": f"{tag}-{suffix}" for suffix in _SLOT_SUFFIXES})
    return entries


def _store_file(cred_dir, keyring_app):
    from common.cred_store import account_store_namespace

    return os.path.join(cred_dir, f"{account_store_namespace(keyring_app)}.json")


def _read(path):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return None


def _write(path, mapping):
    with open(path, "w") as f:
        json.dump(mapping, f)


def _launch_store(driver, tmp_path, n, config=None, *, cred_env=None):
    """The store half of one `launch()`, in `launch()`'s own order: begin the carry
    (harvesting the launch being replaced), then resolve this launch's store —
    the credential store AND the unified account-store root the replica half
    of the carry restores under (`<XDG_CONFIG_HOME>/fauna/sync` on linux/tui)."""
    config = {"app_path": "/nonexistent"} if config is None else config
    cred_dir = tmp_path / f"launch-{n}" / "creds"
    cred_dir.mkdir(parents=True)
    keyring_app = config.get("keyring_app") or f"fauna-e2e-agent-{40000 + n}"
    env = {
        "FAUNA_E2E_CREDENTIAL_DIR": str(cred_dir) if cred_env is None else cred_env,
        "FAUNA_KEYRING_APP": keyring_app,
    }
    driver._launch_config = config
    driver._begin_principal_slot_carry(config, env)
    driver._resolved_credential_dir = env["FAUNA_E2E_CREDENTIAL_DIR"] or None
    driver._resolved_keyring_app = keyring_app
    driver._resolved_store_root = str(tmp_path / f"launch-{n}" / "config" / "fauna" / "sync")
    return str(cred_dir), keyring_app


def _replica_dir(driver, actor=_ACTOR):
    """`actor`'s account-store dir under the driver's CURRENT store root — the
    layout shared Rust resolves (`StoreRoot::store_dir`)."""
    return os.path.join(driver._resolved_store_root, actor, "account-store")


def _write_replica(driver, actor, tag):
    """A stand-in journal under the current launch's store root: what the app's
    account runtime leaves behind for `actor` once it has assembled."""
    path = _replica_dir(driver, actor)
    os.makedirs(path, exist_ok=True)
    with open(os.path.join(path, "account-store.db"), "w") as f:
        f.write(f"{tag}-journal")
    return path


def _read_replica(driver, actor=_ACTOR):
    try:
        with open(os.path.join(_replica_dir(driver, actor), "account-store.db")) as f:
            return f.read()
    except FileNotFoundError:
        return None


def _carry_driver(client):
    """A constructed driver whose bridge transport acks every command at once."""
    driver = create_driver(client)
    sent = {}

    def _post(path, payload):
        sent["id"] = payload["id"]
        return {}

    driver._post = _post
    driver._get = lambda path: {"last_command_id": sent.get("id"), "ready": True, "state": {}}
    return driver


def _sign_in(driver, actor=_ACTOR):
    driver.set_state({"session": dict(_session_block(), actor_id=actor)})


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_relaunch_restores_only_the_signing_in_actors_slot(client, tmp_path):
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), {**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})

    second = _launch_store(driver, tmp_path, 2)
    assert _read(_store_file(*second)) is None, "the carry must not copy anything at launch"
    _sign_in(driver)

    path = _store_file(*second)
    assert _read(path) == _slot(_ACTOR, "a"), (
        f"{client}: the new store should hold exactly the signing-in actor's whole slot — "
        "another actor's entries would be a slot no sign-out in this launch can erase"
    )
    if os.name == "posix":  # a windows mode word carries only the read-only bit
        assert os.stat(path).st_mode & 0o777 == 0o600, "a slot holds key material: 0600, as the app writes it"


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_relaunch_restores_the_signing_in_actors_replica_beside_its_slot(client, tmp_path):
    """The slot never travels without its journal (refinement 11): a writer key
    restored over a fresh account-store dir is one the app abandons on sight —
    it mints a fresh writer and enrolls a new device, the accrual the carry
    exists to stop. So the actor's `account-store` dir is harvested beside the
    slot and laid down under the new launch's store root at the same sign-in,
    and only the signing-in actor's."""
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), {**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})
    _write_replica(driver, _ACTOR, "a")
    _write_replica(driver, _ACTOR_B, "b")

    _launch_store(driver, tmp_path, 2)
    assert _read_replica(driver) is None, "the carry must not copy anything at launch"
    _sign_in(driver)

    assert _read_replica(driver) == "a-journal", (
        f"{client}: the new launch's store root should hold the signing-in actor's replica "
        "beside its restored slot — a slot over a fresh dir is a key the app retires"
    )
    assert _read_replica(driver, _ACTOR_B) is None, (
        f"{client}: another actor's replica is residue no sign-out in this launch can erase"
    )
    # …and the copy is a copy: the source launch's dir is untouched by the restore.
    driver._resolved_store_root = str(tmp_path / "launch-1" / "config" / "fauna" / "sync")
    assert _read_replica(driver) == "a-journal"


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_replica_without_its_slot_is_never_carried(client, tmp_path):
    """A journal whose writer key is gone is not a machine's principal: the app
    fences it onto whatever key it mints (the lost-slot arm) or opens it fresh;
    the carry does not second-guess that with a key-less copy."""
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    partial = _slot(_ACTOR, "a")
    del partial[_ACTOR]
    _write(_store_file(*first), partial)
    _write_replica(driver, _ACTOR, "a")

    _launch_store(driver, tmp_path, 2)
    _sign_in(driver)

    assert _read_replica(driver) is None


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_replica_the_new_launch_already_holds_is_never_overwritten(client, tmp_path):
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), _slot(_ACTOR, "old"))
    _write_replica(driver, _ACTOR, "old")
    _launch_store(driver, tmp_path, 2)
    _write_replica(driver, _ACTOR, "new")

    _sign_in(driver)

    assert _read_replica(driver) == "new-journal"


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_the_restore_happens_once_per_launch(client, tmp_path):
    """A sign-out → sign-in inside ONE process mints afresh, exactly as production
    does — the carry models a restart, never a re-sign-in."""
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), _slot(_ACTOR, "a"))
    second = _launch_store(driver, tmp_path, 2)
    _sign_in(driver)

    os.remove(_store_file(*second))  # what the in-process sign-out's erase does
    _sign_in(driver)

    assert _read(_store_file(*second)) is None, (
        f"{client}: a second sign-in in the same launch re-restored the slot — that would mask "
        "an erase which dropped the slot but kept the store"
    )


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_slot_the_new_launch_already_holds_is_never_overwritten(client, tmp_path):
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), _slot(_ACTOR, "old"))
    second = _launch_store(driver, tmp_path, 2)
    _write(_store_file(*second), _slot(_ACTOR, "new"))

    _sign_in(driver)

    assert _read(_store_file(*second)) == _slot(_ACTOR, "new")


@pytest.mark.parametrize("client", ["linux", "tui"])
@pytest.mark.parametrize(
    "owned",
    [
        pytest.param({"credential_dir": "caller"}, id="caller-credential-dir"),
        pytest.param({"keyring_app": "caller-ns"}, id="caller-keyring-app"),
        pytest.param({"use_real_keyring": True}, id="use-real-keyring"),
        pytest.param({"cred_env": ""}, id="sealed-headless-store"),
    ],
)
def test_a_store_the_caller_owns_is_never_carried_into(client, owned, tmp_path):
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), _slot(_ACTOR, "a"))

    owned = dict(owned)
    cred_env = owned.pop("cred_env", None)
    config = {"app_path": "/nonexistent", **owned}
    second = _launch_store(driver, tmp_path, 2, config, cred_env=cred_env)
    _sign_in(driver)

    assert _read(_store_file(*second)) is None, (
        f"{client}: the carry wrote into a store the caller owns ({owned or 'sealed'})"
    )


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_partial_slot_is_never_harvested(client, tmp_path):
    """No writer key, no slot: a bundle without its key is not a machine's principal."""
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    partial = _slot(_ACTOR, "a")
    del partial[_ACTOR]
    _write(_store_file(*first), partial)
    second = _launch_store(driver, tmp_path, 2)

    _sign_in(driver)

    assert _read(_store_file(*second)) is None


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_patch_that_signs_nobody_in_restores_nothing(client, tmp_path):
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), _slot(_ACTOR, "a"))
    second = _launch_store(driver, tmp_path, 2)

    driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    driver.set_state({"session": dict(_session_block(), authenticated=False)})

    assert _read(_store_file(*second)) is None
    # …and those non-sign-ins did not spend the launch's one restore.
    _sign_in(driver)
    assert _read(_store_file(*second)) == _slot(_ACTOR, "a")


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_the_newest_launch_holding_an_actor_wins(client, tmp_path):
    """Across several relaunches the restored slot is the one the most recent launch
    that held the actor wrote — a re-minted key (a succession, a sign-out → sign-in)
    replaces the older one; a launch that never held the actor forgets nothing."""
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), _slot(_ACTOR, "v1"))
    second = _launch_store(driver, tmp_path, 2)
    _write(_store_file(*second), _slot(_ACTOR, "v2"))
    third = _launch_store(driver, tmp_path, 3)
    _write(_store_file(*third), _slot(_ACTOR_B, "b"))  # this launch never held _ACTOR
    fourth = _launch_store(driver, tmp_path, 4)

    _sign_in(driver)

    assert _read(_store_file(*fourth)) == _slot(_ACTOR, "v2")


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_launch_that_signed_an_actor_out_leaves_it_nothing_to_restore(client, tmp_path):
    """A sign-out ends the machine's standing for the account — the app retires
    the enrollment nest-side and erases the slot — so the next launch must come
    up as a machine that holds no key for that actor, exactly as a real one does
    after a sign-out and a restart. Restoring the copy an EARLIER launch left
    would hand the app a credential the nest has already forgotten: principal
    succession mints a successor on a fresh row, and the assembly's revoked-key
    handshake retries stretch the sign-in long enough for the next reset's stop
    to lapse and strand that row (measured in the 2026-09-20 `--app linux`
    sweep; windows met the same mechanism through its signed-in relaunch)."""
    driver = _carry_driver(client)
    first = _launch_store(driver, tmp_path, 1)
    _write(_store_file(*first), _slot(_ACTOR, "v1"))
    _write_replica(driver, _ACTOR, "v1")
    second = _launch_store(driver, tmp_path, 2)
    _sign_in(driver)  # restores v1 and its replica into launch 2
    # …then the in-process sign-out: the erase takes the slot and the replica.
    os.remove(_store_file(*second))
    import shutil

    shutil.rmtree(_replica_dir(driver))
    third = _launch_store(driver, tmp_path, 3)

    _sign_in(driver)

    assert _read(_store_file(*third)) is None, (
        f"{client}: the carry restored a slot the previous launch had signed out — a key "
        "whose enrollment the sign-out already retired nest-side"
    )
    assert _read_replica(driver) is None, (
        f"{client}: the replica travels with its key or not at all"
    )


# ── The install-device-secret carry (convention 10, ruled 2026-09-20) ───────
#
# The other thing a relaunch keeps: the install-scoped secret every named sync
# device row's id is derived from (`sync-agent-credentials.md` § Credential
# model). It names no account, so it is laid down AT launch, before the app
# starts — never at a sign-in. These drive each carrying driver's real
# `launch()` and `recover()` with the process spawn stubbed: no app, no nest.

_SECRET = bytes(range(32))

#: Where each app keeps its install-scoped sync dir under the launch's
#: `XDG_CONFIG_HOME` — linux `sync.rs::flat_sync_dir`, tui
#: `account_scope.rs::install_sync_dir_under` over `session::config_dir`.
_INSTALL_SYNC_DIR = {"linux": ("fauna", "sync"), "tui": ("fauna-tui", "sync")}


class _StandInChild:
    """A launched child that never exits and writes nothing: Popen-shaped for
    linux, pty-backend-shaped for tui."""

    pid = 0
    returncode = None

    def poll(self):
        return None

    def read(self, _n):
        return b""

    def write(self, _data):
        pass

    def close(self):
        pass


def _stub_linux_spawn(monkeypatch):
    from drivers import linux

    monkeypatch.setattr(linux.subprocess, "Popen", lambda *a, **k: _StandInChild())
    for name in ("track_process", "untrack_process", "terminate_tree"):
        monkeypatch.setattr(linux, name, lambda *a, **k: None)
    monkeypatch.setattr(linux, "_wants_private_bus", lambda config: False)
    monkeypatch.setattr(linux, "_headless_render_cmd_env", lambda cmd, env: (cmd, env))
    return linux.LinuxBridgeDriver()


def _stub_tui_spawn(monkeypatch):
    from drivers import tui

    monkeypatch.setattr(tui, "spawn_pty", lambda *a, **k: _StandInChild())
    for name in ("track_process", "untrack_process", "terminate_tree", "reap_descendants_of"):
        monkeypatch.setattr(tui, name, lambda *a, **k: None)
    return tui.TuiDriver()


@pytest.fixture
def launching_driver(monkeypatch, tmp_path):
    """A factory: `launching_driver(client)` is a driver whose `launch()` runs
    for real up to the spawn — fresh per-launch dirs under `tmp_path`, a
    stand-in child that never exits, and a health probe that answers at once.
    A factory rather than an app-parametrized fixture, because the conftest
    reads a real fixture parametrized with app names as "this item needs that
    app" and would deselect every arm outside `--app`."""
    import tempfile

    real_mkdtemp = tempfile.mkdtemp
    monkeypatch.setattr(
        tempfile, "mkdtemp", lambda *a, **k: real_mkdtemp(*a, **{**k, "dir": str(tmp_path)})
    )
    made = []

    def make(client):
        driver = {"linux": _stub_linux_spawn, "tui": _stub_tui_spawn}[client](monkeypatch)
        driver._agent_health_ok = lambda: True
        driver._get = lambda path: {"ready": True, "state": {"session": {"authenticated": False}}}
        made.append(driver)
        return driver

    yield make
    for driver in made:
        driver.teardown()


def _secret_path(driver):
    """Where the app mints the secret for the CURRENT launch: its install-scoped
    sync dir, beside (never inside) the actor scopes."""
    from helpers.app_surface import app_name

    return os.path.join(
        driver.config_home, *_INSTALL_SYNC_DIR[app_name(driver)], "install-device-secret"
    )


def _mint_secret(driver, secret=_SECRET):
    """What the app's first un-forced sign-in leaves on disk."""
    path = _secret_path(driver)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(secret)


def _read_secret(driver):
    try:
        with open(_secret_path(driver), "rb") as f:
            return f.read()
    except FileNotFoundError:
        return None


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_relaunch_carries_the_install_device_secret_at_launch(launching_driver, client):
    """A relaunch is the same machine restarting: its install secret is on disk
    when the new process starts, so an un-forced sign-in re-derives the named
    row's id instead of registering a new row per relaunch."""
    driver = launching_driver(client)
    driver.launch({"app_path": "/nonexistent"})
    first = driver.config_home
    _mint_secret(driver)

    assert driver.recover() is True
    assert driver.config_home != first, "the relaunch must still get fresh per-launch dirs"

    assert _read_secret(driver) == _SECRET, (
        f"{client}: the relaunched app started without the install device secret the previous "
        "launch minted — every un-forced sign-in after it derives a NEW device id and the "
        "nest gains one named row per module-boundary relaunch"
    )
    if os.name == "posix":  # a windows mode word carries only the read-only bit
        assert os.stat(_secret_path(driver)).st_mode & 0o777 == 0o600, (
            "the secret is key material: 0600, as shared Rust mints it"
        )


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_the_secret_survives_every_later_relaunch(launching_driver, client):
    driver = launching_driver(client)
    driver.launch({"app_path": "/nonexistent"})
    _mint_secret(driver)

    for _ in range(3):
        assert driver.recover() is True
    assert _read_secret(driver) == _SECRET


@pytest.mark.parametrize("client", ["linux", "tui"])
@pytest.mark.parametrize(
    "torn",
    [
        pytest.param(b"", id="empty-create-new-crash"),
        pytest.param(_SECRET[:31], id="short"),
        pytest.param(_SECRET + b"\x00", id="long"),
    ],
)
def test_a_torn_install_device_secret_is_never_carried(launching_driver, client, torn):
    """A secret that is not exactly 32 bytes is one every later derivation
    refuses for good (a crash between the mint's `create_new` and its write
    leaves 0 bytes). Carrying it would spread that dead install across the rest
    of a sweep; dropping it lets the next launch mint a sound one."""
    driver = launching_driver(client)
    driver.launch({"app_path": "/nonexistent"})
    _mint_secret(driver, torn)

    assert driver.recover() is True

    assert _read_secret(driver) is None, (
        f"{client}: the carry laid down a {len(torn)}-byte install secret — a torn mint no "
        "derivation can read, now copied into the next launch"
    )


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_a_launch_that_lost_its_secret_carries_none_forward(launching_driver, client):
    """Losing the secret (the data dir wiped) is the fresh-machine case, and the
    carry models the disk surviving a restart — not an older launch's copy."""
    driver = launching_driver(client)
    driver.launch({"app_path": "/nonexistent"})
    _mint_secret(driver)
    assert driver.recover() is True
    os.remove(_secret_path(driver))

    assert driver.recover() is True

    assert _read_secret(driver) is None


@pytest.mark.parametrize("client", ["linux", "tui"])
def test_the_first_launch_carries_nothing(launching_driver, client):
    driver = launching_driver(client)
    driver.launch({"app_path": "/nonexistent"})

    assert _read_secret(driver) is None


@pytest.mark.parametrize("client", ["linux", "tui"])
@pytest.mark.parametrize(
    "owned",
    [
        pytest.param({"credential_dir": "caller"}, id="caller-credential-dir"),
        pytest.param({"keyring_app": "caller-ns"}, id="caller-keyring-app"),
    ],
)
def test_a_store_the_caller_owns_gets_no_carried_secret(launching_driver, client, owned, tmp_path):
    """The fresh-machine opt-out is the principal carry's own: a caller that
    supplies its store owns what the launch starts from."""
    driver = launching_driver(client)
    driver.launch({"app_path": "/nonexistent"})
    _mint_secret(driver)
    owned = dict(owned)
    if "credential_dir" in owned:
        owned["credential_dir"] = str(tmp_path / "caller-creds")
        os.makedirs(owned["credential_dir"])
    driver._launch_config.update(owned)

    assert driver.recover() is True

    assert _read_secret(driver) is None, (
        f"{client}: the carry wrote an install secret into a launch whose store the caller owns ({owned})"
    )


def test_a_secret_the_new_launch_already_holds_is_never_overwritten(tmp_path):
    """The restore lands only where the launch holds no secret yet — never over
    one (a pinned relaunch's own, or anything else already on disk)."""
    driver = create_driver("linux")
    driver._principal_slot_carry_live = True
    old, new = tmp_path / "old", tmp_path / "new"
    old.mkdir()
    new.mkdir()
    (old / "install-device-secret").write_bytes(_SECRET)
    (new / "install-device-secret").write_bytes(b"\x07" * 32)
    driver._carry_install_device_secret(str(old))

    driver._carry_install_device_secret(str(new))

    assert (new / "install-device-secret").read_bytes() == b"\x07" * 32


# ── windows: the same carry, across a relaunch that KEEPS its store ─────────
#
# windows resolves one credential dir for the driver's whole life, and its
# relaunch is `recover()` re-posting the same session against it — so the
# relaunch itself loses nothing. What drops the slot is the `reset()` that
# follows every relaunch (the app's `ClearCredentialNamespace` erases every
# account's per-actor slots), so windows harvests in `recover()` and restores at
# the first sign-in after it. These drive the real `launch()` and `recover()`
# with the bridge stubbed; the fake app's `reset` erases the store as the app does.


@pytest.fixture
def windows_driver(monkeypatch):
    from drivers import windows as win

    monkeypatch.setattr(win.time, "sleep", lambda *_a, **_k: None)
    driver = win.WindowsBridgeDriver()
    sent = {}
    #: Whether the app still held its session credentials each time a `reset`
    #: reached it. True means the reset found a LIVE account runtime, so the
    #: app's sign-out-shaped arm reaches `DisposeNestClients(signOut: true)` and
    #: retires this machine's enrollment nest-side; False is the structural
    #: no-op every fresh-store driver's post-relaunch reset already is.
    resets_saw_credentials = []
    driver._fake_resets_saw_credentials = resets_saw_credentials

    def _post(path, payload):
        sent["id"] = payload.get("id")
        if payload.get("action") == "reset":
            store = _windows_store(driver)
            resets_saw_credentials.append(os.path.exists(store))
            if os.path.exists(store):
                os.remove(store)
            # …and the account store dirs: the app's factory reset erases every
            # actor scope under the unified root (`AccountStateDir.EraseAll`).
            root = getattr(driver, "_resolved_store_root", None)
            if root and os.path.isdir(root):
                import shutil

                shutil.rmtree(root)
        return {}

    driver._start_bridge = lambda: setattr(driver, "_url", "http://127.0.0.1:1")
    driver._post = _post
    driver._get = lambda path: {
        "last_command_id": sent.get("id"),
        "ready": True,
        "state": {"session": {"authenticated": False}},
    }
    driver._delete = lambda path: {"closed": True}
    driver._await_health = lambda *a, **k: True
    driver.dismiss_system_dialogs = lambda *a, **k: None
    yield driver
    driver.teardown()


def _windows_store(driver):
    return _store_file(driver._resolved_credential_dir, driver._resolved_keyring_app)


def _windows_launch_and_mint(driver, config=None, slots=None):
    """Launch, sign in, and write what that sign-in's runtime minted."""
    driver.launch({"app_path": "FaunaApp.exe", **(config or {})})
    _sign_in(driver)
    _write(_windows_store(driver), slots or _slot(_ACTOR, "a"))


def test_windows_recover_carries_the_slot_across_the_reset_after_it(windows_driver):
    driver = windows_driver
    _windows_launch_and_mint(driver, slots={**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})

    assert driver.recover() is True
    driver.reset()
    assert _read(_windows_store(driver)) is None, "the fake app's reset must erase the store"
    _sign_in(driver)

    assert _read(_windows_store(driver)) == _slot(_ACTOR, "a"), (
        "windows: the first sign-in after a recover() should restore exactly the signing-in "
        "actor's whole slot — without it the reset after every relaunch mints a new writer key "
        "and enrolls a new device row"
    )


def test_windows_recover_carries_the_replica_across_the_reset_after_it(windows_driver):
    """windows keeps ONE store root for the driver's life, so the relaunch itself
    keeps the journal — it is the reset() after it that erases every actor scope.
    The harvest in recover() copies the replica out before that, and the first
    sign-in lays it back beside the slot."""
    driver = windows_driver
    _windows_launch_and_mint(driver, slots={**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})
    _write_replica(driver, _ACTOR, "a")
    _write_replica(driver, _ACTOR_B, "b")

    assert driver.recover() is True
    driver.reset()
    assert _read_replica(driver) is None, "the fake app's reset must erase the store root"
    _sign_in(driver)

    assert _read_replica(driver) == "a-journal", (
        "windows: the first sign-in after a recover() should restore the signing-in actor's "
        "replica beside its slot — a slot over a fresh dir is a key the app retires"
    )
    assert _read_replica(driver, _ACTOR_B) is None


def test_windows_the_restore_happens_once_per_relaunch(windows_driver):
    driver = windows_driver
    _windows_launch_and_mint(driver)
    assert driver.recover() is True
    driver.reset()
    _sign_in(driver)
    assert _read(_windows_store(driver)) == _slot(_ACTOR, "a")

    driver.reset()  # a sign-out inside the relaunched process
    _sign_in(driver)

    assert _read(_windows_store(driver)) is None, (
        "windows: a second sign-in in the same relaunch re-restored the slot — that would mask "
        "an erase which dropped the slot but kept the store"
    )


def test_windows_a_reset_without_a_relaunch_restores_nothing(windows_driver):
    """The carry models a restart, never a reset: a per-test reset → sign-in inside one
    windows process mints afresh, exactly as production does."""
    driver = windows_driver
    _windows_launch_and_mint(driver)

    driver.reset()
    _sign_in(driver)

    assert _read(_windows_store(driver)) is None


@pytest.mark.parametrize(
    "owned",
    [
        pytest.param("credential_dir", id="caller-credential-dir"),
        pytest.param("keyring_app", id="caller-keyring-app"),
    ],
)
def test_windows_a_store_the_caller_owns_is_never_carried_into(windows_driver, owned, tmp_path):
    driver = windows_driver
    config = {owned: str(tmp_path / "caller") if owned == "credential_dir" else "caller-ns"}
    _windows_launch_and_mint(driver, config)

    assert driver.recover() is True
    driver.reset()
    _sign_in(driver)

    assert _read(_windows_store(driver)) is None, (
        f"windows: the carry wrote into a store the caller owns ({owned})"
    )


# ── windows: a relaunch is never a deliberate exercise of the retire arm ─────
#
# Convention 10's rule — "a relaunch models a true restart of the machine's
# store principal, never a deliberate exercise of the retire arm" — held on the
# four fresh-store drivers by construction and NOT on windows, which is what
# `test_relaunch_device_accrual.py --app windows` measured red at relaunch 1 of
# 3 (2026-09-21). windows' relaunch re-posted the same session against the one
# store the driver keeps, so the app came back SIGNED IN; the `reset()` that
# follows every relaunch then reached a live account runtime, and since those
# e2e arms became sign-out-shaped (2026-09-14) that stop retires the enrollment
# nest-side — the nest vacates the placeholder row and revokes the grant. The
# carry then laid the harvested slot back down at the first sign-in, the app
# handshook with a credential the nest had already forgotten, and shared Rust's
# principal succession minted a successor: a new writer key and a new `fauna`
# row, one per relaunch.
#
# The fix keeps the carry exactly as it is and moves the ERASE: `recover()`
# drops the session credentials itself, between the old process's kill and the
# new one's start, so the relaunched app comes back signed out and the reset
# after it is the same structural no-op it is everywhere else. Same semantics as
# the four fresh-store drivers, spelled in windows' own mechanism (one store dir
# kept for the driver's life, emptied per relaunch) rather than linux's (a fresh
# dir per launch) — an isolation axis is per-platform because the path
# derivation is per-platform.


def test_windows_recover_leaves_the_relaunched_app_signed_out(windows_driver):
    """The default relaunch contract, now windows' too: the relaunched app comes
    back with no session credentials, so nothing it does can retire this
    machine's enrollment before the carry lays the slot back down."""
    driver = windows_driver
    _windows_launch_and_mint(driver, slots={**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})

    assert driver.recover() is True

    assert _read(_windows_store(driver)) is None, (
        "windows: recover() left the app's session credentials in place, so the relaunched "
        "app comes back SIGNED IN — and the sign-out-shaped reset that follows every "
        "relaunch then retires this machine's enrollment nest-side, leaving the carry to "
        "restore a slot the nest has already forgotten"
    )


def test_windows_the_reset_after_a_relaunch_finds_nothing_to_sign_out(windows_driver):
    """The assertion convention 10 actually makes: the post-relaunch reset never
    reaches the retire arm. It cannot, because the app it reaches is already
    signed out — exactly the position linux, tui, macOS and iOS are in."""
    driver = windows_driver
    _windows_launch_and_mint(driver)

    assert driver.recover() is True
    driver.reset()

    assert driver._fake_resets_saw_credentials == [False], (
        "windows: the reset after a relaunch found the app still holding its session "
        "credentials, so it is a real sign-out — it reaches DisposeNestClients(signOut: "
        "true) and retires the enrollment the carry is about to restore"
    )


def test_windows_the_carry_still_restores_across_the_relaunch_erase(windows_driver):
    """Moving the erase must not cost the carry: the slot the relaunch harvested
    is still restored at the first sign-in after it, which is the whole reason
    the relaunch may drop the store at all."""
    driver = windows_driver
    _windows_launch_and_mint(driver, slots={**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})
    _write_replica(driver, _ACTOR, "a")

    assert driver.recover() is True
    driver.reset()
    _sign_in(driver)

    assert _read(_windows_store(driver)) == _slot(_ACTOR, "a"), (
        "windows: the relaunch erase dropped the slot the carry had harvested"
    )
    assert _read_replica(driver) == "a-journal", (
        "windows: the slot came back without its replica — a writer lives exactly as long "
        "as its journal, so the app abandons a key restored over a fresh account-store dir"
    )


def test_windows_a_preserved_relaunch_keeps_the_session(windows_driver):
    """`preserve_state_across_relaunch()` is the opt-out, and on windows it is now
    a real pin rather than a standing yes: a test asserting client-side
    durability across a restart still gets the app back signed in, with its own
    store intact."""
    driver = windows_driver
    _windows_launch_and_mint(driver)

    assert driver.preserve_state_across_relaunch() is True
    assert driver.recover() is True

    assert _read(_windows_store(driver)) == _slot(_ACTOR, "a"), (
        "windows: a pinned relaunch erased the store the pin exists to preserve"
    )


def test_windows_the_preserve_pin_lasts_one_test(windows_driver):
    """Same per-test boundary as linux's: `reset()` un-pins, so the next module's
    relaunch is a signed-out one again and a durability test cannot leak its
    preserved store into every later relaunch."""
    driver = windows_driver
    _windows_launch_and_mint(driver)
    driver.preserve_state_across_relaunch()

    driver.reset()  # the per-test boundary drops the pin
    _sign_in(driver)
    _write(_windows_store(driver), _slot(_ACTOR, "b"))  # what that sign-in's runtime minted
    assert driver.recover() is True

    assert _read(_windows_store(driver)) is None, (
        "windows: the preserve pin outlived the test that set it — every later relaunch "
        "comes back signed in and retires its enrollment at the reset after it"
    )


@pytest.mark.parametrize(
    "owned",
    [
        pytest.param("credential_dir", id="caller-credential-dir"),
        pytest.param("keyring_app", id="caller-keyring-app"),
    ],
)
def test_windows_a_relaunch_never_erases_a_store_the_caller_owns(windows_driver, owned, tmp_path):
    """The erase rides the carry's own liveness predicate, so the seam that hands
    ONE store to a SEQUENCE of driver instances (the version-skew at-rest grid,
    `helpers/skew_client.state_home_config`) is untouched by it: a caller who
    named the dir or the namespace owns what rests there."""
    driver = windows_driver
    config = {owned: str(tmp_path / "caller") if owned == "credential_dir" else "caller-ns"}
    _windows_launch_and_mint(driver, config)

    assert driver.recover() is True

    assert _read(_windows_store(driver)) == _slot(_ACTOR, "a"), (
        f"windows: the relaunch erased a store the caller owns ({owned})"
    )


# ── macOS / iOS: the same carry, through the apple store half of `launch()` ──
#
# Both apple drivers give every launch a fresh credential dir, as linux does, so
# they harvest in `launch()` — in the store half both share,
# `InProcessAgentDriver._resolve_credential_store` — and restore at the first
# sign-in. What differs is where the account-store slot rests
# (`common.cred_store.account_store_location`): macOS mints it into a sibling
# `fauna-account-store.json`; iOS rides the foreign seam into the app's own
# `keychain.json`, each key prefixed, beside identity rows a restore must leave
# alone. These drive the real store half against tmp dirs, bridge stubbed.

_APPLE_IDENTITY_ROWS = {"fauna/index": "index-blob", f"fauna/{_ACTOR}/secret": "seed"}


def _apple_launch(driver, tmp_path, n, config=None):
    """The store half of one apple `launch()`: a fresh per-launch tmp, as both
    drivers mint one, handed to the shared resolver — then the store root the
    launch publishes after it (macOS: the relocated HOME's Application Support;
    iOS: the container's), which the replica half of the carry restores under."""
    tmp = tmp_path / f"launch-{n}"
    tmp.mkdir()
    driver._launch_config = {} if config is None else config
    cred_dir = driver._resolve_credential_store(driver._launch_config, str(tmp))
    driver._resolved_store_root = str(tmp / "Application Support" / "Fauna" / "sync")
    return cred_dir


def _apple_slots_path(client, cred_dir):
    from common.cred_store import account_store_location

    return account_store_location(client, cred_dir, "keychain")


def _write_apple_slots(client, cred_dir, slots, beside=None):
    """Lay `slots` down where `client`'s app writes the account store — prefixed
    inside `keychain.json` on iOS, a file of its own on macOS."""
    path, prefix = _apple_slots_path(client, cred_dir)
    _write(path, {**(beside or {}), **{f"{prefix}{k}": v for k, v in slots.items()}})


def _read_apple_slots(client, cred_dir):
    """The account-store entries `client`'s store holds, unprefixed; `None` when
    it holds none."""
    path, prefix = _apple_slots_path(client, cred_dir)
    stored = _read(path) or {}
    slots = {k[len(prefix):]: v for k, v in stored.items() if k.startswith(prefix)}
    return slots or None


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_relaunch_restores_only_the_signing_in_actors_slot(client, tmp_path):
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _write_apple_slots(client, first, {**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})

    second = _apple_launch(driver, tmp_path, 2)
    assert _read_apple_slots(client, second) is None, "the carry must not copy anything at launch"
    # What the relaunched process has already written to its own store by the
    # time the sign-in patch lands — on iOS the very file the slot rides in.
    identity_path = os.path.join(second, "keychain.json")
    _write(identity_path, _APPLE_IDENTITY_ROWS)
    _sign_in(driver)

    assert _read_apple_slots(client, second) == _slot(_ACTOR, "a"), (
        f"{client}: the new store should hold exactly the signing-in actor's whole slot — "
        "another actor's entries would be a slot no sign-out in this launch can erase"
    )
    identity = {k: v for k, v in (_read(identity_path) or {}).items() if k in _APPLE_IDENTITY_ROWS}
    assert identity == _APPLE_IDENTITY_ROWS, (
        f"{client}: the restore disturbed the app's own identity rows in keychain.json"
    )
    assert driver._resolved_keyring_app == "keychain", (
        "apple's resolved keyring app is a constant other helpers read "
        "(`_wait_for_own_pending_factory_reset_mint`); the carry must locate the slot "
        "without changing it"
    )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_relaunch_restores_the_replica_beside_its_slot(client, tmp_path):
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _write_apple_slots(client, first, {**_slot(_ACTOR, "a"), **_slot(_ACTOR_B, "b")})
    _write_replica(driver, _ACTOR, "a")
    _write_replica(driver, _ACTOR_B, "b")

    second = _apple_launch(driver, tmp_path, 2)
    assert _read_replica(driver) is None, "the carry must not copy anything at launch"
    _write(os.path.join(second, "keychain.json"), _APPLE_IDENTITY_ROWS)
    _sign_in(driver)

    assert _read_replica(driver) == "a-journal", (
        f"{client}: the signing-in actor's replica should land under the new launch's store "
        "root beside its restored slot"
    )
    assert _read_replica(driver, _ACTOR_B) is None


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_the_restore_happens_once_per_launch(client, tmp_path):
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _write_apple_slots(client, first, _slot(_ACTOR, "a"))
    second = _apple_launch(driver, tmp_path, 2)
    _sign_in(driver)
    assert _read_apple_slots(client, second) == _slot(_ACTOR, "a")

    _write_apple_slots(client, second, {})  # what the in-process sign-out's erase does
    _sign_in(driver)

    assert _read_apple_slots(client, second) is None, (
        f"{client}: a second sign-in in the same launch re-restored the slot — that would "
        "mask an erase which dropped the slot but kept the store"
    )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_slot_the_new_launch_already_holds_is_never_overwritten(client, tmp_path):
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _write_apple_slots(client, first, _slot(_ACTOR, "old"))
    second = _apple_launch(driver, tmp_path, 2)
    _write_apple_slots(client, second, _slot(_ACTOR, "new"))

    _sign_in(driver)

    assert _read_apple_slots(client, second) == _slot(_ACTOR, "new")


@pytest.mark.parametrize("client", ["macos", "ios"])
@pytest.mark.parametrize("owned", ["credential_dir", "relaunch-pin"])
def test_apple_a_store_the_caller_owns_is_never_carried_into(client, owned, tmp_path):
    """A supplied `credential_dir`, and apple's own relaunch pin
    (`preserve_state_across_relaunch()` → `_preserved_cred_dir`), name a store
    the caller owns: a pinned store already holds whatever its slot is."""
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _write_apple_slots(client, first, _slot(_ACTOR, "a"))

    if owned == "credential_dir":
        caller = tmp_path / "caller"
        caller.mkdir()
        second = _apple_launch(driver, tmp_path, 2, {"credential_dir": str(caller)})
    else:
        driver._preserved_cred_dir = first
        _write_apple_slots(client, first, {})  # the pinned store lost the slot in-process
        second = _apple_launch(driver, tmp_path, 2)
        assert second == first, "precondition: the relaunch pin reuses the previous store"
    _sign_in(driver)

    assert _read_apple_slots(client, second) is None, (
        f"{client}: the carry wrote into a store the caller owns ({owned})"
    )


# ── macOS / iOS: the install-device-secret carry, row-shaped ────────────────
#
# The relaunch's second survivor (convention 10, ruled 2026-09-20), whose
# linux/tui pins are above. apple has no install dir to copy a file into: the
# secret is a ROW in the app's own `keychain.json`, written by the shared
# registry through `KeychainSecretStore` under shared Rust's own logical key
# (mapped verbatim). Two consequences, and
# each has a proof below:
#
#   * **it is MERGED into the file**, never copied over it — a whole-file copy
#     would hand the new launch the previous one's identity rows and it would
#     start signed IN, when every launch must begin signed out;
#   * **it is laid back down at the first SIGN-IN, not at launch.** Every apple
#     relaunch is followed by a `reset()` whose `resetToFactory` sweeps every row
#     in the store (`KeychainStore.deleteAll`), so a row written at launch is
#     erased before anything can derive from it. The harvest still happens at
#     launch (`_begin_install_device_secret_row_carry`, last in the store half,
#     after `seed_credentials` replaces the file wholesale); only the restore
#     moves to the far side of the erase — windows' principal slot for the same
#     reason.

_SECRET_HEX = _SECRET.hex()
_INSTALL_SECRET_KEY = "install/device_secret"


def _apple_keychain(cred_dir):
    return os.path.join(cred_dir, "keychain.json")


def _mint_apple_secret(cred_dir, secret=_SECRET_HEX, beside=None):
    """What the app's first un-forced sign-in leaves in the store: the install
    secret row, beside whatever identity rows that launch also wrote."""
    _write(_apple_keychain(cred_dir), {**(beside or {}), _INSTALL_SECRET_KEY: secret})


def _read_apple_secret(cred_dir):
    return (_read(_apple_keychain(cred_dir)) or {}).get(_INSTALL_SECRET_KEY)


def _apple_reset(cred_dir):
    """`resetToFactory` as the e2e store sees it: `KeychainStore.deleteAll()`
    empties the whole service, deliberately not just the identity three. Every
    apple relaunch is followed by one (`module_relaunch` then `driver.reset()`),
    which is the whole reason the restore is at the sign-in."""
    _write(_apple_keychain(cred_dir), {})


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_relaunch_carries_the_install_device_secret(client, tmp_path):
    """A relaunch is the same machine restarting: its install secret is back in
    the store before the app sees the sign-in, so an un-forced sign-in re-derives
    the named row's id instead of registering a new row per relaunch."""
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _mint_apple_secret(first)

    second = _apple_launch(driver, tmp_path, 2)
    assert second != first, "the relaunch must still get a fresh per-launch store"
    _apple_reset(second)
    _sign_in(driver)

    assert _read_apple_secret(second) == _SECRET_HEX, (
        f"{client}: the relaunched app reached its sign-in without the install device "
        "secret the previous launch minted — every un-forced sign-in after it derives a "
        "NEW device id and the nest gains one named row per module-boundary relaunch"
    )
    if os.name == "posix":  # a windows mode word carries only the read-only bit
        assert os.stat(_apple_keychain(second)).st_mode & 0o777 == 0o600, (
            "the secret is key material: 0600, as the app writes its keychain"
        )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_the_carry_survives_the_reset_that_follows_every_relaunch(client, tmp_path):
    """The load-bearing ordering, stated as its own proof: laid down at LAUNCH
    the secret is swept by the `reset()` between the relaunch and the sign-in,
    and the carry buys nothing. This is the shape that failed on windows'
    principal slot until its restore moved to the same place."""
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))

    second = _apple_launch(driver, tmp_path, 2)
    assert _read_apple_secret(second) is None, (
        "nothing may be written at launch — the reset below would only sweep it"
    )
    _apple_reset(second)
    _sign_in(driver)

    assert _read_apple_secret(second) == _SECRET_HEX


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_the_carry_leaves_the_rows_beside_the_secret_behind(client, tmp_path):
    """The secret is one row in the file the app also keeps its identity in, so
    the carry copies that row alone. A whole-file copy would hand the new launch
    the previous one's identity and it would start signed IN, when every launch
    must begin signed out."""
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _mint_apple_secret(first, beside=_APPLE_IDENTITY_ROWS)

    second = _apple_launch(driver, tmp_path, 2)
    _apple_reset(second)
    _sign_in(driver)

    assert _read(_apple_keychain(second)) == {_INSTALL_SECRET_KEY: _SECRET_HEX}, (
        f"{client}: the carry copied rows other than the install secret into the new store"
    )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_the_restore_lands_beside_the_rows_the_launch_wrote(client, tmp_path):
    """And in the other direction: the restore merges into whatever the app has
    already written by the time the sign-in patch lands — on iOS the very file
    the principal slot rides in."""
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))

    second = _apple_launch(driver, tmp_path, 2)
    _write(_apple_keychain(second), _APPLE_IDENTITY_ROWS)
    _sign_in(driver)

    stored = _read(_apple_keychain(second)) or {}
    assert stored.get(_INSTALL_SECRET_KEY) == _SECRET_HEX
    assert {k: v for k, v in stored.items() if k in _APPLE_IDENTITY_ROWS} == (
        _APPLE_IDENTITY_ROWS
    ), f"{client}: the restore disturbed the app's own rows in keychain.json"


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_seeded_launch_still_carries_its_secret(client, tmp_path):
    """A launch seeding a registry writes `keychain.json` WHOLESALE, which is why
    the harvest is the last thing the store half does. Run before the seed it
    would read the seed's file instead of the previous launch's — for exactly the
    launches a multi-account test uses, with no symptom but a device row that
    grew."""
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))

    second = _apple_launch(
        driver, tmp_path, 2, {"seed_credentials": dict(_APPLE_IDENTITY_ROWS)}
    )
    _sign_in(driver)

    assert _read_apple_secret(second) == _SECRET_HEX, (
        f"{client}: the seeded launch reached its sign-in with no carried secret"
    )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_the_secret_survives_every_later_relaunch(client, tmp_path):
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))

    for n in range(2, 5):
        cred_dir = _apple_launch(driver, tmp_path, n)
        _apple_reset(cred_dir)
        _sign_in(driver)

    assert _read_apple_secret(cred_dir) == _SECRET_HEX


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_the_restore_happens_once_per_launch(client, tmp_path):
    """Once per LAUNCH, not once per sign-in: a `reset()` inside one launch IS a
    factory reset, which the ruling says ends the install secret, so the next
    sign-in after it must mint afresh rather than be handed the old one back."""
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))
    second = _apple_launch(driver, tmp_path, 2)
    _sign_in(driver)
    assert _read_apple_secret(second) == _SECRET_HEX

    _apple_reset(second)  # a second test in the same launch
    _sign_in(driver)

    assert _read_apple_secret(second) is None, (
        f"{client}: a second sign-in in one launch re-restored the secret — that masks a "
        "factory reset, which the ruling says ends the install secret"
    )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_patch_that_signs_nobody_in_restores_nothing(client, tmp_path):
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))
    second = _apple_launch(driver, tmp_path, 2)

    driver.set_state({"session": {"authenticated": False}})

    assert _read_apple_secret(second) is None


@pytest.mark.parametrize("client", ["macos", "ios"])
@pytest.mark.parametrize(
    "torn",
    [
        pytest.param("", id="empty"),
        pytest.param(_SECRET_HEX[:-2], id="short"),
        pytest.param(_SECRET_HEX + "00", id="long"),
        pytest.param("z" * 64, id="not-hex"),
    ],
)
def test_apple_a_torn_install_device_secret_is_never_carried(client, torn, tmp_path):
    """A value that does not decode to exactly 32 bytes is one every later
    derivation refuses for good (`fauna_core::hex32::decode`). Carrying it would
    spread that dead install across the rest of a sweep; dropping it lets the next
    launch mint a sound one."""
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1), torn)

    second = _apple_launch(driver, tmp_path, 2)
    _apple_reset(second)
    _sign_in(driver)

    assert _read_apple_secret(second) is None, (
        f"{client}: the carry laid down {torn!r} as an install secret — a torn mint no "
        "derivation can read, now copied into the next launch"
    )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_launch_that_lost_its_secret_carries_none_forward(client, tmp_path):
    """Losing the secret (the store wiped) is the fresh-machine case, and the
    carry models the disk surviving a restart — not an older launch's copy."""
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))
    second = _apple_launch(driver, tmp_path, 2)
    _apple_reset(second)

    third = _apple_launch(driver, tmp_path, 3)
    _apple_reset(third)
    _sign_in(driver)

    assert _read_apple_secret(third) is None


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_the_first_launch_carries_nothing(client, tmp_path):
    driver = _carry_driver(client)

    first = _apple_launch(driver, tmp_path, 1)
    _sign_in(driver)

    assert _read_apple_secret(first) is None


@pytest.mark.parametrize("client", ["macos", "ios"])
@pytest.mark.parametrize("owned", ["credential_dir", "relaunch-pin"])
def test_apple_a_store_the_caller_owns_gets_no_carried_secret(client, owned, tmp_path):
    """The fresh-machine opt-out is the principal carry's own: a supplied
    `credential_dir`, and apple's relaunch pin, both name a store whose contents
    the caller owns."""
    driver = _carry_driver(client)
    first = _apple_launch(driver, tmp_path, 1)
    _mint_apple_secret(first)

    if owned == "credential_dir":
        caller = tmp_path / "caller"
        caller.mkdir()
        second = _apple_launch(driver, tmp_path, 2, {"credential_dir": str(caller)})
    else:
        driver._preserved_cred_dir = first
        second = _apple_launch(driver, tmp_path, 2)
        assert second == first, "precondition: the relaunch pin reuses the previous store"
    _apple_reset(second)  # the pinned store lost the secret in-process
    _sign_in(driver)

    assert _read_apple_secret(second) is None, (
        f"{client}: the carry wrote an install secret into a store the caller owns ({owned})"
    )


@pytest.mark.parametrize("client", ["macos", "ios"])
def test_apple_a_secret_the_launch_already_holds_is_never_overwritten(client, tmp_path):
    """The restore lands only where the launch holds no secret yet — never over
    one the app minted for itself between the launch and the sign-in."""
    driver = _carry_driver(client)
    _mint_apple_secret(_apple_launch(driver, tmp_path, 1))
    held = "07" * 32

    second = _apple_launch(driver, tmp_path, 2)
    _mint_apple_secret(second, held)
    _sign_in(driver)

    assert _read_apple_secret(second) == held

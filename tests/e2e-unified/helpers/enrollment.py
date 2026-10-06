"""The machine's enrollment as the nest and the credential slot see it — the
reads every device-roster invariant test shares.

Owner: ``docs/goal/architecture/apps/sync-agent-credentials.md`` § Credential
model (the RULED 2026-09-28 one-credential block: one credential, one row) and
§ Implementation status today (the RULED 2026-09-13 relaunch carry and the
2026-09-14 sign-out retirement).

Lifted out of ``tests/test_relaunch_device_accrual.py`` when the sign-out
cycle test needed the same three reads (priority #2: one helper, two tests —
a second copy is where the barrier's shape would drift). Two rules both tests
rest on:

* **A DEDICATED actor, never the shared ``test_user``** — the roster counts
  must be exact, and the shared actor's roster is the whole session's. The
  caller still requests ``test_user``: its fixture lifts the device cap on the
  tier every test user shares, so a regression grows the roster instead of
  meeting a two-device refusal first.
* **The roster is read behind a causal barrier, never a settle-sleep**
  (convention 14): only once THIS launch's enrollment has latched — the slot's
  ``grant-registered`` record names a row the nest lists. Read any earlier and a
  regression passes on a roster that has not grown YET.
"""

from __future__ import annotations

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.cred_store import attach_account_store
from helpers.app_surface import app_name
from helpers.waiting import (
    account_runtime_role_or_skip,
    await_account_runtime_assembled,
    wait_until,
)

#: This launch's enrollment latching once the runtime has assembled: the account
#: pump's first pass registers the machine's row (or finds it current) over nest
#: round trips — in the co-located sync agent's process when the agent holds the
#: pump. Generous by design, the same class as
#: ``budgets.ACCOUNT_RUNTIME_ASSEMBLY_S``; a green run returns the instant the
#: latch names a listed row.
ENROLLMENT_LATCH_S = 240.0


def roster(nest_url: str, user: dict) -> dict[str, str]:
    """The nest's sync-device roster for ``user``, as ``{device_id: label}``."""
    actor = bytes.fromhex(user["actor_id_hex"])
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        devices = client.call("fauna.sync.devices.list", {}).get("devices", [])
    return {d["device_id"]: d.get("label") or "" for d in devices}


def principals(nest_url: str, user: dict) -> dict[str, str | None]:
    """The same roster as :func:`roster`, as ``{device_id: principal}`` — the
    hex device principal the row's grant names, ``None`` on a row that carries
    no grant (``SyncDevice.principal``). The one-credential shape's sign-out
    clears exactly this and keeps the row."""
    actor = bytes.fromhex(user["actor_id_hex"])
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        devices = client.call("fauna.sync.devices.list", {}).get("devices", [])
    return {d["device_id"]: d.get("principal") for d in devices}


#: The nest reflecting a sign-out's retirement — one authenticated round trip
#: the sign-out awaits before it erases, so this is a bound on the driver's own
#: gesture latency, never on the nest. Generous by design.
RETIREMENT_VISIBLE_S = 60.0


def await_grant_cleared(nest_url: str, user: dict, named_row: str, context: str) -> None:
    """Wait until the roster is exactly the machine's named row, carrying no
    grant — a sign-out clears the grant and keeps the row."""
    seen: dict = {}

    def retired():
        listed = principals(nest_url, user)
        seen.clear()
        seen.update(listed)
        return set(listed) == {named_row} and listed[named_row] is None

    wait_until(
        retired,
        RETIREMENT_VISIBLE_S,
        interval=1.0,
        diagnose=lambda: (
            f"{context}: after sign-out the roster reads {seen} (device id → principal) but "
            f"should be the named row {named_row} alone, carrying no grant. The named row "
            "still naming a principal = the sign-out did not retire the enrollment "
            "nest-side — grep the app log for \"the machine's enrollment retirement\". "
            "A MISSING named row = the sign-out deleted the user's device, "
            "which it must never do. Any other row = a second row the one-credential "
            "shape (sync-agent-credentials.md § Credential model, RULED 2026-09-28) "
            "rules out."
        ),
    )


def register_device(nest_url: str, user: dict, label: str) -> str:
    """Register one more sync device on ``user``'s account the way a client's
    sync daemon does (``fauna.sync.register`` as the device-owning actor, the
    label sealed under the actor's own root) — fixture setup that fills roster
    slots, never the action under test (e2e point 8(b)). Returns the new
    device id (hex).

    The shared home of the register two older tests each carry privately
    (``test_device_cards._register_test_device``,
    ``test_family._register_ward_device``); new callers come here.
    """
    import os

    from fauna_ffi import seal_device_label

    device_id = os.urandom(32).hex()
    actor = bytes.fromhex(user["actor_id_hex"])
    signing_key = bytes(user["signing_key"])
    label_sealed = seal_device_label(signing_key, bytes.fromhex(device_id), label)
    payload = {"device_id": device_id, "label": label, "capabilities": "read,write"}
    if label_sealed is not None:
        payload["label_sealed"] = label_sealed
    with WsRpcAdminClient(nest_url, actor, signing_key) as client:
        client.call("fauna.sync.register", payload)
    return device_id


def register_granted_device(nest_url: str, user: dict, label: str) -> tuple[str, str]:
    """Register one more device row on ``user``'s account and grant it a
    principal: a fresh Ed25519 key, the actor's root-signed enrollment grant
    over it (``fauna.sync.device_grant.register``, the grant from the real
    minter via ``fauna_ffi.build_device_grant``). Returns ``(device_id,
    principal_hex)``.

    What it plants is a granted device whose key **never joined the fleet**: it
    holds no generation wrap (so the Devices page marks it relay-only —
    ``devices.md`` § Custody facet piece 1) and it is no verified fleet member
    (so removing it is refused as unverifiable — § Errors & edge cases).
    Fixture setup, never the action under test (e2e point 8(b)).
    """
    import os

    from nacl.signing import SigningKey

    from fauna_ffi import build_device_grant

    device_id = register_device(nest_url, user, label)
    principal = bytes(SigningKey(os.urandom(32)).verify_key)
    authorization = build_device_grant(bytes(user["signing_key"]), principal)
    actor = bytes.fromhex(user["actor_id_hex"])
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        client.call(
            "fauna.sync.device_grant.register",
            {"device_id": device_id, "authorization": authorization},
        )
    return device_id, principal.hex()


def sign_in(app, request, nest_instance, user, *, device_id: str | None = None) -> None:
    """Sign ``app`` in as ``user`` (a dedicated actor — barrier on the switch)
    and wait for the account runtime to assemble. ``device_id`` overrides the
    session patch's forced id (see :data:`UNFORCED_DEVICE_ID`)."""
    from conftest import _login_app_as

    _login_app_as(
        app, request, nest_instance, user, verify_live_actor=True, device_id=device_id
    )
    account_runtime_role_or_skip(app.driver)
    await_account_runtime_assembled(app.driver)


#: A session-patch device id the app REFUSES to adopt (it is not 32-byte hex),
#: so the sign-in falls through to the app's own get-or-create — the production
#: path, which a forced id never reaches. Some forty-five test files reach it by
#: accident with ids of this shape; this one does it on purpose.
UNFORCED_DEVICE_ID = "e2e-unforced-device-id"

#: The named row of an un-forced sign-in reaching the nest: the app registers it
#: from its own sync bring-up, in no fixed order with the enrollment latch.
NAMED_ROW_LISTED_S = 240.0


def _device_db_id(path: str) -> tuple[str | None, dict]:
    """The file-backed arm: the id in the account-scoped ``device.db``
    ``fauna_sync_engine::engine_lifecycle`` writes (linux, tui)."""
    import sqlite3

    where: dict = {"device_db": path}
    try:
        with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
            row = db.execute(
                "SELECT value FROM device_identity WHERE key = 'device_id'"
            ).fetchone()
    except sqlite3.Error as e:
        where["read"] = repr(e)
        return None, where
    if row is None:
        where["read"] = "no device_id row yet"
        return None, where
    return bytes(row[0]).hex(), where


def _secret_store_device_id(driver, actor_hex: str) -> tuple[str | None, dict]:
    """The secret-store arm: the id the shared registry persisted for this actor
    inside the app's own credential store (macOS, iOS, windows).

    apple's e2e keychain is a host-side file — ``keychain.json`` under the
    launch's credential dir — read through the harness's one adapter
    (``common.cred_store.attach_cred_store`` → ``AppleFileCredStore.read_map``),
    where ``KeychainSecretStore`` lands every logical key verbatim. So the
    per-account slot is shared Rust's own key, ``fauna_client_accounts``'s
    ``device_id_key`` → ``fauna/{actor}/device_id``.

    **windows reads the per-account slot through this same function.** Its
    ``CredentialStore.Logical`` resolves to ``FileSecretBackend(dir, ns)`` under
    e2e — the very ``{FAUNA_E2E_CREDENTIAL_DIR}/{FAUNA_KEYRING_APP}.json`` pair
    ``attach_cred_store`` discovers off the driver — and the id is written by
    the same shared ``FfiAccountRegistry.DeviceIdForActor``, so the slot carries
    shared Rust's own key on both platforms. A windows-shaped copy of this
    function would have been a second spelling of one rule (priority #1), and
    the drift it invites is exactly the kind this barrier exists to catch. **android likewise:** its ``SecureStorage``
    resolves the id through the shared ``FfiAccountRegistry.deviceIdForActor``
    into its e2e ``FileSecretBackend`` file, which ``attach_cred_store`` reads
    over the bridge's ``GET /credentials`` (the file is on-device).

    **The per-account slot alone, on both platforms.** apple and windows both
    retired their pre-registry single slot — their session patches enrol the
    identity in the registry, so the id always lands in the per-account slot
    (``long-term-store.md`` § Downgrade mirror + abandoned-append recovery). A
    native ``device_id`` row is never read: it would hand this actor whichever
    account was last active."""
    from common.cred_store import attach_cred_store

    slot = f"fauna/{actor_hex}/device_id"
    where: dict = {"keychain_dir": getattr(driver, "_resolved_credential_dir", None), "key": slot}
    try:
        stored = attach_cred_store(app_name(driver), driver).read_map()
    except (RuntimeError, ValueError) as e:  # not launched / no adapter
        where["read"] = repr(e)
        return None, where
    value = stored.get(slot)
    if value:
        where["source"] = "per-account slot"
        return value, where
    where["read"] = "no device_id row this actor holds yet"
    where["rows"] = sorted(k for k in stored if k.startswith(f"fauna/{actor_hex}/"))
    return None, where


def _own_device_id_reader(driver, actor_hex: str):
    """A zero-argument read of the sync device id ``driver``'s app has persisted
    for ``actor_hex``, answering ``(id_or_None, where)`` — ``where`` going
    verbatim into a failure's diagnosis.

    The ID, never a path: the apps keep it in two at-rest shapes (linux/tui a
    sqlite ``device.db``; apple, windows and android a row in a key→value
    credential store, web's ``localStorage`` included — two shapes across seven
    apps, not one per app), and a
    caller that had to know which would be the third place the shapes could
    drift apart. android's is the second shape too: its e2e credential file is
    on-device, and ``attach_cred_store`` reads it over the bridge's
    ``GET /credentials``. Bound once, before the barrier below starts its clock,
    so a harness error fires eagerly rather than once per poll inside it."""
    import os

    client = app_name(driver)
    config_home = getattr(driver, "_xdg_config", None)
    if client == "linux" and config_home:
        path = os.path.join(config_home, "fauna", "sync", actor_hex, "device.db")
        return lambda: _device_db_id(path)
    if client == "tui" and config_home:
        path = os.path.join(config_home, "fauna-tui", actor_hex, "device.db")
        return lambda: _device_db_id(path)
    if client in ("macos", "ios", "windows", "android", "web"):
        return lambda: _secret_store_device_id(driver, actor_hex)
    # Every app has an arm above, so reaching here is a linux/tui driver that
    # recorded no config home — one that has not launched — or a client the
    # harness does not know: a harness bug either way, never an unbuilt surface.
    raise RuntimeError(
        f"no read-back of {client}'s own sync device id: the driver recorded no "
        "config home (`_xdg_config`) — has it launched?"
        if client in ("linux", "tui")
        else f"no read-back arm for client {client!r}"
    )


def await_named_row(app, nest_url: str, user: dict) -> tuple[str, dict[str, str]]:
    """Wait until the device id this app holds for ``user`` is a row the nest
    lists. Returns ``(device_id, roster)``.

    The barrier an un-forced sign-in needs: the enrollment latch says nothing
    about the NAMED row, and a roster read before the app has registered it
    passes a regression on a roster that has not grown yet. Every app that
    hosts the account runtime registers its named row from the enrollment
    alone — the one-credential shape (``sync-agent-credentials.md``
    § Credential model, RULED 2026-09-28) — so no app is gated here."""
    read_own_id = _own_device_id_reader(app.driver, user["actor_id_hex"])
    last: dict = {}

    def listed_under_own_id():
        own, where = read_own_id()
        last.clear()
        last.update(where)
        if own is None:
            return None
        listed = roster(nest_url, user)
        last.update(own=own, roster=sorted(listed))
        return (own, listed) if own in listed else None

    return wait_until(
        listed_under_own_id,
        NAMED_ROW_LISTED_S,
        interval=1.0,
        diagnose=lambda: (
            f"the app's own sync device id never appeared in the nest's roster (last read: "
            f"{last}). No id read back = the sign-in never reached the app's device-id "
            "get-or-create; an id the nest does not list = the app's sync bring-up never "
            "registered it — grep the app log for 'fauna.sync.register'."
        ),
    )


def latched_row_in(slot: dict, actor_hex: str) -> str | None:
    """The row half of ``actor_hex``'s ``grant-registered`` latch in an
    account-store ``slot`` map (``read_map()``), or ``None`` when nothing has
    latched — the host-side twin of ``AccountStoreHandle::enrolled_device_row``,
    which reports the same record (``devices.md`` § This-device marker, *How
    the app knows it*). The latch value is ``<row>:<grant encoding>``; only the
    row is returned, never the encoding.

    Two of the runtime's own refinements are not repeated here, and neither
    arises on a fresh e2e launch: it answers ``None`` for a latch whose
    encoding no longer matches the slot's grant (a re-mint), and it reads a
    latch value without the row half as unregistered
    (``PrincipalSlot::grant_registration_row``), so the next owner pass
    re-registers it. Since the one-credential shape (RULED 2026-09-28) the row a latch
    names is always the machine's named row."""
    latch = slot.get(f"{actor_hex}/grant-registered") or ""
    row = latch.split(":", 1)[0] if ":" in latch else ""
    return row or None


def await_enrollment(app, nest_url: str, user: dict) -> tuple[str, str, dict[str, str]]:
    """Wait until this launch's enrollment has latched on a row the nest lists.

    Returns ``(writer_key, latched_row, roster)`` — the actor's store writer key
    as the slot holds it, the row id the latch names, and the roster read behind
    that barrier. The diagnosis names slot KEYS and row ids only, never a slot
    value — the writer key is a secret even when it is a fixture's."""
    driver = app.driver
    actor = user["actor_id_hex"]
    client = app_name(driver)
    store = attach_account_store(client, driver)
    last: dict = {}

    def latched():
        slot = store.read_map()
        writer = slot.get(actor)
        row = latched_row_in(slot, actor) or ""
        last.clear()
        last.update(slot_keys=sorted(k for k in slot if k.startswith(actor)), row=row)
        if not writer or not row:
            return None
        listed = roster(nest_url, user)
        last["roster"] = sorted(listed)
        return (writer, row, listed) if row in listed else None

    return wait_until(
        latched,
        ENROLLMENT_LATCH_S,
        interval=1.0,
        diagnose=lambda: (
            f"this launch's enrollment never latched on a listed row (last read: {last}). "
            "No writer key = the runtime never minted or loaded one (see "
            "await_account_runtime_assembled's diagnosis); a writer key but no "
            "'grant-registered' row = the pump's enrollment step has not succeeded — "
            "grep the app log for 'enrollment:'."
        ),
    )

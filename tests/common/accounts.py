"""Multi-account registry seed helper (shared across client E2E drivers).

`libs/fauna-client-accounts` stores several identities on one app install as
a non-secret `fauna/index` blob plus per-actor `fauna/{actor_id}/{secret,nest_url,
device_id}` slots (see `docs/goal/architecture/long-term-store.md`
§ Multi-account evolution). This module builds that exact **logical-key** layout
so a test can pre-seed a client with ≥1 accounts before boot — the client's
`AccountRegistry` reads the seeded state straight through, with no app code.

The map uses *logical* keys (the vocabulary of the shared `SecretStore` seam),
and every app's `SecretStore` maps `fauna/*` keys verbatim onto its store (the
Linux/tui File backend under `FAUNA_E2E_CREDENTIAL_DIR`, web's localStorage, the
apple/windows keychain files, android's file backend), so the returned map is
what every driver writes as `seed_credentials`.

**The registry shape is the only shape (2026-09-24).** The pre-registry single
slot — linux's `secret_key` / `node_url` / `device_id` items, web's
`fauna_secret` / `fauna_node_url`, the natives' `FaunaIdentity` / … rows — is
no longer read by any app's launch routing or migrated into account #1:
onboarding writes the registry directly and launch routes on it alone
(`long-term-store.md` § Downgrade mirror + abandoned-append recovery). A seed
that wrote only the single slot would land nothing the app reads; every app,
android included (2026-09-28, the last), reads the registry alone.

Field shapes mirror the Rust serde types (`AccountIndex` / `AccountEntry`); keep
them in sync — a drift makes `serde_json::from_str::<AccountIndex>` fail and the
registry silently falls back to an empty index (no accounts).
"""

import json


def actor_id_hex(secret_hex: str) -> str:
    """The lowercase-hex Ed25519 public key a 64-hex seed derives to — the
    registry's actor id, exactly as `fauna_core::identity::ActorKeypair::
    from_secret_hex(..).actor_id_hex()` spells it."""
    from nacl.signing import SigningKey

    return SigningKey(bytes.fromhex(secret_hex)).verify_key.encode().hex()


def build_registry_seed(
    accounts: list[dict],
    active: str | None = None,
) -> dict[str, str]:
    """Build the logical-key credential map for a multi-account registry state.

    ``accounts``: list of per-account dicts. Required keys:
      - ``secret_hex`` — 64-hex Ed25519 seed (the per-actor secret slot).
      - ``actor_id``   — lowercase-hex Ed25519 public key (the actor id);
        :func:`actor_id_hex` derives it from the seed.
    Optional per-actor slots / index cache:
      - ``nest_url``, ``device_id`` — the other two per-actor slots.
      - ``handle``, ``domain``, ``tier`` — the non-secret server-data cache.
      - ``require_confirm_to_activate`` (bool, default ``False``) — the re-auth flag.
      - ``succeeded_by`` (actor id) / ``succeeded_from`` (actor ids, nearest hop
        first) — the succession link, on the retired row and the successor's row
        respectively, as ``AccountRegistry::record_succession`` writes it for a
        device that ran the ceremony. Omitted when absent, as serde does.

    ``active``: actor id of the account that should be active. Defaults to the
    first account's actor id (mirrors ``AccountRegistry::add_account``: the first
    added account becomes active).

    Returns a flat ``{logical_key: str}`` map: ``fauna/index`` (the ``AccountIndex``
    JSON) plus ``fauna/{actor}/{secret,nest_url,device_id}`` for each account.
    """
    if not accounts:
        raise ValueError("build_registry_seed needs at least one account")

    active_id = active if active is not None else accounts[0]["actor_id"]

    index_entries = []
    seed: dict[str, str] = {}
    for acct in accounts:
        actor_id = acct["actor_id"]
        seed[f"fauna/{actor_id}/secret"] = acct["secret_hex"]
        if acct.get("nest_url") is not None:
            seed[f"fauna/{actor_id}/nest_url"] = acct["nest_url"]
        if acct.get("device_id") is not None:
            seed[f"fauna/{actor_id}/device_id"] = acct["device_id"]
        entry = {
            "actor_id": actor_id,
            "handle": acct.get("handle"),
            "domain": acct.get("domain"),
            "tier": acct.get("tier"),
            "require_confirm_to_activate": bool(acct.get("require_confirm_to_activate", False)),
        }
        if acct.get("succeeded_by") is not None:
            entry["succeeded_by"] = acct["succeeded_by"]
        if acct.get("succeeded_from"):
            entry["succeeded_from"] = list(acct["succeeded_from"])
        index_entries.append(entry)

    index = {"active": active_id, "accounts": index_entries}
    seed["fauna/index"] = json.dumps(index)
    return seed


def single_account_seed(
    secret_hex: str,
    *,
    nest_url: str | None = None,
    device_id: str | None = None,
    handle: str | None = None,
    domain: str | None = None,
) -> dict[str, str]:
    """The registry shape of ONE signed-in (or mid-onboarding) identity — what a
    real onboarding's `persist_confirmed_identity` + `persist_logged_in` leave
    behind, and what every `CredStore.inject_identity` writes. Omit
    ``nest_url`` to leave that slot absent (the "identity, no nest_url"
    hydration branch — `HandleEntry` on relaunch)."""
    return build_registry_seed(
        [
            {
                "actor_id": actor_id_hex(secret_hex),
                "secret_hex": secret_hex,
                "nest_url": nest_url,
                "device_id": device_id,
                "handle": handle,
                "domain": domain,
            }
        ]
    )

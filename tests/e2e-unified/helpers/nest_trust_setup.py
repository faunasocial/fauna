"""Fixture setup for journeys that mint content trust on a **dedicated** nest
(`docs/goal/ui/nests.md` § Trust facet — grants): sign the app in as that
nest's admin, and enroll the `mda` holder a mail grant is sealed to.

Convention 8 carve-out (b): both are arranging preconditions — a signed-in
owner and a nest that has a mail service to trust — not the mutation under
test. Shared by the Nests-page grant journeys (`test_nest_trust_grants.py`)
and the labeler-catalog subscription journey (`test_labeler_catalog.py`),
whose subscribe mints the same kind of grant to the same holder.
"""

from __future__ import annotations

import sqlite3
from typing import NamedTuple


class MdaHolder(NamedTuple):
    """An enrolled `mda` holder's X25519 seal target and the secret behind it
    — the secret is what the holder's drain unseals a deposited grant with."""

    pubkey: bytes
    secret: bytes


def login_as_admin(app, nest, *, device_id: str) -> None:
    """Point the app at `nest` and sign in as its admin (Admin ⊇ User covers
    the tier-create, the mail enable and the mint). `device_id` names this
    journey's device so two journeys never share one device row."""
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    admin = nest["admin"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": bytes(admin["signing_key"]).hex(),
            "handle": "admin",
            "actor_id": admin["actor_id_hex"],
            "device_id": device_id,
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    # The connection barrier every login seam owes (`_login_app_as`): a
    # deadline poll on the app's own transport word, so the first online-only
    # step (mail enable, a catalog read) never races the WS handshake and finds
    # "no active session" on a loaded box (convention 14).
    from helpers.connection import wait_until_online

    wait_until_online(app.driver)


def seed_mda_holder(nest, *, bridge_id: str = "trust-mda") -> MdaHolder:
    """Enroll an approved `mda`-role content-processor holder with an X25519
    seal target on `nest`, without running a bridge — the same admin
    pre-approve + x25519 attest `_spawn_mda_bridge` performs before its
    process starts. Returns the holder's X25519 keypair.

    The mint only needs a holder to SEAL to; the nest stores the deposit and
    the test reads it back, so no holder process has to fetch anything — a
    test that proves what the deposit OPENS unseals it with the secret."""
    from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
    from cryptography.hazmat.primitives.serialization import (
        Encoding,
        NoEncryption,
        PrivateFormat,
        PublicFormat,
    )
    from nacl.signing import SigningKey

    from helpers.bridge_enrollment import enroll_and_approve_bridge

    ed_pub = bytes(SigningKey.generate().verify_key)
    x_secret = X25519PrivateKey.generate()
    x_pub = x_secret.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    enroll_and_approve_bridge(nest["url"], nest["admin"]["signing_key"], ed_pub, "mda", bridge_id)
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        conn.execute(
            "UPDATE bridge_service_users SET x25519_pubkey = ? WHERE ed25519_pubkey = ?",
            (x_pub, ed_pub),
        )
        conn.commit()
    finally:
        conn.close()
    return MdaHolder(
        pubkey=x_pub,
        secret=x_secret.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption()),
    )

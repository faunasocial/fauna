"""Mint a real identity succession against a running nest — FIXTURE SETUP.

Used to arrange the precondition for the journey in
``tests/test_identity_succession_refusal.py``: an identity that has genuinely
been succeeded, so the old device earns the ``superseded`` refusal on its next
connect (``docs/goal/behavior/identity-succession.md`` § Propagation → *Own
device fleet*).

**This is a precondition, not the behavior under test.** Convention 8 requires
the behavior-under-test to be driven through the app UI, and the journey does
exactly that — the refusal, the routing, the message and the import are all UI.
Minting the succession is fixture setup, which the convention explicitly carves
out. (The *user-facing* way to mint one is the Settings → Recovery kit
``identity-stolen-button`` ceremony; that button is a separate track, and a
journey about the OLD device's refusal should not be gated on it.)

It also drives the two ceremonies the *pre-identity* design exists for — the
seed-initiated replacement window with its veto (§ The RecoveryKey →
*Replacement*) and the seed-signed emergency lockout a thief can invoke — so a
test can assert that an owner holding nothing but the phrase still reaches the
remedy over a genuinely anonymous connection.

**The split with Rust.** Everything here is transport: pre-identity and
USER-class WS-RPC kinds this harness already speaks. Every wire shape — decoding a registration
record, computing the chain head, building and signing the statement under its
three domain-separation tags, canonical DAG-CBOR — lives in the Rust helper
``libs/fauna-client-recovery/examples/recovery_fixture.rs``, which mirrors
``succeed_identity``'s own construction. Python shuttles opaque hex blobs and
knows nothing about their contents, so a change to the statement shape cannot
leave a stale Python twin behind.

The nest verifies the statement for real (the RecoveryKey signature is checked
against the chain the identity registered), which is the point: a fabricated
``actor_successions`` row would not verify, so the client could never name the
successor and the journey's load-bearing positive assertion could not exist.
"""
from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from clients.ws_rpc_anon_client import WsRpcAnonClient

# Both are on the nest's pre-identity allowlist by necessity — the scenario is
# an owner whose seed a thief holds, and a bearer requirement would let the
# attack disable its own remedy (`bins/fauna-nest/src/pre_identity_allowlist.rs`).
_CHAIN_KIND = "fauna.recovery.registration.chain"
_SUBMIT_KIND = "fauna.recovery.succession.submit"
# The anonymous catch-up read: a peer that last saw the old identity asks where
# it went. Pre-identity for the same reason the chain serve is.
_LOOKUP_KIND = "fauna.recovery.succession.lookup"
# USER-class, unlike the two above: the account comes from the authenticated
# connection, and a record naming a different actor is refused.
_REGISTER_KIND = "fauna.recovery.registration.submit"

# The seed-initiated replacement window (`identity-succession.md` § The
# RecoveryKey → *Replacement*). `request`/`status` are USER-class — the owner
# still holds the seed on that arm. `challenge`/`veto` are pre-identity for the
# same reason `submit` is: the vetoing owner may hold nothing but the phrase.
_REPLACEMENT_REQUEST_KIND = "fauna.recovery.replacement.request"
_REPLACEMENT_STATUS_KIND = "fauna.recovery.replacement.status"
_REPLACEMENT_CHALLENGE_KIND = "fauna.recovery.replacement.challenge"
_REPLACEMENT_VETO_KIND = "fauna.recovery.replacement.veto"
# The seed-signed emergency lockout a thief can invoke — the reason the
# succession kinds are pre-identity at all.
_LOCKOUT_KIND = "fauna.account.lockout"
# USER-class: `set` takes the account from the authenticated connection and
# refuses a record naming anyone else; `get` reads any actor's row.
_PROFILE_SET_KIND = "fauna.profile.set"
_PROFILE_GET_KIND = "fauna.profile.get"

def recovery_fixture_binary() -> Path:
    """Path to the test-only ``recovery_fixture`` helper, building it if needed.

    Delegates to ``common.nest.build_recovery_fixture`` — the same
    stamp-keyed, build-slotted, win-aware composition the nest and sync-agent
    builders use. The helper links the shared protocol/client crates, so a
    binary is stale when ANY workspace source is newer, not just its own file: a
    binary older than the wire shape signs a body the nest's strict decode
    refuses.
    """
    from common.nest import build_recovery_fixture

    return Path(build_recovery_fixture())


def registration_chain(nest_url: str, actor_id_hex: str) -> list[bytes]:
    """The identity's RecoveryKey registration chain, oldest first, verbatim.

    Empty when the identity registered no kit — the nest's honest answer, never
    an error. The bytes stay opaque here; only the Rust helper decodes them.
    """
    with WsRpcAnonClient(nest_url) as anon:
        reply = anon.call(_CHAIN_KIND, {"actor_id": bytes.fromhex(actor_id_hex)})
    return list(reply.get("registrations", []))


def succession_statements(nest_url: str, actor_id_hex: str) -> list[bytes]:
    """Every succession from ``actor_id`` forward, oldest first, verbatim.

    Pre-identity: a peer catching up on an identity it last saw holds no account
    here. Empty when the identity was never succeeded — the nest's honest
    answer, never an error, which is what lets a consumer tell "never succeeded"
    from "the nest withheld it". The bytes stay opaque here.
    """
    with WsRpcAnonClient(nest_url) as anon:
        reply = anon.call(_LOOKUP_KIND, {"actor_id": bytes.fromhex(actor_id_hex)})
    return list(reply.get("statements", []))


def _run_fixture(subcommand: str, argv: list[str]) -> dict[str, str]:
    """Run one ``recovery_fixture`` subcommand; return its ``key=value`` lines."""
    proc = subprocess.run(
        [str(recovery_fixture_binary()), subcommand, *argv], capture_output=True
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"recovery_fixture {subcommand} failed (exit {proc.returncode}): "
            f"{proc.stderr.decode(errors='replace')}"
        )
    fields = {}
    for line in proc.stdout.decode().splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            fields[key.strip()] = value.strip()
    return fields


def _require(fields: dict[str, str], key: str, subcommand: str) -> str:
    try:
        return fields[key]
    except KeyError as exc:  # pragma: no cover - a contract break, not a flake
        raise RuntimeError(
            f"recovery_fixture {subcommand} output missing {key!r}; got {fields!r}"
        ) from exc


def register_recovery_kit(
    nest_url: str, *, actor_id_hex: str, identity_seed_hex: str
) -> str:
    """Register a RecoveryKey for an identity that has none. Returns its secret.

    The succession ceremony is authorized by a RecoveryKey signature, so an
    identity with no registered kit cannot be succeeded at all — this is the
    other half of the precondition.

    Done here rather than through Settings → Recovery kit deliberately: that
    ceremony has landed on tui only, and a journey about the *old device's
    refusal* must not skip on six apps because an unrelated UI leg is pending.
    The UI ceremony has its own coverage in ``test_recovery_kit_settings.py``.

    ``registration.submit`` is USER-class — it takes the account from the
    authenticated connection and refuses a record naming a different actor — so
    this goes over the authenticated client, unlike the succession kinds.
    """
    fields = _run_fixture("register-kit", ["--identity-seed", identity_seed_hex])
    secret = _require(fields, "recovery_secret", "register-kit")
    registration = bytes.fromhex(_require(fields, "registration", "register-kit"))

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    # `actor_id` is 32 RAW bytes here, not hex — unlike the pre-identity calls
    # above, which take it inside a CBOR payload.
    with WsRpcAdminClient(
        nest_url,
        actor_id=bytes.fromhex(actor_id_hex),
        signing_key=bytes.fromhex(identity_seed_hex),
    ) as ws:
        ws.call(_REGISTER_KIND, {"registration": registration})
    return secret


def request_seed_alone_replacement(
    nest_url: str, *, actor_id_hex: str, identity_seed_hex: str
) -> tuple[str, int]:
    """Park a seed-alone RecoveryKey replacement. Returns ``(secret, lands_at)``.

    The arm for an owner who still holds the identity seed but lost the recovery
    phrase: the record carries no co-signature by the key being replaced, so it
    cannot land immediately — the nest parks it for ``RECOVERY_REPLACE_GRACE``
    and the *current* key can veto it instantly. USER-class, because holding the
    seed means a session exists.

    The returned secret is the key that *would* be registered if the window ran
    out uncontested — useful to assert it never became the head.
    """
    registrations = registration_chain(nest_url, actor_id_hex)
    if not registrations:
        raise RuntimeError(
            f"identity {actor_id_hex[:16]}… has registered no RecoveryKey, so "
            "there is nothing for a seed-alone request to replace"
        )
    argv = ["--identity-seed", identity_seed_hex]
    for reg in registrations:
        argv += ["--registration", reg.hex()]
    fields = _run_fixture("mint-seed-alone-replacement", argv)
    secret = _require(fields, "recovery_secret", "mint-seed-alone-replacement")
    registration = bytes.fromhex(
        _require(fields, "registration", "mint-seed-alone-replacement")
    )

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_url,
        actor_id=bytes.fromhex(actor_id_hex),
        signing_key=bytes.fromhex(identity_seed_hex),
    ) as ws:
        reply = ws.call(_REPLACEMENT_REQUEST_KIND, {"registration": registration})
    return secret, int(reply["lands_at"])


def replacement_status(
    nest_url: str, *, actor_id_hex: str, identity_seed_hex: str
) -> Optional[dict]:
    """The authenticated actor's own pending replacement, or ``None``.

    The read every app's standing veto banner derives from. ``pending`` is
    *absent* rather than null when nothing pends, so this collapses both to
    ``None``.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_url,
        actor_id=bytes.fromhex(actor_id_hex),
        signing_key=bytes.fromhex(identity_seed_hex),
    ) as ws:
        reply = ws.call(_REPLACEMENT_STATUS_KIND, {})
    return reply.get("pending")


def land_due_replacements(nest_url: str, *, advance_secs: Optional[int] = None) -> dict:
    """Run the nest's landing sweep once, past the replacement window.

    Returns ``{"landed": n, "cancelled": n}``. The door is
    ``POST /api/v1/test/recovery/land_due_replacements``
    (``bins/fauna-nest/src/recovery_landing_test_hook.rs``, ``test-hooks``
    builds only): it calls the SAME ``land_due_replacements(state, now)`` the
    wall-clock sweep in ``main.rs`` calls, with ``now`` moved forward by
    ``advance_secs`` — default one second past ``RECOVERY_REPLACE_GRACE``, so
    every parked replacement is due. Nothing is written to the database around
    it, so what a test observes afterwards is the production landing path
    (convention 14: never wait out the 30 days).
    """
    import json
    import ssl
    import urllib.request

    body = {} if advance_secs is None else {"advance_secs": advance_secs}
    ctx = ssl._create_unverified_context() if nest_url.startswith("https://") else None
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/recovery/land_due_replacements",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, context=ctx) as resp:
        return json.loads(resp.read())


def veto_pending_replacement(
    nest_url: str, *, actor_id_hex: str, recovery_secret_hex: str
) -> bool:
    """Cancel whatever pends, proving the CURRENT RecoveryKey. Returns cancelled.

    Deliberately over an **anonymous** connection: the veto is pre-identity
    because the scenario that needs it is a thief who revoked every session and
    invoked the lockout, leaving the owner holding only the phrase. Driving it
    over an authenticated client would prove nothing about that.

    Two round trips, because the nonce is single-use — a bare replayable signed
    veto could be captured and used to cancel any future honest replacement.
    """
    actor_id = bytes.fromhex(actor_id_hex)
    with WsRpcAnonClient(nest_url) as anon:
        challenge = anon.call(_REPLACEMENT_CHALLENGE_KIND, {"actor_id": actor_id})
        nonce = bytes(challenge["nonce"])
        fields = _run_fixture(
            "sign-veto",
            [
                "--old-actor-id",
                actor_id_hex,
                "--recovery-secret",
                recovery_secret_hex,
                "--nonce",
                nonce.hex(),
            ],
        )
        signature = bytes.fromhex(_require(fields, "signature", "sign-veto"))
        reply = anon.call(
            _REPLACEMENT_VETO_KIND,
            {"actor_id": actor_id, "nonce": nonce, "signature": signature},
        )
    return bool(reply["cancelled"])


def emergency_lockout(nest_url: str, *, actor_id_hex: str, identity_seed_hex: str) -> None:
    """Drive the seed-signed emergency lockout, as a thief holding the seed would.

    Pre-identity and signed by the identity seed over the domain-tagged
    ``account_lockout_signed_message`` (``common.sig_domain``, the Python twin
    of the Rust builder) — no bearer, because the honest use is an owner locked
    out of their own sessions.
    """
    import time

    from nacl.signing import SigningKey

    from common.sig_domain import account_lockout_signed_message

    actor_id = bytes.fromhex(actor_id_hex)
    timestamp = int(time.time())
    signature = SigningKey(bytes.fromhex(identity_seed_hex)).sign(
        account_lockout_signed_message(actor_id, timestamp)
    ).signature
    with WsRpcAnonClient(nest_url) as anon:
        anon.call(
            _LOCKOUT_KIND,
            {
                "actor_id": actor_id_hex,
                "timestamp": timestamp,
                "signature": signature.hex(),
            },
        )


def mint_statement(
    *,
    old_actor_id_hex: str,
    recovery_secret_hex: str,
    successor_seed_hex: str,
    registrations: list[bytes],
    old_seed_hex: Optional[str] = None,
) -> tuple[str, bytes]:
    """Sign a succession statement. Returns ``(new_actor_id_hex, statement)``.

    ``old_seed_hex`` is the old identity's seed when the owner still holds it
    (the theft case). It is optional and never load-bearing — passing it changes
    no consumer's verdict, it is continuity information only.
    """
    argv: list[str] = [
        "--old-actor-id",
        old_actor_id_hex,
        "--recovery-secret",
        recovery_secret_hex,
        "--successor-seed",
        successor_seed_hex,
    ]
    if old_seed_hex:
        argv += ["--old-seed", old_seed_hex]
    for reg in registrations:
        argv += ["--registration", reg.hex()]

    fields = _run_fixture("mint-succession", argv)
    return (
        _require(fields, "new_actor_id", "mint-succession"),
        bytes.fromhex(_require(fields, "statement", "mint-succession")),
    )


def submit_statement(nest_url: str, statement: bytes) -> str:
    """Submit the statement; returns the successor actor id hex the nest echoed.

    The echo is the confirmation that *this* identity landed — the reply names
    it precisely so a helper device that submitted on someone's behalf can check.
    """
    with WsRpcAnonClient(nest_url) as anon:
        reply = anon.call(_SUBMIT_KIND, {"statement": statement})
    return bytes(reply["new_actor_id"]).hex()


def succeed_identity(
    nest_url: str,
    *,
    old_actor_id_hex: str,
    recovery_secret_hex: str,
    successor_seed_hex: str,
    old_seed_hex: Optional[str] = None,
) -> str:
    """Fetch the chain, sign, submit. Returns the successor actor id hex.

    The whole precondition in one call, mirroring the leg order of
    ``fauna_client_recovery::succeed_identity``.
    """
    registrations = registration_chain(nest_url, old_actor_id_hex)
    if not registrations:
        raise RuntimeError(
            f"identity {old_actor_id_hex[:16]}… has registered no RecoveryKey, "
            "so it cannot be succeeded — create a recovery kit first"
        )
    new_actor_id_hex, statement = mint_statement(
        old_actor_id_hex=old_actor_id_hex,
        recovery_secret_hex=recovery_secret_hex,
        successor_seed_hex=successor_seed_hex,
        registrations=registrations,
        old_seed_hex=old_seed_hex,
    )
    echoed = submit_statement(nest_url, statement)
    if echoed != new_actor_id_hex:
        raise RuntimeError(
            f"nest echoed successor {echoed} but the statement named "
            f"{new_actor_id_hex}"
        )
    return new_actor_id_hex


# ── Profile rows a succession moves ─────────────────────────────────────────
#
# A succession re-points the profile row's ownership but leaves its signed bytes
# alone, so the successor inherits a row the PREDECESSOR signed
# (``profile.md`` § After an identity succession). A journey about that row
# needs to arrange it before the succession and read its signer afterwards. As
# above, the Rust helper owns the record (``sign-profile`` / ``decode-profile``)
# and this side is transport.


@dataclass(frozen=True)
class StoredProfile:
    """A stored profile row, as the shared ``decode_profile`` reads it."""

    #: The identity the stored bytes name: the signer for a verified envelope.
    actor_id: str
    #: ``direct`` (the named identity's own key), ``delegated`` or ``unsigned``.
    origin: str
    display_name: str
    bio: str


def publish_signed_profile(
    nest_url: str,
    *,
    actor_id_hex: str,
    identity_seed_hex: str,
    display_name: str,
    bio: str,
) -> None:
    """Publish a profile this identity signed, as its edit form's first save would.

    Fixture setup (convention 8's carve-out): the journeys this serves are about
    what a SUCCESSOR does with the row, so the predecessor's publish is a
    precondition, not the behavior under test.
    """
    fields = _run_fixture(
        "sign-profile",
        [
            "--identity-seed", identity_seed_hex,
            "--display-name", display_name.encode().hex(),
            "--bio", bio.encode().hex(),
        ],
    )
    body = bytes.fromhex(_require(fields, "body", "sign-profile"))

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_url,
        actor_id=bytes.fromhex(actor_id_hex),
        signing_key=bytes.fromhex(identity_seed_hex),
    ) as ws:
        ws.call(_PROFILE_SET_KIND, {"body": body})


def stored_profile(
    nest_url: str,
    *,
    owner_actor_id_hex: str,
    reader_actor_id_hex: str,
    reader_seed_hex: str,
) -> StoredProfile:
    """The row ``owner`` holds today, decoded — read over ``reader``'s session.

    ``reader`` is any live account on the nest: ``fauna.profile.get`` serves any
    actor's row, and the owner of an inherited row may have no Python-side
    session at all. After a succession the owner is the successor while the
    bytes may still name the predecessor, which is exactly what ``actor_id``
    tells apart.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_url,
        actor_id=bytes.fromhex(reader_actor_id_hex),
        signing_key=bytes.fromhex(reader_seed_hex),
    ) as ws:
        reply = ws.call(_PROFILE_GET_KIND, {"actor_id": owner_actor_id_hex})
    fields = _run_fixture("decode-profile", ["--body", bytes(reply["body"]).hex()])
    return StoredProfile(
        actor_id=_require(fields, "actor_id", "decode-profile"),
        origin=_require(fields, "origin", "decode-profile"),
        display_name=bytes.fromhex(
            _require(fields, "display_name", "decode-profile")
        ).decode(),
        bio=bytes.fromhex(_require(fields, "bio", "decode-profile")).decode(),
    )

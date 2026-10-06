"""Python twin of ``fauna_protocol::sig_domain`` for the ACTOR-key contexts.

The nest verifies every actor-key ceremony over a domain-tagged message built
by a single-source Rust builder (``key-material-hierarchy.md`` § Architectural
rules #8 — tagged-only since 2026-08-17; no untagged accept
path exists). Python-side callers that sign these ceremonies themselves (claim,
register, invite, lockout, login handshake, challenge-verify) MUST build the
same bytes; this module is the one place the tags + layouts live, exactly as
``common.envelope.sign_dagcbor_envelope`` twins the federation context.

Layouts (mirroring the Rust builders — on drift the Rust side wins):

- plain contexts: ``tag ‖ body`` (fixed-width fields, variable tail last);
- length-prefixed contexts (registration, invite-submit — the item-3 fix):
  every element INCLUDING the tag encodes as ``len_u64_be ‖ bytes``.
"""

import struct

AUTH_HANDSHAKE_V2 = b"fauna.auth.handshake.v2\0"
AUTH_VERIFY_V2 = b"fauna.auth.verify.v2\0"
# The nest's own channel-binding context (deployment key) — what a login signer
# verifies BEFORE signing, to learn the identity it binds (``login.md``
# § Binding the nest; ``nest_identity.read_nest_identity``).
CERT_BINDING_V1 = b"fauna.cert-binding.v1\0"
CLAIM_ADMIN_V1 = b"fauna.auth.claim-admin.v1\0"
ACCOUNT_LOCKOUT_V1 = b"fauna.account.lockout.v1\0"
ACCOUNT_REGISTER_V1 = b"fauna.account.register.v1\0"
INVITE_SUBMIT_V1 = b"fauna.account.invite-request.submit.v1\0"
INVITE_CANCEL_V1 = b"fauna.account.invite-request.cancel.v1\0"
# RETIRED 2026-09-24 (the unbound NAT-mode form; no accept path kept) — listed
# only so the bytes are never reused, exactly as ``sig_domain.rs`` keeps it.
SETUP_NAT_MODE_V1 = b"fauna.setup.nat-mode.v1\0"
SETUP_NAT_MODE_V2 = b"fauna.setup.nat-mode.v2\0"


def _lp(element: bytes) -> bytes:
    """One length-prefixed element: ``(len as u64 be) ‖ bytes``."""
    return struct.pack(">Q", len(element)) + element


def domain_separated(tag: bytes, body: bytes) -> bytes:
    """``tag ‖ body`` — the plain-concat form for fixed-layout contexts."""
    return tag + body


def domain_separated_length_prefixed(tag: bytes, elements: list[bytes]) -> bytes:
    """The injective form: every element, tag included, length-prefixed."""
    return _lp(tag) + b"".join(_lp(e) for e in elements)


def handshake_signed_message(
    actor_id: bytes, timestamp_ms: int, nest_id: bytes, client_nonce: bytes
) -> bytes:
    """``fauna.auth.handshake`` — login (twin of ``auth::handshake_signed_message``):
    ``actor_id ‖ timestamp_be ‖ nest_id ‖ client_nonce``. ``nest_id`` is the
    receiving nest's identity, read off the connection first
    (``login.md`` § Binding the nest); the nonce is mandatory."""
    return domain_separated(
        AUTH_HANDSHAKE_V2, actor_id + struct.pack(">Q", timestamp_ms) + nest_id + client_nonce
    )


def challenge_verify_signed_message(actor_id: bytes, nonce: bytes, nest_id: bytes) -> bytes:
    """``fauna.auth.verify`` (twin of ``auth::challenge_verify_signed_message``):
    ``actor_id ‖ nonce ‖ nest_id``."""
    return domain_separated(AUTH_VERIFY_V2, actor_id + nonce + nest_id)


def cert_binding_signed_message(spki_sha256: bytes, nonce: bytes) -> bytes:
    """The nest's channel-binding proof (deployment key; twin of
    ``auth_handlers::sign_channel_binding``'s tagged half): ``spki ‖ nonce`` —
    the SPKI empty on a plaintext nest, where the message is the nonce alone."""
    return domain_separated(CERT_BINDING_V1, spki_sha256 + nonce)


def claim_admin_signed_message(actor_id: bytes, timestamp: int) -> bytes:
    """``fauna.auth.claim_admin`` (twin of ``claim::claim_admin_signed_message``)."""
    return domain_separated(CLAIM_ADMIN_V1, actor_id + struct.pack(">Q", timestamp))


def account_lockout_signed_message(actor_id: bytes, timestamp: int) -> bytes:
    """``fauna.account.lockout`` (twin of ``account::account_lockout_signed_message``)."""
    return domain_separated(ACCOUNT_LOCKOUT_V1, actor_id + struct.pack(">Q", timestamp))


def register_signed_message(
    actor_id: bytes, handle: str, domain: str, timestamp_ms: int
) -> bytes:
    """``fauna.account.register`` (twin of ``account::register_signed_message``)."""
    return domain_separated_length_prefixed(
        ACCOUNT_REGISTER_V1,
        [actor_id, handle.encode(), domain.encode(), struct.pack(">Q", timestamp_ms)],
    )


def invite_submit_signed_message(
    actor_id: bytes, handle: str, message: str, timestamp_ms: int
) -> bytes:
    """``fauna.account.invite_request.submit`` (twin of
    ``invite::invite_submit_signed_message``). The nest verifies over the
    LOWERCASED handle."""
    return domain_separated_length_prefixed(
        INVITE_SUBMIT_V1,
        [actor_id, handle.encode(), message.encode(), struct.pack(">Q", timestamp_ms)],
    )


def invite_cancel_signed_message(actor_id: bytes, timestamp_ms: int) -> bytes:
    """``fauna.account.invite_request.cancel`` (twin of
    ``invite::invite_cancel_signed_message``)."""
    return domain_separated(INVITE_CANCEL_V1, actor_id + struct.pack(">Q", timestamp_ms))


def nat_mode_signed_message(mode: str, actor_id_hex: str, timestamp_ms: int, nest_id_hex: str) -> bytes:
    """``fauna.setup.nat_mode`` (twin of ``nat_mode::nat_mode_signed_message``):
    ``SETUP_NAT_MODE_V2 ‖ mode ‖ \\n ‖ actor_id_hex ‖ \\n ‖ ts_decimal ‖ \\n ‖
    nest_id_hex`` — nest-bound, the only form (the unbound ``.v1`` form retired
    2026-09-24)."""
    body = (
        mode.encode()
        + b"\n"
        + actor_id_hex.encode()
        + b"\n"
        + str(timestamp_ms).encode()
        + b"\n"
        + nest_id_hex.encode()
    )
    return domain_separated(SETUP_NAT_MODE_V2, body)


"""tier_3 API: the nest's log never carries what people said or the secrets
that let them in (`docs/goal/architecture/apps/observability.md` § 2.
Persistence & privacy).

`docs/features/admin-logs.md` outcome 8, over a real `fauna-nest` binary and
the admin's own `fauna.admin.logs` read. The nest is put through the moments
that touch each kind of secret — the claim (its code, the deployment seed it
hands back, the admin's key and token), a member signing in and posting text
only they wrote, and a stranger presenting a claim code the box refuses — and
then every line the admin can read is searched for each secret, in every
encoding a log line could plausibly spell it in. The Rust twin
(`bins/fauna-nest/tests/conformance_log_redaction.rs`) pins one log site's
redaction; this asserts the outcome over the whole log a real admin receives.

The nest is dedicated: its log must hold only what this test did to it.
"""

from __future__ import annotations

import base64
import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import create_actor_and_register
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.fixture
def redaction_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "log-redaction-nest")
    try:
        yield nest
    finally:
        cleanup()


def _spellings(secret: str | bytes) -> set[str]:
    raw = secret.encode() if isinstance(secret, str) else bytes(secret)
    out = {raw.hex(), raw.hex().upper(), base64.b64encode(raw).decode(),
           base64.urlsafe_b64encode(raw).decode().rstrip("=")}
    if isinstance(secret, str):
        out.add(secret)
    return {s for s in out if len(s) >= 6}


@pytest.mark.feature("admin-logs")
def test_the_nests_log_never_carries_message_text_keys_tokens_or_claim_codes(redaction_nest):
    nest = redaction_nest
    admin = nest["admin"]
    admin_sk = admin["signing_key"] if isinstance(admin["signing_key"], SigningKey) \
        else SigningKey(bytes.fromhex(admin["signing_key"]))

    member = create_actor_and_register(nest["port"], admin_signing_key=admin_sk)
    said = f"only the member wrote this {secrets.token_hex(6)}"
    ws_api.create_post(
        nest["port"], member,
        sign_and_encode_post(member["signing_key"], int(time.time() * 1_000_000), said),
    )

    refused_code = f"WRONG{secrets.token_hex(4).upper()}"
    stranger = SigningKey.generate()
    with pytest.raises(RpcCallError):
        with WsRpcAnonClient(nest["url"]) as anon:
            anon.call("fauna.auth.claim_admin", {
                "claim_code": refused_code,
                "actor_id": bytes(stranger.verify_key).hex(),
                "signature": "00" * 64,
                "timestamp": int(time.time()),
            })

    with WsRpcAdminClient(nest["url"], actor_id=bytes(admin_sk.verify_key),
                          signing_key=bytes(admin_sk)) as ws:
        entries = ws.call("fauna.admin.logs", {})["entries"]
    assert entries, "the nest's log is not empty after all that"
    log = "\n".join(f"{e['target']} {e['message']}" for e in entries)

    secrets_by_kind = {
        "message text": {said},
        "the claim code": {nest["claim_code"], refused_code},
        "the admin's key": _spellings(bytes(admin_sk)),
        "the member's key": _spellings(bytes(member["signing_key"])),
        "the admin's token": _spellings(admin["token"]) if admin.get("token") else set(),
        "the member's token": _spellings(member["token"]),
        "the deployment seed": _spellings(bytes.fromhex(admin["deployment_seed"]))
        if admin.get("deployment_seed") else set(),
    }
    leaked = {kind: sorted(s for s in spelled if s in log) for kind, spelled in secrets_by_kind.items()}
    leaked = {kind: found for kind, found in leaked.items() if found}
    assert not leaked, f"the admin's log carries {sorted(leaked)}: {leaked!r}"

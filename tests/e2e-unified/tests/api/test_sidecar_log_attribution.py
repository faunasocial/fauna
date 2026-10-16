"""A helper cannot speak as another helper or as the nest on the admin's Logs
surface (`docs/goal/architecture/apps/observability.md` § The sidecar log plane:
the source identity is never in the payload).

`docs/features/admin-logs.md` outcome 12, at the wire. The docker witness of
outcome 2 shows each real helper's line arriving under its own name; this one
tries the forgery outcome 2 never attempts. An enrolled, approved MTA helper —
an RPC client here, its key enrolled the way the helper's own cold boot does —
reports one event whose payload claims every other attribution it could
(`source`, `target`, `bridge`, `role` naming the MDA helper and the nest). The
nest must file it under `mta:` and nowhere else. Admission is synchronous in the
reporting call, so the read that follows sees it without a wait (convention 14).
"""

from __future__ import annotations

import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.bridge_enrollment import enroll_and_approve_bridge

pytestmark = pytest.mark.tier_3


@pytest.fixture
def log_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "log-plane-nest")
    raw = nest["admin"]["signing_key"]
    nest["admin_sk"] = raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))
    yield nest
    cleanup()


@pytest.mark.feature("admin-logs")
def test_a_helper_cannot_make_its_events_appear_under_another_name(log_nest):
    nest = log_nest
    helper = SigningKey.generate()
    enroll_and_approve_bridge(nest["url"], nest["admin_sk"], bytes(helper.verify_key), "mta")

    marker = f"forged-{secrets.token_hex(4)}"
    event = f"forge.{secrets.token_hex(3)}"
    with WsRpcAdminClient(nest["url"], actor_id=bytes(helper.verify_key),
                          signing_key=bytes(helper)) as ws:
        ws.call("fauna.bridges.report_log_events", {
            "events": [{
                "timestamp_ms": int(time.time() * 1000),
                "level": "warn",
                "event": event,
                "message": marker,
                # Every attribution a payload could try to claim.
                "source": "nest",
                "target": f"mda:{event}",
                "bridge": "mda",
                "role": "mda",
            }],
            "dropped": 0,
            "source": "mda",
        })

    admin_sk = nest["admin_sk"]
    with WsRpcAdminClient(nest["url"], actor_id=bytes(admin_sk.verify_key),
                          signing_key=bytes(admin_sk)) as admin:
        entries = admin.call("fauna.admin.logs", {})["entries"]
    carrying = [e for e in entries if marker in repr(e)]
    assert carrying, f"the helper's event never reached the admin's Logs surface: {entries[-5:]!r}"
    assert {e["target"] for e in carrying} == {f"mta:{event}"}, (
        f"the event must be filed under the helper's own name and nowhere else: {carrying!r}"
    )

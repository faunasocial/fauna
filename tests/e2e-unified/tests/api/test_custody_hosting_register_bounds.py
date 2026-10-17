"""A member can have the nest hold only a bounded number of copies for people
elsewhere: one more is refused, and an allowance asked too large is honoured
at the nest's ceiling rather than refused — on a real ``fauna-nest`` binary,
over the door a host's app deposits through.

Owner doc: ``docs/goal/architecture/account-replica-posture.md`` § The custody
grant + ceremony — *rows refuse and bytes clamp*: past
``MAX_CUSTODY_HOSTING_ROWS_PER_HOST`` a NEW row is a caller error, while a
``retained_bytes_cap`` above ``MAX_RETAINED_BYTES_CAP`` is written at the
ceiling, so a legitimate host is never locked out by a number its own app
suggested; and a REWRITE of a row the host already holds (the stop control and
the budget-adjust are the same verb) stays admitted at the cap, or a host at
the cap could never pause its own rows.

Ports ``bins/fauna-nest/tests/conformance_custody_hosting_client.rs::
the_register_door_bounds_a_hostile_accounts_deposits`` onto the binary's
``fauna.custody.hosting.register`` / ``.list`` / ``.remove``. Each witness is
signed by the owner's key through the shared ``sign_custody_grant``
(``fauna_ffi.sign_custody_witness``) and names THIS nest's identity
(``fauna.nest.info``'s ``nest_id``) — the custodian the register door demands.

No row is ever dialled: the deposits name an origin on a reserved domain, and
the test removes every row it planted before it ends, so the shared session
nest's 15-minute pump pass finds nothing of this file's.
"""

import os

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.custody_hosting import UNANSWERED_OWNER_NEST_URL, hosting_deposit, nest_id

pytestmark = pytest.mark.tier_3

#: `fauna_core::custody_ceremony::MAX_CUSTODY_HOSTING_ROWS_PER_HOST`.
MAX_ROWS_PER_HOST = 8
#: `fauna_core::custody_ceremony::MAX_RETAINED_BYTES_CAP` (8 GiB).
MAX_RETAINED_BYTES_CAP = 8 * 1024 * 1024 * 1024


@pytest.mark.feature("admin-held-custody")
def test_a_host_holds_a_bounded_number_of_copies_and_an_oversized_allowance_is_clamped(
    nest_instance,
):
    url = nest_instance["url"]
    host = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"],
        base_url=url)
    owner_secret = os.urandom(32)
    owner_hex = bytes(SigningKey(owner_secret).verify_key).hex()
    custodian = nest_id(url)

    def deposit(grant_id: bytes, *, cap: int, stopped: bool = False) -> dict:
        return hosting_deposit(owner_secret, owner_hex, grant_id, custodian,
                               owner_nest_url=UNANSWERED_OWNER_NEST_URL, cap=cap,
                               stopped=stopped)

    grants = [bytes([i + 1]) * 16 for i in range(MAX_ROWS_PER_HOST + 1)]
    with WsRpcAdminClient(
        url, actor_id=host["actor_id_bytes"], signing_key=bytes(host["signing_key"])
    ) as ws:
        try:
            # Up to the cap every row lands, and an allowance past the nest's
            # ceiling is honoured AT the ceiling — never refused, never
            # written through.
            for g in grants[:MAX_ROWS_PER_HOST]:
                reply = ws.call("fauna.custody.hosting.register",
                                deposit(g, cap=2**64 - 1))
                assert reply.get("ok") is True, reply
            rows = ws.call("fauna.custody.hosting.list", {})["rows"]
            assert len(rows) == MAX_ROWS_PER_HOST, rows
            assert {r["retained_bytes_cap"] for r in rows} == {MAX_RETAINED_BYTES_CAP}, (
                f"an oversized allowance must be clamped to the ceiling: {rows!r}"
            )

            # One more NEW row is refused, and the refusal rests nothing.
            with pytest.raises(RpcCallError):
                ws.call("fauna.custody.hosting.register",
                        deposit(grants[MAX_ROWS_PER_HOST], cap=4096))
            rows = ws.call("fauna.custody.hosting.list", {})["rows"]
            assert len(rows) == MAX_ROWS_PER_HOST, "the refused deposit must rest no row"

            # A rewrite of a row already held is still admitted at the cap —
            # the host can always pause (or re-budget) its own copies.
            reply = ws.call("fauna.custody.hosting.register",
                            deposit(grants[0], cap=4096, stopped=True))
            assert reply.get("ok") is True, reply
            rows = ws.call("fauna.custody.hosting.list", {})["rows"]
            assert len(rows) == MAX_ROWS_PER_HOST, "a rewrite replaces in place"
            paused = [r for r in rows if bytes(r["grant_id"]) == grants[0]]
            assert paused and paused[0]["stopped"] is True and \
                paused[0]["retained_bytes_cap"] == 4096, rows
        finally:
            for g in grants:
                ws.call("fauna.custody.hosting.remove", {"grant_id": g})

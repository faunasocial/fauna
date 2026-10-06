"""The `admin-users` hub routes Users-section action failures through the
dedicated `admin-users-action-error` element, NOT the global `error-message`
banner (admin.md § Users line 34, § Errors & edge cases line 140 — "Users (all
three sections): `admin-users-action-error`").

We induce a *real* nest rejection on the production action path: submit a pending
invite request, render its row, then make the request non-pending out-of-band
(deny it via a direct admin WS-RPC call). Clicking the now-stale UI
`invite-request-row-approve-button` drives the real
`fauna.admin.invite_requests.approve` call, which the nest rejects with
`fauna.admin.conflict` ("invite request is not pending"). The failure must land
in `admin-users-action-error` — proving the routing — while the global
`error-message` banner stays hidden.

tier_3: real linux UI against a real nest; the out-of-band deny uses an
authenticated admin WS-RPC client bound to the nest admin identity.
"""

import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _submit_invite_request(nest_url: str, handle: str, message: str = "let me in"):
    """Submit a real Ed25519-signed invite request (mirrors the requester's
    onboarding path) so the admin hub has a pending row. Same signing as
    `test_admin_users_hub._submit_invite_request`."""
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    ts = int(time.time() * 1000)
    from common.sig_domain import invite_submit_signed_message

    msg = invite_submit_signed_message(bytes.fromhex(actor_hex), handle, message, ts)
    sig = sk.sign(msg).signature.hex()
    with WsRpcAnonClient(nest_url) as anon:
        anon.call(
            "fauna.account.invite_request.submit",
            {
                "actor_id": actor_hex,
                "handle": handle,
                "message": message,
                "timestamp": ts,
                "signature": sig,
            },
        )


@pytest.mark.feature("admin-users")
def test_users_action_error_routes_to_dedicated_element(admin_app, nest_instance):
    """A Users-section action failure surfaces in `admin-users-action-error`,
    not the global `error-message` banner."""
    nest_url = nest_instance["url"]
    admin = nest_instance["admin"]
    # Handles are bare, lowercase, alphanumeric-or-hyphen (registration.rs
    # `validate_handle`) — no domain/dots.
    handle = f"stale-req-{int(time.time() * 1000)}"

    # 1. A pending request → a row to act on.
    _submit_invite_request(nest_url, handle)

    # 2. Render the hub so the request row is built (the navigate refetch fills
    #    the Pending-requests section).
    admin_app.admin.navigate_users()
    deadline = time.monotonic() + 15.0
    row_index = None
    while time.monotonic() < deadline:
        handles = admin_app.admin.invite_request_handles()
        if handle in handles:
            row_index = handles.index(handle)
            break
        time.sleep(0.3)
    assert row_index is not None, (
        f"submitted invite request {handle!r} never rendered as a row; "
        f"saw {admin_app.admin.invite_request_handles()!r}. "
        f"{admin_app.admin.pending_requests_diagnosis()}"
    )

    # 3. Make the request non-pending out-of-band (the UI row stays stale — a
    #    linux refetch only fires on the app's own actions). The next UI approve
    #    on this row will hit `fauna.admin.conflict` ("not pending").
    admin_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        reply = admin_ws.call("fauna.admin.invite_requests.list", {})
        req_id = next(
            r["id"]
            for r in reply["invite_requests"]
            if r["handle"] == handle and r["status"] == "pending"
        )
        admin_ws.call("fauna.admin.invite_requests.deny", {"id": req_id})

    # 4. Drive the real production action against the stale row → nest rejects.
    admin_app.admin.approve_request(index=row_index)

    # 5. The failure must land in the dedicated element, not the global banner.
    #    `admin-users-action-error` sits at the hub tail below a long
    #    ScrolledWindow, so assert via count + get_text (not is_visible).
    assert admin_app.driver.count("admin-users-action-error") > 0, (
        "admin-users-action-error element is not built on this client"
    )
    err = admin_app.admin.users_action_error_text()
    assert err, (
        "expected the rejected approve to surface in admin-users-action-error, "
        f"but it was empty. global error-message: {admin_app.error_text()!r}"
    )
    # The generic global banner must stay hidden — the whole point of the
    # dedicated id is that Users-hub errors don't leak into the app-wide surface.
    assert not admin_app.has_error(), (
        f"Users-hub action error leaked into the global error-message banner: "
        f"{admin_app.error_text()!r}"
    )

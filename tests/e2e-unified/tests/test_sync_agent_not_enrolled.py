"""The *Not enrolled* reading of the `sync-agent-status` shell element: when the
nest no longer holds this machine's renewal grant, the app says the helper is
not enrolled, and keeps saying so until the user signs in again.

Owners: ``docs/goal/architecture/apps/sync-agent.md`` § Local agent health (app
UI) — the fifth state, the reading of ``ServiceStatusInfo::needs_reenrollment``
— and ``sync-agent-credentials.md`` § Credential model → *A refused renewal is
terminal* (what sets the report, and that recovery is the user signing in
again). Feature catalog: ``docs/features/local-folder-sync.md`` outcome 22.

**How the nest stops holding the sign-in, with no test hook.** Two production
kinds the user's own account may call, in order:

1. ``fauna.sync.device_grant.revoke`` on the machine's store principal — the
   grant a device removal or a nest reset would take away. It also revokes the
   sessions that key minted.
2. ``fauna.sessions.revoke_all`` — every session of the account, the app's
   pushed bearer included, closes ``4401``. The agent dials on that pushed
   bearer while an app runs, and a rejected credential routes the renewal loop
   to ask *at once* (*A rejected credential asks at once*), so the renewal over
   the principal happens now rather than at the ~55-minute lead — and the nest
   answers it ``fauna.auth.not_registered``.

The agent then holds the refusal record, which an app's pushed bearer never
clears, so the element reads *Not enrolled* while the app stays signed in.
Recovery is the user's gesture: sign out, sign in again — the enrollment
registers a fresh principal and the element reads *Running* again.

**Latency-independent** (convention 14): every step waits on state — the
enrollment latch, the nest's roster, the element's text — never a sleep. The
budgets are ceilings a green run never pays.
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import enrollment
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The agent's status poll retries every 10 s (tui `sync_agent.rs::
#: status_poll_loop`, linux `app.rs`, macOS `SyncAgentHealthModel`), and the refusal needs one reconnect plus
#: one renewal round trip before the poll can read it. Far above any healthy
#: delay; a green run returns the moment the element changes.
STATUS_CHANGE_S = 120.0


def _status_text(app) -> str | None:
    try:
        return app.driver.get_text("sync-agent-status")
    except Exception:  # noqa: BLE001 — absent mid-redraw reads as "not yet"
        return None


def _await_status(app, want: str, context: str) -> None:
    seen: list[str | None] = [None]

    def reads():
        seen[0] = _status_text(app)
        return seen[0] == want

    wait_until(
        reads,
        STATUS_CHANGE_S,
        interval=0.5,
        diagnose=lambda: (
            f"{context}: sync-agent-status never read {want!r} (last saw {seen[0]!r}); "
            f"error={app.error_text()!r}; agent log tail: "
            f"{(app.driver.app_stderr_text() or '')[-2000:]!r}"
        ),
    )


@pytest.mark.linux
@pytest.mark.tui
# macos joined 2026-11-09: the same `sync-agent-status` element reads the same
# `ServiceStatusInfo::needs_reenrollment` (`sync-agent.md` § Local agent health).
# macOS builds its private child agent only under `FAUNA_E2E_REAL_SYNC_AGENT`,
# which `real_sync_agent` sets; `isolated_sync_agent` beside it keeps a windows
# app in the same invocation on its own pipe — the `test_sync_live_apply.py`
# pair.
@pytest.mark.macos
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_a_revoked_grant_reads_not_enrolled_until_the_user_signs_in_again(
    app, request, nest_instance, test_user
):
    from conftest import _make_user
    from helpers.e2e_session import e2e_login_device_id

    running = S.status.sync_agent.running
    not_enrolled = S.status.sync_agent.not_enrolled

    nest_url = nest_instance["url"]
    # A DEDICATED actor: revoke_all ends every session of the account, which
    # must never reach the session-scoped shared user.
    user = _make_user(nest_instance)
    named_row = e2e_login_device_id(app)

    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_url, user)
    principal = enrollment.principals(nest_url, user).get(named_row)
    assert principal, (
        f"the machine's named row {named_row} carries no grant after the enrollment "
        f"latch; roster {enrollment.principals(nest_url, user)}"
    )
    _await_status(app, running, "before the revoke")

    actor = bytes.fromhex(user["actor_id_hex"])
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        client.call("fauna.sync.device_grant.revoke", {"device_key": principal})
    # A fresh connection: the revoke above may have closed the one it rode.
    # `keep_token_id` naming no session ends them all, this caller's included.
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        client.call("fauna.sessions.revoke_all", {"keep_token_id": ""})

    _await_status(
        app,
        not_enrolled,
        "after the nest retired this machine's grant and its sessions (the agent's "
        "renewal over the principal should have been refused fauna.auth.not_registered)",
    )

    app.settings.sign_out()
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_url, user)
    renewed = enrollment.principals(nest_url, user).get(named_row)
    assert renewed and renewed != principal, (
        f"signing in again should register a fresh principal on {named_row}; "
        f"it reads {renewed!r} (was {principal!r})"
    )
    _await_status(app, running, "after signing in again")

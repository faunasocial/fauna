"""Signing out stops the background helper syncing that identity's folders at
once — the helper's own answer says so.

Owners: ``docs/goal/architecture/apps/sync-agent-credentials.md`` § Credential
model → *The signed-out reconcile* (sign-out un-provisions the agent's one
capability slot; the app writes the durable signed-out marker before its
best-effort ``UnprovisionCapability`` push, and the live agent's reconcile is
the fallback) and the agent's ``pipe_server::unprovision_now`` (the capability
leaves, and the in-memory binding config resets so a signed-out agent serves no
account's bindings). Feature catalog: ``docs/features/local-folder-sync.md``
outcome 24.

**The assertion is positive, read off the agent itself** (convention 14): before
the sign-out the agent answers ``GetSyncStatus`` with ``connected: true`` and
lists the bound set in ``ListEngines``; after it, the same socket answers
``connected: false`` and lists no engine for that set. "A file never arrived"
is never the witness.

**Why the old second clause is not witnessed here.** The agent holds ONE
capability slot, so no two identities sync at once on one machine, and tui's
sign-out is the all-accounts erase (``sync-agent-credentials.md`` § Credential
model, the sign-out ruling) — there is no gesture that signs out an account the
helper is not serving. That the actor-matched marker leaves the served account
alone is pinned at tier_1 instead
(``fauna_ipc::sync`` ``a_marker_for_another_account_revokes_nothing``), so
outcome 24 claims only what an app can show.

The budgets are ceilings a green run never pays; every wait reads state.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.folder_content import agent_diagnosis as _agent_diagnosis
from helpers.folder_content import atomic_write as _atomic_write
from helpers.folder_content import await_agent_upload as _await_agent_upload
from helpers.folder_content import bind_location_under_set as _bind_location_under_set
from helpers.sync_agent_endpoint import agent_endpoint
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

#: A ceiling on the agent's state change. The sign-out's own push lands it
#: within the gesture; the live reconcile's fallback sleeps up to 300 s, far
#: above this, so a green run here is the immediate path.
UNPROVISION_CEILING_S = 60.0


def _agent_client(app):
    from helpers.sync_agent_ipc_unix import SyncAgentClient

    endpoint = agent_endpoint(app.driver)
    assert endpoint and endpoint[0] == "unix", (
        f"this launch publishes no unix agent socket ({endpoint!r}); the witness "
        "addresses the launch's own agent"
    )
    return SyncAgentClient(endpoint[1], timeout=10)


def _engines(client) -> list[dict]:
    payload = client.call("ListEngines")
    return list(payload["Engines"])


@pytest.mark.parametrize(
    "folder_share_owner_app", ["tui", "macos", "linux"], indirect=True
)
@pytest.mark.tui
# macos joined 2026-11-09: its sign-out runs the same un-provision
# (`sync-agent-credentials.md` § Credential model → *The signed-out reconcile*,
# FaunaKit's `onSignOut` teardown) against the private child agent the
# `real_sync_agent` launch builds, on the socket under the launch's own HOME.
# `isolated_sync_agent` keeps a windows app in the same invocation on its own
# pipe — the `test_sync_live_apply.py` pair.
@pytest.mark.macos
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
# linux joined 2026-11-11: its sign-out runs the same un-provision
# (`apps/fauna-linux/src/account_runtime.rs::stop_agent_then_store`) against the
# direct child agent every e2e launch spawns, on the socket under the launch's
# private XDG_RUNTIME_DIR — no pin (`helpers/sync_seats.py`, "linux needs
# NOTHING here").
@pytest.mark.linux
@pytest.mark.real_conversations
@pytest.mark.feature("local-folder-sync")
def test_signing_out_stops_the_helper_serving_that_identitys_folders(
    folder_share_owner_app, tmp_path
):
    app, _nest, _owner = folder_share_owner_app

    set_name = f"signout-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    folder = tmp_path / "bound"
    folder.mkdir()
    _bind_location_under_set(app, set_name, folder, seat="owner")

    filename = f"before-{secrets.token_hex(3)}.txt"
    _atomic_write(folder / filename, "synced while signed in\n")
    _await_agent_upload(app, filename, seat="owner")

    client = _agent_client(app)
    status = client.sync_status()
    assert status["connected"], (
        f"the agent that just uploaded should hold a capability; status {status!r}\n"
        + _agent_diagnosis(app, "owner")
    )
    served = [e for e in _engines(client) if e.get("folder") == set_name]
    assert served, (
        f"the agent should list an engine for the bound set {set_name!r}; engines "
        f"{_engines(client)!r}\n" + _agent_diagnosis(app, "owner")
    )
    served_ids = {e["folder_id"] for e in served}

    app.settings.sign_out()

    seen: dict = {}

    def stopped() -> bool:
        seen["status"] = client.sync_status()
        seen["engines"] = _engines(client)
        listed = {e["folder_id"] for e in seen["engines"]} | {
            e.get("folder") for e in seen["engines"]
        }
        return not seen["status"]["connected"] and not (
            listed & (served_ids | {set_name})
        )

    wait_until(
        stopped,
        UNPROVISION_CEILING_S,
        interval=0.5,
        diagnose=lambda: (
            "after the sign-out the agent should hold no capability and list no "
            f"engine for {set_name!r}; last status {seen.get('status')!r}, engines "
            f"{seen.get('engines')!r}\n" + _agent_diagnosis(app, "owner")
        ),
    )

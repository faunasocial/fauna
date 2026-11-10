"""tier_3: the **cli app** — Fauna operated by an LLM host over MCP — end to end
(``docs/goal/architecture/apps/cli.md`` § The device shell, § Test surface).

The device shell is launched the way an LLM host launches a local MCP server
(``fauna-cli mcp``, JSON-RPC over stdio — ``drivers/cli_mcp.py``), and the
user approves the typed code app-less, on their own WS-RPC session
(``helpers/typed_code_consent.py``). The journeys launch no app driver: the
assistant's reach is a ``[nest]`` outcome of the cli's own catalog page,
observed from outside by the MCP driver, and the approving app is a helper
seat that only seeds the grant (``docs/goal/architecture/feature-catalog.md``
§ The two surfaces). What the approval card itself shows for the cli — its
name and the assistant-reach line — is connected-apps' ``[app]`` outcome,
witnessed on the app (``test_connected_apps.py``). One journey:

- **red first** — before any grant, a feed tool answers the host "not
  connected, ask for the handle", never an empty success; a mail tool is not in
  the catalog at all (mail is a later plane's arm — § Reach grows one plane at
  a time), so a host asking for one is refused;
- ``connect(handle)`` starts the typed-code ceremony and answers the code for
  the HUMAN; typing it on the user's own session opens the card, approving it
  releases the shell's poll (``authorization-server.md`` § Consent → the typed
  code) — and until then every tool answers "waiting for approval";
- the cli then reads the account's feed through the fourth arm
  (``fauna:feed:read:account``) and posts through the oracle's ``post.author``
  door, and the local timeline serves the post;
- the owner's roster row (``fauna.principals.list``, the read the Connected
  apps page composes from) carries the operation the oracle performed for it
  (``ui/connected-apps.md`` § Layout & flow, region 3);
- the profile arm and the oracle's ``profile.author`` door: ``profile_set``
  edits the display name through the custodian and ``nest://profile`` and its
  template read it (``authorization-server.md`` § Scope grammar → *The
  profile arm*); a second ``profile_set`` edits the bio alone, and the name
  survives it, untouched (``key-material-hierarchy.md`` § *The oracle*, the
  ``profile.author`` row);
- the notifications arms: a like from a second actor is polled, marked read
  through the write arm, and the unread count then reads 0 (*The
  notifications arms*) — the refusal while an arm is absent is pinned at
  tier_1 (``principal_handlers::tests``), since the catalog's consent always
  asks for every arm it needs.

The journey runs over both transports the device shell speaks — stdio
(``fauna-cli mcp``) and the loopback streamable-HTTP listener
(``fauna-cli mcp --listen``) — and a last test pins the listener's own
defence: a foreign ``Origin`` or ``Host`` (DNS rebinding) is refused before the
method layer.

The same binary's ``argv`` shell (``fauna-cli <noun> <verb> …``, derived from
the catalog) runs a short journey of its own beside the MCP run: connect by
handle, a read compared with the MCP answer, a post, and a destructive delete
refused without ``--yes`` (its own docstring).

A second journey drives the admin arms both ways — a member's approve of an
admin string resolves nothing, and an admin's principal is refused without
the arms and served with them (its own docstring). Two more pin the shell's
resilience (§ The device shell): two accounts held as two principals, a call
naming which and a relaunch resuming both; and a session the nest dropped,
redialled for a read and never retried for a write. One pins custody on a box
with no usable OS secret store: the unattended arm's declared ``0600`` file,
resumed by a relaunch.

The subject's mutations are the cli's own tool calls; the consent is the
grant's seed, given on the approving account's own session as a Fauna app
gives it. The nest and the real PDS bridge are ``helpers/atproto_consent.py``'s, because a grant is
refused unless the approving account's ATProto identity is active.
"""

import json
import os
import stat
import time
from pathlib import Path

import pytest

from actions.api_actor import ApiActor
from helpers.admin_wire import admit_adult
from drivers.cli_mcp import CliArgv, CliMcpSession
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.client_metadata_server import (  # noqa: F401 — pytest fixtures adopted by import
    CLI_CLIENT_ID,
    cli_metadata,
    consent_nest_env,
)
from helpers.budgets import ORCHESTRATION_STEP_S, RPC_ROUNDTRIP_S
from helpers.typed_code_consent import (
    answer_typed_code_as,
    approve_typed_code_as,
    pending_consents,
    user_session,
)
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

#: The cli's client identity: the organisation's published document
#: (`apps/cli.md` § Open decisions 1), which ``cli_metadata`` serves through
#: the mapped host.
_CLIENT_ID = CLI_CLIENT_ID


@pytest.fixture(params=["stdio"])
def cli(request, cli_binary, consent_nest, tmp_path):
    """The device shell over one transport: stdio by default, the loopback
    HTTP listener when a test asks with ``indirect`` parametrization."""
    session = CliMcpSession(cli_binary, consent_nest["url"], tmp_path, transport=request.param)
    yield session
    session.close()


def _until_connected(cli: CliMcpSession) -> dict:
    """Call ``feed_local`` until the shell's poll has turned into a session.
    Deadline-polled on the shell's own answer (convention 14): the wait is the
    nest's poll interval, the assertion is the state."""
    deadline = time.monotonic() + ORCHESTRATION_STEP_S
    while True:
        result = cli.call("feed_local")
        if not result["isError"] or "Waiting" not in cli.text(result):
            return result
        if time.monotonic() >= deadline:
            return result
        time.sleep(1.0)  # sleep-ok: paces a deadline poll on the shell's own state, never asserted on


def _until_notified(cli: CliMcpSession, sender_hex: str) -> dict:
    """Poll ``notifications_poll`` until a row from ``sender_hex`` is listed —
    deadline-polled on the inbox's own state (convention 14)."""
    deadline = time.monotonic() + ORCHESTRATION_STEP_S
    while True:
        result = cli.call("notifications_poll")
        rows = (result.get("structuredContent") or {}).get("notifications") or []
        if result["isError"] or any(n.get("sender_id") == sender_hex for n in rows):
            return result
        if time.monotonic() >= deadline:
            return result
        time.sleep(1.0)  # sleep-ok: paces a deadline poll on the inbox's own state, never asserted on


@pytest.mark.feature("fauna-for-ai-assistants")
@pytest.mark.parametrize("cli", ["stdio", "http"], indirect=True)
def test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user(
    cli, consent_nest, consent_bridge
):
    alice = consent_nest["user"]
    init = cli.initialize()
    assert init["serverInfo"]["name"] == "fauna-cli", init

    # ── Red first: nothing is reachable before the user's grant. ──
    before = cli.call("feed_local")
    assert before["isError"] and "call connect" in cli.text(before), (
        f"before any grant a tool must tell the host to connect: {before!r}\n{cli.diagnostics()}"
    )
    names = cli.tool_names()
    assert not [n for n in names if n.startswith("mail")], (
        f"mail has no arm yet, so the catalog serves no mail tool: {names!r}"
    )
    refused = cli.request_error("tools/call", {"name": "mail_send", "arguments": {}})
    assert refused["code"] == -32602, refused
    # The profile resource and the notifications poll are refused alike before
    # the grant — a resource read has no `isError` shape, so its refusal is the
    # JSON-RPC error carrying the same "call connect" text.
    profile_before = cli.request_error("resources/read", {"uri": "nest://profile"})
    assert "call connect" in profile_before["message"], profile_before
    poll_before = cli.call("notifications_poll")
    assert poll_before["isError"] and "call connect" in cli.text(poll_before), poll_before

    # ── The typed code: the shell answers it for the human, who approves it. ──
    started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    user_code = started["structuredContent"]["user_code"]
    pending = cli.call("feed_local")
    assert pending["isError"] and user_code in cli.text(pending), (
        f"until the human approves, a tool says so with the code: {pending!r}"
    )

    # The card is the cli's, under its published identity.
    card = approve_typed_code_as(consent_nest, alice, user_code)
    assert card["client_id"] == _CLIENT_ID, f"the code opens the cli's own card: {card!r}"

    # ── The fourth arm: the account's feed, read as the account. ──
    local = _until_connected(cli)
    assert not local["isError"], (
        f"after the approval the cli reads the local timeline: {local!r}\n{cli.diagnostics()}"
    )
    feeds = cli.call("feed_list")
    assert not feeds["isError"], (
        f"the account arm covers the account's own feeds: {feeds!r}\n{cli.diagnostics()}"
    )
    assert isinstance(feeds["structuredContent"].get("feeds"), list), feeds

    # ── Posting through the oracle's door, served by the local timeline. ──
    posted = cli.call("post_create", {"text": "posted by an assistant through fauna-cli"})
    assert not posted["isError"], (
        f"post.author authors a public post as the account: {posted!r}\n{cli.diagnostics()}"
    )
    post_id = posted["structuredContent"]["post_id"]
    after = cli.call("feed_local")
    assert post_id in {p["post_id"] for p in after["structuredContent"]["posts"]}, (
        f"the local timeline serves the post the cli made: {after!r}"
    )

    # ── The owner's roster: the row, and what the oracle did for it. ──
    with user_session(consent_nest, alice) as ws:

        def served_row():
            rows = [
                p
                for p in ws.call("fauna.principals.list", {})["principals"]
                if p["client_id"] == _CLIENT_ID and p.get("oracle")
            ]
            return rows[0] if rows and rows[0]["oracle"].get("ops_in_window", 0) >= 1 else None

        row = wait_until(
            served_row,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: ws.call("fauna.principals.list", {})["principals"],
        )
    assert row["oracle"].get("last_op"), (
        f"the cli's roster row carries the oracle's record of the post: {row!r}"
    )

    # ── The profile arm, through the oracle's door: the name the cli sets is
    # what `nest://profile` and its template read. ──
    published = f"Alice, set by an assistant over {cli.transport}"
    named = cli.call("profile_set", {"display_name": published})
    assert not named["isError"], (
        f"profile.author edits the display name as the account: {named!r}\n{cli.diagnostics()}"
    )
    for uri in ("nest://profile", f"nest://profile/{alice['actor_id_hex']}"):
        read = cli.request("resources/read", {"uri": uri})
        body = json.loads(read["contents"][0]["text"])
        assert body["display_name"] == published, (
            f"{uri} reads the published profile through the shared decode: {read!r}\n"
            f"{cli.diagnostics()}"
        )

    # ── The profile class: the cli edits the bio alone; the name survives it. ──
    bio = f"edited by an assistant over {cli.transport}"
    edited = cli.call("profile_set", {"bio": bio})
    assert not edited["isError"], (
        f"profile.author edits the bio as the account: {edited!r}\n{cli.diagnostics()}"
    )
    assert edited["structuredContent"]["bio"] == bio, edited
    read = cli.request("resources/read", {"uri": "nest://profile"})
    body = json.loads(read["contents"][0]["text"])
    assert body["bio"] == bio and body["display_name"] == published, (
        f"nest://profile reads the delegated edit, the unnamed name kept: {read!r}"
    )

    # ── The notifications arms: a like from a second actor is polled, then
    # marked read through the write arm, and the count reads 0. ──
    # The nest is module-scoped and the journey runs once per transport, so the
    # second actor's handle carries the transport (a handle registers once).
    bob = admit_adult(consent_nest, f"bob{cli.transport}", domain=HANDLE_DOMAIN, token=True)
    ApiActor(
        consent_nest["url"], bob["token"], bob["actor_id_hex"], bytes(bob["signing_key"])
    ).like_post(post_id)
    polled = _until_notified(cli, bob["actor_id_hex"])
    assert not polled["isError"], f"the read arm lists the inbox: {polled!r}\n{cli.diagnostics()}"
    assert bob["actor_id_hex"] in {
        n.get("sender_id") for n in polled["structuredContent"]["notifications"]
    }, f"the like from the second actor is polled: {polled!r}"
    assert polled["structuredContent"]["unread"] >= 1, polled
    marked = cli.call("notifications_mark_read")
    assert not marked["isError"] and marked["structuredContent"]["marked_read"] >= 1, (
        f"the write arm marks the account's notifications read: {marked!r}\n{cli.diagnostics()}"
    )
    after_mark = cli.call("notifications_poll")
    assert after_mark["structuredContent"]["unread"] == 0, (
        f"after the mark the unread count reads 0: {after_mark!r}"
    )


@pytest.mark.feature("fauna-for-ai-assistants")
def test_the_argv_shell_is_the_same_principal_at_the_terminal(
    cli_binary, consent_nest, consent_bridge, tmp_path
):
    """The ``argv`` shell (``apps/cli.md`` § One catalog, many front-ends →
    *The ``argv`` shell*): the same binary driven by command lines, one process
    each, never a terminal — so every result is the tool's JSON.

    - **red first**: before any grant a command fails with the tool's own
      "call connect" refusal on stderr and exit code 1;
    - ``fauna-cli connect alice`` prints the code for *Connect an app*, waits,
      and exits 0 once the user has approved it — the connection is stored;
    - the next command resumes it: ``feed list`` prints exactly what an MCP
      session on the same credential namespace answers for ``feed_list``;
    - ``post create`` posts through the oracle door, and ``feed local`` serves
      the post;
    - ``post delete`` is destructive: off a terminal, without ``--yes``, it
      exits 2 and the post is still served; with ``--yes`` it deletes.
    """
    shell = CliArgv(cli_binary, consent_nest["url"], tmp_path)

    # ── Red first: nothing is reachable before the user's grant. ──
    code, before = shell.run("feed", "local")
    assert code == 1 and "call connect" in before.stderr and before.stdout == "", (
        before.diagnostics()
    )

    # ── `connect <handle>`: the code on stdout, the user's approval. ──
    connecting = shell.start("connect", "alice")
    started = json.loads(connecting.first_line(RPC_ROUNDTRIP_S))
    approve_typed_code_as(consent_nest, consent_nest["user"], started["user_code"])
    assert connecting.wait(ORCHESTRATION_STEP_S) == 0, connecting.diagnostics()

    # ── The stored connection, read by argv and by MCP alike. ──
    code, feeds = shell.run("feed", "list", "--json", timeout=RPC_ROUNDTRIP_S)
    assert code == 0, feeds.diagnostics()
    mcp = CliMcpSession(cli_binary, consent_nest["url"], tmp_path)
    try:
        mcp.initialize()
        served = mcp.call("feed_list")
        assert not served["isError"], f"{served!r}\n{mcp.diagnostics()}"
        assert feeds.json() == served["structuredContent"], (
            f"argv and MCP are one catalog: {feeds.json()!r} != {served['structuredContent']!r}"
        )
    finally:
        mcp.close()

    # ── Posting through the oracle door, served by the local timeline. ──
    code, posted = shell.run(
        "post", "create", "--text", "posted from the fauna-cli command line", timeout=RPC_ROUNDTRIP_S
    )
    assert code == 0, posted.diagnostics()
    post_id = posted.json()["post_id"]

    def local_ids() -> set[str]:
        code, local = shell.run("feed", "local", timeout=RPC_ROUNDTRIP_S)
        assert code == 0, local.diagnostics()
        return {p["post_id"] for p in local.json()["posts"]}

    assert post_id in local_ids(), "the local timeline serves the post the argv shell made"

    # ── A destructive command off a terminal needs --yes. ──
    code, refused = shell.run("post", "delete", post_id, timeout=RPC_ROUNDTRIP_S)
    assert code == 2 and "--yes" in refused.stderr, refused.diagnostics()
    assert post_id in local_ids(), "a refused delete deletes nothing"
    code, deleted = shell.run("post", "delete", post_id, "--yes", timeout=RPC_ROUNDTRIP_S)
    assert code == 0, deleted.diagnostics()
    assert post_id not in local_ids(), "with --yes the delete goes through"


def _until_served(cli: CliMcpSession, tool: str) -> dict:
    """Call ``tool`` until the shell's poll has turned into a session — the
    ``_until_connected`` wait for a tool other than the feed (convention 14)."""
    deadline = time.monotonic() + ORCHESTRATION_STEP_S
    while True:
        result = cli.call(tool)
        if not result["isError"] or "Waiting" not in cli.text(result):
            return result
        if time.monotonic() >= deadline:
            return result
        time.sleep(1.0)  # sleep-ok: paces a deadline poll on the shell's own state, never asserted on


@pytest.mark.feature("fauna-for-ai-assistants")
def test_the_admin_plane_serves_an_admin_consented_principal_and_no_member(
    cli, consent_nest, consent_bridge
):
    """The admin arms end to end (``authorization-server.md`` § Scope grammar →
    *The admin arms*; ``apps/bridges.md`` § Capability-allowlist enforcement →
    rule (e)), both ways:

    - **a member cannot grant an admin string**: alice, a member, approves the
      cli's ``connect(admin)`` card on her own session; the approve resolves
      nothing — the card stays pending in her tray and the cli keeps waiting
      on its code;
    - alice is then made the nest's second admin — a sole admin's
      ``fauna.admin.admins.add`` runs on its delay alone, brought due by the
      ``pending_actions/run_due`` test hook (``helpers.admin_wire.grant_admin``).
      Making the ceremony's own account the admin was cheaper than walking the
      claimer up the ATProto ladder: a grant needs the approving account's
      identity ACTIVE, which the fixture already arranged for alice;
    - **red before the grant**: a connection approved WITHOUT the admin arms
      serves no admin page's tool — one per page, the dashboard, users,
      settings, nest, logs, web, DNS and mail — each the nest's namespaced
      ``permission_denied``;
    - **served after it**: a second connection asking for every admin string
      the catalog names, approved by the admin, reads the dashboard and the
      member list, admits a member that the member list then carries,
      defines a tier the users page's tier list then names,
      and reads one kind of each nest-side page;
    - **the mail-side pages**, one case each on the same connection: a
      one-field mail policy save read back with the rest of its section kept,
      the DMARC reports and the forwarders read, the CalDAV port saved and
      read back, the CardDAV and WebDAV switches landing in the effective
      config, and the pending-bridge roster read — its approve a coaching
      prompt, never a tool. The red half for these pages (a page's string
      absent, a sovereign act refused) is pinned at tier_1
      (``principal_handlers::tests::the_mail_side_pages_serve_their_strings_and_never_a_sovereign_act``).
    """
    from helpers.admin_wire import grant_admin

    alice = consent_nest["user"]
    cli.initialize()

    # ── A member's approve of an admin string resolves nothing. ──
    started = cli.call("connect", {"handle": "alice", "admin": True}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    member_code = started["structuredContent"]["user_code"]
    card, answer = answer_typed_code_as(consent_nest, alice, member_code)
    assert answer == {"resolved": False}, (
        f"a member's approve of an admin string resolves nothing: {card!r} {answer!r}"
    )
    with user_session(consent_nest, alice) as ws:
        pending = pending_consents(ws, _CLIENT_ID)
        assert card["consent_id"] in {c["consent_id"] for c in pending}, (
            f"a member's approve of an admin string must leave the card pending: {pending!r}"
        )
        waiting = cli.call("admin_dashboard")
        assert waiting["isError"] and member_code in cli.text(waiting), (
            f"the cli keeps waiting on the member's unresolved card: {waiting!r}\n{cli.diagnostics()}"
        )
        declined = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": card["consent_id"], "approved": False},
        )
        assert declined["resolved"], f"the member declines the card: {declined!r}"

    # ── Alice becomes the nest's second admin (kept on the nest: a later grant
    # would wait for a quorum — `_admin_connected`). ──
    grant_admin(consent_nest, alice)
    consent_nest["cli_admin_member"] = alice

    # ── Red before the grant: no admin arm, no admin tool. ──
    started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    approve_typed_code_as(consent_nest, alice, started["structuredContent"]["user_code"])
    local = _until_connected(cli)
    assert not local["isError"], f"the plain connection completes: {local!r}\n{cli.diagnostics()}"
    for tool in (
        "admin_dashboard",
        "admin_users_list",
        "admin_membership_tiers_list",
        "admin_region_get",
        "admin_audit_list",
        "admin_apex_actor_get",
        "admin_domains_list",
        "admin_mail_settings",
    ):
        refused = cli.call(tool)
        assert refused["isError"] and ".permission_denied" in cli.text(refused), (
            f"{tool} without the admin arms is the nest's refusal: {refused!r}\n{cli.diagnostics()}"
        )

    # ── Served after it: the admin approves the admin arms. ──
    started = cli.call("connect", {"handle": "alice", "admin": True}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    approve_typed_code_as(consent_nest, alice, started["structuredContent"]["user_code"])
    dashboard = _until_served(cli, "admin_dashboard")
    assert not dashboard["isError"], (
        f"the admin's principal reads the dashboard: {dashboard!r}\n{cli.diagnostics()}"
    )
    assert dashboard["structuredContent"]["stats"]["total_users"] >= 2, dashboard
    listed = cli.call("admin_users_list")
    assert not listed["isError"], f"rw:users covers the member list: {listed!r}"
    assert alice["actor_id_hex"] in {u["actor_id"] for u in listed["structuredContent"]["users"]}, (
        f"the list carries alice's hex actor id: {listed!r}"
    )
    admitted = os.urandom(32).hex()
    admit = cli.call("admin_users_admit", {"actor_id": admitted, "tier": "free", "handle": "carol"})
    assert not admit["isError"], f"rw:users admits a member: {admit!r}\n{cli.diagnostics()}"

    # ── The nest-side pages, one served case each. ──
    tier = f"cli-{os.urandom(3).hex()}"
    caps = {
        "max_inbox_bytes": 1 << 20,
        "max_storage_bytes": 1 << 30,
        "max_devices": 3,
        "max_blob_size": 1 << 20,
        "max_feeds": 10,
    }
    defined = cli.call("admin_tier_create", {"name": tier, **caps})
    assert not defined["isError"], f"rw:settings defines a tier: {defined!r}\n{cli.diagnostics()}"
    tiers = cli.call("admin_tiers_list")
    assert tier in cli.text(tiers), f"the defined tier is listed: {tiers!r}"
    for tool, page in (
        ("admin_membership_tiers_list", "rw:settings"),
        ("admin_region_get", "rw:nest"),
        ("admin_audit_list", "read:logs"),
        ("admin_apex_actor_get", "rw:web"),
        ("admin_domains_list", "rw:dns"),
        ("admin_transport_get", "rw:nest"),
        ("admin_content_rules_get", "rw:nest"),
        ("admin_cluster_status", "rw:nest"),
        ("admin_domain_rename_status", "rw:dns"),
    ):
        served = cli.call(tool)
        assert not served["isError"], f"{page} serves {tool}: {served!r}\n{cli.diagnostics()}"
    # ── The mail-side pages, one case each, each write read back. ──
    def served_data(tool: str, arguments: dict | None = None) -> dict:
        result = cli.call(tool, arguments)
        assert not result["isError"], f"{tool} is served: {result!r}\n{cli.diagnostics()}"
        return result["structuredContent"]

    # admin-mail: a one-field policy save keeps the rest of the section.
    before = served_data("admin_mail_settings")["alias_policy"]
    served_data("admin_mail_policy_set", {"section": "alias", "values": {"exact_aliases_max": 7}})
    after = served_data("admin_mail_settings")["alias_policy"]
    assert after["exact_aliases_max"] == 7, after
    assert after["reserved_local_parts"] == before["reserved_local_parts"], (before, after)
    # admin-mail-reports: the received reports read (none on a fresh nest).
    assert "reports" in served_data("admin_mail_reports_dmarc")
    # admin-aliases: the forwarders read.
    assert "forwarders" in served_data("admin_forwarders_list")
    # admin-calendar: the port saved and read back.
    served_data("admin_calendar_port_set", {"port": 8444})
    assert served_data("admin_calendar_port_get")["port"] == 8444
    # admin-contacts and admin-files: each switch lands in the effective config.
    served_data("admin_contacts_enabled_set", {"enabled": True})
    served_data("admin_files_enabled_set", {"enabled": True})
    config = served_data("admin_mail_settings")
    assert config["carddav_enabled"] and config["webdav_enabled"], config
    # admin-bridges-pending: the roster reads; its approve is a coaching
    # prompt, never a tool, and the sovereign mail acts are no tool either.
    assert "bridges" in served_data("admin_bridges_pending_list")
    tools = cli.request("tools/list")["tools"]
    roster_writes = [
        t["name"]
        for t in tools
        if t["name"].startswith("admin_bridges_") and not t["annotations"]["readOnlyHint"]
    ]
    assert not roster_writes, f"the pending-bridge roster offers reads only: {roster_writes}"
    # DKIM's key acts (mint, rotate, reveal) are no tool; its rotation
    # interval and selector list on admin-dns are (`apps/cli.md` § Implementation
    # status today, the admin plane's served kinds).
    mail_secrets = [
        t["name"]
        for t in tools
        if any(
            s in t["name"]
            for s in ("srs", "unsubscribe_secret", "microsoft", "dkim_key", "dkim_rotate")
        )
    ]
    assert not mail_secrets, f"the mail page's secrets are no tool: {mail_secrets}"
    coached = cli.request("prompts/get", {"name": "approve_pending_bridge"})
    assert "Pending approval" in coached["messages"][0]["content"]["text"], coached

    # A nest page's document write: the connection limit set, read back, and
    # returned to the default with a stated null.
    limit = cli.call("admin_transport_set", {"max_conns_per_ip": 77})
    assert not limit["isError"], f"rw:nest sets the connection limit: {limit!r}\n{cli.diagnostics()}"
    assert cli.call("admin_transport_get")["structuredContent"]["max_conns_per_ip"] == 77
    assert not cli.call("admin_transport_set", {"max_conns_per_ip": None})["isError"]

    # The domain add is guarded: the first domain becomes the never-removable primary
    # domain, so with none active the tool refuses and says who adds it.
    domains = cli.call("admin_domains_list")["structuredContent"]
    added = cli.call("admin_domain_add", {"domain": "added-by-cli.example"})
    if domains["active"]:
        assert not added["isError"], f"rw:dns adds a domain: {added!r}\n{cli.diagnostics()}"
        assert not cli.call("admin_domain_remove", {"domain": "added-by-cli.example"})["isError"]
    else:
        assert added["isError"] and "own Fauna app" in cli.text(added), (
            f"no active domain: the add is refused, not performed: {added!r}"
        )

    # ── The member list carries the member the cli admitted. ──
    listed = cli.call("admin_users_list")
    assert admitted in {u["actor_id"] for u in listed["structuredContent"]["users"]}, (
        f"the member list carries the member the cli admitted ({admitted[:12]}…): {listed!r}"
    )


@pytest.mark.feature("fauna-for-ai-assistants")
def test_the_http_listener_refuses_a_foreign_origin_and_host(cli_binary, tmp_path):
    """``fauna-cli mcp --listen`` has no auth header — loopback is the trust
    boundary — so a page in the user's browser reaching the port through DNS
    rebinding is the one attack the listener closes itself (MCP's streamable-
    HTTP rule): a foreign ``Origin``, or a ``Host`` that is not the listener's
    own loopback name, is refused ``403`` and never reaches the method layer,
    while the same request from a loopback origin is served."""
    session = CliMcpSession(cli_binary, "http://127.0.0.1:1", tmp_path, transport="http")
    try:
        ping = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "ping"})
        port = session.http_port
        assert port is not None
        for host, origin in (
            ("", "https://evil.example"),
            ("", "null"),
            (f"evil.example:{port}", None),
            (None, None),
        ):
            status, body = session.http_exchange(ping, host=host, origin=origin)
            assert status == 403 and "jsonrpc" not in body, (
                f"host={host!r} origin={origin!r} must be refused: {status} {body!r}\n"
                f"{session.diagnostics()}"
            )
        status, body = session.http_exchange(ping, origin=f"http://localhost:{port}")
        assert status == 200 and json.loads(body)["result"] == {}, (status, body)
        assert session.http_exchange("", method="GET")[0] == 405
        # A body that is not JSON is refused 415 before the method layer, even
        # from a loopback origin: the listener speaks JSON-RPC and nothing else.
        status, body = session.http_exchange(ping, content_type="text/plain")
        assert status == 415 and "jsonrpc" not in body, (
            f"a text/plain POST must be refused 415: {status} {body!r}\n{session.diagnostics()}"
        )
    finally:
        session.close()


def _connect_account(cli: CliMcpSession, nest: dict, user: dict, handle: str, **flags) -> None:
    """Connect ``user`` (ATProto identity active) to ``cli`` — the typed code
    answered for the human and approved on the user's own session — and wait
    for the poll to turn into a session. ``flags`` ride ``connect`` (``admin``)."""
    started = cli.call("connect", {"handle": handle, **flags}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    approve_typed_code_as(nest, user, started["structuredContent"]["user_code"])
    served = _until_connected(cli)
    assert not served["isError"], f"the connection completes: {served!r}\n{cli.diagnostics()}"


@pytest.mark.feature("fauna-for-ai-assistants")
def test_the_feed_tools_read_one_feeds_definition_and_its_posts(
    cli, consent_nest, consent_bridge
):
    """``feed_list`` names the account's feeds; ``feed_get`` reads one feed's
    definition by the id the list carries, ``feed_posts`` the posts it selects,
    and ``feed_trending`` the nest's trending posts — each over the account arm,
    app-less. A feed id the account does not hold is the nest's refusal, never an
    empty success."""
    alice = consent_nest["user"]
    cli.initialize()
    _connect_account(cli, consent_nest, alice, "alice")

    actor = ApiActor(
        consent_nest["url"], alice["token"], alice["actor_id_hex"], bytes(alice["signing_key"])
    )
    name = f"cli-feed-{os.urandom(3).hex()}"
    feed_id = actor.create_feed(name)
    # The post is the cli's own (the oracle door): the catch-all feed indexes it.
    posted = cli.call("post_create", {"text": f"seen through feed_posts {name}"})
    assert not posted["isError"], f"post_create: {posted!r}\n{cli.diagnostics()}"
    post_id = posted["structuredContent"]["post_id"]

    listed = cli.call("feed_list")
    assert not listed["isError"], f"{listed!r}\n{cli.diagnostics()}"
    assert feed_id in json.dumps(listed["structuredContent"]), (
        f"feed_list names the feed the account made: {listed!r}"
    )

    got = cli.call("feed_get", {"feed_id": feed_id})
    assert not got["isError"], f"feed_get reads the feed the list names: {got!r}\n{cli.diagnostics()}"
    assert name in json.dumps(got["structuredContent"]), (
        f"feed_get carries the feed's definition: {got!r}"
    )

    def feed_post_ids() -> set[str] | None:
        result = cli.call("feed_posts", {"feed_id": feed_id})
        assert not result["isError"], f"feed_posts reads the feed: {result!r}\n{cli.diagnostics()}"
        ids = {p["post_id"] for p in result["structuredContent"]["posts"]}
        return ids if post_id in ids else None

    # Ingest indexes the post into the feed asynchronously: poll the feed's own
    # state (convention 14).
    wait_until(feed_post_ids, RPC_ROUNDTRIP_S, diagnose=lambda: cli.call("feed_posts", {"feed_id": feed_id}))

    trending = cli.call("feed_trending")
    assert not trending["isError"], f"feed_trending reads: {trending!r}\n{cli.diagnostics()}"
    assert isinstance(trending["structuredContent"].get("posts"), list), trending

    missing = cli.call("feed_get", {"feed_id": os.urandom(16).hex()})
    assert missing["isError"], f"a feed the account does not hold is refused: {missing!r}"


def _admin_connected(cli: CliMcpSession, nest: dict, handle: str) -> dict:
    """The nest's second admin connected to ``cli`` with every admin string.
    ``grant_admin`` needs the nest's sole admin (a later grant would wait for a
    quorum and expire), so the member it makes is kept on ``nest`` and shared:
    by the admin-plane journey (alice) and by these tests, in any order."""
    from helpers.admin_wire import grant_admin

    member = nest.get("cli_admin_member")
    if member is None:
        # Alice, whose ATProto identity the fixture already activated.
        member = nest["user"]
        grant_admin(nest, member)
        nest["cli_admin_member"] = member
    cli.initialize()
    _connect_account(cli, nest, member, member["handle"], admin=True)
    return member


@pytest.mark.feature("fauna-for-ai-assistants")
def test_admission_admits_a_supervised_member_under_a_guardian_and_an_age_band(
    cli, consent_nest, consent_bridge
):
    """``admin_users_admit`` with ``guardian_actor`` and ``age_band`` (the
    direct-admission arm of ``family-safety.md`` § The guardianship link): the
    admitted account's OWN family status names the guardian and reads the band
    back, the member list carries it, and a band without a guardian is the
    nest's refusal. ``admin_registration_mode_get`` reads the posture."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from nacl.signing import SigningKey

    _admin_connected(cli, consent_nest, "admitadmincli")
    guardian = admit_adult(consent_nest, "admitguardiancli", domain=HANDLE_DOMAIN)

    tools = {t["name"]: t for t in cli.request("tools/list")["tools"]}
    bands = tools["admin_users_admit"]["inputSchema"]["properties"]["age_band"]["enum"]
    assert bands, "the catalog lists the age bands from the shared type"
    band = bands[0]

    ward_key = SigningKey.generate()
    ward_hex = bytes(ward_key.verify_key).hex()
    admitted = cli.call(
        "admin_users_admit",
        {
            "actor_id": ward_hex,
            "tier": "free",
            "handle": "admitwardcli",
            "guardian_actor": guardian["actor_id_hex"],
            "age_band": band,
        },
    )
    assert not admitted["isError"], f"a guardian admission is served: {admitted!r}\n{cli.diagnostics()}"

    listed = cli.call("admin_users_list")
    assert ward_hex in {u["actor_id"] for u in listed["structuredContent"]["users"]}, (
        f"the member list carries the supervised member: {listed!r}"
    )

    def ward_status():
        try:
            with WsRpcAdminClient(
                consent_nest["url"], actor_id=bytes(ward_key.verify_key), signing_key=bytes(ward_key)
            ) as me:
                return me.call("fauna.family.status", {})
        except Exception:  # the admitted account authenticates once the admission lands
            return None

    status = wait_until(ward_status, RPC_ROUNDTRIP_S, diagnose=lambda: "the ward never authenticated")
    supervised_by = status.get("supervised_by") or {}
    assert bytes(supervised_by["actor_id"]).hex() == guardian["actor_id_hex"], (
        f"the admitted account is supervised by the guardian: {status!r}"
    )
    assert (status.get("age_band") or {}).get("band") == band, (
        f"the admitted account reads the band the admission stated: {status!r}"
    )

    bandless = cli.call(
        "admin_users_admit",
        {"actor_id": os.urandom(32).hex(), "tier": "free", "handle": "bandlesscli", "age_band": band},
    )
    assert bandless["isError"], f"a band without a guardian is refused by the nest: {bandless!r}"

    posture = cli.call("admin_registration_mode_get")
    assert not posture["isError"], f"the registration posture reads: {posture!r}\n{cli.diagnostics()}"
    assert posture["structuredContent"], posture


@pytest.mark.feature("fauna-for-ai-assistants")
def test_the_admin_reads_one_dmarc_reports_raw_archive(
    cli, consent_nest, consent_bridge, mail_bridge_binary, seal_helper_binary, tmp_path
):
    """``admin_mail_reports_dmarc_get`` (outcome 13's "reports down to a
    report's raw archive"): a DMARC aggregate report mailed into the nest's MX
    for its own domain is listed by ``admin_mail_reports_dmarc``, and the get,
    named by the entry's three keys, answers that entry and the report's XML as
    text, whole.

    The report is seeded the only way one enters a nest — mailed to its MX —
    so the consent nest gets an MTA bridge of its own here
    (``conftest._spawn_mta_bridge``). This test owns the nest-global mail state
    that needs (`_spawn_mta_bridge`'s contract): the claim already registered
    ``HANDLE_DOMAIN`` as the primary mail domain; the admin clears the
    DNS-dependent perimeter gates and opens the mail gate BEFORE the spawn, so
    the bridge binds at once. Nothing else on the module's nest reads mail
    state, so leaving it on is harmless to the tests after this one."""
    from types import SimpleNamespace

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import MTA_FIXTURE_SPAM_POSTURE, _spawn_mta_bridge
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd
    from helpers.dmarc_reports import mail_report, report_message, report_xml

    admin = consent_nest["admin"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as admin_ws:
        admin_ws.call("fauna.bridges.put_spam_policy", dict(MTA_FIXTURE_SPAM_POSTURE))
        admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})

    fake_clamd = FakeClamd().start()
    fake_rspamd = FakeRspamd().start()
    mta_tmp = tmp_path / "mta"
    mta_tmp.mkdir()
    mta = _spawn_mta_bridge(
        mail_bridge_binary=mail_bridge_binary,
        seal_helper_binary=seal_helper_binary,
        nest_instance=consent_nest,
        tmp=mta_tmp,
        bridge_id="cli-dmarc-mta-1",
        domain=HANDLE_DOMAIN,
        operator_hatch_extra=(
            f'clamd_addr = "{fake_clamd.addr}"\n'
            f'rspamd_url = "{fake_rspamd.url}"\n'
        ),
    )
    try:
        report_id = f"cli-raw-{int(time.time() * 1000)}"
        org = "cli-reporter.test"
        xml = report_xml(HANDLE_DOMAIN, report_id, org_name=org)
        rcpt = f"dmarc-report@{HANDLE_DOMAIN}"
        # The 250 on `.` is the barrier: the hand-off to the nest completes
        # inside DATA, before the reply.
        mail_report(
            SimpleNamespace(mx_port=mta.mx_port, domain=HANDLE_DOMAIN),
            rcpt,
            report_message(rcpt, xml, report_id),
        )
    finally:
        mta.cleanup()
        fake_clamd.stop()
        fake_rspamd.stop()

    _admin_connected(cli, consent_nest, "dmarcadmincli")

    listed = _until_served(cli, "admin_mail_reports_dmarc")
    assert not listed["isError"], f"the report list reads: {listed!r}\n{cli.diagnostics()}"
    entries = [
        r for r in listed["structuredContent"]["reports"] if r["report_id"] == report_id
    ]
    assert len(entries) == 1, f"the mailed report is listed once: {listed!r}"
    entry = entries[0]
    assert entry["our_domain"] == HANDLE_DOMAIN.lower(), entry
    assert entry["submitter"] == f"{org} <dmarc-noreply@reporter.test>", entry

    got = cli.call(
        "admin_mail_reports_dmarc_get",
        {
            "our_domain": entry["our_domain"],
            "submitter": entry["submitter"],
            "report_id": entry["report_id"],
        },
    )
    assert not got["isError"], f"the get reads the listed report: {got!r}\n{cli.diagnostics()}"
    answer = got["structuredContent"]
    assert answer["report"]["report_id"] == report_id, answer["report"]
    assert answer["report"]["submitter"] == entry["submitter"], answer["report"]
    assert answer["raw"] == xml, "the raw view is the report's XML as the provider sent it"
    assert answer["raw_truncated"] is False, answer
    assert answer["raw_bytes"] == len(xml.encode()), answer

    missing = cli.call(
        "admin_mail_reports_dmarc_get",
        {"our_domain": entry["our_domain"], "submitter": entry["submitter"], "report_id": "no-such-report"},
    )
    assert missing["isError"], f"a report the nest never stored is refused: {missing!r}"


#: Name fragments no sovereign act may carry in a tool (`apps/cli.md` § The
#: sovereign-act boundary): the admin grant plane, key material and secrets,
#: identity acts, and what a user cannot recreate.
_SOVEREIGN_FRAGMENTS = (
    "admins_add", "admins_remove", "admin_add", "admin_remove",
    "bridge_approve", "bridges_approve", "bridge_reject", "bridges_reject",
    "bridge_revoke", "bridges_revoke", "custody", "pending_action",
    "seed", "issuer", "session_secret", "tls_cert", "dkim_key", "dkim_rotate",
    "srs", "unsubscribe_secret", "client_secret", "handle_clear", "clear_handle",
    "users_delete", "user_delete", "evict", "takedown", "factory_reset",
    "domain_rename_start", "domain_rename_set", "domain_rename_cancel",
)
#: Pages whose every mutation is sovereign: no tool at all, or reads only.
_SOVEREIGN_PAGES = ("devices", "sessions", "nests", "backups", "connected-apps")


@pytest.mark.feature("fauna-for-ai-assistants")
def test_the_catalog_names_no_tool_for_a_sovereign_act(cli, consent_nest, consent_bridge):
    """The sovereign-never list read against what a host sees (``apps/cli.md`` §
    The sovereign-act boundary): ``tools/list`` is exactly the catalog snapshot;
    it carries no ``onboarding`` page tool and, on the devices, sessions, nests,
    backups and connected-apps pages, nothing that mutates (``connect`` is the
    one ceremony the user approves); and no tool name spells an admin grant, a
    key or secret act, a handle clear, a domain rename, a user delete, an
    eviction or the factory reset. Read on an admin-consented connection, whose
    host is offered the widest list."""
    import yaml

    _admin_connected(cli, consent_nest, "sovereignadmincli")
    tools = {t["name"]: t for t in cli.request("tools/list")["tools"]}

    snapshot = yaml.safe_load(
        (Path(__file__).resolve().parent.parent / "catalog-actual-cli.yaml").read_text()
    )
    pages = {t["name"]: t["page"] for t in snapshot["tools"]}
    assert set(pages) <= set(tools), (
        f"tools/list lacks catalog tools: {sorted(set(pages) - set(tools))}"
    )

    onboarding = [n for n, p in pages.items() if p.startswith("onboarding")]
    assert not onboarding, f"the onboarding ceremony is no tool: {onboarding}"
    sovereign_writes = [
        n
        for n, p in pages.items()
        if p in _SOVEREIGN_PAGES and n != "connect" and not tools[n]["annotations"]["readOnlyHint"]
    ]
    assert not sovereign_writes, f"the grant plane offers reads only: {sovereign_writes}"
    # A read of the same plane (a roster, a status) is allowed; the act is not.
    spelled = sorted(
        n
        for n, t in tools.items()
        if not t["annotations"]["readOnlyHint"] and any(f in n for f in _SOVEREIGN_FRAGMENTS)
    )
    assert not spelled, f"a sovereign act is no tool: {spelled}"


def _activate_atproto(nest: dict, user: dict) -> None:
    """``user``'s ATProto identity made ACTIVE over the wire — the fixture's own
    precondition for alice (``helpers/atproto_consent.py``), since a grant is
    refused unless the approving account's identity is active; a precondition,
    not the subject (convention 8's fixture exemption)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.atproto_consent import USER_ROTATION_PUB

    ws = WsRpcAdminClient(
        nest["url"], actor_id=user["actor_id_bytes"], signing_key=bytes(user["signing_key"])
    )
    with ws:
        ws.call(
            "fauna.bridges.atproto.set_integration_level",
            {
                "target_level": "hosted_full",
                "did_method": "plc",
                "user_rotation_pub_did_key": USER_ROTATION_PUB,
                "history_backfill": False,
            },
        )


def _until_served_as(cli: CliMcpSession, account: str) -> dict:
    """``feed_local`` for ``account`` until its consent's poll has turned into a
    session (convention 14)."""
    deadline = time.monotonic() + ORCHESTRATION_STEP_S
    while True:
        result = cli.call("feed_local", {"account": account})
        if not result["isError"] or "Waiting" not in cli.text(result):
            return result
        if time.monotonic() >= deadline:
            return result
        time.sleep(1.0)  # sleep-ok: paces a deadline poll on the shell's own state, never asserted on


@pytest.mark.feature("fauna-for-ai-assistants")
def test_two_accounts_are_two_principals_and_a_call_names_which(
    cli, cli_binary, consent_nest, consent_bridge, tmp_path
):
    """Several accounts are several principals (``apps/cli.md`` § The device
    shell): alice and bob each approve the cli's card on their own session; with
    both held, a call that names no account is refused naming both, a call
    naming one acts as that account — each post's author is the account the
    call named — and a relaunch resumes both from the credential store with no
    new ceremony."""
    alice = consent_nest["user"]
    bob = admit_adult(consent_nest, "bobaccounts", domain=HANDLE_DOMAIN, token=True)
    _activate_atproto(consent_nest, bob)
    cli.initialize()

    started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    assert started["structuredContent"]["account"] == "alice", started
    approve_typed_code_as(consent_nest, alice, started["structuredContent"]["user_code"])
    one = _until_connected(cli)
    assert not one["isError"], f"alice's connection completes: {one!r}\n{cli.diagnostics()}"

    started = cli.call("connect", {"handle": "@bobaccounts"}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    assert started["structuredContent"]["account"] == "bobaccounts", started
    approve_typed_code_as(consent_nest, bob, started["structuredContent"]["user_code"])
    served = _until_served_as(cli, "bobaccounts")
    assert not served["isError"], f"bob's connection completes: {served!r}\n{cli.diagnostics()}"

    # ── Both held: a call names its account. ──
    unnamed = cli.call("feed_local")
    assert unnamed["isError"], f"with two accounts a call must name one: {unnamed!r}"
    assert "alice" in cli.text(unnamed) and "bobaccounts" in cli.text(unnamed), unnamed
    authors = {}
    for account, user in (("alice", alice), ("bobaccounts", bob)):
        posted = cli.call(
            "post_create", {"text": f"posted for {account} by fauna-cli", "account": account}
        )
        assert not posted["isError"], f"{account} posts: {posted!r}\n{cli.diagnostics()}"
        authors[posted["structuredContent"]["post_id"]] = user["actor_id_hex"]
    listed = cli.call("feed_local", {"account": "alice"})
    seen = {p["post_id"]: p["author"] for p in listed["structuredContent"]["posts"]}
    for post_id, author in authors.items():
        assert seen.get(post_id) == author, (
            f"each post's author is the account its call named: {post_id} → {seen.get(post_id)!r}, "
            f"want {author}"
        )

    # ── A relaunch resumes both, with no new ceremony. ──
    cli.close()
    relaunched = CliMcpSession(cli_binary, consent_nest["url"], tmp_path)
    try:
        relaunched.initialize()
        again = relaunched.call("feed_local")
        assert again["isError"] and "bobaccounts" in relaunched.text(again), (
            f"the relaunched shell holds both accounts: {again!r}\n{relaunched.diagnostics()}"
        )
        for account in ("alice", "bobaccounts"):
            read = relaunched.call("feed_local", {"account": account})
            assert not read["isError"], (
                f"{account} resumes from the store: {read!r}\n{relaunched.diagnostics()}"
            )
    finally:
        relaunched.close()


@pytest.mark.feature("fauna-for-ai-assistants")
def test_with_no_os_store_the_connection_rests_in_a_declared_0600_file_and_resumes(
    cli_binary, consent_nest, consent_bridge, tmp_path
):
    """The unattended arm (``apps/cli.md`` § The device shell →
    *Credentials*): launched as on a box with no usable OS secret store, the
    shell's ``connect`` answer declares that the connection rests on disk under
    file permissions alone, the namespace rests in a ``0600`` file under the
    shell's own data dir — never a sealed one — and a relaunch resumes the
    account with no new ceremony."""
    shell = CliMcpSession(cli_binary, consent_nest["url"], tmp_path, store="unattended")
    try:
        shell.initialize()
        started = shell.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
        assert not started["isError"], f"connect refused: {started!r}\n{shell.diagnostics()}"
        custody = started["structuredContent"].get("custody")
        assert custody and "file permissions alone" in custody, (
            f"the connect answer declares the unattended arm: {started!r}"
        )
        assert custody in started["structuredContent"]["for_the_user"], started
        approve_typed_code_as(
            consent_nest, consent_nest["user"], started["structuredContent"]["user_code"]
        )
        served = _until_connected(shell)
        assert not served["isError"], f"the connection completes: {served!r}\n{shell.diagnostics()}"
    finally:
        shell.close()

    data_dir = tmp_path / "xdg-config" / "fauna-cli"
    rests = data_dir / "fauna-cli.json"
    assert rests.is_file(), f"the namespace rests under the data dir: {sorted(data_dir.iterdir())}"
    assert stat.S_IMODE(rests.stat().st_mode) == 0o600, oct(rests.stat().st_mode)
    assert not list(data_dir.glob("*.sealed")), "the unattended arm never creates a sealed file"
    assert not any((tmp_path / "credentials").iterdir()), "the e2e store was never used"

    relaunched = CliMcpSession(cli_binary, consent_nest["url"], tmp_path, store="unattended")
    try:
        relaunched.initialize()
        read = relaunched.call("feed_local")
        assert not read["isError"], (
            f"the relaunch resumes from the file with no new ceremony: {read!r}\n"
            f"{relaunched.diagnostics()}"
        )
    finally:
        relaunched.close()


@pytest.mark.feature("fauna-for-ai-assistants")
def test_a_dropped_session_is_redialled_a_read_runs_again_and_a_write_never_does(
    cli, consent_nest, consent_bridge
):
    """A principal session the nest dropped (``apps/cli.md`` § The device shell
    → *A dropped session*): the nest restarts in place (``restart_nest``, the
    benign flip) under a connected shell; the next read redials and is served,
    with no "connection lost" reaching the host; after a second restart the
    next write — ``notifications_mark_read``, idempotent but a write — is
    redialled but not run, its answer saying so, and the call after it is
    served. Last in the module: the restart is benign, but no later test
    should share its window."""
    from common.nest import restart_nest

    cli.initialize()
    started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    approve_typed_code_as(
        consent_nest, consent_nest["user"], started["structuredContent"]["user_code"]
    )
    local = _until_connected(cli)
    assert not local["isError"], f"the connection completes: {local!r}\n{cli.diagnostics()}"

    restart_nest(consent_nest, graceful=True)
    read = cli.call("feed_local", timeout=RPC_ROUNDTRIP_S)
    assert not read["isError"], (
        f"a read after the nest restarted is redialled and served: {read!r}\n{cli.diagnostics()}"
    )

    restart_nest(consent_nest, graceful=True)
    write = cli.call("notifications_mark_read", timeout=RPC_ROUNDTRIP_S)
    assert write["isError"] and "not retried" in cli.text(write), (
        f"a write the drop interrupted is never retried blind: {write!r}\n{cli.diagnostics()}"
    )
    again = cli.call("notifications_mark_read", timeout=RPC_ROUNDTRIP_S)
    assert not again["isError"], (
        f"the redial left a live session for the next call: {again!r}\n{cli.diagnostics()}"
    )

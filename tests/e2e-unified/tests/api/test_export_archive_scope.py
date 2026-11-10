"""tier_3 API: what an export archive carries, and under which credential
(export-my-data outcomes 5 and 6).

Goal doc: ``docs/goal/architecture/account-data-taxonomy.md`` § Implementation
status today — the eviction token pulls the archive the owner does (auth
strength does not tier it); the actor's own audit rows ride with the log's hash
chain left home; the report plane is withheld; the guardian family exports on
the ward's own column only. The Rust tests in ``bins/fauna-nest/tests/export_api.rs``
(``the_eviction_token_pulls_the_same_archive_the_owner_does``,
``the_actors_own_conduct_rides_while_the_hash_chain_stays_home``,
``the_report_plane_never_reaches_an_archive``) and
``db/actor_tables.rs::the_guardian_family_exports_only_on_the_wards_own_column``
pin the store half; these witness it over a real nest binary through the export
door every app calls, ``GET /api/v1/export``, with every precondition arranged
by ordinary user and admin acts.

A DEDICATED nest: the tests schedule account deletions and evictions, which
must never meet another test's ``pending_actions/run_due``.

Latency-independent (convention 14): every step is a request with a reply.
"""

from __future__ import annotations

import io
import json
import secrets
import time
import zipfile

import pytest
import requests

from common.auth import create_actor_and_register, mint_token_via_handshake
from helpers.admin_wire import admin_client, admit_ward_directly
from helpers import budgets, waiting
from helpers.family_wire import actor_ws
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def export_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "export-scope")
    try:
        yield nest
    finally:
        cleanup()


def _member(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )


def _archive(nest, bearer: str) -> dict[str, bytes]:
    """``GET /api/v1/export?include_blobs=true`` under ``bearer``: entry → bytes."""
    resp = requests.get(
        f"{nest['url']}/api/v1/export",
        params={"include_blobs": "true"},
        headers={"Authorization": f"Bearer {bearer}"},
        timeout=120,
    )
    assert resp.status_code == 200, f"export returned {resp.status_code}: {resp.text[:400]}"
    with zipfile.ZipFile(io.BytesIO(resp.content)) as z:
        return {name: z.read(name) for name in z.namelist()}


def _rows(entries: dict[str, bytes], table: str) -> list[dict]:
    body = entries.get(f"export/tables/{table}.ndjson", b"")
    return [json.loads(line) for line in body.splitlines() if line.strip()]


def _post(nest, author: dict, words: str) -> str:
    return ws_api.create_post(
        nest["port"],
        author,
        sign_and_encode_post(author["signing_key"], int(time.time() * 1_000_000), words, tags=[]),
    )


@pytest.mark.feature("export-my-data")
def test_an_evicted_members_export_is_the_archive_the_owner_gets(export_nest):
    """Outcome 5: the admin evicts a member; the member is handed an export
    token, and that token's archive is the one the member's own sign-in pulls —
    the same entries, byte for byte (the manifest's generation stamp and each
    pull's own notice to the member aside)."""
    nest = export_nest
    member = _member(nest)
    _post(nest, member, f"something of mine to take along {secrets.token_hex(4)}")

    with admin_client(nest) as admin:
        admin.call(
            "fauna.admin.users.evict",
            {
                "actor_id": bytes.fromhex(member["actor_id_hex"]),
                "reason": "moving on",
                "category": "capacity",
            },
        )
    with actor_ws(nest, member) as ws:
        eviction = ws.call("fauna.account.get", {}).get("eviction") or {}
    token = eviction.get("export_token")
    assert token, f"an evicted member is handed an export token: {eviction!r}"

    # Every pull notifies the member of itself (`export_routes.rs`, the
    # `ArchiveExported` security notice: an inbox envelope, a notification and
    # its links), delivered asynchronously, so the second pull of a pair holds
    # the first one's notice. Those three planes are the pulls' own trace, not
    # the credential's reach; every other entry must match byte for byte. The
    # eviction's own notice may still be landing, so the pair is pulled until it
    # agrees, on a named budget (convention 14): an archive tiered by credential
    # never converges.
    last: dict = {}

    def _compared(archive: dict[str, bytes]) -> dict[str, bytes]:
        return {
            n: b for n, b in archive.items()
            if n != "export/manifest.json"
            and not n.startswith("export/inbox/")
            and n not in ("export/tables/notifications.ndjson", "export/tables/content_links.ndjson")
        }

    def _same() -> bool:
        owner = _compared(_archive(nest, member["token"]))
        evicted = _compared(_archive(nest, token))
        last["owner"], last["evicted"] = owner, evicted
        return owner == evicted

    def _diff() -> str:
        owner, evicted = last.get("owner", {}), last.get("evicted", {})
        changed = [n for n in owner if n in evicted and owner[n] != evicted[n]]
        return (
            "the eviction token's archive never matched the owner's: "
            f"only-owner {sorted(set(owner) - set(evicted))!r}, "
            f"only-token {sorted(set(evicted) - set(owner))!r}, differing {changed!r}"
        )

    waiting.wait_until(_same, budgets.RPC_ROUNDTRIP_S, interval=1.0, diagnose=_diff)
    assert any(n.startswith("export/tables/") for n in last["owner"]), (
        f"precondition: the owner's archive carries the member's tables: {sorted(last['owner'])!r}"
    )


@pytest.mark.feature("export-my-data")
def test_an_archive_holds_only_what_is_the_members_own(export_nest):
    """Outcome 6, three exclusions, each beside a control that rides:

    * audit — the member's own row (scheduling its deletion) rides; another
      member's row of the same kind does not; no row carries the log's
      ``prev_hash``/``entry_hash`` chain;
    * reports — a report another member files about the member's post, with
      the member named as its subject, reaches no entry of the member's
      archive (its note phrase is nowhere; no report table rides);
    * family — a supervised child's guardianship family rides in the child's
      archive and in no entry of the guardian's."""
    nest = export_nest
    member, other, reporter = _member(nest), _member(nest), _member(nest)

    # ── audit ──
    with actor_ws(nest, member) as ws:
        mine = ws.call("fauna.account.delete", {})["pending_action_id"]
    with actor_ws(nest, other) as ws:
        theirs = ws.call("fauna.account.delete", {})["pending_action_id"]

    # ── reports ──
    post_id = _post(nest, member, f"a post someone will report {secrets.token_hex(4)}")
    note = f"report-note-{secrets.token_hex(8)}"
    with actor_ws(nest, reporter) as ws:
        ws.call(
            "fauna.moderation.abuse_report.submit",
            {
                "subject": {"kind": "post", "cid": post_id},
                "reason": "harassment",
                "note": note,
                "subject_actor": member["actor_id_hex"],
            },
        )

    entries = _archive(nest, member["token"])

    audit = _rows(entries, "audit_log")
    details = [r.get("detail") for r in audit]
    assert f"id={mine}" in details, f"the member's own audit row rides: {audit!r}"
    assert f"id={theirs}" not in details, "another member's audit row crossed into the archive"
    for row in audit:
        for chain in ("prev_hash", "entry_hash"):
            assert chain not in row, f"audit_log.{chain} rode out of the nest: {row!r}"

    leaked = [n for n, body in entries.items() if note.encode() in body]
    assert not leaked, f"a report about the member reached its archive: {leaked!r}"
    report_tables = [n for n in entries if "report" in n and n.startswith("export/tables/")]
    assert not any(t in n for n in report_tables for t in ("abuse_report", "content_report")), (
        f"no report table rides an archive: {report_tables!r}"
    )

    # ── family ──
    guardian = _member(nest)
    ward = admit_ward_directly(nest, guardian["actor_id_hex"], f"ward{secrets.token_hex(3)}")
    ward_token = mint_token_via_handshake(nest["url"], ward["signing_key"])

    def family(archive: dict[str, bytes]) -> list[str]:
        return [
            n for n in archive
            if n.startswith("export/tables/guardian") and _rows(archive, n[len("export/tables/"):-len(".ndjson")])
        ]

    ward_family = family(_archive(nest, ward_token))
    assert ward_family, "the child's archive carries its guardianship family"
    guardian_archive = _archive(nest, guardian["token"])
    assert not family(guardian_archive), (
        f"the guardian's archive carries the child's family data: {family(guardian_archive)!r}"
    )

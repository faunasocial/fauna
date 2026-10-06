"""What the export archive declares it does NOT hold — full-stack (tier_3) API E2E.

Witnesses two `export-my-data` outcomes that ride on the archive itself rather
than on any app's button:

* **3** — *"The archive names what it does not contain: anything the nest holds
  back is listed with the reason, so 'everything' is never a guess."*
* **4** — *"Your keys, passphrases and escrowed secrets are never in the
  archive."*

Both are `docs/goal/architecture/account-data-plane.md` § Nest-side
requirements item 1 (rule 4, *the export declares its own partiality*, and the
`WithheldSecret` class the belt enforces). Until this file both were pinned
only by ``bins/fauna-nest/tests/export_api.rs`` — an in-process suite the
feature catalog cannot cite — while the only e2e assertion over any archive
was that ``export/manifest.json`` exists
(``tests/test_export_my_data_journey.py``). So the *declaration* reached no
wire-level witness at all.

**This pulls a real archive over the wire** — ``GET /api/v1/export`` with a
real bearer, the same route every app's export button drives — and reads the
zip the user would receive.

**The two assertions are each two-way.** For outcome 3 the declaration is
checked for *accuracy*, not presence: every table the manifest declares
withheld must be absent from ``export/tables/``, **and** every table file that
IS in the archive must be absent from the declaration. A manifest that
declared everything, or nothing, fails one side or the other. For outcome 4
the secret class is checked per name rather than for non-emptiness: a
non-empty check passes just as happily when fifteen of sixteen secret-bearing
tables have silently dropped off the declaration.

Process safety: no ``pkill``/``killall``; the nest is the session-scoped
``nest_instance`` and is torn down by its own fixture.
"""

from __future__ import annotations

import io
import json
import secrets
import zipfile

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

#: A registry-driven table the actor below really owns a row in, so the
#: archive carries `export/tables/<it>.ndjson`. `backup_destinations` is
#: `Export::Verbatim` and one User-class call away
#: (`fauna.backup.destination.register`) — the 11 hand-written *shaped* domains
#: (profile, contacts, sync devices/folders …) ride as their own JSON files and
#: never appear under `tables/`, so a bare actor's archive has no table file at
#: all and the "every table file present is not declared withheld" half of the
#: check below would assert over an empty set.
SEEDED_TABLE = "backup_destinations"

#: The three recognised withholding classes
#: (`Export::withheld_reason_class`, `bins/fauna-nest/src/db/actor_tables.rs`).
REASON_CLASSES = {"secret", "derived", "operational"}

#: Secret-bearing tables the archive MUST keep declaring by name. Deliberately
#: a subset of the nest's own roster (`export_api.rs::SECRET_BEARING_TABLES`),
#: restricted to tables `db/migrations.rs` creates unconditionally — so this
#: list is stable across feature sets — and to the ones a user would name as
#: their key material: the sealing keys, the recovery escrow, the eviction
#: token and the nest's backup key.
#:
#: Naming them is the point. "the secret class is non-empty" would stay green
#: with all but one of them gone, which is exactly the coverage the Rust belt
#: refused for the same reason (`export_never_carries_a_secret_bearing_table`'s
#: per-table note). A table legitimately leaving the registry updates this
#: list in the same commit; one leaving *silently* reds here.
ANCHOR_SECRET_TABLES = {
    "current_key_blobs",
    "eviction_tokens",
    "generation_escrow_wraps",
    "nest_backup_keys",
    "recovery_escrow",
}


def _seed_one_registry_row(nest_instance, actor) -> None:
    """Give the actor one row in `SEEDED_TABLE`, over the production kind.

    Inert on the shared nest: the nest-side backup worker and the lease runner
    both walk `list_nest_backup_key_owners()` first, and this actor grants no
    `NestBackupKey` — so the destination is registry data and nothing ever
    dials it.
    """
    ws = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )
    with ws:
        reply = ws.call(
            "fauna.backup.destination.register",
            {
                "destination_id": f"export-coverage-{secrets.token_hex(4)}",
                "destination_nest_url": "http://127.0.0.1:1",
                "destination_nest_id": secrets.token_bytes(32).hex(),
                "kind": "nest",
            },
        )
    assert reply["ok"] is True, reply


def _pull_archive(nest_instance, actor) -> tuple[dict, list[str]]:
    """`GET /api/v1/export` as the actor. Returns (manifest, entry names)."""
    resp = requests.get(
        f"{nest_instance['url']}/api/v1/export",
        params={"include_blobs": "true"},
        headers={"Authorization": f"Bearer {actor['token']}"},
        timeout=120,
    )
    assert resp.status_code == 200, (
        f"GET /api/v1/export returned {resp.status_code}: {resp.text[:400]}"
    )
    assert resp.content[:2] == b"PK", (
        f"the export route must hand back a zip, got {resp.content[:16]!r}"
    )
    with zipfile.ZipFile(io.BytesIO(resp.content)) as z:
        names = z.namelist()
        manifest = json.loads(z.read("export/manifest.json"))
    return manifest, names


def _table_files(names: list[str]) -> set[str]:
    """The table names actually carried as `export/tables/<t>.ndjson`."""
    return {
        n[len("export/tables/") : -len(".ndjson")]
        for n in names
        if n.startswith("export/tables/") and n.endswith(".ndjson")
    }


@pytest.mark.feature("export-my-data")
def test_archive_declares_what_it_withholds_by_name_and_reason(nest_instance):
    """Outcome 3: the omissions are listed, with a reason, and the list is true.

    The manifest's `coverage` block is the declaration: `withheld_tables`
    (name + reason class), `unreviewed_tables` (the honest interim for a table
    whose disposition has not been ruled) and the `partial` flag that summarises
    them. `segment_store` declares the payload-byte side — the channel-scoped
    plane a per-actor byte walk structurally cannot reach is named rather than
    silently absent.

    `partial` is asserted as an INVARIANT against `unreviewed_tables` rather
    than pinned to today's value: the flag's meaning is "an open judgment
    remains", so the two agreeing is what makes it honest, and pinning the
    value would red on an unrelated registry change while saying nothing about
    this outcome.
    """
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    _seed_one_registry_row(nest_instance, actor)
    manifest, names = _pull_archive(nest_instance, actor)

    assert manifest["format"] == 1, manifest
    coverage = manifest["coverage"]
    assert isinstance(coverage, dict), f"the archive must carry a coverage declaration: {manifest!r}"

    withheld = coverage["withheld_tables"]
    assert withheld, (
        "the nest holds tables it does not export (keys, derived indexes, "
        "operational rows); declaring none of them is the 'everything' guess "
        f"this outcome exists to prevent: {coverage!r}"
    )
    for entry in withheld:
        assert entry["table"], f"a withheld entry must name its table: {entry!r}"
        assert entry["reason_class"] in REASON_CLASSES, (
            f"a withheld entry must carry a recognised reason class "
            f"{sorted(REASON_CLASSES)}, got {entry!r}"
        )

    assert coverage["partial"] == bool(coverage["unreviewed_tables"]), (
        "the partiality flag must mean exactly 'a table still awaits a "
        f"verdict', else it says nothing an owner can rely on: {coverage!r}"
    )
    assert coverage["blob_encoding"] == "hex", (
        f"the archive must say how BLOB columns are encoded, since NDJSON "
        f"cannot carry the distinction itself: {coverage!r}"
    )

    # The declaration must be TRUE, both ways.
    declared = {e["table"] for e in withheld}
    present = _table_files(names)
    leaked = sorted(declared & present)
    assert not leaked, (
        f"these tables are declared withheld yet ride in the archive: {leaked}"
    )
    assert SEEDED_TABLE in present, (
        f"the actor owns a {SEEDED_TABLE} row, so the archive must carry "
        f"export/tables/{SEEDED_TABLE}.ndjson — without it the other direction "
        f"of this check asserts over an empty set. Table files: {sorted(present)}"
    )
    assert SEEDED_TABLE not in declared, (
        f"{SEEDED_TABLE} is both exported and declared withheld: {sorted(declared)}"
    )

    # The payload-byte side of the same promise.
    segment_store = manifest["segment_store"]
    assert segment_store["included"] is True, segment_store
    assert segment_store["kinds"], (
        f"the archive must name which segment planes the walk covers: {segment_store!r}"
    )
    assert segment_store["channel_scoped_not_included"] == ["conv"], (
        "the channel-scoped plane a per-actor byte walk cannot reach must be "
        f"declared by name, never silently absent: {segment_store!r}"
    )
    for entry in segment_store["withheld"]:
        assert entry["kind"] and entry["reason"], (
            f"a withheld segment pair must name its kind and reason: {entry!r}"
        )


@pytest.mark.feature("export-my-data")
def test_archive_never_carries_key_or_escrow_material(nest_instance):
    """Outcome 4: no secret-bearing table is in the archive, and each is named.

    The route accepts an **eviction export token** — the weakest credential it
    takes — so an archive carrying key material would mint a fresh resting
    place for it under the weakest door on the nest. The belt is asserted here
    over the real wire: every table the manifest classes `secret` is absent
    from `export/tables/`, and the anchors above are all still classed that
    way.
    """
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    manifest, names = _pull_archive(nest_instance, actor)

    secret_tables = {
        e["table"]
        for e in manifest["coverage"]["withheld_tables"]
        if e["reason_class"] == "secret"
    }
    missing = sorted(ANCHOR_SECRET_TABLES - secret_tables)
    assert not missing, (
        f"these tables hold key / passphrase / escrow material and are no "
        f"longer declared withheld-secret by the archive: {missing}. Either "
        f"the registry demoted them (a leak) or they left the schema (update "
        f"ANCHOR_SECRET_TABLES in the same commit). Declared secret: "
        f"{sorted(secret_tables)}"
    )

    present = _table_files(names)
    leaked = sorted(secret_tables & present)
    assert not leaked, (
        f"these secret-bearing tables ride in the archive: {leaked} — the "
        f"export must never mint a new resting place for key material"
    )

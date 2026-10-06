"""tier_3 E2E: schema-version downgrade detection + degraded "needs-update" boot.

Exercises the `schema_meta` two-number scheme + boot-check + degraded-serve
(version-compatibility.md § 2.2) end-to-end against a REAL `fauna-nest` binary:

1. Boot a fresh nest (records `schema_meta = (CURRENT, MIN) = (1, 1)`), stop it.
2. Mutate the on-disk `nest.db` `schema_meta` row with host-side `sqlite3` — the
   tier_4 `test_mail_deploy_schema_upgrade.py` pattern, applied to a local binary.
3. Restart the binary on the SAME data dir and observe the boot verdict over the
   anonymous WS-RPC surface.

Two cases:
- **Incompatible** (`min_reader_version` above the binary's `CURRENT_SCHEMA_VERSION`):
  the nest must boot DEGRADED — `/api/v1/health` stays 200 (no crash-loop /
  off-box brick — `nest/common.md` § Client-state recoverability), and every
  WS-RPC call (the anonymous `fauna.nest.info` included) returns the typed
  `fauna.nest.outdated` error, NOT a raw SQL leak and NOT a destructive migration.
- **NewerCompatible** (`schema_version` ahead but `min_reader_version` within the
  binary's floor): the additive reconciler tolerates the newer DB, so the nest
  operates normally and `fauna.nest.info` succeeds (I2 backward-compat — an older
  binary keeps working against a newer *additive* DB).
- **NewerCompatible with a structural skew** (`test_old_binary_serves_new_db_with_
  extra_table`): the same NewerCompatible verdict, but the newer DB also carries a
  *table this binary never defined* — the literal "old-binary-opens-new-DB" cell of
  the Dimension 6 grid. The binary must ignore the unknown table and serve, not
  choke on a structure it doesn't know (§ 2.2 verdict table — "the reconciler
  already tolerates the extra columns"). This is the at-rest half of the
  bidirectional version-skew grid whose WIRE half lives in
  `test_wire_version_skew.py` (old client → new nest / new client → old nest).
"""

import sqlite3

import pytest

from common.nest import (
    stamp_schema_meta,
    start_nest_in_place,
    stop_nest,
)

from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def schema_skew_nest(request, nest_mode, tmp_path_factory):
    """An UNCLAIMED nest whose data dir this test then mutates behind its back.

    Routed like every other dedicated nest, and the two mechanisms that make
    that safe here are worth naming, because reading only the calling code
    suggests the opposite:

    * `start_nest_in_place` is **mode-agnostic by delegation** — it asks the
      handle for a `start_in_place` key, which the docker provider answers by
      restarting its own container, and only falls through to re-spawning a
      local binary for a handle that has none. So "stop, mutate `nest.db`,
      restart on the SAME data dir" is a sentence every mode can say.
    * The host-side `sqlite3` mutation reaches a container's DB because
      `db_path` is answered in docker too: the provider bind-mounts a host
      directory at `/data` and the image's `fauna` user is uid 1000, the same
      uid the dev VMs run as.

    Each test gets its own (function-scoped): the mutation is destructive by
    design, and the point of every one of them is a FIRST boot that recorded a
    clean `schema_meta`.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "schema-skew", unclaimed=True)
    yield nest
    cleanup()

# Far above any plausible CURRENT_SCHEMA_VERSION this binary carries, so the
# verdict is unambiguous regardless of future baseline bumps.
FUTURE_VERSION = 9999


@pytest.mark.feature("upgrades-never-lose-data")
def test_incompatible_schema_boots_degraded_and_serves_outdated(schema_skew_nest):
    """A DB written by a newer nest with a breaking change → degraded boot that
    answers `fauna.nest.outdated` to every client, never a crash-loop."""
    nest = schema_skew_nest
    # First boot recorded schema_meta=(1,1); stop, then stamp the reader
    # floor above this binary so it can no longer safely operate the DB.
    stop_nest(nest, graceful=False)
    stamp_schema_meta(nest["db_path"], FUTURE_VERSION, FUTURE_VERSION)

    # Restart on the SAME data dir. `start_nest_in_place` blocks on
    # `/api/v1/health` returning 200 — proving the degraded nest comes up
    # (no crash-loop) rather than exiting.
    start_nest_in_place(nest)

    with WsRpcAnonClient(nest["url"]) as anon:
        with pytest.raises(RpcCallError) as exc:
            anon.call("fauna.nest.info", {})
        assert exc.value.code == "fauna.nest.outdated", (
            f"expected fauna.nest.outdated, got {exc.value.code!r} "
            f"(details={exc.value.details!r})"
        )
        # The message is a localized key, NOT a raw server/SQL string (Dim 4).
        assert exc.value.message_key == "error.nest.outdated"

@pytest.mark.feature("upgrades-never-lose-data")
def test_newer_but_additive_schema_still_serves(schema_skew_nest):
    """A DB newer than this binary but only ADDITIVELY (reader floor within
    range) → the nest operates normally; `fauna.nest.info` succeeds."""
    nest = schema_skew_nest
    stop_nest(nest, graceful=False)
    # schema_version ahead, but min_reader_version=1 (within this binary's
    # floor) → NewerCompatible, not Incompatible.
    stamp_schema_meta(nest["db_path"], FUTURE_VERSION, 1)
    start_nest_in_place(nest)

    with WsRpcAnonClient(nest["url"]) as anon:
        info = anon.call("fauna.nest.info", {})
    # A normal reply — not an error. nest.info always carries `version`.
    assert isinstance(info, dict)
    assert "version" in info

@pytest.mark.feature("upgrades-never-lose-data")
def test_old_binary_serves_new_db_with_extra_table(schema_skew_nest):
    """The literal "old-binary-opens-new-DB" cell (Dim 6): a current binary opens a
    `/data` a NEWER binary wrote — `schema_version` ahead, reader floor within range
    (NewerCompatible), AND carrying a TABLE this binary never defined — and serves
    normally, ignoring the unknown table.

    `test_newer_but_additive_schema_still_serves` proves the version-*number* skew;
    this adds the *structural* skew an actual additive upgrade produces (a newer
    binary's new table sitting in the DB). The boot-time `check_schema_compatibility`
    verdict is the same (`db_v > bin_v` and `db_min <= bin_v` → operate normally,
    don't restamp), and migrations + the column reconciler must leave the unknown
    `_future_*` table untouched rather than erroring — § 2.2 verdict-table line:
    "the reconciler already tolerates the extra columns (I2 backward-compat)."""
    nest = schema_skew_nest
    stop_nest(nest, graceful=False)

    # A newer binary added a table this binary never defined. Add it directly,
    # then stamp the version pair a newer-but-additive binary would record.
    conn = sqlite3.connect(nest["db_path"])
    try:
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
        conn.execute(
            "CREATE TABLE _future_feature_rows ("
            "  id        INTEGER PRIMARY KEY,"
            "  payload   BLOB NOT NULL,"
            "  added_in  TEXT NOT NULL DEFAULT 'a-newer-binary'"
            ");"
        )
        conn.execute(
            "INSERT INTO _future_feature_rows (payload) VALUES (?);", (b"\x01\x02\x03",)
        )
        conn.commit()
    finally:
        conn.close()

    # schema_version ahead, min_reader_version=1 (within this binary's floor)
    # → NewerCompatible, not Incompatible.
    stamp_schema_meta(nest["db_path"], FUTURE_VERSION, 1)
    start_nest_in_place(nest)

    with WsRpcAnonClient(nest["url"]) as anon:
        info = anon.call("fauna.nest.info", {})
    # Served normally — not `fauna.nest.outdated`, not a crash-loop (the
    # in-place restart blocks on /api/v1/health 200 first).
    assert isinstance(info, dict) and "version" in info, (
        "the binary must serve normally against a newer additive DB carrying an "
        f"extra table it doesn't know (NewerCompatible verdict); got {info!r}"
    )

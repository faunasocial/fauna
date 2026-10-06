"""At-rest probes for the kinds whose body moved out of a SQLite column into a
segment store (`docs/goal/architecture/message-segment-store.md` § Layout kind
table).

**Why this module exists — the vacuous-green trap.** Before the S6.6 cutover a
calendar event's / contact card's sealed body lived in the row's
`encrypted_body` BLOB, so "assert no plaintext in `encrypted_body`" was a real
assertion. After the cutover the body lives in the actor's `__calendar` /
`__card` segment store, and since nest schema 110 the metadata row has no body
column at all. A test that scans only the metadata table passes **vacuously**:
it holds no body, so the assertion would keep greening even if the real body
went to disk in the clear.

Every probe here therefore checks *both* halves: the metadata table really has
no body column (so a future change that puts a body back into the row — which
would shadow the segment — fails loudly), and the bytes that actually exist on
disk are the ones inspected.

Reading whole segment files rather than parsing CARv2 records out of them is
deliberate: it is the strictly stronger assertion — no plaintext anywhere in the
container, envelope, floor or footer included.
"""
from __future__ import annotations

import pathlib
import sqlite3

# kind → (SQLite metadata table, on-disk reserved segment dir). The kind strings
# are the ones the nest mirrors into `segment_records.kind`
# (`bins/fauna-nest/src/segments/{cal,card}.rs` `KIND`).
_KINDS: dict[str, tuple[str, str]] = {
    "calendar": ("bridge_caldav_events", "__calendar"),
    "card": ("bridge_carddav_cards", "__card"),
}


def _table_and_dir(kind: str) -> tuple[str, str]:
    try:
        return _KINDS[kind]
    except KeyError:
        raise AssertionError(f"unknown segment kind {kind!r} (want one of {sorted(_KINDS)})")


def segment_dir(nest: dict, kind: str, actor_id: bytes) -> pathlib.Path:
    """The actor's on-disk segment directory for `kind`.

    The segment root is the nest data dir, which is the parent of `db_path`
    (`lib.rs` derives it exactly that way), and each scope gets an actor-hex
    subdirectory: `<data_dir>/__<kind>/<actor_hex>/`.
    """
    _, dirname = _table_and_dir(kind)
    return pathlib.Path(nest["db_path"]).parent / dirname / actor_id.hex()


def assert_rows_carry_no_body(nest: dict, kind: str, actor_id: bytes) -> int:
    """Assert the actor's metadata table has no body column, and return the
    actor's row count there.

    This is the half that keeps the segment scan honest: a body resting in the
    row would be a second copy the segment scan never inspects.
    """
    table, _ = _table_and_dir(kind)
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True, timeout=10.0)
    try:
        columns = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
        (n,) = conn.execute(
            f"SELECT COUNT(*) FROM {table} WHERE actor_id = ?",  # noqa: S608
            (actor_id,),
        ).fetchone()
    finally:
        conn.close()
    assert "encrypted_body" not in columns, (
        f"{table} carries an `encrypted_body` column again — the body belongs in "
        f"the __{kind} segment only (message-segment-store.md § Invariants, no. 6)"
    )
    assert n, f"{table} holds no row for this actor — nothing was written"
    return n


def live_segment_records(nest: dict, kind: str, actor_id: bytes) -> int:
    """Count the actor's LIVE (non-tombstoned) `segment_records` mirror rows for
    `kind` — the projection that resolves a body's CID to the segment holding it.
    A metadata row with no mirror row would be a lost body.
    """
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True, timeout=10.0)
    try:
        (n,) = conn.execute(
            "SELECT COUNT(*) FROM segment_records "
            "WHERE scope_id = ? AND kind = ? AND tombstoned = 0",
            (actor_id, kind),
        ).fetchone()
    finally:
        conn.close()
    return n


def bodies_at_rest(nest: dict, kind: str, actor_id: bytes) -> list[bytes]:
    """Every byte the nest has written to disk for this actor's `kind` records,
    so a caller can prove none of it is plaintext.

    Asserts the metadata row carries no body first (see the module docstring —
    the scan is only meaningful once that half is pinned), then returns the raw
    contents of the on-disk segment files.
    """
    assert_rows_carry_no_body(nest, kind, actor_id)
    seg_root = segment_dir(nest, kind, actor_id)
    seg_files = sorted(seg_root.glob("*.dat"))
    assert seg_files, (
        f"no __{kind} segment files under {seg_root} — the body was never appended "
        "to the segment store, so nothing is proving it is sealed"
    )
    return [f.read_bytes() for f in seg_files]


def assert_nothing_plaintext(blobs: list[bytes], markers: list[bytes]) -> None:
    """Assert none of `markers` appears anywhere in `blobs` (raw segment bytes)."""
    for marker in markers:
        for blob in blobs:
            assert marker not in blob, (
                f"{marker!r} appears in cleartext at rest — the body is not sealed "
                "(encryption-at-rest.md § Plaintext ceiling per mode)"
            )

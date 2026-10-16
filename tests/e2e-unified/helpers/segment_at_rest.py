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


def unopenable_mail_record(nest: dict, actor_id: bytes, mailbox: str = "INBOX") -> tuple[str, int]:
    """Make the newest message in ``actor_id``'s ``mailbox`` unopenable AT REST,
    and return its ``(mailbox, uid)``.

    The fault is a **tampered envelope** (`mail-export.md` § An unopenable
    record fails the session names it beside a retired key): one byte inside
    the record's sealed body is flipped in its finalized `__mail` segment on
    nest disk, so its AEAD can no longer authenticate under any key the account
    holds. Nothing else moves — the record's CID, its length, the CARv2 index
    and the `.meta` sidecar all stay as they were, and the nest's read path
    (`read_sealed_body_with_floor` → `Segment::read_record` →
    `fauna_carv2::Reader::get`) checks the on-disk CID header but never
    re-hashes the block — so the nest still serves the record, and the fault
    reaches the one party that can tell: the client that tries to open it.

    The record is found through the nest's own read-only mirrors: its UID and
    record id from `bridge_imap_messages`, the segment that holds it from
    `segment_records` (joined on the CID's 32-byte digest tail, the documented
    join). Read-your-own-writes finalizes the open segment on the first read of
    the record, so the caller must have had it read once already (the app
    surfacing it decrypted does) — this waits for the finalized file, never
    for a clock.

    Raises ``PermissionError`` when the harness cannot write the nest's data
    dir in this nest mode; the caller decides how to declare that.
    """
    import io
    import time

    import cbor2

    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT uid, message_id FROM bridge_imap_messages "
            "WHERE actor_id = ? AND mailbox = ? ORDER BY uid",
            (actor_id, mailbox),
        ).fetchall()
        assert rows, f"no message in {mailbox!r} for this actor — nothing to make unopenable"
        uid, message_id = rows[-1]
        placed = conn.execute(
            "SELECT segment_id, record_cid FROM segment_records "
            "WHERE scope_id = ? AND kind = 'mail' AND substr(record_cid, 5) = ?",
            (actor_id, bytes(message_id)),
        ).fetchall()
    finally:
        conn.close()
    assert len(placed) == 1, (
        f"{mailbox} uid {uid} must rest in exactly one mail segment record; "
        f"segment_records has {len(placed)}"
    )
    segment_id, record_cid = placed[0][0], bytes(placed[0][1])
    seg = pathlib.Path(nest["db_path"]).parent / "__mail" / actor_id.hex() / f"seg-{segment_id:08}.dat"
    meta = seg.with_suffix(".meta")

    deadline = time.monotonic() + 30.0
    while not (meta.exists() and seg.exists() and record_cid in seg.read_bytes()):
        assert time.monotonic() < deadline, (
            f"{mailbox} uid {uid}'s segment {seg} was never finalized on disk "
            f"(.dat present: {seg.exists()}, .meta present: {meta.exists()}) — "
            "it is read once before this is called, which flushes it"
        )
        time.sleep(0.2)

    data = seg.read_bytes()
    # A CARv2 data-section record is `varint(len) || cid || block`: find the CID
    # whose following block decodes as the mail envelope (the index section may
    # carry the digest again, never followed by an envelope).
    body_at = None
    start = 0
    while (at := data.find(record_cid, start)) != -1:
        start = at + 1
        try:
            envelope = cbor2.CBORDecoder(io.BytesIO(data[at + len(record_cid):])).decode()
        except Exception:
            continue
        body = envelope.get("encrypted_body") if isinstance(envelope, dict) else None
        if isinstance(body, bytes) and len(body) > 32:
            body_at = data.find(body, at + len(record_cid))
            body_len = len(body)
            break
    assert body_at is not None and body_at >= 0, (
        f"{mailbox} uid {uid}: no inline envelope carrying an `encrypted_body` "
        f"follows its CID in {seg} — the record rests in another shape "
        "(a continuation head?), so this probe cannot reach its ciphertext"
    )
    flip = body_at + body_len // 2
    with open(seg, "r+b") as fh:
        fh.seek(flip)
        (byte,) = fh.read(1)
        fh.seek(flip)
        fh.write(bytes([byte ^ 0xFF]))
    return mailbox, int(uid)

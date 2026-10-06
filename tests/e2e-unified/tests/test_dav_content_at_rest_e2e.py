"""tier_3 — S6.11: the calendar/card content-at-rest proof on a REAL
`fauna-nest` binary + a REAL CalDAV/CardDAV MDA.

Goal docs: `docs/goal/architecture/message-segment-store.md` § Implementation
status (the calendar/card kind-table rows' Proof column) + § Invariants binding
any kind whose body moves out of a SQLite column;
`docs/goal/architecture/encryption-at-rest.md` § Plaintext ceiling per mode.

The in-process Rust twins (`bins/fauna-nest/src/segments/{cal,card}.rs`'s
`#[cfg(test)]` modules) already prove append/serve/restore directly
against the segment store. What they cannot reach is the real-binary + real-DAV
dimension, which is what this file adds:

  1. A body PUT through the production CalDAV/CardDAV path rests **only** in the
     actor's `__calendar` / `__card` segment store — the SQLite column is the
     empty "segment-served" flag — and none of the on-disk bytes are plaintext.
  2. It opens back out of the segment intact through the real serve path (GET +
     calendar-multiget / addressbook-multiget).
  3. Every record at rest passes the S6.12 sealed-envelope check (the
     conformance walk), body AND index hint.
  4. It survives a storage transition **byte-identically** — the S6.9 snapshot
     create→restore, driven through the real production path.

(A sixth proof — the v1-era placement-manifest boot heal at real boot ordering —
was retired with the heal itself by the compat-remnant sweep, 2026-09-24: no v1
manifest exists anywhere, and one is now refused at load.)

⚠ **Byte-identity is asserted across a TRANSITION, never against the PUT bytes.**
The MDA parses iCalendar/vCard into its object model and re-serializes it
canonically before sealing (its encoder reorders `PRODID` ahead of `VERSION`),
so the bytes it seals were never the client's bytes — identity with the PUT body
is not an invariant the storage layer owes, and asserting it fails on property
order alone. What the store DOES owe is: whatever it swallowed, it hands back
unchanged. So each transition test captures a served **baseline** first and
re-reads after; both reads cross the same serialization, so it cancels and any
difference is the store's doing. See :func:`_put_event`.

⚠ **The vacuous-green trap this file is built around.** With segment-served bodies the
metadata row holds no body (its `encrypted_body` column was dropped at nest
schema 110), so an at-rest assertion over the row is trivially true and proves
nothing. Every assertion below therefore reads the bytes that actually exist —
the on-disk segment files — and separately pins that the row carries no body.
See `helpers/segment_at_rest.py`.

Runs on the dedicated `restartable_mda_nest` (none of these tests restarts
the nest; they use it for isolation from the shared session nest): it already serves CalDAV *and* CardDAV from MDA boot
(`caldav_enabled`/`carddav_enabled` inherit `mail_enabled`, which defaults true),
and its MSEK recipient's credential is exactly the DAV Basic-auth credential —
so this is the one DAV fixture that needs no UI client to mint one.
"""
from __future__ import annotations

import json
import sqlite3
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.caldav_client import CalDAVClient, build_vevent
from helpers.carddav_client import CardDAVClient, CardDAVError, build_vcard
from helpers.segment_at_rest import (
    assert_nothing_plaintext,
    assert_rows_carry_no_body,
    bodies_at_rest,
    live_segment_records,
)

pytestmark = pytest.mark.tier_3

# Distinctive strings that must never appear in the at-rest bytes. Each is
# carried inside the sealed payload, so finding one on disk means the seal broke.
_EVENT_SECRET = "TOPSECRET-calendar-at-rest-marker-4471"
_CARD_SECRET = "TOPSECRET-card-at-rest-marker-8823"


# ── test-hook clients (`--features test-hooks`, mounted at /api/v1/test/content) ──


def _conformance(nest_url: str, actor_id_hex: str, kind: str) -> dict:
    """`GET /api/v1/test/content/dav_content_conformance` — the S6.12 walk over
    every live record in the actor's `__{kind}` store. `{"total", "sealed",
    "unsealed"}`; conformant iff `total == sealed` and `unsealed == []`."""
    url = (
        f"{nest_url}/api/v1/test/content/dav_content_conformance"
        f"?actor_id={actor_id_hex}&kind={kind}"
    )
    with urllib.request.urlopen(url, timeout=15.0) as resp:
        assert resp.status == 200, f"dav_content_conformance returned {resp.status}"
        return json.loads(resp.read())


# ── SQLite probes ────────────────────────────────────────────────────────────

_ID_COL = {
    "calendar": ("bridge_caldav_events", "event_id"),
    "card": ("bridge_carddav_cards", "card_id"),
}


# ── DAV clients on the fixture ───────────────────────────────────────────────


def _caldav(handle) -> CalDAVClient:
    c = CalDAVClient(
        f"https://127.0.0.1:{handle.caldav_port}",
        handle.recipient_username,
        handle.recipient_password,
        verify=False,
    )
    c.wait_until_serving()
    return c


def _carddav(handle) -> CardDAVClient:
    # CardDAV rides the SAME DAV listener/port as CalDAV — there is no separate
    # CardDAV port.
    c = CardDAVClient(
        f"https://127.0.0.1:{handle.caldav_port}",
        handle.recipient_username,
        handle.recipient_password,
        verify=False,
    )
    c.wait_until_serving()
    return c


def _put_event(mua: CalDAVClient, uid: str, summary: str) -> tuple[str, bytes]:
    """PUT one VEVENT through the real CalDAV path, then GET it straight back.

    Returns `(calendar href, the served bytes)` — the **baseline**, and the only
    correct oracle for a later byte-identity assertion.

    ⚠ NOT the bytes we PUT. The MDA parses the iCalendar into its object model
    and re-serializes it canonically before sealing (its encoder emits `PRODID`
    before `VERSION`, whatever order the client sent), so the bytes it seals were
    never the client's bytes and identity-with-the-PUT-body is not an invariant
    the storage layer owes — asserting it would fail on property order alone,
    for a reason that has nothing to do with the segment store.

    What the segment store DOES owe is identity **across a storage transition**:
    whatever it swallowed, it must hand back unchanged. That is measured by
    comparing this baseline to a re-read after the transition (a snapshot
    restore) — both reads pass through the same MDA serialization, so
    normalization cancels and any difference is the store's doing.
    """
    cal = mua.personal_calendar()
    ics = build_vevent(
        uid,
        summary,
        "20260801T120000Z",
        "20260801T130000Z",
        description=_EVENT_SECRET,
    )
    mua.put_event(cal, uid, ics)
    served = mua.get_event_bytes(cal, uid)
    assert served, f"the event {uid} did not serve back after its PUT"
    return cal, served


def _put_card(client: CardDAVClient, uid: str, fn: str) -> tuple[str, bytes]:
    """The CardDAV twin of :func:`_put_event` — returns the served baseline, not
    the PUT bytes, for the same re-serialization reason."""
    book = client.contacts_addressbook()
    vcf = build_vcard(uid, fn, email="at-rest@fauna.test", note=_CARD_SECRET)
    client.put_card(book, uid, vcf)
    served = client.get_card(book, uid)
    assert served, f"the card {uid} did not serve back after its PUT"
    return book, served.encode()


def _assert_intact(served: bytes | None, *, must_contain: list[str]) -> None:
    """The served body opened and still carries everything the client sealed."""
    assert served, "the record did not serve back at all"
    text = served.decode()
    for needle in must_contain:
        assert needle in text, (
            f"{needle!r} is missing from the served body — the sealed payload did "
            f"not open back intact. Got:\n{text}"
        )


# ── 1 + 2 + 3: at rest, served back, conformant ──────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_calendar_body_rests_sealed_in_its_segment_and_serves_byte_identically(
    restartable_mda_nest,
):
    """A CalDAV PUT lands the sealed body in `__calendar/<actor>/` and NOWHERE
    else: the SQLite column is the empty segment-served flag, a live mirror row
    resolves the CID, no on-disk byte is plaintext, every record passes the
    S6.12 sealed-envelope walk — and the body still serves back byte-identically.
    """
    handle = restartable_mda_nest
    nest = handle.nest_instance
    actor_bytes = handle.recipient_actor["actor_id_bytes"]
    actor_hex = handle.recipient_actor["actor_id_hex"]

    mua = _caldav(handle)
    uid = "s611-cal-at-rest-1"
    cal, baseline = _put_event(mua, uid, "S6.11 at-rest calendar event")

    # ── at rest: the column is empty, the mirror is live, the disk is sealed.
    assert live_segment_records(nest, "calendar", actor_bytes) >= 1, (
        "no live __calendar segment_records row — an emptied column with no "
        "mirror row is a LOST body (message-segment-store.md § Invariants, no. 1)"
    )
    blobs = bodies_at_rest(nest, "calendar", actor_bytes)  # also pins the empty column
    assert_nothing_plaintext(
        blobs,
        [b"BEGIN:VCALENDAR", _EVENT_SECRET.encode(), b"S6.11 at-rest calendar event"],
    )

    # ── the S6.12 conformance walk: EVERY record at rest is a sealed envelope,
    # body and index hint alike. This is the assertion the empty column cannot
    # make — it reads the bytes that actually exist.
    walk = _conformance(nest["url"], actor_hex, "calendar")
    assert walk["total"] >= 1, f"conformance walk found no calendar records: {walk}"
    assert walk["unsealed"] == [] and walk["sealed"] == walk["total"], (
        f"a calendar record at rest is not a sealed envelope: {walk}"
    )

    # ── serve: the sealed body opens back out of the segment with everything the
    # client wrote still in it (see _put_event on why this is content-equality,
    # not identity with the PUT bytes; `baseline` is the byte-identity oracle the
    # snapshot-restore tests compare against across their transition).
    _assert_intact(
        mua.get_event_bytes(cal, uid),
        must_contain=[f"UID:{uid}", "S6.11 at-rest calendar event", _EVENT_SECRET],
    )
    assert mua.get_event_bytes(cal, uid) == baseline, (
        "two consecutive GETs of the same segment-served event differ — the open "
        "path is not deterministic"
    )

    # ── and through calendar-multiget, the read path a real MUA drives (content
    # compared, not bytes: a REPORT wraps the body in XML, whose text parsing
    # normalizes CRLF→LF).
    got = mua.multiget(cal, mua.event_hrefs(cal))
    assert [e.uid for e in got] == [uid], f"calendar-multiget did not return the event: {got}"
    assert _EVENT_SECRET in (got[0].ics or ""), (
        "calendar-multiget served an event whose sealed DESCRIPTION did not open"
    )


@pytest.mark.feature("address-book")
def test_card_body_rests_sealed_in_its_segment_and_serves_byte_identically(
    restartable_mda_nest,
):
    """The `__card` twin: same four properties, same bar (priority #4 — the two
    DAV stores must never diverge in DR posture)."""
    handle = restartable_mda_nest
    nest = handle.nest_instance
    actor_bytes = handle.recipient_actor["actor_id_bytes"]
    actor_hex = handle.recipient_actor["actor_id_hex"]

    client = _carddav(handle)
    uid = "s611-card-at-rest-1"
    book, baseline = _put_card(client, uid, "S6.11 At-Rest Contact")

    assert live_segment_records(nest, "card", actor_bytes) >= 1, (
        "no live __card segment_records row — an emptied column with no mirror "
        "row is a LOST body"
    )
    blobs = bodies_at_rest(nest, "card", actor_bytes)
    assert_nothing_plaintext(
        blobs,
        [b"BEGIN:VCARD", _CARD_SECRET.encode(), b"at-rest@fauna.test"],
    )

    walk = _conformance(nest["url"], actor_hex, "card")
    assert walk["total"] >= 1, f"conformance walk found no card records: {walk}"
    assert walk["unsealed"] == [] and walk["sealed"] == walk["total"], (
        f"a card record at rest is not a sealed envelope: {walk}"
    )

    served = client.get_card(book, uid)
    _assert_intact(
        None if served is None else served.encode(),
        must_contain=[f"UID:{uid}", "S6.11 At-Rest Contact", _CARD_SECRET],
    )
    assert served is not None and served.encode() == baseline, (
        "two consecutive GETs of the same segment-served card differ — the open "
        "path is not deterministic"
    )

    got = client.multiget(book, [r.href for r in client.list_card_hrefs(book)])
    assert [r.uid for r in got] == [uid], f"addressbook-multiget did not return the card: {got}"
    assert _CARD_SECRET in (got[0].vcf or ""), (
        "addressbook-multiget served a card whose sealed NOTE did not open"
    )


@pytest.mark.feature("calendar-in-standard-apps")
def test_every_dav_record_at_rest_is_a_sealed_envelope_across_many_records(
    restartable_mda_nest,
):
    """The conformance walk at width: several events AND cards, written through
    the real DAV paths, every one of them a sealed envelope at rest.

    A single-record walk cannot catch a rollout that seals the first record and
    then drifts (a second segment, a rotated bucket, a supersede path).
    """
    handle = restartable_mda_nest
    nest = handle.nest_instance
    actor_bytes = handle.recipient_actor["actor_id_bytes"]
    actor_hex = handle.recipient_actor["actor_id_hex"]

    mua = _caldav(handle)
    client = _carddav(handle)

    n_events, n_cards = 3, 2
    for i in range(n_events):
        _put_event(mua, f"s611-many-cal-{i}", f"S6.11 bulk event {i}")
    for i in range(n_cards):
        _put_card(client, f"s611-many-card-{i}", f"S6.11 Bulk Contact {i}")

    for kind, want in (("calendar", n_events), ("card", n_cards)):
        walk = _conformance(nest["url"], actor_hex, kind)
        assert walk["total"] == want, (
            f"expected {want} {kind} records at rest, walk saw {walk['total']}: {walk}"
        )
        assert walk["unsealed"] == [] and walk["sealed"] == want, (
            f"not every {kind} record at rest is a sealed envelope: {walk}"
        )
        # The metadata-row half, for all rows at once.
        assert assert_rows_carry_no_body(nest, kind, actor_bytes) == want
        assert live_segment_records(nest, kind, actor_bytes) == want

    assert_nothing_plaintext(
        bodies_at_rest(nest, "calendar", actor_bytes),
        [b"BEGIN:VCALENDAR", _EVENT_SECRET.encode()],
    )
    assert_nothing_plaintext(
        bodies_at_rest(nest, "card", actor_bytes),
        [b"BEGIN:VCARD", _CARD_SECRET.encode()],
    )


# ── 4: snapshot create → restore, through the real binary ────────────────────


def _owner_ws(handle) -> WsRpcAdminClient:
    """A User-class WS-RPC client driven by the recipient's own Ed25519 identity
    — the snapshot kinds are owner-implicit, so the data owner IS the caller."""
    actor = handle.recipient_actor
    return WsRpcAdminClient(
        handle.nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


@pytest.mark.parametrize("kind", ["calendar", "card"])
@pytest.mark.feature("calendar-in-standard-apps")
def test_snapshot_create_then_restore_round_trips_a_dav_body(
    restartable_mda_nest, kind,
):
    """Snapshot → DELETE the record through the real DAV path → restore → the
    body is BACK and serves byte-identically.

    This is the S6.9 create/restore arm's real-binary proof (its in-process twin
    is `create_then_restore_{calendar,card}_round_trips_*`). The DELETE between
    create and restore is what makes it a genuine no-data-loss assertion rather
    than a no-op: S6.8a tombstones the content record on delete, so the body only
    comes back because restore rebuilds the mirror from the segments the snapshot
    PINNED, and rebuilds the row from the placement journal ⋈ floor/envelope.

    Before S6.9 the calendar create arm was `calendar_not_implemented()` and card
    had none — which is why `tests/api/test_dr_restore.py` could only ever drive
    the mail kind.
    """
    handle = restartable_mda_nest
    nest = handle.nest_instance
    actor_bytes = handle.recipient_actor["actor_id_bytes"]

    uid = f"s611-snapshot-{kind}"
    if kind == "calendar":
        mua = _caldav(handle)
        collection, baseline = _put_event(mua, uid, "S6.11 snapshot event")

        def read() -> bytes | None:
            return mua.get_event_bytes(collection, uid)

        def delete() -> None:
            mua.delete_event(collection, uid)
    else:
        mua = _carddav(handle)
        collection, baseline = _put_card(mua, uid, "S6.11 Snapshot Contact")

        def read() -> bytes | None:
            vcf = mua.get_card(collection, uid)
            return None if vcf is None else vcf.encode()

        def delete() -> None:
            mua.delete_card(collection, uid)

    assert read() == baseline, "sanity: the record must serve before the snapshot"

    with _owner_ws(handle) as ws:
        created = ws.call(
            "fauna.filesync.snapshot.create_message_kind", {"kind": kind}
        )
        snapshot_id = created["snapshot_id"]
        assert created["kind"] == kind
        assert isinstance(snapshot_id, int) and snapshot_id > 0

        # ── destroy it through the real DAV path (row deleted + content record
        # tombstoned by S6.8a).
        delete()
        assert read() is None, "sanity: the DELETE must actually remove the record"

        restored = ws.call(
            "fauna.filesync.snapshot.restore_message_kind",
            {"snapshot_id": snapshot_id, "confirm_id": str(snapshot_id)},
        )
        assert restored["snapshot_id"] == snapshot_id
        assert restored["kind"] == kind

    # ── NO DATA LOSS: the body is back, byte-identical, and still sealed at rest.
    assert read() == baseline, (
        f"the restored {kind} record did not come back byte-identically — the "
        "snapshot/restore round trip lost or corrupted the body"
    )
    assert live_segment_records(nest, kind, actor_bytes) >= 1, (
        "restore rebuilt the row without a live mirror row — the body is unreachable"
    )
    walk = _conformance(nest["url"], handle.recipient_actor["actor_id_hex"], kind)
    assert walk["unsealed"] == [] and walk["sealed"] == walk["total"] >= 1, (
        f"a restored {kind} record is not a sealed envelope at rest: {walk}"
    )


# ── 5: the contacts app reconverges ACROSS a card restore ───────────────────


@pytest.mark.feature("address-book")
def test_card_restore_tells_the_contacts_app_to_reconverge(restartable_mda_nest):
    """After a card restore, an already-synced CardDAV client is TOLD to
    re-enumerate — and re-enumerating gets it the restored card.

    `docs/goal/behavior/carddav-server.md` § Storage model → *Durability &
    disaster recovery* promises that a restore tells an already-synced MUA to
    reconverge (corrected 2026-09-21 from "MUA sync-token/ETag continuity",
    the phrase that misled this test's first draft). The S6.9 arm above
    (`test_snapshot_create_then_restore_round_trips_a_dav_body`) proves the BODY
    survives, but it drives the round trip through plain GETs and never holds a
    sync token across it, so it cannot see what an already-synced client sees.

    ⚠ **A stale token here is CORRECT, and asserting otherwise inverts the
    contract.** Restore rewinds the collection, so the server's modseq lands
    *behind* what the MUA already holds. RFC 6578 §3.8 has exactly one honest
    answer to that, and the bridge gives it: the `DAV:valid-sync-token`
    precondition failure, which directs the client to a full PROPFIND
    (`bins/fauna-bridges/internal/mda/carddav/sync_collection.go` — the
    `SyncAddressbookSinceStale` arm, commented "post-DR-restore 'MUA ahead'
    case"). `tests/api/test_dr_restore.py` pins the same outcome on the CalDAV
    twin, where the divergence also feeds the Backups page's "N writes lost"
    banner. So "still in sync" means the app **reconverges loudly**, not that a
    pre-restore delta token keeps resolving.

    The failure this test exists to catch is therefore the SILENT one: the
    server answering that stale token with a delta that omits the restored card.
    A client would apply an empty delta, believe itself current, and never show
    the card again — no error anywhere. Both halves are asserted:

      1. the stale token is REFUSED (`DAV:valid-sync-token`), never served a
         misleading delta;
      2. the re-enumeration it directs the client to actually carries the card
         back, with an ETag, byte-identical and still sealed at rest.

    ⚠ Byte-identity is asserted against the served **baseline**, never the PUT
    body — see :func:`_put_card` and the module docstring.
    """
    handle = restartable_mda_nest
    nest = handle.nest_instance
    actor_bytes = handle.recipient_actor["actor_id_bytes"]

    mua = _carddav(handle)
    uid = "s611-restore-reconverge"
    book, baseline = _put_card(mua, uid, "S6.11 Reconverge Contact")

    # ── 1: the contacts app's first sync — the state a real MUA is in before
    # anything goes wrong.
    changed, _removed, token_before = mua.sync(book)
    pre = next((c for c in changed if c.uid == uid), None)
    assert pre is not None, (
        f"sanity: the initial sync-collection must carry the card {uid} — got "
        f"uids {[c.uid for c in changed]}"
    )
    assert pre.etag, "sanity: a synced card must carry an ETag for the MUA to cache"
    assert token_before, "sanity: sync-collection must return a sync token"

    with _owner_ws(handle) as ws:
        created = ws.call("fauna.filesync.snapshot.create_message_kind", {"kind": "card"})
        snapshot_id = created["snapshot_id"]

        # ── 2: destroy it through the real CardDAV path (S6.8a tombstones the
        # content record) and let the MUA observe the removal, exactly as a real
        # one would before anyone notices something is wrong.
        mua.delete_card(book, uid)
        assert mua.get_card(book, uid) is None, "sanity: the DELETE must remove the card"

        _changed, _removed, token_after_delete = mua.sync(book, token_before)
        assert token_after_delete, "the post-delete sync must still mint a token"

        # ── 3: restore.
        restored = ws.call(
            "fauna.filesync.snapshot.restore_message_kind",
            {"snapshot_id": snapshot_id, "confirm_id": str(snapshot_id)},
        )
        assert restored["kind"] == "card"

    # ── 4: NO DATA LOSS — the body is back, byte for byte.
    served = mua.get_card(book, uid)
    assert served is not None and served.encode() == baseline, (
        "the restored card did not come back byte-identically — the "
        "snapshot/restore round trip lost or corrupted the body"
    )

    # ── 5a: the MUA's now-rewound token is REFUSED, not answered with a delta.
    # Serving one here is the silent failure: the client would apply it, believe
    # itself current, and never see the restored card again.
    with pytest.raises(CardDAVError) as stale:
        mua.sync(book, token_after_delete)
    assert "valid-sync-token" in str(stale.value), (
        "a rewound collection must refuse the MUA's pre-restore token with the "
        "RFC 6578 §3.8 `DAV:valid-sync-token` precondition so the client "
        f"re-enumerates; got {stale.value}"
    )

    # ── 5b: and the re-enumeration it directs the client to reconverges on the
    # restored card. Without this the refusal above would just be a dead end.
    rechanged, _reremoved, token_after_restore = mua.sync(book)
    assert token_after_restore, "the re-enumeration must mint a usable token"
    back = next((c for c in rechanged if c.uid == uid), None)
    assert back is not None, (
        "the restored card never reached the contacts app: a full re-enumeration "
        f"— the remedy the stale-token refusal directs it to — does not list it "
        f"(uids {[c.uid for c in rechanged]}). The body is on the nest but no "
        "client would ever see it again."
    )
    assert back.etag, "the reconverged card must carry an ETag for the MUA to cache"
    assert back.vcf is not None and back.vcf.encode() == baseline, (
        "the re-enumerated body differs from the pre-snapshot baseline — the "
        "client would reconverge onto the WRONG bytes"
    )
    multiget = {c.uid: c.etag for c in mua.multiget(book, [back.href])}
    assert multiget.get(uid) == back.etag, (
        f"the collection disagrees with itself about the restored card's ETag "
        f"(sync-collection {back.etag!r} vs multiget {multiget.get(uid)!r}) — a "
        "client caching one and revalidating against the other would thrash"
    )

    # ── 6: and it is still SEALED at rest — a restore must not reconstitute a
    # plaintext record (address-book outcome 2's half of this witness).
    assert live_segment_records(nest, "card", actor_bytes) >= 1, (
        "restore rebuilt the row without a live mirror row — the body is unreachable"
    )
    walk = _conformance(nest["url"], handle.recipient_actor["actor_id_hex"], "card")
    assert walk["unsealed"] == [] and walk["sealed"] == walk["total"] >= 1, (
        f"a restored card is not a sealed envelope at rest: {walk}"
    )
    assert_nothing_plaintext(
        bodies_at_rest(nest, "card", actor_bytes), [_CARD_SECRET.encode()]
    )

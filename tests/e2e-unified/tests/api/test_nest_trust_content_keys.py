"""A content trust opens only its own kind's records: the calendar grant opens
calendar records and nothing else, and the mail grant opens mail but never an
event or a contact card.

Owner docs: ``docs/goal/ui/nests.md`` § Trust facet — grants (Now lens) (the
*Read my calendar* option) and its § Implementation status today (the calendar
option opens only the calendar, built 2026-10-26);
``docs/goal/architecture/key-material-hierarchy.md`` § Implementation status
today (the calendar and card keypairs, each its own). Catalog outcome
``nests-and-trust`` 26, a ``[nest]`` outcome: the key split lives in shared
client code and the nest's stored deposit, so it is witnessed without an app
(``feature-catalog.md`` § The two surfaces).

Every write goes through the shared machines the apps run, app-less: mail is
enabled by the Mail settings machine (``harness_enable_mail`` — which registers
the owner's mail, calendar and card recipient keys on the nest) and both trusts
are minted by the Nests page's ``LinkedNestsMachine`` from its own derived
picker options (``harness_mint_trust``). The holder is an enrolled ``mda``
bridge whose X25519 secret the test holds (``seed_mda_holder``, an arranging
precondition). The holder side is the seal-helper's ``open-records-with-grant``
— the production drain's ``UnsealCapabilityGrant`` with the holder's secret,
then every key the grant carries tried on every record under the record's own
kind — so a refusal is an AEAD failure, an empty ``opened_by``: the holder
opens records locally and no wire code exists to name.
"""

import base64
import json
import sqlite3
import time

import pytest

import fauna_ffi
from helpers.nest_trust_setup import seed_mda_holder

pytestmark = pytest.mark.tier_3

#: Deadline for the two deposits to land in ``capability_grants`` (a WS-RPC
#: round trip each) — a poll ceiling, never a settle-sleep (convention 14).
_DEPOSIT_S = 20.0

MAIL = "content.read{mail}"
CALENDAR = "content.read{calendar}"


@pytest.fixture
def trust_keys_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest: holder discovery is admin-gated, so the owner is the
    nest's admin, and a grant cannot be un-minted from a shared nest's log."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trust-keys-nest")
    yield nest
    cleanup()


def _owner_rows(nest, sql: str) -> list[tuple]:
    owner = bytes.fromhex(nest["admin"]["actor_id_hex"])
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return conn.execute(sql, (owner,)).fetchall()
    finally:
        conn.close()


def _grant_blobs(nest, count: int) -> list[bytes]:
    """The canonical deposits the nest holds for the owner — what a holder's
    ``fauna.capabilities.fetch`` answers with — once ``count`` have landed."""
    sql = ("SELECT blob FROM capability_grants WHERE owner_actor_id = ?"
           " ORDER BY created_at")
    deadline = time.monotonic() + _DEPOSIT_S
    rows = _owner_rows(nest, sql)
    while len(rows) < count and time.monotonic() < deadline:
        time.sleep(0.25)
        rows = _owner_rows(nest, sql)
    assert len(rows) == count, f"expected {count} deposits on the nest, found {len(rows)}"
    return [bytes(r[0]) for r in rows]


@pytest.mark.feature("nests-and-trust")
def test_a_calendar_trust_opens_only_calendar_records_and_a_mail_trust_no_event_or_card(
    trust_keys_nest, run_seal_helper
):
    nest = trust_keys_nest
    url = nest["url"]
    owner_seed = bytes(nest["admin"]["signing_key"])

    # ── Arrange: mail on (registers the recipient keys), an mda holder. ──
    fauna_ffi.harness_enable_mail(url, owner_seed)
    holder = seed_mda_holder(nest)
    keys = _owner_rows(
        nest,
        "SELECT mls_pubkey, calendar_pubkey, card_pubkey FROM actor_mls_pubkeys"
        " WHERE actor_id = ?",
    )
    assert keys, "enabling mail must register the owner's recipient keys"
    mail_pub, calendar_pub, card_pub = (bytes(k) for k in keys[0])
    assert len({mail_pub, calendar_pub, card_pub}) == 3, (
        "the mail, calendar and card recipient keys must each be their own"
    )

    # ── Act: mint both trusts from the Nests page's own picker options. ──
    fauna_ffi.harness_mint_trust(url, owner_seed, "mail")
    fauna_ffi.harness_mint_trust(url, owner_seed, "calendar")
    blobs = _grant_blobs(nest, 2)

    # One record of each kind, sealed to the key the owner registered for it —
    # the seal targets the MDA and the calendar/card RPCs use.
    def seal(kind: str, plaintext: bytes, pubkey: bytes) -> str:
        return base64.b64encode(run_seal_helper("seal-mail-record", {
            "plaintext_b64": base64.b64encode(plaintext).decode(),
            "recipient_x25519_pubkey_b64": base64.b64encode(pubkey).decode(),
            "kind": kind,
        })).decode()

    mail = b"Subject: content-keys\r\n\r\nbody"
    event = b"BEGIN:VCALENDAR\r\nSUMMARY:content-keys\r\nEND:VCALENDAR\r\n"
    card = b"BEGIN:VCARD\r\nFN:Content Keys\r\nEND:VCARD\r\n"
    kinds = ("mail", "calendar", "card")
    records = [
        {"kind": "mail", "envelope_b64": seal("mail", mail, mail_pub)},
        {"kind": "calendar", "envelope_b64": seal("calendar", event, calendar_pub)},
        {"kind": "card", "envelope_b64": seal("card", card, card_pub)},
    ]

    # ── Assert, as the holder: each deposit opens exactly its own kind. ──
    by_scope: dict[tuple[str, ...], list[dict]] = {}
    for blob in blobs:
        opened = json.loads(run_seal_helper("open-records-with-grant", {
            "grant_blob_b64": base64.b64encode(blob).decode(),
            "holder_x25519_secret_b64": base64.b64encode(holder.secret).decode(),
            "records": records,
        }))
        scopes = tuple(sorted({s for rec in opened for s in rec["opened_by"]}))
        by_scope[scopes] = opened
    assert set(by_scope) == {(MAIL,), (CALENDAR,)}, (
        "each deposit must open records under its own scope alone; per deposit, "
        f"the scopes that opened anything: {sorted(by_scope)!r}"
    )

    def opened_by(grant: list[dict]) -> dict[str, list[str]]:
        return {kind: rec["opened_by"] for kind, rec in zip(kinds, grant)}

    calendar_grant, mail_grant = by_scope[(CALENDAR,)], by_scope[(MAIL,)]
    assert opened_by(calendar_grant) == {"mail": [], "calendar": [CALENDAR], "card": []}, (
        f"the calendar trust must open the event and neither mail nor the card: "
        f"{opened_by(calendar_grant)!r}"
    )
    assert base64.b64decode(calendar_grant[1]["plaintext_b64"]) == event
    assert opened_by(mail_grant) == {"mail": [MAIL], "calendar": [], "card": []}, (
        f"the mail trust must open mail and neither the event nor the card: "
        f"{opened_by(mail_grant)!r}"
    )
    assert base64.b64decode(mail_grant[0]["plaintext_b64"]) == mail

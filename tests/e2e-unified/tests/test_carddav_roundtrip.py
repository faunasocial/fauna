"""tier_3: generic CardDAV client round-trip through the real MDA — slice 2e.

The CardDAV twin of the shipped CalDAV tier_3 round-trip
(`test_caldav_mkcalendar_create.py` / `test_caldav_discovery_sequence.py`): a
scripted CardDAV client (`helpers/carddav_client.CardDAVClient`, speaking the raw
RFC 6352 / 6578 wire over HTTPS Basic Auth — the surface Apple Contacts / DAVx5 /
Thunderbird use) drives the REAL binary stack (real nest + real mail-bridge MDA,
real HTTPS, real seal of the vCard body with the actor's MLS pubkey, real
`put_card_ciphertext` insert into `bridge_carddav_cards`) end-to-end.

Every nest-Rust + Go serving/discovery/CRUD path is already conformance-green
(`carddav-server.md` § Implementation status);
this is the missing **client-integration** proof that a real client walks the
served surface. Two entry paths (carddav-server.md § Read/Write/sync surface +
§ Network exposure):

  1. DIRECT collection URL — point the client at `/carddav/{user}/` and run the
     full CRUD cycle: lazy Contacts book → PUT a vCard → PROPFIND Depth:1
     enumerates it → addressbook-multiget REPORT returns it by href →
     sync-collection REPORT returns it, then after a change returns the delta →
     DELETE the card (tombstone surfaces on the next sync) → DELETE the collection
     (204; a re-DELETE is 404 — `delete_addressbook` is idempotent). The at-rest
     card body is asserted CIPHERTEXT (SEAL-ALWAYS — carddav-server.md § Threat
     model). Since the Phase-3 S6.6 cutover the body rests in the `__card`
     segment store — the `bridge_carddav_cards` row carries no body — so the
     assertion reads the segment files.
  2. HOST-ONLY autodiscovery — point the client at just the server root and walk
     current-user-principal → addressbook-home-set → book (the unified principal,
     Track B). The SRV / apex-well-known hops are DNS-level and already
     conformance-green; this is the client-integration proof of the WebDAV walk.

A DEDICATED nest (`dedicated_mail_nest`) is used, mirroring the CalDAV round-trip:
the nest admin must be the only actor enabling mail there. At
MDA boot `carddav_enabled` inherits `mail_enabled` (unset → true —
bridge_routing_handlers.rs `get_carddav_enabled().unwrap_or(mail_enabled)`), so
the shared :443 DAV listener mounts `/carddav/` from boot exactly as it mounts
`/caldav/` for the CalDAV round-trip — no separate CardDAV enable/port. Enabling
mail through the client UI provisions the recipient MLS pubkey + wrapped-MSEK +
MLS snapshot the MDA seals/opens card bodies with (the same key material the
CalDAV `test_caldav_mkcalendar_create` preamble mints).

Only tier_3 catches this: the vCard the client seals server-side must open under
the same MDA session capability on the REPORT read path, and the at-rest row must
be ciphertext — a seal/open contract no stub or in-process Go twin exercises.

carddav-server.md § Read/Write/sync surface + § Threat model + § Network exposure.
"""

import time
import uuid

import pytest

from helpers.carddav_client import CardDAVClient, build_vcard
from helpers.mail_dedicated_nest import (
    alias_admin_to_address as _alias_admin_to_address,
    dedicated_node_url as _dedicated_node_url,
    login_as_nest_admin as _login_as_nest_admin,
)
from helpers.segment_at_rest import assert_nothing_plaintext, bodies_at_rest

# Drives the client's own "Enable mail" settings UI to mint the seal/open key
# material; every app implements the mail-settings page.
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    # web added: the CardDAV walk beyond enable_mail_plain
    # is a scripted CardDAVClient, driving no app-specific UI at all.
    pytest.mark.web,
    # android added 2026-10-05: its mail-settings form paints every element
    # `enable_mail_plain` drives, and the dedicated nest reaches the device
    # through `_relaunch_trusting_nest`'s `adb reverse`.
    pytest.mark.android,
]

# The PLAIN mail credential the client mints and the CardDAV client authenticates
# with (shared IMAP + CalDAV + CardDAV, AEAD-unwrap-as-auth). A KNOWN value so the
# round-trip holds the exact password — mirrors the proven-green CalDAV tier_3
# tests' `_PASSWORD`.
_PASSWORD = "CardDavRoundtripPlainPw0009Kk"

# How long to wait for a card write to surface to a REPORT (a server-side seal, so
# usually immediate — a short retry guards a settle race). Twin of the CalDAV
# round-trip's `_PROP_TIMEOUT`.
_PROP_TIMEOUT = 30.0


def _enable_mail_and_client(app, handle, request):
    """Shared preamble: log the client in as the dedicated nest admin, enable mail
    through its own UI (provisions the recipient MLS pubkey + wrapped-MSEK + MLS
    snapshot the MDA seals/opens card bodies with — sess.MLSPubkey() /
    MLSSnapshotBytes()), give the admin a routable address, and return a SERVING
    CardDAVClient plus the nest + admin address. Mirrors the CalDAV
    `test_caldav_mkcalendar_create` preamble (priority #2/#4 — one shape)."""
    nest = handle.nest
    domain = handle.domain

    _login_as_nest_admin(app, nest, _dedicated_node_url(app, handle, request))
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()
    admin_addr = _alias_admin_to_address(nest, domain)  # admin@<domain>

    base = f"https://127.0.0.1:{handle.caldav_port}"
    client = CardDAVClient(base, admin_addr, _PASSWORD, verify=False)
    client.wait_until_serving()
    return client, nest, admin_addr


def _wait_uid_present(client, book, uid, *, timeout=_PROP_TIMEOUT):
    """Poll a PROPFIND+multiget enumeration until ``uid`` is present."""
    deadline = time.monotonic() + timeout
    seen: list[str] = []
    while time.monotonic() < deadline:
        seen = client.uids(book)
        if uid in seen:
            return seen
        time.sleep(1.0)
    return seen



@pytest.mark.feature("contacts-in-standard-apps")
def test_carddav_direct_url_round_trip(app, dedicated_mail_nest, request):
    """A scripted CardDAV client, pointed straight at `/carddav/{user}/`, runs the
    full RFC 6352 CRUD cycle end-to-end and proves the stored body is sealed.

    RED before the CardDAV MDA surface existed: the shared :443 listener never
    mounted `/carddav/`, so every PROPFIND/PUT/REPORT 404'd.
    """
    app.mail_settings.require_scripted_mua_seed_supported()

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, nest, admin_addr = _enable_mail_and_client(app, handle, request)
    admin_actor_id = bytes(nest["admin"]["signing_key"].verify_key)

    # 1) Lazy "Contacts" address book — auto-provisioned on the first PROPFIND of
    #    an empty home set (carddav backend.go lazyProvisionContacts).
    book = client.contacts_addressbook()
    assert book, "the lazy Contacts book must exist on first PROPFIND"

    # 2) PUT a vCard carrying private fields (name, phone, email, note) — the MDA
    #    validates VERSION+FN+UID, seals the body to the actor's MLS pubkey, and
    #    rewrites Location to the canonical blake3(UID)[:32].vcf.
    uid = f"urn:uuid:{uuid.uuid4()}"
    fn = f"QA Contact {int(time.time())}"
    tel = "+15550142"
    email = "qa-contact@example.com"
    note = "sealed private note — must never reach nest in plaintext"
    vcf = build_vcard(uid, fn, email=email, tel=tel, org="Fauna QA", note=note)
    etag = client.put_card(book, uid, vcf)
    assert etag, "PUT must return an ETag (the MDA sets it from the nest reply)"

    # 3) PROPFIND Depth:1 (+ addressbook-multiget for bodies) enumerates it.
    seen = _wait_uid_present(client, book, uid)
    assert uid in seen, (
        f"card {uid!r} PUT to {book!r} never surfaced to a PROPFIND+multiget "
        f"enumeration within {_PROP_TIMEOUT:.0f}s; saw {seen!r}"
    )
    cards = client.list_cards(book)
    got = next((c for c in cards if c.uid == uid), None)
    assert got is not None and got.fn == fn, (
        f"the enumerated card must carry the sealed FN {fn!r}; got {got!r}"
    )
    assert got.vcf and tel in got.vcf and email in got.vcf, (
        "the round-tripped vCard body must carry the phone + email the client PUT "
        f"(decrypt-on-read); got {got.vcf!r}"
    )

    # 4) addressbook-multiget REPORT returns the same card by its href.
    mg = client.multiget(book, [got.href])
    assert len(mg) == 1 and mg[0].uid == uid and mg[0].fn == fn, (
        f"addressbook-multiget of {got.href!r} must return the card; got {mg!r}"
    )

    # 5) sync-collection REPORT (RFC 6578): the initial sync returns the card;
    #    after a CHANGE (re-PUT with a new FN) a delta sync returns just the delta.
    changed, removed, token1 = client.sync(book)
    assert token1, "initial sync must return a sync-token"
    assert any(c.uid == uid for c in changed), (
        f"initial sync-collection must include card {uid!r}; got {[c.uid for c in changed]!r}"
    )

    fn2 = f"{fn} (updated)"
    vcf2 = build_vcard(uid, fn2, email=email, tel=tel, org="Fauna QA", note=note)
    client.put_card(book, uid, vcf2)
    # Poll the delta until the updated FN shows (server-side seal → usually
    # immediate; a re-PUT bumps modseq so it re-surfaces past token1).
    deadline = time.monotonic() + _PROP_TIMEOUT
    delta_fns: list[str] = []
    token2 = token1
    while time.monotonic() < deadline:
        d_changed, _d_removed, token2 = client.sync(book, token1)
        delta_fns = [c.fn for c in d_changed]
        if fn2 in delta_fns:
            break
        time.sleep(1.0)
    assert fn2 in delta_fns, (
        f"a delta sync since {token1!r} must return the updated card {fn2!r}; "
        f"got changed FNs {delta_fns!r}"
    )

    # 6) DELETE the card → the tombstone surfaces as a removed href on the next
    #    sync (RFC 6578 §3.6).
    client.delete_card(book, uid)
    deadline = time.monotonic() + _PROP_TIMEOUT
    removed_seen: list[str] = []
    while time.monotonic() < deadline:
        _c, removed_seen, _t = client.sync(book, token2)
        if removed_seen:
            break
        time.sleep(1.0)
    assert removed_seen, (
        f"deleting card {uid!r} must surface a tombstone on the next sync since "
        f"{token2!r}; got no removed hrefs"
    )
    assert uid not in client.uids(book), (
        "the deleted card must be gone from a fresh enumeration"
    )

    # 7) SEAL-ALWAYS at-rest assertion: re-add a card, then read its stored body
    #    straight from SQLite and prove it is ciphertext (never the plaintext
    #    vCard). nest's bridge_carddav_cards must never hold plaintext.
    uid3 = f"urn:uuid:{uuid.uuid4()}"
    secret_note = "TOPSECRET-at-rest-marker-9137"
    vcf3 = build_vcard(uid3, "Sealed Person", email="sealed@example.com",
                       tel="+15550199", note=secret_note)
    client.put_card(book, uid3, vcf3)
    assert uid3 in _wait_uid_present(client, book, uid3), "re-added card must enumerate"
    # Reads the `__card` segment files (and asserts the metadata row carries no
    # body) — see `helpers.segment_at_rest`, which owns this
    # probe for every kind whose body moved out of a SQLite column.
    assert_nothing_plaintext(
        bodies_at_rest(nest, "card", admin_actor_id),
        [b"BEGIN:VCARD", secret_note.encode(), b"sealed@example.com"],
    )

    # 8) Collection DELETE → 204; an idempotent re-DELETE of the same book → 404.
    status = client.delete_addressbook(book)
    assert status == 204, f"DELETE of the address-book collection must be 204; got {status}"
    restatus = client.delete_addressbook(book)
    assert restatus == 404, (
        f"a re-DELETE of the already-deleted book must be 404 (idempotent "
        f"delete_addressbook); got {restatus}"
    )


@pytest.mark.feature("contacts-in-standard-apps")
def test_carddav_host_only_autodiscovery(app, dedicated_mail_nest, request):
    """A CardDAV client that knows only the server host (no collection path) walks
    current-user-principal → addressbook-home-set → book, then round-trips a card
    at the DISCOVERED book — the client-integration proof of the RFC-6764 /
    unified-principal discovery chain (Track B).

    The SRV `_carddavs._tcp` + apex `/.well-known/carddav` hops are DNS-level and
    already conformance-green; this exercises the WebDAV walk a real client
    performs after resolving the host.
    """
    app.mail_settings.require_scripted_mua_seed_supported()

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, _admin_addr = _enable_mail_and_client(app, handle, request)

    # Walk root → current-user-principal → addressbook-home-set → book, WITHOUT
    # the client ever being told the collection path.
    book = client.discover_addressbook("/")
    assert "/carddav/" in book, (
        f"autodiscovery must resolve to a /carddav/ book href; got {book!r}"
    )

    # Prove the discovered book is a working surface: PUT + read a card back at it.
    uid = f"urn:uuid:{uuid.uuid4()}"
    fn = f"Discovered Contact {int(time.time())}"
    client.put_card(book, uid, build_vcard(uid, fn, email="disco@example.com"))
    seen = _wait_uid_present(client, book, uid)
    assert uid in seen, (
        f"a card PUT to the AUTODISCOVERED book {book!r} must round-trip; saw {seen!r}"
    )


@pytest.mark.feature("contacts-in-standard-apps")
def test_cards_from_a_contacts_app_never_become_fauna_contacts(
    app, dedicated_mail_nest, request
):
    """A card a contacts app adds lands in the address book — the Fauna app shows
    it on the Address Book tab — and nowhere else: the People roster, its
    pending confirmations and the contact requests are exactly as they were
    (`carddav-server.md` § What a CardDAV address book *is*: two stores, never
    one).

    The Address Book tab showing the card is what makes the "unchanged" half
    mean something: the app has demonstrably read the new card, and still
    minted no contact or request from it."""
    app.mail_settings.require_scripted_mua_seed_supported()
    app.contacts.require_address_book_supported()

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    client, _nest, admin_addr = _enable_mail_and_client(app, handle, request)

    app.contacts.navigate()
    app.contacts.switch_to_people()
    roster_before = sorted(app.contacts.contact_names())
    confirms_before = app.contacts.confirm_count()
    knocks_before = app.contacts.knock_count()

    # A card that looks as much like a Fauna person as a card can: a name, and
    # an address on this very nest.
    book = client.contacts_addressbook()
    assert book, "the lazy Contacts book must exist on first PROPFIND"
    uid = f"urn:uuid:{uuid.uuid4()}"
    fn = f"Grace Hopper {int(time.time())}"
    domain = admin_addr.partition("@")[2]
    vcf = build_vcard(uid, fn, email=f"grace-{uuid.uuid4().hex[:6]}@{domain}", tel="+15550199")
    assert client.put_card(book, uid, vcf), "the contacts app's PUT must return an ETag"
    assert uid in _wait_uid_present(client, book, uid), "the card must be in the address book"

    app.contacts.navigate()
    app.contacts.switch_to_address_book()
    assert app.contacts.wait_for_addressbook(), (
        f"the Address Book tab must list the book; error={app.error_text()!r}"
    )
    app.contacts.select_addressbook(0)
    assert app.contacts.wait_for_card(fn, timeout=30.0), (
        f"the Address Book tab must show the card the contacts app added; "
        f"names={app.contacts.card_names()!r} error={app.error_text()!r}"
    )

    app.contacts.switch_to_people()
    roster_after = sorted(app.contacts.contact_names())
    assert roster_after == roster_before, (
        f"a card from a contacts app must never become a Fauna contact: roster "
        f"{roster_before!r} → {roster_after!r}"
    )
    assert not any(fn in n for n in roster_after), (
        f"{fn!r} appeared on the People roster: {roster_after!r}"
    )
    assert app.contacts.confirm_count() == confirms_before, (
        "a card from a contacts app must not create a contact awaiting confirmation"
    )
    assert app.contacts.knock_count() == knocks_before, (
        "a card from a contacts app must not create a contact request"
    )
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"

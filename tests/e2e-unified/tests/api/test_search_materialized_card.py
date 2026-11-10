"""tier_3 — with your address book kept searchable on your nest, your search
finds a phrase from a card your contacts app saved, nobody else's search does,
and turning it off deletes the nest's card copy and only that one.

`[nest]` outcome of `docs/features/search.md` (the card sibling of the mail
witness `test_search_materialized_mail.py`). Owner doc:
`docs/goal/architecture/encryption-at-rest.md` § Capability tiering → *The
materialization grant* (parts (c) one view per granted kind under
`__materialized/<owner>/<kind>/`, (d) a card hit is `content_type` `contact`
keyed by the hex `uid_hash`) and § Readable classes, class 4 (own-actor scopes
only; revoke = keys dropped + that kind's view deleted).

Every step is the production path, app-less:

- mail is enabled by the shared Mail settings machine (``harness_enable_mail``),
  which leaves the MSEK in the owner's sealed custody, registers the owner's
  own calendar and card recipient keys, and hands back the plain credential a
  contacts app signs in with;
- both grants — card and calendar — are minted by the Nests page's own
  ``LinkedNestsMachine`` (``harness_mint_trust``), so the revoke can be seen
  to leave the other kind's view standing;
- the card is saved the way a person's contacts app saves it: a CardDAV PUT
  over HTTPS to the Go MDA, which seals it to the owner's card key;
- the owner's ``fauna.search.query`` finds it by a note phrase once the
  materializer's trickle has indexed it (a deadline poll, convention 14), the
  hit's ``content_id`` the hex ``uid_hash`` the MDA names the resource by;
- another member's query over the same phrase finds nothing;
- ``fauna.capabilities.revoke`` on the card grant deletes
  ``__materialized/<owner>/card/`` before it replies, the calendar view
  standing.

The restart, the type filter, the card delete and the version-1 area are
pinned at tier_3 in Rust (``bins/fauna-nest/tests/conformance_materialization_grant.rs``);
this test is the cross-process witness of the journey.
"""

import secrets
import time

import pytest

import fauna_ffi
from helpers.materialized_views import kind_dir, mint_kind, uid_hash_hex, wait_hits, hits

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("search")
def test_your_address_book_kept_searchable_on_your_nest_is_found_by_you_alone_and_turning_it_off_deletes_that_view(
    nest_instance, mail_bridge_mda
):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register
    from helpers.carddav_client import CardDAVClient, build_vcard
    from helpers.mail_aliases import add_exact_alias

    url = nest_instance["url"]
    db_path = nest_instance["db_path"]
    admin_key = nest_instance["admin"]["signing_key"]
    owner = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_key)
    other = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_key)
    owner_seed = bytes(owner["signing_key"])
    owner_id, owner_hex = owner["actor_id_bytes"], owner["actor_id_hex"]

    # ── Arrange: mail on (the DAV credential), an address to sign in as. ──
    password = fauna_ffi.harness_enable_mail(url, owner_seed)
    address = add_exact_alias(url, owner["signing_key"], mail_bridge_mda.domain,
                              f"card-{secrets.token_hex(4)}")

    # ── Act: the card and calendar "keep searchable" grants. ──
    card_grant = mint_kind(url, db_path, owner_seed, owner_id, "card")
    mint_kind(url, db_path, owner_seed, owner_id, "calendar")
    card_view = kind_dir(db_path, owner_hex, "card")
    calendar_view = kind_dir(db_path, owner_hex, "calendar")

    # ── Act: the contacts app saves a card. ──
    book = CardDAVClient(f"https://127.0.0.1:{mail_bridge_mda.caldav_port}",
                         address, password, verify=False)
    book.wait_until_serving()
    book_href = book.contacts_addressbook()
    uid = f"urn:uuid:{secrets.token_hex(16)}"
    phrase = f"wickerlantern{time.time_ns()}"
    book.put_card(book_href, uid, build_vcard(uid, "Ada Millwright",
                                              note=f"Mends the {phrase} at the mill."))
    saved = [c for c in book.list_cards(book_href) if c.uid == uid]
    assert len(saved) == 1, f"the saved card must be listed in its book: {book.uids(book_href)!r}"
    want_id = uid_hash_hex(saved[0].href, ".vcf")

    owner_ws = WsRpcAdminClient(url, actor_id=owner_id, signing_key=owner_seed)
    other_ws = WsRpcAdminClient(url, actor_id=other["actor_id_bytes"],
                                signing_key=bytes(other["signing_key"]))
    with owner_ws, other_ws:
        # ── Assert: the owner finds the phrase. ──
        found = wait_hits(owner_ws, phrase, "contact")
        assert len(found) == 1, (
            f"the owner's search must find the saved card by a note phrase; "
            f"contact hits: {found!r}"
        )
        assert found[0]["content_id"] == want_id, (
            f"a card hit's content id must be the hex uid_hash ({want_id}): {found[0]!r}")
        assert card_view.is_dir() and calendar_view.is_dir(), (
            f"each granted kind keeps its own view: {card_view}, {calendar_view}")

        # ── Assert: nobody else's search does. ──
        assert hits(other_ws, phrase, "contact") == [], (
            "another member's search must never reach the owner's materialized cards")

        # ── Act + assert: turning it off deletes the card copy alone. ──
        owner_ws.call("fauna.capabilities.revoke", {"grant_id": card_grant})
        assert not card_view.exists(), (
            f"revoke must delete the card view before it replies: {card_view}")
        assert calendar_view.is_dir(), (
            f"the calendar view rides its own grant and must stand: {calendar_view}")
        assert hits(owner_ws, phrase, "contact") == [], (
            "after revoke the owner's search must no longer find the card")

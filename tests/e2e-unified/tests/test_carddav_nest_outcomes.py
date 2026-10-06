"""tier_3 — the nest's own CardDAV outcomes, witnessed from a contacts app's seat.

Every test here drives a real `fauna-nest` binary and a real mail-bridge MDA over
the CardDAV wire with a scripted client (`helpers/carddav_client.py`, raw RFC
6352/6578) — the shape a stock contacts app speaks. No Fauna app participates,
so each is a `[nest]` witness (`docs/goal/architecture/feature-catalog.md` § The
two surfaces) for `docs/features/contacts-in-standard-apps.md`.

Goal doc: `docs/goal/behavior/carddav-server.md` — each test names its section.
Users are minted per test through the `dav_user` fixture (conftest), so no test
shares an address book or a lockout bucket with another.
"""
from __future__ import annotations

import json
import secrets
import urllib.error
import urllib.request

import pytest

from helpers import budgets

from helpers.carddav_client import CardDAVClient, CardDAVError, build_vcard

pytestmark = pytest.mark.tier_3


def _carddav(handle, username: str, password: str) -> CardDAVClient:
    # CardDAV rides the same DAV listener/port as CalDAV — there is no separate
    # CardDAV port.
    client = CardDAVClient(
        f"https://127.0.0.1:{handle.caldav_port}", username, password, verify=False,
    )
    client.wait_until_serving()
    return client


def _vcf_put(client: CardDAVClient, book: str, slug: str, body: str, **headers):
    return client.raw("PUT", f"{book}{slug}.vcf",
                      headers={"Content-Type": "text/vcard", **headers}, data=body)


# ── contacts-in-standard-apps 8 ──────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_a_contacts_app_away_too_long_is_sent_to_a_full_resync(
    mail_bridge_mda, nest_instance, dav_user,
):
    """`carddav-server.md` § Address-book collection model — sync model:
    "stale-token-past-retention returns `DAV:valid-sync-token`". A contacts app
    whose token predates the tombstone-retention window is refused rather than
    handed a delta that silently omits deletions, and the full re-sync it is
    sent to is complete. (The after-restore stale token has its own witness,
    `test_dav_content_at_rest_e2e.py::test_card_restore_tells_the_contacts_app_to_reconverge`.)

    Retention is days long (7-day floor), so the test-hooks nest ages the
    user's tombstones instead of waiting; the retention check is the real
    handler's.
    """
    handle = mail_bridge_mda
    user, pw, actor = dav_user("card-stale")
    app = _carddav(handle, user, pw)
    book = app.contacts_addressbook()
    kept, gone = f"kept-{secrets.token_hex(4)}", f"gone-{secrets.token_hex(4)}"
    app.put_card(book, gone, build_vcard(gone, "Deleted While Away"))
    _changed, _removed, token = app.sync(book)
    assert token, "sanity: the first sync must mint a token"

    app.put_card(book, kept, build_vcard(kept, "Still Here"))
    app.delete_card(book, gone)
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/test/content/age_dav_tombstones",
        data=json.dumps({"actor_id": actor["actor_id_hex"], "kind": "card",
                         "days": 400}).encode(),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    with urllib.request.urlopen(req, timeout=budgets.RPC_ROUNDTRIP_S) as resp:
        assert json.loads(resp.read())["aged"] >= 1, "sanity: the deletion left a tombstone"

    with pytest.raises(CardDAVError) as stale:
        app.sync(book, token)
    assert "valid-sync-token" in str(stale.value), (
        f"a token past retention must be refused with DAV:valid-sync-token; got {stale.value}"
    )
    # The full re-sync may also list the deletion (harmless on a full sync);
    # what it must hold is the whole book as it now stands.
    changed, _removed, fresh = app.sync(book)
    assert fresh, "the full re-sync must mint a usable token"
    assert sorted(c.uid for c in changed) == [kept], (
        f"the full re-sync must hold exactly the book as it is: {[c.uid for c in changed]}"
    )


# ── contacts-in-standard-apps 9 ──────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_editing_or_deleting_from_an_out_of_date_copy_is_refused(mail_bridge_mda, dav_user):
    """`carddav-server.md` § Address-book collection model — write surface:
    "Conditional card PUT/DELETE honor `If-Match`/`If-None-Match` ETags". Two
    contacts apps hold one version of a card; the first saves; the second's
    save and delete, made against the version it holds, are refused 412 and the
    first app's edit stands. Creating a card that already exists with
    `If-None-Match: *` is refused the same way.
    """
    handle = mail_bridge_mda
    user, pw, _ = dav_user("card-412")
    app_a = _carddav(handle, user, pw)
    app_b = _carddav(handle, user, pw)
    book = app_a.contacts_addressbook()
    uid = f"card-412-{secrets.token_hex(6)}"
    app_a.put_card(book, uid, build_vcard(uid, "Original Name"))
    (stub,) = app_a.list_card_hrefs(book)
    loaded = stub.etag
    assert loaded, "sanity: a stored card must carry an ETag"

    first = app_a.raw("PUT", stub.href, data=build_vcard(uid, "Edited In A"),
                      headers={"If-Match": loaded, "Content-Type": "text/vcard"})
    assert first.status_code in (200, 201, 204), f"the first save must land: {first.status_code}"

    second = app_b.raw("PUT", stub.href, data=build_vcard(uid, "Edited In B"),
                       headers={"If-Match": loaded, "Content-Type": "text/vcard"})
    assert second.status_code == 412, (
        f"a save against an out-of-date copy must be refused 412; got {second.status_code}"
    )
    stale_delete = app_b.raw("DELETE", stub.href, headers={"If-Match": loaded})
    assert stale_delete.status_code == 412, (
        f"a delete against an out-of-date copy must be refused 412; got {stale_delete.status_code}"
    )
    create_over = app_b.raw("PUT", stub.href, data=build_vcard(uid, "Clobber"),
                            headers={"If-None-Match": "*", "Content-Type": "text/vcard"})
    assert create_over.status_code == 412, (
        f"If-None-Match: * on an existing card must be refused 412; got {create_over.status_code}"
    )
    assert app_a.fns(book) == ["Edited In A"], (
        f"every refused write must leave the first app's edit in place: {app_a.fns(book)}"
    )


# ── contacts-in-standard-apps 10 ─────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_both_vcard_formats_are_accepted_and_a_malformed_card_is_refused(
    mail_bridge_mda, dav_user,
):
    """`carddav-server.md` § Address-book collection model — write surface:
    "vCard 4.0 canonical, 3.0 accepted for MUA compat; reject malformed". A 4.0
    card and a 3.0 card both land and read back; a card missing its required
    name, and a body that is not a vCard at all, are refused 400 and never
    stored.
    """
    handle = mail_bridge_mda
    user, pw, _ = dav_user("card-400")
    app = _carddav(handle, user, pw)
    book = app.contacts_addressbook()
    v4, v3 = f"v4-{secrets.token_hex(4)}", f"v3-{secrets.token_hex(4)}"
    app.put_card(book, v4, build_vcard(v4, "Four Point Oh", email="v4@example.com"))
    app.put_card(book, v3, build_vcard(v3, "Three Point Oh", email="v3@example.com",
                                       version="3.0"))
    assert "Three Point Oh" in (app.get_card(book, v3) or ""), "the vCard 3.0 card did not read back"
    assert "Four Point Oh" in (app.get_card(book, v4) or ""), "the vCard 4.0 card did not read back"

    nameless = build_vcard("nameless-1", "x").replace("FN:x\r\n", "")
    assert _vcf_put(app, book, "nameless-1", nameless).status_code == 400, (
        "a card missing its required FN must be refused 400"
    )
    assert _vcf_put(app, book, "garbage-1", "this is not a vCard\r\n").status_code == 400, (
        "a body that is not a vCard must be refused 400"
    )
    assert sorted(app.uids(book)) == sorted([v4, v3]), (
        f"nothing refused may reach the address book: {app.uids(book)}"
    )


# ── contacts-in-standard-apps 11 + 12 ────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_a_contacts_app_creates_another_address_book_beside_the_default(
    mail_bridge_mda, dav_user,
):
    """`carddav-server.md` § Address-book collection model: "Additional books
    via `MKCOL` / extended-MKCOL." A second book is created, listed beside the
    default, and holds its own cards apart from it."""
    handle = mail_bridge_mda
    user, pw, _ = dav_user("card-mkcol")
    app = _carddav(handle, user, pw)
    default = app.contacts_addressbook()
    work = app.mkcol(f"work-{secrets.token_hex(4)}", displayname="Work")
    # The listing names books by their canonical id, not the slug the app
    # chose; the new one is the listed book that is not the default.
    listed = app.list_addressbooks()
    assert default in listed and len(listed) == 2, f"both books must be listed: {listed}"
    uid = f"work-card-{secrets.token_hex(4)}"
    app.put_card(work, uid, build_vcard(uid, "Work Contact"))
    assert app.uids(work) == [uid] and uid not in app.uids(default), (
        "a card stored in the new book must live there and only there"
    )


@pytest.mark.feature("contacts-in-standard-apps")
def test_renaming_an_address_book_and_its_description_is_kept(mail_bridge_mda, dav_user):
    """`carddav-server.md` § Address-book collection model — collection-level
    PROPPATCH persists `displayname` and `addressbook-description`, answering
    207. A second connection (another app) reads the new name and description."""
    handle = mail_bridge_mda
    user, pw, _ = dav_user("card-proppatch")
    app = _carddav(handle, user, pw)
    book = app.contacts_addressbook()
    name, desc = f"Friends {secrets.token_hex(3)}", "People I actually call"
    resp = app.raw("PROPPATCH", book, data=(
        '<?xml version="1.0" encoding="utf-8"?>'
        '<d:propertyupdate xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">'
        f"<d:set><d:prop><d:displayname>{name}</d:displayname>"
        f"<c:addressbook-description>{desc}</c:addressbook-description>"
        "</d:prop></d:set></d:propertyupdate>"
    ))
    assert resp.status_code == 207, f"a collection PROPPATCH must answer 207; got {resp.status_code}"
    assert "403" not in resp.text, f"a recognised property was refused:\n{resp.text}"

    other_app = _carddav(handle, user, pw)
    assert other_app.collection_props(book) == {"displayname": name, "description": desc}, (
        "the renamed book must read back with its new name and description"
    )


# ── contacts-in-standard-apps 13 ─────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_a_contacts_app_searches_the_address_book_by_field(mail_bridge_mda, dav_user):
    """`carddav-server.md` § Address-book collection model — read surface:
    "addressbook-query REPORT (property + text-match filters)". Searching by
    name, by email and by phone returns exactly the matching cards."""
    handle = mail_bridge_mda
    user, pw, _ = dav_user("card-query")
    app = _carddav(handle, user, pw)
    book = app.contacts_addressbook()
    ada, grace = f"ada-{secrets.token_hex(4)}", f"grace-{secrets.token_hex(4)}"
    app.put_card(book, ada, build_vcard(ada, "Ada Lovelace", email="ada@engines.example",
                                        tel="+44 20 7946 0001"))
    app.put_card(book, grace, build_vcard(grace, "Grace Hopper", email="grace@navy.example",
                                          tel="+1 202 555 0199"))

    for prop, text, want in (("FN", "lovelace", ada), ("EMAIL", "navy.example", grace),
                             ("TEL", "555 0199", grace)):
        got = [c.uid for c in app.query(book, prop, text)]
        assert got == [want], f"searching {prop} for {text!r} must find exactly {want}; got {got}"
    assert app.query(book, "FN", "turing") == [], "a search nothing matches must return nothing"


# ── contacts-in-standard-apps 14 ─────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_given_only_an_email_address_a_contacts_app_finds_the_address_book(
    mail_bridge_mda, nest_instance,
):
    """`dns-management.md` § Records covered + `carddav-server.md` § Network
    exposure & discovery: starting from nothing but `user@<domain>`, every hop
    of automatic setup leads to the address book —

      1. the nest publishes `_carddavs._tcp.<domain> SRV 0 1 443 mail.<domain>.`
         (the record macOS Contacts' automatic setup queries);
      2. the apex `https://<domain>/.well-known/carddav` sends the app on to the
         mail host's own well-known (RFC 6764);
      3. the mail host's well-known, once signed in, sends it on to the book,
         where the default address book is served.

    DNS itself is not resolved here (tier_3 has no resolver); hop 1 witnesses
    the record the nest publishes, and hops 2–3 follow the redirects it serves.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mda
    domain = handle.domain
    admin = nest_instance["admin"]
    with WsRpcAdminClient(nest_instance["url"], actor_id=bytes(admin["signing_key"].verify_key),
                          signing_key=bytes(admin["signing_key"])) as admin_ws:
        domains = admin_ws.call("fauna.dns.list_records", {"domain": domain})["domains"]
    records = [r for d in domains for r in d["records"]]
    srv = [r for r in records if r["record_type"] == "SRV"
           and r["name"].rstrip(".") == f"_carddavs._tcp.{domain}"]
    assert srv, f"no _carddavs SRV record is published for {domain}: {records}"
    assert srv[0]["expected"].split() == ["0", "1", "443", f"mail.{domain}."], srv[0]

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None

    opener = urllib.request.build_opener(NoRedirect)
    try:
        opener.open(f"{nest_instance['url']}/.well-known/carddav", timeout=budgets.RPC_ROUNDTRIP_S)
        pytest.fail("the apex well-known must redirect, not serve")
    except urllib.error.HTTPError as hop:
        assert hop.code == 301, f"the apex must 301; got {hop.code}"
        location = hop.headers["Location"]
    assert location == f"https://mail.{domain}/.well-known/carddav", (
        f"the apex must send the app to the mail host's well-known; got {location}"
    )

    # Hop 3 at the mail host (loopback stands in for mail.<domain>): signed in,
    # the app asks its well-known who it is (RFC 6764 lets the context path be
    # served or redirected — requests follows a redirect either way), and the
    # walk from there reaches the default book.
    app = _carddav(handle, handle.recipient_username, handle.recipient_password)
    book = app.discover_addressbook("/.well-known/carddav")
    assert book.rstrip("/") == app.contacts_addressbook().rstrip("/"), (
        f"discovery from the well-known reached {book}, not the user's address book"
    )


# ── contacts-in-standard-apps 15 ─────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_a_contacts_app_signing_in_with_just_the_username_is_let_in(
    mail_bridge_mda, dav_user,
):
    """`carddav-server.md` § Network exposure & discovery — Interop: "the same
    bare-username default-to-`PrimaryDomain` … quirks the CalDAV surface already
    solved apply identically". A contacts app that sends `alice` rather than
    `alice@<domain>` signs in, discovers its book and writes a card."""
    handle = mail_bridge_mda
    user, pw, _ = dav_user("card-bare")
    bare = user.split("@", 1)[0]
    app = CardDAVClient(f"https://127.0.0.1:{handle.caldav_port}", bare, pw, verify=False)
    app.wait_until_serving()
    book = app.discover_addressbook("/")
    uid = f"bare-{secrets.token_hex(4)}"
    app.put_card(book, uid, build_vcard(uid, "Bare Sign-in"))
    assert "Bare Sign-in" in (app.get_card(book, uid) or ""), (
        "a contacts app signed in by bare username must read back its card"
    )
    full = _carddav(handle, user, pw)
    assert uid in full.uids(full.contacts_addressbook()), (
        "the bare-username sign-in must reach the same address book as the full address"
    )


# ── contacts-in-standard-apps 16 ─────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_without_the_password_nothing_is_served_and_repeated_guessing_is_braked(
    mail_bridge_mda, dav_user,
):
    """`carddav-server.md` § Process topology & attach pattern: AUTH, lockout
    brake identical to CalDAV. A wrong password and an unknown user get the
    same 401 (no hint that the account exists) and no contact data; after a run
    of wrong passwords the brake refuses even the right one for that account,
    while another account from the same address is unaffected.
    """
    handle = mail_bridge_mda
    user, pw, _ = dav_user("card-lock")
    bystander_user, bystander_pw, _ = dav_user("card-bystander")
    base = f"https://127.0.0.1:{handle.caldav_port}"
    good = _carddav(handle, user, pw)
    book = good.contacts_addressbook()
    uid = f"secret-{secrets.token_hex(4)}"
    good.put_card(book, uid, build_vcard(uid, "Hidden Person"))

    def attempt(username: str, password: str):
        client = CardDAVClient(base, username, password, verify=False)
        return client.raw("PROPFIND", book, headers={"Depth": "1"})

    wrong = attempt(user, "not-the-password")
    unknown = attempt(f"nobody-{secrets.token_hex(4)}@{handle.domain}", "not-the-password")
    assert wrong.status_code == unknown.status_code == 401, (wrong.status_code, unknown.status_code)
    assert wrong.text == unknown.text, (
        "a wrong password and an unknown user must be indistinguishable:\n"
        f"{wrong.text!r}\nvs\n{unknown.text!r}"
    )
    assert wrong.headers.get("WWW-Authenticate") == unknown.headers.get("WWW-Authenticate")
    assert "Hidden Person" not in wrong.text

    # A run of wrong passwords past the per-minute threshold (the deployment
    # auth policy's `max_auth_failures_per_minute`, 30 by default) — with no
    # correct attempt in between, since a success resets the brake — and then
    # even the right password is refused for this account.
    for _ in range(45):
        assert attempt(user, "still-not-the-password").status_code == 401
    braked = attempt(user, pw)
    assert braked.status_code == 401, (
        f"after 45 wrong passwords the right one must be braked; got {braked.status_code}"
    )
    assert "Hidden Person" not in braked.text

    assert _carddav(handle, bystander_user, bystander_pw).raw(
        "PROPFIND", _carddav(handle, bystander_user, bystander_pw).contacts_addressbook(),
        headers={"Depth": "0"},
    ).status_code == 207, "the brake must hold one account, not every account from the address"


# ── contacts-in-standard-apps 20 ─────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_another_user_can_never_reach_your_address_book_even_with_the_same_name(
    mail_bridge_mda, dav_user,
):
    """`carddav-server.md` § Architectural rules: "No caller reaches another
    actor's address books", where the lazy default book's id is the same fleet
    constant for every user. Two users' default books share that id; each sees
    only its own cards, and the other user's app can neither read nor write the
    owner's book at its exact paths.
    """
    handle = mail_bridge_mda
    owner_user, owner_pw, _ = dav_user("card-owner")
    other_user, other_pw, _ = dav_user("card-other")
    mine = _carddav(handle, owner_user, owner_pw)
    theirs = _carddav(handle, other_user, other_pw)
    my_book, their_book = mine.contacts_addressbook(), theirs.contacts_addressbook()
    assert my_book.rstrip("/").rsplit("/", 1)[1] == their_book.rstrip("/").rsplit("/", 1)[1], (
        "sanity: both default books carry the same shared id — the case the rule is about"
    )
    uid = f"private-card-{secrets.token_hex(4)}"
    mine.put_card(my_book, uid, build_vcard(uid, "Private Person"))
    (card,) = mine.list_card_hrefs(my_book)

    assert uid not in theirs.uids(their_book), "the owner's card appeared in the other user's book"
    for method, url, body in (
        ("PROPFIND", my_book, None), ("GET", card.href, None),
        ("PUT", card.href, build_vcard(uid, "Overwritten")), ("DELETE", card.href, None),
    ):
        headers = {"Depth": "1"} if method == "PROPFIND" else {"Content-Type": "text/vcard"}
        resp = theirs.raw(method, url, headers=headers, data=body)
        assert resp.status_code in (403, 404), (
            f"another user's {method} of the owner's book must be refused; got "
            f"{resp.status_code}: {resp.text[:300]}"
        )
        assert "Private Person" not in resp.text, f"{method} leaked the owner's card"
    assert mine.fns(my_book) == ["Private Person"], "the owner's card must be untouched"


# ── contacts-in-standard-apps 17 + 18 ────────────────────────────────────────


def _apex_carddav(nest) -> tuple[int, str | None]:
    """GET the nest's apex `/.well-known/carddav`, no redirect-following."""

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None

    try:
        resp = urllib.request.build_opener(NoRedirect).open(
            f"{nest['url']}/.well-known/carddav", timeout=budgets.RPC_ROUNDTRIP_S)
        return resp.status, resp.headers.get("Location")
    except urllib.error.HTTPError as err:
        return err.code, err.headers.get("Location")


@pytest.mark.feature("contacts-in-standard-apps")
def test_contacts_keep_working_with_mail_turned_off(dav_toggle_venue):
    """`carddav-server.md` § Independent enablement — `carddav_enabled`:
    CardDAV "gates separately from email". On a nest with mail off and contacts
    on, a contacts app is directed to the book and reads and writes cards."""
    venue = dav_toggle_venue(mail=False, carddav=True)
    status, location = _apex_carddav(venue.nest_instance)
    assert status == 301 and location, (
        f"with contacts on, the apex must direct a contacts app on even with mail "
        f"off; got {status} → {location}"
    )
    app = _carddav(venue, venue.recipient_username, venue.recipient_password)
    book = app.contacts_addressbook()
    uid = f"no-mail-{secrets.token_hex(4)}"
    app.put_card(book, uid, build_vcard(uid, "Mail Is Off"))
    assert "Mail Is Off" in (app.get_card(book, uid) or ""), (
        "a contacts-only deployment must store and serve cards"
    )


@pytest.mark.feature("contacts-in-standard-apps")
def test_switching_contacts_off_closes_the_door_and_the_signpost(dav_toggle_venue):
    """`carddav-server.md` § Independent enablement + § Network exposure &
    discovery: with contacts explicitly off, the apex `/.well-known/carddav`
    answers 503 and no contacts app can reach an address book — while calendars
    on the same listener keep serving; with every DAV surface off, the DAV
    listener is not open at all."""
    import socket

    venue = dav_toggle_venue(mail=True, carddav=False)
    status, location = _apex_carddav(venue.nest_instance)
    assert status == 503, f"contacts explicitly off must 503 at the apex; got {status} → {location}"

    app = CardDAVClient(f"https://127.0.0.1:{venue.caldav_port}", venue.recipient_username,
                        venue.recipient_password, verify=False)
    app.wait_until_serving()
    resp = app.raw("PROPFIND", app.home, headers={"Depth": "1"})
    assert resp.status_code != 207 or "addressbook" not in resp.text, (
        f"with contacts off, the address-book home must not be served; got "
        f"{resp.status_code}:\n{resp.text[:600]}"
    )
    principal = app.raw("PROPFIND", f"/{venue.recipient_username}/", headers={"Depth": "0"},
                        data='<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">'
                             "<d:prop><c:addressbook-home-set/></d:prop></d:propfind>")
    assert "/carddav/" not in principal.text, (
        f"with contacts off, the principal must not advertise an address book:\n{principal.text[:600]}"
    )

    # Every surface the shared DAV listener serves switched off (an unset one
    # would follow mail and keep the listener up).
    closed = dav_toggle_venue(mail=True, caldav=False, carddav=False, webdav=False)
    with pytest.raises(OSError):
        socket.create_connection(("127.0.0.1", closed.caldav_port), timeout=budgets.RPC_ROUNDTRIP_S).close()


# ── contacts-in-standard-apps 19 ─────────────────────────────────────────────


@pytest.mark.feature("contacts-in-standard-apps")
def test_on_a_home_nest_with_no_domain_contacts_are_reached_by_the_bare_address(
    dav_toggle_venue,
):
    """`carddav-server.md` § Independent enablement: CardDAV "works when
    explicitly enabled on a domainless / bare-IP box via the floor cert + handle
    login". On a home nest with no domain, a contacts app pointed at the bare
    `https://<address>:<port>` and signing in with the handle discovers the
    address book and reads and writes cards."""
    venue = dav_toggle_venue(mail=False, carddav=True, domainless=True)
    app = CardDAVClient(f"https://127.0.0.1:{venue.caldav_port}", venue.recipient_username,
                        venue.recipient_password, verify=False)
    app.wait_until_serving()
    book = app.discover_addressbook("/")
    uid = f"home-{secrets.token_hex(4)}"
    app.put_card(book, uid, build_vcard(uid, "Home Nest Contact"))
    assert "Home Nest Contact" in (app.get_card(book, uid) or ""), (
        "a contacts app on a domainless home nest must store and read back its card"
    )

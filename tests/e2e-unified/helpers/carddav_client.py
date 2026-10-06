"""Minimal, dependency-light CardDAV client for e2e tests.

Speaks the real CardDAV wire (RFC 6352 / RFC 6578 / RFC 4918) over HTTPS with
HTTP Basic Auth — the exact surface a macOS Contacts.app / DAVx5 / Thunderbird
address-book client uses against the Fauna mail-bridge MDA role. It is the
STRUCTURAL TWIN of `helpers/caldav_client.CalDAVClient` (same MDA process, same
AUTH, same emersion depth-routing, same sync-token shape — different protocol:
RFC 6352 CardDAV vs RFC 4791 CalDAV). Built on `requests` (already in the venv)
rather than the `carddav`/`vobject` PyPI libs so the tests carry no new
dependency and we control every byte on the wire (mirrors `caldav_client.py`).

Scope: just enough for the vCard round-trip matrix —
  - discover address books under /carddav/{user}/ (PROPFIND Depth:1) — the lazy
    "Contacts" book is auto-provisioned on first PROPFIND;
  - create a book via extended MKCOL;
  - PUT / GET / DELETE a vCard by UID (addressing the server-canonical href, the
    way a real client does — the MDA rewrites Location to blake3(UID)[:32].vcf);
  - enumerate cards via PROPFIND Depth:1 + addressbook-multiget REPORT (NOT an
    empty-filter addressbook-query — emersion returns zero for that, per
    carddav-server.md follow-up B), and via sync-collection REPORT (RFC 6578).

It is intentionally NOT a general CardDAV library; extend as the tests need.

Authority for the wire contract: docs/goal/behavior/carddav-server.md.
"""

from __future__ import annotations

import hashlib
import re
import ssl
import time
import xml.etree.ElementTree as ET
from dataclasses import dataclass

import requests
from requests.auth import HTTPBasicAuth

# XML namespaces used on the CardDAV wire.
_DAV = "DAV:"
_CARDDAV = "urn:ietf:params:xml:ns:carddav"
_NS = {"d": _DAV, "c": _CARDDAV}


def _client_put_slug(uid: str) -> str:
    """The *client-chosen* PUT filename — NOT the server's canonical resource
    slug. A real client (e.g. macOS Contacts.app) PUTs at whatever filename it
    likes; the MDA ignores it, parses the UID from the body, and rewrites
    Location to the canonical ``blake3(UID)[:32].vcf`` (carddav put.go step 3).
    So GET/DELETE must address the server-returned href (see ``_href_for_uid``),
    NEVER reconstruct a slug client-side. We only need a stable, unique, URL-safe
    PUT filename here; sha256-hex (truncated, deterministic per UID) is plenty
    and avoids a blake3 dependency in the test harness (twin of
    caldav_client._client_put_slug)."""
    return hashlib.sha256(uid.encode()).hexdigest()[:32]


@dataclass
class VCardResource:
    href: str
    etag: str | None
    uid: str | None
    fn: str | None
    vcf: str | None


class CardDAVError(RuntimeError):
    def __init__(self, msg: str, resp: requests.Response | None = None):
        if resp is not None:
            msg = f"{msg}: HTTP {resp.status_code} {resp.reason}\n{resp.text[:2000]}"
        super().__init__(msg)
        self.resp = resp


class CardDAVClient:
    """A single CardDAV session (one user, one credential).

    base_url is the CardDAV root, e.g. https://127.0.0.1:<caldav_port> (the client
    appends /carddav/{user}/). username is the full handle or its RFC-5233
    +credential form (test+mua@<domain>); password is the mail credential password
    (AEAD-unwrap-as-auth, shared with IMAP/CalDAV). Twin of CalDAVClient.
    """

    def __init__(
        self,
        base_url: str,
        username: str,
        password: str,
        *,
        verify: bool | str = True,
        timeout: float = 30.0,
    ):
        self.base = base_url.rstrip("/")
        self.username = username
        self.password = password
        self.timeout = timeout
        self.session = requests.Session()
        self.session.auth = HTTPBasicAuth(username, password)
        self.session.verify = verify
        # The home-set path is keyed on the BASE mailbox — the +suffix-stripped
        # local part. When the AUTH username carries an RFC-5233 `+credential`
        # suffix (`<handle>+<credential_id>@<domain>`), the URL PATH must still use
        # the bare `<handle>@<domain>` — exactly what a real client does after
        # principal/home-set discovery (carddav backend.go userBasePath uses
        # sess.AuthedLocalPart()/AuthedDomain() — the base, +suffix-stripped). A
        # `+` left in the path makes the MDA's home-set guard miss. The auth
        # middleware runs before path routing, so the suffixed username still
        # selects the right credential blob. Mirrors CalDAVClient.__init__.
        local, sep, dom = username.partition("@")
        base_local = local.split("+", 1)[0]
        path_user = f"{base_local}@{dom}" if sep else base_local
        self.home = f"{self.base}/carddav/{path_user}/"

    # ── low-level request ────────────────────────────────────────────────
    def _req(self, method: str, url: str, *, headers=None, data=None) -> requests.Response:
        h = {"Content-Type": "application/xml; charset=utf-8"}
        if headers:
            h.update(headers)
        return self.session.request(
            method, url, headers=h, data=data, timeout=self.timeout
        )

    def _abs(self, href: str) -> str:
        if href.startswith("http://") or href.startswith("https://"):
            return href
        return f"{self.base}{href}" if href.startswith("/") else f"{self.base}/{href}"

    def wait_until_serving(self, *, timeout: float = 120.0) -> None:
        """Poll until the MDA's shared :443 DAV listener answers on the /carddav/
        path (any HTTP status — even a 401 challenge counts as 'serving'). A
        connection-level error means it is still cold-booting (re-enroll +
        re-attest + bind), so a fresh client must wait before its first real
        request. Raises CardDAVError if it never serves. Twin of
        CalDAVClient.wait_until_serving."""
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            try:
                self.session.request("PROPFIND", self.home, timeout=6.0)
                return
            except requests.exceptions.RequestException as e:
                last = type(e).__name__
                time.sleep(2.0)
        raise CardDAVError(
            f"CardDAV endpoint {self.base} never served within {timeout:.0f}s ({last})"
        )

    # ── discovery ────────────────────────────────────────────────────────
    def list_addressbooks(self, home: str | None = None) -> list[str]:
        """PROPFIND Depth:1 on the home set → address-book collection hrefs.

        The MDA lazily provisions the "Contacts" book on the first PROPFIND that
        finds an empty home set (carddav backend.go ListAddressBooks →
        lazyProvisionContacts), so a fresh actor's first call returns exactly one
        book. Returns absolute hrefs (the home set itself filtered out). ``home``
        overrides the default ``self.home`` — used by the autodiscovery walk to
        enumerate a home set it discovered rather than derived.
        """
        target = home if home is not None else self.home
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">'
            "<d:prop><d:resourcetype/><d:displayname/></d:prop></d:propfind>"
        )
        resp = self._req("PROPFIND", target, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CardDAVError("PROPFIND home set failed", resp)
        books: list[str] = []
        root = ET.fromstring(resp.content)
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            if href_el is None or not href_el.text:
                continue
            href = href_el.text
            # An address-book collection has resourcetype containing
            # <c:addressbook/> (RFC 6352 §5.2).
            rtype = r.find(".//d:resourcetype", _NS)
            is_book = rtype is not None and rtype.find("c:addressbook", _NS) is not None
            if is_book:
                books.append(self._abs(href))
        return books

    def contacts_addressbook(self) -> str:
        """The lazy 'Contacts' address book auto-created on first PROPFIND (per
        carddav-server.md § Address-book collection model). Returns its href;
        raises if no book is discoverable."""
        books = self.list_addressbooks()
        if not books:
            raise CardDAVError(
                f"no address books discovered under {self.home} — expected the lazy "
                "Contacts book to be created on first PROPFIND"
            )
        return books[0]

    # ── RFC-6764 discovery walk ───────────────────────────────────────────
    def current_user_principal(self, start_path: str = "/") -> str:
        """PROPFIND Depth:0 ``<d:current-user-principal/>`` at ``start_path`` → the
        AUTH'd actor's principal href (the single-segment `/{user@domain}/`). This
        is hop 1 of RFC-6764 host-only autodiscovery: a client pointed at just the
        server root (no collection path) asks "who am I?". When both DAV protocols
        are on, `/` is served by the CalDAV chain, whose unified principal is the
        one CardDAV also uses (carddav-server.md § Process topology — unified root
        principal). Raises CardDAVError if no principal href is returned."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:"><d:prop>'
            "<d:current-user-principal/></d:prop></d:propfind>"
        )
        url = self._abs(start_path if start_path.startswith("/") else "/" + start_path)
        resp = self._req("PROPFIND", url, headers={"Depth": "0"}, data=body)
        if resp.status_code not in (207, 200):
            raise CardDAVError("PROPFIND current-user-principal failed", resp)
        root = ET.fromstring(resp.content)
        href_el = root.find(".//d:current-user-principal/d:href", _NS)
        if href_el is None or not href_el.text:
            raise CardDAVError(
                f"no current-user-principal href in PROPFIND of {url}: "
                f"{resp.text[:800]}"
            )
        return self._abs(href_el.text)

    def addressbook_home_set(self, principal_href: str) -> str:
        """PROPFIND Depth:0 ``<c:addressbook-home-set/>`` on the principal → the
        address-book home-set href (`/carddav/{user@domain}/`). Hop 2 of the
        RFC-6764 walk: the unified principal advertises addressbook-home-set
        alongside calendar-home-set (carddav-server.md § Implementation status —
        unified root principal, Track B). Raises CardDAVError if absent (which is
        exactly the discovery-stall a host-only client hits when the principal
        omits it)."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">'
            "<d:prop><c:addressbook-home-set/></d:prop></d:propfind>"
        )
        resp = self._req(
            "PROPFIND", self._abs(principal_href), headers={"Depth": "0"}, data=body
        )
        if resp.status_code not in (207, 200):
            raise CardDAVError("PROPFIND addressbook-home-set failed", resp)
        root = ET.fromstring(resp.content)
        href_el = root.find(".//c:addressbook-home-set/d:href", _NS)
        if href_el is None or not href_el.text:
            raise CardDAVError(
                f"no addressbook-home-set href on principal {principal_href}: "
                f"{resp.text[:800]}"
            )
        return self._abs(href_el.text)

    def discover_addressbook(self, start_path: str = "/") -> str:
        """The full RFC-6764 host-only autodiscovery walk: root →
        current-user-principal → addressbook-home-set → the (lazy Contacts) book
        href. Returns the discovered book href, proving a client that knows only
        the server host reaches the address book without a hard-coded collection
        path."""
        principal = self.current_user_principal(start_path)
        home = self.addressbook_home_set(principal)
        # PROPFIND Depth:1 the DISCOVERED home-set (not the derived self.home) —
        # this both walks hop 3 and lazily provisions the Contacts book if empty.
        books = self.list_addressbooks(self._abs(home))
        if not books:
            raise CardDAVError(
                f"autodiscovery reached the home set {home} but it exposed no "
                "address book (expected the lazy Contacts book)"
            )
        return books[0]

    # ── collection create / metadata ─────────────────────────────────────
    def mkcol(self, segment: str, *, displayname: str | None = None) -> str:
        """Create an address-book collection at the *client-chosen* ``segment`` via
        extended MKCOL (RFC 5689) carrying an addressbook resourcetype — the shape
        emersion/go-webdav routes to ``CreateAddressBook`` (there is no dedicated
        MKADDRESSBOOK verb; carddav backend.go CreateAddressBook). Returns the
        collection href the client keeps using for every later PROPFIND/PUT/REPORT
        (the MDA's ``resolveAddressbookSegment`` maps a non-hex slug to the
        canonical ``blake3(slug)[:32]`` id). Raises ``CardDAVError`` on any non-201
        — e.g. a 405 on an already-provisioned collection (RFC 4918 §9.3), whose
        ``.resp.status_code`` the caller can assert.
        """
        col_href = f"{self.home}{segment.strip('/')}/"
        name = displayname if displayname is not None else segment
        data = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:mkcol xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">'
            "<d:set><d:prop>"
            "<d:resourcetype><d:collection/><c:addressbook/></d:resourcetype>"
            f"<d:displayname>{name}</d:displayname>"
            "</d:prop></d:set></d:mkcol>"
        ).encode()
        resp = self._req("MKCOL", col_href, data=data)
        if resp.status_code != 201:
            raise CardDAVError(f"MKCOL {segment} failed", resp)
        return col_href

    # ── card CRUD ────────────────────────────────────────────────────────
    def _href_for_uid(self, book_href: str, uid: str) -> str | None:
        """Resolve the server-canonical resource href for ``uid`` via a PROPFIND
        Depth:1 enumeration. Mirrors a real client: the MDA assigns the resource
        path (canonical ``blake3(UID)[:32].vcf``, ignoring the client's PUT
        filename — put.go step 3), so GET/DELETE must address the href the server
        actually returned, not a client-reconstructed slug. Returns None if no
        card with that UID is currently in the book."""
        for card in self.list_cards(book_href):
            if card.uid == uid:
                return card.href
        return None

    def put_card(self, book_href: str, uid: str, vcf: str, *, if_match: str | None = None) -> str:
        """PUT a vCard object. Returns the new ETag (from the response header, or
        "" if the server omits it — the caller can re-fetch via a REPORT). The
        client filename is a throwaway slug; the MDA rewrites Location to the
        canonical blake3(UID)[:32].vcf."""
        url = f"{book_href.rstrip('/')}/{_client_put_slug(uid)}.vcf"
        headers = {"Content-Type": "text/vcard; charset=utf-8"}
        if if_match:
            headers["If-Match"] = if_match
        resp = self._req("PUT", url, headers=headers, data=vcf.encode())
        if resp.status_code not in (201, 204, 200):
            raise CardDAVError(f"PUT card {uid} failed", resp)
        return resp.headers.get("ETag") or ""

    def get_card(self, book_href: str, uid: str) -> str | None:
        """GET a card's vCard body, or None if absent. Addresses the
        server-returned href (resolved by UID via PROPFIND), the way a real client
        does — the client PUT filename is not the GET path."""
        href = self._href_for_uid(book_href, uid)
        if href is None:
            return None
        resp = self.session.get(href, timeout=self.timeout)
        if resp.status_code == 404:
            return None
        if resp.status_code != 200:
            raise CardDAVError(f"GET card {uid} failed", resp)
        return resp.text

    def delete_card(self, book_href: str, uid: str, *, if_match: str | None = None) -> None:
        """DELETE a card by UID. Resolves the server-canonical href via PROPFIND
        first (a real client deletes the href it discovered). Absent already →
        no-op (DELETE is idempotent)."""
        href = self._href_for_uid(book_href, uid)
        if href is None:
            return
        headers = {}
        if if_match:
            headers["If-Match"] = if_match
        resp = self._req("DELETE", href, headers=headers)
        if resp.status_code not in (204, 200, 404):
            raise CardDAVError(f"DELETE card {uid} failed", resp)

    def list_card_hrefs(self, book_href: str) -> list[VCardResource]:
        """PROPFIND Depth:1 on an address-book collection → [VCardResource] with
        href + etag (NO body — PROPFIND enumerates the resources; use ``multiget``
        for bodies). This is the enumeration a real client does first, then
        addressbook-multigets the bodies (carddav-server.md follow-up B — NOT an
        empty-filter addressbook-query, which emersion answers with zero cards)."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:"><d:prop>'
            "<d:getetag/><d:resourcetype/></d:prop></d:propfind>"
        )
        resp = self._req("PROPFIND", book_href, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CardDAVError("PROPFIND book failed", resp)
        out: list[VCardResource] = []
        root = ET.fromstring(resp.content)
        book_path = book_href.rstrip("/") + "/"
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            if href_el is None or not href_el.text:
                continue
            href = self._abs(href_el.text)
            # Skip the collection itself; keep only card resources (.vcf).
            if not href.rstrip("/").lower().endswith(".vcf"):
                continue
            etag_el = r.find(".//d:getetag", _NS)
            out.append(
                VCardResource(
                    href=href,
                    etag=etag_el.text if etag_el is not None else None,
                    uid=None,
                    fn=None,
                    vcf=None,
                )
            )
        return out

    def multiget(self, book_href: str, hrefs: list[str]) -> list[VCardResource]:
        """addressbook-multiget REPORT (RFC 6352 §8.7) → the vCard body + etag for
        each requested href. This is how a real client fetches bodies after a
        PROPFIND/sync enumeration. Empty ``hrefs`` → [] (no request issued)."""
        if not hrefs:
            return []
        href_xml = "".join(
            f"<d:href>{self._href_path(h)}</d:href>" for h in hrefs
        )
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<c:addressbook-multiget xmlns:d="DAV:" '
            'xmlns:c="urn:ietf:params:xml:ns:carddav">'
            "<d:prop><d:getetag/><c:address-data/></d:prop>"
            f"{href_xml}</c:addressbook-multiget>"
        )
        resp = self._req("REPORT", book_href, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CardDAVError("addressbook-multiget REPORT failed", resp)
        return self._parse_card_responses(resp.content)

    def list_cards(self, book_href: str) -> list[VCardResource]:
        """The full-body enumeration a real client uses: PROPFIND Depth:1 for the
        hrefs, then addressbook-multiget for the bodies + etags (carddav-server.md
        follow-up B). Returns [VCardResource] with uid/fn parsed from each body."""
        stubs = self.list_card_hrefs(book_href)
        if not stubs:
            return []
        return self.multiget(book_href, [s.href for s in stubs])

    def uids(self, book_href: str) -> list[str]:
        return [c.uid for c in self.list_cards(book_href) if c.uid]

    def fns(self, book_href: str) -> list[str]:
        return [c.fn for c in self.list_cards(book_href) if c.fn]

    # ── sync-collection (RFC 6578) ───────────────────────────────────────
    def sync(
        self, book_href: str, sync_token: str = ""
    ) -> tuple[list[VCardResource], list[str], str]:
        """sync-collection REPORT (RFC 6578) → (changed, removed_hrefs, new_token).

        An empty ``sync_token`` (the initial sync) returns every card as a change
        with its body inline; a later call with the returned token returns only
        the delta since (changed cards + expunged hrefs as 404 tombstones). The
        MDA's sync interceptor answers ``<D:sync-token>`` at the end (carddav
        sync_collection.go). A 403 ``DAV:valid-sync-token`` (stale token) raises
        CardDAVError so the caller can fall through to a full enumeration.
        """
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:sync-collection xmlns:d="DAV:" '
            'xmlns:c="urn:ietf:params:xml:ns:carddav">'
            f"<d:sync-token>{sync_token}</d:sync-token>"
            "<d:sync-level>1</d:sync-level>"
            "<d:prop><d:getetag/><c:address-data/></d:prop>"
            "</d:sync-collection>"
        )
        resp = self._req("REPORT", book_href, headers={"Depth": "1"}, data=body)
        if resp.status_code == 403:
            raise CardDAVError("sync-collection returned DAV:valid-sync-token (stale)", resp)
        if resp.status_code not in (207, 200):
            raise CardDAVError("sync-collection REPORT failed", resp)
        root = ET.fromstring(resp.content)
        changed: list[VCardResource] = []
        removed: list[str] = []
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            href = self._abs(href_el.text) if href_el is not None and href_el.text else ""
            status_el = r.find("d:status", _NS)
            status_txt = (status_el.text or "") if status_el is not None else ""
            if "404" in status_txt:
                removed.append(href)
                continue
            data_el = r.find(".//c:address-data", _NS)
            etag_el = r.find(".//d:getetag", _NS)
            vcf = data_el.text if data_el is not None else None
            changed.append(
                VCardResource(
                    href=href,
                    etag=etag_el.text if etag_el is not None else None,
                    uid=_vcf_field(vcf, "UID"),
                    fn=_vcf_field(vcf, "FN"),
                    vcf=vcf,
                )
            )
        token_el = root.find("d:sync-token", _NS)
        new_token = token_el.text if token_el is not None and token_el.text else ""
        return changed, removed, new_token

    # ── refusal-shaped + search + collection-property requests ───────────
    def raw(self, method: str, url: str, *, headers=None, data=None) -> requests.Response:
        """One request exactly as given, with no status check — for asserting
        what the server refuses (a 400, 412, 401) rather than what it serves.
        A relative ``url`` resolves against the server. Twin of
        `CalDAVClient.raw`."""
        payload = data.encode() if isinstance(data, str) else data
        return self._req(method, self._abs(url), headers=headers, data=payload)

    def query(self, book_href: str, prop: str, text: str) -> list[VCardResource]:
        """addressbook-query REPORT (RFC 6352 §8.6) with one ``prop-filter`` +
        ``text-match`` (contains, case-insensitive) → the matching cards — the
        search a contacts app runs when you type a name or an address."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<c:addressbook-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">'
            "<d:prop><d:getetag/><c:address-data/></d:prop>"
            f'<c:filter><c:prop-filter name="{prop}">'
            f'<c:text-match collation="i;unicode-casemap" match-type="contains">{text}'
            "</c:text-match></c:prop-filter></c:filter>"
            "</c:addressbook-query>"
        )
        resp = self._req("REPORT", book_href, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CardDAVError("addressbook-query REPORT failed", resp)
        return self._parse_card_responses(resp.content)

    def collection_props(self, book_href: str) -> dict[str, str | None]:
        """PROPFIND Depth:0 → the book's ``displayname`` and
        ``addressbook-description`` as the server now reports them."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop>'
            "<d:displayname/><c:addressbook-description/></d:prop></d:propfind>"
        )
        resp = self._req("PROPFIND", book_href, headers={"Depth": "0"}, data=body)
        if resp.status_code not in (207, 200):
            raise CardDAVError("PROPFIND book props failed", resp)
        root = ET.fromstring(resp.content)
        name = root.find(".//d:displayname", _NS)
        desc = root.find(".//c:addressbook-description", _NS)
        return {
            "displayname": name.text if name is not None else None,
            "description": desc.text if desc is not None else None,
        }

    # ── collection delete ────────────────────────────────────────────────
    def delete_addressbook(self, book_href: str) -> int:
        """DELETE an address-book collection (cascade-deletes the whole book + all
        its cards, carddav backend.go DeleteAddressBook). Returns the HTTP status:
        204 on the first delete, 404 on an idempotent re-delete of the same book.
        Raises on any other status."""
        resp = self._req("DELETE", self._abs(book_href))
        if resp.status_code not in (204, 200, 404):
            raise CardDAVError("DELETE address book failed", resp)
        return resp.status_code

    # ── helpers ──────────────────────────────────────────────────────────
    def _href_path(self, href: str) -> str:
        """Reduce an absolute href to its path (multiget <d:href> elements are
        conventionally path-only; emersion accepts both, but path-only matches
        what a real client sends)."""
        if href.startswith("http://") or href.startswith("https://"):
            # Strip scheme://host, keep the path.
            rest = href.split("://", 1)[1]
            slash = rest.find("/")
            return rest[slash:] if slash >= 0 else "/"
        return href

    def _parse_card_responses(self, content: bytes) -> list[VCardResource]:
        """Parse a multistatus body of <d:response> carrying <c:address-data> +
        <d:getetag> into [VCardResource]."""
        out: list[VCardResource] = []
        root = ET.fromstring(content)
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            data_el = r.find(".//c:address-data", _NS)
            etag_el = r.find(".//d:getetag", _NS)
            vcf = data_el.text if data_el is not None else None
            if vcf is None:
                continue
            out.append(
                VCardResource(
                    href=self._abs(href_el.text) if href_el is not None and href_el.text else "",
                    etag=etag_el.text if etag_el is not None else None,
                    uid=_vcf_field(vcf, "UID"),
                    fn=_vcf_field(vcf, "FN"),
                    vcf=vcf,
                )
            )
        return out


# ── vCard helpers ─────────────────────────────────────────────────────────
def _vcf_field(vcf: str | None, field: str) -> str | None:
    """Extract a top-level vCard property value (FN, UID, EMAIL, TEL). Handles a
    trailing ``;param=...`` on the property name and CRLF/LF line endings. Returns
    the first occurrence's value, stripped."""
    if not vcf:
        return None
    m = re.search(rf"^{field}[;:](.*)$", vcf, re.MULTILINE)
    if not m:
        return None
    val = m.group(1).strip()
    # If the regex caught a property carrying params ("TYPE=work:+1..."), the
    # value is after the first colon.
    if "=" in val.split(":", 1)[0] and ":" in val:
        val = val.split(":", 1)[1].strip()
    return val


def build_vcard(
    uid: str,
    fn: str,
    *,
    email: str | None = None,
    tel: str | None = None,
    org: str | None = None,
    note: str | None = None,
    version: str = "4.0",
) -> str:
    """Build a minimal RFC 6350 vCard. VERSION + FN + UID are mandatory (the MDA
    rejects a card missing any with 400 — carddav put.go extractRequiredCardFields
    + the encoder's VERSION requirement). vCard 4.0 is canonical; 3.0 is accepted
    for MUA compatibility."""
    lines = [
        "BEGIN:VCARD",
        f"VERSION:{version}",
        f"UID:{uid}",
        f"FN:{fn}",
    ]
    if email:
        lines.append(f"EMAIL:{email}")
    if tel:
        lines.append(f"TEL:{tel}")
    if org:
        lines.append(f"ORG:{org}")
    if note:
        lines.append(f"NOTE:{note}")
    lines.append("END:VCARD")
    return "\r\n".join(lines) + "\r\n"


def insecure_ssl_context() -> ssl.SSLContext:
    """A no-verify TLS context for pointing at a self-signed dev endpoint (the
    local MDA serves a self-signed floor/per-domain cert). Twin of
    caldav_client.insecure_ssl_context."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx

"""Minimal, dependency-light CalDAV client for e2e tests.

Speaks the real CalDAV wire (RFC 4791 / RFC 4918 / RFC 6764) over HTTPS with
HTTP Basic Auth — the exact surface a macOS Calendar.app / Thunderbird / Evolution
MUA uses against the Fauna mail-bridge MDA role. It is built on `requests` (already
in the venv) rather than the `caldav` PyPI lib so the tests carry no new dependency
and we control every byte on the wire (mirrors the raw-socket IMAP client the mail
deploy tests use in `helpers/mail_wire.py`).

Scope: just enough for the calendar round-trip matrix —
  - discover calendars under /caldav/{user}/ (PROPFIND Depth:1)
  - PUT / GET / DELETE an event by UID
  - list events with SUMMARY + ETag via a calendar-query REPORT

It is intentionally NOT a general CalDAV library; extend as the tests need.

Authority for the wire contract: docs/goal/behavior/caldav-server.md.
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

# XML namespaces used on the CalDAV wire.
_DAV = "DAV:"
_CALDAV = "urn:ietf:params:xml:ns:caldav"
_NS = {"d": _DAV, "c": _CALDAV}


def _client_put_slug(uid: str) -> str:
    """The *client-chosen* PUT filename — NOT the server's canonical resource
    slug. A real MUA (e.g. macOS Calendar.app) PUTs at whatever filename it
    likes; the MDA ignores it, parses the UID from the sealed body, and rewrites
    Location to the canonical ``blake3(UID)[:32].ics`` (caldav-server.md
    § Event resources; put.go step 3). So GET/DELETE must address the
    server-returned href (see ``_href_for_uid``), NEVER reconstruct a slug
    client-side — the canonical slug is a 32-byte blake3 hash the MDA's DELETE
    handler validates (delete.go: "must decode to 32 bytes"), which this
    sha256-truncated filename is not. We only need a stable, unique, URL-safe
    PUT filename here; sha256-hex (truncated, deterministic per UID) is plenty
    and avoids a blake3 dependency in the test harness.
    """
    return hashlib.sha256(uid.encode()).hexdigest()[:32]


@dataclass
class CalEvent:
    href: str
    etag: str | None
    uid: str | None
    summary: str | None
    ics: str | None


class CalDAVError(RuntimeError):
    def __init__(self, msg: str, resp: requests.Response | None = None):
        if resp is not None:
            msg = f"{msg}: HTTP {resp.status_code} {resp.reason}\n{resp.text[:2000]}"
        super().__init__(msg)
        self.resp = resp


class CalDAVClient:
    """A single CalDAV session (one user, one credential).

    base_url is the CalDAV root, e.g. https://example.com (the client appends
    /caldav/{user}/). username is the full handle (test@example.com); password is
    the mail credential password (AEAD-unwrap-as-auth, shared with IMAP).
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
        # local part (`userBasePath` → `sess.AuthedLocalPart()` base, backend.go).
        # When the AUTH username carries an RFC-5233 `+credential` suffix
        # (`<handle>+<credential_id>@<domain>`, mail-credentials.md § MUA-username),
        # the URL PATH must still use the bare `<handle>@<domain>` — exactly what a
        # real MUA does after principal/home-set discovery. A `+` left in the path
        # makes the MDA's home-set guard (`r.URL.Path == homeSetPath`) miss and serve
        # an EMPTY multistatus (no lazy-Personal create). The auth middleware runs
        # before path routing, so the suffixed username still selects the right
        # credential blob. Bare-handle usernames (no `+`) are unchanged.
        local, sep, dom = username.partition("@")
        base_local = local.split("+", 1)[0]
        path_user = f"{base_local}@{dom}" if sep else base_local
        self.home = f"{self.base}/caldav/{path_user}/"

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
        """Poll until the MDA's CalDAV HTTPS listener answers (any HTTP status —
        even a 401 challenge counts as 'serving'). A connection-level error means
        it is still cold-booting (re-enroll + re-attest + bind), which the
        dedicated-nest fixtures do lazily, so a fresh client must wait before its
        first real request. Raises CalDAVError if it never serves."""
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            try:
                self.session.request("PROPFIND", self.home, timeout=6.0)
                return
            except requests.exceptions.RequestException as e:
                last = type(e).__name__
                time.sleep(2.0)
        raise CalDAVError(f"CalDAV endpoint {self.base} never served within {timeout:.0f}s ({last})")

    # ── discovery ────────────────────────────────────────────────────────
    def list_calendars(self) -> list[str]:
        """PROPFIND Depth:1 on the home set → calendar collection hrefs.

        Returns absolute hrefs (including the home set itself filtered out).
        """
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
            "<d:prop><d:resourcetype/><d:displayname/>"
            "<c:supported-calendar-component-set/></d:prop></d:propfind>"
        )
        resp = self._req("PROPFIND", self.home, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CalDAVError("PROPFIND home set failed", resp)
        cals: list[str] = []
        root = ET.fromstring(resp.content)
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            if href_el is None or not href_el.text:
                continue
            href = href_el.text
            # A calendar collection has resourcetype containing <c:calendar/>.
            rtype = r.find(".//d:resourcetype", _NS)
            is_cal = rtype is not None and rtype.find("c:calendar", _NS) is not None
            if is_cal:
                cals.append(self._abs(href))
        return cals

    def personal_calendar(self) -> str:
        """The lazy 'Personal' calendar auto-created on first PROPFIND (per
        caldav-server.md § Lazy Personal calendar). Returns its href; raises if
        no calendar is discoverable."""
        cals = self.list_calendars()
        if not cals:
            raise CalDAVError(
                f"no calendars discovered under {self.home} — expected the lazy "
                "Personal calendar to be created on first PROPFIND"
            )
        return cals[0]

    # ── collection create / metadata ─────────────────────────────────────
    def mkcalendar(
        self,
        slug: str,
        *,
        displayname: str | None = None,
        color: str | None = None,
        description: str | None = None,
    ) -> str:
        """Create a calendar collection at the *client-chosen* ``slug`` via the
        dedicated RFC 4791 §5.3.1 MKCALENDAR verb — the exact request macOS
        Calendar.app issues to add a calendar (NOT the extended-MKCOL form
        emersion/go-webdav routes). Returns the slug href the calendar lives at;
        a real client keeps using this slug URL for every later
        PROPFIND/PUT/REPORT (the MDA's ``resolveCalendarSegment`` maps the non-hex
        slug to the canonical ``blake3(slug)[:32]`` id). Raises ``CalDAVError`` if
        the server does not answer 201 Created — e.g. 405 on an existing slug
        (RFC 4918 §9.3), whose ``.resp.status_code`` the caller can assert.

        A bare MKCALENDAR (no props) is valid (RFC 4791 §5.3.1) and lands the
        server defaults; pass displayname/color/description to seed initial
        properties, mirroring the ``<C:mkcalendar><D:set><D:prop>`` body a real
        client sends. Namespaces match the MDA parser: displayname=DAV:,
        calendar-color=Apple (http://apple.com/ns/ical/), description=CalDAV.
        """
        cal_href = f"{self.home}{slug.strip('/')}/"
        props: list[str] = []
        if displayname is not None:
            props.append(f"<d:displayname>{displayname}</d:displayname>")
        if color is not None:
            props.append(f"<a:calendar-color>{color}</a:calendar-color>")
        if description is not None:
            props.append(f"<c:calendar-description>{description}</c:calendar-description>")
        data = None
        if props:
            data = (
                '<?xml version="1.0" encoding="utf-8"?>'
                '<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" '
                'xmlns:a="http://apple.com/ns/ical/">'
                f'<d:set><d:prop>{"".join(props)}</d:prop></d:set>'
                "</c:mkcalendar>"
            ).encode()
        resp = self._req("MKCALENDAR", cal_href, data=data)
        if resp.status_code != 201:
            raise CalDAVError(f"MKCALENDAR {slug} failed", resp)
        return cal_href

    def displayname(self, cal_href: str) -> str | None:
        """PROPFIND Depth:0 for ``<d:displayname>`` on a single calendar
        collection. Returns the displayname text, or None if absent/empty.
        Doubles as proof the client's slug href resolves on PROPFIND (not just
        PUT/REPORT) and that a MKCALENDAR-sealed displayname round-tripped through
        the MDA's decrypt-on-read path."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:"><d:prop><d:displayname/><d:resourcetype/>'
            "</d:prop></d:propfind>"
        )
        resp = self._req("PROPFIND", self._abs(cal_href), headers={"Depth": "0"}, data=body)
        if resp.status_code not in (207, 200):
            raise CalDAVError("PROPFIND displayname failed", resp)
        root = ET.fromstring(resp.content)
        el = root.find(".//d:displayname", _NS)
        return el.text if el is not None and el.text else None

    def proppatch_displayname(self, cal_href: str, displayname: str) -> None:
        """Rename a calendar collection via PROPPATCH on ``{DAV:}displayname`` —
        the request macOS Calendar.app sends when a user renames a calendar. The
        MDA decrypts the sealed metadata, mutates the name, re-seals, and calls
        ``provision_calendar(update_metadata=true)``. Raises ``CalDAVError`` on
        any non-207 — including the spurious 404 a create-time rename used to hit
        when it raced the calendar's own ``MKCALENDAR`` insert (now absorbed by a
        bounded re-read; caldav-server.md § Create-then-rename race)."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propertyupdate xmlns:d="DAV:">'
            f"<d:set><d:prop><d:displayname>{displayname}</d:displayname></d:prop></d:set>"
            "</d:propertyupdate>"
        )
        resp = self._req("PROPPATCH", self._abs(cal_href), data=body)
        if resp.status_code not in (207, 200):
            raise CalDAVError(f"PROPPATCH displayname {displayname!r} failed", resp)

    # ── event CRUD ───────────────────────────────────────────────────────
    def _href_for_uid(self, cal_href: str, uid: str) -> str | None:
        """Resolve the server-canonical resource href for ``uid`` via a REPORT.

        Mirrors a real MUA: the MDA assigns the resource path (canonical
        ``blake3(UID)[:32].ics``, ignoring the client's PUT filename — put.go
        step 3), so GET/DELETE must address the href the server actually
        returned, not a client-reconstructed slug. Returns None if no event
        with that UID is currently in the collection.
        """
        for ev in self.list_events(cal_href):
            if ev.uid == uid:
                return ev.href
        return None

    def put_event(self, cal_href: str, uid: str, ics: str, *, if_match: str | None = None) -> str:
        """PUT an iCalendar object. Returns the new ETag (from the response
        header, or fetched if the server omits it)."""
        url = f"{cal_href.rstrip('/')}/{_client_put_slug(uid)}.ics"
        headers = {"Content-Type": "text/calendar; charset=utf-8"}
        if if_match:
            headers["If-Match"] = if_match
        resp = self._req("PUT", url, headers=headers, data=ics.encode())
        if resp.status_code not in (201, 204, 200):
            raise CalDAVError(f"PUT event {uid} failed", resp)
        etag = resp.headers.get("ETag")
        return etag or ""

    def get_event(self, cal_href: str, uid: str) -> str | None:
        """GET an event's iCalendar body, or None if absent.

        Addresses the server-returned href (resolved by UID via REPORT), the
        way a real MUA does — the client PUT filename is not the GET path.
        """
        href = self._href_for_uid(cal_href, uid)
        if href is None:
            return None
        resp = self.session.get(href, timeout=self.timeout)
        if resp.status_code == 404:
            return None
        if resp.status_code != 200:
            raise CalDAVError(f"GET event {uid} failed", resp)
        return resp.text

    def get_event_bytes(self, cal_href: str, uid: str) -> bytes | None:
        """GET an event's iCalendar body as RAW BYTES, or None if absent.

        The byte-exact twin of :meth:`get_event`. Use this — never
        :meth:`list_events`/:meth:`multiget` — for a byte-identity assertion:
        iCalendar lines are CRLF-terminated, but a REPORT carries the body
        inside a `<c:calendar-data>` XML element, and XML text parsing
        normalizes CRLF → LF. A REPORT-sourced body therefore differs from the
        PUT bytes in every line ending, which would make a byte-identity
        assertion fail for a reason that has nothing to do with the segment
        store. A GET returns the stored body unwrapped, so it is the only
        faithful place to compare bytes.
        """
        href = self._href_for_uid(cal_href, uid)
        if href is None:
            return None
        resp = self.session.get(href, timeout=self.timeout)
        if resp.status_code == 404:
            return None
        if resp.status_code != 200:
            raise CalDAVError(f"GET event {uid} failed", resp)
        return resp.content

    def multiget(self, cal_href: str, hrefs: list[str]) -> list[CalEvent]:
        """calendar-multiget REPORT over explicit hrefs → [CalEvent].

        The CalDAV twin of `CardDAVClient.multiget`, and the read path S6.11
        exercises: a real MUA fetches bodies by href this way, which drives the
        MDA's per-record open of the sealed body out of the `__calendar`
        segment. Bodies come back XML-wrapped, so their line endings are
        LF-normalized — compare content, not bytes (see :meth:`get_event_bytes`).
        """
        if not hrefs:
            return []
        href_xml = "".join(f"<d:href>{self._href_path(h)}</d:href>" for h in hrefs)
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
            "<d:prop><d:getetag/><c:calendar-data/></d:prop>"
            f"{href_xml}"
            "</c:calendar-multiget>"
        )
        resp = self._req("REPORT", cal_href, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CalDAVError("calendar-multiget REPORT failed", resp)
        events: list[CalEvent] = []
        root = ET.fromstring(resp.content)
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            etag_el = r.find(".//d:getetag", _NS)
            data_el = r.find(".//c:calendar-data", _NS)
            ics = data_el.text if data_el is not None else None
            events.append(
                CalEvent(
                    href=self._abs(href_el.text) if href_el is not None and href_el.text else "",
                    etag=etag_el.text if etag_el is not None else None,
                    uid=_ics_field(ics, "UID"),
                    summary=_ics_field(ics, "SUMMARY"),
                    ics=ics,
                )
            )
        return events

    def event_hrefs(self, cal_href: str) -> list[str]:
        """Every event href the server reports in the collection."""
        return [e.href for e in self.list_events(cal_href) if e.href]

    def raw(self, method: str, url: str, *, headers=None, data=None) -> requests.Response:
        """One request exactly as given, with no status check — for asserting
        what the server refuses (a 400, 412, 413) rather than what it serves.
        A relative ``url`` resolves against the server."""
        payload = data.encode() if isinstance(data, str) else data
        return self._req(method, self._abs(url), headers=headers, data=payload)

    def sync(self, cal_href: str, sync_token: str = "") -> tuple[list[CalEvent], list[str], str]:
        """sync-collection REPORT (RFC 6578) → (changed, removed_hrefs, new_token).

        The CalDAV twin of `CardDAVClient.sync`: an empty token is the initial
        sync (every event, body inline); a returned token asks for the delta
        since. A 403 ``DAV:valid-sync-token`` — the server telling the client its
        token can no longer be honoured — raises CalDAVError so the caller can
        assert it and fall through to a full re-sync, as a calendar app does.
        """
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:sync-collection xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
            f"<d:sync-token>{sync_token}</d:sync-token>"
            "<d:sync-level>1</d:sync-level>"
            "<d:prop><d:getetag/><c:calendar-data/></d:prop>"
            "</d:sync-collection>"
        )
        resp = self._req("REPORT", cal_href, headers={"Depth": "1"}, data=body)
        if resp.status_code == 403:
            raise CalDAVError("sync-collection returned DAV:valid-sync-token (stale)", resp)
        if resp.status_code not in (207, 200):
            raise CalDAVError("sync-collection REPORT failed", resp)
        root = ET.fromstring(resp.content)
        changed: list[CalEvent] = []
        removed: list[str] = []
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            href = self._abs(href_el.text) if href_el is not None and href_el.text else ""
            status_el = r.find("d:status", _NS)
            if status_el is not None and "404" in (status_el.text or ""):
                removed.append(href)
                continue
            data_el = r.find(".//c:calendar-data", _NS)
            etag_el = r.find(".//d:getetag", _NS)
            ics = data_el.text if data_el is not None else None
            changed.append(
                CalEvent(
                    href=href,
                    etag=etag_el.text if etag_el is not None else None,
                    uid=_ics_field(ics, "UID"),
                    summary=_ics_field(ics, "SUMMARY"),
                    ics=ics,
                )
            )
        token_el = root.find("d:sync-token", _NS)
        new_token = token_el.text if token_el is not None and token_el.text else ""
        return changed, removed, new_token

    def uids_in_range(self, cal_href: str, start: str, end: str) -> list[str]:
        """calendar-query REPORT with a ``<C:time-range>`` window (UTC
        ``YYYYMMDDTHHMMSSZ``) → the UIDs of every event with an occurrence in it,
        the question a calendar app asks when it opens a week or a month."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
            "<d:prop><d:getetag/><c:calendar-data/></d:prop>"
            '<c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">'
            f'<c:time-range start="{start}" end="{end}"/>'
            "</c:comp-filter></c:comp-filter></c:filter>"
            "</c:calendar-query>"
        )
        resp = self._req("REPORT", cal_href, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CalDAVError("time-range calendar-query REPORT failed", resp)
        root = ET.fromstring(resp.content)
        uids = []
        for r in root.findall("d:response", _NS):
            data_el = r.find(".//c:calendar-data", _NS)
            uid = _ics_field(data_el.text if data_el is not None else None, "UID")
            if uid:
                uids.append(uid)
        return uids

    def href_for_uid(self, cal_href: str, uid: str) -> str | None:
        """The server-canonical href of the event with ``uid``, or None."""
        return self._href_for_uid(cal_href, uid)

    def etag_for_uid(self, cal_href: str, uid: str) -> str | None:
        """The ETag the server currently reports for the event with ``uid``."""
        for e in self.list_events(cal_href):
            if e.uid == uid:
                return e.etag
        return None

    def _href_path(self, href: str) -> str:
        """Reduce an absolute href to its path (multiget <d:href> elements are
        conventionally path-only). Twin of `CardDAVClient._href_path`."""
        if href.startswith("http://") or href.startswith("https://"):
            rest = href.split("://", 1)[1]
            slash = rest.find("/")
            return rest[slash:] if slash >= 0 else "/"
        return href

    def delete_event(self, cal_href: str, uid: str, *, if_match: str | None = None) -> None:
        """DELETE an event by UID.

        Resolves the server-canonical href via REPORT first (a real MUA deletes
        the href it discovered, not a reconstructed slug). Absent already → no-op
        (DELETE is idempotent).
        """
        href = self._href_for_uid(cal_href, uid)
        if href is None:
            return
        headers = {}
        if if_match:
            headers["If-Match"] = if_match
        resp = self._req("DELETE", href, headers=headers)
        if resp.status_code not in (204, 200, 404):
            raise CalDAVError(f"DELETE event {uid} failed", resp)

    def list_events(self, cal_href: str) -> list[CalEvent]:
        """calendar-query REPORT for all VEVENTs → [CalEvent] with SUMMARY+UID.

        Used for cross-client visibility checks (we match events by SUMMARY).
        """
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
            "<d:prop><d:getetag/><c:calendar-data/></d:prop>"
            '<c:filter><c:comp-filter name="VCALENDAR">'
            '<c:comp-filter name="VEVENT"/></c:comp-filter></c:filter>'
            "</c:calendar-query>"
        )
        resp = self._req("REPORT", cal_href, headers={"Depth": "1"}, data=body)
        if resp.status_code not in (207, 200):
            raise CalDAVError("calendar-query REPORT failed", resp)
        events: list[CalEvent] = []
        root = ET.fromstring(resp.content)
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            etag_el = r.find(".//d:getetag", _NS)
            data_el = r.find(".//c:calendar-data", _NS)
            ics = data_el.text if data_el is not None else None
            events.append(
                CalEvent(
                    href=self._abs(href_el.text) if href_el is not None and href_el.text else "",
                    etag=etag_el.text if etag_el is not None else None,
                    uid=_ics_field(ics, "UID"),
                    summary=_ics_field(ics, "SUMMARY"),
                    ics=ics,
                )
            )
        return events

    def summaries(self, cal_href: str) -> list[str]:
        return [e.summary for e in self.list_events(cal_href) if e.summary]


# ── iCalendar helpers ────────────────────────────────────────────────────
def _ics_field(ics: str | None, field: str) -> str | None:
    if not ics:
        return None
    m = re.search(rf"^{field}[;:](.*)$", ics, re.MULTILINE)
    if not m:
        return None
    val = m.group(1).strip()
    # Strip a leading "param=...:" if the regex caught a property with params.
    if "=" in val.split(":", 1)[0] and ":" in val:
        val = val.split(":", 1)[1].strip()
    return val


def build_vevent(
    uid: str,
    summary: str,
    dtstart: str,
    dtend: str,
    *,
    description: str = "",
    sequence: int = 0,
) -> str:
    """Build a minimal RFC 5545 VEVENT. dtstart/dtend are 'YYYYMMDDTHHMMSSZ' (UTC).

    DTSTAMP/UID/DTSTART are mandatory per caldav-server.md § iCalendar parsing.
    """
    lines = [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Fauna//e2e-caldav//EN",
        "BEGIN:VEVENT",
        f"UID:{uid}",
        f"DTSTAMP:{dtstart}",
        f"DTSTART:{dtstart}",
        f"DTEND:{dtend}",
        f"SUMMARY:{summary}",
        f"SEQUENCE:{sequence}",
    ]
    if description:
        lines.append(f"DESCRIPTION:{description}")
    lines += ["END:VEVENT", "END:VCALENDAR"]
    return "\r\n".join(lines) + "\r\n"


def build_invite_vevent(
    uid: str,
    summary: str,
    dtstart: str,
    dtend: str,
    organizer: str,
    attendees: list[str],
    *,
    description: str = "",
    sequence: int = 0,
) -> str:
    """An RFC 5545 VEVENT carrying an ORGANIZER + ATTENDEE roster — the shape a
    scheduling client PUTs and the MDA's ``calendar-auto-schedule`` gateway fans
    out from (caldav-server.md § Server-side auto-schedule). The gateway fires
    only when the AUTH'd CalDAV user equals the ``ORGANIZER`` (autoschedule.go),
    so ``organizer`` must be the authenticating mailbox; ``attendees`` are bare
    ``local@domain`` mailboxes (the ``mailto:`` is added here).

    The same builder serves the live auto-schedule proof and is the shared
    sibling of the tier_4 docker test's inline ``_ics`` (priority #2 — one
    invite-body writer). DTSTAMP/UID/DTSTART are mandatory per
    caldav-server.md § iCalendar parsing; PARTSTAT seeds NEEDS-ACTION.
    """
    lines = [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Fauna//e2e-caldav-invite//EN",
        "BEGIN:VEVENT",
        f"UID:{uid}",
        f"DTSTAMP:{dtstart}",
        f"DTSTART:{dtstart}",
        f"DTEND:{dtend}",
        f"SUMMARY:{summary}",
        f"SEQUENCE:{sequence}",
        f"ORGANIZER:mailto:{organizer}",
    ]
    for a in attendees:
        lines.append(f"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{a}")
    if description:
        lines.append(f"DESCRIPTION:{description}")
    lines += ["END:VEVENT", "END:VCALENDAR"]
    return "\r\n".join(lines) + "\r\n"


def insecure_ssl_context() -> ssl.SSLContext:
    """A no-verify TLS context for pointing at a self-signed dev endpoint.

    The live example.com deployment serves a real ACME cert, so prefer verify=True.
    This exists only for local self-signed bring-up during iteration.
    """
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx

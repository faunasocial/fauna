"""Minimal, dependency-light WebDAV (RFC 4918) client for e2e tests.

Speaks the real WebDAV files wire over HTTPS with HTTP Basic Auth — the exact
surface GNOME Files / KDE Dolphin / macOS Finder / Cyberduck / rclone / the
NextCloud apps' WebDAV mode use against the Fauna mail-bridge MDA role. It is the
STRUCTURAL TWIN of `helpers/carddav_client.CardDAVClient` (same MDA process, same
`davauth` AUTH, same emersion depth-routing) — different protocol: RFC 4918
WebDAV *files* over the user's folder substrate, not RFC 6352 CardDAV. Built on
`requests` (already in the venv) so the tests carry no new dependency and we
control every byte on the wire (mirrors `caldav_client.py` / `carddav_client.py`).

The namespace the MDA serves (webdav-server.md § What the WebDAV namespace is):

    /webdav/{user}/                                  ← root collection
    /webdav/{user}/{folder-name}/                  ← one collection per served set
    /webdav/{user}/{folder-name}/<relative-path…>  ← the set's files

The {user} segment is cosmetic — every nest call is scoped to the AUTH'd actor,
so a client can only ever reach its own served sets regardless of the path
(webdav backend.go davPath). This client uses the AUTH address for realism.

Scope: just enough for the mount/gate, read/write round-trip and mount-behaviour
matrix —
  - PROPFIND Depth:0/1 (parsed to entries carrying href, collection-ness, size,
    last-modified and ETag);
  - GET / PUT (with If-Match / If-None-Match) / DELETE / MKCOL / MOVE / COPY,
    returning the raw `requests.Response` so a test asserts on status + body +
    ETag directly.

It is intentionally NOT a general WebDAV library; extend as the tests need.

Authority for the wire contract: docs/goal/behavior/webdav-server.md.
"""

from __future__ import annotations

import time
import urllib.parse
import xml.etree.ElementTree as ET
from dataclasses import dataclass

import requests
from requests.auth import HTTPBasicAuth

# The one XML namespace on the WebDAV files wire (no CalDAV/CardDAV extension NS).
_DAV = "DAV:"
_NS = {"d": _DAV}


@dataclass
class PropfindEntry:
    """One `<d:response>` from a PROPFIND multistatus: the resource href,
    whether it is a collection (``<d:resourcetype><d:collection/>``), and the
    live properties a file manager shows beside a file — ``getcontentlength``,
    ``getlastmodified`` (the raw RFC 1123 string) and ``getetag`` — each None
    when the server did not return it."""

    href: str
    is_collection: bool
    content_length: int | None = None
    last_modified: str | None = None
    etag: str | None = None

    @property
    def name(self) -> str:
        """The decoded last path segment of the href (no trailing slash)."""
        path = urllib.parse.urlsplit(self.href).path or self.href
        return urllib.parse.unquote(path.rstrip("/")).rsplit("/", 1)[-1]


class WebDAVError(RuntimeError):
    def __init__(self, msg: str, resp: requests.Response | None = None):
        if resp is not None:
            msg = f"{msg}: HTTP {resp.status_code} {resp.reason}\n{resp.text[:2000]}"
        super().__init__(msg)
        self.resp = resp


class WebDAVClient:
    """A single WebDAV session (one user, one credential).

    base_url is the WebDAV root, e.g. https://127.0.0.1:<caldav_port> (the shared
    MDA :443 DAV listener; the client appends /webdav/{user}/…). username is the
    full mail address (`admin@<domain>`); password is the mail credential password
    (AEAD-unwrap-as-auth, shared with IMAP/CalDAV/CardDAV). Twin of CardDAVClient.
    """

    def __init__(
        self,
        base_url: str,
        username: str,
        password: str,
        *,
        verify: bool | str = True,
        timeout: float = 60.0,
    ):
        self.base = base_url.rstrip("/")
        self.username = username
        self.password = password
        self.timeout = timeout
        self.session = requests.Session()
        self.session.auth = HTTPBasicAuth(username, password)
        self.session.verify = verify
        # The path {user} segment uses the bare `<handle>@<domain>` (any +suffix
        # stripped) — cosmetic on WebDAV (backend scopes every call to the AUTH'd
        # actor), but kept faithful to what a real client walks. The AUTH middleware
        # runs before path routing, so the username still selects the credential.
        local, sep, dom = username.partition("@")
        base_local = local.split("+", 1)[0]
        path_user = f"{base_local}@{dom}" if sep else base_local
        self.user = path_user
        self.root = f"{self.base}/webdav/{path_user}/"

    # ── URL helpers ──────────────────────────────────────────────────────────
    def _url(self, path: str) -> str:
        """Resolve a `path` to an absolute URL. Absolute http(s) passes through; a
        `/`-rooted path hangs off the base; anything else is relative to the user
        root (so `set/rel` → /webdav/{user}/set/rel)."""
        if path.startswith("http://") or path.startswith("https://"):
            return path
        if path.startswith("/"):
            return f"{self.base}{path}"
        return f"{self.root}{path.lstrip('/')}"

    # ── low-level request ────────────────────────────────────────────────────
    def _req(self, method: str, path: str, *, headers=None, data=None) -> requests.Response:
        return self.session.request(
            method, self._url(path), headers=headers or {}, data=data, timeout=self.timeout
        )

    def wait_until_serving(self, *, timeout: float = 120.0) -> None:
        """Poll until the MDA's shared :443 DAV listener answers on the /webdav/
        path (any HTTP status — even a 401 challenge counts as 'serving'). A
        connection-level error means it is still cold-booting (re-enroll +
        re-attest + bind), so a fresh client must wait before its first real
        request. Raises WebDAVError if it never serves. Twin of
        CardDAVClient.wait_until_serving."""
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            try:
                self.session.request(
                    "PROPFIND", self.root, headers={"Depth": "0"}, timeout=6.0
                )
                return
            except requests.exceptions.RequestException as e:
                last = type(e).__name__
                time.sleep(2.0)
        raise WebDAVError(
            f"WebDAV endpoint {self.base} never served within {timeout:.0f}s ({last})"
        )

    # ── PROPFIND ──────────────────────────────────────────────────────────────
    def propfind_raw(self, path: str = "", *, depth: str = "1") -> requests.Response:
        """PROPFIND `path` (relative to the user root, or "" for the root
        collection) at the given Depth, requesting resourcetype+displayname+
        getcontentlength+getetag. Returns the raw response so a test asserts on the
        status (207 vs 404 vs 401) directly."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:"><d:prop>'
            "<d:resourcetype/><d:displayname/>"
            "<d:getcontentlength/><d:getetag/><d:getlastmodified/>"
            "</d:prop></d:propfind>"
        )
        return self._req(
            "PROPFIND",
            path,
            headers={"Depth": depth, "Content-Type": "application/xml; charset=utf-8"},
            data=body,
        )

    def propfind(self, path: str = "", *, depth: str = "1") -> list[PropfindEntry]:
        """PROPFIND → the multistatus entries (href + is_collection). Raises
        WebDAVError on a non-207/200 status (so a 404 gate surfaces distinctly —
        tests wanting to assert the 404 use `propfind_raw`)."""
        resp = self.propfind_raw(path, depth=depth)
        if resp.status_code not in (207, 200):
            raise WebDAVError(f"PROPFIND {path!r} (Depth:{depth}) failed", resp)
        return self._parse_multistatus(resp)

    @staticmethod
    def _parse_multistatus(resp: requests.Response) -> list[PropfindEntry]:
        out: list[PropfindEntry] = []
        root = ET.fromstring(resp.content)
        for r in root.findall("d:response", _NS):
            href_el = r.find("d:href", _NS)
            if href_el is None or not href_el.text:
                continue
            rtype = r.find(".//d:resourcetype", _NS)
            is_col = rtype is not None and rtype.find("d:collection", _NS) is not None

            def _text(tag: str) -> str | None:
                el = r.find(f".//d:{tag}", _NS)
                return el.text.strip() if el is not None and el.text else None

            length = _text("getcontentlength")
            out.append(
                PropfindEntry(
                    href=href_el.text,
                    is_collection=is_col,
                    content_length=int(length) if length is not None else None,
                    last_modified=_text("getlastmodified"),
                    etag=_text("getetag"),
                )
            )
        return out

    def entry(self, path: str) -> PropfindEntry | None:
        """The Depth:1 PROPFIND entry for the file at `path` (``set/rel``), read
        from its parent collection's listing — the way a file manager learns a
        file's size and date. None when the listing does not carry it."""
        parent, _, leaf = path.rstrip("/").rpartition("/")
        for e in self.propfind(f"{parent}/" if parent else "", depth="1"):
            if not e.is_collection and e.name == leaf:
                return e
        return None

    def quota_raw(self, path: str = "") -> requests.Response:
        """PROPFIND Depth:0 on the collection `path` asking for the RFC 4331
        quota properties — what a file manager asks to show "space used / free"."""
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<d:propfind xmlns:d="DAV:"><d:prop>'
            "<d:quota-available-bytes/><d:quota-used-bytes/>"
            "</d:prop></d:propfind>"
        )
        return self._req(
            "PROPFIND",
            path,
            headers={"Depth": "0", "Content-Type": "application/xml; charset=utf-8"},
            data=body,
        )

    @staticmethod
    def parse_quota(resp: requests.Response) -> tuple[int | None, int | None]:
        """`(used, available)` from a `quota_raw` answer — each None when the
        server answered that property 404 (or not at all)."""
        root = ET.fromstring(resp.content)
        found: dict[str, int] = {}
        for ps in root.iter(f"{{{_DAV}}}propstat"):
            status = ps.find("d:status", _NS)
            if status is None or " 200 " not in f" {status.text or ''} ":
                continue
            for tag in ("quota-used-bytes", "quota-available-bytes"):
                el = ps.find(f"d:prop/d:{tag}", _NS)
                if el is not None and el.text and el.text.strip():
                    found[tag] = int(el.text.strip())
        return found.get("quota-used-bytes"), found.get("quota-available-bytes")

    def child_hrefs(self, path: str = "", *, depth: str = "1") -> list[str]:
        """The child hrefs a Depth:1 PROPFIND lists, EXCLUDING the collection
        itself (WebDAV includes the target resource as one `<d:response>`, and
        some servers repeat it). Used to assert 'the served-set list is empty' vs
        'lists set X'. Robust to percent-encoding (emersion may emit the `@` in the
        {user} segment as `%40`) and trailing-slash differences."""
        target = self._url(path)
        target_path = target[len(self.base):] if target.startswith(self.base) else target

        def norm(h: str) -> str:
            # href may be absolute or path-only; reduce to a decoded, slash-trimmed
            # path so the collection's self-entry compares equal regardless of
            # encoding/trailing slash.
            parsed = urllib.parse.urlsplit(h).path or h
            return urllib.parse.unquote(parsed).rstrip("/")

        self_path = norm(target_path)
        return [e.href for e in self.propfind(path, depth=depth) if norm(e.href) != self_path]

    # ── GET / PUT / DELETE / MKCOL ────────────────────────────────────────────
    def get(self, path: str) -> requests.Response:
        return self._req("GET", path)

    def put(
        self,
        path: str,
        body: bytes,
        *,
        if_match: str | None = None,
        if_none_match: str | None = None,
    ) -> requests.Response:
        headers = {"Content-Type": "application/octet-stream"}
        if if_match is not None:
            headers["If-Match"] = if_match
        if if_none_match is not None:
            headers["If-None-Match"] = if_none_match
        return self._req("PUT", path, headers=headers, data=body)

    def delete(self, path: str) -> requests.Response:
        return self._req("DELETE", path)

    def mkcol(self, path: str) -> requests.Response:
        return self._req("MKCOL", path)

    def move(self, path: str, dest: str, *, overwrite: bool = True) -> requests.Response:
        """MOVE `path` to `dest` (both relative to the user root) — a file
        manager's rename, or its move into another directory."""
        return self._copy_or_move("MOVE", path, dest, overwrite)

    def copy(self, path: str, dest: str, *, overwrite: bool = True) -> requests.Response:
        """COPY `path` to `dest` (both relative to the user root)."""
        return self._copy_or_move("COPY", path, dest, overwrite)

    def _copy_or_move(
        self, method: str, path: str, dest: str, overwrite: bool
    ) -> requests.Response:
        # RFC 4918 §10.3: Destination is an absolute URI; percent-encode the
        # path so the `@` in the {user} segment and any space survive.
        target = self._url(dest)
        parts = urllib.parse.urlsplit(target)
        dest_uri = urllib.parse.urlunsplit(
            parts._replace(path=urllib.parse.quote(parts.path, safe="/"))
        )
        return self._req(
            method,
            path,
            headers={"Destination": dest_uri, "Overwrite": "T" if overwrite else "F"},
        )

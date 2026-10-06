"""Playwright-backed automation bridge HTTP server.

Implements the same HTTP API as the FlaUI bridge so that the web
driver can use HttpBridgeDriver instead of embedding Playwright
directly.  This keeps the architecture consistent: every platform
is driven through a bridge process speaking the standard JSON API.

Usage:
    python server.py [--port PORT]

Prints BRIDGE_PORT=<port> to stdout on startup.
"""

from __future__ import annotations

import base64
import json
import mimetypes
import os
import shutil
import signal
import sys
import tempfile
import threading
from collections import deque
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path
from random import randint
from urllib.parse import parse_qs, urlparse

from playwright.sync_api import sync_playwright, Playwright, Browser, Page
from playwright.sync_api import TimeoutError as PlaywrightTimeoutError

# Browser resolution is shared with the tier_4 docker smokes (drivers/browser.py):
# Playwright's bundled Chromium first, system browser fallback. This script runs
# standalone (spawned by drivers/web.py), so put the e2e-unified root on the path.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from drivers.browser import launch_kwargs  # noqa: E402

SCREENSHOT_DIR = Path(__file__).parent.parent / "screenshots"

# Session-setup timeouts (ms). Chromium launch and the initial SPA navigation
# both carry Playwright's 30 s default; under machine load (concurrent
# builds/e2e saturating the box) either can exceed it and return a 500 that
# reads as a spurious setup failure — the server-side half of
# web-e2e-session-timeout-under-load. These generous ceilings let a loaded box
# finish setup instead of false-failing (a healthy box returns in seconds); the
# client `_post("/session")` ceiling (web.py SESSION_SETUP_TIMEOUT_S) is set to
# outlast launch+goto so it never times out first.
_SESSION_LAUNCH_TIMEOUT_MS = 90_000
_SESSION_NAV_TIMEOUT_MS = 90_000

# ---------------------------------------------------------------------------
# Global state managed by session lifecycle
# ---------------------------------------------------------------------------
# Pages are keyed by a string page id; "1" is the primary page every request
# defaults to (requests carry an optional `page` field/query param). Each page
# lives in its OWN BrowserContext — isolated localStorage/IndexedDB — so a
# second page is a second *device* for multi-device tests (the snap Chromium
# is machine-wide single-instance, so a second Browser is not an option; a
# second context in the one running browser is the sanctioned shape).
_playwright: Playwright | None = None
_browser: Browser | None = None
_contexts: dict[str, object] = {}
_pages: dict[str, Page] = {}
# Page ids whose `_contexts` entry is a BORROWED reference to another page's
# context (a same-context second TAB, `share_context` in `_handle_page_create`).
# Closing such a page must leave the owner's context — and therefore the shared
# origin store both tabs read — alive.
_borrowed_contexts: set[str] = set()
_next_page_id = 2

# Chromium `new_context()` options, shared by the session page and every /page
# twin so the two creation sites can never drift.
#
# `ignore_https_errors`: an e2e nest that serves real TLS serves the always-live
# **self-signed floor** cert (`start_nest(serve_tls=True)` drops the harness's
# process-wide `FAUNA_INSECURE_DISABLE_TLS` for that one nest;
# `nest/domains-and-tls-bootstrap.md` § Test posture). Every native client
# accepts it through the channel-binding trust model (`fauna_anon_client::
# tls_verify` `NoPinPolicy::AcceptProvisional`); a browser cannot — it has no
# programmatic trust hook, so the handshake dies with
# `net::ERR_CERT_AUTHORITY_INVALID` and no page code can catch it. That is a
# property of the FIXTURE's cert, not of the code under test: a real foreign
# nest a browser federates with is reachable at a CA-signed name. Telling
# Chromium to accept it is what makes the browser leg of a cross-nest journey
# observable at all.
#
# Concretely, without this the web arm of `test_fauna_mls_cross_nest_roundtrip`
# failed silently and misleadingly: the anonymous `actor_by_handle_remote` hop
# to the foreign nest could not open its `wss://`, `resolve_foreign` mapped the
# error to `NotFound`, the recipient degraded to an `Email` chip, and the DM
# went out over SMTP — no exception, just a key-package count that never moved.
_CONTEXT_KWARGS = {"ignore_https_errors": True}

_command_queue = []
# App-state cache keyed by page id (each page is its own app instance).
_cached_app_state: dict[str, dict] = {}

# Browser console + uncaught-error capture, keyed by page id. An unhandled
# promise rejection in the SPA's async onMount is INVISIBLE to every DOM/store
# signal the harness has (the page just renders its loader forever), but
# Chromium surfaces it as a `pageerror` event — so the bridge records it and
# `GET /page/console` hands it to failure messages (testing.md § point 6 —
# failures must diagnose themselves). A bounded ring so a chatty page can't
# grow the bridge without bound; one buffer per page id that SURVIVES
# kill/relaunch (the crash journeys care about the boot after the kill, and
# bridge-inserted markers separate the epochs).
_CONSOLE_RING_MAX = 500
_console_logs: dict[str, deque] = {}
# Total messages ever appended per page id, so `GET /page/console` can say how
# many the ring DROPPED. A bounded ring that silently forgets its head turns
# "the line is not here" into "the thing never happened", and a session spent a
# whole pass on exactly that inversion: a succession journey boots the SPA twice
# (the ceremony ends in a hard `window.location.assign`), the two boots together
# overrun 500 lines, the FIRST boot's evidence scrolls off — and the surviving
# `grep -c` of 1 reads like proof the pass ran once.
# 
_console_seen: dict[str, int] = {}


def _console_buf(pid: str) -> deque:
    return _console_logs.setdefault(pid, deque(maxlen=_CONSOLE_RING_MAX))


def _console_append(pid: str, line: str) -> None:
    """Append to `pid`'s ring and count it, so eviction stays observable."""
    _console_buf(pid).append(line)
    _console_seen[pid] = _console_seen.get(pid, 0) + 1


def _console_dropped(pid: str) -> int:
    """How many of `pid`'s console lines the ring has evicted (0 when intact)."""
    return max(0, _console_seen.get(pid, 0) - len(_console_logs.get(pid) or ()))


# Browser downloads the page started on its own (no `expect_download` wrapping
# the click), keyed by page id, and the directory each page's are saved into.
#
# `/element/download` covers a click that downloads at once. A download that
# starts long after its click cannot be wrapped that way: the mailbox export's
# Download unwraps and opens the whole archive first and only then hands the
# finished file to the browser (`mail-export.md` § Download flow step 5), and a
# REFUSED archive must start no download at all — which is an absence, and an
# absence needs a standing observer, not a wait wrapped round one click.
#
# So the event handler only records the `Download` (it may not call back into
# Playwright — same rule as the console capture below), and
# `POST /downloads/collect` saves what has arrived under each download's
# suggested filename and answers the directory: the web driver's
# `download_dir()`, the same observation every native driver offers.
_pending_downloads: dict[str, list] = {}
_download_dirs: dict[str, str] = {}


def _attach_download_capture(page: Page, pid: str) -> None:
    def on_download(download) -> None:
        try:
            _pending_downloads.setdefault(pid, []).append(download)
        except Exception:
            pass

    page.on("download", on_download)


def _collect_downloads(page: Page, pid: str) -> str:
    """Save every download page `pid` has started so far into its download
    directory, and return that directory."""
    directory = _download_dirs.get(pid)
    if directory is None:
        directory = tempfile.mkdtemp(prefix="fauna-web-downloads-")
        _download_dirs[pid] = directory
    # Playwright's sync API dispatches events only while one of its calls is
    # running, so a download that began since the last bridge request is not
    # in `_pending_downloads` until something round-trips.
    page.evaluate("0")
    for download in _pending_downloads.pop(pid, []):
        name = os.path.basename(download.suggested_filename) or "download"
        try:
            download.save_as(os.path.join(directory, name))
        except Exception as e:  # a cancelled or failed download saves nothing
            _console_append(pid, f"[bridge] download {name!r} was not saved: {e}")
    return directory


def _forget_downloads() -> None:
    """Drop every page's download state and remove the directories."""
    _pending_downloads.clear()
    for directory in _download_dirs.values():
        shutil.rmtree(directory, ignore_errors=True)
    _download_dirs.clear()


def _attach_console_capture(page: Page, pid: str, *, tag: str = "") -> None:
    """Record console messages and uncaught page errors (incl. unhandled
    promise rejections) for page `pid`. Handlers never raise and never call
    back into Playwright (a round-trip inside a sync-API event handler can
    deadlock the dispatch greenlet) — they only format payload fields."""
    _console_buf(pid)
    prefix = f"{tag} " if tag else ""

    def on_console(msg) -> None:
        try:
            loc = msg.location or {}
            where = f"{loc.get('url', '')}:{loc.get('lineNumber', '')}"
            _console_append(pid, f"{prefix}[console.{msg.type}] {msg.text} ({where})")
        except Exception:
            pass

    def on_pageerror(err) -> None:
        try:
            stack = getattr(err, "stack", "") or ""
            message = getattr(err, "message", None) or str(err)
            _console_append(pid, f"{prefix}[pageerror] {message}"
                            + (f"\n{stack}" if stack and stack not in message else ""))
        except Exception:
            pass

    page.on("console", on_console)
    page.on("pageerror", on_pageerror)


def _file_payloads(files: str | list[str]) -> list[dict]:
    """Convert file path(s) to in-memory Playwright FilePayloads.

    Passing a path to ``set_input_files`` makes the browser open that path
    itself. A snap-confined Chromium (the default on Ubuntu — ``chromium`` is
    a snap whose sandbox can't read ``/work/...``) then hands the page a File
    whose disk backing it cannot open, so the SPA's ``FileReader`` rejects with
    ``NotFoundError`` and the attachment silently never reaches compose state.
    Reading the bytes here (the bridge is unconfined) and sending them as a
    ``{name, mimeType, buffer}`` payload embeds the contents over CDP, so the
    browser never touches the filesystem — robust regardless of sandboxing.
    """
    paths = [files] if isinstance(files, str) else list(files)
    payloads: list[dict] = []
    for p in paths:
        path = Path(p)
        mime, _ = mimetypes.guess_type(path.name)
        payloads.append({
            "name": path.name,
            "mimeType": mime or "application/octet-stream",
            "buffer": path.read_bytes(),
        })
    return payloads


def _selector(element_id: str) -> str:
    return f'[data-testid="{element_id}"]'


def _scoped_root(page, scope_steps: list[dict] | None):
    """Chain Playwright locators for each scope step, returning the scoped root."""
    root = page
    if scope_steps:
        for step in scope_steps:
            root = root.locator(_selector(step["id"])).nth(step.get("index", 0))
    return root


# Convention 11's actuation gate, web's half (`e2e-conventions.md` § convention
# 11). Web refuses a DISABLED control by construction — Playwright's
# `click`/`dblclick`/`clear`/`select_option` wait for the element to be enabled
# and give up — but it used to give up as a bare 500 carrying Playwright's
# timeout text, which names no rule and never says "disabled". Every other app's
# agent answers the SAME named 409, and `drivers/http_bridge.py` discriminates
# it from `select`'s option-not-offered 409 on the text alone, so a cross-app
# test (`test_conversation_room_roles.py`: a plain member's greyed Remove chip)
# could not assert the refusal on web. The wait window is kept exactly as it
# was — the gate speaks only once Playwright's own wait has closed, as windows'
# does after its enabled-retry — so no control that legitimately enables a
# moment late is refused. `type` stays ungated: `press_sequentially` performs
# no enabled wait for a refusal to be mapped from.


def _disabled_actuation_refusal(route: str, element_id: str, index: int) -> dict:
    """Word for word `fauna_e2e_agent::disabled_actuation_refusal` (and windows'
    `ActuationGate.RefusalMessage`) — 409, never 404: the element WAS found."""
    return {
        "error": (
            f"element is disabled: {element_id}[{index}] — {route} refused "
            "(convention 11: an actuation route must not drive a control the UI "
            "has disabled)"
        ),
        "status": 409,
        "id": element_id,
        "index": index,
    }


def _found_disabled(loc) -> bool:
    """Whether `loc` resolves to an element Playwright reads as disabled (a
    `disabled` form control, or `aria-disabled="true"` — the `/element/enabled`
    route's own predicate). Asked only after an actuation gave up, so a missing
    element answers False and the original failure propagates unchanged."""
    try:
        return loc.count() > 0 and loc.is_disabled(timeout=2_000)
    except Exception:
        return False


def _actuate(handler, route: str, req: dict, loc, act) -> None:
    """Run `act`; answer 200, or the named disabled refusal when Playwright's
    actionability wait gave up on a control the UI has disabled."""
    try:
        act()
    except PlaywrightTimeoutError:
        if _found_disabled(loc):
            handler._respond(
                409, _disabled_actuation_refusal(route, req["id"], req.get("index", 0))
            )
            return
        raise
    handler._respond(200, {})


# `GET /registry` — the structured twin of `/element/*`: every VISIBLE
# `[data-testid]` element on the page as one record, so a caller can quantify
# over the whole frame (e2e-conventions.md convention 17 / point 17, whose text
# now lives in `e2e-systematic-ui-walks.md`) instead of grepping hand-picked
# ids. Mirrors apple's `AutomationRegistry.snapshot()` /
# `InProcessAutomationServer.swift`'s `/registry` handler field-for-field
# (`base.py::registry_snapshot`'s doc comment is the cross-app contract).
#
# One `page.evaluate()` round trip computes the whole frame client-side —
# doing this element-by-element over N Playwright round trips would be both
# slow and, worse, non-atomic (the frame could mutate between reads, the same
# hazard `/element/text`'s doc comment calls out for count-then-get).
#
# `index` MUST match what `_selector(id)).nth(index)` addresses, so it is
# computed the identical way: per-testid occurrence index among VISIBLE
# elements, document order — exactly what a scope step or an indexed action
# call resolves against.
_REGISTRY_SNAPSHOT_JS = r"""
() => {
  function isVisible(el) {
    if (!el.isConnected) return false;
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse') return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }
  const FORM_TAGS = new Set(['BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'OPTION', 'OPTGROUP', 'FIELDSET']);
  const EDITABLE_TAGS = new Set(['INPUT', 'TEXTAREA', 'SELECT']);
  const ACTUABLE_TAGS = new Set(['BUTTON', 'A']);

  const visible = Array.from(document.querySelectorAll('[data-testid]')).filter(isVisible);

  // Per-testid occurrence index among the VISIBLE set, document order — the
  // same addressing `_selector(id).nth(index)` and every scope step use.
  const indexOf = new Map();
  const counters = new Map();
  for (const el of visible) {
    const id = el.getAttribute('data-testid');
    const idx = counters.get(id) || 0;
    indexOf.set(el, idx);
    counters.set(id, idx + 1);
  }

  // The ancestor `[data-testid]` chain, rendered as the wire DSL
  // (`post-card[1]/quoted-post`) — the descendant-scope steps a caller would
  // chain through `_scoped_root` to reach this element, outermost first.
  // Only ancestors that are THEMSELVES visible-and-indexed qualify: an
  // invisible ancestor is not a step any caller could actually address.
  function scopeOf(el) {
    const steps = [];
    let node = el.parentElement;
    while (node) {
      if (indexOf.has(node)) {
        steps.unshift(node.getAttribute('data-testid') + '[' + indexOf.get(node) + ']');
      }
      node = node.parentElement;
    }
    return steps.join('/');
  }

  return visible.map((el) => {
    const tag = el.tagName;
    const nativeDisabled = FORM_TAGS.has(tag) ? !!el.disabled : false;
    const rect = el.getBoundingClientRect();
    return {
      id: el.getAttribute('data-testid'),
      index: indexOf.get(el),
      // Mirrors /element/enabled's own is_visible() && is_enabled() — every
      // element here already passed the visibility filter above, so this is
      // exactly the disabled-cascade check (a fieldset-disabled descendant's
      // OWN .disabled IDL property already reflects the inherited state).
      enabled: !nativeDisabled,
      // Reads the `use:offlineGate` action's own marker (offline-gate.ts) —
      // NOT a tag-type guess — so this answers "did a call site's own
      // predicate ever reach this element", the apple-hazard distinction
      // (account-data-plane.md § Built — the apple leg) a tag check cannot
      // make: a plain unwired <button> and a wired-but-currently-enabled one
      // read identically by tag alone.
      declares_enabled: el.getAttribute('data-offline-gate-declared') === 'true',
      actuable: ACTUABLE_TAGS.has(tag) && (tag !== 'A' || el.hasAttribute('href')),
      editable: EDITABLE_TAGS.has(tag),
      scope: scopeOf(el),
      // Same "x,y,w,h" spelling apple's /registry uses, so one parser (were
      // a caller to want one) serves both.
      frame: Math.round(rect.x) + ',' + Math.round(rect.y) + ',' + Math.round(rect.width) + ',' + Math.round(rect.height),
    };
  });
}
"""


# `/element/attr?attr=text-runs` — see that arm. Walks the element's text nodes
# in document order; each run's `tags` are its `cm-md-*` ancestors below the
# element, each describing only the looks it changes from its parent (weight,
# family, font-size scale) plus its own left indent (padding + margin, px).
_TEXT_RUNS_JS = r"""
el => {
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  const runs = [];
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const tags = [];
    for (let a = node.parentElement; a && a !== el; a = a.parentElement) {
      const names = Array.from(a.classList).filter(c => c.startsWith('cm-md-'));
      if (names.length === 0) continue;
      const cs = getComputedStyle(a);
      const ps = getComputedStyle(a.parentElement);
      const size = parseFloat(cs.fontSize), parentSize = parseFloat(ps.fontSize);
      const indent = parseFloat(cs.paddingLeft) + parseFloat(cs.marginLeft);
      tags.push({
        name: names.join(' '),
        weight: cs.fontWeight !== ps.fontWeight ? parseInt(cs.fontWeight, 10) : null,
        family: cs.fontFamily !== ps.fontFamily ? cs.fontFamily : null,
        scale: size !== parentSize ? size / parentSize : null,
        left_margin: indent > 0 ? indent : null,
        invisible: cs.display === 'none' || cs.visibility === 'hidden',
      });
    }
    runs.push({ text: node.data, tags });
  }
  return runs.length ? runs : null;
}
"""


# ---------------------------------------------------------------------------
# HTTP handler
# ---------------------------------------------------------------------------
class BridgeHandler(BaseHTTPRequestHandler):
    """Handles the bridge HTTP API."""

    def log_message(self, format, *args):
        pass  # suppress request logs

    def _read_json(self) -> dict:
        length = int(self.headers.get("Content-Length", 0))
        if length == 0:
            return {}
        return json.loads(self.rfile.read(length))

    def _respond(self, status: int, data: dict) -> None:
        body = json.dumps(data).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _query(self) -> dict[str, str]:
        qs = parse_qs(urlparse(self.path).query)
        return {k: v[0] for k, v in qs.items()}

    def _route_path(self) -> str:
        return urlparse(self.path).path

    @staticmethod
    def _parse_scope(source: dict) -> list[dict] | None:
        raw = source.get("scope")
        if not raw:
            return None
        if isinstance(raw, str):
            return json.loads(raw)
        return raw

    def _require_page(self, source: dict | None = None) -> Page:
        pid = str((source or {}).get("page") or "1")
        page = _pages.get(pid)
        if page is None:
            raise RuntimeError(
                f"No active page '{pid}'. POST /session first (POST /page for twins)."
            )
        return page

    # --- GET routes ---

    def do_GET(self):  # noqa: N802
        route = self._route_path()
        try:
            if route == "/health":
                self._respond(200, {"ready": True, "platform": "web", "version": "1.0.0"})

            elif route == "/clipboard/text":
                # The browser's real async clipboard, read the way a page would —
                # never an intercepted `writeText` call (that would witness the
                # SPA's intent, not what landed). Chromium gates `readText` on
                # the `clipboard-read` permission and on a focused document, so
                # grant it for this page's context and focus the page first.
                # The windows twin is the FlaUI bridge's GET /clipboard/text.
                q = self._query()
                page = self._require_page(q)
                page.context.grant_permissions(["clipboard-read", "clipboard-write"])
                page.bring_to_front()
                text = page.evaluate(
                    "async () => { try { return await navigator.clipboard.readText(); }"
                    " catch (e) { return null; } }"
                )
                self._respond(200, {"text": text or None})

            elif route == "/element/text":
                q = self._query()
                page = self._require_page(q)
                eid = q.get("id", "")
                idx = int(q.get("index", "0"))
                root = _scoped_root(page, self._parse_scope(q))
                # Honor the cross-app get_text contract: on a form *field*
                # (<input>/<textarea>/<select>) the "text" is its current
                # value, not its (always-empty) text content — matching how the
                # GTK / XCUITest / AT-SPI drivers report a field's contents.
                # Everything else (including <button>, which has a `.value`
                # property that defaults to "" and would otherwise shadow its
                # visible label) reports textContent — a button's cross-app
                # "text" is its label, the same as GTK/XCUITest return. A
                # checkbox or radio is a button in this sense: its `.value` is
                # the constant "on", so it reports its <label>'s text, as a GTK
                # CheckButton reports its label.
                #
                # Bound the wait to 3s (not Playwright's 30s default) and treat a
                # transient miss as "". The shared count-then-get_text read pattern
                # (drivers/base.count() + per-index get_text in actions.*.patterns())
                # is NOT atomic: a list mutating between the count() round-trip and
                # a get_text(i) round-trip (e.g. a row deleted mid-poll on the
                # mail-aliases page, exercised by wait_for_pattern_gone) leaves
                # nth(i) transiently out of range. A present element evaluates in
                # <100ms so the bound never bites a real read; an out-of-range
                # index fails fast to "" instead of hanging 30s, letting the
                # caller's poll retry — matching the race-tolerant behavior the
                # GTK/AT-SPI drivers already have.
                try:
                    text = root.locator(_selector(eid)).nth(idx).evaluate(
                        "el => { const t = el.tagName.toLowerCase();"
                        " if (t === 'input' && (el.type === 'checkbox' || el.type === 'radio'))"
                        " return el.labels && el.labels.length"
                        " ? (el.labels[0].textContent ?? '').trim() : '';"
                        " return (t === 'input' || t === 'textarea' || t === 'select')"
                        " ? (el.value ?? '') : (el.textContent ?? ''); }",
                        timeout=3000,
                    )
                except PlaywrightTimeoutError:
                    text = ""
                self._respond(200, {"text": text or ""})

            elif route == "/element/visible":
                q = self._query()
                page = self._require_page(q)
                eid = q.get("id", "")
                root = _scoped_root(page, self._parse_scope(q))
                # `.first`: an unscoped `is_visible` on an indexed id (e.g.
                # `recipient-picker-chip` with two chips accepted) otherwise hits
                # Playwright's strict-mode "resolved to N elements" error. The
                # native bridges report visibility of the first match; mirror
                # that. Scoped queries (`[i]`) still target the exact element.
                visible = root.locator(_selector(eid)).first.is_visible()
                self._respond(200, {"visible": visible})

            elif route == "/element/count":
                q = self._query()
                page = self._require_page(q)
                eid = q.get("id", "")
                root = _scoped_root(page, self._parse_scope(q))
                count = root.locator(_selector(eid)).count()
                self._respond(200, {"count": count})

            elif route == "/element/enabled":
                # An element is "enabled" if it's both visible and not
                # marked disabled (HTML `disabled` attribute or
                # aria-disabled="true"). Mirrors how XCUITest /
                # AT-SPI / FlaUI report enabledness.
                q = self._query()
                page = self._require_page(q)
                eid = q.get("id", "")
                root = _scoped_root(page, self._parse_scope(q))
                # `.first` for the same strict-mode reason as /element/visible:
                # an indexed id with 2+ matches must not raise "resolved to N
                # elements". Mirrors the native bridges (first match).
                loc = root.locator(_selector(eid)).first
                enabled = loc.is_visible() and loc.is_enabled()
                self._respond(200, {"enabled": enabled})

            elif route == "/element/attr":
                # Read a named attribute. Tries plain `attr` first, then
                # falls back to `data-attr` so callers can use unprefixed
                # names ("state" matches `data-state`).
                q = self._query()
                page = self._require_page(q)
                eid = q.get("id", "")
                attr = q.get("attr", "")
                root = _scoped_root(page, self._parse_scope(q))
                # `index` picks among matches, exactly as `/element/text` and
                # every actuation route take it — the cross-app `get_attr`
                # contract (`drivers/base.py::get_attr`), which the driver has
                # always sent. This route read `.first` instead, so EVERY
                # indexed attribute read on web silently answered element 0's
                # attribute: found 2026-09-12 by the room journeys' first web
                # run, where `thread-member-chip[1]`'s `role` read back as
                # chip 0's `owner` and `room-owner-transfer-button[1]`'s
                # `checked` as row 0's `false`. Unlike `/element/visible`'s
                # deliberate first-match (native parity for an UNINDEXED
                # read), an attribute read names its element — `.nth` is as
                # strict-mode-safe as `.first`.
                loc = root.locator(_selector(eid)).nth(int(q.get("index", "0")))
                if attr == "disabled":
                    # `disabled` ← `is_disabled()`, not a raw attribute read —
                    # mirrors the GTK bridge's `disabled` ← `!is_sensitive()`
                    # (`fauna-linux/src/automation/agent.rs`), the shared
                    # cross-platform `get_attr(id, "disabled")` contract every
                    # action-layer caller relies on. A raw
                    # `get_attribute("disabled")` would break that contract: a
                    # real HTML boolean `disabled` attribute serializes to
                    # `""` when present (not `None`), so it would never reach
                    # the `data-disabled` fallback below and would read as
                    # "not disabled" regardless of actual state.
                    value = "true" if loc.is_disabled() else "false"
                elif attr == "options":
                    # The full list of option TEXTS this render painted for a
                    # native `<select>` — not just the selected one — JSON-
                    # encoded so `drivers/base.py::option_texts` can assert
                    # the whole set is pairwise distinct, which a single
                    # selected-value read-back cannot do (two colliding
                    # options read back identically once one is picked).
                    # `null` (not `[]`) for a non-<select> element, mirroring
                    # `/element/select`'s own `offered` probe above — an
                    # empty list means "a real <select> with zero options",
                    # which is a different fact.
                    offered = loc.evaluate(
                        "el => el.options ? Array.from(el.options).map(o => o.textContent) : null"
                    )
                    value = json.dumps(offered) if offered is not None else None
                elif attr == "selected-text":
                    # The SELECTED option's painted text — what every native
                    # bridge's `get_text` already answers for a picker, while
                    # web's `get_text` answers the `<option value>`. A caller
                    # comparing the selection against the `options` set above
                    # needs both read in one vocabulary (the task-delegation
                    # picker: a foreign pin's value is a full participant hex,
                    # its painted label a short one). `null` for a non-<select>.
                    value = loc.evaluate(
                        "el => el.options && el.selectedIndex >= 0"
                        " ? el.options[el.selectedIndex].textContent : null"
                    )
                elif attr == "text-runs":
                    # The compose field's STYLING as the page APPLIED it — the
                    # web twin of linux's `text-runs` (`fauna-linux/src/automation/
                    # agent.rs`), same JSON shape: one record per rendered text
                    # node, its text and one tag per `cm-md-*` ancestor carrying
                    # the looks that ancestor SETS (a look equal to its parent's is
                    # `null`, so a default never reads as a style). Read off the
                    # computed style of the live DOM, never recomputed from the
                    # shared decoration plan: the question is whether the editor
                    # painted the styling. `null` for an element with no text.
                    runs = loc.evaluate(_TEXT_RUNS_JS)
                    value = json.dumps(runs) if runs is not None else None
                else:
                    # `data-<attr>` WINS over a same-named real attribute.
                    #
                    # The convenience used to be a fallback — raw first, the
                    # `data-` twin only when the raw read came back `None`.
                    # That makes the twin UNREACHABLE for exactly the names
                    # where it matters most, because a real HTML/ARIA
                    # attribute of that name is never `None`: the room model's
                    # cross-app contract asks a driver for `class` on
                    # `thread-room-class` (`end-to-end | community |
                    # transport-only`) and `role` on `thread-member-chip[i]`
                    # (`owner | admin | member`), and on web those names are
                    # already taken by the CSS class list and by the chip's
                    # ARIA `role="button"`. A fallback would have handed every
                    # room journey `"room-class"` and `"button"` — silently,
                    # and only on this one app.
                    #
                    # Preferring the prefixed name is strictly more correct: a
                    # `data-` attribute exists only because a painter put it
                    # there FOR a driver, while the bare name may mean
                    # something to the browser. It changes a read only where
                    # an element carries both, which no element did before the
                    # room model. (`disabled` never reaches here — it is
                    # answered from `is_disabled()` above, the shared
                    # cross-platform contract.)
                    value = None
                    if not attr.startswith("data-"):
                        value = loc.get_attribute(f"data-{attr}")
                    if value is None:
                        value = loc.get_attribute(attr)
                self._respond(200, {"value": value})

            elif route == "/registry":
                q = self._query()
                page = self._require_page(q)
                elements = page.evaluate(_REGISTRY_SNAPSHOT_JS)
                self._respond(200, {"elements": elements})

            elif route == "/app/commands":
                if not _command_queue:
                    self._respond(204, {})
                else:
                    self._respond(200, _command_queue.pop(0))

            elif route == "/page/console":
                # The captured browser console + pageerror ring for a page id.
                # Deliberately allowed for a KILLED page (its buffer survives
                # until /page DELETE or session close): the crash journeys read
                # it after the post-kill boot wedges.
                q = self._query()
                pid = str(q.get("page") or "1")
                live = _pages.get(pid)
                if live is not None:
                    # The sync API only dispatches queued events while a
                    # Playwright call is in flight — pump once so messages
                    # logged since the last bridge call reach the ring.
                    try:
                        live.evaluate("1")
                    except Exception:
                        pass
                self._respond(200, {
                    "lines": list(_console_logs.get(pid) or []),
                    # How many lines the ring has evicted. The driver turns a
                    # non-zero count into a leading warning line, so an absent
                    # line is never read as proof it was never logged.
                    "dropped": _console_dropped(pid),
                })

            elif route == "/app/state":
                q = self._query()
                state = _cached_app_state.get(str(q.get("page") or "1"))
                if not state:
                    self._respond(204, {})
                else:
                    self._respond(200, state)

            else:
                self._respond(404, {"error": f"Unknown route: GET {route}"})

        except RuntimeError as e:
            self._respond(500, {"error": str(e)})
        except Exception as e:
            self._respond(500, {"error": str(e)})

    # --- POST routes ---

    def do_POST(self):  # noqa: N802
        route = self._route_path()
        try:
            if route == "/session":
                self._handle_session_create()

            elif route == "/page":
                self._handle_page_create()

            elif route == "/page/kill":
                self._handle_page_kill()

            elif route == "/page/relaunch":
                self._handle_page_relaunch()

            elif route == "/page/seed-factory-reset":
                self._handle_seed_factory_reset()

            elif route == "/element/click":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                _actuate(self, "click", req, loc, loc.click)

            elif route == "/element/double_click":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                _actuate(self, "double-click", req, loc, loc.dblclick)

            elif route == "/element/type":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                root.locator(_selector(req["id"])).nth(req.get("index", 0)).press_sequentially(req["text"])
                self._respond(200, {})

            elif route == "/element/clear":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                _actuate(self, "clear", req, loc, loc.clear)

            elif route == "/element/scroll-into-view":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                try:
                    loc.scroll_into_view_if_needed()
                except PlaywrightTimeoutError:
                    # A genuinely absent element (never attached) times out here
                    # exactly like `_actuate`'s actionability wait does — report
                    # it as "not found" (404), the one status `_post` maps to
                    # `LookupError`, so `is_visible_scrolled` (which only
                    # degrades to a plain `is_visible` read on
                    # `NotImplementedError`/`LookupError`/`TimeoutError`) can
                    # reach its documented "never masks a genuinely absent
                    # element" answer instead of a bare 500 (`RuntimeError`
                    # client-side) propagating past every caller. An element that DOES exist but still
                    # can't be scrolled into view (obstructed, detached
                    # container, …) is a real bug, so that case still re-raises
                    # to the generic 500 path.
                    if loc.count() == 0:
                        self._respond(404, {"error": f"could not find element: {req['id']!r}"})
                        return
                    raise
                self._respond(200, {"found": True})

            elif route == "/element/key":
                # Focus the element, then dispatch a single named keypress.
                # Key names follow web KeyboardEvent.key — Playwright's
                # keyboard.press takes the same convention.
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                loc.focus()
                page.keyboard.press(req["key"])
                self._respond(200, {})

            elif route == "/keyboard/press":
                # Bare keypress with NO prior focus — unlike /element/key
                # above (which `loc.focus()`s the target first, and would
                # therefore always restart the tab order from that element
                # rather than advance it), this is the correct primitive for
                # driving keyboard-focus TRAVERSAL (Tab / Shift+Tab) from
                # wherever focus already sits. Used by `drivers/web.py`'s
                # `focus_move`/`switch_pane` — convention 17 layer (c)'s
                # driver-level walk vocabulary
                # (`e2e-systematic-ui-walks.md` § web leg).
                req = self._read_json()
                page = self._require_page(req)
                page.keyboard.press(req["key"])
                self._respond(200, {})

            elif route == "/element/download":
                # Click `id` and capture the browser download it triggers (e.g.
                # `snapshot-file-download-button`, backups.md § Single-file byte
                # download) — Playwright's `expect_download` context manager must
                # wrap the click itself, so this can't be composed from the plain
                # `/element/click` route. Returns the downloaded bytes base64'd
                # (JSON has no binary payload) + the suggested filename.
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                with page.expect_download() as download_info:
                    loc.click()
                download = download_info.value
                data = Path(download.path()).read_bytes()
                self._respond(200, {
                    "filename": download.suggested_filename,
                    "data_b64": base64.b64encode(data).decode("ascii"),
                })

            elif route == "/downloads/collect":
                # Save the downloads this page has started so far and answer
                # the directory they are in (`_collect_downloads`).
                req = self._read_json()
                page = self._require_page(req)
                pid = str(req.get("page") or "1")
                self._respond(200, {"dir": _collect_downloads(page, pid)})

            elif route == "/screenshot":
                req = self._read_json()
                page = self._require_page(req)
                SCREENSHOT_DIR.mkdir(parents=True, exist_ok=True)
                path = SCREENSHOT_DIR / f"{req.get('name', 'screenshot')}.png"
                page.screenshot(path=path)
                self._respond(200, {"path": str(path)})

            elif route == "/navigate":
                req = self._read_json()
                page = self._require_page(req)
                page.goto(req["url"])
                self._respond(200, {})

            elif route == "/execute":
                req = self._read_json()
                page = self._require_page(req)
                result = page.evaluate(req["script"], req.get("arg"))
                self._respond(200, {"result": result})

            elif route == "/element/fill":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                try:
                    loc.clear()
                except Exception:
                    # Range/number inputs don't support clear() — fill() handles replacement.
                    pass
                loc.fill(req["text"])
                self._respond(200, {})

            elif route == "/element/select":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                loc = root.locator(_selector(req["id"])).nth(req.get("index", 0))
                # Refuse a value this render never offered, EXPLICITLY — the
                # twin rule of convention 11 (a picker refuses a value the frame
                # did not offer). `select_option` alone already refuses, but by
                # burning its full 30 s timeout and then reporting "did not find
                # some options" without naming what WAS offered — a slow,
                # non-self-diagnosing refusal (convention 6). Reading the option
                # values first turns it into an immediate 409 that says exactly
                # what happened, matching tui's shape and the driver's
                # `SelectOptionNotOffered`.
                # `null` (not `[]`) when the element is not a native <select>:
                # a custom picker has no readable option list here, and an empty
                # list would mean "offers nothing", which would refuse every
                # value on it. A real <select> with zero options DOES return `[]`
                # and correctly refuses.
                offered = loc.evaluate(
                    "el => el.options ? Array.from(el.options).map(o => o.value) : null"
                )
                if isinstance(offered, list) and req["value"] not in offered:
                    # The disabled refusal outranks this one (tui's ordering,
                    # `e2e-conventions.md` § convention 11): a disabled
                    # picker's option list is routinely empty for the very
                    # reason it is disabled, and "not offered" would send the
                    # reader hunting an option list that was never the problem.
                    if _found_disabled(loc):
                        self._respond(409, _disabled_actuation_refusal(
                            "select", req["id"], req.get("index", 0)))
                        return
                    self._respond(409, {"error": (
                        f"select target {req['value']!r} is not offered by "
                        f"{req['id']!r} — this render painted "
                        f"[{', '.join(map(str, offered))}]"
                    )})
                    return
                _actuate(self, "select", req, loc,
                         lambda: loc.select_option(value=req["value"]))

            elif route == "/element/set_input_files":
                req = self._read_json()
                page = self._require_page(req)
                root = _scoped_root(page, self._parse_scope(req))
                files = _file_payloads(req["files"])
                root.locator(_selector(req["id"])).nth(req.get("index", 0)).set_input_files(files)
                self._respond(200, {})

            elif route == "/app/commands":
                req = self._read_json()
                _command_queue.append(req)
                self._respond(201, {"queued": True})

            elif route == "/app/state":
                req = self._read_json()
                _cached_app_state[str(req.pop("page", None) or "1")] = req
                self._respond(200, {"received": True})

            else:
                self._respond(404, {"error": f"Unknown route: POST {route}"})

        except RuntimeError as e:
            self._respond(500, {"error": str(e)})
        except Exception as e:
            code = 404 if "could not find" in str(e).lower() else 500
            self._respond(code, {"error": str(e)})

    # --- DELETE routes ---

    def do_DELETE(self):  # noqa: N802
        route = self._route_path()
        if route == "/session":
            self._handle_session_close()
        elif route == "/page":
            self._handle_page_close()
        else:
            self._respond(404, {"error": f"Unknown route: DELETE {route}"})

    # --- Session management ---

    @staticmethod
    def _close_all_pages() -> None:
        global _playwright, _browser
        for page in list(_pages.values()):
            try:
                page.close()
            except Exception:
                pass
        _pages.clear()
        # A borrowed context appears under several page ids; `ctx.close()` is
        # wrapped, so the duplicate close is harmless, but dropping the borrows
        # first keeps the teardown honest about what it owns.
        for pid in list(_borrowed_contexts):
            _contexts.pop(pid, None)
        _borrowed_contexts.clear()
        for ctx in list(_contexts.values()):
            try:
                ctx.close()
            except Exception:
                pass
        _contexts.clear()
        _cached_app_state.clear()
        _console_logs.clear()
        if _browser:
            _browser.close()
            _browser = None
        if _playwright:
            _playwright.stop()
            _playwright = None

    def _handle_session_create(self) -> None:
        global _playwright, _browser, _next_page_id
        req = self._read_json()
        url = req.get("url", "")
        # Optional BCP-47 locale (e.g. "en-GB") — drives `navigator.language`
        # and every `Intl` API a page reads off it, including
        # `Intl.Locale.getWeekInfo` (web's locale week-start probe,
        # `weekStart.ts`). Absent by default so
        # every other caller's context is byte-identical to before this
        # field existed.
        locale = req.get("locale")

        # Close any existing session
        self._close_all_pages()

        _playwright = sync_playwright().start()
        _browser = _playwright.chromium.launch(
            timeout=_SESSION_LAUNCH_TIMEOUT_MS, **launch_kwargs(_playwright)
        )
        # An explicit context (rather than `new_page()`'s implicit one) so the
        # primary page and any /page twins are uniform: one context per page.
        context_kwargs = dict(_CONTEXT_KWARGS)
        if locale:
            context_kwargs["locale"] = locale
        _contexts["1"] = _browser.new_context(**context_kwargs)
        _pages["1"] = _contexts["1"].new_page()
        _console_logs.pop("1", None)  # a fresh session starts a fresh log
        _attach_console_capture(_pages["1"], "1")
        _forget_downloads()
        _attach_download_capture(_pages["1"], "1")
        _next_page_id = 2
        if url:
            _pages["1"].goto(url, timeout=_SESSION_NAV_TIMEOUT_MS)

        self._respond(201, {"session_id": "1"})

    def _handle_page_create(self) -> None:
        """Open an additional page. Body: {"url": ...} (optional initial
        navigation), {"share_context": "<page id>"} (optional). Responds with the
        page id every subsequent request can address via `page`.

        Two genuinely different things, and the distinction is the whole point:

        * **Default — its OWN BrowserContext** (isolated localStorage/IndexedDB):
          a second *device* sharing the one snap-locked Chromium.
        * **`share_context`** — a second **TAB** of an existing page's context,
          so the two pages share one origin store exactly as two real tabs do.
          This is the only shape that can exercise web's concurrent-instances
          leg (`account-scoping.md` § Concurrent instances → *Web*): the tabs
          share `localStorage`, so a per-tab account pin is the mechanism under
          test, and an isolated twin would trivially "pass" by never sharing
          anything in the first place.
        """
        global _next_page_id
        req = self._read_json()
        if _browser is None:
            raise RuntimeError("No active session. POST /session first.")
        pid = str(_next_page_id)
        _next_page_id += 1
        share = str(req.get("share_context") or "")
        if share:
            owner_ctx = _contexts.get(share)
            if owner_ctx is None:
                raise RuntimeError(
                    f"No BrowserContext for page '{share}' to share; "
                    "create that page first."
                )
            _contexts[pid] = owner_ctx
            # The context belongs to `share`, so closing THIS page must not take
            # the owner's store down with it (and `_close_all_pages` would
            # otherwise close the same context twice).
            _borrowed_contexts.add(pid)
        else:
            _contexts[pid] = _browser.new_context(**_CONTEXT_KWARGS)
        _pages[pid] = _contexts[pid].new_page()
        _attach_console_capture(_pages[pid], pid)
        _attach_download_capture(_pages[pid], pid)
        url = req.get("url", "")
        if url:
            _pages[pid].goto(url)
        self._respond(201, {"page_id": pid})

    def _handle_page_close(self) -> None:
        pid = str(self._query().get("page") or "")
        page = _pages.pop(pid, None)
        ctx = _contexts.pop(pid, None)
        borrowed = pid in _borrowed_contexts
        _borrowed_contexts.discard(pid)
        _cached_app_state.pop(pid, None)
        _console_logs.pop(pid, None)
        if page is not None:
            try:
                page.close()
            except Exception:
                pass
        # A borrowed context belongs to the page that created it: closing it here
        # would wipe the owner tab's localStorage mid-test, which for a
        # same-context tab pair is precisely the shared store under test.
        if ctx is not None and not borrowed:
            try:
                ctx.close()
            except Exception:
                pass
        self._respond(200, {"closed": pid})

    def _handle_page_kill(self) -> None:
        """Unclean-kill the app: close the PAGE but KEEP its BrowserContext, so
        the context's localStorage/IndexedDB — the durable client store — survives
        (the web analogue of a native SIGKILL leaving the on-disk data dir intact;
        drivers/base.py PlatformDriver.kill_uncleanly). `page.close()` defaults to
        `run_before_unload=False`: no beforeunload/unload flush runs, so nothing the
        app deferred to unload is gracefully persisted — the genuine crash. The SPA
        writes localStorage synchronously (no unload backup — `wasm.ts` slice 6), so
        whatever was durably written at the kill instant is exactly what survives.

        Requires the context to still exist so a later `/page/relaunch` can reopen a
        page against the SAME store; a caller that killed a page whose context is
        gone gets a loud 500 rather than a vacuous relaunch onto a fresh store."""
        req = self._read_json()
        pid = str(req.get("page") or "1")
        page = _pages.pop(pid, None)
        _cached_app_state.pop(pid, None)
        if page is not None:
            try:
                page.close()  # run_before_unload defaults False → abrupt, no flush
            except Exception:
                pass
            _console_buf(pid).append("[bridge] page killed uncleanly")
        if pid not in _contexts:
            raise RuntimeError(
                f"No preserved BrowserContext for page '{pid}' — cannot simulate a "
                f"crash that keeps the durable store"
            )
        self._respond(200, {"killed": pid})

    def _handle_page_relaunch(self) -> None:
        """Reopen page `pid` in its PRESERVED BrowserContext (kept alive by
        `/page/kill`) and navigate it to `url` — a fresh SPA boot over the SAME
        localStorage/IndexedDB, the web analogue of a native process relaunch
        against the same data dir. Reuses the same page id so the driver keeps
        addressing page "1"."""
        req = self._read_json()
        pid = str(req.get("page") or "1")
        ctx = _contexts.get(pid)
        if ctx is None:
            raise RuntimeError(
                f"No preserved BrowserContext for page '{pid}'; /page/kill it first"
            )
        old = _pages.pop(pid, None)
        if old is not None:
            try:
                old.close()
            except Exception:
                pass
        _pages[pid] = ctx.new_page()
        _console_buf(pid).append("[bridge] page relaunched (fresh boot, same store)")
        _attach_console_capture(_pages[pid], pid)
        _attach_download_capture(_pages[pid], pid)
        url = req.get("url", "")
        if url:
            _pages[pid].goto(url)
        self._respond(200, {"relaunched": pid})

    def _handle_seed_factory_reset(self) -> None:
        """Arrange a STALE pending-factory-reset slot (CR-2) in the killed page's
        preserved BrowserContext localStorage — the web twin of the native
        `seed_pending_factory_reset` seam (drivers/base.py). Writes the ACTIVE
        account's per-actor row (`fauna/{actor}/pending_factory_reset`), the one
        slot the launch adapter reads (the pre-registry global keys are not read
        anywhere since 2026-09-24); a seed that landed elsewhere would be
        invisible and the reconcile-under-test would green vacuously (no slot ≠
        a cleared slot).

        The seeded record's `minted_at_secs` is deliberately 30 minutes in the
        past — twice the machine's mint grace (`FACTORY_RESET_CLAIM_GRACE_SECS`,
        launch-machine persistence.rs), the age its `Claimed` arm reads as STALE,
        which is this seam's whole subject; the native seam
        (`PlatformDriver.STALE_FACTORY_RESET_AGE_SECS`) uses the same age. The
        field is required: a record without it does not parse and reads as NO
        slot, which would make the journey vacuous. It
        also OVERWRITES any fresh slot the client's own mint left behind — the
        overwrite is the seam's time-compression: a real stale slot is one whose
        mint happened long ago, and a seconds-old mint would (correctly, by
        design) be HONORED by the grace arm rather than cleared, arranging the
        wrong scenario for a staleness test.

        localStorage needs a same-origin document, so the write happens on a
        THROWAWAY page in the same context via an `add_init_script` that runs BEFORE
        any SPA script — the slot lands before a boot can read it and cannot be
        clobbered — navigated `wait_until="commit"` (don't wait for the SPA to boot)
        and closed immediately. Page "1" stays dead; `recover()` reopens it for the
        real boot-reconcile pass."""
        req = self._read_json()
        pid = str(req.get("page") or "1")
        ctx = _contexts.get(pid)
        if ctx is None:
            raise RuntimeError(
                f"No preserved BrowserContext for page '{pid}'; /page/kill it first"
            )
        nest_url = json.dumps(req.get("nest_url", ""))
        handle = json.dumps(req.get("handle", ""))
        code = json.dumps(req.get("code", ""))
        seed_js = (
            "try {"
            "  const idx = JSON.parse(localStorage.getItem('fauna/index') || 'null');"
            "  if (idx && idx.active) {"
            "    localStorage.setItem("
            "      'fauna/' + idx.active + '/pending_factory_reset',"
            f"     JSON.stringify({{ nest_url: {nest_url}, handle: {handle}, claim_code: {code},"
            "       minted_at_secs: Math.floor(Date.now() / 1000) - 2 * 15 * 60 })"
            "    );"
            "  }"
            "} catch (e) {}"
        )
        tmp = ctx.new_page()
        try:
            _attach_console_capture(tmp, pid, tag="[seed-page]")
            tmp.add_init_script(seed_js)
            tmp.goto(req.get("url", ""), wait_until="commit")
        finally:
            try:
                tmp.close()
            except Exception:
                pass
        self._respond(200, {"seeded": pid})

    def _handle_session_close(self) -> None:
        self._close_all_pages()
        self._respond(200, {"closed": True})


def _forward_signal_to_group(signum, frame):
    """SIGTERM/SIGINT → take the whole bridge subtree (playwright node +
    Chromium) down, not just this python. The driver spawns us as a process-
    group leader (drivers/port_util.popen_group_kwargs), and PDEATHSIG sends
    us SIGTERM even when pytest is SIGKILLed — this handler is what turns
    that lone signal into a clean subtree exit with no orphan browser."""
    signal.signal(signum, signal.SIG_DFL)
    if os.name == "posix" and os.getpgrp() == os.getpid():
        try:
            os.killpg(0, signum)  # includes ourselves — default action exits
        except OSError:
            pass
    os.kill(os.getpid(), signum)  # non-leader (manual run): plain default exit


def main():
    if os.name == "posix":
        signal.signal(signal.SIGTERM, _forward_signal_to_group)
        signal.signal(signal.SIGINT, _forward_signal_to_group)
    port = int(sys.argv[sys.argv.index("--port") + 1]) if "--port" in sys.argv else randint(18000, 19000)
    server = HTTPServer(("127.0.0.1", port), BridgeHandler)
    print(f"BRIDGE_PORT={port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

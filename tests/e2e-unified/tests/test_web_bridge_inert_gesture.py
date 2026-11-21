"""The web bridge refuses a gesture on an element that carries none
(convention 11's structural refusal — `e2e-conventions.md` § Implementation
status today).

Playwright clicks whatever it is pointed at, so a click on an inert `<span>`
used to answer 200 having done nothing: a silently acked command, which is what
convention 11 forbids. tui, linux and apple refuse such a gesture and windows'
FlaUI bridge answers 409 "not actuable"; the web bridge now does the same
(`web-bridge/server.py::_INERT_GESTURE_JS`), and answers a click on an element
that never attached 404 "could not find element" instead of Playwright's bare
500 timeout.

The predicate must never refuse a LIVE element, so the page below carries one
of each shape the SPA uses to make something clickable — a native button, a
Svelte 5 delegated handler (`el.__click`) on a role'd tab stop, a handler a
framework keeps on a bare `<tr>` under a key of its own (the SPA's Svelte 5.57
build keeps `{click: fn}` under a `Symbol(events)` key and none at `__click`, so
its folders table's live `<tr onclick>` once read as inert), a wrapper id whose
centre is a button, a DOM0 `onclick` property — beside the inert shape the SPA paints for a plain
member's `thread-member-chip`: a bare `<span>`.

tier_2: the real bridge and real Chromium against a static page served here; no
nest, no app.
"""

from __future__ import annotations

import http.server
import threading

import pytest

from drivers.web import WebBridgeDriver
from helpers.inert_refusal import is_inert_refusal

pytestmark = [pytest.mark.tier_2, pytest.mark.web]

_PAGE = b"""<!doctype html><html><head><title>inert gesture</title></head><body>
<div id="log"></div>
<span data-testid="inert-chip" class="chip">Plain member</span>
<span data-testid="svelte-inert-chip" class="chip">Plain member, as the SPA paints it</span>
<span data-testid="svelte-wrapped-chip" class="chip">Conditional handler, undefined</span>
<span data-testid="delegated-chip" role="button" tabindex="0">Removable member</span>
<div data-testid="wrapper" style="display:inline-block"><button>inside</button></div>
<div data-testid="wide-wrapper"><button>beside the centre</button></div>
<div data-testid="dom0">dom0 handler</div>
<div data-testid="hoisted">hoisted handler</div>
<table><tbody><tr data-testid="symbol-row"><td>a row whose handler sits under a symbol</td></tr></tbody></table>
<button data-testid="native">native</button>
<a data-testid="inert-child-of-link" href="#x"><span data-testid="link-label">label</span></a>
<script>
  const log = (what) => { document.getElementById('log').textContent += what + ';'; };
  // Svelte 5's delegated-event shape: the handler is a property on the
  // element, dispatched by one root listener.
  const chip = document.querySelector('[data-testid="delegated-chip"]');
  chip.__click = () => log('delegated');
  // A hoisted handler with arguments is stored as `[fn, ...args]`.
  document.querySelector('[data-testid="hoisted"]').__click = [(e, what) => log(what), 'hoisted'];
  // The SPA's Svelte 5.57 build keeps a delegated handler under a symbol
  // key, as `{click: fn}` (measured on the built SPA: `Symbol(events)`), and
  // nothing at `__click`. A bare `<tr>` with no role or tab stop is the shape
  // the folders table paints.
  const EVENTS = Symbol('events');
  document.querySelector('[data-testid="symbol-row"]')[EVENTS] = { click: () => log('symbol') };
  // The plain member's chip as the SPA's Svelte 5.57 build really leaves it
  // (measured in the browser, 2026-11-19): own symbol keys for the attributes
  // Svelte applied (`role` and `tabindex` among them, undefined) and the class
  // - and NO events object. It must read as inert.
  const inertChip = document.querySelector('[data-testid="svelte-inert-chip"]');
  inertChip[Symbol('attributes')] = { 'data-testid': 'svelte-inert-chip', role: undefined, tabindex: undefined };
  inertChip[Symbol('class')] = 'chip';
  // What the same chip carried while it was one span with
  // `onclick={removable ? fn : undefined}`: Svelte hangs a dispatching wrapper
  // under `Symbol(events)` whatever the condition says, so the element CARRIES
  // a click handler that does nothing. No predicate can see through that, so
  // it reads as live - which is why the SPA renders an inert element with no
  // handler at all rather than an undefined one.
  document.querySelector('[data-testid="svelte-wrapped-chip"]')[EVENTS] = {
    click: () => log('wrapped-noop'),
    keydown: () => {},
  };
  document.addEventListener('click', (e) => {
    for (let n = e.target; n; n = n.parentElement) {
      const m = n[EVENTS];
      if (m && typeof m.click === 'function') { m.click(e); return; }
      const h = n.__click;
      if (typeof h === 'function') { h(e); return; }
      if (Array.isArray(h)) { h[0](e, ...h.slice(1)); return; }
    }
  });
  document.querySelector('[data-testid="dom0"]').onclick = () => log('dom0');
  document.querySelector('[data-testid="native"]').addEventListener('click', () => log('native'));
  document.querySelector('[data-testid="wrapper"] button').addEventListener('click', () => log('wrapped'));
</script>
</body></html>"""


class _StaticPage(http.server.BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(_PAGE)))
        self.end_headers()
        self.wfile.write(_PAGE)

    def log_message(self, *args):
        pass


@pytest.fixture(scope="module")
def bridge_on_static_page():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _StaticPage)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    drv = WebBridgeDriver()
    try:
        drv.launch({"url": f"http://127.0.0.1:{server.server_address[1]}/"})
        yield drv
    finally:
        try:
            drv._stop_app()
        finally:
            server.shutdown()
            server.server_close()


def _log(drv) -> str:
    return drv._execute_js("document.getElementById('log').textContent")


def test_a_click_on_an_inert_span_is_refused_not_actuable(bridge_on_static_page):
    drv = bridge_on_static_page
    with pytest.raises(RuntimeError) as refused:
        drv.click("inert-chip")
    assert "(409)" in str(refused.value) and is_inert_refusal(refused.value), refused.value


def test_the_spa_s_inert_member_chip_shape_is_refused(bridge_on_static_page):
    # Svelte's own attribute and class bookkeeping on the element is not a
    # handler: the plain member's chip as the built SPA leaves it is inert.
    with pytest.raises(RuntimeError) as refused:
        bridge_on_static_page.click("svelte-inert-chip")
    assert "(409)" in str(refused.value) and is_inert_refusal(refused.value), refused.value


def test_a_double_click_on_an_inert_span_is_refused_too(bridge_on_static_page):
    drv = bridge_on_static_page
    with pytest.raises(RuntimeError) as refused:
        drv.double_click("inert-chip")
    assert "(409)" in str(refused.value) and is_inert_refusal(refused.value), refused.value


@pytest.mark.parametrize(
    ("element_id", "witness"),
    [
        ("delegated-chip", "delegated;"),
        ("dom0", "dom0;"),
        ("hoisted", "hoisted;"),
        ("symbol-row", "symbol;"),
        # The predicate's known limit, pinned so nobody mistakes it for a
        # refusal it makes: a framework wrapper reads as live.
        ("svelte-wrapped-chip", "wrapped-noop;"),
        ("native", "native;"),
        ("wrapper", "wrapped;"),
    ],
)
def test_a_live_element_is_clicked_and_its_handler_runs(
    bridge_on_static_page, element_id, witness
):
    drv = bridge_on_static_page
    drv._execute_js("document.getElementById('log').textContent = ''")
    drv.click(element_id)
    assert _log(drv) == witness


def test_a_wide_wrapper_whose_centre_misses_its_button_is_refused(bridge_on_static_page):
    # Playwright clicks the target's centre. A full-width wrapper's centre is
    # empty space beside its button, so the click would reach nothing: the
    # refusal is the honest answer, not a false one.
    with pytest.raises(RuntimeError) as refused:
        bridge_on_static_page.click("wide-wrapper")
    assert is_inert_refusal(refused.value), refused.value


def test_an_inert_child_of_a_link_is_live(bridge_on_static_page):
    # The ancestor walk: the label carries nothing, the link around it does.
    bridge_on_static_page.click("link-label")


def test_a_click_on_a_never_attached_element_answers_not_found(bridge_on_static_page):
    # Waits out Playwright's own actionability window (its 30 s default) — the
    # bridge answers only once that wait has given up, as it always has.
    with pytest.raises(LookupError, match="could not find element"):
        bridge_on_static_page.click("never-attached")

"""The web driver's readiness probes survive the SPA navigating mid-probe
(convention 14, `docs/goal/architecture/e2e-latency-independent-assertions.md`).

The red this pins: a web test erroring in SETUP, before its body ran, with
``Page.evaluate: Execution context was destroyed, most likely because of a
navigation`` — raised from the driver's readiness probe
(``drivers/web.py::_ensure_agent`` / ``_inject_agent``) when the page happened
to be navigating as the probe ran. Whether it fired depended only on where the
navigation fell relative to the probe, so it hit a different test each run.

The fix has two halves, and each is witnessed here:

* the bridge's ``/execute`` takes ``across_navigation``: a context destroyed
  by a navigation is waited out (the new document's ``load``) and the script
  re-run, inside a budget the caller names. Only the driver's readiness
  probes ask for it — a body action's ``/execute`` still surfaces the error.
* the driver's ``reset()`` — the evaluate whose own action ends in a hard
  navigation — asks for ``navigates``: the context that navigation destroys
  under the evaluate is answered as done, not raised. (The 2026-10-14
  whole-suite run's four setup errors were all this call.)

The tier_1 tests drive the bridge helper and the driver against stand-ins; the
tier_2 witness runs the real bridge and real Chromium against a two-document
static page, and forces the navigation INSIDE the probe — deterministic, no
timing involved.
"""

from __future__ import annotations

import http.server
import importlib.util
import pathlib
import threading

import pytest

from drivers.web import HEAVY_BOOT_TIMEOUT_S, WebBridgeDriver

_DESTROYED = (
    "Page.evaluate: Execution context was destroyed, most likely because of a "
    "navigation"
)


def _bridge():
    server_path = (
        pathlib.Path(__file__).resolve().parents[1] / "web-bridge" / "server.py"
    )
    spec = importlib.util.spec_from_file_location("fauna_e2e_web_bridge_probe", server_path)
    bridge = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bridge)
    return bridge


class _NavigatingPage:
    """A Playwright page stand-in: the first ``destroyed`` evaluates die as a
    navigation would kill them, then the script answers."""

    def __init__(self, destroyed: int = 1, error: str = _DESTROYED):
        self._left = destroyed
        self._error = error
        self.evaluates = 0
        self.load_waits: list[tuple[str, float]] = []

    def evaluate(self, script, arg=None):
        self.evaluates += 1
        if self._left:
            self._left -= 1
            raise Exception(self._error)
        return "answered"

    def wait_for_load_state(self, state="load", timeout=None):
        self.load_waits.append((state, timeout))


# --- tier_1: the bridge helper ---------------------------------------------


@pytest.mark.tier_1
def test_a_destroyed_context_is_waited_out_and_the_probe_re_run():
    page = _NavigatingPage(destroyed=2)
    assert _bridge().evaluate_across_navigation(page, "1", None, 60.0) == "answered"
    assert page.evaluates == 3
    assert [state for state, _ in page.load_waits] == ["load", "load"], (
        "each destroyed context must wait for the new document's load before "
        "the re-run, never a fixed sleep"
    )


@pytest.mark.tier_1
def test_any_other_evaluate_error_is_raised_untouched():
    page = _NavigatingPage(error="ReferenceError: nope is not defined")
    with pytest.raises(Exception, match="nope is not defined"):
        _bridge().evaluate_across_navigation(page, "nope", None, 60.0)
    assert page.evaluates == 1 and page.load_waits == []


@pytest.mark.tier_1
def test_a_page_that_never_stops_navigating_fails_loudly_at_the_budget():
    page = _NavigatingPage(destroyed=10**9)
    with pytest.raises(RuntimeError, match="Execution context was destroyed.*still navigating"):
        _bridge().evaluate_across_navigation(page, "1", None, 0.0)


# --- tier_1: which driver calls ask for it ---------------------------------


class _RecordingDriver(WebBridgeDriver):
    """The real driver with the bridge HTTP replaced by a recorder."""

    def __init__(self, answers):
        super().__init__()
        self._agent_code = "window.__faunaTestAgent = {}"
        self._answers = list(answers)
        self.posts: list[tuple[str, dict, float | None]] = []

    def _post(self, path, data=None, timeout=None):
        self.posts.append((path, dict(data or {}), timeout))
        return {"result": self._answers.pop(0) if self._answers else True}


@pytest.mark.tier_1
def test_the_readiness_probes_ask_for_navigation_tolerance():
    drv = _RecordingDriver(answers=[False, None, True])
    drv._ensure_agent()
    executes = [body for path, body, _ in drv.posts if path == "/execute"]
    assert len(executes) == 3, executes  # probe, inject, ready-probe
    for body in executes:
        assert body.get("across_navigation") is True, body
        assert 0 < body["navigation_budget_s"] < HEAVY_BOOT_TIMEOUT_S, (
            "the retry budget must end before the call's own socket ceiling, "
            "so an exhausted budget answers with its own message"
        )


@pytest.mark.tier_1
def test_a_body_execute_still_surfaces_a_destroyed_context():
    drv = _RecordingDriver(answers=[1])
    drv.eval_js("1")
    (_, body, _), = drv.posts
    assert "across_navigation" not in body, (
        "a test body's own evaluate must still see the navigation — only the "
        "readiness probes may wait one out"
    )


@pytest.mark.tier_1
def test_reset_asks_for_its_own_navigation_to_be_answered_as_done():
    drv = _RecordingDriver(answers=[True, True, None, True, True, "{}"])
    drv.reset()
    (body,) = [b for p, b, _ in drv.posts
               if p == "/execute" and "__action: 'reset'" in str(b.get("script"))]
    assert body.get("navigates") is True, body


# --- tier_2: real bridge + real Chromium -----------------------------------

# Two documents at one URL: the probe below reloads the page from INSIDE its
# own evaluate on the first document (the flag lives in sessionStorage, which
# survives the reload), so the evaluate's context is destroyed by a navigation
# every time — and answers on the second document. No timing is involved.
_SELF_NAVIGATING_PROBE = """
new Promise((resolve) => {
  if (!sessionStorage.getItem('probe-navigated')) {
    sessionStorage.setItem('probe-navigated', '1');
    location.reload();
    return;
  }
  resolve(document.title);
})
"""

_PAGE = b"<!doctype html><html><head><title>second document</title></head><body></body></html>"


class _StaticPage(http.server.BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(_PAGE)))
        self.end_headers()
        self.wfile.write(_PAGE)

    def log_message(self, *args):
        pass


@pytest.fixture
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


def _reset_flag(drv):
    drv._execute_js("sessionStorage.removeItem('probe-navigated')")


@pytest.mark.tier_2
@pytest.mark.web
def test_a_probe_whose_page_navigates_mid_evaluate_answers_on_the_new_document(
    bridge_on_static_page,
):
    drv = bridge_on_static_page
    _reset_flag(drv)
    # The bridge request exactly as `WebBridgeDriver._probe_js` sends it (the
    # tier_1 test above pins that the probes send it): a bridge without the
    # tolerance answers 500 "Execution context was destroyed" here.
    resp = drv._post(
        "/execute",
        {"script": _SELF_NAVIGATING_PROBE, "arg": None,
         "across_navigation": True, "navigation_budget_s": 60},
        timeout=HEAVY_BOOT_TIMEOUT_S,
    )
    assert resp.get("result") == "second document"


@pytest.mark.tier_2
@pytest.mark.web
def test_a_body_evaluate_across_the_same_navigation_still_errors(bridge_on_static_page):
    drv = bridge_on_static_page
    _reset_flag(drv)
    with pytest.raises(RuntimeError, match="Execution context was destroyed"):
        drv._execute_js(_SELF_NAVIGATING_PROBE)


@pytest.mark.tier_2
@pytest.mark.web
def test_an_action_whose_navigation_destroys_its_context_is_answered_as_done(
    bridge_on_static_page,
):
    # `reset()`'s `applyPatch({__action: 'reset'})` ends in `location.href = …`
    # inside the promise the evaluate returns, so the navigation can commit
    # before the result reaches Playwright — the 2026-10-14 whole-suite run's
    # four setup errors were all this call. This action never resolves, so its
    # context dies under the evaluate on every run (red before: 500 "Execution
    # context was destroyed").
    drv = bridge_on_static_page
    resp = drv._post(
        "/execute",
        {"script": "new Promise(() => { location.href = location.pathname + '?after'; })",
         "arg": None, "navigates": True},
        timeout=HEAVY_BOOT_TIMEOUT_S,
    )
    assert resp.get("result") is None
    assert drv._probe_js("location.search") == "?after"

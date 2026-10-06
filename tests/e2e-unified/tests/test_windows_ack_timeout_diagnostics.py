r"""tier_1: a windows ack timeout says whether the app is still ALIVE.

Convention 6 (`docs/goal/architecture/e2e-conventions.md`): a failure must
diagnose itself. ``HttpBridgeDriver.ack_timeout_diagnostics`` is that hook for
the single least diagnosable failure this harness makes — a command that never
acks, where the driver knows only "no ack" — and it was overridden **only on
linux**. On windows it returned ``""``.

That silence cost a live-provisioning session its whole diagnosis
(): the app process was *dying* a few seconds into a
real Hetzner run, and the only thing the harness said was ``App did not
acknowledge call_machine_method provisioning_snapshot within 60.0s`` — a message
that reads exactly like a wedge, and was chased as one across two paid boxes.

The evidence was already in memory the whole time. The windows app is started by
the FlaUI bridge with ``UseShellExecute=false`` and **no redirect of its own**
(`flaui-bridge/SessionManager.cs::Launch`), so it INHERITS the bridge's stdout and
stderr — and the driver drains those into ``_bridge_log`` (`drain_pipes`). A Rust
panic crossing the UniFFI boundary, which aborts the process, prints there. But
the only reader, ``_bridge_lines()``, filters to ``[bridge]``-prefixed lines,
which is precisely the half the *app* does not write.

These tests pin the two facts that turn that failure into a self-diagnosing one:
the liveness/exit-code verdict, and the app's own inherited stderr tail.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import windows as win  # noqa: E402

pytestmark = [pytest.mark.tier_1, pytest.mark.windows]

# .NET fail-fast / a Rust `abort()` on Windows both surface as this NTSTATUS.
STATUS_STACK_BUFFER_OVERRUN = -1073740791  # 0xC0000409 as a signed int32


def _driver(monkeypatch, *, status=None, status_raises=False, log=()):
    """A `WindowsBridgeDriver` with only its outward calls replaced.

    Never launched: `ack_timeout_diagnostics` is reached on a failure path, so it
    must stand on its own against whatever state the driver happens to be in.
    """
    d = win.WindowsBridgeDriver()

    def _get(path, *a, **k):
        if status_raises:
            raise ConnectionError("bridge is gone")
        return dict(status or {})

    monkeypatch.setattr(d, "_get", _get)
    d._bridge_log = list(log)
    return d


def test_names_a_dead_app_and_its_exit_code(monkeypatch):
    """The regression: a crashed app must not read as a wedge.

    This is the exact shape row 155 measured — every thread stopping at once,
    including an independent watchdog, with no exception on any rail.
    """
    d = _driver(
        monkeypatch,
        status={"running": False, "exit_code": STATUS_STACK_BUFFER_OVERRUN},
    )

    out = d.ack_timeout_diagnostics()

    assert out, "windows still returns the base's empty diagnostics"
    assert "DEAD" in out.upper(), f"the verdict did not say the app had died: {out!r}"
    assert "0xC0000409" in out, (
        "the exit code must appear in the hex form a reader can look up; "
        f"got: {out!r}"
    )


def test_carries_the_apps_own_inherited_stderr(monkeypatch):
    """The panic line is in `_bridge_log` — it must reach the message.

    Deliberately tagged `[err]` with no `[bridge]` prefix: that is how the app's
    inherited native tracing actually appears, and filtering it out is the bug.
    """
    panic = (
        "[err] thread 'tokio-runtime-worker' panicked at "
        "libs/fauna-onboarding-machine/src/machine.rs:7076:13:"
    )
    d = _driver(
        monkeypatch,
        status={"running": False, "exit_code": STATUS_STACK_BUFFER_OVERRUN},
        log=[
            "[bridge] launch epoch=3 pid=1234",
            panic,
            "[err] note: run with RUST_BACKTRACE=1",
        ],
    )

    out = d.ack_timeout_diagnostics()

    assert "panicked at" in out, (
        "the app's inherited stderr never reached the timeout message — this is "
        f"the `[bridge]`-only filter that hid the crash: {out!r}"
    )
    assert "machine.rs:7076" in out, "the panic's own location was dropped"


def test_a_clean_exit_is_called_a_QUIT_not_a_crash(monkeypatch):
    """Exit code 0 is the one verdict a reader will otherwise misread.

    An app that tore itself down on purpose — a wizard terminal reaching
    ``window.close()`` — stops every thread at once and writes **no** WER
    record, which is exactly what a crash looks like from outside. linux hit
    precisely this at the `LoggedIn` terminal on the paid live provisioning
    e2e, where three runs were
    spent before anyone suspected a deliberate quit. Say the word "quit" so the
    next reader does not spend those runs again.
    """
    d = _driver(monkeypatch, status={"running": False, "exit_code": 0})

    out = d.ack_timeout_diagnostics()

    assert "QUIT" in out.upper(), (
        f"a clean exit must be named a quit, not left as a bare code: {out!r}"
    )


def test_reports_an_alive_app_so_a_real_wedge_still_reads_as_one(monkeypatch):
    """The other half of the verdict: not every ack timeout is a crash."""
    d = _driver(monkeypatch, status={"running": True, "exit_code": None})

    out = d.ack_timeout_diagnostics()

    assert "ALIVE" in out.upper(), f"a surviving app must be named as such: {out!r}"


def test_survives_a_bridge_that_cannot_answer(monkeypatch):
    """It runs *inside* a raise. It may never raise, or it masks the failure."""
    d = _driver(monkeypatch, status_raises=True, log=["[err] something"])

    out = d.ack_timeout_diagnostics()  # must not raise

    assert isinstance(out, str)
    assert "UNKNOWN" in out.upper(), (
        f"an unreachable bridge must be reported, not silently empty: {out!r}"
    )


# ── the app's own fatal report, kept whole ────────────────────────────────────
#
# `App.xaml.cs` installs three unhandled-exception hooks that write a header line
# plus the exception's entire `ToString()` to stderr — the one channel that
# outlives the process. That report was being CAPTURED and then filtered away,
# because every reader of the buffer matched `[bridge]` alone. Measured
# 2026-09-01: FaunaApp died 0xC000027B in Microsoft.UI.Xaml.dll during
# `test_backups.py`, its own report sat in the buffer, and the only thing any log
# ever showed was a bridge timeout 600s later.


def test_a_fatal_report_survives_the_bridge_filter_with_its_trace(monkeypatch):
    """The header AND the untagged stack-trace lines beneath it must come back.

    The continuation lines of a .NET `ToString()` carry no prefix at all, so a
    per-line prefix match keeps the headline and discards the only part that says
    where the app died.
    """
    d = _driver(
        monkeypatch,
        status={"running": False, "exit_code": STATUS_STACK_BUFFER_OVERRUN},
        log=[
            "[bridge] launch epoch=1 pid=2404",
            "[err] [fauna] FATAL (AppDomain) — Object reference not set.",
            "[err] System.NullReferenceException: Object reference not set",
            "[err]    at FaunaApp.Views.BackupsPage.RenderDestinations()",
            "[err]    at FaunaApp.Views.BackupsPage.Page_Loaded(Object, RoutedEventArgs)",
            "[err] 2026-09-01T16:03:36.079671Z  WARN fauna_client: an ordinary line",
        ],
    )

    out = d._bridge_lines()

    assert "[fauna] FATAL" in out, f"the fatal header was filtered out: {out!r}"
    assert "NullReferenceException" in out, (
        "the exception type was dropped — a header with no trace does not say "
        f"where the app died: {out!r}"
    )
    assert "BackupsPage.RenderDestinations" in out, (
        f"the frame naming the faulting method was dropped: {out!r}"
    )


def test_the_fatal_trace_stops_at_ordinary_logging(monkeypatch):
    """Stickiness must END, or one crash swallows the rest of the buffer."""
    d = _driver(
        monkeypatch,
        status={"running": False, "exit_code": STATUS_STACK_BUFFER_OVERRUN},
        log=[
            "[err] [fauna] FATAL (UI thread) — boom",
            "[err]    at FaunaApp.Views.BackupsPage.Page_Loaded()",
            "[err] 2026-09-01T16:03:36.079671Z  INFO fauna_client: ordinary line",
            "[err] 2026-09-01T16:03:37.000000Z  INFO fauna_client: another line",
        ],
    )

    out = d._bridge_lines()

    assert "ordinary line" not in out, (
        "the fatal block never closed, so ordinary logging was swept in with it "
        f"— the trace boundary is not being honoured: {out!r}"
    )


# ── the shared-Rust half of the same failure ──────────────────────────────────
#
# ``_bridge_log`` carries what the app printed to the inherited handles. It does
# NOT carry ``tracing``: ``FaunaFfiMethods.InstallLogging`` files that under
# ``<data_dir>/logs/fauna.log.<date>``, a different sink entirely — and one this
# driver already knows how to read (``app_log_text``, added for exactly this
# question). So every decision the shared core narrates — which probe client the
# Online poll built, which attempt it is on — was reachable by hand and
# unreachable from the failure message.
#
# That gap is what a paid box keeps paying for. A live provisioning budget that
# simply expires calls this hook and nothing else
# (``helpers/live_provision.py::await_provisioning``), and the three readings that
# separate "the TLS builder failed" from "the future is not being polled" are all
# ``tracing`` lines.


def _driver_with_app_log(monkeypatch, text, **kwargs):
    d = _driver(monkeypatch, **kwargs)
    monkeypatch.setattr(d, "app_log_text", lambda: text)
    return d


def test_carries_the_shared_rust_tracing_log(monkeypatch):
    """``tracing`` lands in a file, not on stderr — and the file is the diagnosis."""
    d = _driver_with_app_log(
        monkeypatch,
        "2026-09-04T09:00:00.000000Z  INFO fauna_provisioning: attempt 2/480 starting\n",
        status={"running": True, "exit_code": None},
    )

    out = d.ack_timeout_diagnostics()

    assert "attempt 2/480" in out, (
        "the shared core's own narration never reached the timeout message, so a "
        f"paid live run still cannot say what the poll was doing: {out!r}"
    )
    assert "fauna.log" in out, (
        f"the section must name the sink it read, or a reader cannot find more: {out!r}"
    )


def test_the_one_time_probe_decision_survives_a_long_log(monkeypatch):
    """A tail alone loses it: the probe client is built ONCE, then N attempts run.

    ``machine.rs::nest_probe_client{,_resolving}`` narrate their choice a single
    time, before the retry loop; ``run_step`` then writes one line per attempt, up
    to 480 of them. So the single line saying *which client is dialling* is the
    first thing to scroll out of any fixed tail — and it is the line that
    separates a failed TLS builder from a future nobody is polling.
    """
    head = (
        "2026-09-04T09:00:00.000000Z ERROR fauna_onboarding: nest probe client "
        "build failed (unknown TLS backend): falling back to a client with NO "
        "provisional-TLS acceptance\n"
    )
    filler = "".join(
        f"2026-09-04T09:00:{i % 60:02d}.000000Z  INFO fauna_provisioning: "
        f"attempt {i}/480 starting\n"
        for i in range(1, 400)
    )
    d = _driver_with_app_log(
        monkeypatch, head + filler, status={"running": True, "exit_code": None}
    )

    out = d.ack_timeout_diagnostics()

    assert "nest probe client build failed" in out, (
        "the one-time probe-client decision scrolled out of the tail — the "
        f"reading that names the cause is exactly the one that gets lost: {out!r}"
    )
    assert "attempt 399/480" in out, (
        f"the tail must still carry the most recent attempt: {out!r}"
    )


def test_an_unreadable_app_log_changes_nothing_else(monkeypatch):
    """It runs inside a ``raise``; a log read that throws may not mask the failure."""

    def _boom():
        raise OSError("the data dir went away with the app")

    d = _driver(monkeypatch, status={"running": True, "exit_code": None})
    monkeypatch.setattr(d, "app_log_text", _boom)

    out = d.ack_timeout_diagnostics()  # must not raise

    assert "ALIVE" in out.upper(), (
        f"a failing log read swallowed the liveness verdict as well: {out!r}"
    )


# ── the app dies, and the bridge answers PROMPTLY ─────────────────────────────
#
# Everything above hangs off ``ack_timeout_diagnostics`` — the hook for a command
# that never acks. But the bridge got BETTER at this: ``SessionManager`` now
# notices a dead app and refuses the next UIA query outright
# (``AppExitedException``) instead of waiting out a budget the corpse will never
# satisfy. That turns the failure into an ordinary bridge 500 — a path that calls
# no diagnostics hook at all.
#
# So the crash this whole instrument was built for reports the BRIDGE's stack and
# discards the APP's. Measured 2026-09-09 on Windows:
# `test_backups.py` died 0xC000027B, the app's own `[fauna] FATAL` header was
# captured, and the trace beneath it — plus every `[fauna] FIRST-CHANCE` block,
# the ONLY sighting of an exception the WinRT ABI stows — sat unread in
# ``_bridge_log`` while the failure message quoted seven frames of FauiBridge.
#
# The fix is the same convention 6 move, one layer out: a bridge error that says
# the app has exited must carry the app's own last words.


def _app_exited_error(pid: int = 21820, code: int = -1073741189) -> str:
    """The bridge's own wording, verbatim from the measured run."""
    return (
        f"FauiBridge.AppExitedException: resolving the main window: the app under "
        f"test (pid {pid}) has exited with code {code} (0xC000027B) — no UIA query "
        f"about it can be answered.\n"
        f"   at FauiBridge.SessionManager.RefuseIfAppExited(String what)"
    )


def test_an_app_exit_bridge_error_carries_the_apps_own_last_words(monkeypatch):
    """The regression: the bridge's stack is not the app's stack.

    A 500 that says the app is gone is the most diagnosable failure this harness
    produces — the app printed why on its way out — and it was the one that said
    least.
    """
    d = _driver(
        monkeypatch,
        status={"running": False, "exit_code": -1073741189},
        log=[
            "[bridge] launch epoch=1 pid=21820",
            "[err] [fauna] FIRST-CHANCE System.NullReferenceException: boom",
            "[err]    at FaunaApp.Views.BackupsPage.RenderDestinations()",
            "[err] [fauna] FATAL (UI thread) — Catastrophic failure",
        ],
    )

    out = d.bridge_error_diagnostics(_app_exited_error())

    assert out, "an app-exit bridge error still adds nothing to the failure"
    assert "FIRST-CHANCE" in out, (
        "the first-chance block is the ONLY sighting of a stowed exception, and "
        f"it was dropped: {out!r}"
    )
    assert "BackupsPage.RenderDestinations" in out, (
        f"the frame naming the faulting method never reached the message: {out!r}"
    )


def test_an_ordinary_bridge_error_stays_quiet(monkeypatch):
    """Stickiness must not become noise: only an app EXIT earns the dump."""
    d = _driver(
        monkeypatch,
        status={"running": True, "exit_code": None},
        log=["[err] [fauna] FATAL (UI thread) — an OLD crash, already recovered"],
    )

    out = d.bridge_error_diagnostics(
        "FauiBridge.ScopeResolutionException: scope 'post-card[2]' matched nothing"
    )

    assert out == "", (
        f"an ordinary bridge error dumped the whole buffer at the reader: {out!r}"
    )


def test_the_app_exit_dump_never_raises(monkeypatch):
    """It runs inside a ``raise``, exactly like ``ack_timeout_diagnostics``."""
    d = _driver(monkeypatch, status_raises=True)
    del d._bridge_log  # a driver that never launched has no buffer at all

    out = d.bridge_error_diagnostics(_app_exited_error())  # must not raise

    assert isinstance(out, str)


def test_the_get_path_actually_appends_it(monkeypatch):
    """The flow, not the helper: a real ``_get`` 500 must carry the dump.

    The hook existing is not the fix — ``_get``/``_post`` raising with it is.
    """
    import io
    import json
    import urllib.error

    from drivers import http_bridge

    # NOT `_driver()`: that stubs `_get` itself, which is the very method under
    # test here. Only the two outward edges are replaced — the socket and the
    # bridge-liveness precondition.
    d = win.WindowsBridgeDriver()
    d._bridge_log = ["[err] [fauna] FIRST-CHANCE System.NullReferenceException: boom"]
    d._url = "http://127.0.0.1:1"
    monkeypatch.setattr(d, "_check_bridge", lambda: None)

    body = json.dumps({"error": _app_exited_error()}).encode()

    def _raise(*a, **k):
        raise urllib.error.HTTPError(
            "http://127.0.0.1:1/element/visible", 500, "Internal Server Error",
            {}, io.BytesIO(body),
        )

    monkeypatch.setattr(http_bridge.urllib.request, "urlopen", _raise)

    with pytest.raises(RuntimeError) as caught:
        d._get("/element/visible", {"id": "last-backed-up"})

    assert "FIRST-CHANCE" in str(caught.value), (
        "the driver raised the bridge's error without the app's own last words — "
        f"the crash still does not diagnose itself: {caught.value}"
    )

"""Settings → Logs sub-page — the client's durable log record
(docs/goal/architecture/apps/observability.md § Surfaces).

A Settings rail sub-page that renders the process-global `fauna_log` ring
(`snapshot()`) newest-first with a severity filter, copy-to-clipboard, and a
clear button. The ring captures `tracing` events in-process, so the page works
without any nest round-trip — but the shell only exists in the authenticated UI,
so the test reaches it through `logged_in_app`.

tier_3 (full stack — a real client process whose in-memory ring captures the
client's own `tracing` output, including the `install_logging` startup line).
linux leads; the other five apps lift this shape (priority #1).
"""
import re
import time
import uuid

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("app-log")
def test_logs_page_heading_is_visible(logged_in_app):
    """A real, visible heading must paint the page title (settings.md §
    Sub-page heading conformance). Two accepted shapes fleet-wide: linux /
    windows / tui pin the id directly on the heading label ("own id" —
    `settings-logs` IS the heading, no separate `page-heading`); web / macos
    / ios paint a separate generic `page-heading` Text alongside the
    `settings-logs` landmark (whose own automation value then carries no
    text — `test_logs_page_renders` below covers that landmark's presence,
    this test covers the heading TEXT wherever it lives).

    Guards the class of bug closed 2026-08-22: iOS's
    Logs sub-page painted NO heading at all, in EITHER shape — `LogsView`'s
    `headingId` param was never passed at the iOS call site, so its `if let
    headingId { automationText(...) }` branch never fired.
    """
    app = logged_in_app
    app.logs.navigate()
    assert app.logs.is_page_visible(), (
        f"settings-logs page not reachable. error: {app.error_text()!r}"
    )
    heading = app.driver.get_text("page-heading") if app.driver.is_visible("page-heading") else None
    if not heading:
        heading = app.driver.get_text("settings-logs")
    assert heading == S.logs.title, (
        f"Logs heading should read {S.logs.title!r} (via page-heading or the "
        f"settings-logs landmark itself), got {heading!r}: "
        f"page-heading={app.driver.diagnose('page-heading')} "
        f"settings-logs={app.driver.diagnose('settings-logs')}"
    )


@pytest.mark.feature("app-log")
def test_logs_page_renders(logged_in_app):
    """The Logs page is reachable from the Settings rail and shows captured
    activity (at minimum the client's startup log line)."""
    app = logged_in_app
    app.logs.navigate()
    assert app.logs.is_page_visible(), (
        f"settings-logs page not reachable. error: {app.error_text()!r}"
    )
    assert app.logs.wait_for_min_entries(1, timeout=10.0), (
        "the in-memory ring should hold at least the client's startup log line, "
        f"got {app.logs.entry_count()} entries"
    )


@pytest.mark.feature("app-log")
def test_logs_level_filter_narrows(logged_in_app):
    """Selecting a severity narrows the view to that level and everything more
    severe — `Error` is a subset of `All` (`fauna_log::snapshot_at_least`)."""
    app = logged_in_app
    app.logs.navigate()
    assert app.logs.is_page_visible()
    n_all = app.logs.entry_count()
    assert n_all > 0, "expected captured log entries to filter"

    app.logs.set_level("Error")
    n_err = app.logs.entry_count()
    assert n_err <= n_all, (
        f"Error-only view ({n_err}) must be a subset of All ({n_all})"
    )


@pytest.mark.feature("app-log")
def test_displayed_settings_error_is_captured(logged_in_app):
    """A *displayed* settings error also lands in the ring — producer-side.

    observability.md § What must be logged (category 1) + § Log on the *event*,
    not the *paint*: a reactive `error-message` banner fed by a shared client
    machine logs where the error state is **set** (the machine's error
    transition), so the failure is both shown to the user *and* recorded
    durably on the Logs page. Here the Linked-nests page (the shared
    `LinkedNestsMachine`, libs/fauna-client-pair) is driven into its error
    transition by linking an unreachable nest address; the same string the
    banner shows must then appear in the ring (logged at `fauna_pair`).

    Pre-change, the machine set `snapshot.error` but never logged it, so the
    Logs page would not capture a displayed settings error — this test guards
    that gap.
    """
    app = logged_in_app

    # Start from a clean ring so the captured error is unambiguous.
    app.logs.navigate()
    assert app.logs.is_page_visible()
    app.logs.clear()

    # Trigger a failing settings action: link an unreachable nest address. A
    # non-identity input classifies as a URL (LinkBoth), and the connect
    # failure surfaces in `snapshot.error` → the reactive banner.
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    app.linked_nests.link("http://127.0.0.1:1")
    err = app.linked_nests.page_error_text(timeout=15.0)
    assert err, (
        "linking an unreachable nest should display an error banner, "
        f"got {err!r}"
    )

    # The displayed error is also in the ring (producer-side logging). Match a
    # stable fragment of the banner text against the captured log lines.
    needle = err.strip().splitlines()[0][:60]
    app.logs.navigate()
    assert app.logs.is_page_visible()
    assert app.logs.wait_for_min_entries(1, timeout=10.0)
    assert app.logs.any_entry_contains(needle), (
        f"the displayed settings error {needle!r} should be captured in the "
        f"Logs ring (producer-side logging), but no entry matched. "
        f"entries={app.logs.entry_texts()!r}"
    )


@pytest.mark.feature("app-log")
def test_logs_copy_and_clear(logged_in_app):
    """The copy + clear affordances are present and actuable; clearing empties
    the live in-memory view (the on-disk file is untouched, not asserted here)."""
    app = logged_in_app
    app.logs.navigate()
    assert app.logs.is_page_visible()
    assert app.logs.wait_for_min_entries(1, timeout=10.0)
    n_before = app.logs.entry_count()

    # Copy: the affordance must be present and clickable (clipboard contents
    # aren't readable headlessly — we assert the affordance, not the OS buffer).
    app.logs.copy()

    # Clear wipes the ring → the rendered list drops below its pre-clear size.
    app.logs.clear()
    assert app.logs.entry_count() < n_before, (
        f"clearing should drop the live view below {n_before} entries, "
        f"got {app.logs.entry_count()}"
    )


@pytest.mark.feature("app-log")
def test_displayed_banner_reaches_ring(logged_in_app):
    """Category 1 (observability.md § What must be logged): a message *displayed*
    to the user — here an error banner — also lands in the durable `fauna_log`
    ring, visible on the Settings → Logs page. The visible banner covers the
    active task; the ring is the durable record. On web the `MessageBanner`
    display funnel logs at the producer (the prop change, not the paint) via the
    wasm `logMessage`.

    Web-scoped: the probe injects the banner through the state-protocol
    `messages` field, which only web serializes today (see `test_messages.py`).
    The other shells lift this assertion as they wire their own funnel + the
    `messages` field per their NEXT hand-offs (observability.md § Comprehensive
    capture — Shell surfaces).
    """
    app = logged_in_app
    if not app.driver.is_web():
        skip_unbuilt(
            app.driver,
            surface="the displayed-banner→ring probe's `messages` state field",
            detail="wired on web today; other shells lift it alongside their "
            "own display funnel",
            tracked="observability.md § Comprehensive capture — Shell surfaces",
        )

    sentinel = "fauna-log-banner-probe-7f3c"

    # Land on a page that mounts MessageBanner (feed), then inject an error
    # banner through the same window event the test agent uses
    # (`fauna-message-update`) — that sets the bound `error` prop, firing the
    # funnel's change-detecting `$effect`.
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    time.sleep(0.5)
    app.driver.set_state({"messages": {"error": sentinel}})
    time.sleep(0.5)

    # The ring is a wasm process global — it survives navigation to the Logs page.
    app.logs.navigate()
    assert app.logs.is_page_visible(), (
        f"settings-logs page not reachable. error: {app.error_text()!r}"
    )
    assert app.logs.any_entry_contains(sentinel), (
        "a displayed error banner must also land in the fauna_log ring "
        f"(observability.md category 1); ring rows: {app.logs.entry_texts()!r}"
    )


_ROW_HEAD = re.compile(r"(TRACE|DEBUG|INFO|WARN|ERROR) · \d\d:\d\d:\d\d · (\S+)")


def _ring_rows(app) -> list[tuple[str, str, str]]:
    """The Logs page's rows as ``(level, target, message)``.

    Two row shapes, both from `fauna_log::format`: the one-line `format_line`
    (``level · time · target · message``, tui/linux) and the two-line `LogRow`
    (the message, then its ``level · time · target`` subtitle — web)."""
    rows = []
    for text in app.logs.entry_texts():
        m = _ROW_HEAD.search(text)
        if not m:
            continue
        if m.start() == 0:
            message = text[m.end():].removeprefix(" · ").strip()
        else:
            message = text[: m.start()].strip()
        rows.append((m.group(1), m.group(2), message))
    return rows


@pytest.mark.feature("app-log")
def test_a_copied_notice_reaches_the_ring_at_info(logged_in_app):
    """Category 1 beyond errors (observability.md § What must be logged): the
    "Copied" confirmation a copy button shows is a displayed success line, so
    it also lands in the ring — at `info`, its matching level, not `error`.

    Both existing category-1 witnesses use an error; this is the success-line
    half. The Status page's identity copy button is the probe because every
    app has one: tui confirms on `account-actor-id-copy-btn` (its Status
    landing's copy), the rest on `status-actor-id-copy-btn`. The ring must NOT
    carry the copied id itself — only the displayed line (§ 2's redaction rule).
    """
    app = logged_in_app
    if not (
        app.driver.is_tui() or app.driver.is_linux() or app.driver.is_web()
        or app.driver.is_macos() or app.driver.is_ios() or app.driver.is_windows()
    ):
        skip_unbuilt(
            app.driver,
            surface="the copy-button confirmation's log line",
            detail="tui, linux, web, macos, ios and windows log the 'Copied' "
            "confirmation through their copy funnel; this app's copy "
            "buttons do not log yet",
            tracked="",
        )
    copied_lines = {S.common.copied, S.settings.account_page.copied_clipboard}

    app.logs.navigate()
    assert app.logs.is_page_visible()
    app.logs.clear()

    app.settings._navigate_subpage("status")
    actor_id = app.settings.actor_id()
    button = "account-actor-id-copy-btn" if app.driver.is_tui() else "status-actor-id-copy-btn"
    app.driver.click(button)

    # Leave Settings before re-entering Logs. On web a Status → Logs sub-page
    # switch (component kept mounted) painted NO rows at all, not even the
    # line this assert looks for, while the feed hop remounts and reads the
    # ring fresh — observed 2026-09-24, cause not yet found. This test's subject is the ring, so it hops.
    app.driver.navigate_to("feed")
    app.logs.navigate()
    assert app.logs.is_page_visible()
    matches = wait_until(
        lambda: [r for r in _ring_rows(app) if r[2] in copied_lines],
        10.0,
        diagnose=lambda: (
            f"clicking {button} showed a 'Copied' confirmation, but no ring row "
            f"carries it; rows: {_ring_rows(app)!r}; raw: {app.logs.entry_texts()[:10]!r}"
        ),
    )
    rows = _ring_rows(app)
    assert all(level == "INFO" for level, _, _ in matches), (
        f"a success line logs at INFO, not as a warning or error: {matches!r}"
    )
    # Scoped to the copy funnel's own target(s): an actor id is a public
    # identifier other subsystems log by design (`ws_adapter`'s
    # `authenticated WS connect actor=…`, the MLS engine's path), and on a
    # session app that reconnects between the clear and this read those rows
    # are in the ring too — the 2026-10-06 linux sweep read one as this leak.
    funnel_targets = {target for _, target, _ in matches}
    leaked = [r for r in rows if r[1] in funnel_targets and actor_id in r[2]]
    assert actor_id and not leaked, (
        "the confirmation's log line must carry the displayed text only, never "
        f"the copied value: {leaked!r}"
    )


_ANSI = re.compile(r"\x1b\[[0-9;]*m")
_TRACING_LINE = re.compile(r"^\S+\s+(TRACE|DEBUG|INFO|WARN|ERROR)\s+(.*)$")


#: The per-user sync agent is a separate process that inherits the app's
#: stderr, so its lines land in the same captured file — but it keeps its own
#: ring, not the one the app's Logs page shows.
_OTHER_PROCESS_TARGETS = ("fauna_sync_agent",)


def _app_only_text(stderr: str) -> str:
    """``stderr`` with any `stitch_agent_log`'d DETACHED sync-agent section
    dropped — the app's own printed text alone.

    On windows (and tui on windows) ``app_stderr_text()`` is the app's own
    log, then `drivers.base.AGENT_LOG_BANNER`, then the DETACHED sync agent's
    own separate log file. That agent is its own OS process with its own
    in-memory ring — never the one the Settings → Logs page renders — so its
    printed lines must never be compared against the app's ring as if the
    app itself had printed them. Callers needing a stable before/after offset
    must take `len()` / slice on THIS text, never the raw stitched one: the
    agent's own section can grow between two reads independently of the
    app's, so a byte offset computed on the stitched text does not survive
    to the next read (found chasing a
    `test_what_the_app_printed_or_swallowed_reaches_the_ring[windows]` red —
    see `_printed_events`)."""
    from drivers.base import AGENT_LOG_BANNER

    app_only = stderr.split(f"\n{AGENT_LOG_BANNER}\n", 1)[0]
    return "" if app_only.startswith(f"{AGENT_LOG_BANNER}\n") else app_only


def _drop_agent_lines(stderr: str, agent_log: str) -> str:
    """``stderr`` without the lines an inherited-fd sync agent wrote into it.

    On linux and tui-on-POSIX the agent shares the app's stderr, and the
    shared-crate lines it logs (`fauna_account_plane::account_driver`'s
    "engine singleton held by another process", `fauna_client::
    ws_device_handshake_bearer`'s device-principal client) carry no
    `fauna_sync_agent` target to filter on — the 2026-10-06 linux sweep read
    two of them as app events missing from the ring. The agent writes every
    line to its own rolling log too, but each layer stamps its own time (tens
    of microseconds apart), so a line is the agent's when its text matches an
    agent-log line stamped within :data:`_AGENT_STAMP_SLACK_S` of it."""
    if not agent_log:
        return stderr
    agent: dict[str, list[float]] = {}
    for line in agent_log.splitlines():
        stamped = _split_stamp(line)
        if stamped:
            agent.setdefault(stamped[1], []).append(stamped[0])
    kept = []
    for line in stderr.splitlines():
        stamped = _split_stamp(_ANSI.sub("", line))
        if stamped and any(
            abs(t - stamped[0]) <= _AGENT_STAMP_SLACK_S for t in agent.get(stamped[1], ())
        ):
            continue
        kept.append(line)
    return "\n".join(kept)


_AGENT_STAMP_SLACK_S = 0.05
_STAMP = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z\s+(.*)$")


def _split_stamp(line: str) -> tuple[float, str] | None:
    """``(epoch seconds, rest of line)`` for a tracing-fmt line, else None."""
    m = _STAMP.match(line)
    if not m:
        return None
    from datetime import datetime, timezone

    ts = datetime.fromisoformat(m.group(1)).replace(tzinfo=timezone.utc).timestamp()
    return ts, m.group(2)


def _printed_events(stderr: str) -> list[tuple[str, str]]:
    """``(level, rest-of-line)`` for every `tracing` event the app printed to
    stderr. Continuation lines and anything the app did not print through its
    own logging (a toolkit's C-side warning) do not match and are skipped, as
    are the sync agent's lines (:data:`_OTHER_PROCESS_TARGETS`) — including
    every shared-crate line the agent itself logs (`fauna_client::ws_adapter`,
    `fauna_account_plane::account_driver`, …), which don't start with
    `fauna_sync_agent` and so need the stronger `_app_only_text` cut, not just
    the prefix check. ``stderr`` should already be `_app_only_text`-clean —
    call it on the RAW `app_stderr_text()` before slicing, per that
    function's docstring — but this also self-defends in case a future caller
    forgets."""
    events = []
    for line in _app_only_text(stderr).splitlines():
        m = _TRACING_LINE.match(_ANSI.sub("", line))
        if m and not m.group(2).startswith(_OTHER_PROCESS_TARGETS):
            events.append((m.group(1), m.group(2)))
    return events


@pytest.mark.feature("app-log")
def test_what_the_app_printed_or_swallowed_reaches_the_ring(logged_in_app):
    """Categories 2 and 3 (observability.md § What must be logged): a line the
    app printed to a console the user never sees — including the `warn`/`debug`
    a best-effort path logs beside the failure it swallows — is in the ring too.

    A general invariant rather than one hand-picked site (convention 17): clear
    the ring, provoke a real failure (linking an unreachable nest — a dial that
    fails below the displayed banner), then EVERY event the app printed to its
    stderr since the clear must have a ring row at the same level and target.
    stderr is the console nobody watches; the ring is what the Logs page shows.

    tui, linux, macOS, iOS and windows: the apps whose captured output is the
    shared `fauna_log` subscriber's own record (macOS/iOS `FaunaApp.installLogging`
    mirrors linux's `install_logging()` at launch — same tracing-fmt line
    format; macOS captures it off the launched process's inherited stderr,
    iOS off the same rolling `fauna.log.<date>` file inside the app's own
    sandbox container, windows off the on-disk file its FFI `install_logging`
    writes with the same `fauna_log::init` tracing-fmt layer, all surfaced
    through each driver's `app_stderr_text()`). web's "stderr" is its console,
    a different format.
    """
    app = logged_in_app
    if not (
        app.driver.is_tui() or app.driver.is_linux() or app.driver.is_macos()
        or app.driver.is_ios() or app.driver.is_windows()
    ):
        skip_unbuilt(
            app.driver,
            surface="the printed-vs-ring comparison",
            detail="written against the Rust apps' tracing fmt stderr; this "
            "app's console/print stream needs its own parser",
            tracked="",
        )

    app.logs.navigate()
    assert app.logs.is_page_visible()
    app.logs.clear()
    # `_app_only_text`, not the raw `app_stderr_text()`: the DETACHED sync
    # agent's own stitched-on section grows independently of the app's, so a
    # byte offset into the combined text can land inside — or past — that
    # section on the next read (see `_app_only_text`'s docstring).
    printed_before = len(_app_only_text(app.driver.app_stderr_text()))

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    app.linked_nests.link("http://127.0.0.1:1")
    assert app.linked_nests.page_error_text(timeout=15.0), (
        "linking an unreachable nest should fail visibly"
    )

    printed = _printed_events(_drop_agent_lines(
        _app_only_text(app.driver.app_stderr_text())[printed_before:],
        app.driver.inherited_agent_log_text(),
    ))
    assert printed, (
        "the failed link printed nothing to stderr — the probe provoked no "
        "event to compare"
    )

    app.logs.navigate()
    assert app.logs.is_page_visible()
    rows = _ring_rows(app)
    missing = [
        (level, rest)
        for level, rest in printed
        if not any(r_level == level and r_target in rest for r_level, r_target, _ in rows)
    ]
    assert not missing, (
        f"{len(missing)} of {len(printed)} printed events never reached the "
        f"ring: {missing[:5]!r}; ring rows: {rows[:40]!r}"
    )


@pytest.mark.feature("app-log")
def test_message_text_never_reaches_the_log(logged_in_app):
    """observability.md § 2. Persistence & privacy: the log records what
    happened, never message plaintext.

    Post a feed message carrying a sentinel body, wait for it to land, then the
    sentinel must appear in NEITHER the ring (the Logs page) nor anything the
    app printed (the stderr/console stream, which on the Rust apps is the same
    subscriber's other leg). The ring is cleared first, so the post's whole
    send path — compose, seal, publish, the feed reload — is what is read.
    """
    app = logged_in_app
    sentinel = f"log-redaction-probe-{uuid.uuid4().hex[:12]}"

    app.logs.navigate()
    assert app.logs.is_page_visible()
    app.logs.clear()

    app.feed.navigate()
    app.feed.create_post(f"hello {sentinel}")

    app.logs.navigate()
    assert app.logs.is_page_visible()
    leaked = [t for t in app.logs.entry_texts() if sentinel in t]
    assert not leaked, f"message text reached the Logs page: {leaked!r}"
    printed = app.driver.app_stderr_text()
    assert sentinel not in printed, (
        "message text reached the app's printed log: "
        f"{[line for line in printed.splitlines() if sentinel in line][:3]!r}"
    )

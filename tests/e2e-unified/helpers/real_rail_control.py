"""Did this app actually launch on the REAL FaunaMls rail? — ask its own log.

`conftest.py::real_faunamls_app` opens the real `FaunaMlsBackend` three different
ways, and on **macOS / iOS / android** it is a LAUNCH-TIME gate with no
client-side readiness signal at all: the fixture makes no bridge call for them,
because they implement no `conversations_enable_real_faunamls` command and
publish no `data.conv_real_backend_active` (`actions/conversations.py::
enable_real_faunamls`'s launch-gate branch spells this out). So on those apps a
consuming test can run against the deterministic MOCK backend and **pass while
proving nothing** — the failure mode this module exists to make falsifiable.

**Why the consuming test's own markers cannot answer it.** `real_conversations`
is applied SESSION-WIDE (`conftest.py::_apply_real_conversations_env`), for the
reason `e2e-automation-surface-gating.md` § The e2e trust seed →
*Opt-out: the `no_r14_trust` marker* gives for that sibling opt-out: the app
drivers are session-scoped, so a per-test flag cannot un-seed an app that is
already running. An *unmarked* consumer collected alongside a marked one
therefore gets the real rail anyway, and a *marked* one whose env never reached
the app process does not. Neither markers nor env inspection distinguish those
four combinations. Only the app's own account of its launch does — which is
convention 6's instrument (`e2e-conventions.md` § point 6,
`helpers/app_log_section.py`) pointed at a precondition instead of at a
post-mortem.

**Session-wide is not the same as process-wide, which is why there are two
callers in the conftest.** The env is one thing; the processes it has to reach
are several. `real_faunamls_app` witnesses ALICE, and
`_launch_second_real_faunamls_app` witnesses BOB — a separate launch whose
`ConversationsSession` is built by his own `set_state` login and can fail on its
own, and the process every receive-side assertion in a two-driven-client test is
actually about. Each caller passes a `context` naming its own app, because a red
that cannot say WHICH of two live apps stayed on the mock sends the reader to the
wrong process (convention 6's *diagnose against a witness you HOLD* rider).

**Two stages, because a red must name which half broke.**

* **Stage 1 — the shell's own gate verdict** (apple only). Both apple shells
  stamp `FaunaE2E.realConversationsGateVerdict` immediately before branching
  (`FaunaMacApp.swift`, `Fauna-iOS/App/FaunaApp.swift`), so the verdict lands in
  the log whether or not the branch was taken — a bare `if` writes nothing when
  it is false, and that silence is how a mock session came to look identical to
  a real one. No other shell stamps it, so this stage is apple-conditional: on
  a non-apple launch-gate app its absence is not a finding.
* **Stage 2 — the real session actually started.** `mls-sync:` is written by
  `fauna_client_mls_sync::launcher`, reached only through
  `ConversationsSession::start_receive_loop`, which exists only on the real rail.
  A gate that opened and a session that then never started are different
  defects, and only two stages can tell them apart.

⚠ **The prefix is NATIVE-ONLY, and one of the launcher's three arms is invisible
at the default log level.** `libs/fauna-client-mls-sync/src/launcher.rs` logs arm
1 at `info!` (`mls-sync: cross-device plane wired (N channel(s) restored from
replica)`) and arm 2 at `warn!` (`mls-sync: replica load failed (...); staying
single-device`) — but arm 3 at `debug!` (`mls-sync: session dropped during launch
retry`), below the default `info` filter, so that arm is NOT observable here
without a `RUST_LOG` widening. And the wasm path never emits the prefix at all:
its `RestoreRetryEnd` arms return `JsValue`s (`libs/fauna-wasm/src/
conversations.rs`). So stage 2 reads "the real rail started" on a native shell
and nothing whatever on web — which costs nothing, because web is a runtime
toggle with a real readiness poll of its own (`data.conv_real_backend_active`)
and never reaches this control from the fixture.

⚠ **Does stage 2 prove the line came from THIS launch? Per driver, and the
answer is DECLARED — `drivers/base.py::log_scope_across_relaunch`, pinned per
family by `tests/test_module_relaunch.py`, so the summary below cannot drift
away from the drivers the way its first version did.** `helpers/module_relaunch.py` cold-relaunches the app at
a module boundary (`conftest.py`, before `driver.reset()`), so it is worth
knowing which readers carry the previous launch's words across that:

* **macOS, linux, tui — NO carry-over, by construction.** Each `launch()` mints
  a fresh `mkdtemp` and reopens `app.err` inside it, and the reader returns only
  that file (`drivers/macos.py::app_stderr_text`, "This launch's `app.err`"; its
  `_tmp_dir` is assigned unconditionally in `launch()`, so no pin can reuse it).
  Stage 2 on these families already means "THIS launch started the real rail".
* **iOS — no carry-over normally, carry-over under a container pin.** `launch()`
  uninstalls the app's data container, which wipes the `logs/*` the reader globs
  — unless `preserve_state_across_relaunch()` set `_preserve_container`. The
  module-boundary relaunch runs BEFORE `reset()` clears that pin, so a pin left
  by the previous module's test survives into it. This is the one live gap in
  the control — and it is CLOSED as of 2026-09-22: `launch()` takes a byte
  floor (`drivers/ios.py::_mark_log_baseline`) between `simctl install` and
  `simctl launch`, the one window where the container exists and the app is
  not running, so the reader returns only this launch's words even under a
  pin.
* **windows — genuinely cumulative** (`drivers/windows.py::_app_log_since`
  documents the append-shared data-dir log), **and irrelevant here:** windows
  takes the readiness-poll branch and never reaches this control.
* **android — per-launch, by a byte floor.** The app's `filesDir` (and so its
  `fauna_log` `logs/*`) survives every relaunch and `adb install -r`, so the
  reader (`drivers/android.py::app_log_text`, over the bridge's `GET /app-log`)
  slices from a floor `_mark_log_baseline()` takes before `/session` starts the
  app — the iOS shape. It reads the shared rolling file, not `adb logcat`: the
  Rust tracing output never reaches logcat.

⚠ **An earlier version of this note claimed the readers are "cumulative for the
driver's lifetime" outright.** That was wrong, and wrong in the direction that
invents work: it reads as a hole in every family when three of them close it in
`launch()`. Corrected against the drivers 2026-09-22, and the real gap
(iOS-under-a-pin) closed in the driver rather than here — a cross-cutting
mark/read-since seam on `app_log_section` would have been a new API for one
family, when the other three already close it in `launch()`. Any future
narrowing is deliberately NOT a timestamp heuristic: the
control's whole value is that it cannot be satisfied by something other than the
real rail, and a clock-based window trades that for a wall-clock race
(convention 14).

⚠ **Not `keypackage_count`.** The obvious nest-side control — alice's key
packages going non-zero — is unsound here: `test_user` is session-scoped, so in
a multi-app invocation an earlier app may have published them and the poll
passes without this app ever leaving the mock. A control another app can satisfy
on your behalf is not a control. The app's own log cannot be borrowed that way.
"""

from helpers import app_log_section, app_surface, waiting

#: The launcher's own first word once a real FaunaMls session's receive loop has
#: run its launch step. Matching the PREFIX rather than any one arm is
#: deliberate: the question is "did the real rail start", and every arm that is
#: observable at all answers yes (see the module docstring's ⚠ on arm 3).
REAL_RAIL_LAUNCH_MARKER = "mls-sync:"

#: What the apple shells stamp on BOTH arms of the gate itself, so a red
#: separates "the env never reached the app" from "the gate opened and the
#: session did not start". Wording owned by
#: `FaunaKit/Testing/AutomationRegistry.swift::realConversationsGateVerdict`.
GATE_OPEN = "real-conversations gate OPEN"
GATE_CLOSED = "real-conversations gate CLOSED"

#: Per stage. The launch step runs off the LOGIN path (both apple shells stamp
#: the verdict and build the session inside the post-auth block), so the lines
#: may not be written at the instant login returns — they landed ~0.8 s after it
#: in the macOS run that first read them. POLL rather than read once: a single
#: read would be a wall-clock race dressed as an assertion, which convention 14
#: rules out. The budget bounds the wait; it never *defines* the pass.
STAGE_BUDGET_S = 30.0


def _diagnose(driver) -> str:
    """The app's own tail, or the distinct finding that there is none."""
    lines = app_log_section.app_text(driver).splitlines()
    if not lines:
        return (
            "the driver kept a log reader but the log is EMPTY — a distinct "
            "finding from 'the marker is missing': the app wrote nothing at all"
        )
    return f"log holds {len(lines)} line(s); last 20:\n" + "\n".join(lines[-20:])


def witness_real_rail(driver, *, context: str, budget_s: float = STAGE_BUDGET_S) -> None:
    """Assert, from the app's own log, that this app is on the real FaunaMls rail.

    `context` names the caller in every message ("the `real_faunamls_app`
    fixture", "this module's own positive control"), so a red says which
    precondition failed rather than only what was missing from a log.

    `budget_s` bounds EACH stage. It exists so the in-process branch tests
    (`tests/test_real_rail_control.py`) can exercise the timeout arms without
    waiting out a real launch budget; a caller driving a real app leaves it
    alone. Shrinking it for a real app would convert this control into the
    wall-clock race convention 14 rules out.

    Raises `AssertionError` on a finding; `pytest.skip`s — with its class
    declared (convention 7) — where the app's log cannot answer at all.
    """
    if not app_log_section.has_reader(driver):
        app_surface.skip_unbuilt(
            driver,
            surface="an app-log reader on the e2e driver",
            detail=f"{context} can witness the real FaunaMls rail only through the "
                   "app's own account of its launch, and this driver answers "
                   "neither app_log_text nor app_stderr_text — so the absence of "
                   "the marker here would be no evidence at all",
            tracked="docs/goal/architecture/e2e-self-diagnosing-failures.md § The convention",
        )
    if driver.is_web():
        # Not debt: no reader of ANY size could make web pass this control. The
        # `mls-sync:` marker is native-only (`libs/fauna-wasm` never emits it),
        # and web is a runtime toggle with its own readiness poll
        # (`data.conv_real_backend_active`). Its reader is also the declared
        # `"evicting"` console ring (`drivers/base.py::log_scope_across_relaunch`),
        # whose absence is never evidence.
        app_surface.declared_absence(
            driver,
            capability="non-evicting app-log reader with a native `mls-sync:` marker "
                       "(web's console ring evicts, and the wasm path never emits "
                       "the marker — it polls `data.conv_real_backend_active` instead)",
            doc="docs/goal/architecture/e2e-self-diagnosing-failures.md § The convention",
        )

    # Stage 1 — the shell's own verdict on the gate. Apple stamps it on BOTH
    # arms, so reaching it at all proves the login path got here, and its
    # wording says which way the branch went. No other shell stamps it.
    if driver.is_macos() or driver.is_ios():
        verdict = waiting.wait_until(
            lambda: next(
                (m for m in (GATE_OPEN, GATE_CLOSED)
                 if m in app_log_section.app_text(driver)),
                None,
            ),
            budget_s=budget_s,
            diagnose=lambda: (
                f"{context}: the app never stamped its real-conversations gate "
                "verdict, which both apple shells write immediately before "
                "branching — so its login path never reached the gate at all, "
                f"which is itself the finding. {_diagnose(driver)}"
            ),
        )
        assert verdict == GATE_OPEN, (
            f"{context}: the app's own log says the real-conversations gate was "
            "CLOSED, so this session ran the deterministic MOCK conversation "
            "backends — every green built on it proves nothing about the real "
            "rail. FAUNA_E2E_REAL_CONVERSATIONS did not reach the app process: "
            "`_apply_real_conversations_env` sets it only when some SELECTED "
            "test in this invocation carries `@pytest.mark.real_conversations`, "
            "and it is session-wide, so the fix is a marker on the consuming "
            f"test (and its own invocation). {_diagnose(driver)}"
        )

    # Stage 2 — the gate opened, so the real session must actually have started.
    waiting.wait_until(
        lambda: REAL_RAIL_LAUNCH_MARKER in app_log_section.app_text(driver),
        budget_s=budget_s,
        diagnose=lambda: (
            f"{context}: the app's own log never mentioned "
            f"{REAL_RAIL_LAUNCH_MARKER!r}, so the real FaunaMls receive loop "
            "never ran its launch step — the session was built and then did not "
            "start, a different defect from a closed gate. "
            f"{_diagnose(driver)}"
        ),
    )

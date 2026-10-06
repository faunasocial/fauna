"""Shared assertion for the (OS login, account) single-instance guard's
terminal refusal contract (`account-scoping.md` § Concurrent instances).

Every native app's re-keying leg proves the same observable — a refused
launch is process DEATH, never a survivor serving the wrong account — so the
assertion lives here once (consumers: the macOS, linux and tui legs).
"""
from __future__ import annotations

import time

from helpers.waiting import wait_until

#: Where each driver family keeps the client child process. They legitimately
#: differ — the GUI drivers hold a `subprocess.Popen`, tui holds a
#: `PtyBackend` wrapping one — but both answer `poll()`, which is the only
#: thing this module needs. Resolving it here keeps the platform branch in the
#: shared layer instead of the test files (testing.md § conventions point 7).
_PROCESS_ATTRS = ("_app_proc", "_pty")

#: Drivers that do not own the client process at all answer liveness directly
#: instead. windows is the case: its app child belongs to the FlaUI bridge, so
#: python holds no handle and the `_PROCESS_ATTRS` lookup finds nothing — which
#: used to make every windows refusal assertion **vacuously green** (it returned
#: at "never spawned at all" against a perfectly healthy app).
_LIVENESS_METHOD = "app_running"

#: Where each driver family exposes the dead process's own account of itself.
#: linux and tui captured the child's stderr; windows and macOS route the same
#: line through a log instead (an inherited-but-undrained stderr pipe is what
#: e2e convention 13 forbids). Any one of them is enough to corroborate.
_TEXT_ATTRS = ("app_stderr_text", "app_log_text")

#: The line every leg prints as it refuses (`[launch-refused] for <actor>:
#: <reason> — exiting`) — apple's `refuseLaunch`, linux's `AuthSuccess` arm,
#: tui's `become_session_instance_or_exit`. One spelling on purpose: it is the
#: causal evidence that the guard is what ended the process.
REFUSAL_MARKER = "[launch-refused]"


def _app_process(driver):
    """The driver's client child process, or `None` if it never started."""
    for attr in _PROCESS_ATTRS:
        proc = getattr(driver, attr, None)
        if proc is not None:
            return proc
    return None


def is_app_alive(driver):
    """Whether the driver's client process is still running — ``True`` /
    ``False``, or ``None`` when this driver cannot say.

    The positive half of the contract the two ``expect_*`` helpers assert the
    negative of: a journey that must *survive* a step (the bound seat's own
    succession — the guard's refusal used to end that process mid-ceremony)
    polls this so a death is reported at once, with the process's own
    ``[launch-refused]`` line, instead of at the end of a budget spent waiting
    on a state a dead process can never reach. ``None`` is deliberately not
    ``True``: a driver with neither a process handle nor an ``app_running()``
    must not read as alive (the vacuous-green trap ``expect_launch_refused``
    documents), so callers treat it as "cannot tell" and keep waiting on the
    state they actually need.
    """
    proc = _app_process(driver)
    if proc is not None:
        return proc.poll() is None
    liveness = getattr(driver, _LIVENESS_METHOD, None)
    if liveness is None:
        return None
    return bool(liveness())


def wait_alive_until(driver, predicate, budget_s, what, diagnose):
    """Deadline-poll ``predicate`` while ``driver``'s client process is alive.

    Lifted 2026-09-02 from the tui instance-lock suite's own copy when the
    apple leg needed the identical guard for the same reason: a death is
    reported the moment it happens, quoting the process's own stderr — the
    guard's ``[launch-refused]`` line, which is the causal evidence for the
    bug this pattern pins — rather than at the end of a budget spent waiting
    on a state a dead process can never reach. ``None`` from :func:`is_app_alive`
    (a driver that cannot say) keeps waiting on the state itself, never counts
    as alive.

    Returns the predicate's truthy value, as :func:`wait_until` does, so a
    caller that polls for a reading keeps the reading rather than taking a
    second one that can land somewhere else.
    """
    value = wait_until(
        lambda: predicate() or is_app_alive(driver) is False,
        budget_s,
        diagnose=diagnose,
    )
    assert is_app_alive(driver) is not False, (
        f"the bound instance EXITED during its own succession — {what}. A "
        "binding left on the retired identity re-fires bound-or-refuse on the "
        "successor's id (account-scoping.md § Concurrent instances → the "
        "binding follows the account). Captured stderr:\n"
        f"{driver.app_stderr_text()[-4000:]}"
    )
    return value


def _assert_refused_for_the_right_reason(driver, message):
    """Require the guard's own refusal line in the dead process's stderr.

    Process death alone is a *weak* observable: two instances sharing one
    install world can also die of an unrelated launch failure (a clobbered
    credential store, a port clash), which would leave a vacuously-green
    refusal test — precisely the "passes for the wrong reason" trap. The
    marker is the causal link, and quoting the captured stderr on failure
    makes the test diagnose itself (testing.md § conventions point 6).

    Inert where it cannot be observed: only the linux and tui drivers capture
    the child's stderr (``app_stderr_text``), and macOS routes the same line
    through the unified log instead — so an absent accessor, or empty output,
    falls back to the death check alone rather than failing a leg that simply
    cannot answer.
    """
    text = None
    for attr in _TEXT_ATTRS:
        reader = getattr(driver, attr, None)
        if reader is None:
            continue
        try:
            candidate = reader()
        except Exception:
            continue
        if candidate and candidate.strip():
            text = candidate
            break
    if text is None:
        return
    assert REFUSAL_MARKER in text, (
        f"{message}\n"
        f"The process did exit, but without the guard's {REFUSAL_MARKER!r} line — "
        f"so it died of something else and this assertion would have been "
        f"vacuously green. Captured output:\n{text[-4000:]}"
    )


def expect_exit_after_click(driver, element_id, message, timeout=20):
    """Click a control whose handler QUITS the app, then require the process to
    be gone.

    The chooser's focus-existing exit (`account-scoping.md` § the per-(OS
    login, account) raise channel): the raise is delivered to the instance
    serving the account and *this* process leaves. Process death is the
    observable, and it is a sharp one — the failure mode it rules out is the
    button that cannot reach anyone and leaves the user on `error-message`,
    which is exactly what a collision against a bound sibling did before the
    raise channel existed.

    Two mechanics are deliberate and must not be "tidied":

    - **The click's reply races the exit.** The agent is in the process being
      quit, so the HTTP reply may never arrive — the same race
      ``LinuxDriver.window_close`` documents. A dropped reply is not a failure
      here; liveness afterwards is the assertion.
    - **No refusal marker.** Unlike :func:`expect_launch_refused` this is not
      the guard refusing a launch, it is a served raise, so nothing prints
      ``[launch-refused]`` and requiring it would be wrong.
    """
    try:
        driver.click(element_id)
    except Exception:
        # The quit tore the agent down mid-reply — expected; assert liveness.
        pass

    proc = _app_process(driver)
    if proc is not None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and proc.poll() is None:
            time.sleep(0.3)
        assert proc.poll() is not None, message
        return

    # Drivers that do not own the process (windows) answer liveness directly —
    # never treat a missing handle as proof, the vacuous-green trap above.
    liveness = getattr(driver, _LIVENESS_METHOD, None)
    assert liveness is not None, (
        f"{message}\nNo process handle and no {_LIVENESS_METHOD}() on this "
        f"driver — the assertion cannot be made and would be vacuously green."
    )
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and liveness():
        time.sleep(0.3)
    assert not liveness(), message


def expect_launch_refused(driver, config, message):
    """Launch and require terminal refusal (process death), tolerating both
    agent-vs-exit interleavings: usually the exit lands before the in-process
    agent reports healthy (``launch`` raises its early-exit RuntimeError), but
    the agent starts ahead of the session build, so a fast agent can win that
    race — then the exit must land moments later. Either interleaving
    satisfies the contract ("the instance must NOT run"); a survivor fails.
    The tolerance is deliberate — do not narrow this to a bare
    ``pytest.raises``.

    Death is then corroborated by the guard's own refusal line where the
    driver can read it (see :func:`_assert_refused_for_the_right_reason`)."""
    try:
        driver.launch(config)
    except RuntimeError:
        _assert_refused_for_the_right_reason(driver, message)
        return  # exited before the agent came up — the refusal, seen early
    proc = _app_process(driver)
    if proc is not None:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and proc.poll() is None:
            time.sleep(0.3)
        assert proc.poll() is not None, message
        _assert_refused_for_the_right_reason(driver, message)
        return

    # No handle here — either the driver does not own the process (windows: the
    # FlaUI bridge does) or it truly never spawned. Ask the driver itself before
    # concluding anything: treating "no handle" as proof of refusal is how this
    # assertion silently passed against a healthy app on windows.
    liveness = getattr(driver, _LIVENESS_METHOD, None)
    if liveness is None:
        return  # never spawned at all — refused before the process existed
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline and liveness():
        time.sleep(0.3)
    assert not liveness(), message
    _assert_refused_for_the_right_reason(driver, message)

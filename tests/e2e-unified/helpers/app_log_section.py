"""The client's own account of itself, attached to every failing test's report.

`docs/goal/architecture/e2e-conventions.md` § point 6: *"don't debug with
screenshots — failures must diagnose themselves"*. Most driver families keep the
client's own log — windows and iOS on disk (`app_log_text`), linux, tui, macOS
and web as captured output (`app_stderr_text`, the same two attributes
`instance_guard.py::_TEXT_ATTRS` reaches for, for the same reason; on web it is
the console ring). Nothing read them generically, so each investigation grew its
own in-test reader and every test without one reported a bare `wait_until`
timeout with nothing about what the app thought it was doing.

**Which spelling a family answers is not guessable, and this module is the only
place that knows.** The pairing above has been miscited before — this very
docstring claimed macOS kept `app_log_text` while `drivers/macos.py` has only
ever exposed `app_stderr_text`, and a reader who trusted the prose would have
looked for the wrong attribute. Ask :func:`has_reader` rather than naming an
attribute, and android — which answers *neither* today — is a `False` instead of
a silent empty string.

**This is the ONE surfacing path.** The two in-test readers that predate it
(`tests/test_mail_lists.py::_diagnose`, `tests/test_sync_live_apply.py::_diagnose`)
are a different thing and stay: they *filter* for domain markers and feed an
assertion message. What was missing — and what a third such reader would not have
fixed — is the unfiltered tail reaching **every** failing test, whether or not
someone thought to write a diagnoser for it.

**Why the report and not the assertion.** The conftest hook calls this from
`pytest_runtest_makereport(when="call")`, which runs after the call phase and
*before* the teardown that reclaims a per-instance data dir. That ordering is the
whole mechanism: the 2026-08-24 `--app windows` succession run could not say why
two aftermath legs did nothing, because windows e2e data dirs are per-instance
`mkdtemp` and the log was gone by the time pytest printed its report.

⚠ **Three answers, and they must stay distinct** — each is a different verdict:
no section at all means *this driver family keeps no log, so nothing was lost*;
an explicit `<empty>` section means *there is a log and it says nothing*, which is
itself evidence the app never started or never logged; a read-failure section
means *the log exists and we could not read it*. Collapsing any two into silence
is how a diagnostic stops being trusted.
"""

#: Where each driver family keeps the client's own account of itself, in the
#: order to try. Deliberately the same pair, in the same order, as
#: `instance_guard.py::_TEXT_ATTRS`: one spelling of "the app's own words".
_TEXT_ATTRS = ("app_log_text", "app_stderr_text")

#: A full app log runs to tens of thousands of lines. The tail is what a
#: post-mortem wants, and the total is always stated so the drop stays visible —
#: a silently truncated log reads as a complete one.
#:
#: ⚠ **Sized by measurement, and 200 was too small.** The first real use of this
#: instrument — the `--app windows` succession pair, 2026-08-24 — cut a 440-line log to its last 200, and what fell off the front was
#: the whole post-ceremony aftermath pass: the one thing the run was launched to
#: read. What filled the tail instead was ~90 repetitions of a rate-limited
#: `fauna.nest.info` poll. A cap tight enough for the quiet case is exactly the cap
#: that fails the loud one, and the loud one is when a post-mortem is needed. The
#: stated total is what turned that into a five-second correction instead of a
#: false "the pass never ran" finding — keep it whatever this number becomes.
DEFAULT_TAIL_LINES = 1000


def _reader(driver):
    """`(attr_name, callable)` for this driver's log reader, or `None`.

    The ONE place `_TEXT_ATTRS` is walked, so the report section below and the
    in-test diagnosers that filter the same text for a domain marker cannot
    drift onto different spellings of "the app's own words".
    """
    if driver is None:
        return None
    for attr in _TEXT_ATTRS:
        reader = getattr(driver, attr, None)
        if reader is not None:
            return attr, reader
    return None


def has_reader(driver) -> bool:
    """Does this driver family keep the app's own log at all?

    :func:`app_text` flattens "no reader" and "the log is empty" to the same
    `""` on purpose — it runs while a test is already failing and has no reader
    to tell them apart *to*. A test that reads the log as **evidence** needs the
    distinction: an absent marker in a log that exists is a finding, while the
    same absence with no log behind it is nothing at all. So a test asserting on
    the app's own words asks this first and declares the gap
    (`helpers/app_surface.py`) rather than asserting into a void — android
    answers neither attribute today, which is exactly that case.
    """
    return _reader(driver) is not None


def app_text(driver) -> str:
    """The app's own words as plain text — `""` when there are none to have.

    The primitive behind a *filtered* in-test diagnosis (a test that wants only
    the lines carrying its own domain marker, folded into an assertion message)
    as opposed to the unfiltered report section :func:`build` attaches. It
    deliberately flattens every "nothing here" case to `""`: a diagnoser runs
    while a test is already failing and must never raise, and unlike the report
    section it has no reader to tell the three cases apart *to*.
    """
    found = _reader(driver)
    if found is None:
        return ""
    try:
        return found[1]() or ""
    except Exception:  # diagnostics must never mask the failure they explain
        return ""


def launched_drivers(funcargs):
    """`(label, driver)` for every seat a launcher fixture started in the test body.

    A fixture that yields a LAUNCHER rather than a driver (`alice_builder_seats`,
    `helpers/fleet.py::SiblingSeats`) builds its seats inside the test, so
    `item.funcargs` holds the launcher and the value-based resolution never sees
    the seat. A launcher opts in by answering `launched_drivers()` — every seat it
    started this test, retired ones included (their log may still be readable, and
    :func:`build` reports it if not). Measured gap: a red phone-witness run whose
    whole question was the DESKTOP seat's launch attached only the phone's log.
    A launcher whose list cannot be read is skipped, never raised.
    """
    from helpers import frame_invariants

    found = []
    for name, value in sorted(funcargs.items()):
        lister = getattr(value, "launched_drivers", None)
        if not callable(lister):
            continue
        try:
            seats = list(lister())
        except Exception:  # diagnostics must never mask the failure they explain
            continue
        for i, seat in enumerate(seats):
            driver = frame_invariants._as_driver(seat) or seat
            found.append((f"{name} #{i + 1}", driver))
    return found


def build(driver, tail_lines: int = DEFAULT_TAIL_LINES, label: str | None = None):
    """`(title, body)` for a failing test's report section, or `None`.

    `None` means this driver has no log attribute at all — there is nothing to
    say, and an empty section would falsely imply the app went quiet. `label`
    names the seat when a test has more than one.
    """
    found = _reader(driver)
    if found is None:
        return None
    attr, reader = found
    title = f"app log ({attr})" + (f" — {label}" if label else "")
    try:
        text = reader()
    except Exception as exc:  # diagnostics must never mask the failure they explain
        return title, f"<read failed: {type(exc).__name__}: {exc}>"
    if not text:
        return title, (
            "<empty — the reader returned no text at all. The app either never "
            "started, never logged, or logged somewhere this driver does not read.>"
        )
    # A driver whose agent runs detached stitches the agent's own log after the
    # app's; each part keeps its OWN tail, or the louder one erases the other.
    from drivers.base import AGENT_LOG_BANNER

    parts = text.split(f"\n{AGENT_LOG_BANNER}\n")
    if len(parts) == 1 and text.startswith(f"{AGENT_LOG_BANNER}\n"):
        parts = ["", text[len(AGENT_LOG_BANNER) + 1 :]]
    tails = [_tail(part.splitlines(), tail_lines) for part in parts]
    return title, f"\n{AGENT_LOG_BANNER}\n".join(tails)


def _tail(lines: list[str], tail_lines: int) -> str:
    if len(lines) <= tail_lines:
        return "\n".join(lines)
    head = f"<truncated: last {tail_lines} of {len(lines)} lines>"
    return "\n".join([head, *lines[-tail_lines:]])

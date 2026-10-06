"""The on-failure app-log section — one shared surfacing path, not a per-test reader.

`docs/goal/architecture/e2e-conventions.md` § point 6 ("don't debug with
screenshots — failures must diagnose themselves"). A driver that keeps the
client's own account of itself on disk (`app_log_text`) or in a captured stderr
buffer (`app_stderr_text`) already holds the answer to most post-mortems — but
until now nothing read it *generically*, so each investigation grew its own
in-test reader (`test_mail_lists.py::_diagnose`, `test_sync_live_apply.py::_diagnose`)
and every test without one reported a bare `wait_until` timeout with no trace of
what the app thought it was doing.

**The failure this closes, and why a third in-test reader was the wrong fix.**
The 2026-08-24 `--app windows` succession run could not answer *why* two legs of
the post-succession aftermath did nothing: windows e2e data dirs are per-instance
`mkdtemp` and the driver's teardown reclaims them, so by the time pytest printed
its report the log was gone — and no line of it had ever reached the report.
Reading the log at `makereport(when="call")` is what fixes that for good: it runs
after the call phase and **before** the teardown that reclaims the dir, so the
launch's own log is still on disk exactly when the failure is being written up.

These are tier_1: the extraction is a pure function over a duck-typed driver, so
what counts as a useful section can be pinned without a nest, a driver or an app.

⚠ **The three answers must stay distinct**, and each one is a different verdict
about the run: no section at all ("this driver family keeps no log — nothing was
lost"), an explicit `<empty>` section ("there IS a log and it says nothing", which
is itself evidence the app never started or never logged), and a read-failure
section ("the log exists and we could not read it"). Collapsing any two of them
into silence is how a diagnostic stops being trusted.
"""

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_1]

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import app_log_section  # noqa: E402


class _NoLog:
    """A driver family that keeps no client-side log at all (e.g. the web driver)."""


class _WithLog:
    def __init__(self, text):
        self._text = text

    def app_log_text(self):
        return self._text


class _WithStderr:
    def __init__(self, text):
        self._text = text

    def app_stderr_text(self):
        return self._text


class _Raises:
    def app_log_text(self):
        raise OSError("data dir already reclaimed")


def _section(driver, **kw):
    return app_log_section.build(driver, **kw)


def test_a_driver_with_no_log_gets_no_section():
    """No section, not an empty one: nothing was lost, so there is nothing to say."""
    assert _section(_NoLog()) is None


def test_no_driver_at_all_gets_no_section():
    assert _section(None) is None


def test_the_log_text_reaches_the_section_body():
    title, body = _section(_WithLog("aftermath DraftsReseal: settled\nsuccession aftermath: Ran"))
    assert "app log" in title.lower()
    assert "succession aftermath: Ran" in body


def test_stderr_is_the_fallback_when_there_is_no_log_file():
    """linux and tui capture the child's stderr where windows and macOS keep a log
    (`helpers/instance_guard.py::_TEXT_ATTRS` names the same two attributes for the
    same reason) — one surfacing path must read whichever the driver family has."""
    title, body = _section(_WithStderr("thread 'main' panicked"))
    assert "panicked" in body
    assert "stderr" in title.lower() or "stderr" in body.lower()


def test_an_empty_log_is_reported_rather_than_swallowed():
    """A log that exists and says NOTHING is evidence, not an absence of evidence:
    it separates "the app never logged" from "this driver keeps no log"."""
    title, body = _section(_WithLog(""))
    assert "empty" in body.lower()


def test_a_none_read_is_reported_the_same_way():
    title, body = _section(_WithLog(None))
    assert "empty" in body.lower()


def test_a_read_failure_is_reported_and_never_raised():
    """Diagnostics must never mask the failure they were called to explain."""
    title, body = _section(_Raises())
    assert "OSError" in body
    assert "data dir already reclaimed" in body


def test_only_the_tail_is_kept_and_the_body_says_so():
    """A full app log can be tens of thousands of lines; the report must stay
    readable while still admitting what it dropped — a silently truncated log
    reads as a complete one."""
    text = "\n".join(f"line {i}" for i in range(1, 5001))
    title, body = _section(_WithLog(text), tail_lines=50)
    assert "line 5000" in body
    assert "line 4951" in body
    assert "line 4950" not in body
    assert "5000" in body  # the total is stated, so the drop is visible


def test_a_short_log_is_not_described_as_truncated():
    title, body = _section(_WithLog("only line"), tail_lines=50)
    assert "only line" in body
    assert "truncat" not in body.lower()


def test_the_default_cap_clears_a_real_measured_log():
    """The regression this number exists for.

    The instrument's first real use cut a 440-line windows app log to its last
    200, and what fell off the front was the post-ceremony aftermath pass — the
    one thing the run was launched to read; the tail held ~90 repetitions of a
    rate-limited poll instead. A cap tight enough for the quiet case is exactly
    the cap that fails the loud one. This pins the default against that measured
    log rather than against a taste, so a future "tidy up the noise" pass has to
    argue with the run that set it.
    """
    text = "\n".join(f"line {i}" for i in range(1, 441))
    title, body = _section(_WithLog(text))
    assert "line 1\n" in body + "\n"
    assert "truncat" not in body.lower()


def test_a_stitched_agent_log_cannot_push_the_apps_own_lines_out():
    """Each stitched part keeps its OWN tail.

    On windows — for the windows app and for tui running there — the sync agent
    is spawned detached, so the driver appends the agent's own log after the
    app's (`drivers/base.py::stitch_agent_log`). One tail over the joined text
    kept only the agent: measured 2026-09-28 on two tui-on-win succession reds,
    whose 1000-line section was ~1000 lines of the agent's `ListEngines` pipe
    polling and not ONE line of the app that failed.
    """
    from drivers.base import stitch_agent_log

    app_lines = "\n".join(f"app {i}" for i in range(1, 301))
    agent_lines = "\n".join(f"agent {i}" for i in range(1, 5001))
    title, body = _section(
        _WithStderr(stitch_agent_log(app_lines, agent_lines)), tail_lines=100
    )
    assert "app 300" in body and "app 201" in body, body[:300]
    assert "app 200" not in body
    assert "agent 5000" in body and "agent 4901" in body
    assert "agent 4900" not in body
    # Each part states its own drop, so neither reads as complete.
    assert "last 100 of 300 lines" in body
    assert "last 100 of 5000 lines" in body


def test_a_stitch_with_one_side_empty_is_the_other_side_alone():
    from drivers.base import stitch_agent_log

    assert stitch_agent_log("app only", "") == "app only"
    assert stitch_agent_log("", "") == ""
    assert "agent only" in stitch_agent_log("", "agent only")


# ── The wiring, not just the extraction ────────────────────────────────────────
#
# The helper above is useless if nothing calls it, and the conftest hook is where
# every way to get this wrong lives: the wrong phase (teardown has already
# reclaimed the dir), the wrong resolution (the conftest `app` fixture is shadowed
# by seven modules), or a diagnostic that raises and takes the run with it. These
# call `conftest._attach_app_log_section` directly — a plain function precisely so
# the wiring is pinnable without a nest, a driver, or pytest's own machinery.

import conftest  # noqa: E402


class _FakeDriver(_WithLog):
    """`frame_invariants._as_driver` requires the `get_state`/`reset` pair — a
    snapshot that merely exposes a `get_state` must not be probed as an app."""

    def get_state(self):
        return {}

    def reset(self):
        pass


class _FakeApp:
    """The shape the `app` fixture yields: the driver hangs off `.driver`."""

    def __init__(self, driver):
        self.driver = driver


class _FakeItem:
    def __init__(self, funcargs):
        self.funcargs = funcargs


class _FakeReport:
    def __init__(self):
        self.sections = []


def _attach(funcargs):
    report = _FakeReport()
    conftest._attach_app_log_section(_FakeItem(funcargs), report)
    return report.sections


def test_the_hook_attaches_the_log_of_the_drivers_app_fixture():
    sections = _attach({"app": _FakeApp(_FakeDriver("succession aftermath: Ran"))})
    assert len(sections) == 1
    assert "succession aftermath: Ran" in sections[0][1]


def test_the_hook_finds_a_driver_a_module_resolved_under_another_name():
    """Seven modules shadow the conftest `app`; resolution is by fixture VALUE."""
    sections = _attach({"persistent_app": _FakeApp(_FakeDriver("shadowed but found"))})
    assert len(sections) == 1
    assert "shadowed but found" in sections[0][1]


def test_a_test_with_no_driver_attaches_nothing():
    sections = _attach({"some_value": "a-string", "nest_instance": object()})
    assert sections == []


def test_a_driver_that_explodes_never_breaks_the_report():
    class _Exploding(_FakeDriver):
        def app_log_text(self):
            raise RuntimeError("boom")

    sections = _attach({"app": _FakeApp(_Exploding(""))})
    assert len(sections) == 1
    assert "RuntimeError" in sections[0][1]


# ── `app_text`: the same pair, for a FILTERED in-test diagnosis ─────────────
# The report section above is unfiltered and goes to every failing test. A test
# that wants only the lines carrying its own domain marker — folded into an
# assertion message rather than a report section — needs the same two attributes
# walked in the same order, which is the whole reason this is a shared primitive
# and not a fourth capability name on the drivers
# (`test_conversations_message_banner.py::_why` is the first caller).


def test_app_text_reads_whichever_attribute_the_driver_family_has():
    assert app_log_section.app_text(_WithLog("a\nb")) == "a\nb"
    assert app_log_section.app_text(_WithStderr("c")) == "c"


def test_app_text_flattens_every_nothing_here_case_to_empty_string():
    """Deliberately unlike :func:`build`, which keeps the three cases apart.

    A filtered diagnoser runs while a test is ALREADY failing and has no reader
    to tell "no log" from "empty log" to — it only decides whether to append a
    line. Raising there would replace the real failure with this one, so every
    such case collapses to `""` and the assertion speaks for itself.
    """
    assert app_log_section.app_text(_NoLog()) == ""
    assert app_log_section.app_text(None) == ""
    assert app_log_section.app_text(_WithLog("")) == ""
    assert app_log_section.app_text(_WithLog(None)) == ""


def test_app_text_never_raises_out_of_a_failing_test():
    """The reader explodes (a reclaimed data dir) — the diagnosis goes quiet,
    the failure it was explaining survives."""
    assert app_log_section.app_text(_Raises()) == ""


# ── Seats a test launches in its own body ───────────────────────────────────
# A fixture that yields a launcher (`alice_builder_seats`, `SiblingSeats`)
# builds its drivers INSIDE the test, so `item.funcargs` holds the launcher, not
# the seat — and the one-driver resolution above attached only the phone's log
# to a red phone-witness run whose question was entirely about the DESKTOP seat's
# launch. A launcher answers `launched_drivers()`, and every seat it launched
# gets its own labelled section beside the primary driver's.


class _FakeLauncher:
    def __init__(self, *seats):
        self._seats = list(seats)

    def launched_drivers(self):
        return list(self._seats)


def test_every_seat_a_launcher_started_gets_its_own_labelled_section():
    phone = _FakeApp(_FakeDriver("phone: 0 channel(s) restored"))
    desktop = _FakeDriver("desktop: 3 channel(s) restored")
    sections = _attach({
        "real_faunamls_app": phone,
        "alice_builder_seats": _FakeLauncher(desktop),
    })
    bodies = [body for _, body in sections]
    assert any("phone: 0 channel(s)" in b for b in bodies)
    assert any("desktop: 3 channel(s)" in b for b in bodies)
    [seat_title] = [t for t, b in sections if "desktop:" in b]
    assert "alice_builder_seats" in seat_title


def test_a_launcher_that_launched_nothing_adds_no_section():
    sections = _attach({
        "app": _FakeApp(_FakeDriver("primary")),
        "alice_builder_seats": _FakeLauncher(),
    })
    assert len(sections) == 1


def test_a_launcher_that_explodes_never_breaks_the_primary_section():
    class _Broken:
        def launched_drivers(self):
            raise RuntimeError("seat list gone")

    sections = _attach({"app": _FakeApp(_FakeDriver("primary")), "seats": _Broken()})
    assert [b for _, b in sections] == ["primary"]

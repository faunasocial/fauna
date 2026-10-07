"""The real-rail control's own branches, headlessly.

`helpers/real_rail_control.witness_real_rail` is a PRECONDITION of
`real_faunamls_app` (conftest) and therefore of every real-conversations test on
every launch-gate app. Its four branches — android's declared skip, web's
declared skip, apple's two-stage read, a non-apple native's single stage — are
otherwise reachable only by launching real apps, which means the branch that
matters most (a CLOSED gate must FAIL, not pass and not skip) would be witnessed
only by the failure it exists to catch.

So: split the mile. The control's *decisions* are pure logic over two inputs (an
app-identity predicate and a string of log text), and a fake driver exercises
them in-process in milliseconds. What stays untestable here — that a real macOS
app really does stamp its verdict and really does write ``mls-sync:`` — is
exactly what the `--app macos` runs witness.
"""
import pytest

from helpers import app_surface, real_rail_control

pytestmark = pytest.mark.tier_1


@pytest.fixture(autouse=True)
def _keep_the_runs_own_tally():
    """A fake driver's declared skips must never reach the run's summary.

    `skip_unbuilt` appends to the accumulated unbuilt-surface tally that
    `conftest.py::pytest_terminal_summary` prints at the end of the run, and the
    two declaration branches below deliberately trigger it — so without this the
    run would report two unbuilt surfaces (`android`, `web`) that no real app
    was ever asked about, in the count convention 7 ratchets.

    It also keeps those declarations *skips*. Under `--strict-app`
    `skip_unbuilt` raises `Failed`, which is right for a real app's unbuilt
    surface and wrong here: the two declaration cases assert that the control
    raises the skip, on a fake driver standing in for an app the run is not
    testing, so a linux sweep failed them for android's and web's declarations.
    """
    outer = app_surface.unbuilt_hits()
    was_strict = app_surface.strict_app_enabled()
    app_surface.set_strict_app(False)
    yield
    app_surface.set_strict_app(was_strict)
    app_surface.restore_unbuilt_hits(outer)


class _FakeDriver:
    """Answers the app-identity predicates and one log reader, nothing else.

    Deliberately exposes ``app_stderr_text`` (or nothing at all), because
    `app_log_section._reader` walks exactly that attribute pair — asking the
    driver rather than naming a family is the whole contract of that module,
    and a fake that hard-codes a family would test the wrong thing.
    """

    def __init__(self, app: str, log: str | None = ""):
        self._app = app
        if log is not None:
            self.app_stderr_text = lambda: log

    def __getattr__(self, name):
        if name.startswith("is_"):
            return lambda: self._app == name[3:]
        raise AttributeError(name)


def _witness(app, log, **kw):
    real_rail_control.witness_real_rail(
        _FakeDriver(app, log), context="this test", **kw
    )


# ── No reader at all is a DECLARED skip, never an assert ──
def test_a_driver_with_no_log_reader_declares_rather_than_asserting():
    """A driver answering neither log attribute cannot show an absent marker is
    evidence — convention 7's `skip_unbuilt`, not a red. (Every shipped family
    now has a reader; the branch guards the next driver that does not.)"""
    with pytest.raises(pytest.skip.Exception) as exc:
        _witness("android", None)
    assert "app-log reader" in str(exc.value)
    assert "e2e-self-diagnosing-failures.md" in str(exc.value), (
        "the skip must name where the work is tracked"
    )


def test_the_android_driver_has_an_app_log_reader():
    """The seam `has_reader` reads: android answers `app_log_text` over the
    bridge's `GET /app-log`, so the control no longer skips it as unbuilt."""
    from drivers.android import AndroidBridgeDriver
    from helpers import app_log_section

    assert app_log_section.has_reader(AndroidBridgeDriver())


def test_web_declares_rather_than_reading_its_evicting_ring():
    """web keeps a 500-entry console ring, so an absent line is not evidence it
    was never logged — sound for diagnosis, unsound as a control's negative half."""
    with pytest.raises(pytest.skip.Exception) as exc:
        _witness("web", "")
    assert "non-evicting" in str(exc.value)
    assert "declared absence" in str(exc.value), "a permanent absence, not unbuilt debt"


# ── The branch this whole module exists for ──────────────────────────────────
def test_a_closed_apple_gate_FAILS_and_says_the_session_ran_the_mock():
    """The silent-mock hole, made loud. A green here would mean the control
    cannot tell a real-rail session from a mock one — which is the defect."""
    log = (
        "[e2e] real-conversations gate CLOSED (FAUNA_E2E_REAL_CONVERSATIONS unset) "
        "— keeping the deterministic mock conversation backends\n"
    )
    with pytest.raises(AssertionError) as exc:
        _witness("macos", log)
    message = str(exc.value)
    assert "CLOSED" in message and "MOCK" in message
    assert "real_conversations" in message, "a red must name the fix, not only the fact"


def test_an_open_apple_gate_with_a_started_session_passes():
    log = (
        "[e2e] real-conversations gate OPEN — building the real ConversationsSession\n"
        "INFO mls-sync: cross-device plane wired (2 channel(s) restored from replica)\n"
    )
    _witness("macos", log)  # no raise


def test_an_open_apple_gate_whose_session_never_started_FAILS_at_stage_2():
    """A gate that opened and a session that then died are different defects —
    the reason the control is two stages and not one."""
    log = "[e2e] real-conversations gate OPEN — building the real ConversationsSession\n"
    with pytest.raises(AssertionError) as exc:
        _witness("macos", log, budget_s=0.4)
    message = str(exc.value)
    assert "mls-sync:" in message
    assert "did not start" in message, "stage 2's red must not read like a closed gate"


def test_the_gate_stage_is_apple_only():
    """No other shell stamps `realConversationsGateVerdict`, so requiring it off
    apple would red every other launch-gate app for a line it never writes."""
    _witness("windows", "INFO mls-sync: cross-device plane wired (0 channel(s))\n")


def test_an_empty_log_is_reported_as_its_own_distinct_finding():
    """'there is a log and it says nothing' is evidence the app never started —
    a different verdict from 'the marker is missing', and the diagnosis says so."""
    with pytest.raises(AssertionError) as exc:
        _witness("macos", "", budget_s=0.4)
    assert "EMPTY" in str(exc.value)


# ── The action-level door: `enable_real_faunamls()` asks the same witness ────
def test_enable_real_faunamls_on_a_closed_apple_gate_FAILS_naming_the_marker():
    """A consumer that calls the action directly (not through
    `real_faunamls_app`) and forgot the marker used to get a silent return, so
    the red surfaced helpers later as a thread that never rendered. The action
    must answer the gate the same way the fixture does."""
    from actions.conversations import ConversationsActions

    log = "[e2e] real-conversations gate CLOSED (FAUNA_E2E_REAL_CONVERSATIONS unset)\n"
    with pytest.raises(AssertionError) as exc:
        ConversationsActions(_FakeDriver("macos", log)).enable_real_faunamls()
    message = str(exc.value)
    assert "enable_real_faunamls" in message, "the red must name the caller"
    assert "real_conversations" in message, "a red must name the fix, not only the fact"


def test_enable_real_faunamls_on_an_open_apple_gate_sends_no_command():
    """The launch-gate apps implement no `conversations_enable_real_faunamls`
    command, so a witnessed-open gate returns without calling one (`_FakeDriver`
    answers no `call_command`, so a call would raise AttributeError)."""
    from actions.conversations import ConversationsActions

    log = (
        "[e2e] real-conversations gate OPEN — building the real ConversationsSession\n"
        "INFO mls-sync: cross-device plane wired (1 channel(s) restored from replica)\n"
    )
    ConversationsActions(_FakeDriver("ios", log)).enable_real_faunamls()

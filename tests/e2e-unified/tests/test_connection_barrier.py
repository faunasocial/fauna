"""The connection barrier waits on shared Rust's verdict, and refuses loudly.

`helpers/connection.py` is the harness's one answer to a defect that had no
answer at all: every app desensitizes an ``OnlineOnly`` affordance while its
transport word is offline (``fauna_protocol::offline_class::affordance``, the
rule `account-data-plane.md` § The offline-mutation contract class 3 owns),
``"connecting"`` is one of the offline words, and 276 of the 628 registered wire
kinds are ``OnlineOnly`` — yet nothing in ``drivers/``, ``actions/`` or
``helpers/`` read connection state, so every test driving an online-only control
on a freshly launched app raced the WS handshake and lost when the box was
loaded.

**Why this file is tier_1 rather than a journey.** The property that makes the
barrier trustworthy is not "it eventually returned"; it is *what it waits for*,
and the two ways that can be silently wrong are both unreachable from a passing
journey:

1. **Waiting for equality with ``"connected"``.** The gate's polarity is the
   opposite — ``is_online`` is *"online unless the word is a KNOWN offline
   word"*, deliberately, so an older app meeting a future state word keeps its
   controls live. A barrier written the obvious way agrees with the gate on
   every word that exists **today** and hangs to its full ceiling on the first
   new one. No journey can see that; a fake driver reporting a future word can.
2. **Degrading into a no-op.** A barrier that reads a missing observable as
   "fine" passes every test it is in while asserting nothing — convention 11's
   dropped-command failure, one layer up. So the absence must be *loud*, and
   "loud" is a property of the failure path, which a green journey never walks.

The live half — that ``tui``/``linux`` really do publish the observable, and
that it really does reach online — is ``test_connection_observable.py``.
"""

import pathlib
import re
import time

import pytest

from helpers.app_surface import app_name
from helpers.connection import (
    APPS_PUBLISHING_CONNECTION,
    CONNECTION_KEY,
    ConnectionBarrierTimeout,
    wait_until_online,
)

pytestmark = pytest.mark.tier_1

# Scenario ceilings, NOT budgets in convention 14's sense: there is no real
# latency behind a scripted driver, so these are scenario parameters — how long
# this test is willing to watch a fake, and (for the refusal cases) the ceiling
# whose expiry IS the behaviour under test. They deliberately do not live in
# `helpers/budgets.py`: a name there would imply a real-world wait, and sizing
# them "far above any non-pathological delay" would only make the suite slower
# while asserting exactly the same thing.
_AMPLE_S = 10.0
_EXPIRES_S = 0.5

# The exemplar for "an app whose leg has not landed" — deliberately NOT one of
# the seven app names.
#
# `APPS_PUBLISHING_CONNECTION` only ever grows, so every real name is a name
# that will one day be IN it, and a test pinned to one silently inverts its own
# subject on the day that leg lands. That is not hypothetical: this test read
# `app="windows"` until windows' leg landed 2026-09-07, after which it asserted
# the no-op branch for an app that takes the polling one — red, and invisible,
# because no gate runs this directory.
# `android` would re-arm the identical trap: it is the one leg still outside.
#
# So the exemplar is a name no app can ever have. `app_name` resolves an
# unrecognised driver to its class name (its own documented fallback), which
# keeps the failure message named rather than "unknown", and keeps this test's
# subject outside the set by construction rather than by today's roster.
_APP_WITH_NO_CONNECTION_LEG = "an-app-whose-connection-leg-has-not-landed"


class _FakeDriver:
    """A driver that reports a scripted sequence of ``connection`` values.

    Deliberately not a mock of the transport: the barrier's contract is with
    ``/app/state``'s shape, and scripting that shape is exactly as much fidelity
    as the contract has.
    """

    def __init__(self, values, *, app="tui"):
        self._values = list(values)
        self._app = app
        self.reads = 0

    # `helpers.app_surface.app_name` walks these predicates in order.
    def is_macos(self):
        return self._app == "macos"

    def is_ios(self):
        return self._app == "ios"

    def is_android(self):
        return self._app == "android"

    def is_windows(self):
        return self._app == "windows"

    def is_linux(self):
        return self._app == "linux"

    def is_tui(self):
        return self._app == "tui"

    def is_web(self):
        return self._app == "web"

    def get_state(self, path=None, **_kwargs):
        assert path == CONNECTION_KEY, f"barrier read {path!r}, not the connection key"
        self.reads += 1
        # The last scripted value repeats forever — a transport that stays put.
        return self._values[min(self.reads - 1, len(self._values) - 1)]


def test_barrier_returns_once_the_app_reports_online():
    """The ordinary case: a handshake in flight, then live."""
    driver = _FakeDriver([
        {"state": "connecting", "online": False},
        {"state": "connecting", "online": False},
        {"state": "connected", "online": True},
    ])
    assert wait_until_online(driver, timeout=_AMPLE_S) == "connected"
    assert driver.reads == 3, "the barrier must poll, not read once and hope"


def test_barrier_accepts_a_future_state_word_because_the_gate_does():
    """The polarity, and the reason ``online`` crosses the boundary as a bool.

    ``"degraded"`` is not a word any app ships today. ``is_online`` reads it as
    online (ruling 3: never overclaim — the worst case is a request that fails
    with the error it would have shown anyway, instead of a control the user
    cannot press), so the gate leaves the control live and the barrier MUST let
    the test proceed. A barrier comparing the word to ``"connected"`` would
    block here for its whole budget on an app that is working fine.
    """
    driver = _FakeDriver([{"state": "degraded", "online": True}])
    assert wait_until_online(driver, timeout=_AMPLE_S) == "degraded"


def test_barrier_fails_loudly_naming_the_word_it_was_stuck_on():
    """A transport that never comes up is a real failure, and it diagnoses
    itself (conventions point 6) — not "timed out" but "still 'connecting'"."""
    driver = _FakeDriver([{"state": "connecting", "online": False}])
    started = time.monotonic()
    with pytest.raises(ConnectionBarrierTimeout) as excinfo:
        wait_until_online(driver, timeout=_EXPIRES_S)
    assert "connecting" in str(excinfo.value)
    assert "tui" in str(excinfo.value)
    # The ceiling is honoured (it is a ceiling, not a sleep).
    assert time.monotonic() - started < 5.0


def test_barrier_refuses_a_declared_app_that_publishes_nothing():
    """An app on the list that stops publishing is a REGRESSION, never a skip.

    This is the convention 11 half: publishing ``{"state": …, "online": false}``
    and publishing nothing are different answers, so the barrier must not read a
    missing key as "no leg, carry on" for an app whose leg has landed.
    """
    driver = _FakeDriver([None])
    with pytest.raises(ConnectionBarrierTimeout) as excinfo:
        wait_until_online(driver, timeout=_EXPIRES_S)
    assert CONNECTION_KEY in str(excinfo.value)
    assert "APPS_PUBLISHING_CONNECTION" in str(excinfo.value)


def test_barrier_is_a_no_op_on_an_app_whose_leg_has_not_landed():
    """And it costs nothing — no poll, no budget — so adding it to the shared
    login path could not slow down an app still waiting for its leg.

    The guard below is the point: this test's whole subject is an app OUTSIDE
    ``APPS_PUBLISHING_CONNECTION``, so the exemplar's membership is asserted
    rather than assumed (see :data:`_APP_WITH_NO_CONNECTION_LEG` for why it is
    a synthetic name and not a real one).
    """
    driver = _FakeDriver(
        [{"state": "connecting", "online": False}],
        app=_APP_WITH_NO_CONNECTION_LEG,
    )
    assert app_name(driver) not in APPS_PUBLISHING_CONNECTION, (
        "this test asserts the NO-LEG branch, so its exemplar must be outside "
        "APPS_PUBLISHING_CONNECTION — it is not, so the test is now asserting "
        "the opposite of its own name"
    )
    assert wait_until_online(driver, timeout=_AMPLE_S) is None
    assert driver.reads == 0


# A list literal whose first element is one of shared Rust's offline words —
# the shape a re-implementation takes. Prose naming the words is fine; a
# *list* of them is a second owner.
_OFFLINE_WORD_LIST = re.compile(
    r"""\[\s*['"](?:connecting|disconnected|unreachable)['"]\s*,""",
)


def test_the_offline_word_list_has_exactly_one_owner():
    """No file under ``tests/e2e-unified/`` may carry a copy of
    ``fauna_protocol::offline_class::OFFLINE_STATE_WORDS``.

    The whole bargain of shipping ``online`` as a boolean is that the word set
    keeps one owner, in Rust. A second copy here would drift silently the first
    time either side gained a variant — the per-app-divergence mistake one
    directory down (priority #4), which is how five hand-rolled
    ``_wait_connected`` copies came to poll a *localized* indicator string for
    ``"Connected"`` in the first place.
    """
    root = pathlib.Path(__file__).resolve().parent.parent
    me = pathlib.Path(__file__).resolve()
    offenders = []
    for path in sorted(root.rglob("*.py")):
        if path.resolve() == me:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        if _OFFLINE_WORD_LIST.search(text):
            offenders.append(str(path.relative_to(root)))
    assert not offenders, (
        "these files carry what looks like a copy of shared Rust's "
        "OFFLINE_STATE_WORDS; wait on the published `online` boolean instead "
        f"(helpers/connection.py): {offenders}"
    )

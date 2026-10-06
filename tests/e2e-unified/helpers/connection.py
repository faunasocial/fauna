"""The cross-app **connection barrier** — wait for the app's transport to be
online before driving a control the app itself desensitizes while it is not.

Why this module exists
----------------------
Every app greys an ``OnlineOnly`` affordance while its transport word is one of
the *known* offline words — shared rule, ``fauna_protocol::offline_class::
affordance`` (`account-offline-mutation.md` § The offline-mutation contract,
class 3 — partitioned out of `account-data-plane.md` 2026-09-06).
``"connecting"`` is one of those words, and 44 % of the registered wire kinds
(276 of 628) are ``OnlineOnly``. So a test that drives an online-only control on
a **freshly launched** app is racing the WS handshake and loses exactly when the
box is loaded: the app refuses the actuation with a named 409 and the test fails
for a reason that has nothing to do with what it was asserting.

Nothing in the harness could wait that out — before this module, ``grep
connection_state`` over ``drivers/``, ``actions/`` and ``helpers/`` returned
zero hits. What existed instead were five hand-rolled ``_wait_connected``
copies polling the *localized* ``connection-status`` indicator text for a
``"Connected"`` prefix, each returning ``None`` (a vacuous pass) on any app that
does not paint the indicator. This is their one shared, non-localized
replacement.

Two of the five are gone (the web-only pair, once web's leg landed): they now
wrap :func:`wait_until_online` and keep only the *local* half of their
diagnosis — what that test in particular loses when the transport never comes
up. The other three wait on apps whose legs are still open, and a swap there
would silently *remove* a real wait (see :data:`APPS_PUBLISHING_CONNECTION`),
so they stay until those legs land.

The polarity is shared Rust's, never re-derived here
----------------------------------------------------
The gate's own predicate is ``!OFFLINE_STATE_WORDS.contains(word)`` — *online
unless the word is a KNOWN offline word*, so an older app meeting a future state
word keeps its controls live rather than greying them. A barrier that waited for
equality with ``"connected"`` would therefore **hang to its full ceiling on
exactly the case the gate was designed to tolerate**, and would drift from the
gate the first time either side gained a variant. So the apps publish the
verdict already decided (``fauna_e2e_agent::connection_json`` →
``fauna_protocol::offline_class::is_online``) and this module waits on a boolean
it never interprets. **Nothing in ``tests/e2e-unified/`` may carry its own list
of the offline words** — that list has exactly one owner, in Rust, and
``test_connection_barrier.py`` fails if a second one appears.

Conventions
-----------
Point 14 (assert latency-independent state): a named generous budget plus a
deadline poll, never a settle-sleep — the app's state is the observable, and the
budget is a ceiling on a handshake, not a guess at its duration.

Point 11 (a dropped command must be loud): publishing ``{"state": "connecting",
"online": false}`` and publishing nothing at all are **different answers**. An
app whose leg has landed but whose transport never comes up fails loudly, naming
the last word it saw; an app whose leg has not landed yet is named in
:data:`APPS_PUBLISHING_CONNECTION` — never silently treated as online.
"""

from __future__ import annotations

import time

# ``fauna_e2e_agent::CONNECTION_KEY`` — top-level in the app's ``state`` blob, at
# one depth on every app, like every other cross-app observable.
CONNECTION_KEY = "connection"

# The apps whose in-app agent publishes :data:`CONNECTION_KEY`.
#
# This set is the reason a missing observable cannot silently degrade into a
# no-op: an app listed here that stops publishing fails loudly (see
# ``test_connection_observable.py``), and an app NOT listed here is a declared,
# tracked absence rather than an assumption that it is online. **It only ever
# grows** — the one remaining leg is android's declaration. Apple's leg (macos + ios) landed 2026-08-29; windows' landed 2026-09-07.
#
# ⚠ **Landing a leg and listing it here are two different acts.** android
# *publishes* the key as of 2026-08-28 (`TestAgent.kt`, compile-verified) and is
# still absent from this set: android e2e runs only on the emulator host, which is
# blocked on setup, so no run has yet watched it come online. Listing an app on
# a leg nobody has watched is not optimism, it is a claim that fails EVERY test
# on that app at login if it is wrong — so an app joins this set in the commit
# whose ``test_connection_observable.py`` run went green on it, never before.
#
# A Rust app publishes ``fauna_e2e_agent::connection_json(<its own transport
# word>)`` and is done. An app on the far side of a language boundary builds the
# same two keys itself, taking the boolean from the shared predicate's own face
# — ``connection_is_online`` over UniFFI (android/windows/apple),
# ``connectionIsOnline`` over wasm (web) — and **never** from a local
# comparison, which is the one way this observable can go quietly wrong.
APPS_PUBLISHING_CONNECTION = frozenset({"tui", "linux", "web", "macos", "ios", "windows"})

# A ceiling on a WS handshake against a locally-built nest on a saturated dev
# box — deliberately far above any non-pathological handshake (which is tens of
# milliseconds when the box is idle), because the *only* thing a tight budget
# buys is the flake this module exists to delete. Reaching it means the
# transport genuinely never came up, which is a real failure worth 60 s.
ONLINE_BUDGET_S = 60.0

_POLL_S = 0.2


class ConnectionBarrierTimeout(AssertionError):
    """The app published a connection state and it never became online."""


def connection_observable(driver) -> dict | None:
    """The app's raw ``{"state": word, "online": bool}``, or ``None`` when the
    app publishes nothing there (no leg yet, or not yet booted far enough)."""
    value = driver.get_state(CONNECTION_KEY)
    return value if isinstance(value, dict) else None


def app_publishes_connection(driver) -> bool:
    """Is this app's connection leg landed? (Declared, not sniffed — a sniff
    would read a not-yet-booted app as "no leg" and skip the barrier exactly
    when it is needed.)"""
    from helpers.app_surface import app_name

    return app_name(driver) in APPS_PUBLISHING_CONNECTION


def wait_until_online(driver, *, timeout: float = ONLINE_BUDGET_S) -> str | None:
    """Block until the app's transport is online; return the final state word.

    Returns ``None`` — immediately, without polling — on an app whose leg has
    not landed (:data:`APPS_PUBLISHING_CONNECTION`). Raises
    :class:`ConnectionBarrierTimeout` when an app that *does* publish never
    comes online within ``timeout``, naming the last word it reported so the
    failure diagnoses itself (conventions point 6) instead of reading "timed
    out".
    """
    if not app_publishes_connection(driver):
        return None

    deadline = time.monotonic() + timeout
    last: dict | None = None
    while True:
        last = connection_observable(driver)
        if last is not None and last.get("online") is True:
            return last.get("state")
        if time.monotonic() >= deadline:
            break
        time.sleep(_POLL_S)

    from helpers.app_surface import app_name

    if last is None:
        raise ConnectionBarrierTimeout(
            f"{app_name(driver)} is declared to publish "
            f"`{CONNECTION_KEY}` (helpers/connection.py:APPS_PUBLISHING_CONNECTION) "
            f"but its /app/state carried no such key for {timeout:.0f}s. Either "
            f"the app's state provider regressed (it must publish "
            f"`fauna_e2e_agent::connection_json(<its transport word>)`) or the "
            f"app never booted far enough to publish state at all."
        )
    raise ConnectionBarrierTimeout(
        f"{app_name(driver)}'s transport never came online: still "
        f"{last.get('state')!r} after {timeout:.0f}s. Every `OnlineOnly` "
        f"affordance stays desensitized while the word is offline "
        f"(`fauna_protocol::offline_class::affordance`), so any control this "
        f"test was about to drive would refuse with a named 409."
        f"{_app_log_evidence(driver)}"
    )


def _app_log_evidence(driver) -> str:
    """The app's own loud log lines, for a barrier that timed out.

    Convention 6. "Still disconnected" names the symptom and nothing else, and on
    windows the evidence dies with the fixture: ``teardown()`` deletes the data
    dir the app log lives in, so a setup error that does not quote the log leaves
    nothing to read afterwards (measured 2026-09-28, a whole module's setup
    cascade with no cause in the output). Filtered to WARN/ERROR/panic because
    the log kept growing for the whole budget and a plain tail is healthy
    background chatter. Duck-typed on ``app_stderr_text``, as
    ``helpers.waiting.await_session_actor`` is: a driver without it costs
    nothing.
    """
    reader = getattr(driver, "app_stderr_text", None)
    if not callable(reader):
        return ""
    try:
        text = reader() or ""
    except Exception as exc:  # noqa: BLE001 - diagnostics must never mask the failure
        return f" (app log unreadable: {exc})"
    loud = [
        ln for ln in text.splitlines()
        if any(m in ln for m in ("ERROR", "WARN", "panic", "PANIC"))
    ]
    return (
        "\napp log (loud lines, last 40):\n" + "\n".join(loud[-40:])
        if loud else ""
    )

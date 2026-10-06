"""Hold one WS-RPC kind's reply pending, so a test can stand inside a client's
*pre-fetch* window (`bins/fauna-nest/src/rpc_hold_test_hook.rs`,
``--features test-hooks``).

A whole class of goal-doc rule is about what an app shows BEFORE a read has
answered: `settings.md` § Privacy sub-page ("Until ``inbox_mode_get`` has
answered, the mode is *unknown*"), `family-safety.md` § Cold start. Their
failure mode is a client that paints a plausible **guess** and corrects itself a
round trip later — invisible to every post-fetch assertion in the suite, because
by the time one looks the guess is gone.

Holding the reply turns that window from an interval you race into a state you
can stand in. Why a hold rather than freezing the nest (forbidden — a
session-scoped fixture every other test shares) or shortening a debounce (moves
the window, does not open it) is the Rust module's own doc.

**Use it in three steps, and always release in a ``finally``** — an armed kind
parks every later request of that kind too, including the ones the tests after
this one issue::

    arm_rpc_hold(port, "fauna.inbox.mode.get")
    try:
        settings.request_privacy()
        wait_for_held_rpc(port, "fauna.inbox.mode.get")   # arrival, not a sleep
        assert settings.get_inbox_mode() == ""            # the pre-fetch pin
    finally:
        release_rpc_hold(port, "fauna.inbox.mode.get")

**Arm before the process boundary when the window you want is a COLD one.** An
app that already holds the value is not guessing, it is remembering, and
re-entering the page does not unlearn it — so a cold-start pin must relaunch
*inside* the armed window, not before it. Arming after the relaunch races the
app's own start-up read and loses whenever a preceding test left the app
restoring that page (measured on linux, 2026-08-27: passed run-alone, failed
behind two siblings, having come back already holding the real mode with the
nest never asked again).

``wait_for_held_rpc`` is the step that makes it deterministic rather than merely
slower: without it the assertion could run before the app had even sent the
request, and would then be measuring the app's startup speed. It is a deadline
poll on latency-independent state (``holding >= 1``), not a settle-sleep —
`e2e-conventions.md` point 14.

Lives in ``helpers/`` rather than in one journey because it is a nest surface
that any app's leg of any pre-fetch rule can drive.
"""
from __future__ import annotations

import json
import time
import urllib.request

#: Generous ceiling for "the app has issued the request". Sized far above any
#: non-pathological page-open latency, because the value is irrelevant to
#: correctness — a green run returns on the first poll and only a genuine
#: failure ever spends the budget (`e2e-conventions.md` point 14).
HELD_ARRIVAL_S = 60.0


def _post(port: int, path: str) -> dict:
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=b"",
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    return json.loads(urllib.request.urlopen(req).read() or b"{}")


def arm_rpc_hold(port: int, kind: str) -> None:
    """Park every subsequent request of ``kind`` until :func:`release_rpc_hold`.

    ``kind`` is the wire kind the nest registers (``fauna.inbox.mode.get``), not
    the client-side method name — the nest rejects an unregistered one with a
    400 rather than silently arming a gate nothing will ever reach, which would
    surface much later as "the app never sent the request".
    """
    _post(port, f"/api/v1/test/rpc-hold/{kind}")


def rpc_hold_status(port: int, kind: str) -> dict:
    """``{"armed", "refusing", "dropping", "holding", "released", "refused",
    "dropped"}`` for ``kind``.

    ``holding`` counts the requests parked *right now* — the arrival observable.
    ``released``, ``refused`` and ``dropped`` are cumulative over the nest
    process's life.
    """
    with urllib.request.urlopen(
        f"http://127.0.0.1:{port}/api/v1/test/rpc-hold/{kind}"
    ) as resp:
        return json.loads(resp.read())


def wait_for_held_rpc(port: int, kind: str, *, count: int = 1,
                      timeout: float = HELD_ARRIVAL_S,
                      diagnose=None) -> None:
    """Block until at least ``count`` requests of ``kind`` are parked.

    This is the step that makes a pre-fetch assertion deterministic: it proves
    the app has actually issued the read and the nest is actually holding it, so
    whatever the UI shows next is the genuine pending-state paint rather than a
    frame captured before the request was ever sent.

    ``diagnose`` is a zero-arg callable appended to the failure message. Pass one
    that dumps the app's own view of where it is: "no request arrived" has three
    very different causes — the app is not on the page, the app is not connected
    to the nest, or the app issues a different kind — and the nest-side counters
    can distinguish none of them on their own (`e2e-conventions.md` point 6).
    """
    deadline = time.monotonic() + timeout
    status: dict = {}
    while time.monotonic() < deadline:
        status = rpc_hold_status(port, kind)
        if status.get("holding", 0) >= count:
            return
        time.sleep(0.05)
    extra = ""
    if diagnose is not None:
        try:
            extra = f" {diagnose()}"
        except Exception as e:  # a diagnostic must never mask the real failure
            extra = f" (diagnose raised {e!r})"
    raise AssertionError(
        f"no request of kind {kind!r} reached the nest within {timeout:.0f}s "
        f"(last status {status!r}). Either the app never reached the page that "
        f"issues this read, or it is not connected to the nest, or it issues a "
        f"different kind — check the nest-side router registration for the "
        f"exact spelling.{extra}"
    )


def refuse_rpc(port: int, kind: str) -> None:
    """Answer every request of ``kind`` with an error reply until
    :func:`release_rpc_hold` — the parked ones included.

    The failed-request sibling of the hold: the nest turns the request away
    (``fauna.test.refused``, rendered as the generic "Something went wrong"
    string) without running its handler, so a test can witness what an app does
    with a failure no ordinary input provokes — a composer's failed send
    (`feed.md` § Errors & edge cases). Release it in a ``finally`` exactly like
    a hold: a refusing kind turns away every later test's requests too.
    """
    _post(port, f"/api/v1/test/rpc-hold/{kind}/refuse")


def wait_for_refused_rpc(port: int, kind: str, *, count: int = 1,
                         timeout: float = HELD_ARRIVAL_S) -> None:
    """Block until at least ``count`` requests of ``kind`` have been refused —
    the proof the app's request reached the nest and was turned away, so what
    the UI shows next is its reaction to the failure (a deadline poll on
    latency-independent state, `e2e-conventions.md` point 14)."""
    deadline = time.monotonic() + timeout
    status: dict = {}
    while time.monotonic() < deadline:
        status = rpc_hold_status(port, kind)
        if status.get("refused", 0) >= count:
            return
        time.sleep(0.05)
    raise AssertionError(
        f"no request of kind {kind!r} was refused within {timeout:.0f}s "
        f"(last status {status!r}) — the app never sent it, or sent another kind."
    )


def drop_rpc_reply(port: int, kind: str) -> None:
    """Run every request of ``kind`` for real, then close its connection
    instead of replying, until :func:`release_rpc_hold`.

    The lost-reply sibling: unlike :func:`refuse_rpc` the handler DOES run and
    whatever it commits stays committed — the caller just never hears so, and
    meets a transport failure (the connection closes 4401, the revocation
    teardown, with the Reply never written). The state
    ``identity-succession.md``'s *lost submit reply* ruling is about: a request
    that took effect while the app was told it failed. Client-plane kinds only
    (the nest 400s a federation kind). Release it in a ``finally``.
    """
    _post(port, f"/api/v1/test/rpc-hold/{kind}/drop-reply")


def wait_for_dropped_rpc(port: int, kind: str, *, count: int = 1,
                         timeout: float = HELD_ARRIVAL_S) -> None:
    """Block until at least ``count`` replies of ``kind`` have been dropped —
    the proof the handler ran and its caller was cut off, so what the UI shows
    next is its reaction to the lost reply (a deadline poll on
    latency-independent state, `e2e-conventions.md` point 14)."""
    deadline = time.monotonic() + timeout
    status: dict = {}
    while time.monotonic() < deadline:
        status = rpc_hold_status(port, kind)
        if status.get("dropped", 0) >= count:
            return
        time.sleep(0.05)
    raise AssertionError(
        f"no reply of kind {kind!r} was dropped within {timeout:.0f}s "
        f"(last status {status!r}) — the app never sent it, sent another kind, "
        f"or sent it on a plane that cannot drop a reply."
    )


def release_rpc_hold(port: int, kind: str) -> int:
    """Disarm ``kind`` and wake every parked request; returns the cumulative
    release count.

    Idempotent and safe on a kind that was never armed, so it belongs in a
    ``finally`` unconditionally — leaving a kind armed leaks into every test
    that runs after this one on the same session-scoped nest.
    """
    return int(_post(port, f"/api/v1/test/rpc-hold/{kind}/release").get("released", 0))


def drop_rpc_reply_once(port: int, kind: str) -> None:
    """Drop the reply of the NEXT request of ``kind`` only, then answer as usual.

    :func:`drop_rpc_reply` for one request: its handler runs for real, its
    connection closes in place of the Reply, and the gate reopens by itself —
    no release needed — so the app's own retry of the same request goes
    through. The *brief* drop a client is meant to ride out
    (`mail-credentials.md` § Partial-state-during-minting): with the
    open-ended drop the retry is dropped too, and "the retry finishes" would
    hinge on releasing inside the app's backoff window, a wall-clock race.
    :func:`wait_for_dropped_rpc` is the arrival proof. Release it in a
    ``finally`` anyway: a drop the app never triggered stays armed for the
    next test's request of the kind.
    """
    _post(port, f"/api/v1/test/rpc-hold/{kind}/drop-reply-once")

"""tier_3: an in-place session patch retires the client it replaces, so one
live client is left to sign in after a nest restart.

The e2e agent's ``session`` patch is the harness's account-switch shortcut: a
second authenticated patch naming a different actor, with no ``reset`` before
it. ``account-scoping.md`` § The scoping taxonomy, corollary 2, binds it as it
binds the real switcher: the background loops that write the outgoing scope's
state are retired by the same drop that removes it. The loop that matters here
is the replaced client's WS-RPC supervisor. Left running, it redials after a
nest restart (which forgets every bearer) and signs in again, so one restart
costs one sign-in per leaked client, each spending the failed-credential bucket
(``transport-connection.md`` § Abuse posture → *The failed-credential
throttle*); a whole-module ``test_family.py`` run on macos measured the sum as
``Handshake status 429``.

The observable is the nest's own ``challenge-response sign-in handle=<h>``
line (``bins/fauna-nest/src/auth_core.rs``), read from the dedicated nest's
log. Measured on macos (2026-11-04): before the fix, the restart drew TWO
sign-ins under the incoming handle and none under the outgoing one (the leaked
supervisor presents the incoming credential); with the fix, one. So the
assertion is the whole tally: incoming exactly once, outgoing never.

Latency (convention 14): the incoming actor's first re-sign-in is the causal
anchor, polled under a budget, never a settle-sleep. A leaked supervisor
redials on the same backoff as the live one (0.9 s apart, measured), so it
shows up within ``_LEAK_WINDOW_S`` of the anchor. That window only guards a
red. Under heavy load a leak could slip past it and give a false green, but a
correct client can never turn the test red.

The dedicated nest keeps the shared session nest out of the restart, and its
log holds only this module's sign-ins.
"""

from __future__ import annotations

import re
import time

import pytest

pytestmark = [pytest.mark.tier_3]

_RESIGN_BUDGET_S = 60.0
_LEAK_WINDOW_S = 20.0


_ANSI = re.compile(r"\x1b\[[0-9;]*m")


def _sign_ins(log_text: str, handle: str) -> int:
    # The nest's tracing output is coloured (`handle` and `=` each sit inside
    # their own escape codes), so strip them before matching.
    pattern = re.compile(
        r"challenge-response sign-in.*\bhandle=" + re.escape(handle) + r"\b"
    )
    plain = _ANSI.sub("", log_text)
    return sum(1 for line in plain.splitlines() if pattern.search(line))


def _log(nest) -> str:
    with open(nest["log_path"], "r", errors="replace") as f:
        return f.read()


@pytest.mark.macos
@pytest.mark.ios
def test_an_in_place_actor_patch_retires_the_outgoing_client(app, dedicated_no_mail_nest):
    from common.nest import restart_nest
    from conftest import _make_user
    from helpers.connection import wait_until_online
    from helpers.e2e_session import e2e_login_device_id, login_as

    nest = dedicated_no_mail_nest
    outgoing = nest["user"]
    incoming = _make_user(nest)
    device_id = e2e_login_device_id(app)

    login_as(app, nest, outgoing, handle=outgoing["handle"], device_id=device_id)
    wait_until_online(app.driver)
    # The in-place switch: a second authenticated patch, no `reset` before it.
    login_as(app, nest, incoming, handle=incoming["handle"], device_id=device_id)
    wait_until_online(app.driver)

    restart_nest(nest, graceful=True)
    # The restarted process truncates the log (`_spawn_and_wait` opens it "w"),
    # so every sign-in line read below is the new process's.

    deadline = time.monotonic() + _RESIGN_BUDGET_S
    while time.monotonic() < deadline:
        if _sign_ins(_log(nest), incoming["handle"]) >= 1:
            break
        time.sleep(0.5)
    else:
        pytest.fail(
            f"the incoming actor {incoming['handle']!r} never signed in again within "
            f"{_RESIGN_BUDGET_S:.0f} s of the nest restart: the live client did not "
            "reconnect, so the sign-in tally below would prove nothing. "
            f"app error: {app.error_text()!r}"
        )

    window_end = time.monotonic() + _LEAK_WINDOW_S
    counts = (0, 0)
    while time.monotonic() < window_end:
        text = _log(nest)
        counts = (_sign_ins(text, incoming["handle"]), _sign_ins(text, outgoing["handle"]))
        if counts != (1, 0):
            break
        time.sleep(0.5)
    assert counts == (1, 0), (
        f"after the nest restart the incoming actor {incoming['handle']!r} signed in "
        f"{counts[0]} time(s) and the outgoing actor {outgoing['handle']!r} "
        f"{counts[1]} time(s); one live client signs in once and nothing signs in "
        "for the outgoing actor. Every sign-in past that is a supervisor the "
        "in-place session patch left running when it built the incoming "
        "FaunaClient (account-scoping.md § The scoping taxonomy, corollary 2), "
        "and every one of them spends the failed-credential bucket."
    )

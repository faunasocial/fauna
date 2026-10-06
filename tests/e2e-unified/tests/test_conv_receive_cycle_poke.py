"""Tier_3 single-app proof of convention 14's receive-loop run-now poke +
cycle observable (``fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`` /
``::CONV_RECEIVE_NOW``, the D9 build-out —
``docs/goal/architecture/e2e-conventions.md`` § convention 14 mechanism 3):
the client receive loop runs a fresh cycle when poked, independent of its
30 s backstop ticker.

Deliberately app-agnostic and single-device: the mechanism under test (poke →
counters move) needs no peer and no MLS handshake — tui/linux/web start
``ConversationsSession``'s receive loop for every logged-in session, mock or
real backend alike. **apple and windows do NOT**: both native shells gate
``startReceiveLoop``/``StartReceiveLoop`` behind ``FAUNA_E2E_REAL_CONVERSATIONS``
(the ``real_conversations`` marker) even in their default e2e login path, so
this module carries the marker itself: on those two apps the loop then runs
even when this file is the only one selected (the flag flips session-wide,
`conftest._apply_real_conversations_env`), and on tui/linux/web the marker is a
no-op because their launch branches never read it. Without it the file only
passed when some marked sibling happened to share the pytest invocation, and a
solo run — the one-file-per-process sweep the windows ledger uses — timed out
with the loop never started, which read as a leg gap. The two-client
MLS delivery proofs (``test_fauna_mls_two_client_inbox_drain.py`` and
friends) exercise the SAME poke as part of a real delivery, but their
``second_real_faunamls_app`` fixture is linux/tui-only today, so THIS test is
what actually reaches every app that owns the leg.
"""

from __future__ import annotations

import pytest

from helpers.budgets import RECEIVE_CYCLE_S
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


def test_conv_receive_now_pokes_a_fresh_cycle(logged_in_app):
    """A poke's `completed` cycle passes the pre-poke `started` baseline
    within budget. `await_receive_cycle_after` is itself the whole proof: a
    missing leg raises naming the app + key (convention 11), and a leg that
    publishes but never actually sweeps times out naming the gap."""
    app = logged_in_app
    cycles = conv_receive_cycles(app.driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(app.driver)
    await_receive_cycle_after(
        app.driver, started, budget_s=RECEIVE_CYCLE_S, what="the poked cycle",
    )

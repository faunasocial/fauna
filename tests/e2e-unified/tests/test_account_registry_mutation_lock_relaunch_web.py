"""tier_3 e2e: web's registry mutation lock on the two paths that run AFTER a
page boots — the aftermath drain's compare-and-clear.

``docs/goal/architecture/long-term-store.md`` § Multi-account evolution →
*Cross-process mutation lock*, the web leg: every wasm-facing mutator entry runs
inside ``with_web_mutation_lock``. ``test_account_registry_mutation_lock_web.py``
witnesses the Settings gesture; this file witnesses the drain's delete, which no
gesture reaches: the post-store-ready ledger pass
(``fauna_client_recovery::ledger_aftermath::run_ledger_aftermath``) drains the
ceremony the succession fold parked in the registry, and clears the park only if
it still holds what the pass drained (``AccountRegistry::
clear_aftermath_ceremony_if_serialized``). That compare-and-delete is one
``fauna/<actor>/pending_aftermath_ceremony`` read and delete; unserialized, a
re-park of a wider roster by a sibling tab landing between the two is deleted
unraised.

**The world.** The origin store holds a parked ceremony that does not decode —
the drain's ``Voided`` arm, which clears the park without raising anything, so
no nest round trip or attested predecessor is needed. A sibling tab holds the
registry lock; this tab reopens (a reload over the preserved ``localStorage``,
``driver.recover()``) and its launch runs the ledger pass.

**What isolates the write.** The observable is the park key, read at the moment
the pass is AT its clear. The drain writes a console line immediately before the
clear (a ``tracing`` warning on the undecodable arm), and an unguarded clear runs
in the same task, so by the time the test has read that line the park is already
gone on a tree without the wrap — while with the wrap it is still there, with the
sibling's hold standing and a request queued behind it. The pass cannot start
until the launch's own registry writes have run, and those queue behind the
holder too, so the holder CYCLES the lock (``HANDOFF_LOCK_JS``: a fresh hold is
queued behind everything pending, then the old one released) instead of dropping
it. A bare "a request is pending" reading would not do: the launch's other
locked writes pend as well, and such a test passes on a tree without the wrap
(measured). Convention 14: state, not elapsed time.

tier_3: needs a real ``fauna-nest`` binary; web app only (the mechanism is the
Web Locks API — every native app takes the kernel file lock inside the call).
"""
from __future__ import annotations

import json
import time

import pytest

from helpers.web_tabs import (
    GRANTED_LOCK_JS,
    HANDOFF_LOCK_JS,
    HOLD_LOCK_JS,
    RELEASE_LOCK_JS,
    REGISTRY_LOCK,
    lock_queue,
    seed_origin_store_from_session,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

# Convention 14: a named, generous ceiling on latency-independent state (a page
# boot plus the ledger pass's store-ready edge), never a settle-sleep.
BUDGET_S = 120.0

#: A park the drain cannot decode: its ``Voided`` arm clears it without any
#: raise, so the clear is the pass's only registry write to this key.
UNDECODABLE_PARK = "not-a-parked-ceremony"

#: The drain's own console line for that arm
#: (``ledger_aftermath::drain_parked_ceremony``), written immediately before its
#: ``clear_if_unchanged`` — the observable that the pass is AT the clear.
DRAIN_LOG_MARK = "the parked aftermath ceremony does not decode"


def _park(driver, actor: str):
    """``actor``'s parked aftermath ceremony straight off the origin store."""
    return driver.eval_js(
        f"localStorage.getItem({json.dumps(f'fauna/{actor}/pending_aftermath_ceremony')})"
    )


@pytest.mark.feature("multiple-accounts")
def test_the_aftermath_drains_clear_waits_for_a_sibling_tabs_registry_lock(logged_in_app):
    """A launch's aftermath drain clears its parked ceremony only once the
    sibling tab holding the registry lock lets go."""
    app = logged_in_app
    holder = None
    try:
        # (1) A sibling tab takes the registry lock FIRST and holds it. The
        # signed-in page may still be running its own launch's ledger pass; that
        # pass must find the lock held too, so nothing can clear the park before
        # the hold is in place.
        holder = app.driver.open_lock_holder_tab()
        assert holder.eval_js(HOLD_LOCK_JS) is True
        queue = lock_queue(holder)
        assert REGISTRY_LOCK in queue["held"], (
            f"the sibling tab never got the registry lock; the origin's queue reads {queue!r}"
        )

        # (2) The origin store carries the identity (so the reopened tab boots
        # signed in) and a park the drain will clear.
        session = seed_origin_store_from_session(app.driver)
        actor = session["actor_id"]
        key = f"fauna/{actor}/pending_aftermath_ceremony"
        app.driver.eval_js(
            f"localStorage.setItem({json.dumps(key)},{json.dumps(UNDECODABLE_PARK)})"
        )
        assert _park(app.driver, actor) == UNDECODABLE_PARK

        # (3) The app reopens over the preserved store; its launch runs the
        # ledger pass, which reaches the drain's clear.
        assert app.driver.recover(), "the app did not come back up after a relaunch"

        # (4) The reopened tab's launch queues its own registry writes behind the
        # holder, and the ledger pass cannot start until they run. So the lock is
        # CYCLED, never dropped: each hand-off queues a fresh hold behind every
        # request already pending, then releases the old one, so whatever the
        # launch has queued runs while any request made later (the pass's clear)
        # still finds the lock held. The cycling ends at the drain's own log line,
        # written BEFORE its clear (``drain_parked_ceremony``'s undecodable arm).
        deadline = time.monotonic() + BUDGET_S
        reached = False
        while time.monotonic() < deadline:
            if any(DRAIN_LOG_MARK in line for line in app.driver.console_log()):
                reached = True
                break
            queue = lock_queue(holder)
            if REGISTRY_LOCK in queue["pending"] and holder.eval_js(GRANTED_LOCK_JS) is True:
                assert holder.eval_js(HANDOFF_LOCK_JS) is True
            time.sleep(0.5)
        assert reached, (
            f"the ledger pass never reached the aftermath drain within {BUDGET_S}s: "
            f"queue={lock_queue(holder)!r}, park={_park(holder, actor)!r}, "
            f"error={app.error_text()!r}"
        )

        # (5) The drain is AT its clear. An unguarded clear runs in the same task
        # as the log line, so it is already done by the time the line is read: the
        # park must still be there, with the lock still held, and the clear
        # waiting for it.
        parked = _park(holder, actor)
        queue = lock_queue(holder)
        assert parked == UNDECODABLE_PARK, (
            "the aftermath park was CLEARED while a sibling tab held the registry "
            f"lock {REGISTRY_LOCK!r} — the drain's compare-and-clear never asked "
            f"for the lock. park={parked!r} queue={queue!r}"
        )
        assert REGISTRY_LOCK in queue["held"] and REGISTRY_LOCK in queue["pending"], (
            "the drain's clear is not queued behind the sibling's hold: "
            f"queue={queue!r}"
        )

        # (6) The sibling lets go → the queued writes drain, and among them the
        # drain's clear: the park goes.
        assert holder.eval_js(RELEASE_LOCK_JS) is True
        deadline = time.monotonic() + BUDGET_S
        parked = UNDECODABLE_PARK
        while time.monotonic() < deadline:
            parked = _park(holder, actor)
            if parked is None:
                break
            time.sleep(0.5)
        assert parked is None, (
            "the aftermath drain never cleared its park after the sibling released "
            f"the lock: park={parked!r}, queue={lock_queue(holder)!r}, "
            f"error={app.error_text()!r}"
        )
    finally:
        if holder is not None:
            holder.teardown()

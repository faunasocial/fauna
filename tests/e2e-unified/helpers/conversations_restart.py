"""Shared helpers for the conversations restart-survival tests.

One home for the poll/settle machinery three witnesses share:
``test_conversations_history_survives_app_restart.py`` (windows, mls.db
data-dir localization), ``test_conversations_own_message_survives_an_immediate_restart.py``
(windows, durable-before-done), and
``test_conversations_history_survives_restart_cross_app.py`` (every column via
the CR-1 store pin). The docstrings carry the load-bearing reasoning; the
importing tests state only what differs.
"""

from __future__ import annotations

import time


def fauna_thread(app, channel_hex=None, flavor="OneToOne", exclude=()):
    """The FaunaMls thread with ``flavor`` (optionally pinned to ``channel_hex``).

    ``exclude`` is a set of channel hexes to ignore — the session-scoped
    ``test_user``/``nest_instance`` mean threads from *earlier tests in the same
    session* restore into this app too (durable-before-done makes them durable),
    so a positional pick without it can grab another test's thread.
    """
    threads = app.conversations.list_threads()
    matches = [
        t
        for t in threads
        if t.rail == "FaunaMls"
        and t.flavor == flavor
        and t.channel_id_hex not in set(exclude)
        and (channel_hex is None or t.channel_id_hex == channel_hex)
    ]
    assert matches, (
        f"no FaunaMls {flavor} thread"
        f"{f' on channel {channel_hex}' if channel_hex else ''} "
        f"(have {[(t.rail, t.flavor) for t in threads]})"
    )
    return matches[-1]


def wait_replica_settled(client_factory, path, quiet_s=4.0, timeout=40.0):
    """Poll ``fauna.mls.get(path)`` until its sealed blob is present AND has
    stopped changing for ``quiet_s``.

    Presence alone is NOT a sufficient gate. ``snapshot_replica`` walks every
    *bound* channel, and `bootstrap_group` binds the channel (``fauna_mls.rs``)
    well before ``manager.send`` appends the own message — so the 1.5 s
    ``REPLICA_DEBOUNCE`` routinely fires mid-bootstrap and seals a **message-less**
    ``history/<ch>`` slice. That empty blob is non-null, so a presence check would
    green immediately and let the restart race the *real* (post-append) save,
    making a zero-bubble result impossible to attribute.

    The blob is ``BackupKey``-sealed, so the test cannot read its contents; the
    honest proxy is quiescence — the sealed bytes stop changing once the debounced
    autosave has flushed everything it is going to flush.
    """
    deadline = time.time() + timeout
    last, stable_since = None, None
    while time.time() < deadline:
        with client_factory() as c:
            blob = c.call("fauna.mls.get", {"path": path}).get("blob")
        if blob is not None:
            now = time.time()
            if blob != last:
                last, stable_since = blob, now
            elif now - stable_since >= quiet_s:
                return
        time.sleep(0.5)
    raise AssertionError(
        f"replica path {path!r} never settled within {timeout}s "
        f"(present={last is not None}) — the autosave did not flush, so the "
        "restart would prove nothing"
    )


def wait_fauna_thread(app, channel_hex, timeout=45.0):
    """Poll the snapshot until the restored FaunaMls thread on ``channel_hex``
    appears. Returns the summary, or ``None`` on timeout."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        for t in app.conversations.list_threads():
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex:
                return t
        time.sleep(0.5)
    return None


def wait_rendered_messages(app, want, timeout=15.0):
    """Poll the opened thread's rendered bubbles until ``want`` are present.
    Returns the final count (which may be < ``want`` on timeout)."""
    deadline = time.time() + timeout
    count = 0
    while time.time() < deadline:
        count = app.driver.count("dm-message-text")
        if count >= want:
            return count
        time.sleep(0.5)
    return count

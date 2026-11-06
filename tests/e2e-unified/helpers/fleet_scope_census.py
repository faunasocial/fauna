"""Who fills a shared account's fleet scope — a per-test census, read at rest.

Owner of the cap and the gate: ``docs/goal/architecture/fleet-scope-reclamation.md``
§ *Fleet-scope reclamation* (a scope holds at most 4096 live entries; clause (1),
the retention gate, withholds every retire above the lowest walk mark whose key
still holds a live grant). Owner of the harness rule this watches:
``docs/goal/architecture/e2e-launch-isolation.md`` convention 10, *A discarded
driver retires its machine — on a shared account*.

**Why it exists.** A whole-suite sweep's session account (``test_user``) met the
cap twice — on 2026-10-12, and again on 2026-10-30 after convention 10 had
landed — and the second time nothing in the log could say which writer filled
it: the entries are sealed, a failing test's captured output shows only the
pump's own report, and a passing test shows nothing. So after every test that
resolved the session nest this reads that nest's ``nest.db`` (read-only, never
a write) for every shared actor and keeps, in memory:

* the scope's live entry count, its tip seq, and the gate's watermark — the
  same ``MIN(mark)`` over counted walkers ``retirable_through_seq`` computes;
* every writer (``origin_writer``) and every walker (``state_walk_marks``) the
  first time it appears, attributed to the test after which it was first seen;
* every *gate hold* — a stretch of consecutive reads in which one counted walker
  kept the watermark at one mark below the tip. A live walker's mark moves with
  its next walk; one that stays put across tests is a machine that stopped
  walking with its grant still live, and every retire above it is withheld
  while it holds. The final read alone cannot show a hold that ended before the
  run did (the 2026-10-30 sweep's fill came from one such hold), so the spans
  are kept as they happen.

``pytest_terminal_summary`` prints the outcome: the tests that grew the scope
most, the writers holding most live rows (each with its grant state and the test
that introduced it), and the counted walkers whose mark sits below the tip — the
ones pinning the gate at the end — and the gate holds that lasted longest or
grew the scope most, each naming the walker and the test it was first seen
after; a hold reaching ``HOLD_REPORT_TESTS`` reads is also printed to stderr as
it happens, so a run killed before its summary still names it. A count, never a duration (convention 14). Observation
only: it never fails a test, and a read that cannot run turns the census off for
the rest of the run with one line saying why.
"""

from __future__ import annotations

import sqlite3

FLEET_SCOPE_FOLDER = "__state-fleet"
"""The reserved folder backing the ``state-fleet`` scope
(``bins/fauna-nest/src/db/account_state.rs::find_state_scope``: ``__`` + scope)."""

STATE_ENTRY = "state-entry"
"""``fauna_protocol::account_state::ItemClass::StateEntry`` on the wire."""

TOP = 15
"""Rows printed per table in the summary."""

HOLD_REPORT_TESTS = 3
"""Consecutive reads at one (walker, mark) below the tip that make a gate hold
worth reporting — one or two is an ordinary walk lag between tests."""

_state: dict = {
    "disabled": None,  # the reason, once a read failed
    "snapshots": {},  # (db_path, actor) -> last {"live", "tip", "watermark"}
    "growth": {},  # (actor, nodeid) -> live-row delta
    "writers": {},  # (actor, writer hex) -> nodeid first seen after
    "walkers": {},  # (actor, walker hex) -> nodeid first seen after
    "peak": {},  # actor -> (live, nodeid)
    "final": {},  # actor -> the last full read (for the summary)
    "holds": {},  # actor -> [span], the last one open while it holds
}


def _open(db_path: str):
    return sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=5.0)


def _read(conn, actor: bytes) -> dict | None:
    row = conn.execute(
        "SELECT id FROM folders WHERE name = ?1 AND actor_id = ?2",
        (FLEET_SCOPE_FOLDER, actor),
    ).fetchone()
    if row is None:
        return None
    folder = row[0]
    live, tip = conn.execute(
        "SELECT SUM(superseded_at IS NULL), MAX(seq) FROM sync_changes "
        "WHERE folder_id = ?1 AND item_class = ?2",
        (folder, STATE_ENTRY),
    ).fetchone()
    granted = (
        "EXISTS (SELECT 1 FROM sync_devices d WHERE d.actor_id = ?2 "
        "AND d.auth_device_key = {k}) AND NOT EXISTS (SELECT 1 FROM "
        "revoked_device_grants r WHERE r.actor_id = ?2 AND r.auth_device_key = {k})"
    )
    writers = conn.execute(
        "SELECT origin_writer, COUNT(*), "
        + granted.format(k="c.origin_writer")
        + " FROM sync_changes c WHERE c.folder_id = ?1 AND c.item_class = 'state-entry' "
        "AND c.superseded_at IS NULL GROUP BY origin_writer",
        (folder, actor),
    ).fetchall()
    walkers = conn.execute(
        "SELECT m.walker, m.seq, " + granted.format(k="m.walker")
        + " FROM state_walk_marks m WHERE m.folder_id = ?1",
        (folder, actor),
    ).fetchall()
    counted = [seq for _w, seq, live_grant in walkers if live_grant]
    return {
        "live": int(live or 0),
        "tip": int(tip or 0),
        "watermark": min(counted) if counted else None,
        "writers": [((w or b"").hex(), int(n), bool(g)) for w, n, g in writers],
        "walkers": [((w or b"").hex(), int(s), bool(g)) for w, s, g in walkers],
    }


def observe(nest: dict | None, nodeid: str) -> None:
    """Census every shared actor on ``nest`` after the test ``nodeid``."""
    if _state["disabled"] or not nest or not nest.get("db_path"):
        return
    from helpers import shared_identity

    actors = shared_identity.shared_actors()
    if not actors:
        return
    db_path = nest["db_path"]
    try:
        conn = _open(db_path)
        try:
            for actor in sorted(actors):
                census = _read(conn, bytes.fromhex(actor))
                if census is not None:
                    _record(db_path, actor, nodeid, census)
        finally:
            conn.close()
    except (sqlite3.Error, OSError, ValueError) as exc:
        _state["disabled"] = f"{type(exc).__name__}: {exc}"


def _record(db_path: str, actor: str, nodeid: str, census: dict) -> None:
    key = (db_path, actor)
    before = _state["snapshots"].get(key)
    if before is not None and census["live"] != before["live"]:
        delta_key = (actor, nodeid)
        _state["growth"][delta_key] = (
            _state["growth"].get(delta_key, 0) + census["live"] - before["live"]
        )
    _state["snapshots"][key] = census
    for writer, _n, _g in census["writers"]:
        _state["writers"].setdefault((actor, writer), nodeid)
    for walker, _s, _g in census["walkers"]:
        _state["walkers"].setdefault((actor, walker), nodeid)
    _track_hold(actor, nodeid, census)
    peak = _state["peak"].get(actor)
    if peak is None or census["live"] > peak[0]:
        _state["peak"][actor] = (census["live"], nodeid)
    _state["final"][actor] = census


def _track_hold(actor: str, nodeid: str, census: dict) -> None:
    mark = census["watermark"]
    spans = _state["holds"].setdefault(actor, [])
    open_span = spans[-1] if spans and spans[-1]["open"] else None
    if mark is None or mark >= census["tip"]:
        if open_span:
            open_span["open"] = False
        return
    holder = min(w for w, s, g in census["walkers"] if g and s == mark)
    if open_span and open_span["walker"] == holder and open_span["mark"] == mark:
        open_span.update(to=nodeid, tests=open_span["tests"] + 1, live_to=census["live"])
    else:
        if open_span:
            open_span["open"] = False
        open_span = {
            "walker": holder, "mark": mark, "from": nodeid, "to": nodeid, "tests": 1,
            "live_from": census["live"], "live_to": census["live"], "open": True,
            "first_seen": _state["walkers"].get((actor, holder), "?"),
        }
        spans.append(open_span)
    if open_span["tests"] == HOLD_REPORT_TESTS:
        import sys

        print(
            f"[fleet-scope census] actor {actor[:12]}: gate held at mark {mark} by walker "
            f"{holder[:16]} (first seen after {open_span['first_seen']}) since "
            f"{open_span['from']}",
            file=sys.__stderr__,
            flush=True,
        )


def report_lines() -> list[str]:
    """The summary, one line each; empty when nothing was observed."""
    lines: list[str] = []
    if _state["disabled"]:
        lines.append(f"census stopped: {_state['disabled']}")
    for actor, census in sorted(_state["final"].items()):
        peak_live, peak_at = _state["peak"][actor]
        live_writers = [w for w in census["writers"] if w[2]]
        dead_writers = [w for w in census["writers"] if not w[2]]
        lines.append(
            f"actor {actor[:12]}: {census['live']} live entries (peak {peak_live} "
            f"after {peak_at}), tip seq {census['tip']}, gate watermark "
            f"{census['watermark']}; writers holding a live grant "
            f"{len(live_writers)} ({sum(n for _w, n, _g in live_writers)} rows), "
            f"writers without one {len(dead_writers)} "
            f"({sum(n for _w, n, _g in dead_writers)} rows)"
        )
        grew = sorted(
            ((d, n) for (a, n), d in _state["growth"].items() if a == actor),
            reverse=True,
        )[:TOP]
        if grew:
            lines.append("  tests that grew the scope most (live-entry delta):")
            lines += [f"    {d:+5d}  {n}" for d, n in grew]
        lines.append("  writers holding the most live entries:")
        for writer, n, granted in sorted(census["writers"], key=lambda w: -w[1])[:TOP]:
            lines.append(
                f"    {n:5d}  {writer[:16]} grant={'live' if granted else 'none'} "
                f"first seen after {_state['writers'].get((actor, writer), '?')}"
            )
        pinning = sorted(
            (w for w in census["walkers"] if w[2] and w[1] < census["tip"]),
            key=lambda w: w[1],
        )[:TOP]
        if pinning:
            lines.append("  counted walkers below the tip (the lowest pins the gate):")
            for walker, seq, _g in pinning:
                lines.append(
                    f"    mark {seq:6d}  {walker[:16]} first seen after "
                    f"{_state['walkers'].get((actor, walker), '?')}"
                )
        holds = sorted(
            (h for h in _state["holds"].get(actor, []) if h["tests"] >= HOLD_REPORT_TESTS),
            key=lambda h: (-(h["live_to"] - h["live_from"]), -h["tests"]),
        )[:TOP]
        if holds:
            lines.append("  gate holds (one walker at one mark below the tip, read after read):")
            for h in holds:
                lines.append(
                    f"    mark {h['mark']:6d}  {h['walker'][:16]} over {h['tests']} reads, live "
                    f"{h['live_from']}->{h['live_to']}, from {h['from']} to {h['to']}; walker "
                    f"first seen after {h['first_seen']}"
                )
    return lines

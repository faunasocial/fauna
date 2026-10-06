#!/usr/bin/env python
"""Turn a permissive actuation marker log into the two things grading needs:
the VALIDITY verdict, and the offender table to triage.

Convention 11 grades a permissive sweep in a fixed order, and the order matters
because the failure modes look like success:

  Signal 2 FIRST -- the known-positive control's marker must be present. A sweep
  whose detector never armed and a genuinely clean sweep produce IDENTICAL logs
  (apple's first iOS sweep read "1483 tests observed, 0 violating calls --
  CLEAN" while 567 tests had silently skipped and the app never launched). So
  this script REFUSES to call a log clean unless the probe's marker is in it.

  Then the offender table, grouped per ELEMENT, because the triage question is
  asked per element and not per call: is the predicate stricter than the real UI
  (a registration bug), or did the harness drive what a user cannot (a test
  bug)? A single element with twelve calls is one decision, not twelve. A test
  that drives a disabled control to assert the refusal itself
  (`_DELIBERATE_REFUSALS`) is listed apart, never offered for triage.

`--tests-observed` is the count from the run's own pytest summaries; passing it
lets the report state coverage alongside validity, the way convention 11's
per-app implementation note records it.
"""

import argparse
import json
import re
import sys

#: `=== TEST <nodeid>` -- pytest stamps one per test so every marker below it
#: can name the test that produced it.
_TEST = re.compile(r"^=== TEST (?P<nodeid>\S.*)$")

#: The marker the agents and bridges emit, pinned byte-for-byte on every host
#: (windows' bridge mirrors the shared Rust text rather than calling it).
_MARK = re.compile(
    r"DISABLED-ACTUATION (?P<route>\S+) id=(?P<id>\S+) index=(?P<index>\d+)"
)

#: The elements each app's known-positive control drives on purpose. The FIRST
#: EVERY one of them is a signal-2 witness -- each marker must be present or
#: the log is not an enumeration. Requiring all of them, not just the first, is
#: the same argument one layer in: convention 11 says gating click alone is not
#: compliance, so a probe that drives a disabled button AND a disabled picker
#: (windows, to cover the click and the select route) proves only the click
#: detector when only the button's marker arrives -- and an empty offender list
#: for every select call in the sweep is then indistinguishable from a select
#: gate that never ran. All of them are also excluded from the offender table:
#: the probe drives a disabled control BY DESIGN, so counting it as an offender
#: would hand every future grader one phantom element to triage.
_PROBE_ELEMENTS = {
    "windows": ("restore-confirm-button", "restore-source-select"),
    "linux": ("restore-confirm-button",),
    "macos": ("restore-confirm-button",),
    "ios": ("restore-confirm-button",),
}

#: Tests that drive a disabled control ON PURPOSE outside the probe, because the
#: refusal itself is what they assert. Keyed by (test module, element), never by
#: element alone: the same element driven from any other test is a real offender.
#: Under `--permissive-actuation` the gate marks such a call like any other (and
#: the test then fails its own `pytest.raises`, since permissive wins over a
#: seat's strict opt-in), so it lands in every permissive sweep of every app the
#: test runs on. Listed here, it is reported once as DELIBERATE instead of being
#: re-triaged by every grader. Unlike a probe element it proves nothing about the
#: detector: a module that skipped, or died before its refusal step, never marks.
_DELIBERATE_REFUSALS = {
    # A plain member's Remove chip is greyed AND refused (`conversations.md`
    # § Architectural rules 5) -- the windows sweep of 2026-09-11 offered it as
    # its one offender.
    ("tests/test_conversation_room_roles.py", "thread-member-chip"),
}


def is_deliberate(element, nodeid):
    """Is this marker a test asserting the refusal on purpose?"""
    module = (nodeid or "").split("::", 1)[0].replace("\\", "/")
    return (module, element) in _DELIBERATE_REFUSALS


def parse(path):
    """-> (violations, tests_stamped). A violation is (element, route, nodeid)."""
    violations, stamped, current = [], [], None
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.rstrip("\n")
            m = _TEST.match(line)
            if m:
                current = m.group("nodeid")
                stamped.append(current)
                continue
            m = _MARK.search(line)
            if m:
                violations.append((m.group("id"), m.group("route"), current))
    return violations, stamped


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("marker_log")
    ap.add_argument("--app", default="windows")
    ap.add_argument("--tests-observed", type=int, default=None,
                    help="tests the run actually executed, for the coverage line")
    ap.add_argument("--progress", help="the chunk runner's JSONL progress file")
    ap.add_argument("--expect-chunks", type=int,
                    help="how many chunks the plan has; with --progress this is "
                         "signal 1 (did the whole sweep actually run)")
    args = ap.parse_args()

    violations, stamped = parse(args.marker_log)
    probe_els = _PROBE_ELEMENTS.get(args.app, ())
    probe_el = probe_els[0] if probe_els else None
    probe_hits = [v for v in violations if v[0] in probe_els]

    print(f"=== actuation sweep report — {args.marker_log} (--app {args.app}) ===")
    print(f"test boundaries stamped : {len(stamped)}")
    if args.tests_observed is not None:
        print(f"tests observed          : {args.tests_observed}")
    print(f"violating calls         : {len(violations)}")

    complete = None
    single_tree = None
    if args.progress:
        done = []
        try:
            with open(args.progress, encoding="utf-8") as fh:
                for line in fh:
                    line = line.strip()
                    if line:
                        done.append(json.loads(line))
        except FileNotFoundError:
            done = []
        print("\n--- SIGNAL 1: did the whole sweep run ---")
        ran = sorted(r["index"] for r in done)
        print(f"  chunks finished: {len(done)}"
              + (f" of {args.expect_chunks}" if args.expect_chunks else ""))
        if args.expect_chunks is not None:
            missing = [i for i in range(args.expect_chunks) if i not in ran]
            complete = not missing
            if missing:
                print(f"  ❌ INCOMPLETE — chunk(s) {missing} never finished.")
                print("     The log below covers only what ran. Resume with")
                print(f"     --start-at {missing[0]} before grading it as a sweep.")
            else:
                print("  ✅ COMPLETE — every planned chunk finished.")
        bad = [r for r in done if r["rc"] not in (0, 1)]
        if bad:
            print("  ⚠ chunk(s) with an unusual exit code (not 0/1): "
                  + ", ".join(f"{r['index']}:rc={r['rc']}" for r in bad))

        # Signal 3: one sweep, one tree.
        trees = {}
        for r in done:
            t = r.get("tree")
            if t:
                trees.setdefault(t, []).append(r["index"])
        if len(trees) > 1:
            single_tree = False
            print("\n--- SIGNAL 3: one sweep, one tree ---")
            print(f"  ❌ SPLIT — chunks ran against {len(trees)} different trees:")
            for t, idxs in sorted(trees.items(), key=lambda kv: min(kv[1])):
                print(f"      {t[:12]}  chunks {sorted(idxs)}")
            newest = max(trees, key=lambda t: max(trees[t]))
            older = [i for t, idxs in trees.items() if t != newest for i in idxs]
            print("     A module swept before a rebase was never measured against")
            print("     the code that shipped. Re-run the pre-rebase chunk(s) "
                  f"{sorted(older)}")
            print("     on the settled tree — into a FRESH marker log, so their")
            print("     first pass's markers are not counted twice.")
        elif trees:
            single_tree = True
            print("\n--- SIGNAL 3: one sweep, one tree ---")
            print(f"  ✅ SINGLE TREE — every chunk ran at {list(trees)[0][:12]}")

        # A chunk whose tree MOVED WHILE IT RAN is the split a per-chunk stamp
        # cannot otherwise see: both stamps taken at the end would agree, and
        # the run would read as clean. `tree_start` is what makes it visible.
        # Chunks written before `tree_start` existed simply omit it.
        straddlers = [
            r for r in done
            if r.get("tree_start") and r.get("tree") and r["tree_start"] != r["tree"]
        ]
        if straddlers:
            single_tree = False
            print("  ❌ STRADDLED — chunk(s) had the tree move UNDER them mid-run:")
            for r in straddlers:
                print(f"      chunk {r['index']} ({r['name']}): "
                      f"{r['tree_start'][:12]} -> {r['tree'][:12]}")
            print("     Those chunks measured neither tree whole. Re-run them on")
            print("     the settled tree, into a FRESH marker log.")

    print("\n--- SIGNAL 2: the known-positive control ---")
    def _witness(el):
        hits = [v for v in probe_hits if v[0] == el]
        routes = sorted({r for _, r, _ in hits})
        return f"{el} marked {len(hits)}x (routes: {', '.join(routes)})"

    witnessed = {el for el, _, _ in probe_hits}
    missing = [el for el in probe_els if el not in witnessed]
    if probe_el is None:
        print(f"  UNKNOWN — no probe element registered for --app {args.app}")
        valid = False
    elif not missing:
        for el in probe_els:
            print(f"  ✅ PRESENT — {_witness(el)}")
        print(f"     the detector was armed on all {len(probe_els)} probe "
              "element(s)")
        valid = True
    elif not witnessed:
        print(f"  ❌ ABSENT — no marker for {', '.join(missing)}.")
        print("     This log is NOT an enumeration. A sweep whose detector never")
        print("     ran is indistinguishable from a clean one — chase the probe")
        print("     first and do not read anything below as a finding.")
        valid = False
    else:
        print(f"  ❌ HALF-ARMED — the probe drives {len(probe_els)} element(s) "
              f"and {len(missing)} never reached the log:")
        for el in missing:
            print(f"       {el}")
        for el in probe_els:
            if el in witnessed:
                print(f"     witnessed: {_witness(el)}")
        print("     Gating one route is not compliance, so an empty offender")
        print("     list for the unwitnessed route(s) is indistinguishable from")
        print("     a detector that never ran there. Chase the missing probe(s)")
        print("     first and do not read anything below as a finding.")
        valid = False

    beyond_probe = [v for v in violations if v[0] not in probe_els]
    deliberate = [v for v in beyond_probe if is_deliberate(v[0], v[2])]
    others = [v for v in beyond_probe if not is_deliberate(v[0], v[2])]
    if deliberate:
        print("\n--- DELIBERATE (a test asserting the refusal itself — "
              "not an offender) ---")
        for el, route, nodeid in deliberate:
            print(f"  {el} via {route} — {nodeid}")
    print("\n--- OFFENDERS (per ELEMENT — one triage decision each) ---")
    if not others:
        if valid:
            print("  none — offender list EMPTY against an armed detector.")
        else:
            print("  none listed, but the detector is UNPROVEN (see signal 2).")
    else:
        by_el = {}
        for el, route, nodeid in others:
            by_el.setdefault(el, []).append((route, nodeid))
        for el in sorted(by_el, key=lambda e: (-len(by_el[e]), e)):
            calls = by_el[el]
            routes = sorted({r for r, _ in calls})
            tests = sorted({n for _, n in calls if n})
            print(f"\n  {el}  — {len(calls)} call(s), routes: {', '.join(routes)}")
            for t in tests:
                print(f"      {t}")
        print(f"\n  {len(by_el)} element(s) to triage.")

    print("\n--- VERDICT ---")
    if not valid:
        print("  INVALID as an enumeration — signal 2 failed.")
        return 2
    if complete is False:
        print("  PARTIAL — the detector is armed, but the sweep did not finish.")
        print("  Any element listed above is a real offender; an EMPTY list here")
        print("  means nothing yet, because the chunks that never ran cannot")
        print("  report what they would have found. Resume, then re-grade.")
        return 3
    if single_tree is False:
        print("  SPLIT — the detector was armed and every chunk ran, but not all")
        print("  against the same tree (signal 3). A listed element is real; an")
        print("  EMPTY list is not trustworthy, because the pre-rebase chunks")
        print("  never saw the shipped code. Re-run those, then re-grade.")
        return 4
    if others:
        print(f"  VALID enumeration; {len({v[0] for v in others})} element(s) "
              "to triage before the flip.")
        return 1
    print("  VALID enumeration, offender list EMPTY — ready to flip.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

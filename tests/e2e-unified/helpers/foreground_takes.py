"""Which tests still took the desktop's foreground — e2e convention 10's windows
focus axis (`docs/goal/architecture/e2e-launch-isolation.md`, axis (d)).

The FlaUI bridge records every gesture that moves the foreground onto the app
under test (`flaui-bridge/Actions.cs::RecordForegroundTake` — the physical
`SendInput` fallbacks and UIA `SetFocus`, which activates the window). The UIA
gesture paths never appear there, and a harness launch never activates its
window, so what this record lists is exactly the set of tests that still take the
keyboard focus from the person working in the same Windows session.

The conftest teardown hook drains the record after every test that has a driver
answering ``foreground_report`` (windows only), attributes the drained gestures to
that test, appends them to a gitignored corpus beside the harness, and the end of
the run prints the tally. It also notes a test that ended with the APP owning the
foreground although the bridge recorded no take — the app raised itself (a
picker, a dialog, an activation this axis missed), which the bridge's record
cannot see. Observation only — never a failure: a foregrounding fallback is a
named, legitimate path, and the dedicated case
(`tests/test_windows_no_focus_steal.py`) is what pins the UIA paths to zero.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

#: The corpus file, next to the harness (gitignored).
CORPUS_NAME = ".foreground-takes.log"
#: How many offending tests the terminal summary names; the corpus holds all.
SUMMARY_DETAIL_CAP = 15
#: The corpus line for "the app owns the foreground and no bridge gesture put it there".
SELF_RAISED = "<app owns the foreground with no bridge take: the app raised itself>"


@dataclass
class Drained:
    takes: list[str]
    self_raised: bool

    @property
    def entries(self) -> list[str]:
        return self.takes + ([SELF_RAISED] if self.self_raised else [])


class RunTally:
    """What one pytest run's foreground record said, for the terminal summary."""

    def __init__(self) -> None:
        self.observed = 0
        self.takers: list[tuple[str, list[str]]] = []

    def record(self, nodeid: str, drained: Drained) -> None:
        self.observed += 1
        if drained.entries:
            self.takers.append((nodeid, drained.entries))

    def summary(self, path: Path) -> list[str]:
        if not self.observed:
            return []
        lines = [
            f"[foreground] windows: {len(self.takers)} of {self.observed} test(s) "
            f"took the desktop's foreground (e2e convention 10, axis (d); "
            f"per-gesture record: {path})"
        ]
        for nodeid, entries in self.takers[:SUMMARY_DETAIL_CAP]:
            lines.append(f"  {nodeid}: {len(entries)} take(s), first: {entries[0]}")
        if len(self.takers) > SUMMARY_DETAIL_CAP:
            lines.append(f"  … and {len(self.takers) - SUMMARY_DETAIL_CAP} more")
        return lines


def drain(driver) -> Drained | None:
    """What the driver's bridge recorded since the last drain (emptying the
    record), plus whether the app owns the foreground with no take to explain it;
    ``None`` when the driver keeps no such record (every non-windows driver) or
    the bridge could not answer. Never raises — this observes a run and must never
    break it."""
    report_fn = getattr(driver, "foreground_report", None)
    if report_fn is None:
        return None
    try:
        report = report_fn(clear=True)
    except Exception:
        return None
    # A report without the key is not a bridge's answer (a stubbed tier_1
    # driver, an older bridge binary) — unobserved, never "zero takes".
    if not isinstance(report, dict) or "takes" not in report:
        return None
    takes = list(report["takes"] or [])
    owned = bool(report.get("app_owns_foreground"))
    # A foreground an EARLIER test's take left with the app is still the app's
    # until the person clicks elsewhere; only a fresh ownership with no take in
    # this window is the app raising itself.
    owned_before = getattr(driver, "_foreground_owned_at_last_drain", False)
    try:
        driver._foreground_owned_at_last_drain = owned
    except Exception:
        pass
    return Drained(takes, owned and not takes and not owned_before)


def format_lines(nodeid: str, drained: Drained) -> str:
    return "".join(f"{nodeid}\t{entry}\n" for entry in drained.entries)

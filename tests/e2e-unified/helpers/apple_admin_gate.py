"""Read the apple apps' own ``[admin-gate]`` / ``[admin-auto-default]`` log lines.

The admin auto-default is a four-link chain, and until this reader existed no
failing run could say which link broke::

    SettingsView/ContentView .task
      → FaunaClient.refreshAdminStatus        → "[admin-gate] no client → isAdmin=false"
      → api.amIAdmin()                        → "[admin-gate] am_i_admin=<bool>"
      → (only if true) FaunaAccounts.autoEnableRequireConfirmForActiveAdmin()
                                              → "[admin-auto-default] no active account …"
      → registry write                        → "[admin-auto-default] …auto-enabled…"
                                                 / "[admin-auto-default] refused …"

Every link logs, so the line SET is the diagnosis — including the empty set,
which is the one verdict no other witness can deliver: *the probe never ran at
all*. `session.is_admin` in the state snapshot cannot distinguish that from "it
ran and returned false", because both leave the flag `false`.

Source: the daily-rolling file the shared `fauna_log` ring writes under
``<data_dir>/logs/`` (`libs/fauna-log` `init`), where `data_dir` is the app's
Application Support dir — both apple app roots pass exactly that to
`installLogging` at startup (`FaunaApp.init` / `FaunaMacApp`). Both apple
drivers expose the same ``app_support_dir()``, so one reader serves macOS and
iOS with no platform branch (e2e convention 3).
"""
from __future__ import annotations

from pathlib import Path

#: The two prefixes every link of the chain logs under.
MARKERS = ("[admin-gate]", "[admin-auto-default]")

#: Returned when the log files exist but carry no chain line at all. Phrased as
#: the verdict, not the observation: this is the "probe never ran" case, and a
#: reader who has to infer that from "0 lines" will not.
NEVER_RAN = (
    "the am-i-admin probe NEVER RAN: no [admin-gate]/[admin-auto-default] line was "
    "ever logged, so refreshAdminStatus was not invoked at all"
)


def admin_gate_log(app_support_dir: str | None, limit: int = 12) -> list[str]:
    """The chain's log lines, oldest first, at most the last ``limit``.

    ``app_support_dir`` is the driver's ``app_support_dir()``. Best-effort by
    construction — a diagnostic must never mask the assertion it explains, so
    every failure to read comes back as an explanatory line rather than an
    exception (e2e rule 6).
    """
    if not app_support_dir:
        return ["(no app support dir on this launch — cannot read the app log)"]
    log_dir = Path(app_support_dir) / "logs"
    try:
        # `tracing_appender::rolling::daily` suffixes the date, so glob the stem
        # rather than naming today's file — a run crossing midnight has two.
        files = sorted(log_dir.glob("fauna.log*"))
    except OSError as exc:
        return [f"(log dir {log_dir} unreadable: {exc!r})"]
    if not files:
        return [f"(no fauna.log* under {log_dir} — the app never installed logging)"]
    hits: list[str] = []
    for path in files:
        try:
            with open(path, errors="replace") as fh:
                hits += [ln.strip() for ln in fh if any(m in ln for m in MARKERS)]
        except OSError as exc:
            hits.append(f"({path.name} unreadable: {exc!r})")
    if not hits:
        return [f"({NEVER_RAN})"]
    return hits[-limit:]

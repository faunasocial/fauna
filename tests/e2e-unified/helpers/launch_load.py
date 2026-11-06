"""What the machine did over a launch's window — a timed-out launch's fourth witness.

Convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention): the
machine over [+0, deadline] is read from this machine's own agent ledger, a local
file and no network, and, best-effort, the host's line for the same ticks from the
broker. Each absence is stated rather than guessed at, because the report this
feeds runs inside a `raise`: nothing here may raise, and nothing here may hold that
raise for long.

The ledger's location and its arithmetic are the fleet core broker's own, loaded
lazily and only on a failure path: a green run never pays for the import, and a
checkout without the fleet's tooling costs a stated absence rather than an
ImportError at collection.
"""

from __future__ import annotations

import datetime
import importlib.util
import json
import os
from dataclasses import dataclass
from pathlib import Path

#: How long the host's reading may hold a failure report. The broker answers a
#: one-page window in milliseconds; a broker that does not is an absence to state,
#: never a reason to delay the report of the launch that already failed.
HOST_READ_TIMEOUT_S = 3.0

_REPO_ROOT = Path(__file__).resolve().parents[3]
_broker_module = None


@dataclass
class MachineWindow:
    """The machine over one window. `unread` non-empty means the ledger could not
    answer and every reading below is None; `host_unread` is the same for the
    host's two readings alone."""

    ticks: int = 0
    unread: str = ""
    #: The narrowest and widest fence over the window, in CPUs, and the machine's
    #: number it was cut from. None where no tick ran under a fence.
    fence_cpus: int | None = None
    fence_cpus_max: int | None = None
    fence_of: int | None = None
    #: The lane's figure, which the fence is read against (`_LANE_CORES`).
    lane_k: int | None = None
    #: Means over the ticks.
    priv_pct: float | None = None
    phantom: float | None = None
    busy: float | None = None
    outside: float | None = None
    host_cores: float | None = None
    steal_host: float | None = None
    host_unread: str = ""


def _broker():
    """The fleet core broker as a module, once (its hyphen defeats `import`)."""
    global _broker_module
    if _broker_module is None:
        script = _REPO_ROOT / "scripts" / "core-broker.py"
        if not script.exists():
            raise FileNotFoundError(f"this checkout has no {script}")
        spec = importlib.util.spec_from_file_location("fauna_launch_load_broker", script)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _broker_module = module
    return _broker_module


def _mean(values) -> float | None:
    numbers = [v for v in values if isinstance(v, (int, float)) and not isinstance(v, bool)]
    return sum(numbers) / len(numbers) if numbers else None


def ledger_rows(path: str, since: float, until: float) -> list[dict] | None:
    """The ledger's rows with `since` <= ts <= `until` (epoch s), over the file and
    its rotated `.1`; None when neither exists. A torn or foreign line is skipped.

    Newest first, stopping at the first row older than the window: the rows are
    appended in time order and a launch's window is the file's tail, so the two
    seconds a full parse of the pair takes are not spent inside a `raise`."""
    rows: list[dict] = []
    found = False
    for name in (path, f"{path}.1"):
        try:
            with open(name, "rb") as fh:
                lines = fh.read().splitlines()
        except FileNotFoundError:
            continue
        found = True
        for raw in reversed(lines):
            try:
                row = json.loads(raw)
                at = datetime.datetime.fromisoformat(row["ts"]).timestamp()
            except (ValueError, KeyError, TypeError):
                continue
            if at < since:
                return rows
            if at <= until:
                rows.append(row)
    return rows if found else None


def _host_reading(broker, since: float, until: float) -> tuple[float | None, float | None, str]:
    """(`host_cores`, `steal_host`, why neither) for this machine's ticks."""
    token = broker.read_token()
    if not token:
        return None, None, f"no broker token at {broker.TOKEN_FILE}"
    name = broker._machine()
    try:
        ticks = broker.fetch_ticks(
            broker.BROKER_ADDR, token, since, until, machines=[name], timeout=HOST_READ_TIMEOUT_S
        )
    except (OSError, ValueError, broker.BrokerRefused) as exc:
        return None, None, f"the broker did not answer ({type(exc).__name__}: {exc})"
    cells = [(t.get("machines") or {}).get(name) or {} for t in ticks]
    cores = _mean(c.get("host_cores") for c in cells)
    steal = _mean(c.get("steal_host") for c in cells)
    if cores is None and steal is None:
        return None, None, "the broker holds no host reading for these ticks"
    return cores, steal, ""


def _read_window(since: float, until: float, apps, vcpus: int) -> MachineWindow:
    broker = _broker()
    path = os.path.join(broker.bs._slot_dir(), broker._PROBE_LEDGER)
    rows = ledger_rows(path, since, until)
    if rows is None:
        return MachineWindow(unread=f"no agent ledger at {path}")
    if not rows:
        return MachineWindow(unread=f"no tick in the window in {path} (is the core agent running?)")

    fences = [r["fence"] for r in rows if isinstance(r.get("fence"), dict)]
    cpus = [f["cpus"] for f in fences if isinstance(f.get("cpus"), int)]
    busy = [
        (r.get("user_pct", 0.0) + r.get("priv_pct", 0.0)) * vcpus / 100.0
        for r in rows
        if isinstance(r.get("priv_pct"), (int, float))
    ]
    usages = [(broker.ledger_usage(r), r) for r in rows]
    outside = [
        broker.owners_outside(usage, (r.get("user_pct", 0.0) + r.get("priv_pct", 0.0)) * vcpus / 100.0)
        for usage, r in usages
        if usage is not None
    ]
    host_cores, steal_host, host_unread = _host_reading(broker, since, until)
    return MachineWindow(
        ticks=len(rows),
        fence_cpus=min(cpus) if cpus else None,
        fence_cpus_max=max(cpus) if cpus else None,
        fence_of=max((f["of"] for f in fences if isinstance(f.get("of"), int)), default=None),
        lane_k=broker.bs._lane_cores(broker.bs.e2e_app_pool(apps)),
        priv_pct=_mean(r.get("priv_pct") for r in rows),
        phantom=_mean(broker.phantom({"usage": r}) for r in rows),
        busy=_mean(busy),
        outside=_mean(outside),
        host_cores=host_cores,
        steal_host=steal_host,
        host_unread=host_unread,
    )


def machine_window(since: float, until: float, *, apps, vcpus: int | None = None) -> MachineWindow:
    """The machine over [`since`, `until`] (epoch s) for a run launching `apps`.
    Never raises: it runs inside a `raise`, so what cannot be read is reported."""
    try:
        return _read_window(since, until, apps, vcpus or os.cpu_count() or 1)
    except Exception as exc:  # noqa: BLE001 - it must never mask the real failure
        return MachineWindow(unread=f"{type(exc).__name__}: {exc}")

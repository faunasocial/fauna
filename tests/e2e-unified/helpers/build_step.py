"""One artifact build step run to completion from inside a test or fixture."""

import subprocess

import pytest


def run_build_step(cmd, what, cwd=None):
    """Run one build step to completion; `pytest.fail` with its tail on error,
    else return its last 400 output lines (a step whose verdict lines the
    caller checks — the MSI's notices run).

    No wall-clock bound, the same shape as conftest's `_build_via_just`. Every
    step here is a compile or a link, and on a machine running several build
    slots at once, by design, a cold one legitimately outlasts any fixed bound.
    A `subprocess.run(timeout=…)` fails a build that is merely busy, and on
    Windows it cannot even do that promptly: it kills only the immediate child,
    then blocks draining the pipe until the grandchild cargo exits. Measured
    2026-09-26/27 on Windows: TimeoutExpired raised at 6789 s, 7723 s and 4257 s,
    with each build's result thrown away;
    measured 2026-10-31 on Linux: a Flatpak build killed at 2700 s while
    compiling its last two crates. Convention 9 is kept by
    `reap_descendants_of`: the whole tree dies with the run if pytest is reaped.
    `drain_pipes` keeps the pipe read for the child's life (convention 13) and
    holds the tail this failure quotes.
    """
    from drivers.port_util import (
        drain_pipes,
        popen_group_kwargs,
        reap_descendants_of,
        wait_pipes_drained,
    )

    print(f"[build-step] {what}", flush=True)
    proc = subprocess.Popen(
        cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, bufsize=1, **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    recent = drain_pipes(proc, maxlen=400)
    rc = proc.wait()
    wait_pipes_drained(proc, timeout=10.0)
    if rc != 0:
        pytest.fail(f"{what} failed (rc={rc}):\n" + "\n".join(recent))
    return list(recent)

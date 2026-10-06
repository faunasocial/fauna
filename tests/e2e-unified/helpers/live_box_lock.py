"""Cross-session serialization of e2e tests against a SHARED LIVE nest.

The live-nest tests (`FAUNA_LIVE_NEST_URL` set — believable-mail, CalDAV, port-25
inbound, mail-enable) **factory-reset and re-claim the single shared box** (e.g.
example.com). That box is a singleton with no server-side cross-session lock, so
two sessions running live tests against it at once clobber each other's
claim/reset state — observed 2026-06-03, when two sessions both drove the live
box and one's onboarding client died mid-factory-reset (memory
`live-box-e2e-pkill-faunadesktop-hazard`).

A machine-wide advisory `flock`, **keyed on the nest URL**, that makes the
second session's live test wait for the first instead of colliding. This is the
one machine-wide e2e lock left (the snap-Chromium flock was deleted 2026-07-14
with the singleton it guarded), and it follows the loud-bounded-lock convention
(testing.md § Cross-app e2e conventions, point 9) the 2026-07-13 wedge
incident ratified:

* **Loud** — a waiter immediately prints WHO holds the lock (pid + cwd, i.e.
  which checkout), read from the holder note the owner wrote into the lockfile,
  and re-prints it once a minute so a long queue is visibly a queue.
* **Bounded** — the wait fails with that diagnosis after
  `FAUNA_E2E_LIVE_BOX_LOCK_TIMEOUT` seconds (default 1800) instead of sleeping
  forever; "slow sibling" and "wedged sibling" become distinguishable, and the
  failure is a normal, diagnosable test error.

Scope + limits:
- **Keyed on the URL** so two *different* live boxes (or a session pointed at a
  local nest) don't serialize against each other — only same-box runs contend.
- **Machine-local** (an `flock` on a local file): it serializes sessions on ONE
  machine, which is the observed failure mode (many sessions on a shared dev
  machine). Coordinating two *different machines* against the same live box
  stays the user's manual concern — a local file lock can't reach across hosts.
- **POSIX-only**; a no-op on a non-POSIX host (the live tests run on Linux/macOS).
- The lock auto-releases if the holding process dies (kernel-backed `flock`), so
  a crashed sibling never wedges the fleet — unlike a stale lockfile. The holder
  note may then describe a dead pid until the next owner overwrites it; waiters
  annotate the printed holder with its liveness for exactly that case.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time

_DEFAULT_WAIT_S = 1800.0
_REPRINT_EVERY_S = 60.0


def _lock_path(nest_url: str) -> str:
    """Per-live-box lock path, keyed on the normalized URL. The directory is
    overridable via `FAUNA_E2E_LIVE_BOX_LOCK_DIR` (defaults to the real home,
    shared across this user's sessions regardless of any per-session TMPDIR)."""
    key = hashlib.sha256(nest_url.strip().rstrip("/").encode()).hexdigest()[:16]
    base = os.environ.get("FAUNA_E2E_LIVE_BOX_LOCK_DIR") or os.path.expanduser("~")
    return os.path.join(base, f".fauna-e2e-live-box-{key}.lock")


def _holder_note(fd: int) -> None:
    """(owner-side) Record who holds the lock, for waiters' diagnostics."""
    try:
        note = json.dumps({
            "pid": os.getpid(),
            "cwd": os.getcwd(),
            "since": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        })
        os.ftruncate(fd, 0)
        os.pwrite(fd, note.encode(), 0)
    except Exception:
        pass  # diagnostics only — never fail an acquire over the note


def _describe_holder(path: str) -> str:
    """(waiter-side) Human-readable holder description from the lockfile note."""
    try:
        with open(path, "rb") as f:
            note = json.loads(f.read() or b"{}")
        pid = note.get("pid")
        alive = "alive"
        # POSIX-only module (acquire() no-ops on Windows — no fcntl), so
        # os.kill(pid, 0) is the safe POSIX no-op probe here. It must stay
        # unreachable on Windows: there signal 0 == CTRL_C_EVENT and this would
        # Ctrl-C the whole console (Track J). If this lock is ever made
        # Windows-capable, switch to a signal-free probe like the machine-wide
        # build-slot tool's own pid-liveness check (OpenProcess).
        try:
            os.kill(int(pid), 0)
        except (ProcessLookupError, TypeError, ValueError):
            alive = "DEAD (note is stale; flock auto-released, retrying should win)"
        except PermissionError:
            pass
        return (
            f"pid {pid} ({alive}), cwd {note.get('cwd')!r}, since {note.get('since')}"
        )
    except Exception:
        return "unknown (no holder note in the lockfile)"


def acquire(nest_url: str, *, max_wait_s: float | None = None) -> int | None:
    """Wait (loudly, boundedly) until this process owns the live box `nest_url`.

    Returns the lock fd (pass it to `release()`), or None on a non-POSIX host
    where the lock is a no-op. Waiting queues concurrent live tests against the
    same box instead of letting them clobber each other; the bound turns a
    wedged sibling into a diagnosable TimeoutError naming the holder instead of
    an infinite silent sleep.
    """
    try:
        import fcntl
    except ImportError:
        return None
    if max_wait_s is None:
        try:
            max_wait_s = float(os.environ.get("FAUNA_E2E_LIVE_BOX_LOCK_TIMEOUT", ""))
        except ValueError:
            max_wait_s = _DEFAULT_WAIT_S
    path = _lock_path(nest_url)
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
    start = time.monotonic()
    last_print = -_REPRINT_EVERY_S
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            break
        except OSError:
            waited = time.monotonic() - start
            if waited - last_print >= _REPRINT_EVERY_S:
                last_print = waited
                print(
                    f"[live-box-lock] waiting {int(waited)}s for the shared live nest "
                    f"{nest_url} — held by {_describe_holder(path)}",
                    file=sys.stderr,
                    flush=True,
                )
            if waited >= max_wait_s:
                holder = _describe_holder(path)
                try:
                    os.close(fd)
                except Exception:
                    pass
                raise TimeoutError(
                    f"live-box lock for {nest_url} not acquired within "
                    f"{int(max_wait_s)}s — held by {holder}. If the holder is a "
                    "wedged run, its per-test timeout (pytest-timeout) should "
                    "release it shortly; if it is a legitimately long live run, "
                    "raise FAUNA_E2E_LIVE_BOX_LOCK_TIMEOUT or rerun later."
                )
            time.sleep(2.0)
    _holder_note(fd)
    return fd


def release(fd: int | None) -> None:
    """Release the lock fd returned by `acquire()` (no-op for None)."""
    if fd is None:
        return
    try:
        import fcntl

        fcntl.flock(fd, fcntl.LOCK_UN)
    except Exception:
        pass
    try:
        os.close(fd)
    except Exception:
        pass

"""The not-run refusal of the real-ambient-session category (`tests/real_session/`).

A run the box refuses — the ambient-session guard, or a precondition a test will
not break (a live Fauna desktop app owning the bus name it needs, which is live
work and never killed) — judged nothing, so it is not a red: it prints an
``UNRUN`` line and exits ``REAL_SESSION_UNRUN_EXIT``, loud and non-zero but
distinct from a test that ran and failed. The verify queue's tail declares the
same exit for a ``real_session/`` node and leaves the request standing for a later
pass (docs/goal/architecture/delayed-testing.md § The verify queue).
"""

import pytest

#: The verify queue's tail declares the same number for a `real_session/` node
#: (`VERIFY_TEST_REAL_SESSION_UNRUN_EXIT`), held equal by its tier_1 test. Outside pytest's own codes (0-5) and the root
#: conftest's long-run refusal (75).
REAL_SESSION_UNRUN_EXIT = 77


def hold_token(kind: str, name: str, pid: int) -> str:
    """The hold a refusal names: live work holding the precondition, which no
    session may break. The verify queue's drain parses it and, while its own
    probe reads the hold live, keeps the request standing with no try spent; a
    refusal naming no hold counts toward the bounded tries
    (docs/goal/architecture/delayed-testing.md § The verify queue)."""
    return f"hold: {kind} {name} pid {pid}"


def refuse_unrun(msg: str, hold: tuple[str, str, int] | None = None):
    """End the run as NOT RUN. Teardown of fixtures already set up still runs
    (pytest's session finish). `hold` = (kind, name, pid) names the live hold
    first in the line, so a clipped line still carries it."""
    if hold is not None:
        msg = f"{hold_token(*hold)} — {msg}"
    print(f"UNRUN: {msg}", flush=True)
    pytest.exit(f"UNRUN: {msg}", returncode=REAL_SESSION_UNRUN_EXIT)

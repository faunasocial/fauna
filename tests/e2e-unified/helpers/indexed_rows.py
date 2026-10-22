"""Read an ``indexed: true`` row family in either id form, while the apps move.

Convention 1 (``docs/goal/architecture/e2e-conventions.md``) gives an indexed
list item ONE repeated id, and a driver addresses row ``n`` by ``index=n`` —
never by an index baked into the id. Three families were specified and
painted the other way, one literal id per row (``<base>-0``, ``<base>-1``):
``admin-nest-oauth-key-item``, ``admin-nest-seed-rotate-roster-item`` and
``recover-box-item``. The owner ruled (2026-10-16) that all three move to the
bare repeated id, app by app, machine by machine, with the shared tests
reading BOTH forms until the last app has moved.

This module is that dual read, so no test spells either form itself. An app
that paints the bare id answers ``count(base) > 0`` whenever any row exists;
one that still bakes the index answers only ``count(f"{base}-{n}")``. Each
helper asks the bare form first and falls back to the suffixed one. Once
every app paints the bare id, the fallback goes and the callers keep
calling these helpers (or inline the bare reads) — the
``indexed-id-suffixed`` report's ``INDEXED_SUFFIX_PENDING`` list in
``scripts/lint-ui-registry.py`` names which apps still suffix.
"""

from __future__ import annotations

import time


def _suffixed(base: str, n: int) -> str:
    return f"{base}-{n}"


def row_count(driver, base: str) -> int:
    """How many rows of ``base`` the app has put in its tree, either form."""
    bare = driver.count(base)
    if bare:
        return bare
    n = 0
    while driver.count(_suffixed(base, n)) > 0:
        n += 1
    return n


def row_present(driver, base: str, n: int = 0) -> bool:
    """Whether row ``n`` of ``base`` is in the tree (tree membership — the
    ``count(...) > 0`` read the suffixed tests made)."""
    return driver.count(base) > n or driver.count(_suffixed(base, n)) > 0


def row_ref(driver, base: str, n: int = 0) -> tuple[str, int]:
    """``(element_id, index)`` addressing row ``n`` — pass it on as
    ``driver.click(*ref)`` / ``driver.get_text(*ref)``."""
    if driver.count(base) > 0:
        return base, n
    return _suffixed(base, n), 0


def row_text(driver, base: str, n: int = 0) -> str:
    element_id, index = row_ref(driver, base, n)
    return driver.get_text(element_id, index)


def row_texts(driver, base: str) -> list[str]:
    """Every row's text, in row order."""
    if driver.count(base) > 0:
        return driver.get_texts(base)
    rows: list[str] = []
    while driver.count(_suffixed(base, len(rows))) > 0:
        rows.append(driver.get_text(_suffixed(base, len(rows))))
    return rows


def click_row(driver, base: str, n: int = 0) -> None:
    element_id, index = row_ref(driver, base, n)
    driver.click(element_id, index)


def row_visible(driver, base: str, n: int = 0) -> bool:
    """The positive visibility read for row ``n``. The bare form has no
    per-index ``is_visible``, so past row 0 it reads tree membership."""
    bare = driver.count(base)
    if bare:
        return driver.is_visible(base) if n == 0 else bare > n
    return driver.is_visible(_suffixed(base, n))


def row_absent(driver, base: str, n: int = 0) -> bool:
    """The honest NEGATIVE read for row ``n`` (``is_absent``'s contract)."""
    bare = driver.count(base)
    if bare:
        return bare <= n
    return driver.is_absent(_suffixed(base, n))


def wait_for_row(driver, base: str, n: int = 0, timeout: float = 10.0) -> None:
    """Wait until row ``n`` of ``base`` is in the tree; then hand the visible
    wait to the driver's own ``wait_for`` (scroll-into-view and its
    self-diagnosing timeout) for whichever form the app paints. Raises
    ``TimeoutError`` naming both forms when neither ever appears."""
    deadline = time.monotonic() + timeout
    while not row_present(driver, base, n):
        if time.monotonic() >= deadline:
            raise TimeoutError(
                f"row {n} of indexed `{base}` never rendered within {timeout}s "
                f"in either id form — bare: {driver.diagnose(base)}; "
                f"suffixed: {driver.diagnose(_suffixed(base, n))}"
            )
        time.sleep(0.3)
    remaining = max(deadline - time.monotonic(), 1.0)
    if driver.count(base) > 0:
        if n == 0:
            driver.wait_for(base, timeout=remaining)
        return
    driver.wait_for(_suffixed(base, n), timeout=remaining)


def diagnose_row(driver, base: str, n: int = 0) -> str:
    """Convention 6's failure snapshot for row ``n``, both id forms."""
    return (f"bare `{base}`[{n}]: {driver.diagnose(base)}; "
            f"suffixed: {driver.diagnose(_suffixed(base, n))}")

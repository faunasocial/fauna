"""Read an ``indexed: true`` row family by its one repeated id.

Convention 1 (``docs/goal/architecture/e2e-conventions.md``) gives an indexed
list item ONE repeated id, and a driver addresses row ``n`` by ``index=n`` —
never by an index baked into the id. Three families were once specified and
painted the other way, one literal id per row: ``admin-nest-oauth-key-item``,
``admin-nest-seed-rotate-roster-item`` and ``recover-box-item``. The owner
ruled (2026-10-16) that all three move to the bare repeated id; the last app
moved on 2026-11-06, and the ``indexed-id-suffixed`` report in
``scripts/lint-ui-registry.py`` (its ``INDEXED_SUFFIX_PENDING`` list now
empty) reds any app that paints the suffixed form again.

These helpers read the bare id only. They stay as the one place the
per-index rules live: the bare form has no per-index ``is_visible``, so past
row 0 a visibility read is tree membership, and absence of row ``n`` is
``count(base) <= n``.
"""

from __future__ import annotations

import time


def row_count(driver, base: str) -> int:
    """How many rows of ``base`` the app has put in its tree."""
    return driver.count(base)


def row_present(driver, base: str, n: int = 0) -> bool:
    """Whether row ``n`` of ``base`` is in the tree (tree membership)."""
    return driver.count(base) > n


def row_ref(driver, base: str, n: int = 0) -> tuple[str, int]:
    """``(element_id, index)`` addressing row ``n`` — pass it on as
    ``driver.click(*ref)`` / ``driver.get_text(*ref)``."""
    return base, n


def row_text(driver, base: str, n: int = 0) -> str:
    return driver.get_text(base, n)


def row_texts(driver, base: str) -> list[str]:
    """Every row's text, in row order."""
    if driver.count(base) > 0:
        return driver.get_texts(base)
    return []


def click_row(driver, base: str, n: int = 0) -> None:
    driver.click(base, n)


def row_visible(driver, base: str, n: int = 0) -> bool:
    """The positive visibility read for row ``n``. The bare form has no
    per-index ``is_visible``, so past row 0 it reads tree membership."""
    if n == 0:
        return driver.count(base) > 0 and driver.is_visible(base)
    return driver.count(base) > n


def row_absent(driver, base: str, n: int = 0) -> bool:
    """The honest NEGATIVE read for row ``n`` (``is_absent``'s contract)."""
    return driver.count(base) <= n


def wait_for_row(driver, base: str, n: int = 0, timeout: float = 10.0) -> None:
    """Wait until row ``n`` of ``base`` is in the tree. Row 0 goes through
    the driver's own ``wait_for`` (scroll-into-view and its self-diagnosing
    timeout); a later row polls tree membership and raises ``TimeoutError``
    with the driver's diagnosis when it never appears."""
    if n == 0:
        driver.wait_for(base, timeout=timeout)
        return
    deadline = time.monotonic() + timeout
    while not row_present(driver, base, n):
        if time.monotonic() >= deadline:
            raise TimeoutError(
                f"row {n} of indexed `{base}` never rendered within {timeout}s"
                f" — {driver.diagnose(base)}"
            )
        time.sleep(0.3)


def diagnose_row(driver, base: str, n: int = 0) -> str:
    """Convention 6's failure snapshot for row ``n``."""
    return f"`{base}`[{n}]: {driver.diagnose(base)}"

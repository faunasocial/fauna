"""Module-boundary cold relaunch — a 7-app fixture contract, not a platform workaround.

Owner: `testing.md` § point 10 (a launch is isolated from the box it runs on).
A session-scoped app process accumulates state that the in-process ``reset()``
does not model, so the ``app`` fixture cold-relaunches the app at every test-
MODULE boundary: each module starts against a fresh instance — the same
baseline it would see run in isolation. ``reset()`` still runs per-test WITHIN
a module, and the reset probe (``FAUNA_E2E_RESET_PROBE``, conftest) stays the
standing detector for reset()-survivable state — it caught a real shared-Rust
factory-reset bug (`4827594821`); the relaunch is never its replacement.

A relaunch is the same MACHINE restarting its app, not a new machine, so two
things survive it on the drivers that carry them (convention 10). The first is
the signed-in actor's store principal — its slot AND its account-store replica,
together — restored at that actor's first sign-in in the new launch
(``drivers/http_bridge.py``, the principal-slot carry; linux, tui, windows,
macOS, iOS). A fresh slot per launch enrolled a new device row per module until
the session actor hit its device cap — ``tests/test_relaunch_device_accrual.py``
pins it — and a slot restored over a fresh replica is a key the app now abandons
on sight (``account-replica-posture.md`` § The store device principal,
refinement 11: a writer lives exactly as long as its journal), which is why the
two travel as one. The second is the install device secret the named sync row's
id is derived from, laid down at launch because it names no account (the
install-device-secret carry; linux and tui): without it every un-forced sign-in
registered a new named row per module.

History: built 2026-06-13 for the apple apps (XCUITest's accessibility tree
degrades across accumulated reset()s — a reset() that ACKS success can still
leave a corrupted tree) and type-gated ``is_macos() or is_ios()`` from then
on. But the justification was always platform-neutral, and the type list was
itself the drift (2026-08-02 ruling; ``testing.md`` § point 10 states the
contract, and git history carries the trail). The contract is therefore
driver-type-free: it asks ``driver.supports_cold_relaunch()`` (derived from
whether the class really overrides ``recover()`` — see
``drivers/http_bridge.py``) and routes through ``driver.recover()``, where
each platform's relaunch cost honestly lives. An app that cannot relaunch is
a DECLARED absence, announced in the run output and equality-pinned by
``tests/test_module_relaunch.py`` — never a resurrected type list.
"""

# Distinct from None: a module legitimately named None (a test node with no
# module) must record without ever counting as a crossable boundary.
_NEVER = object()


def at_module_boundary(driver, module_name):
    """Decide and perform the module-boundary relaunch for one test setup.

    Called by the ``app`` fixture before each test's ``reset()``. Returns a
    printable outcome line when something noteworthy happened (a relaunch, a
    failed relaunch, the first sight of a declared absence), else ``None``.
    A failed relaunch only reports — the fixture's ``reset()`` path owns
    surfacing the wedge, and state still advances so the failure costs one
    report, not a per-test relaunch storm.
    """
    last = getattr(driver, "_e2e_last_module", _NEVER)
    driver._e2e_last_module = module_name
    if last is _NEVER or last is None or module_name is None or module_name == last:
        return None
    if not driver.supports_cold_relaunch():
        if getattr(driver, "_e2e_relaunch_absence_told", False):
            return None
        driver._e2e_relaunch_absence_told = True
        return (
            "per-module app relaunch unavailable (declared absence: this "
            "driver's recover() cannot cold-relaunch the app; pinned by "
            "test_module_relaunch.py)"
        )
    if driver.recover():
        return f"per-module app relaunch ({last} -> {module_name})"
    return (
        f"per-module app relaunch FAILED ({last} -> {module_name}) — "
        "reset() below will surface the wedge"
    )

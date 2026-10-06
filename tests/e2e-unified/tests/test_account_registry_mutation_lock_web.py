"""tier_3 e2e: web's account-registry mutation lock — two tabs, one index, one writer.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*Web*, the mutation-section paragraph, and ``long-term-store.md``
§ Multi-account evolution → *Cross-process mutation lock*. Every registry
mutator is a read-modify-write over the single ``fauna/index`` blob every tab
of one origin shares, and ``localStorage`` offers no cross-tab transaction: two
tabs rewriting the index at once can lose one another's update. Native
serializes every mutator under an install-scoped file lock taken inside the
shared crate; web's leg is the origin-wide Web Lock ``fauna.accounts.migrate``
taken by the shared crate's wasm wrapper around every mutator, so a tab's
write **queues** behind any holder rather than racing it.

**How the witness holds the lock.** A second tab of the same profile takes the
registry lock through the same ``navigator.locks`` API the product uses and
keeps it until told to let go — the exact shape a sibling tab mid-mutation
presents. The first tab then flips its own account's ``account-require-confirm-
toggle``, a real user gesture whose write path is one index read-modify-write
(``accountsSetRequireConfirm`` → ``WasmAccountRegistry.setRequireConfirm``).

**Why the assertion is polarised (convention 14).** "The flag did not change for
N seconds" would be a wall-clock claim and would pass on a tab that never
wrote at all. The positive observable is the lock manager's own queue:
``navigator.locks.query()`` reports the first tab's request as *pending* on
``fauna.accounts.migrate`` while the second tab holds it — proof the mutator
asked for the lock — and the flag is still off in that same reading. Releasing
the lock then lets the write land, and the queue drains. A pre-fix tree never
requests the lock: its write lands while the sibling holds the name, which the
poll reports as the first failure it sees.

tier_3: needs a real ``fauna-nest`` binary; web app only (the mechanism is the
Web Locks API — every native app takes the kernel file lock instead).
"""
from __future__ import annotations

import json
import time

import pytest

from helpers.web_tabs import open_same_account_tab

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}
SWITCHER_ITEM = "account-switcher-item"
REQUIRE_CONFIRM_TOGGLE = "account-require-confirm-toggle"

#: The one origin-wide registry mutation lock. The name is the historical one
#: (`fauna_client_accounts::WEB_MUTATION_LOCK_NAME`), kept so tabs of an older
#: and a newer build still exclude each other.
REGISTRY_LOCK = "fauna.accounts.migrate"

# Convention 14: a named, generous ceiling on latency-independent state (a page
# boot plus one Web Locks round trip), never a settle-sleep.
BUDGET_S = 60.0

HOLD_LOCK_JS = f"""
(() => {{
  window.__faunaTestLockRelease = null;
  const held = new Promise((resolve) => {{ window.__faunaTestLockRelease = resolve; }});
  window.__faunaTestLockDone = navigator.locks.request(
    {json.dumps(REGISTRY_LOCK)}, {{ mode: 'exclusive' }}, () => held,
  );
  return true;
}})()
"""

RELEASE_LOCK_JS = """
(async () => {
  window.__faunaTestLockRelease();
  await window.__faunaTestLockDone;
  return true;
})()
"""

QUERY_LOCKS_JS = """
(async () => {
  const q = await navigator.locks.query();
  return { held: q.held.map((l) => l.name), pending: q.pending.map((l) => l.name) };
})()
"""


def _lock_queue(driver) -> dict:
    """The origin's Web Locks state as ``{"held": [...], "pending": [...]}`` —
    origin-wide, so either tab reads the same answer."""
    return driver.eval_js(QUERY_LOCKS_JS) or {"held": [], "pending": []}


def _require_confirm_flag(driver, actor: str):
    """``actor``'s persisted ``require_confirm_to_activate`` off the shared
    ``fauna/index`` — the headless observable of the write, read the way
    ``test_account_switcher_web.py`` reads it."""
    raw = driver.eval_js("localStorage.getItem('fauna/index')")
    index = json.loads(raw) if raw else {}
    for entry in index.get("accounts", []):
        if entry.get("actor_id") == actor:
            return entry.get("require_confirm_to_activate", False)
    return None


@pytest.mark.feature("multiple-accounts")
def test_a_registry_write_queues_behind_a_sibling_tabs_mutation_lock(logged_in_app):
    """One profile, two tabs: a registry write in tab A waits for the lock tab
    B holds, then lands when B lets go."""
    app = logged_in_app
    tab_b = None
    try:
        # (1) A second TAB of the same profile — one shared origin store, hence
        # one `fauna/index` — signed in as the same account. `open_same_account_tab`
        # seeds the origin store from the session, which is also what gives the
        # index the row the toggle below writes to.
        tab_b = open_same_account_tab(app.driver)
        session = (app.driver.get_state() or {}).get("session", {}) or {}
        actor = session.get("actor_id")
        assert actor, f"tab A reports no signed-in actor: {session!r}"

        # (2) Tab B takes the registry lock and holds it — the shape a sibling
        # mid-mutation presents — and the lock manager confirms it is held.
        assert tab_b.eval_js(HOLD_LOCK_JS) is True
        queue = _lock_queue(tab_b)
        assert REGISTRY_LOCK in queue["held"], (
            f"tab B never got the registry lock; the origin's queue reads {queue!r}"
        )

        # (3) Tab A lands on the Account settings page and flips its own row's
        # toggle — a real gesture whose write path is one index read-modify-write.
        app.driver.set_state(ACCOUNT_PAGE_NAV)
        app.driver.wait_for(SWITCHER_ITEM, timeout=BUDGET_S)
        assert _require_confirm_flag(app.driver, actor) is False, (
            "precondition: the account's re-auth flag must start OFF"
        )
        app.driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[0]")

        # (4) The write QUEUES: the lock manager reports tab A's request as
        # pending on the registry lock while tab B still holds it, and the flag
        # is still off in the same reading. A write that lands while the name is
        # held is the pre-fix behaviour, reported the moment it is seen.
        deadline = time.monotonic() + BUDGET_S
        queue, flag = None, None
        while time.monotonic() < deadline:
            flag = _require_confirm_flag(app.driver, actor)
            queue = _lock_queue(app.driver)
            assert flag is not True, (
                "tab A's registry write LANDED while tab B held the registry lock "
                f"{REGISTRY_LOCK!r} — the mutator never asked for the lock, so two "
                f"tabs can rewrite `fauna/index` at once. queue={queue!r}"
            )
            if REGISTRY_LOCK in queue["pending"]:
                break
            time.sleep(0.5)
        assert queue and REGISTRY_LOCK in queue["pending"], (
            "tab A's toggle write never showed up as a PENDING request on the "
            f"registry lock within {BUDGET_S}s: queue={queue!r}, flag={flag!r}, "
            f"error={app.error_text()!r}"
        )
        assert REGISTRY_LOCK in queue["held"], (
            f"tab B stopped holding the lock mid-test: queue={queue!r}"
        )

        # (5) Tab B lets go → tab A's write lands, and the queue drains.
        assert tab_b.eval_js(RELEASE_LOCK_JS) is True
        deadline = time.monotonic() + BUDGET_S
        while time.monotonic() < deadline:
            flag = _require_confirm_flag(app.driver, actor)
            if flag is True:
                break
            time.sleep(0.5)
        assert flag is True, (
            "tab A's queued write never landed after tab B released the lock: "
            f"flag={flag!r}, queue={_lock_queue(app.driver)!r}, "
            f"error={app.error_text()!r}"
        )
        deadline = time.monotonic() + BUDGET_S
        while time.monotonic() < deadline:
            queue = _lock_queue(app.driver)
            if REGISTRY_LOCK not in queue["pending"] and REGISTRY_LOCK not in queue["held"]:
                break
            time.sleep(0.5)
        assert REGISTRY_LOCK not in queue["pending"] and REGISTRY_LOCK not in queue["held"], (
            f"the registry lock is still held or queued after the write landed: {queue!r}"
        )
        # (6) And the row renders the persisted truth — the page re-reads the
        # registry after the write (`setRequireConfirm` → `loadAccounts`).
        assert app.driver.get_attr(REQUIRE_CONFIRM_TOGGLE, "data-state", scope=f"{SWITCHER_ITEM}[0]") == "on", (
            "the toggle does not render the flag the registry now carries"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()

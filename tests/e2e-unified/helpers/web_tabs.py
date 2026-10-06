"""Web's second-tab arrangement: make the origin store hold the signed-in
identity, so a tab opened next resolves it.

**Why this exists.** ``logged_in_app`` signs web in through the session patch
(``set_state``), which writes the IN-MEMORY identity store of the page the
driver holds — not ``localStorage``. That is invisible to every test that
drives one page, and fatal to every test that opens a second one: a new tab
boots from the origin store, finds ``{"legacy_secret":"absent","index":
"absent","load_identity":"NONE"}`` (its own launch log), and lands on
onboarding as nobody. Measured 2026-09-20 against
``test_engine_role_election_web.py``, which had never had a green recorded run
on any app: its second tab rendered no engine-role refusal because it was
never the same account — or any account.

The multi-account web tests do not hit this because they seed the registry into
``localStorage`` themselves before booting (``test_account_switcher_web.py``'s
``_auth_on_account_page``). This helper is that same seeding for the
single-account case, derived from the session the driver is already holding
rather than from a fixture's own key material, so a test that did not mint the
identity can still arrange a second tab.

It writes what the SPA's ``loadIdentity`` reads: the registry's ``fauna/index``
and the per-actor slots (``$lib/store`` resolves the identity through the
registry's session material; there is no single-slot mirror any more).
"""
from __future__ import annotations

import json

from common import build_registry_seed


def seed_origin_store_from_session(driver) -> dict:
    """Mirror ``driver``'s signed-in session into the origin-shared store and
    return that session.

    Safe on the tab it is called from: the page is already running on this
    identity in memory, and these are the same account's own slots.

    Raises ``AssertionError`` when the driver is not signed in — a second tab
    arranged from nothing would fail later, on an assertion about the product.
    """
    session = (driver.get_state() or {}).get("session", {}) or {}
    actor = session.get("actor_id")
    secret = session.get("secret_hex")
    assert actor and secret, (
        "seed_origin_store_from_session needs a signed-in seat: the session "
        f"reads {session!r}"
    )

    # The device id is the origin's own, when the page has minted one: both tabs
    # must present the SAME device leaf (that sharing is what a same-account
    # two-tab test is about), so never invent one here.
    device_id = driver.eval_js(f"localStorage.getItem('fauna/{actor}/device_id')")
    account = {
        "actor_id": actor,
        "secret_hex": secret,
        "nest_url": session.get("node_url"),
        "handle": session.get("handle"),
        "domain": session.get("domain"),
    }
    if isinstance(device_id, str) and device_id:
        account["device_id"] = device_id

    seed = build_registry_seed([account])
    driver.eval_js(
        ";".join(
            f"localStorage.setItem({json.dumps(k)},{json.dumps(v)})"
            for k, v in seed.items()
        )
    )
    # Read one slot back. A write that silently did not land leaves the next tab
    # booting as nobody, and that failure surfaces much later as a product-shaped
    # assertion about a page the tab never reached (convention 6 — fail where the
    # cause is).
    landed = driver.eval_js(f"localStorage.getItem('fauna/{actor}/secret')")
    assert landed == secret, (
        "the origin store did not take the identity: the per-actor secret row reads "
        f"{landed!r}, wanted the session's own secret. Seeded keys: {sorted(seed)}"
    )
    return session


#: This tab's account pin (`$lib/tabPin`) — `sessionStorage`, per-tab by
#: construction, and the slot a tab's own boot resolves its identity through.
PIN_KEY = "fauna_tab_account"


def open_same_account_tab(driver, *, budget_s: float = 60.0):
    """Open a SECOND TAB of ``driver``'s profile, serving the same account, and
    return it once it has resolved that identity.

    Three steps, each load-bearing: seed the origin store
    (:func:`seed_origin_store_from_session`) so the two tabs are one profile —
    one device leaf, one registry — then pin the new tab to the account
    (`$lib/tabPin`, what a real tab carries once it has chosen one), then sign
    it in with the same session patch `_login_app_as` uses for web.

    **Why the patch and not the tab's own boot.** Measured 2026-09-20: a second
    tab arranged from the store alone sat 60 s publishing ``actor_id: null`` —
    with its secret, handle and domain all readable in that same state — and
    never reached the page it was sent to, pinned or not. What a seeded tab
    resolves on its own is therefore an open question for web's boot, and not something a test of the
    engine-role election or of the recovery ceremony should settle by accident.

    Raises ``AssertionError`` naming the tab's published session when it never
    resolves — the failure belongs here, not in a later assertion about a page
    the tab never reached.
    """
    import time

    session = seed_origin_store_from_session(driver)
    actor = session["actor_id"]
    tab = driver.open_same_context_tab()
    tab.eval_js(f"sessionStorage.setItem({json.dumps(PIN_KEY)},{json.dumps(actor)})")
    # The same session patch `_login_app_as` signs web in with — web's sanctioned
    # e2e login — aimed at this tab. The origin-store seeding above is what makes
    # the two tabs one profile (shared device leaf, one registry); the patch is
    # what makes THIS page hold the identity.
    tab.set_state({"session": dict(session)})

    deadline = time.monotonic() + budget_s
    last = None
    while time.monotonic() < deadline:
        try:
            last = (tab.get_state() or {}).get("session", {}) or {}
        except RuntimeError:  # mid-navigation; the context is being replaced
            last = None
        if last and last.get("actor_id") == actor:
            return tab
        time.sleep(0.5)
    raise AssertionError(
        f"the second tab never resolved the account {actor!r} within {budget_s}s; "
        f"its session reads {last!r}"
    )

"""tier_3 e2e: the web app's identity store sealed under a passphrase — create,
the rows sealed at rest, the tab hand-off, relaunch-unlock, wrong passphrase,
change passphrase, and start over.

Authoritative behaviour: ``docs/goal/architecture/apps/web.md`` § Identity store
at rest (the envelope sealed row by row, unlock before launch routing, the
store key in page memory and handed between tabs over a ``BroadcastChannel``,
start-over) and ``docs/goal/architecture/apps/common.md`` § Credential storage →
*The web app's posture* (the required launch passphrase). The page is ui.yaml
``unlock`` (shared with tui); the settings section is ``credential-store-*``
(``docs/goal/ui/settings.md`` § Credential store).

The test build keeps the plain arm unless a journey opts in
(``web.md`` § Identity store at rest → *Test builds*): this journey writes
``FAUNA_E2E_SEALED_IDENTITY_STORE`` into the origin's ``localStorage`` and
reloads, and from then on runs the REAL sealed store — real Argon2id, real
ChaCha20-Poly1305 rows — against a real nest. It starts from a seeded signed-in
browser, so the create also proves the in-place sealing of rows a browser
already holds.

One test on purpose: every stage reads the store the previous one wrote.
"""

from __future__ import annotations

import pytest

from common import build_registry_seed, create_actor_and_register
from drivers import create_driver
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_account_switcher_web import ACCOUNT_PAGE_NAV, _await_boot_landed

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.feature("identity-protected-on-this-device", "identity-sealed-under-a-passphrase"),
]

OPT_IN_KEY = "FAUNA_E2E_SEALED_IDENTITY_STORE"

PASSPHRASE_INPUT = "unlock-passphrase-input"
CONFIRM_INPUT = "unlock-confirm-input"
SUBMIT = "unlock-submit-button"
RESET = "unlock-reset-button"
RESET_CONFIRM = "unlock-reset-confirm-button"

SECTION = "credential-store-section"
STATUS = "credential-store-status"
REKEY_BUTTON = "credential-store-rekey-button"
REKEY_MODAL = "credential-store-rekey-modal"
SEED_NUDGE = "credential-store-rekey-seed-nudge"
REKEY_CURRENT = "credential-store-rekey-current-input"
REKEY_NEW = "credential-store-rekey-new-input"
REKEY_CONFIRM = "credential-store-rekey-confirm-input"
REKEY_SUBMIT = "credential-store-rekey-submit-button"
REKEY_SUCCESS = "credential-store-rekey-success"
SWITCHER_LIST = "account-switcher-list"

FIRST = "correct-horse-battery"
SECOND = "staple-tram-lantern"

# Every identity-store row and its raw value, read straight from localStorage —
# the rows `fauna_client_accounts::is_identity_store_key` names (the `fauna/…`
# rows bar the seal's own, `install/…`, bare `{actor}/…`), so this read is the
# browser's own view of what rests at rest.
_IDENTITY_ROWS_JS = (
    "(() => { const out = {}; for (let i = 0; i < localStorage.length; i++) {"
    " const k = localStorage.key(i);"
    " if (k.startsWith('fauna/seal')) continue;"
    " if (k.startsWith('fauna/') || k.startsWith('install/') || /^[0-9a-f]{64}\\//.test(k))"
    " out[k] = localStorage.getItem(k); } return out; })()"
)


def _seal_snapshot(driver) -> dict:
    """Whether the seal header is there, and each identity row's first bytes —
    the self-diagnosing half of a sealing assertion (convention 6)."""
    rows = driver.eval_js(_IDENTITY_ROWS_JS) or {}
    return {
        "header": driver.eval_js("localStorage.getItem('fauna/seal') !== null"),
        "rows": {k: v[:12] for k, v in rows.items()},
    }


def _error(driver) -> str:
    return driver.get_text("error-message")


def _refused(driver, expected: str, what: str) -> None:
    """Wait until ``error-message`` reads exactly ``expected``. Web's submit runs
    the Argon2id derive after a yield, so a refusal paints asynchronously, and
    an earlier refusal's text may still be standing: wait for THIS one
    (convention 14), never read once."""
    wait_until(lambda: _error(driver) == expected, 30,
               diagnose=lambda: f"{what}: error-message reads {_error(driver)!r}")


def _submit(driver, passphrase, confirm=None):
    driver.clear_and_type(PASSPHRASE_INPUT, passphrase)
    if confirm is not None:
        driver.clear_and_type(CONFIRM_INPUT, confirm)
    driver.click(SUBMIT)


def _await_unlocked(driver):
    """The gate let the shell through: the unlock page is gone AND the boot's own
    redirect landed. `_await_boot_landed` alone reads only the path, which the
    gate leaves as it was (it renders in place of the shell)."""
    wait_until(lambda: driver.is_absent(PASSPHRASE_INPUT), 60,
               diagnose=lambda: f"still on the unlock page: {_error(driver)!r}")
    _await_boot_landed(driver)


def _relaunch_to_unlock(driver):
    driver.hard_reload()
    driver.wait_for(PASSPHRASE_INPUT, timeout=30)
    assert driver.is_absent(CONFIRM_INPUT), (
        "a sealed store must relaunch into UNLOCK mode (no confirm input)"
    )


def test_web_sealed_identity_store_journey(nest_instance, spa_url):
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    actor = user["actor_id_hex"]
    secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": spa_url,
          "device_id": "sealed-store", "handle": "sealed"}],
        active=actor,
    )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/", "seed_credentials": seed})
    tab_b = None
    try:
        # 0. The test build's plain arm: the seeded rows serve the boot as-is.
        _await_boot_landed(driver)
        assert driver.is_absent(PASSPHRASE_INPUT), "the plain arm paints no unlock page"

        # 1. Opt in and relaunch → CREATE mode, before anything reads the store.
        driver.eval_js(f"localStorage.setItem({OPT_IN_KEY!r}, '1')")
        driver.hard_reload()
        driver.wait_for(PASSPHRASE_INPUT, timeout=30)
        assert driver.is_visible(CONFIRM_INPUT), (
            f"no header yet must be CREATE mode: {driver.diagnose(CONFIRM_INPUT)}"
        )

        # The create prompt is ID-less chrome (like tui's): read it off the page.
        assert S.unlock.create_prompt in driver.eval_js("document.body.innerText"), (
            "create mode must paint its prompt"
        )

        # 2. A typed passphrase reads back only as dots.
        driver.type_text(PASSPHRASE_INPUT, FIRST)
        assert driver.get_text(PASSPHRASE_INPUT) == "•" * len(FIRST), (
            "the passphrase input must read back as a same-length bullet mask"
        )

        # 3. Two entries that differ are refused, nothing created.
        _submit(driver, FIRST, confirm="something-else")
        _refused(driver, S.unlock.error_mismatch, "a mismatched confirm must be refused")
        assert driver.is_visible(CONFIRM_INPUT), "a mismatch stays in create mode"
        assert driver.eval_js("localStorage.getItem('fauna/seal')") is None, (
            "a refused create must write no header"
        )

        # 4. Matching entries → sealed; launch routing finds the identity.
        _submit(driver, FIRST, confirm=FIRST)
        wait_until(lambda: driver.is_absent(PASSPHRASE_INPUT), 60,
                   diagnose=lambda: f"create never left the unlock page: {_error(driver)!r}")
        at_create = _seal_snapshot(driver)
        _await_boot_landed(driver)

        # 5. Never written unprotected: every identity row rests sealed and the
        # secret appears nowhere in localStorage.
        after_boot = _seal_snapshot(driver)
        rows = driver.eval_js(_IDENTITY_ROWS_JS)
        assert "fauna/index" in rows, f"the index row must exist: {sorted(rows)}"
        plain = {k: v[:16] for k, v in rows.items() if not v.startswith("sealed:v1:")}
        assert not plain, (
            f"identity rows left unsealed: {plain}\n"
            f"right after the create: {at_create}\nafter the boot: {after_boot}\n"
            "console tail:\n" + "\n".join(driver.console_log()[-60:])
        )
        everything = driver.eval_js(
            "(() => { let s = ''; for (let i = 0; i < localStorage.length; i++)"
            " s += localStorage.getItem(localStorage.key(i)) + '\\n'; return s; })()"
        )
        assert secret not in everything, "the identity secret rests in localStorage in the clear"

        # 6. A second tab takes the key from this unlocked one: no prompt.
        tab_b = driver.open_same_context_tab()
        _await_boot_landed(tab_b)
        # A positive shell element first: the gate paints nothing during its
        # hand-off grace, so an absent unlock input alone proves nothing yet.
        tab_b.wait_for("feed-tab", timeout=60)
        assert tab_b.is_absent(PASSPHRASE_INPUT), (
            "a tab opened beside an unlocked one must take its key, not ask again"
        )
        tab_b.teardown()
        tab_b = None

        # 7. Relaunch with no other tab open → UNLOCK; a wrong passphrase is
        # refused and stays locked; the right one finds the identity.
        _relaunch_to_unlock(driver)
        _submit(driver, "not-the-passphrase")
        _refused(driver, S.unlock.error_wrong, "a wrong passphrase must be refused")
        assert driver.is_visible(PASSPHRASE_INPUT), "a wrong passphrase stays locked"
        _submit(driver, FIRST)
        _await_unlocked(driver)

        # 8. Settings → Account → the credential-store section.
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SECTION, timeout=30)
        assert driver.get_text(STATUS) == S.credential_store.status_sealed, (
            "the section must say how the identity is held on this device"
        )
        driver.click(REKEY_BUTTON)
        driver.wait_for(REKEY_MODAL, timeout=10)
        assert driver.get_text(SEED_NUDGE), "the backup reminder shows before the change"

        def rekey(current, new, confirm):
            for eid, value in ((REKEY_CURRENT, current), (REKEY_NEW, new), (REKEY_CONFIRM, confirm)):
                driver.clear_and_type(eid, value)
            driver.click(REKEY_SUBMIT)

        rekey("wrong-current", SECOND, SECOND)
        _refused(driver, S.credential_store.error_wrong, "a wrong current passphrase must be refused")
        assert driver.is_visible(REKEY_MODAL), "a refused change keeps the modal open"
        rekey(FIRST, SECOND, "not-matching")
        _refused(driver, S.credential_store.error_mismatch,
                 "a new passphrase that does not match its confirmation is refused")
        rekey(FIRST, SECOND, SECOND)
        driver.wait_for(REKEY_SUCCESS, timeout=30)
        assert driver.is_absent(REKEY_MODAL), "success disarms the modal"
        assert driver.is_visible(SWITCHER_LIST), "changing the passphrase does not sign you out"

        # 9. The old passphrase stops working; the new one opens the store.
        _relaunch_to_unlock(driver)
        _submit(driver, FIRST)
        _refused(driver, S.unlock.error_wrong, "the old passphrase must stop working after a change")
        _submit(driver, SECOND)
        _await_unlocked(driver)

        # 10. Start over: after an explicit confirmation the store is erased and
        # the page returns to create mode.
        _relaunch_to_unlock(driver)
        driver.click(RESET)
        driver.wait_for(RESET_CONFIRM, timeout=10)
        assert driver.eval_js("localStorage.getItem('fauna/seal')"), (
            "revealing the confirm must erase nothing"
        )
        # The confirm says what starting over erases and the ways back in (a
        # plain paragraph beside the button — it has no ui.yaml id).
        shown = driver.eval_js("document.body.innerText") or ""
        assert S.unlock.reset_explainer in " ".join(shown.split()), (
            f"the start-over confirm must paint unlock.reset_explainer; page reads {shown!r}"
        )
        driver.click(RESET_CONFIRM)
        driver.wait_for(CONFIRM_INPUT, timeout=30)
        left = driver.eval_js(_IDENTITY_ROWS_JS)
        assert not left, f"start-over left identity rows behind: {sorted(left)}"
        assert driver.eval_js("localStorage.getItem('fauna/seal')") is None, (
            "start-over must remove the seal header"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()
        try:
            driver.eval_js(f"localStorage.removeItem({OPT_IN_KEY!r})")
        except Exception:  # noqa: BLE001 — teardown discards the context anyway
            pass
        driver.teardown()

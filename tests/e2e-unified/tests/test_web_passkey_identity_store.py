"""tier_3 e2e: a passkey as a second wrap of the web app's sealed identity store —
add it, unlock with it after a relaunch, the passphrase still opens the store,
remove it, and the passkey is no longer offered while the passphrase still opens.

Authoritative behaviour: ``docs/goal/architecture/apps/web.md`` § Identity store
at rest → *A passkey wrap* (a WebAuthn PRF output wraps the same store key; added
and removed without touching a row; the passphrase wrap stays beside it) and
``docs/goal/architecture/apps/common.md`` § Credential storage → *The web app's
posture* (a passkey is a second way to open the same seal, only in a secure
context on a named host). The ids are ui.yaml's ``unlock-passkey-button`` and
``credential-store-passkey-{add,remove}-button`` (web only; tui declares the
absence).

The authenticator is Chromium's CDP virtual authenticator with PRF, attached by
the web bridge. WebAuthn refuses an IP-literal origin, so this journey loads the
SPA by ``localhost`` (``spa_proxy_for(..., url_host="localhost")``) — measured
2026-10-19: on ``127.0.0.1`` every ceremony is refused ``SecurityError``. The
sealed store under it is the real one (``test_web_sealed_identity_store.py``
witnesses the passphrase half): real Argon2id, real ChaCha20-Poly1305, a real
PRF output through BLAKE3 ``derive_key``.

One test on purpose: every stage reads the header the previous one wrote.
"""

from __future__ import annotations

import json

import pytest

from common import build_registry_seed, create_actor_and_register
from drivers import create_driver
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_account_switcher_web import ACCOUNT_PAGE_NAV, _await_boot_landed

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.feature("identity-sealed-under-a-passphrase"),
]

OPT_IN_KEY = "FAUNA_E2E_SEALED_IDENTITY_STORE"

PASSPHRASE_INPUT = "unlock-passphrase-input"
CONFIRM_INPUT = "unlock-confirm-input"
SUBMIT = "unlock-submit-button"
PASSKEY_UNLOCK = "unlock-passkey-button"

SECTION = "credential-store-section"
PASSKEY_ADD = "credential-store-passkey-add-button"
PASSKEY_REMOVE = "credential-store-passkey-remove-button"

PASSPHRASE = "correct-horse-battery"

_HEADER_JS = "localStorage.getItem('fauna/seal')"


def _error(driver) -> str:
    return driver.get_text("error-message") if not driver.is_absent("error-message") else ""


def _wrap_kinds(driver) -> list:
    """The seal header's wrap kinds — the at-rest witness that add and remove
    wrote exactly one header change (convention 6: read it into failures)."""
    raw = driver.eval_js(_HEADER_JS)
    if not raw:
        return []
    try:
        return [w.get("kind") for w in json.loads(raw).get("wraps", [])]
    except (ValueError, AttributeError):
        return [f"<unparsed header {raw[:40]!r}>"]


def _identity_rows(driver) -> dict:
    return driver.eval_js(
        "(() => { const out = {}; for (let i = 0; i < localStorage.length; i++) {"
        " const k = localStorage.key(i); if (k.startsWith('fauna/seal')) continue;"
        " if (k.startsWith('fauna/') || k.startsWith('install/')) out[k] = localStorage.getItem(k); }"
        " return out; })()"
    ) or {}


def _await_unlocked(driver, what: str):
    wait_until(lambda: driver.is_absent(PASSPHRASE_INPUT), 60,
               diagnose=lambda: f"{what}: still on the unlock page, error {_error(driver)!r}, "
                                f"wraps {_wrap_kinds(driver)}")
    _await_boot_landed(driver)


def _relaunch_to_unlock(driver):
    driver.hard_reload()
    driver.wait_for(PASSPHRASE_INPUT, timeout=30)
    assert driver.is_absent(CONFIRM_INPUT), "a sealed store relaunches into UNLOCK mode"


def _page_says(driver, text: str) -> bool:
    """Whether the page paints ``text`` (plain paragraphs have no ui.yaml id)."""
    shown = driver.eval_js("document.body.innerText") or ""
    return text in " ".join(shown.split())


def _open_section(driver):
    driver.set_state(ACCOUNT_PAGE_NAV)
    driver.wait_for(SECTION, timeout=30)


def test_web_passkey_wrap_journey(nest_instance, spa_proxy_for):
    spa = spa_proxy_for(nest_instance["url"], url_host="localhost")
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": bytes(user["signing_key"]).hex(), "nest_url": spa,
          "device_id": "passkey-wrap", "handle": "passkey"}],
        active=actor,
    )

    driver = create_driver("web")
    driver.launch({"url": spa + "/app/", "seed_credentials": seed})
    try:
        _await_boot_landed(driver)
        driver.add_virtual_authenticator(has_prf=True)

        # 1. Seal the store under a passphrase (the opt-in test-build path).
        driver.eval_js(f"localStorage.setItem({OPT_IN_KEY!r}, '1')")
        driver.hard_reload()
        driver.wait_for(CONFIRM_INPUT, timeout=30)
        driver.clear_and_type(PASSPHRASE_INPUT, PASSPHRASE)
        driver.clear_and_type(CONFIRM_INPUT, PASSPHRASE)
        driver.click(SUBMIT)
        _await_unlocked(driver, "the create")
        assert _wrap_kinds(driver) == ["passphrase"], f"one passphrase wrap: {_wrap_kinds(driver)}"

        # 2. No passkey yet: unlock mode offers none.
        _relaunch_to_unlock(driver)
        assert driver.is_absent(PASSKEY_UNLOCK), "no passkey wrap → no passkey unlock offered"
        driver.clear_and_type(PASSPHRASE_INPUT, PASSPHRASE)
        driver.click(SUBMIT)
        _await_unlocked(driver, "the passphrase unlock")

        # 3. Settings → Account: add a passkey — one header write, no row touched.
        _open_section(driver)
        assert driver.is_absent(PASSKEY_REMOVE), "no wrap yet → no remove offered"
        assert _page_says(driver, S.credential_store.passkey_explainer), "no wrap → the explainer is painted"
        assert not _page_says(driver, S.credential_store.passkey_status), "no wrap → no status line"
        rows_before = _identity_rows(driver)
        driver.click(PASSKEY_ADD)
        wait_until(lambda: driver.is_visible(PASSKEY_REMOVE), 30,
                   diagnose=lambda: f"adding the passkey never landed: error {_error(driver)!r}, "
                                    f"wraps {_wrap_kinds(driver)}")
        assert _page_says(driver, S.credential_store.passkey_status), "a wrap exists → the status line is painted"
        assert not _page_says(driver, S.credential_store.passkey_explainer), "a wrap exists → no explainer"
        assert sorted(_wrap_kinds(driver)) == ["passkey", "passphrase"], (
            f"the passkey wraps beside the passphrase: {_wrap_kinds(driver)}"
        )
        assert _identity_rows(driver) == rows_before, "adding a passkey must touch no identity row"

        # 4. Relaunch → the passkey opens the store; nothing is typed.
        _relaunch_to_unlock(driver)
        driver.wait_for(PASSKEY_UNLOCK, timeout=30)
        driver.click(PASSKEY_UNLOCK)
        _await_unlocked(driver, "the passkey unlock")

        # 5. The passphrase still opens the store.
        _relaunch_to_unlock(driver)
        driver.clear_and_type(PASSPHRASE_INPUT, PASSPHRASE)
        driver.click(SUBMIT)
        _await_unlocked(driver, "the passphrase unlock beside a passkey")

        # 6. Remove the passkey — one header write; the passphrase wrap remains.
        _open_section(driver)
        driver.wait_for(PASSKEY_REMOVE, timeout=30)
        rows_before = _identity_rows(driver)
        driver.click(PASSKEY_REMOVE)
        wait_until(lambda: driver.is_visible(PASSKEY_ADD), 30,
                   diagnose=lambda: f"removing the passkey never landed: error {_error(driver)!r}, "
                                    f"wraps {_wrap_kinds(driver)}")
        assert _wrap_kinds(driver) == ["passphrase"], f"only the passphrase wrap remains: {_wrap_kinds(driver)}"
        assert _identity_rows(driver) == rows_before, "removing a passkey must touch no identity row"

        # 7. The passkey no longer opens anything — it is not offered — and the
        # passphrase does.
        _relaunch_to_unlock(driver)
        assert driver.is_absent(PASSKEY_UNLOCK), "a removed passkey must no longer be offered"
        driver.clear_and_type(PASSPHRASE_INPUT, PASSPHRASE)
        driver.click(SUBMIT)
        _await_unlocked(driver, "the passphrase unlock after removing the passkey")
    finally:
        try:
            driver.eval_js(f"localStorage.removeItem({OPT_IN_KEY!r})")
        except Exception:  # noqa: BLE001 — teardown discards the context anyway
            pass
        driver.teardown()

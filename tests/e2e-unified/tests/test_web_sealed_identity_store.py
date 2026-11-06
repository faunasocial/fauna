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

The journey is one test on purpose: every stage reads the store the previous
one wrote. The integrity witnesses after it (``identity-sealed-under-a-passphrase``
outcomes 10 and 12–16, lifted from tui's ``test_tui_credential_store_rekey.py``
and ``test_tui_headless_credential_store.py``) each start from their own sealed,
signed-in browser and drive web's own shapes of the terminal app's file
mechanics: a page reload racing a write in place of a SIGKILL, a damaged row or
header in place of a damaged file, a staging row in place of a write file, a
second tab in place of a second copy, and the test build's plain arm in place of
a keyring the box gained.
"""

from __future__ import annotations

import contextlib
import json

import pytest

from common import build_registry_seed, create_actor_and_register
from drivers import create_driver
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_account_switcher_web import ACCOUNT_PAGE_NAV, _await_boot_landed

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

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


@pytest.mark.feature("identity-protected-on-this-device", "identity-sealed-under-a-passphrase")
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



# ---------------------------------------------------------------------------
# The integrity witnesses — outcomes 10 and 12–16.
# ---------------------------------------------------------------------------

SEAL_HEADER = "fauna/seal"
INDEX_ROW = "fauna/index"
STAGING_PREFIX = "fauna/seal-staging/"

# Everything the identity store rests as — the header, every identity row and
# every staging row — raw, as the browser holds it.
_STORE_JS = (
    "(() => { const out = {}; for (let i = 0; i < localStorage.length; i++) {"
    " const k = localStorage.key(i);"
    " if (k.startsWith('fauna/') || k.startsWith('install/') || /^[0-9a-f]{64}\\//.test(k))"
    " out[k] = localStorage.getItem(k); } return out; })()"
)


def _store(driver) -> dict:
    return driver.eval_js(_STORE_JS) or {}


def _ls_get(driver, key):
    return driver.eval_js(f"localStorage.getItem({json.dumps(key)})")


def _ls_set(driver, key, value):
    driver.eval_js(f"localStorage.setItem({json.dumps(key)}, {json.dumps(value)})")


def _flip_last_char(value: str) -> str:
    """One character of a sealed token changed — a damaged row (the AEAD tag no
    longer verifies) that still LOOKS sealed."""
    last = value[-1]
    return value[:-1] + ("A" if last != "A" else "B")


def _signed_in_as(driver, actor) -> None:
    """The boot landed with ``actor`` as the active identity — read from the
    app's own session state, so an index the store could not open never passes."""
    _await_boot_landed(driver)
    wait_until(lambda: driver.get_state("session.actor_id") == actor, 60,
               diagnose=lambda: f"session.actor_id={driver.get_state('session.actor_id')!r}")


def _sealed_signed_in(nest_instance, spa_url, device_id):
    """A browser signed in as a registered actor whose identity store is sealed
    under ``FIRST`` — the seeded rows sealed in place by a real create."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    actor = user["actor_id_hex"]
    secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": spa_url,
          "device_id": device_id, "handle": device_id}],
        active=actor,
    )
    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/", "seed_credentials": seed})
    _await_boot_landed(driver)
    driver.eval_js(f"localStorage.setItem({OPT_IN_KEY!r}, '1')")
    driver.hard_reload()
    driver.wait_for(CONFIRM_INPUT, timeout=30)
    _submit(driver, FIRST, confirm=FIRST)
    _await_unlocked(driver)
    _signed_in_as(driver, actor)
    rows = driver.eval_js(_IDENTITY_ROWS_JS)
    assert rows and all(v.startswith("sealed:v1:") for v in rows.values()), (
        f"precondition: every identity row sealed: {_seal_snapshot(driver)}"
    )
    return driver, actor, secret


def _teardown(*drivers):
    for d in drivers:
        if d is None:
            continue
        with contextlib.suppress(Exception):
            d.eval_js(f"localStorage.removeItem({OPT_IN_KEY!r})")
        d.teardown()


def _unlock_opens(driver, passphrase) -> bool:
    """Submit ``passphrase`` on the unlock page; True when it opened the store,
    False when it was refused as wrong-or-damaged. Waits for whichever happens
    (the submit clears the previous refusal first)."""
    _submit(driver, passphrase)
    outcome = {}

    def settled():
        if driver.is_absent(PASSPHRASE_INPUT):
            outcome["opened"] = True
            return True
        if _error(driver) == S.unlock.error_wrong:
            outcome["opened"] = False
            return True
        return False

    wait_until(settled, 60, diagnose=lambda: f"unlock never settled: {_error(driver)!r}")
    return outcome["opened"]


def _arm_reload(driver, delay_ms):
    """Arm a reload of this document ``delay_ms`` from now — the browser's
    SIGKILL — and mark the document, so :func:`_await_reloaded` can tell the
    next document from this one."""
    driver.eval_js(
        "window.__faunaBeforeReload = true;"
        f" setTimeout(() => location.reload(), {delay_ms})"
    )


def _await_reloaded(driver):
    """Wait until the armed reload has replaced the document and the new one
    paints the unlock page — never answered by the old document still standing
    (convention 14)."""

    def new_document():
        try:
            return driver.eval_js("window.__faunaBeforeReload === undefined") is True
        except RuntimeError:  # the context swap itself is no answer
            return False

    wait_until(new_document, 60, diagnose=lambda: "the armed reload never landed")
    driver.wait_for(PASSPHRASE_INPUT, timeout=60)


def _open_rekey(driver):
    driver.set_state(ACCOUNT_PAGE_NAV)
    driver.wait_for(SECTION, timeout=30)
    driver.click(REKEY_BUTTON)
    driver.wait_for(REKEY_MODAL, timeout=10)


def _fill_rekey(driver, current, new, confirm=None):
    for eid, value in ((REKEY_CURRENT, current), (REKEY_NEW, new),
                       (REKEY_CONFIRM, new if confirm is None else confirm)):
        driver.clear_and_type(eid, value)


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_an_interrupted_passphrase_change_opens_with_old_or_new(nest_instance, spa_url):
    """Outcome 10 — a page stopped in the middle of changing the passphrase
    still opens with the old passphrase or the new one, never neither
    (``web.md`` § Identity store at rest: the change is ONE header write, so at
    every crash instant exactly one passphrase opens the store).

    Round after round the change is submitted with a page reload armed in the
    page itself at several delays, so some reloads land before the write and
    others after it — the browser's SIGKILL. The relaunch must open under one of
    the two, and that one is the next round's current passphrase; every round
    ends signed back in. The assertions are on what opens, never on where the
    reload landed."""
    driver, actor, _ = _sealed_signed_in(nest_instance, spa_url, "rekey-crash")
    try:
        current = FIRST
        for round_no, delay_ms in enumerate((0, 50, 150, 400)):
            new = f"{SECOND}-{round_no}"
            _open_rekey(driver)
            _fill_rekey(driver, current, new)
            _arm_reload(driver, delay_ms)
            with contextlib.suppress(Exception):  # the reload may beat the click
                driver.click(REKEY_SUBMIT)
            _await_reloaded(driver)
            header = _ls_get(driver, SEAL_HEADER)
            if _unlock_opens(driver, current):
                opened = current
            else:
                assert _unlock_opens(driver, new), (
                    f"round {round_no} (reload {delay_ms} ms into the change): the store "
                    f"opens under neither {current!r} nor {new!r}; header={header!r}"
                )
                opened = new
            _signed_in_as(driver, actor)
            current = opened
    finally:
        _teardown(driver)


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_a_forgotten_passphrase_cannot_be_reset(nest_instance, spa_url):
    """Outcome 12 — a forgotten passphrase cannot be reset; the identity comes
    back only from a copy of the key kept elsewhere or the recovery kit
    (``web.md`` § Identity store at rest: the seal has no escrow; start-over
    erases this browser's copy):

    1. Relaunch: guess after guess is refused and the store is untouched; the
       only way past the unlock page is start-over, and revealing it erases
       nothing.
    2. Start over → create mode; a new passphrase lands on a wizard holding no
       identity, offering the kept key and the recovery kit as the ways back.
    3. Importing the kept key brings the same identity back into this browser,
       sealed under the new passphrase."""
    driver, actor, secret = _sealed_signed_in(nest_instance, spa_url, "forgot")
    try:
        _relaunch_to_unlock(driver)
        before = _store(driver)
        for guess in ("correct-horse", "battery-staple", "i-forgot-it"):
            assert not _unlock_opens(driver, guess), f"the guess {guess!r} opened the store"
        assert _store(driver) == before, "refused guesses must not touch the identity store"

        driver.click(RESET)
        driver.wait_for(RESET_CONFIRM, timeout=10)
        assert _store(driver) == before, "revealing start-over must erase nothing"
        driver.click(RESET_CONFIRM)
        driver.wait_for(CONFIRM_INPUT, timeout=30)
        assert not driver.eval_js(_IDENTITY_ROWS_JS), "start-over erases the identity rows"

        _submit(driver, "a-brand-new-passphrase", confirm="a-brand-new-passphrase")
        wait_until(
            lambda: driver.is_visible("import-identity-button"), 60,
            diagnose=lambda: (
                "a new store with no identity never reached the wizard: path="
                f"{driver.eval_js('location.pathname')!r} tab pin="
                f"{driver.eval_js('sessionStorage.getItem(`fauna_tab_account`)')!r} "
                f"store={sorted(_store(driver))} page="
                f"{(driver.eval_js('document.body.innerText') or '')[:400]!r}"
            ),
        )
        assert not any(actor in k for k in _store(driver)), (
            f"a fresh store must hold no identity until one is brought back: {sorted(_store(driver))}"
        )
        for way_back in ("import-identity-button", "restore-from-recovery-kit-button"):
            assert driver.is_visible(way_back), (
                f"the wizard must offer {way_back} as a way back: {driver.diagnose(way_back)}"
            )

        driver.click("import-identity-button")
        driver.wait_for("paste-secret-field", timeout=30)
        driver.clear_and_type("paste-secret-field", secret)
        driver.click("import-submit-button")
        driver.wait_for("handle-input", timeout=45)
        back: dict = {}

        def brought_back() -> bool:
            back.clear()
            rows = driver.eval_js(_IDENTITY_ROWS_JS) or {}
            back.update({k: v for k, v in rows.items() if actor in k})
            return bool(back)

        wait_until(
            brought_back,
            30,
            interval=0.5,
            diagnose=lambda: (
                f"importing the kept key must bring the identity {actor[:12]}… back into this "
                f"browser; the store holds {sorted(_store(driver))}, the page is at "
                f"{driver.eval_js('location.pathname')!r}, error {_error(driver)!r}"
            ),
        )
        assert all(v.startswith("sealed:v1:") for v in back.values()), (
            f"the identity brought back must rest sealed: { {k: v[:12] for k, v in back.items()} }"
        )
    finally:
        _teardown(driver)


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_a_crash_while_saving_never_cuts_the_sealed_file_short(nest_instance, spa_url):
    """Outcome 13 — a page stopped while saving the sealed identity never
    leaves what it saved cut short or unreadable (``web.md`` § Identity store
    at rest → *Rows a browser already holds*: a row's sealed form is staged and
    read back before the row is replaced, and a row that does not open beside
    a staged copy that does is restored from the staging row):

    1. A crash mid-replace, as it rests: the index row cut short and its staged
       sealed copy beside it. Unlock restores the row from the staging row,
       removes the staging row and signs the identity back in.
    2. Reloads racing the unlock's own sealing pass, at several delays, leave
       a store the passphrase opens with every row whole."""
    driver, actor, _ = _sealed_signed_in(nest_instance, spa_url, "crash")
    try:
        sealed_index = _ls_get(driver, INDEX_ROW)
        _relaunch_to_unlock(driver)
        _ls_set(driver, STAGING_PREFIX + INDEX_ROW, sealed_index)
        _ls_set(driver, INDEX_ROW, sealed_index[: len(sealed_index) // 2])
        assert _unlock_opens(driver, FIRST), _error(driver)
        # The unlock's own sealing pass restores the row before anything reads
        # it — read it before the boot rewrites it (a write re-seals under a
        # fresh nonce, so later reads never equal the staged bytes).
        assert _ls_get(driver, STAGING_PREFIX + INDEX_ROW) is None, (
            "the staging row goes once the row is restored"
        )
        # The boot reading the index as this actor IS the restored row opening:
        # the cut-short one opens under nothing and would read as unreadable.
        _signed_in_as(driver, actor)
        assert _ls_get(driver, INDEX_ROW).startswith("sealed:v1:")

        for delay_ms in (0, 20, 100):
            _relaunch_to_unlock(driver)
            # Typed BEFORE the reload is armed, so only the click races it: a
            # click that lands on the new document submits an empty field,
            # which is refused, and can never unlock the page being awaited.
            driver.clear_and_type(PASSPHRASE_INPUT, FIRST)
            _arm_reload(driver, delay_ms)
            with contextlib.suppress(Exception):  # the reload may beat the click
                driver.click(SUBMIT)
            _await_reloaded(driver)
            rows = driver.eval_js(_IDENTITY_ROWS_JS)
            assert INDEX_ROW in rows and all(v.startswith("sealed:v1:") for v in rows.values()), (
                f"a reload {delay_ms} ms into an unlock left rows unsealed or gone: "
                f"{_seal_snapshot(driver)}"
            )
            assert _unlock_opens(driver, FIRST), (
                f"after a reload {delay_ms} ms into an unlock the passphrase must still "
                f"open the store: {_error(driver)!r}"
            )
            _signed_in_as(driver, actor)
    finally:
        _teardown(driver)


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_an_unreadable_sealed_identity_is_left_as_it_is(nest_instance, spa_url):
    """Outcome 14 — a sealed identity the app cannot read (damaged, or written
    by a newer build) is left as it is, never replaced with an empty one
    (``web.md`` § Identity store at rest: a sealed row that opens under neither
    the store key nor a staged copy is evidence, left as found and read as it
    rests, never as absent):

    1. A damaged header → the right passphrase is refused; the header is
       exactly what was there.
    2. A newer build's header (version bumped) → refused, header unchanged.
    3. A damaged index row → the passphrase opens the store, and the row stays
       byte for byte through the launch that follows.
    4. The header damaged under a running, unlocked page → a passphrase change
       is refused and the damaged header stays.
    5. Everything put back → the passphrase signs straight back in: the
       identity was never lost."""
    driver, actor, _ = _sealed_signed_in(nest_instance, spa_url, "unread")
    try:
        header = _ls_get(driver, SEAL_HEADER)
        index = _ls_get(driver, INDEX_ROW)

        # 1. Damaged header.
        damaged_header = header[:-2]
        _ls_set(driver, SEAL_HEADER, damaged_header)
        _relaunch_to_unlock(driver)
        assert not _unlock_opens(driver, FIRST), "a damaged header must refuse the passphrase"
        assert _ls_get(driver, SEAL_HEADER) == damaged_header, "a damaged header is left as found"

        # 2. A newer build's header.
        newer = json.loads(header)
        newer["v"] = newer["v"] + 1
        newer_header = json.dumps(newer)
        _ls_set(driver, SEAL_HEADER, newer_header)
        _relaunch_to_unlock(driver)
        _submit(driver, FIRST)
        wait_until(lambda: bool(_error(driver)), 60,
                   diagnose=lambda: "a newer build's header must be refused with a message")
        assert driver.is_visible(PASSPHRASE_INPUT), "a newer build's header stays locked"
        assert _ls_get(driver, SEAL_HEADER) == newer_header, (
            "a newer build's header must never be reinterpreted or overwritten"
        )

        # 3. A damaged index row under the right header.
        _ls_set(driver, SEAL_HEADER, header)
        damaged_index = _flip_last_char(index)
        _ls_set(driver, INDEX_ROW, damaged_index)
        _relaunch_to_unlock(driver)
        assert _unlock_opens(driver, FIRST), _error(driver)
        wait_until(
            lambda: driver.is_visible("import-identity-button")
            or driver.get_state("session.actor_id") is not None,
            60,
            diagnose=lambda: (
                "the launch over a damaged index never routed: "
                f"{driver.eval_js('location.pathname')!r}"
            ),
        )
        assert _ls_get(driver, INDEX_ROW) == damaged_index, (
            "a sealed index the store key cannot open must be left exactly as found"
        )
        assert _ls_get(driver, STAGING_PREFIX + INDEX_ROW) is None

        # 4. Damaged under a running, unlocked page; a write must not reseal.
        _ls_set(driver, INDEX_ROW, index)
        _relaunch_to_unlock(driver)
        assert _unlock_opens(driver, FIRST), _error(driver)
        _signed_in_as(driver, actor)
        _open_rekey(driver)
        _ls_set(driver, SEAL_HEADER, damaged_header)
        _fill_rekey(driver, FIRST, SECOND)
        driver.click(REKEY_SUBMIT)
        _refused(driver, S.credential_store.error_wrong,
                 "a change over a header the page can no longer read must be refused")
        assert driver.is_absent(REKEY_SUCCESS)
        assert _ls_get(driver, SEAL_HEADER) == damaged_header, (
            "a write must never replace a header the page could not read"
        )

        # 5. The identity was never lost.
        _ls_set(driver, SEAL_HEADER, header)
        _relaunch_to_unlock(driver)
        assert _unlock_opens(driver, FIRST), _error(driver)
        _signed_in_as(driver, actor)
    finally:
        _teardown(driver)


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_two_copies_on_one_sealed_identity_never_lose_it(nest_instance, spa_url):
    """Outcome 15 — two tabs open at once on the same sealed identity never
    lose it between them (``web.md`` § Identity store at rest: a change is one
    header write over an unchanged store key, so other tabs keep working):

    1. Tab A is unlocked; tab B opens beside it and takes the key — no prompt.
    2. B changes the passphrase. A stays signed in on the same store key; A's
       own change from the replaced passphrase is refused against the header
       itself, never resealing B's header.
    3. Both close; one relaunch: the old passphrase is refused and the new one
       signs the identity straight back in."""
    a, actor, _ = _sealed_signed_in(nest_instance, spa_url, "two-tabs")
    b = None
    try:
        b = a.open_same_context_tab()
        _await_boot_landed(b)
        b.wait_for("feed-tab", timeout=60)
        assert b.is_absent(PASSPHRASE_INPUT), "tab B must take A's key, not ask again"

        _open_rekey(b)
        _fill_rekey(b, FIRST, SECOND)
        b.click(REKEY_SUBMIT)
        b.wait_for(REKEY_SUCCESS, timeout=30)
        after_b = _ls_get(b, SEAL_HEADER)

        assert a.get_state("session.actor_id") == actor, "tab A stays signed in beside B"
        _open_rekey(a)
        _fill_rekey(a, FIRST, "the-stale-tabs-try")
        a.click(REKEY_SUBMIT)
        _refused(a, S.credential_store.error_wrong,
                 "a tab whose passphrase was changed in another tab must be refused")
        assert a.is_absent(REKEY_SUCCESS)
        assert _ls_get(a, SEAL_HEADER) == after_b, "the stale tab must not reseal B's header"
        rows = a.eval_js(_IDENTITY_ROWS_JS)
        assert all(v.startswith("sealed:v1:") for v in rows.values()), (
            f"tab A must keep writing sealed on the unchanged store key: {_seal_snapshot(a)}"
        )
        b.teardown()
        b = None

        _relaunch_to_unlock(a)
        assert not _unlock_opens(a, FIRST), "the passphrase tab B replaced must be refused"
        assert _unlock_opens(a, SECOND), _error(a)
        _signed_in_as(a, actor)
    finally:
        _teardown(b, a)


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_a_sealed_identity_is_never_left_behind_for_another_store(nest_instance, spa_url):
    """Outcome 16 — once the identity is sealed under a passphrase, the app
    keeps finding it there and never switches by itself to another store.
    The browser's one other place for the identity is the plain arm (rows
    unsealed), which a test build serves while it has no header; ``web.md``
    § Identity store at rest: which arm serves follows the rows, never a flag,
    and a header is honoured on every build.

    1. Signed in on the sealed store.
    2. The flag that selects the sealed arm is removed and the page relaunched:
       the header wins — the UNLOCK page, never the plain arm or the wizard.
    3. The passphrase signs straight back in, and every identity row still
       rests sealed: nothing was rewritten in the clear."""
    driver, actor, secret = _sealed_signed_in(nest_instance, spa_url, "nostray")
    try:
        driver.eval_js(f"localStorage.removeItem({OPT_IN_KEY!r})")
        _relaunch_to_unlock(driver)
        assert _unlock_opens(driver, FIRST), _error(driver)
        _signed_in_as(driver, actor)
        rows = driver.eval_js(_IDENTITY_ROWS_JS)
        plain = {k: v[:16] for k, v in rows.items() if not v.startswith("sealed:v1:")}
        assert not plain, f"identity rows rewritten outside the seal: {plain}"
        assert secret not in json.dumps(_store(driver)), "the identity secret rests in the clear"
    finally:
        _teardown(driver)

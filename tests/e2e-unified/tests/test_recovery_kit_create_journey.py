"""tier_3 e2e: the onboarding **recovery-kit** screen and its deferred handoff.

Goal docs: ``docs/goal/behavior/onboarding.md`` § 1 Identity (the screen, its
position after ``identity_created``, and the one state its escrow line may
render) and ``docs/goal/behavior/identity-succession.md`` § The RecoveryKey →
*Creation UX* (the ratified timing: the screen **mints and displays only**;
registration and the escrow ``put`` run in-process at the wizard's signed-in
handoff, the only point custody permits).

The other kit journeys (``test_recovery_kit_settings.py``,
``test_recovery_kit_restore.py``) both start from a *signed-in* device driving
the Settings ceremony. This one covers the path a real first-time user takes,
and the only one whose ceremony is **split across a wizard exit**. Three things
no unit test can establish:

- **The deferred registration actually fires.** The screen has no nest, so it
  registers nothing; the machine hands the minted root to the app, which runs
  the ceremony after ``adopt``. That handoff crosses a wizard teardown, a
  session establish and a task spawn — the exact seam where a "we'll do it
  later" gets dropped. Settings reading *registered* afterwards is the proof,
  and it is read from the registration chain on the nest, never a local flag.
- **The screen sits where the goal doc puts it.** ``identity-continue-button``
  advancing to ``recovery_kit`` rather than straight to ``handle_entry`` is a
  routing claim about the whole create path, and it is gated on the app having
  declared the capability — a declaration a factory reset could silently drop.
- **The screen never claims the account is protected.** At this position it is
  not: nothing is registered and nothing is escrowed. ``onboarding.md`` makes
  that wording iron-clad, and wording is exactly what a renderer gets wrong.

Latency-independent throughout (convention 14): every wait is a named generous
ceiling on an element, and the post-handoff registration is polled to a
deadline through a re-read of the nest's own answer rather than slept on.

tier_3: needs a real ``fauna-nest`` binary. Runs on any app that renders the
screen — today tui, the lead app; the others join as their legs land.
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until

_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)

from common.auth import register_user  # noqa: E402

pytestmark = pytest.mark.tier_3

# The recovery root is 32 bytes as lowercase hex.
_SECRET_HEX_LEN = 64

# Words the screen must never say at this position: nothing is registered and
# nothing is escrowed yet, so any of these would be a lie the user acts on.
_FORBIDDEN_ESCROW_CLAIMS = ("protected", "stored", "backed up", "active now")

# Named ceilings, sized far above any non-pathological delay — a green run never
# spends them (convention 14). Local rather than in `helpers/budgets.py` because
# both describe *this* journey's shape: a full wizard run, and a screen the
# machine mints on entry.
WIZARD_TO_AUTHED_S = 120.0
KIT_SCREEN_RENDER_S = 30.0
# The deferred ceremony is fire-and-forget across a wizard teardown, a session
# establish and a task spawn, then two RPCs — generous, and never spent green.
HANDOFF_REGISTRATION_S = 90.0


def _wait_for_kit_screen_or_skip(app) -> None:
    """Wait for the onboarding kit screen, or skip declaring the class if it
    never renders.

    Does the wait itself, on purpose — same shape as
    ``SettingsActions.open_recovery_kit_or_skip``: a caller that waited first
    and gate-checked second (the shape this replaced, found 2026-08-22) would
    hard-fail with a ``TimeoutError`` on an app lacking the screen instead of
    skipping. A bare ``recovery_kit_showing()`` check with no wait would race
    a genuinely-present but still-rendering screen on a slower app, which is
    why this wraps the real wait rather than swapping the two calls' order.

    Not a declared absence — every app owes this screen. ``skip_unbuilt``
    fails under ``--strict-app`` and is tallied every run, so the gap stays
    visible instead of reading as a pass.
    """
    try:
        app.driver.wait_for("recovery-kit-secret-display", timeout=KIT_SCREEN_RENDER_S)
        return
    except Exception:
        pass
    skip_unbuilt(
        app.driver,
        surface="onboarding-recovery-kit-screen",
        detail=(
            "onboarding.md § 1 Identity; tui leads and the other six follow "
            "in batched trickle-down"
        ),
        tracked=(
            "docs/goal/behavior/identity-succession.md "
            "§ Implementation status today"
        ),
    )


def _sign_in_as(app, nest, handle: str) -> None:
    """Drive `handle_entry` → the authenticated app for an already-admitted actor.

    **Why this journey needs the TLS nest** (`self_signed_nest`) rather than the
    shared plain-HTTP `nest_instance`, and why a `set_provider_base_urls`
    override cannot stand in for it: the typed handle resolves to
    ``https://<host>:<port>`` (uniform https — the host class no longer picks
    the scheme), and while the override does redirect the *pre-identity* probe,
    the **authenticated** connection `session::adopt` opens afterwards still
    dials the resolved https URL. Against a plain-HTTP nest that connection dies
    (`rpc disconnected`), which surfaces as every post-login page read failing.
    A nest serving its real self-signed floor cert on loopback is what the
    client trusts by the same rule production uses, so nothing is faked.
    """
    app.onboarding.fill_handle(f"{handle}@{nest['url'].split('//', 1)[1]}")
    app.onboarding.run_handle_check(timeout=45)

    # Read the check's own answer BEFORE clicking, so a probe failure diagnoses
    # itself here instead of as a disabled button two frames later
    # (convention 6). The actor was admitted above, so the resolving outcome is
    # the already-on-nest one; `run_handle_check` deliberately returns on ANY
    # terminal message, including an error.
    message = app.driver.get_text("handle-message-area")
    assert app.is_enabled("handle-entry-continue-button"), (
        "the handle check must resolve this admitted actor to the local nest and "
        "enable Continue; message-area reads "
        f"{message!r}, error surface: {app.error_text()!r}"
    )

    app.onboarding.submit_handle()
    _finish_wizard_to_logged_in(app)
    assert app.driver.get_state("session.authenticated"), (
        "the wizard rendered the authed shell but the session is not "
        f"authenticated; error surface: {app.error_text()!r}"
    )


def _open_recovery_section(app) -> str:
    """Land on Settings → Account and return the recovery-kit status line.

    Deliberately NOT wrapped in a retry. While this journey was being written it
    failed here with a driver ack timeout, which looked like a post-sign-in
    settling window worth tolerating — it was not. The app's error surface named
    the real cause (`rpc disconnected`): the authenticated connection was dead,
    so every page read failed and the main loop never acked. Swallowing that
    would have turned a broken transport into a slow-looking test. The fixture
    is the fix (see :func:`_sign_in_as`); a timeout here again means something is
    genuinely wrong, and it should say so on the first pass.
    """
    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    return app.settings.recovery_kit_status()


def _finish_wizard_to_logged_in(app, timeout: float = WIZARD_TO_AUTHED_S) -> None:
    """Drive whatever post-handle screens this flow surfaces until the authed
    app renders. Mirrors ``test_mail_auto_enable_first_setup``'s helper — the
    welcome-back path may park on the terminal NAT-mode confirm or a launch
    retry, and neither is what this test is about.
    """
    # The same marker set `test_mail_auto_enable_first_setup` uses: the landing
    # view differs per app, so any of them means "the authed app renders".
    markers = ("feed-view", "feed-tab", "new-conversation-button", "settings-tab")

    def authed_or_click_through() -> bool:
        """True once the authed shell renders; otherwise dismiss whatever
        interstitial is up and let the next poll look again. Clicking inside the
        predicate is what keeps this a deadline poll rather than a sleep loop
        (convention 14) — `wait_until` owns the interval."""
        if any(app.driver.is_visible(m) for m in markers):
            return True
        for btn in ("nat-mode-confirm-button", "launch-retry-button"):
            try:
                if app.driver.is_visible(btn):
                    app.driver.click(btn)
            except Exception:
                pass
        return False

    wait_until(
        authed_or_click_through,
        timeout,
        diagnose=lambda: (
            "the wizard never reached the authenticated app after the "
            f"create-path onboarding; error surface: {app.error_text()!r}"
        ),
    )


@pytest.mark.feature("recovery-kit-at-sign-up")
def test_the_kit_minted_at_onboarding_is_registered_by_the_signed_in_handoff(
    app, self_signed_nest
):
    """create identity → kit screen → confirm → sign in → Settings reads registered.

    The account is admitted admin-side between the identity screen and the
    handle check — fixture setup arranging a precondition (convention 8), not a
    shortcut around the behaviour under test: every mutation this test asserts
    on (minting the kit, confirming it, and the registration the handoff runs)
    goes through the app.
    """
    # The journey signs in against `self_signed_nest` at its end (`_sign_in_as`),
    # so relaunch trusting it HERE, before any in-app state (identity, kit) is
    # built — a relaunch after would wipe that state (`_sign_in_as` runs after
    # it is minted, too late for a fresh launch).
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, self_signed_nest)

    # ── Phase 1: a genuinely fresh device on the create path ───────────────
    # `driver.reset()` is the factory reset: clear the credential namespace and
    # return to onboarding, no relaunch. The create path is only reachable from
    # here, and the reset is also what proves the app's kit-screen capability
    # declaration survives one (`app.rs::reset_keeps_the_recovery_kit_declaration`).
    app.driver.reset()
    app.onboarding.navigate_to_status()
    app.onboarding.generate_identity()

    # The identity's own secret, read off the screen the user reads it off.
    # Needed to admit the account below; the app keeps its own copy either way.
    secret_hex = app.driver.get_text("secret-key-display").strip()
    assert len(secret_hex) == _SECRET_HEX_LEN, (
        f"identity_created shows the 64-hex seed, got {len(secret_hex)} chars; "
        f"error surface: {app.error_text()!r}"
    )

    # ── Phase 2: the kit screen, where the goal doc puts it ────────────────
    app.driver.click("identity-continue-button")
    # Continue advances to `recovery_kit`, NOT straight to `handle_entry`
    # (`onboarding.md` § 1 Identity, changed 2026-08-01 with the screen).
    _wait_for_kit_screen_or_skip(app)

    kit_phrase = app.onboarding.recovery_kit_secret()
    assert len(kit_phrase) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(kit_phrase)} chars; "
        f"error surface: {app.error_text()!r}"
    )
    assert kit_phrase != secret_hex, (
        "the RecoveryKey is a FRESH random root, independent of the identity "
        "seed (§ The RecoveryKey — *Material*); showing the seed back would "
        "mean the whole plane recovers nothing"
    )

    # The one state this screen's escrow line may render. Nothing is registered
    # and nothing is escrowed at this position, so a line implying otherwise
    # sends a user away believing a written-down phrase is already live.
    escrow_line = app.onboarding.recovery_kit_escrow_status().lower()
    assert escrow_line, (
        "the screen must say what the kit's status actually is; "
        f"error surface: {app.error_text()!r}"
    )
    for claim in _FORBIDDEN_ESCROW_CLAIMS:
        assert claim not in escrow_line, (
            "at this position no nest exists, so the screen must never imply "
            f"the account is already protected; line reads {escrow_line!r} "
            f"(matched {claim!r})"
        )

    # ── Phase 3: admit the account, then finish the wizard ─────────────────
    # The handle check runs a silent challenge as this brand-new actor; the
    # harness nest is closed-registration, so admit it the way the shared
    # fixtures admit every other e2e actor.
    handle = f"e2e-kit-{secret_hex[:10]}"
    from nacl.signing import SigningKey

    actor_id_hex = bytes(SigningKey(bytes.fromhex(secret_hex)).verify_key).hex()
    register_user(
        self_signed_nest["port"],
        actor_id_hex,
        base_url=self_signed_nest["url"],
        admin_signing_key=self_signed_nest["admin"]["signing_key"],
        handle=handle,
    )

    app.onboarding.confirm_recovery_kit()

    _sign_in_as(app, self_signed_nest, handle)

    # ── Phase 4: the deferred ceremony's proof ─────────────────────────────
    # The handoff registration is fire-and-forget (a failure degrades to the
    # Settings never-created warning by design), so the assertion is a deadline
    # poll of the nest's own answer — re-navigating the page each pass, since
    # the section reads the chain on entry. Not a settle-sleep: the budget is a
    # generous ceiling that a green run never spends.
    status = ""

    def registration_landed() -> bool:
        nonlocal status
        status = _open_recovery_section(app)
        return bool(status) and not app.is_enabled("recovery-kit-create-button")

    wait_until(
        registration_landed,
        HANDOFF_REGISTRATION_S,
        diagnose=lambda: (
            f"status reads {status!r}, error surface: {app.error_text()!r}"
        ),
    )

    assert status, (
        "the Settings section must render a status once the chain read "
        f"resolves; error surface: {app.error_text()!r}"
    )
    assert not app.is_enabled("recovery-kit-create-button"), (
        "the kit minted at onboarding must be REGISTERED by the signed-in "
        "handoff — create still being offered means the deferred ceremony "
        f"never ran or never landed; status reads {status!r}"
    )
    # The secret is never shown again, on any surface, ever.
    assert app.settings.recovery_kit_secret() == "", (
        "the kit was displayed once at onboarding; Settings must not be able "
        "to bring it back (§ The RecoveryKey — *Custody*)"
    )


@pytest.mark.feature("recovery-kit-at-sign-up")
def test_skipping_the_kit_registers_nothing_and_says_so(app, self_signed_nest):
    """skip → sign in → Settings still offers create.

    The other half of the ratified screen: skipping costs one click, never
    blocks onboarding, and drops the minted root so nothing registers at the
    handoff. Worth a journey because the failure mode is *silent* — a skip that
    still registered would leave the user believing they hold a phrase they
    never wrote down, and the only witness is the nest's chain.
    """
    # Relaunch trusting `self_signed_nest` BEFORE any in-app state is built —
    # see the sibling journey above for why this can't wait until `_sign_in_as`.
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, self_signed_nest)

    app.driver.reset()
    app.onboarding.navigate_to_status()
    app.onboarding.generate_identity()
    secret_hex = app.driver.get_text("secret-key-display").strip()
    assert len(secret_hex) == _SECRET_HEX_LEN

    app.driver.click("identity-continue-button")
    _wait_for_kit_screen_or_skip(app)

    handle = f"e2e-skip-{secret_hex[:10]}"
    from nacl.signing import SigningKey

    actor_id_hex = bytes(SigningKey(bytes.fromhex(secret_hex)).verify_key).hex()
    register_user(
        self_signed_nest["port"],
        actor_id_hex,
        base_url=self_signed_nest["url"],
        admin_signing_key=self_signed_nest["admin"]["signing_key"],
        handle=handle,
    )

    app.onboarding.skip_recovery_kit()

    _sign_in_as(app, self_signed_nest, handle)

    # Anchor on the section having ANSWERED — the status line resolving — then
    # assert WHICH answer, rather than sleeping for a registration that must
    # never arrive (convention 14's negative-assert rule: the causal barrier is
    # the chain read itself completing).
    status = _open_recovery_section(app)
    wait_until(
        lambda: bool(status or app.settings.recovery_kit_status()),
        30.0,
        diagnose=lambda: f"error surface: {app.error_text()!r}",
    )
    assert app.is_enabled("recovery-kit-create-button"), (
        "a skipped kit registers NOTHING, so Settings must still offer the "
        "first-registration action; status reads "
        f"{app.settings.recovery_kit_status()!r}"
    )


@pytest.mark.feature("recovery-kit-at-sign-up")
def test_confirming_the_kit_then_never_signing_in_leaves_settings_honest(
    app, self_signed_nest
):
    """Confirm "I've saved it", abandon the wizard before it signs in, then sign
    in later — Settings says the kit was never created.

    `onboarding.md` § 1. Identity: "A handoff whose `put` fails, **or a wizard
    exit that never signs in**, degrades to the Settings status truth … so a
    written-down phrase can be inert but the app never claims it is active."

    This is the sharp twin of the skip journey above, and the sharper of the
    two. A user who SKIPPED knows they declined. A user who confirmed has
    written a phrase down and believes they are protected — so this is the one
    case where Settings claiming a kit exists would send someone away trusting
    a phrase that recovers nothing. The screen legitimately mints and displays
    only: no nest exists at that position, so registration and the escrow `put`
    are deferred to the signed-in handoff, and a wizard that never reaches it
    persists nothing (the root is taken and dropped, never written). Nothing
    local records the attempt, which is exactly why the honest answer has to
    come from the nest's own registration chain — `kit_status` reads the chain,
    never a local flag, so "never created" here is a real absence rather than a
    forgotten note.

    The abandonment is `reset()` — the wizard is torn down between confirming
    the kit and reaching `LoggedIn`, which is what "never finish signing in"
    IS. The actor is admitted out of band first so the later sign-in has an
    account to land on; that registration is the nest admitting a user, never
    the kit ceremony, so it cannot manufacture the thing under test.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, self_signed_nest)

    app.driver.reset()
    app.onboarding.navigate_to_status()
    app.onboarding.generate_identity()
    secret_hex = app.driver.get_text("secret-key-display").strip()
    assert len(secret_hex) == _SECRET_HEX_LEN

    app.driver.click("identity-continue-button")
    _wait_for_kit_screen_or_skip(app)

    # Read the phrase the way the user would — this is the copy they write
    # down, and the whole premise of the assertion at the end is that they
    # hold it while the account does not.
    kit_phrase = app.onboarding.recovery_kit_secret()
    assert len(kit_phrase) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(kit_phrase)} chars; "
        f"error surface: {app.error_text()!r}"
    )

    handle = f"e2e-abandon-{secret_hex[:10]}"
    from nacl.signing import SigningKey

    actor_id_hex = bytes(SigningKey(bytes.fromhex(secret_hex)).verify_key).hex()
    register_user(
        self_signed_nest["port"],
        actor_id_hex,
        base_url=self_signed_nest["url"],
        admin_signing_key=self_signed_nest["admin"]["signing_key"],
        handle=handle,
    )

    # CONFIRM — not skip. The user has saved the phrase and believes it is live.
    app.onboarding.confirm_recovery_kit()

    # …and then the wizard ends without ever signing in. The deferred ceremony
    # runs at the signed-in handoff and only there, so it never runs at all.
    app.driver.reset()

    # The user comes back later and signs in with the key they created — which
    # is the only way the inert phrase is ever discovered. The identity has to
    # be re-imported because the abandoned run committed it to nothing; that is
    # the same fact the assertion is about, seen from the other side.
    app.onboarding.navigate_to_status()
    app.onboarding.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=KIT_SCREEN_RENDER_S)

    _sign_in_as(app, self_signed_nest, handle)

    status = _open_recovery_section(app)
    wait_until(
        lambda: bool(status or app.settings.recovery_kit_status()),
        30.0,
        diagnose=lambda: f"error surface: {app.error_text()!r}",
    )
    assert app.is_enabled("recovery-kit-create-button"), (
        "the wizard exited before signing in, so the confirmed kit was never "
        "registered — Settings must offer the first-registration action rather "
        "than imply a kit exists. The user is holding a phrase that recovers "
        f"nothing; status reads {app.settings.recovery_kit_status()!r}"
    )

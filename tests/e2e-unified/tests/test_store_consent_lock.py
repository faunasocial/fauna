"""The store-consent lock journey (`account-age-band.md` § The account age band
→ the store-law developer duties bullet, items (3), (4), (6) and (7); § App surface →
*Store-consent surfaces*).

The two store-distributed mobile apps lock the WHOLE app — at launch and on
every return to the foreground, before any page — while the store reports a
significant app change awaiting the store parent's consent, or declined. The
gate keys on the store's own signal for the person on this device, never on
supervision or the age band, so it needs no nest and no sign-in.

The agent stands in for the platform read only (`store_consent_inject`, the
`set_age_claim` precedent): the app runs the real shared round
(`fauna_ffi::store_consent_round`) over it and paints `store-consent-lock` from
the round's own message. The shipped significant-change ledger is empty until
the owner admits its first entry (D-S8), so the journeys drive the arms that
lock without one — android the Play status (a `PENDING` / `DECLINED` locks on
its own), iOS the `AppStore.ageRatingCode` trigger (item (4): a rating changed
against the install's stored one is a synthetic pending change), with an
injected PermissionKit answer standing in for the parent's.

**Strings.** The `family.store_consent_lock.*` lines are public texts (shown
before sign-in), landed in public-texts round 2; the journey asserts their
resolved English wording.

**App scope.** android and ios, each on its own store's arm (the other's is a
declared absence — item (1): the gate keys on the platform's own signal). The
five other apps declare the absence.
"""

import pytest

from i18n.strings import S

pytestmark = pytest.mark.tier_2


_DOC = (
    "account-age-band.md § The account age band → the store-law developer "
    "duties bullet, item (6): a store construct on the two store-distributed "
    "mobile apps; the five other apps declare the absence"
)


_SIGNAL_DOC = (
    "account-age-band.md § The account age band → the store-law developer "
    "duties bullet, item (1): the gate keys on the platform's own signal — "
    "iOS DeclaredAgeRange + StoreKit, android Play Age Signals"
)


def _skip_unless_store_app(app, store: str):
    """Run only on `store`'s app (`"android"` — the Play arm, or `"ios"` — the
    Apple arm); the other store app reads the other store's signal, and the
    five other apps have no store signal at all."""
    from helpers.app_surface import app_name, declared_absence

    name = app_name(app.driver)
    if name == store:
        return
    if name in ("android", "ios"):
        declared_absence(
            app.driver,
            capability=f"{store}'s store significant-change signal (it reads its own store's)",
            doc=_SIGNAL_DOC,
        )
    declared_absence(
        app.driver,
        capability="store significant-change signal, so no store-consent-lock",
        doc=_DOC,
    )


def _diag(app) -> str:
    return (
        f"{app.driver.diagnose('store-consent-lock')} "
        f"error={app.error_text()!r}"
    )


def test_a_play_pending_locks_the_app_and_an_approval_lifts_it(app):
    """PENDING → the lock paints with its message (title, the waiting state,
    the Google Play hint) and no ask button — Play raises the parent's request
    itself. APPROVED (no approval date: trust the store) → the lock lifts."""
    _skip_unless_store_app(app, "android")
    family = app.family
    try:
        family.inject_play_consent_signal("PENDING")
        assert app.is_visible("store-consent-lock"), (
            f"a Play PENDING locks the app: {_diag(app)}"
        )
        message = family.store_consent_lock_message()
        lock = S.family.store_consent_lock
        for line in (lock.title, lock.state_pending, lock.play_hint):
            assert line in message, f"the lock's message carries {line!r}: {message!r}"
        assert lock.ask_hint not in message, (
            f"android names no in-app ask: {message!r}"
        )
        assert app.is_absent("store-consent-lock-ask-button"), (
            "no ask button on android (Play asks the parent itself): "
            f"{app.driver.diagnose('store-consent-lock-ask-button')}"
        )

        family.inject_play_consent_signal("APPROVED")
        assert app.is_absent("store-consent-lock"), (
            f"an approval lifts the lock: {_diag(app)}"
        )
    finally:
        family.clear_store_consent_signal()


def test_a_play_decline_reads_so(app):
    """DECLINED → the lock stays, saying the parent said no."""
    _skip_unless_store_app(app, "android")
    family = app.family
    try:
        family.inject_play_consent_signal("DECLINED", "2026-01-01")
        assert app.is_visible("store-consent-lock"), (
            f"a Play DECLINED locks the app: {_diag(app)}"
        )
        message = family.store_consent_lock_message()
        lock = S.family.store_consent_lock
        assert lock.state_declined in message, (
            f"the declined state reads so: {message!r}"
        )
        assert lock.state_pending not in message, message
    finally:
        family.clear_store_consent_signal()


def test_no_play_status_never_gates(app):
    """A person Play reports no significant-change status for is outside the
    duty — no lock, whatever else the store says."""
    _skip_unless_store_app(app, "android")
    family = app.family
    try:
        family.inject_play_consent_signal(None, "2026-01-01")
        assert app.is_absent("store-consent-lock"), (
            f"no status, no gate: {_diag(app)}"
        )
    finally:
        family.clear_store_consent_signal()


def _baseline_ios(family, *, parental: bool = True, rating: int = 4) -> None:
    """A first-run install whose rating read is the silent baseline."""
    family.reset_store_consent_state()
    family.inject_apple_consent_signal(
        parental_consent_required=parental, current_rating=rating
    )


def test_an_ios_rating_change_locks_and_an_approved_ask_lifts_it(app):
    """The duty applies (parental consent) and the App Store rating changed
    since the install's baseline → the lock paints with its message (title, what
    changed, the rating line, the waiting state, the ask hint) and the ask
    button; tapping it sends ONE ask and lifts nothing; the parent's approval
    arriving lifts the lock, and the same rating read again stays unlocked —
    the approval consented through it."""
    _skip_unless_store_app(app, "ios")
    family = app.family
    try:
        _baseline_ios(family, rating=4)
        assert app.is_absent("store-consent-lock"), (
            f"the first rating read is a silent baseline: {_diag(app)}"
        )

        family.inject_apple_consent_signal(parental_consent_required=True, current_rating=9)
        assert app.is_visible("store-consent-lock"), (
            f"a changed rating under the duty locks the app: {_diag(app)}"
        )
        message = family.store_consent_lock_message()
        lock = S.family.store_consent_lock
        for line in (lock.title, lock.what_changed, lock.rating_changed,
                     lock.state_pending, lock.ask_hint):
            assert line in message, f"the lock's message carries {line!r}: {message!r}"
        assert lock.play_hint not in message, f"iOS names no Google Play: {message!r}"
        assert app.is_visible("store-consent-lock-ask-button"), (
            "iOS carries the ask button: "
            f"{app.driver.diagnose('store-consent-lock-ask-button')}"
        )
        label = app.driver.get_text("store-consent-lock-ask-button") or ""
        assert lock.ask_button in label, f"the ask button's label: {label!r}"

        family.ask_store_parent()
        asks = app.driver.get_state(
            "store_consent.asks_sent", wait_for=lambda n: n == 1, timeout=10
        )
        assert asks == 1, f"one tap sends one ask: asks_sent={asks!r} {_diag(app)}"
        assert app.is_visible("store-consent-lock"), (
            f"sending the ask lifts nothing: {_diag(app)}"
        )

        family.answer_store_consent_ask("approved")
        assert app.is_absent("store-consent-lock"), (
            f"the parent's approval lifts the lock: {_diag(app)}"
        )

        family.inject_apple_consent_signal(parental_consent_required=True, current_rating=9)
        assert app.is_absent("store-consent-lock"), (
            f"the approval consented through the new rating: {_diag(app)}"
        )
    finally:
        family.clear_store_consent_signal()
        family.reset_store_consent_state()


def test_an_ios_decline_reads_so_and_keeps_the_ask(app):
    """The parent declines → the lock stays, saying so, and still offers the
    ask button (a re-ask returns it to waiting)."""
    _skip_unless_store_app(app, "ios")
    family = app.family
    try:
        _baseline_ios(family, rating=4)
        family.inject_apple_consent_signal(parental_consent_required=True, current_rating=5)
        family.answer_store_consent_ask("declined")
        assert app.is_visible("store-consent-lock"), (
            f"a declined change keeps the lock: {_diag(app)}"
        )
        message = family.store_consent_lock_message()
        lock = S.family.store_consent_lock
        assert lock.state_declined in message, (
            f"the declined state reads so: {message!r}"
        )
        assert lock.state_pending not in message, message
        assert app.is_visible("store-consent-lock-ask-button"), (
            f"a declined lock can ask again: {_diag(app)}"
        )
    finally:
        family.clear_store_consent_signal()
        family.reset_store_consent_state()


def test_an_ios_rating_change_outside_the_duty_never_gates(app):
    """A person the store reports no significant-change duty for is outside it
    — a changed rating locks nothing."""
    _skip_unless_store_app(app, "ios")
    family = app.family
    try:
        _baseline_ios(family, parental=False, rating=4)
        family.inject_apple_consent_signal(parental_consent_required=False, current_rating=9)
        assert app.is_absent("store-consent-lock"), (
            f"no duty, no gate: {_diag(app)}"
        )
    finally:
        family.clear_store_consent_signal()
        family.reset_store_consent_state()

"""The store-consent lock journey (`account-age-band.md` § The account age band
→ the store-law developer duties bullet, items (3), (6) and (7); § App surface →
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
the legal review admits its first entry (D-S8), so the journey drives the
status arm — a Play `PENDING` / `DECLINED` locks on its own.

**Strings.** The `family.store_consent_lock.*` lines are public texts (shown
before sign-in), landed in public-texts round 2; the journey asserts their
resolved English wording.

**App scope.** android only today. iOS is unbuilt (its journey adds the ask
button). The five other apps declare the absence.
"""

import pytest

from i18n.strings import S

pytestmark = pytest.mark.tier_2


_DOC = (
    "account-age-band.md § The account age band → the store-law developer "
    "duties bullet, item (6): a store construct on the two store-distributed "
    "mobile apps; the five other apps declare the absence"
)


def _skip_unless_store_app(app):
    from helpers.app_surface import app_name, declared_absence, skip_unbuilt

    name = app_name(app.driver)
    if name == "android":
        return
    if name == "ios":
        skip_unbuilt(
            app.driver,
            surface="store-consent-lock",
            detail="the iOS round over DeclaredAgeRange + PermissionKit",
            tracked="account-age-band.md § Implementation status today",
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
    _skip_unless_store_app(app)
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
    _skip_unless_store_app(app)
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
    _skip_unless_store_app(app)
    family = app.family
    try:
        family.inject_play_consent_signal(None, "2026-01-01")
        assert app.is_absent("store-consent-lock"), (
            f"no status, no gate: {_diag(app)}"
        )
    finally:
        family.clear_store_consent_signal()

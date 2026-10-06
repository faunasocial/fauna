"""E2E coverage for invite_request InviteRequestSnapshot states.

Each test uses set_invite_request_snapshot to put the wizard in a
specific state, then asserts the page renders the expected elements
+ Continue gating. Pure UI rendering — the snapshot setter parks the
machine without driving the real probe path.

Per the onboarding client target-state design (tracked internally), §3:
- state: InviteRequestState ∈ {Idle, Submitting, Rechecking,
    Denied{reason, request_id},
    PendingReview{request_id, last_checked_ms},
    Error{transient, context, cause}}
  (the former Approved{quota, request_id} retired 2026-08-12 — no live nest
  ever served it, and the injected-Approved case that used to live in this
  file is precisely what made the variant look reachable for months)
- out_of_band_code_state: OobCodeState ∈ {Idle, Verifying,
    Valid{invite_id}, Invalid{reason}, Error{cause}}
- continue_enabled, recheck_visible

These tests are RED at the end of Plan 1; per-app wiring (Plans 2-6)
turns them green one app at a time. Web app landed in this file.
"""

import json

import pytest

from drivers.machine_test_setter import set_invite_request_snapshot

pytestmark = pytest.mark.tier_2


def _snapshot(*, state, oob='Idle',
              continue_enabled=True, recheck_visible=False,
              message_key='onboarding.invite.idle', message_args=None):
    # `oob_message` is populated server-side when the bridge calls
    # `inviteRequestSnapshot()` (see `oob_message_for` in
    # `libs/fauna-onboarding-machine/src/snapshots/invite_request.rs`).
    # For the test setter we pass an empty LocalizedText; the read-side
    # getter overwrites it before delivery.
    return {
        'state': state,
        'message': {'key': message_key, 'args': message_args or {}},
        'continue_enabled': continue_enabled,
        'recheck_visible': recheck_visible,
        'out_of_band_code_state': oob,
        'oob_message': {'key': '', 'args': {}},
    }


def _skip_unless_store_app(app):
    """The notice is a `platform_elements` entry for android/ios only; the five
    apps without a store age signal declare the absence (D3: at most 2 of 7 apps
    ever receive one)."""
    from helpers.app_surface import app_name, declared_absence

    if app_name(app.driver) not in ("android", "ios"):
        declared_absence(
            app.driver,
            capability="store age signal, so no invite-request-age-notice",
            doc=(
                "family-safety.md § The account age band (D3: at most the two "
                "store-distributed mobile apps receive a store signal); "
                "apps/tui.md § Declared platform absences item 7"
            ),
        )


def _attested_claim(platform, nonce_hex):
    return {
        "band": "13-15",
        "attestation": {
            "platform": platform,
            "nonce_hex": nonce_hex,
            "key_id_hex": "",
            "attestation_object": list(b"token"),
        },
    }


def test_age_notice_is_declared_for_an_attestation_the_nest_did_not_list(app):
    """`invite-request-age-notice` (family-safety.md § App surface → *Age-band
    surfaces*): the notice tells the user what the nest will RECORD. The shared
    machine puts an attestation on the wire only when its nonce is the one last
    minted from the addressed nest and that reply listed the platform (§ The
    account age band → *An attestation the nest cannot check*) — so an attested
    claim with no nonce minted rides declared-only, and the notice takes the
    declared form, blaming nobody. Clearing the claim clears the notice.
    """
    _skip_unless_store_app(app)
    set_invite_request_snapshot(app, _snapshot(state='Idle', continue_enabled=False))
    assert app.is_absent('invite-request-age-notice'), (
        "no claim → no notice: "
        f"{app.driver.diagnose('invite-request-age-notice')} error={app.error_text()!r}"
    )
    app.driver.call_machine_method(
        "set_age_claim", json.dumps(_attested_claim("android", "ab" * 32)),
    )
    set_invite_request_snapshot(app, _snapshot(state='Idle', continue_enabled=False))
    assert app.is_visible('invite-request-age-notice'), (
        "a claim renders the notice: "
        f"{app.driver.diagnose('invite-request-age-notice')} error={app.error_text()!r}"
    )
    text = app.get_text('invite-request-age-notice')
    assert "13" in text and "as declared" in text, (
        f"an attestation the nest did not list rides declared-only: {text!r}"
    )
    assert "Google Play" not in text, (
        f"the declared form names no store and claims no verification: {text!r}"
    )
    app.driver.call_machine_method("set_age_claim", json.dumps(None))
    set_invite_request_snapshot(app, _snapshot(state='Idle', continue_enabled=False))
    assert app.is_absent('invite-request-age-notice'), (
        "clearing the claim clears the notice: "
        f"{app.driver.diagnose('invite-request-age-notice')} error={app.error_text()!r}"
    )


def test_age_notice_follows_the_minted_nonces_platform_list(app, nest_instance):
    """The verified form is reached the way the glue reaches it: mint the nonce
    through the machine (`request_age_nonce`, against the real nest), attest
    this app's platform over it. The nest's `attestation_platforms` decides —
    a listed platform renders "verified by …" naming the store, an unlisted
    one (android while the Play Integrity keys are unset) the declared form.
    No bridge path bypasses the guard.
    """
    from helpers.app_surface import app_name, skip_unbuilt

    _skip_unless_store_app(app)
    platform = app_name(app.driver)
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_instance["url"], "agekid"]),
    )
    raw = app.driver.call_machine_method("request_age_nonce", "")
    if raw is None:
        skip_unbuilt(
            app.driver,
            surface="request_age_nonce bridge reader",
            detail="the bridge does not route the machine's async dispatcher",
            tracked="family-safety.md § Implementation status today, the age-band row",
        )
    minted = json.loads(raw) if isinstance(raw, str) else raw
    assert "Ok" in minted, f"the nonce mint must succeed: {minted!r}"
    nonce = minted["Ok"]
    app.driver.call_machine_method(
        "set_age_claim", json.dumps(_attested_claim(platform, nonce["nonce_hex"])),
    )
    set_invite_request_snapshot(app, _snapshot(state='Idle', continue_enabled=False))
    text = app.get_text('invite-request-age-notice')
    store = {"android": "Google Play", "ios": "App Store"}[platform]
    if platform in nonce["attestation_platforms"]:
        assert store in text and "verified by" in text, (
            f"a listed platform over the minted nonce renders the verified form: {text!r}"
        )
    else:
        assert "as declared" in text and store not in text, (
            f"a platform the nest did not list renders the declared form: {text!r}"
        )
    app.driver.call_machine_method("set_age_claim", json.dumps(None))


@pytest.mark.parametrize("driver_kind", ["web", "linux", "windows", "ios", "macos", "android"])
def test_idle_disables_continue(app, driver_kind):
    set_invite_request_snapshot(app, _snapshot(state='Idle', continue_enabled=False))
    assert app.is_visible('invite-request-submit-button'), (
        "Idle should render the submit button: "
        f"{app.driver.diagnose('invite-request-submit-button')} error={app.error_text()!r}"
    )
    assert app.is_visible('invite-code-input'), (
        "Idle should render the invite-code input: "
        f"{app.driver.diagnose('invite-code-input')} error={app.error_text()!r}"
    )
    assert not app.is_enabled('invite-request-continue-button'), (
        "Idle (continue_enabled=False) should disable Continue: "
        f"{app.driver.diagnose('invite-request-continue-button')} error={app.error_text()!r}"
    )
    # Recheck is not visible in Idle.
    assert app.is_absent('invite-request-recheck-button'), (
        "Idle should not show the recheck button: "
        f"{app.driver.diagnose('invite-request-recheck-button')} error={app.error_text()!r}"
    )


def test_submitting_locks_form(app):
    set_invite_request_snapshot(app, _snapshot(
        state='Submitting',
        continue_enabled=False,
        message_key='onboarding.invite.submitting',
    ))
    assert app.is_visible('invite-request-status'), (
        "Submitting should render the status line: "
        f"{app.driver.diagnose('invite-request-status')} error={app.error_text()!r}"
    )
    text = app.get_text('invite-request-status')
    assert text  # message rendered
    assert not app.is_enabled('invite-request-continue-button'), (
        "Submitting should disable Continue: "
        f"{app.driver.diagnose('invite-request-continue-button')} error={app.error_text()!r}"
    )


@pytest.mark.feature("join-a-nest")
def test_pending_review_shows_recheck_and_disables_continue(app):
    """PendingReview offers recheck, and Continue is DEAD.

    The Continue assertion flipped 2026-08-12: that button used to exit the
    wizard via `submit_invite_request_continue()`, and the exit is retired —
    this journey advances by polling (`onboarding.md` § The pending-invite
    surface). A live Continue here would be a control with nothing behind it,
    which is the failure `continue_enabled_for` exists to prevent.
    """
    set_invite_request_snapshot(app, _snapshot(
        state={'PendingReview': {'request_id': 'req-pending', 'last_checked_ms': 0}},
        continue_enabled=False,
        recheck_visible=True,
        message_key='onboarding.invite.pending_review',
    ))
    assert app.is_visible('invite-request-recheck-button'), (
        "PendingReview should show the recheck button: "
        f"{app.driver.diagnose('invite-request-recheck-button')} error={app.error_text()!r}"
    )
    assert not app.is_enabled('invite-request-continue-button'), (
        "PendingReview must DISABLE Continue — the journey advances by polling: "
        f"{app.driver.diagnose('invite-request-continue-button')} error={app.error_text()!r}"
    )


# ⚠ There is deliberately no `test_approved_*` case (retired 2026-08-12 with
# `InviteRequestState::Approved`). It injected a state no live nest can serve —
# an admin approve DELETES the request row — and by looking green it is what
# kept the variant, and its doubly-dead redeem leg, alive for months. The real
# approval journey is covered by `test_pending_invite_journey.py`, which drives
# an out-of-band admin approve and asserts the requester advances with no user
# action. Do not reintroduce an injected approval here.


@pytest.mark.feature("join-a-nest")
def test_denied_shows_reason_disables_continue(app):
    set_invite_request_snapshot(app, _snapshot(
        state={'Denied': {'reason': 'we are not accepting requests right now',
                          'request_id': 'req-denied'}},
        continue_enabled=False,
        message_key='onboarding.invite.denied',
        message_args={'reason': 'we are not accepting requests right now'},
    ))
    assert not app.is_enabled('invite-request-continue-button'), (
        "Denied should disable Continue: "
        f"{app.driver.diagnose('invite-request-continue-button')} error={app.error_text()!r}"
    )
    text = app.get_text('invite-request-status')
    assert 'not accepting' in text


def test_error_transient_disables_continue(app):
    set_invite_request_snapshot(app, _snapshot(
        state={'Error': {'transient': True, 'context': 'Submitting',
                         'cause': 'timeout'}},
        continue_enabled=False,
        message_key='onboarding.invite.error.transient',
        message_args={'cause': 'timeout'},
    ))
    assert not app.is_enabled('invite-request-continue-button'), (
        "Error (transient) should disable Continue: "
        f"{app.driver.diagnose('invite-request-continue-button')} error={app.error_text()!r}"
    )


@pytest.mark.feature("join-a-nest")
def test_oob_valid_enables_continue(app):
    """An accepted out-of-band code is sufficient by itself to enable
    Continue (which routes to redeemInvite). No admin approval needed."""
    set_invite_request_snapshot(app, _snapshot(
        state='Idle',
        oob={'Valid': {'invite_id': 'inv-1'}},
        continue_enabled=True,
        message_key='onboarding.oob_code.valid',
    ))
    assert app.is_enabled('invite-request-continue-button'), (
        "OOB Valid should enable Continue: "
        f"{app.driver.diagnose('invite-request-continue-button')} error={app.error_text()!r}"
    )
    assert app.is_visible('invite-code-status'), (
        "OOB Valid should render the invite-code status: "
        f"{app.driver.diagnose('invite-code-status')} error={app.error_text()!r}"
    )


def test_oob_invalid_shows_reason(app):
    set_invite_request_snapshot(app, _snapshot(
        state='Idle',
        oob={'Invalid': {'reason': 'expired'}},
        continue_enabled=False,
        message_key='onboarding.oob_code.invalid',
        message_args={'reason': 'expired'},
    ))
    text = app.get_text('invite-code-status')
    assert 'expired' in text
    assert not app.is_enabled('invite-request-continue-button'), (
        "OOB Invalid should disable Continue: "
        f"{app.driver.diagnose('invite-request-continue-button')} error={app.error_text()!r}"
    )


def test_back_button_always_visible(app):
    """Back is enabled in every state per target-state §3."""
    set_invite_request_snapshot(app, _snapshot(state='Idle'))
    assert app.is_visible('invite-request-back-button'), (
        "Idle should render the back button: "
        f"{app.driver.diagnose('invite-request-back-button')} error={app.error_text()!r}"
    )
    assert app.is_enabled('invite-request-back-button'), (
        "Back should be enabled in Idle: "
        f"{app.driver.diagnose('invite-request-back-button')} error={app.error_text()!r}"
    )

    set_invite_request_snapshot(app, _snapshot(
        state={'Error': {'transient': False, 'context': 'Rechecking',
                         'cause': 'invite.error.not_found'}},
        continue_enabled=False,
    ))
    assert app.is_enabled('invite-request-back-button'), (
        "Back should be enabled even in the Error state: "
        f"{app.driver.diagnose('invite-request-back-button')} error={app.error_text()!r}"
    )

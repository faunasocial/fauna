---
slug: join-a-nest
title: Join an existing nest
section: getting in
goal: docs/goal/behavior/onboarding.md § 3. Invite request
guide: docs/guides/getting-started.md § Step 3a — Join with an invite
absences:
  web (outcome 12): "docs/goal/behavior/account-age-band.md § App surface"
  linux (outcome 12): "docs/goal/behavior/account-age-band.md § App surface"
  windows (outcome 12): "docs/goal/behavior/account-age-band.md § App surface"
  macos (outcome 12): "docs/goal/behavior/account-age-band.md § App surface"
  tui (outcome 12): "docs/goal/behavior/account-age-band.md § App surface"
---

## What a user gets

Type the handle you want and the app finds the nest behind it and tells you
where you stand: already a member, welcome back; not yet, then you ask to join. An
invite code from the admin lets you straight in; without one, your request waits for
approval and the app takes you in by itself the moment it is granted. A refused
request shows you the reason and lets you ask again.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

1. [app] Typing your handle tells you whether that nest knows you and what happens next — `docs/goal/behavior/onboarding.md` § 2. Handle entry
   - `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_nest_user_unregistered_routes_to_invite`
   - `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_already_on_nest_handle_matches_enables_continue`
   - `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_probe_error_transient_shows_retry`
2. [app] You can ask to join and see your request pending — `docs/goal/behavior/onboarding.md` § 3. Invite request
   - `tests/e2e-unified/tests/test_invite_request_submit_roundtrip.py::test_invite_request_submit_reaches_pending_review`
3. [app] When the admin approves, the app takes you in with no further action on your part — `docs/goal/behavior/onboarding.md` § The pending-invite surface
   - `tests/e2e-unified/tests/test_pending_invite_journey.py::test_admin_approval_advances_the_requester_with_no_user_action`
4. [app] A refused request shows the reason and lets you ask again — `docs/goal/behavior/onboarding.md` § The pending-invite surface
   - `tests/e2e-unified/tests/test_pending_invite_journey.py::test_a_denied_requester_reads_the_reason_and_can_resubmit`
   - `tests/e2e-unified/tests/test_invite_request_states.py::test_denied_shows_reason_disables_continue`
5. [app] An invite code you were given admits you directly — `docs/goal/behavior/onboarding.md` § 3. Invite request
   - `tests/e2e-unified/tests/test_invite_request_states.py::test_oob_valid_enables_continue`
6. [app] A pending request survives quitting the app; the next launch picks it up where it was — `docs/goal/behavior/onboarding.md` § Long-term store contract
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_a_pending_invite_survives_force_quit`
   - `tests/e2e-unified/tests/web/test_pending_invite_persistence.py::test_force_quit_after_submit_relaunches_at_invite_request_pending`
7. [nest] Your nest records, approves and refuses join requests, and only a signed request counts — `docs/goal/architecture/nest/public-mode.md` § Registration & Identity
   - `tests/e2e-unified/tests/api/test_invite_requests.py::test_submit_happy_path`
   - `tests/e2e-unified/tests/api/test_invite_requests.py::test_submit_bad_signature`
   - `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_approve_creates_user`
   - `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_deny_marks_denied`
8. [app] Your first sign-in to a nest you joined offers the same one-tap trust a claiming admin gets — `docs/goal/behavior/onboarding.md` § 3b-ter. One-tap "trust this box" default grant
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_joining_by_invite_code_offers_the_one_tap_trust`
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_the_joiner_can_decline_and_still_reach_the_app`
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_the_joiner_can_accept_and_still_reach_the_app`
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_an_admitted_users_first_sign_in_offers_the_trust_and_a_later_one_does_not`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_add_account_offers_the_trust_to_an_admitted_identity`
9. [nest] A paid membership code admits you like an invite code, wherever the nest takes invite codes — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_paid_membership_code_admits_like_an_invite_code`
   - `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_paid_membership_code_is_refused_when_the_nest_takes_no_codes`
10. [nest] A request to join that a verified payment backs is approved without waiting for an admin — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_join_request_a_verified_payment_backs_is_approved_without_an_admin`
11. [nest] A sign-up whose app reports the person as a minor is refused and pointed to joining through a guardian — `docs/goal/architecture/nest/public-mode.md` § Registration Modes
    - `tests/e2e-unified/tests/api/test_registration_posture.py::test_a_sign_up_reported_as_a_minor_is_pointed_to_a_guardian`
12. [app] Before you send a join request or redeem a code, you are told what age range your app store will share with the nest's administrators, and whether it is shared as verified or as declared — `docs/goal/behavior/account-age-band.md` § App surface
    - `tests/e2e-unified/tests/test_invite_request_states.py::test_age_notice_is_declared_for_an_attestation_the_nest_did_not_list`
    - `tests/e2e-unified/tests/test_invite_request_states.py::test_age_notice_follows_the_minted_nonces_platform_list`
13. [nest] A handle on the nest's reserved list — admin, www, mail, relay and the like — is refused at sign-up and as a later handle change, and no admin setting frees it — `docs/goal/architecture/nest/public-mode.md` § Registration & Identity
   - `tests/e2e-unified/tests/api/test_registration_reserved_and_unadmitted.py::test_a_reserved_handle_is_refused_at_sign_up_and_as_a_handle_change`
14. [nest] Holding a key is never an account: a key the nest has not admitted is refused sign-in in every registration mode, open included, and an account comes only from registering with a handle, claiming the nest, or an admin admitting you — `docs/goal/architecture/nest/public-mode.md` § Registration & Identity
   - `tests/e2e-unified/tests/api/test_registration_reserved_and_unadmitted.py::test_a_key_the_nest_never_admitted_is_refused_sign_in_whatever_the_mode`
15. [app] While your request to join is pending, a nest the app cannot reach keeps it pending and the app keeps checking; a nest whose identity has changed ends the request with an error and no offer to trust the new identity — `docs/goal/behavior/onboarding.md` § The pending-invite surface
   - `tests/e2e-unified/tests/test_pending_invite_recheck_verdicts.py::test_a_pending_request_stays_pending_while_the_nest_is_unreachable`
   - `tests/e2e-unified/tests/test_pending_invite_recheck_verdicts.py::test_a_changed_nest_identity_ends_the_pending_request_with_no_trust_offer`
16. [nest] A verified age band comes only from a valid store attestation: a forged, replayed or wrong-app attestation refuses the whole admission and is never downgraded to a declared band — `docs/goal/behavior/account-age-band.md` § The account age band
   - `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_a_nonce_the_nest_never_minted_refuses_the_whole_admission`
   - `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_a_replayed_nonce_is_refused_because_the_first_attempt_spent_it`
   - `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_an_attestation_naming_a_bundle_the_nest_does_not_pin_is_refused`
   - `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_a_platform_the_nest_cannot_check_is_admitted_as_declared_not_verified`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+1c9a6a26 standalone |
| linux | ⚠ partial | 0.1.5-dev+cbd813be standalone |
| windows | ⚠ partial | 0.1.6-dev+89d36747 standalone |
| macos | ⚠ partial | 0.1.6-dev+e896a55d standalone |
| ios | ⚠ partial | 0.1.6-dev+e896a55d standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.6-dev+89d36747 standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_nest_user_unregistered_routes_to_invite` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_already_on_nest_handle_matches_enables_continue` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_probe_error_transient_shows_retry` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_invite_request_submit_roundtrip.py::test_invite_request_submit_reaches_pending_review` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_pending_invite_journey.py::test_admin_approval_advances_the_requester_with_no_user_action` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_pending_invite_journey.py::test_a_denied_requester_reads_the_reason_and_can_resubmit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_invite_request_states.py::test_denied_shows_reason_disables_continue` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_invite_request_states.py::test_oob_valid_enables_continue` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_a_pending_invite_survives_force_quit` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/web/test_pending_invite_persistence.py::test_force_quit_after_submit_relaunches_at_invite_request_pending` | web (linux): passed, linux (linux): failed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_submit_happy_path` | nest (linux): passed, nest (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_submit_bad_signature` | nest (linux): passed, nest (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_approve_creates_user` | nest (linux): passed, nest (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_deny_marks_denied` | nest (linux): passed, nest (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_joining_by_invite_code_offers_the_one_tap_trust` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_the_joiner_can_decline_and_still_reach_the_app` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_the_joiner_can_accept_and_still_reach_the_app` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_an_admitted_users_first_sign_in_offers_the_trust_and_a_later_one_does_not` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_add_account_offers_the_trust_to_an_admitted_identity` | web (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_paid_membership_code_admits_like_an_invite_code` | nest (linux): passed, nest (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_paid_membership_code_is_refused_when_the_nest_takes_no_codes` | nest (linux): passed, nest (macos): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_join_request_a_verified_payment_backs_is_approved_without_an_admin` | nest (linux): passed, nest (macos): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_registration_posture.py::test_a_sign_up_reported_as_a_minor_is_pointed_to_a_guardian` | nest (linux): passed, nest (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_invite_request_states.py::test_age_notice_is_declared_for_an_attestation_the_nest_did_not_list` | web (linux): skipped, linux (linux): skipped, macos (macos): skipped, ios (macos): passed, tui (linux): skipped, tui (macos): skipped |
| 12 | app | `tests/e2e-unified/tests/test_invite_request_states.py::test_age_notice_follows_the_minted_nonces_platform_list` — requires `machine-async-dispatch-bridge` (test harness) (not built: web, linux, windows, macos, tui) | web (linux): skipped, linux (linux): skipped, macos (macos): skipped, ios (macos): passed, tui (linux): skipped, tui (macos): skipped |
| 12 | app | absent by design on web, linux, windows, macos, tui | — |
| 13 | nest | `tests/e2e-unified/tests/api/test_registration_reserved_and_unadmitted.py::test_a_reserved_handle_is_refused_at_sign_up_and_as_a_handle_change` | nest (linux): passed, nest (macos): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_registration_reserved_and_unadmitted.py::test_a_key_the_nest_never_admitted_is_refused_sign_in_whatever_the_mode` | nest (linux): passed, nest (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_pending_invite_recheck_verdicts.py::test_a_pending_request_stays_pending_while_the_nest_is_unreachable` | tui (linux): passed, tui (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_pending_invite_recheck_verdicts.py::test_a_changed_nest_identity_ends_the_pending_request_with_no_trust_offer` | tui (linux): passed, tui (macos): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_a_nonce_the_nest_never_minted_refuses_the_whole_admission` | nest (linux): passed, nest (macos): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_a_replayed_nonce_is_refused_because_the_first_attempt_spent_it` | nest (linux): passed, nest (macos): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_an_attestation_naming_a_bundle_the_nest_does_not_pin_is_refused` | nest (linux): passed, nest (macos): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_age_band_attestation.py::test_a_platform_the_nest_cannot_check_is_admitted_as_declared_not_verified` | nest (linux): passed, nest (macos): passed |
<!-- features-render:end -->

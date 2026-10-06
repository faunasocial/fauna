---
slug: join-a-nest
title: Join an existing nest
section: getting in
goal: docs/goal/behavior/onboarding.md § 3. Invite request
guide: docs/guides/getting-started.md § Step 3a — Join with an invite
---

## What a user gets

Type the handle you want and the app finds the nest behind it and tells you
where you stand: already a member, welcome back; not yet, then you ask to join. An
invite code from the admin lets you straight in; without one, your request waits for
approval and the app takes you in by itself the moment it is granted. A refused
request shows you the reason and lets you ask again.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] Typing your handle tells you whether that nest knows you and what happens next — `docs/goal/behavior/onboarding.md` § 2. Handle entry
   - `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_nest_user_unregistered_routes_to_invite`
   - `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_already_on_nest_handle_matches_enables_continue`
   - `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_probe_error_transient_shows_retry`
2. [app] You can ask to join and see your request pending — `docs/goal/behavior/onboarding.md` § 3. Invite request
   - `tests/e2e-unified/tests/test_invite_request_submit_roundtrip.py::test_invite_request_submit_reaches_pending_review`
   - `tests/e2e-unified/tests/test_invite_request_states.py::test_pending_review_shows_recheck_and_disables_continue`
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
9. [nest] A paid membership code admits you like an invite code, wherever the nest takes invite codes — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
10. [nest] A request to join that a verified payment backs is approved without waiting for an admin — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
11. [nest] A sign-up whose app reports the person as a minor is refused and pointed to joining through a guardian — `docs/goal/architecture/nest/public-mode.md` § Registration Modes
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+4bc2efab standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_nest_user_unregistered_routes_to_invite` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_already_on_nest_handle_matches_enables_continue` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_handle_entry_outcomes.py::test_probe_error_transient_shows_retry` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_invite_request_submit_roundtrip.py::test_invite_request_submit_reaches_pending_review` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_invite_request_states.py::test_pending_review_shows_recheck_and_disables_continue` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_pending_invite_journey.py::test_admin_approval_advances_the_requester_with_no_user_action` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_pending_invite_journey.py::test_a_denied_requester_reads_the_reason_and_can_resubmit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_invite_request_states.py::test_denied_shows_reason_disables_continue` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_invite_request_states.py::test_oob_valid_enables_continue` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_a_pending_invite_survives_force_quit` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/web/test_pending_invite_persistence.py::test_force_quit_after_submit_relaunches_at_invite_request_pending` | web (linux): passed, linux (linux): failed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_submit_happy_path` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_submit_bad_signature` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_approve_creates_user` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_deny_marks_denied` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_joining_by_invite_code_offers_the_one_tap_trust` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_the_joiner_can_decline_and_still_reach_the_app` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_the_joiner_can_accept_and_still_reach_the_app` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | nest | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
<!-- features-render:end -->

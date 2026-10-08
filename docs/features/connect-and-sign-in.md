---
slug: connect-and-sign-in
title: Connect to your nest and sign in
section: getting in
goal: docs/goal/behavior/login.md § Goal
guide: docs/guides/getting-started.md § You're in
---

## What a user gets

With your identity on the device, opening the app signs you in and shows your
feed; there is nothing to type. The app remembers which nest is yours and checks it
is really that nest every time. A nest you have just set up opens connected straight
away, without waiting for its web address to spread across the internet. If the nest
cannot be reached you get a retry, not a dead end, and if the nest is replaced
underneath you (an upgrade, a restart) the app reconnects and catches up on its own.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] With a saved identity, launching the app signs you in and lands you on your feed — `docs/goal/behavior/onboarding.md` § App-launch routing
   - `tests/e2e-unified/tests/test_onboarding.py::test_login_and_see_feed`
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_b_registered_identity_relaunches_to_main_app`
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_k_real_onboarding_completion_reaches_the_main_app`
2. [app] A nest you cannot reach shows a retry and a way to pick a different nest, never a dead end — `docs/goal/behavior/onboarding.md` § App-launch routing
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_c_unreachable_refused_shows_retry_surface`
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_d_unreachable_dns_fail_shows_retry_surface`
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_f_retry_surface_fallthrough_goes_to_handle_entry`
3. [app] The app remembers your nest's identity and warns you, before signing in, if it has changed — `docs/goal/architecture/security.md` § Transport trust
   - `tests/e2e-unified/tests/test_nest_identity_pin.py::test_changed_nest_identity_warns_then_recovers`
   - `tests/e2e-unified/tests/test_nest_identity_pin_post_auth.py::test_post_auth_identity_change_warns_then_recovers`
   - `tests/e2e-unified/tests/test_web_bearer_remint_identity_pin.py::test_post_auth_identity_change_bearer_remint_channel`
4. [app] A home nest with no public certificate still works: the app reaches it and signs you in — `docs/goal/architecture/nest/tls-certificates.md` § A. The self-signed floor
   - `tests/e2e-unified/tests/test_onboarding_self_signed_probe.py::test_onboarding_handle_check_tolerates_self_signed_local_nest`
   - `tests/e2e-unified/tests/platform/docker/test_firewalled_trust_differential.py::test_native_tofu_client_reaches_connected_and_admin_on_self_signed_floor`
   - `tests/e2e-unified/tests/test_self_signed_nest_client_legs.py::test_blob_legs_round_trip_against_a_self_signed_nest`
5. [app] You stay signed in across a restart of the app — `docs/goal/behavior/onboarding.md` § Long-term store contract
   - `tests/e2e-unified/tests/test_signed_in_relaunch_stays_ready.py::test_a_signed_in_app_relaunches_reset_ready`
   - `tests/e2e-unified/tests/test_onboarding_logged_in_terminal_web.py::test_web_logged_in_terminal_survives_a_page_reload`
6. [app] If your nest is restarted or upgraded under you, the app reconnects and your feed catches up by itself — `docs/goal/architecture/apps/common.md` § Nest Connection
   - `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_resilience`
   - `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_feed_rehydrate`
   - `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_page_machines_open_no_socket_of_their_own`
   - `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_a_folders_gesture_lands_the_moment_the_app_reads_online_after_a_flip`
7. [nest] Your nest signs you in from your key alone; there is no password anywhere — `docs/goal/behavior/login.md` § Two auth endpoints
   - `tests/e2e-unified/tests/test_api_helpers.py::test_auth_token`
   - `tests/e2e-unified/tests/api/test_onboarding.py::test_alice_self_service_onboarding`
8. [app] A nest you have just set up opens connected straight away, without waiting for its web address to spread across the internet — `docs/goal/behavior/onboarding.md` § Reach hint
   - `tests/e2e-unified/tests/test_reach_hint_dial_journey.py::test_the_hint_opens_a_connected_app_and_the_domains_first_answer_drops_it`
9. [app] A device whose clock is wrong still signs you in when the app opens — `docs/goal/behavior/login.md` § Goal
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_l_wrong_clock_still_signs_in`
10. [app] A device whose clock is wrong stays signed in: the app renews its session on its own schedule and the renewal succeeds — `docs/goal/behavior/login.md` § Token lifetime on the client's clock
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_m_wrong_clock_ahead_stays_signed_in`
11. [app] If a nest already holds your account but no longer lets you in, asking it for an invite says so plainly and never tells you to try again — `docs/goal/behavior/onboarding.md` § 3. Invite request (`invite_request`)
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_n_suspended_user_fallthrough_invite_submit_is_terminal`
12. [app] If your nest's identity history shows two competing successors, the app warns you, names both, and offers no way to trust either — `docs/goal/architecture/nest/box-recovery.md` § Client acceptance — re-pin on a verified chain
    - (none)
13. [app] If your nest's admin suspends your account while you are signed in, the app says the nest refused you without being restarted, and Retry lets you back in once the admin restores you — `docs/goal/behavior/onboarding.md` § App-launch routing
    - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_e2_suspended_while_signed_in_lands_refused_surface_without_relaunch`
14. [app] An app that already knows your nest's identity still checks it when the nest presents a publicly trusted certificate, so another server at the same address cannot pass as your nest — `docs/goal/architecture/nest/tls-certificates.md` § B-IP. The IP bridge cert
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_onboarding.py::test_login_and_see_feed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_b_registered_identity_relaunches_to_main_app` | web (linux): passed, linux (linux): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_k_real_onboarding_completion_reaches_the_main_app` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_c_unreachable_refused_shows_retry_surface` | web (linux): passed, linux (linux): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_d_unreachable_dns_fail_shows_retry_surface` | web (linux): passed, linux (linux): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_f_retry_surface_fallthrough_goes_to_handle_entry` | web (linux): passed, linux (linux): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_identity_pin.py::test_changed_nest_identity_warns_then_recovers` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_identity_pin_post_auth.py::test_post_auth_identity_change_warns_then_recovers` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_web_bearer_remint_identity_pin.py::test_post_auth_identity_change_bearer_remint_channel` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_onboarding_self_signed_probe.py::test_onboarding_handle_check_tolerates_self_signed_local_nest` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/platform/docker/test_firewalled_trust_differential.py::test_native_tofu_client_reaches_connected_and_admin_on_self_signed_floor` | — |
| 4 | app | `tests/e2e-unified/tests/test_self_signed_nest_client_legs.py::test_blob_legs_round_trip_against_a_self_signed_nest` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_signed_in_relaunch_stays_ready.py::test_a_signed_in_app_relaunches_reset_ready` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_onboarding_logged_in_terminal_web.py::test_web_logged_in_terminal_survives_a_page_reload` | web (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_resilience` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_feed_rehydrate` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_page_machines_open_no_socket_of_their_own` | web (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_a_folders_gesture_lands_the_moment_the_app_reads_online_after_a_flip` | web (linux): passed, linux (linux): passed, tui (linux): failed |
| 7 | nest | `tests/e2e-unified/tests/test_api_helpers.py::test_auth_token` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_onboarding.py::test_alice_self_service_onboarding` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_reach_hint_dial_journey.py::test_the_hint_opens_a_connected_app_and_the_domains_first_answer_drops_it` | web (linux): failed, linux (linux): skipped, windows (windows): failed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_l_wrong_clock_still_signs_in` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_m_wrong_clock_ahead_stays_signed_in` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_n_suspended_user_fallthrough_invite_submit_is_terminal` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 12 | app | (none) | — |
| 13 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_e2_suspended_while_signed_in_lands_refused_surface_without_relaunch` | web (linux): passed, linux (linux): passed, windows (windows): skipped |
| 14 | app | (none) | — |
<!-- features-render:end -->

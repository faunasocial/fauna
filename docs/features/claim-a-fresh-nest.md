---
slug: claim-a-fresh-nest
title: Claim a fresh nest
section: getting in
goal: docs/goal/behavior/onboarding.md § 3a. Claim code
guide: docs/guides/getting-started.md § Step 3b — Claim a new nest
---

## What a user gets

A brand-new nest belongs to the first person who claims it with the one-time
code it printed when it started. Paste the code in the app and you are its owner and
admin: the app asks whether the nest lives on the open internet or behind your home
router, offers you one tap to let the nest hold sealed copies of your data, and if
your handle carries a real domain, turns mail on for you without another question.

## Coverage contract

Stamped 2026-09-23 at 207d96534d.

1. [app] A nest nobody has claimed yet sends you to the claim-code step, and a wrong code says so in place — `docs/goal/behavior/onboarding.md` § 3a. Claim code
   - `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_unclaimed_nest_routes_to_claim_code`
   - `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_claim_code_invalid_renders_status_not_error_message`
   - `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_claim_code_uri_paste_reaches_the_machine_intact`
   - `tests/e2e-unified/tests/test_silent_challenge_unclaimed_nest.py::test_launch_flow_navigate_to_claim_code_renders_page`
   - `tests/e2e-unified/tests/test_onboarding_localhost.py::test_test_at_localhost_reaches_admin_claim`
   - `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_same_root_preserves_rotation_seq`
   - `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_differing_root_overwrites`
2. [app] Right after the claim you choose whether the nest is public or behind your home router — `docs/goal/behavior/onboarding.md` § 3b-bis. NAT mode choice
   - `tests/e2e-unified/tests/test_nat_mode_choice.py::test_nat_mode_choosing_public_seed`
   - `tests/e2e-unified/tests/test_nat_mode_choice.py::test_nat_mode_selecting_private_updates_selection`
   - `tests/e2e-unified/tests/test_nat_mode_choice.py::test_nat_mode_private_ward_preselection`
3. [app] The claim ends with a one-tap offer to trust this nest with sealed copies of your data, and either answer takes you into the app — `docs/goal/behavior/onboarding.md` § 3b-ter. One-tap "trust this box" default grant
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_the_claim_offers_the_one_tap_trust_before_the_app`
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_declining_the_offer_leaves_everything_as_today`
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_accepting_the_offer_also_reaches_the_app`
   - `tests/e2e-unified/tests/test_trust_prompt.py::test_the_manual_dns_resume_offers_the_trust_and_survives_a_crash_on_it`
4. [app] Claiming with a handle on a real domain turns mail on for you; a local handle leaves it off until you ask — `docs/goal/behavior/onboarding.md` § 3b. Serving enablement at claim
   - `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_admin_claim_with_real_domain_handle_auto_enables_mail`
   - `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_admin_claim_with_loopback_handle_derives_mail_disabled`
   - `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_a_returning_admin_sign_in_issues_no_deployment_enable`
5. [nest] The claim code works once; a second person cannot claim a claimed nest, even if the code lingers — `docs/goal/architecture/nest/public-mode.md` § First Admin Bootstrap
   - `tests/e2e-unified/tests/api/test_admin_auth.py::test_claim_admin`
   - `tests/e2e-unified/tests/platform/docker/test_cloud_init_claim_code_mount.py::test_mounted_claim_code_box_boots_and_is_claimable`
   - `tests/e2e-unified/tests/platform/docker/test_cloud_init_claim_code_mount.py::test_lingering_claim_code_cannot_create_second_admin`
   - `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_nest_claim_admin`
6. [nest] The claim works over the released image's own wire, and the set-up status never leaks the code — `docs/goal/behavior/onboarding.md` § 3a. Claim code
   - `tests/e2e-unified/tests/platform/docker/test_docker_api_claim.py::test_docker_api_provisioning`
7. [app] Finishing the claim always puts you in the app — it never closes on you at the last step — `docs/goal/behavior/onboarding.md` § Wizard exit handling
   - `tests/e2e-unified/tests/test_onboarding_logged_in_terminal_empty_store.py::test_logged_in_terminal_survives_an_empty_secret_slot`
8. [app] The claim step takes the claim link your nest printed as readily as the bare code — `docs/goal/behavior/onboarding.md` § 3a. Claim code
   - `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_claim_code_uri_paste_reaches_the_machine_intact`
9. [app] A claim made with the link holds the nest to the identity the link names, and a damaged link is refused rather than claimed unprotected — `docs/goal/behavior/onboarding.md` § 3a. Claim code
   - `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_same_root_preserves_rotation_seq`
   - `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_differing_root_overwrites`
   - `tests/e2e-unified/tests/test_claim_link_nest_pin.py::test_a_claim_link_naming_another_nest_is_refused`
   - `tests/e2e-unified/tests/test_claim_link_nest_pin.py::test_a_claim_link_naming_this_nest_claims_it_and_pins_that_identity`
10. [app] Claiming with a handle on a real domain also turns on calendar, contacts and file access for standard apps — `docs/goal/behavior/onboarding.md` § 3b. Serving enablement at claim
   - `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_admin_claim_with_real_domain_handle_auto_enables_mail`
11. [app] A nest you mark as behind your home router starts with mail and those three services off — `docs/goal/behavior/onboarding.md` § 3b. Serving enablement at claim
   - `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_marking_the_box_private_at_claim_leaves_all_four_serving_off`
12. [nest] The person who claims a nest becomes its admin — `docs/goal/architecture/nest/public-mode.md` § First Admin Bootstrap
    - `tests/e2e-unified/tests/api/test_admin_auth.py::test_claim_admin`
    - `tests/e2e-unified/tests/platform/docker/test_cloud_init_claim_code_mount.py::test_mounted_claim_code_box_boots_and_is_claimable`
    - `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_nest_claim_admin`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| linux | ✅ full | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| macos | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| ios | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_unclaimed_nest_routes_to_claim_code` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_claim_code_invalid_renders_status_not_error_message` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_claim_code_uri_paste_reaches_the_machine_intact` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_silent_challenge_unclaimed_nest.py::test_launch_flow_navigate_to_claim_code_renders_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_onboarding_localhost.py::test_test_at_localhost_reaches_admin_claim` | web (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_same_root_preserves_rotation_seq` | web (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_differing_root_overwrites` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_nat_mode_choice.py::test_nat_mode_choosing_public_seed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nat_mode_choice.py::test_nat_mode_selecting_private_updates_selection` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nat_mode_choice.py::test_nat_mode_private_ward_preselection` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_the_claim_offers_the_one_tap_trust_before_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_declining_the_offer_leaves_everything_as_today` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_accepting_the_offer_also_reaches_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_trust_prompt.py::test_the_manual_dns_resume_offers_the_trust_and_survives_a_crash_on_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_admin_claim_with_real_domain_handle_auto_enables_mail` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_admin_claim_with_loopback_handle_derives_mail_disabled` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_a_returning_admin_sign_in_issues_no_deployment_enable` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_admin_auth.py::test_claim_admin` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_cloud_init_claim_code_mount.py::test_mounted_claim_code_box_boots_and_is_claimable` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_cloud_init_claim_code_mount.py::test_lingering_claim_code_cannot_create_second_admin` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_nest_claim_admin` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_docker_api_claim.py::test_docker_api_provisioning` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_onboarding_logged_in_terminal_empty_store.py::test_logged_in_terminal_survives_an_empty_secret_slot` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_claim_code_unclaimed_nest.py::test_claim_code_uri_paste_reaches_the_machine_intact` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_same_root_preserves_rotation_seq` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py::test_wasm_claim_pin_differing_root_overwrites` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_claim_link_nest_pin.py::test_a_claim_link_naming_another_nest_is_refused` | web (linux): skipped, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_claim_link_nest_pin.py::test_a_claim_link_naming_this_nest_claims_it_and_pins_that_identity` | web (linux): skipped, linux (linux): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_admin_claim_with_real_domain_handle_auto_enables_mail` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py::test_marking_the_box_private_at_claim_leaves_all_four_serving_off` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_admin_auth.py::test_claim_admin` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/platform/docker/test_cloud_init_claim_code_mount.py::test_mounted_claim_code_box_boots_and_is_claimable` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_nest_claim_admin` | nest (linux): passed |
<!-- features-render:end -->

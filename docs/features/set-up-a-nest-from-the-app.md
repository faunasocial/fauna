---
slug: set-up-a-nest-from-the-app
title: Set up a nest in the cloud from the app
section: getting in
goal: docs/goal/behavior/onboarding-provisioning.md § 5. VPS configuration
guide: docs/guides/nest-app-setup.md § What you're building
---

## What a user gets

You do not need to know how to run a server. Pick a domain provider and a
hosting provider, paste the keys they give you, choose a server, and watch the app
buy the domain, rent the machine, install the nest and point your domain at it. You
see every step, the price before you commit, and you can cancel or retry. If you
would rather set DNS up yourself, the app shows you exactly which records to add and
waits for them.

## Coverage contract

Stamped 2026-09-26 at d33b1481dd.

1. [app] You pick a domain provider, enter its credentials, and can buy a new domain or use one you own — `docs/goal/behavior/onboarding.md` § 4. DNS configuration
   - `tests/e2e-unified/tests/test_dns_config.py::test_dns_config_select_provider_shows_credentials_form`
   - `tests/e2e-unified/tests/test_dns_config.py::test_dns_config_buy_domain_filters_providers`
   - `tests/e2e-unified/tests/test_provider_registry_parity.py::test_dns_provider_fields_visible`
   - `tests/e2e-unified/tests/test_provider_registry_parity.py::test_registrar_provider_fields_visible`
2. [app] You pick a hosting provider and a server size; a mail-capable server is kept to sizes that can run mail — `docs/goal/behavior/onboarding.md` § 5. VPS configuration
   - `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_select_provider_shows_credentials_form`
   - `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_server_types_after_verify`
   - `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_mail_mode_toggle_gates_server_types`
   - `tests/e2e-unified/tests/test_provider_registry_parity.py::test_vps_provider_fields_visible`
3. [app] One bundled provider can be registrar, DNS and hosting in a single step — `docs/goal/architecture/provisioning/registry.md` § Bundled provider
   - `tests/e2e-unified/tests/test_bundled_provider.py::test_bundled_provider_buys_domain_and_server_through_one_row`
4. [app] You watch the build step by step, see the price, and can cancel and retry — `docs/goal/behavior/onboarding.md` § 6. Nest provisioning
   - `tests/e2e-unified/tests/test_provisioning_progress.py::test_failed_renders_error_and_retry_button`
   - `tests/e2e-unified/tests/test_provisioning_progress.py::test_cancel_mid_online_then_retry_succeeds`
   - `tests/e2e-unified/tests/test_provisioning_progress.py::test_price_bom_shows_both_lines_when_buying_domain`
   - `tests/e2e-unified/tests/test_provisioning_progress.py::test_continue_on_a_claimed_succeeded_run_lands_on_nat_mode_choice`
   - `tests/e2e-unified/tests/test_provisioning_claims_a_real_nest.py::test_a_standard_path_run_claims_the_real_box_and_continues_to_nat_mode`
   - `tests/e2e-unified/tests/web/test_provisioning_continue_to_nat_mode.py::test_provisioning_continue_lands_on_nat_mode_choice`
5. [app] A real server is rented, set up and passes the same checks a production nest passes — `docs/goal/architecture/installers/vps.md` § What Happens Behind the Scenes
   - (maintainer-only)
6. [app] If you set DNS up yourself, the app shows the records to add and keeps checking until they are live, across a restart — `docs/goal/behavior/onboarding.md` § "Almost ready" surface
   - `tests/e2e-unified/tests/test_dns_post_instructions.py::test_dns_post_instructions_shows_seeded_records`
   - `tests/e2e-unified/tests/test_awaiting_manual_dns.py::test_awaiting_manual_dns_surface_renders`
   - `tests/e2e-unified/tests/test_awaiting_manual_dns.py::test_awaiting_manual_dns_recheck_and_copy_actionable`
   - `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_i_deferred_dns_relaunches_to_the_almost_ready_surface`
   - `tests/e2e-unified/tests/web/test_awaiting_dns_poll_error_guard.py::test_recheck_against_unreachable_nest_logs_no_pageerror`
7. [app] If setup is interrupted — you quit, the app crashes, or the server order fails — reopening the app brings you back to it and you can finish, with no code to type — `docs/goal/behavior/onboarding.md` § 6. Nest provisioning
   - `tests/e2e-unified/tests/test_provisioning_slot_recovery.py::test_a_crash_during_online_resumes_on_the_records_less_surface`
   - `tests/e2e-unified/tests/test_provisioning_slot_recovery.py::test_create_server_failing_after_the_slot_write_still_resumes`
8. [app] The domain provider credentials you verified are kept, sealed so your nest cannot read them, and the app can manage your DNS with them afterwards — `docs/goal/behavior/onboarding-provisioning.md` § 4. DNS configuration
   - `tests/e2e-unified/tests/test_onboarding_dns_glue_app.py::test_the_verified_dns_credential_is_sealed_by_the_launched_app`
9. [app] If the server you ordered is never going to answer, the waiting screen gives you a way out — "Use a different nest" — and reopening the app afterwards takes you to the handle page, not back to the waiting screen — `docs/goal/behavior/onboarding-provisioning.md` § "Almost ready" surface (post-provisioning DNS-pending)
   - `tests/e2e-unified/tests/test_awaiting_manual_dns.py::test_awaiting_manual_dns_fallthrough_lands_on_handle_entry`
   - `tests/e2e-unified/tests/test_provisioning_slot_recovery.py::test_the_exit_retires_the_slot_so_a_relaunch_no_longer_resumes_the_box`
10. [app] You choose which updates the new server follows — stable releases, test builds or development builds — `docs/goal/behavior/onboarding-provisioning.md` § 5. VPS configuration
   - `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_update_channel_rows_select_one_of_three`

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
| 1 | app | `tests/e2e-unified/tests/test_dns_config.py::test_dns_config_select_provider_shows_credentials_form` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_dns_config.py::test_dns_config_buy_domain_filters_providers` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_provider_registry_parity.py::test_dns_provider_fields_visible` | linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_provider_registry_parity.py::test_registrar_provider_fields_visible` | linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_select_provider_shows_credentials_form` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_server_types_after_verify` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): failed, ios (macos): failed, tui (linux): skipped |
| 2 | app | `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_mail_mode_toggle_gates_server_types` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_provider_registry_parity.py::test_vps_provider_fields_visible` | linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_bundled_provider.py::test_bundled_provider_buys_domain_and_server_through_one_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_provisioning_progress.py::test_failed_renders_error_and_retry_button` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_provisioning_progress.py::test_cancel_mid_online_then_retry_succeeds` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_provisioning_progress.py::test_price_bom_shows_both_lines_when_buying_domain` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_provisioning_progress.py::test_continue_on_a_claimed_succeeded_run_lands_on_nat_mode_choice` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_provisioning_claims_a_real_nest.py::test_a_standard_path_run_claims_the_real_box_and_continues_to_nat_mode` | web (linux): passed, linux (linux): passed, windows (windows): failed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/web/test_provisioning_continue_to_nat_mode.py::test_provisioning_continue_lands_on_nat_mode_choice` | web (linux): passed, linux (linux): failed |
| 5 | app | (maintainer-only) | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_dns_post_instructions.py::test_dns_post_instructions_shows_seeded_records` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_awaiting_manual_dns.py::test_awaiting_manual_dns_surface_renders` | web (linux): passed, linux (linux): skipped, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_awaiting_manual_dns.py::test_awaiting_manual_dns_recheck_and_copy_actionable` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py::test_smoke_i_deferred_dns_relaunches_to_the_almost_ready_surface` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/web/test_awaiting_dns_poll_error_guard.py::test_recheck_against_unreachable_nest_logs_no_pageerror` | web (linux): passed, linux (linux): failed |
| 7 | app | `tests/e2e-unified/tests/test_provisioning_slot_recovery.py::test_a_crash_during_online_resumes_on_the_records_less_surface` | web (linux): passed, linux (linux): passed, windows (windows): skipped, tui (linux): passed, tui (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_provisioning_slot_recovery.py::test_create_server_failing_after_the_slot_write_still_resumes` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_onboarding_dns_glue_app.py::test_the_verified_dns_credential_is_sealed_by_the_launched_app` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_awaiting_manual_dns.py::test_awaiting_manual_dns_fallthrough_lands_on_handle_entry` | linux (linux): skipped, windows (windows): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_provisioning_slot_recovery.py::test_the_exit_retires_the_slot_so_a_relaunch_no_longer_resumes_the_box` | linux (linux): skipped, windows (windows): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_vps_config.py::test_vps_config_update_channel_rows_select_one_of_three` | linux (linux): skipped, tui (linux): passed |
<!-- features-render:end -->

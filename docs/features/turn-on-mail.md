---
slug: turn-on-mail
title: Turn on mail for your account
section: mail, calendar and contacts
goal: docs/goal/ui/mail-settings.md § Goal
guide: docs/guides/own-your-mail.md § Turning mail on
---

## What a user gets

One switch gives you a mailbox at your own address. The page then shows the
settings a regular mail app needs, lists the app passwords you have made, shows
each one again on request, lets you add and revoke them, rotate the keys behind
them, and turn mail off again. A new member gets mail from their first sign-in
when the admin allows it, and turning mail on in the app also starts the mail
server on the nest.

## Coverage contract

Stamped 2026-09-23 at 039e9619ca.

1. [app] Turning mail on shows the mail-app settings and your first app password as a row — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_page_reachable`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_enable_populates_mua_instructions`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_enable_renders_credential_row`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_top_elements_present`
2. [app] An app password shows again on request, and you add and revoke them — `docs/goal/ui/mail-settings.md` § User actions
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_reveal_credential_secret`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_then_revoke_credential`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_plain_credential_manual_stores_typed_secret`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_plain_credential_autogen_stores_shown_secret`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_oauthbearer_credential_shows_token_once`
3. [app] Rotating your mail keys completes and every surviving password keeps working — `docs/goal/behavior/mail-credentials.md` § Rotation and recovery
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_rotate_keys_completes`
   - `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_keys_info_present`
4. [app] Turning mail off asks you to confirm and revokes every app password — `docs/goal/ui/mail-settings.md` § User actions
   - `tests/e2e-unified/tests/test_mail_disable.py::test_disable_mail_revokes_all_credentials_via_confirm_dialog`
5. [app] A new member gets a mailbox at first sign-in when the admin allows it, with nothing to do — `docs/goal/behavior/mail-credentials.md` § Auto-enable for new users
   - `tests/e2e-unified/tests/test_mail_auto_enable_first_setup.py::test_non_admin_first_setup_auto_mints_and_imap_logs_in`
6. [app] Turning mail on from the app starts the mail server on a nest running the released image — `docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first claim
   - `tests/e2e-unified/tests/platform/docker/test_mail_client_ui_enable_docker.py::test_linux_ui_enable_mail_boots_docker_bridge`
7. [app] Whether this nest serves your mailbox to mail apps is your switch, on by default — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_serving_toggle.py::test_serve_here_toggle_defaults_on_and_writes_through_to_nest`
8. [app] With only calendar on, the page shows the calendar settings and the shared password controls, and no mail rows — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_settings_caldav_only.py::test_caldav_only_mail_settings_render_gate`
9. [app] The page shows at a glance whether your mail is off, up to date, syncing, or partway through a key rotation, and never says "up to date" while mail is off — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_status_line_reads_off_then_up_to_date_and_syncing_while_a_change_runs`
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_an_interrupted_rotation_is_shown_and_one_tap_finishes_it`
10. [app] Each app password is listed with its name, its kind, when you made it, and the exact login for a mail app, ready to copy — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_credential_row_shows_name_kind_created_and_a_copyable_login`
11. [app] If a key rotation was interrupted, the page tells you and one tap finishes it; you cannot start a second rotation meanwhile — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_an_interrupted_rotation_is_shown_and_one_tap_finishes_it`
12. [app] When rotating keys you can mark passwords you think are compromised, and those stop working — `docs/goal/behavior/mail-credentials.md` § Hard revoke (suspected compromise)
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_a_password_marked_compromised_at_rotation_stops_working`
13. [app] Revoking one app password cuts off only the mail apps using it; your other app passwords keep working — `docs/goal/behavior/mail-credentials.md` § Soft revoke (retire a credential)
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_revoking_one_password_leaves_the_others_working`
14. [app] If turning mail on is interrupted, turning it on again finishes the job — `docs/goal/behavior/mail-credentials.md` § Partial-state-during-minting (first-enable)
   - `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_mail_enable_box_recovers`
15. [app] While mail is only partly on, the page says so instead of pretending it is off — `docs/goal/behavior/mail-credentials.md` § Partial-state-during-minting (first-enable)
   - (none)
16. [app] A brief connection drop while turning mail on does not undo it; the app retries and finishes — `docs/goal/behavior/mail-credentials.md` § Partial-state-during-minting (first-enable)
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_a_brief_drop_while_turning_mail_on_is_retried_and_finishes`
17. [app] Turning email off while your calendar or contacts are on keeps those apps signed in with the same password — `docs/goal/ui/mail-settings.md` § User actions
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_turning_email_off_keeps_the_calendar_signed_in`
18. [app] Turning your own mail off never turns mail off for anyone else on the nest — `docs/goal/ui/mail-settings.md` § User actions
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_turning_your_own_mail_off_leaves_another_users_mail_working`
19. [app] With serving turned off on a nest, mail and calendar apps can no longer open your mailbox there, while your own app still shows your mail — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_serving_off_closes_mail_apps_but_the_app_still_shows_mail`
20. [app] A new app password is generated strong for you by default; if you type your own, you are warned it limits your mail's protection and shown how strong it is — `docs/goal/ui/mail-settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_new_password_is_generated_by_default_and_a_typed_one_is_warned`
21. [app] Your mail setup and app passwords appear on each of your devices without setting mail up again — `docs/goal/behavior/mail-credentials.md` § Goal
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_mail_setup_and_passwords_follow_you_to_a_second_device`
22. [app] You can separately stop receiving or stop sending mail for your account; both are on by default — `docs/goal/behavior/mail-policy-config.md` § Tier 3 — per-account (user)
   - (none)
23. [app] The page shows how much of your mailbox you have used, and you are told in the app when it is full — `docs/goal/behavior/imap-server.md` § Composition with submission quota
   - (none)
24. [app] If the nest cannot be reached or refuses a change, the page says why and you can try again — `docs/goal/ui/mail-settings.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_mail_settings_controls.py::test_a_refused_change_says_why_and_can_be_tried_again`
25. [app] A mail app keeps sending month after month without you re-entering anything — `docs/goal/behavior/mail-credentials.md` § Architectural rules
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
| 1 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_page_reachable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_enable_populates_mua_instructions` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_enable_renders_credential_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_top_elements_present` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_reveal_credential_secret` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_then_revoke_credential` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_plain_credential_manual_stores_typed_secret` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_plain_credential_autogen_stores_shown_secret` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_add_oauthbearer_credential_shows_token_once` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): failed |
| 3 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_rotate_keys_completes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_credentials.py::test_mail_settings_keys_info_present` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_disable.py::test_disable_mail_revokes_all_credentials_via_confirm_dialog` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_mail_auto_enable_first_setup.py::test_non_admin_first_setup_auto_mints_and_imap_logs_in` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/platform/docker/test_mail_client_ui_enable_docker.py::test_linux_ui_enable_mail_boots_docker_bridge` | — |
| 7 | app | `tests/e2e-unified/tests/test_mail_serving_toggle.py::test_serve_here_toggle_defaults_on_and_writes_through_to_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 8 | app | `tests/e2e-unified/tests/test_mail_settings_caldav_only.py::test_caldav_only_mail_settings_render_gate` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_status_line_reads_off_then_up_to_date_and_syncing_while_a_change_runs` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_an_interrupted_rotation_is_shown_and_one_tap_finishes_it` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_credential_row_shows_name_kind_created_and_a_copyable_login` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_an_interrupted_rotation_is_shown_and_one_tap_finishes_it` | tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_a_password_marked_compromised_at_rotation_stops_working` | tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_revoking_one_password_leaves_the_others_working` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_crash_recovery_journeys.py::test_kill_client_mid_mail_enable_box_recovers` | linux (linux): passed, tui (linux): passed |
| 15 | app | (none) | — |
| 16 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_a_brief_drop_while_turning_mail_on_is_retried_and_finishes` | tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_turning_email_off_keeps_the_calendar_signed_in` | tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_turning_your_own_mail_off_leaves_another_users_mail_working` | tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_serving_off_closes_mail_apps_but_the_app_still_shows_mail` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_new_password_is_generated_by_default_and_a_typed_one_is_warned` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_mail_setup_and_passwords_follow_you_to_a_second_device` | tui (linux): passed |
| 22 | app | (none) | — |
| 23 | app | (none) | — |
| 24 | app | `tests/e2e-unified/tests/test_mail_settings_controls.py::test_a_refused_change_says_why_and_can_be_tried_again` | tui (linux): passed |
| 25 | app | (none) | — |
<!-- features-render:end -->

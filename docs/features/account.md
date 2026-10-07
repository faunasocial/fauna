---
slug: account
title: Your account: handle, sign out, delete
section: your data and devices
goal: docs/goal/ui/settings.md § User actions
guide: docs/guides/app-tour.md § Settings
---

## What a user gets

Change your handle, see and copy your identity, sign out, or delete the
account. Changes that matter take effect after a delay and stay cancellable in the
meantime, in a section that shows everything pending. Signing out wipes every
credential from the device.

## Coverage contract

Stamped 2026-09-19 at acb58fddf3.

1. [app] A handle that breaks the rules is refused with a message and not applied — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_settings.py::test_handle_change_validation`
2. [app] Your identity is shown on the Status page with copy buttons — `docs/goal/ui/status.md` § Layout & flow
   - `tests/e2e-unified/tests/test_settings.py::test_actor_id_visible`
   - `tests/e2e-unified/tests/test_settings.py::test_copy_buttons_visible`
3. [app] Signing out returns to sign-up and leaves no credential on the device — `docs/goal/architecture/long-term-store.md` § Cleanup contract
   - `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_returns_to_onboarding`
   - `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_erases_the_credential_namespace`
   - `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_erases_the_account_scoped_state_directory`
   - `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_erases_the_account_registry`
   - `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_retires_the_enrollment_and_erases_the_account_store`
   - `tests/e2e-unified/tests/test_sign_out_web.py::test_web_load_after_a_tab_closed_mid_sign_out_finishes_the_sign_out`
   - `tests/e2e-unified/tests/test_bearer_cache_web.py::test_web_sign_out_then_new_identity_posts_as_itself`
4. [app] Deleting the account asks you to type a confirmation, then schedules the deletion — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_delete_account.py::test_delete_account_via_confirm_field`
5. [app] A scheduled handle change or account deletion shows as pending and can be cancelled — `docs/goal/ui/settings.md` § Pending actions
   - `tests/e2e-unified/tests/test_pending_actions.py::test_a_scheduled_handle_change_is_visible_and_cancellable`
   - `tests/e2e-unified/tests/test_pending_actions.py::test_a_queued_account_delete_is_visible_and_cancellable`
   - `tests/e2e-unified/tests/test_pending_actions.py::test_an_admins_deletion_of_this_account_is_listed_and_cancellable`
6. [nest] Your nest schedules handle changes and deletions with a delay and lets you cancel them — `docs/goal/architecture/nest/common.md` § Pending Actions System
   - `tests/e2e-unified/tests/api/test_onboarding.py::test_alice_self_service_onboarding`
7. [app] If signing out cannot remove some of your data or your sign-in credentials from this device, the app says so instead of reporting a clean sign-out — `docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy (iron-clad for new app state)
   - `tests/e2e-unified/tests/test_sign_out.py::test_a_sign_out_that_cannot_erase_everything_says_so`
   - `tests/e2e-unified/tests/test_sign_out.py::test_a_sign_out_whose_credentials_cannot_be_erased_says_so`
   - `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_that_cannot_erase_a_store_says_so_and_a_later_load_finishes`
8. [app] A handle that someone else already holds is refused with your nest's reason and not applied — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_settings.py::test_handle_someone_else_holds_is_refused_and_not_applied`
9. [app] Signing out asks you to confirm before anything is cleared — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_returns_to_onboarding`
10. [app] Confirming the account's deletion leaves you signed in with nothing erased until the scheduled deletion runs — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_delete_account.py::test_delete_account_via_confirm_field`
11. [app] A handle change you scheduled is not shown as your handle until it takes effect — `docs/goal/ui/settings.md` § Pending actions
   - `tests/e2e-unified/tests/test_pending_actions.py::test_a_scheduled_handle_change_is_not_shown_as_your_handle`
12. [app] An account your nest stops accepting does not erase what is on this device; only your own sign-out does that — `docs/goal/ui/settings.md` § Where logic lives
   - `tests/e2e-unified/tests/test_delete_account.py::test_tui_an_account_the_nest_stops_accepting_erases_nothing_on_the_device`
   - `tests/e2e-unified/tests/test_delete_account.py::test_linux_an_account_the_nest_stops_accepting_erases_nothing_on_the_device`
   - `tests/e2e-unified/tests/test_delete_account.py::test_windows_an_account_the_nest_stops_accepting_erases_nothing_on_the_device`
   - `tests/e2e-unified/tests/test_delete_account.py::test_apple_an_account_the_nest_stops_accepting_erases_nothing_on_the_device`
   - `tests/e2e-unified/tests/test_delete_account.py::test_web_an_account_the_nest_stops_accepting_erases_nothing_on_the_device`
13. [app] Signing out while another Fauna window on this device is using one of your accounts is refused: nothing is erased, you stay signed in, and you are told to close the other window and sign out again — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_sign_out_refused_other_tab_web.py::test_web_sign_out_refuses_while_another_tab_holds_the_engine`
   - `tests/e2e-unified/tests/test_erase_refused_other_instance.py::test_sign_out_refuses_while_another_instance_serves_the_account`
14. [app] What a sign-out could not remove can be removed again from the sign-in screen, and the notice goes away only once nothing is left — `docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy (iron-clad for new app state)
   - `tests/e2e-unified/tests/test_sign_out.py::test_the_sign_out_residue_retry_finishes_the_erase`
   - `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_residue_retry_refuses_beside_another_tab_and_finishes_the_erase`
15. [app] The notice about data a sign-out left behind is still shown after the app is closed and reopened, and reopening quietly tries again first — `docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy (iron-clad for new app state)
   - `tests/e2e-unified/tests/test_sign_out.py::test_the_sign_out_residue_outlives_the_app`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_settings.py::test_handle_change_validation` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings.py::test_actor_id_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings.py::test_copy_buttons_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_returns_to_onboarding` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_erases_the_credential_namespace` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_erases_the_account_scoped_state_directory` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_erases_the_account_registry` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_retires_the_enrollment_and_erases_the_account_store` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sign_out_web.py::test_web_load_after_a_tab_closed_mid_sign_out_finishes_the_sign_out` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_bearer_cache_web.py::test_web_sign_out_then_new_identity_posts_as_itself` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_delete_account.py::test_delete_account_via_confirm_field` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_pending_actions.py::test_a_scheduled_handle_change_is_visible_and_cancellable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_pending_actions.py::test_a_queued_account_delete_is_visible_and_cancellable` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_pending_actions.py::test_an_admins_deletion_of_this_account_is_listed_and_cancellable` | linux (linux): passed, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_onboarding.py::test_alice_self_service_onboarding` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_sign_out.py::test_a_sign_out_that_cannot_erase_everything_says_so` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_sign_out.py::test_a_sign_out_whose_credentials_cannot_be_erased_says_so` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_that_cannot_erase_a_store_says_so_and_a_later_load_finishes` | web (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_settings.py::test_handle_someone_else_holds_is_refused_and_not_applied` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_sign_out.py::test_sign_out_returns_to_onboarding` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_delete_account.py::test_delete_account_via_confirm_field` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_pending_actions.py::test_a_scheduled_handle_change_is_not_shown_as_your_handle` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_delete_account.py::test_tui_an_account_the_nest_stops_accepting_erases_nothing_on_the_device` | tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_delete_account.py::test_linux_an_account_the_nest_stops_accepting_erases_nothing_on_the_device` | linux (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_delete_account.py::test_windows_an_account_the_nest_stops_accepting_erases_nothing_on_the_device` | windows (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_delete_account.py::test_apple_an_account_the_nest_stops_accepting_erases_nothing_on_the_device` | macos (macos): passed, ios (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_delete_account.py::test_web_an_account_the_nest_stops_accepting_erases_nothing_on_the_device` | web (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_sign_out_refused_other_tab_web.py::test_web_sign_out_refuses_while_another_tab_holds_the_engine` | web (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_erase_refused_other_instance.py::test_sign_out_refuses_while_another_instance_serves_the_account` | — |
| 14 | app | `tests/e2e-unified/tests/test_sign_out.py::test_the_sign_out_residue_retry_finishes_the_erase` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_sign_out_web.py::test_web_sign_out_residue_retry_refuses_beside_another_tab_and_finishes_the_erase` | web (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_sign_out.py::test_the_sign_out_residue_outlives_the_app` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
<!-- features-render:end -->

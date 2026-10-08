---
slug: multiple-accounts
title: Several identities in one app
section: your data and devices
goal: docs/goal/architecture/long-term-store.md § Multi-account evolution (target state)
guide: docs/guides/identity-and-devices.md § Several identities on one device
---

## What a user gets

Add a second identity and switch between them from Settings; each keeps its own
conversations, files and settings, and removing one never touches the others. An
identity you mark as protected asks you to confirm before it activates, and the
nest's admin identity is protected by default. Abandoning an add halfway leaves the
identity you had.

## Coverage contract

Stamped 2026-09-25 at efcea449ea.

1. [app] The switcher lists your identities, switches between them, and reveals the admin one — `docs/goal/architecture/long-term-store.md` § Multi-account evolution (target state)
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_account_switcher_lists_switches_and_reveals_admin`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_account_switcher_lists_switches_and_reveals_admin`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_account_switcher_lists_switches_and_reveals_admin`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_account_switcher_lists_switches_and_reveals_admin`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_account_switcher_lists_switches_and_reveals_admin`
2. [app] Adding an identity appends it, and abandoning the add halfway leaves the one you had — `docs/goal/behavior/onboarding.md` § Multi-account (add-account = append-mode onboarding)
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_add_account_appends_second_identity_to_registry`
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_add_account_abandon_recovers_prior_identity_on_relaunch`
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_add_account_pending_invite_submit_switches_the_live_session`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_add_account_appends_second_identity_to_registry`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_add_account_pending_invite_submit_switches_the_live_session`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_add_account_appends_second_identity_to_registry`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_add_account_abandon_recovers_prior_identity_on_relaunch`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_add_account_appends_second_identity_to_registry`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_add_account_abandon_recovers_prior_identity_on_current_load`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_add_account_appends_second_identity_to_registry`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_add_account_abandon_after_import_recovers_prior_identity`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_add_account_pending_invite_submit_switches_the_live_session`
3. [app] Removing an identity drops it from the switcher at once — `docs/goal/architecture/long-term-store.md` § Multi-account evolution (target state)
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_remove_account_shrinks_switcher_live`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_remove_account_shrinks_switcher_live`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_remove_account_shrinks_switcher_live`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_remove_account_shrinks_switcher_live`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_remove_account_shrinks_switcher_live`
4. [app] A protected identity asks you to confirm before it activates, and the admin identity is protected by default — `docs/goal/architecture/long-term-store.md` § Multi-account evolution (target state)
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_require_confirm_gates_switch_decline_then_approve`
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_admin_auto_default_flags_admin_and_explicit_off_sticks`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_require_confirm_gates_switch_decline_then_approve`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_require_confirm_gates_switch_decline_then_approve`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_require_confirm_gates_switch_decline_then_approve`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_require_confirm_gates_switch_decline_then_approve`
5. [app] Each identity's data stays its own; a switch never leaks one identity's state into another — `docs/goal/architecture/apps/account-scoping-dispositions.md` § Serialized switching — completing the isolation contract
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_switch_scopes_account_state_dirs_and_preserves_the_outgoing_account`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_account_switch_scopes_mls_state_per_actor`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_switching_back_to_an_account_serves_its_conversations_again`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_mls_store_is_account_scoped_and_never_adopts_a_flat_store`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_account_switch_isolates_scoped_state_on_disk`
   - `tests/e2e-unified/tests/test_conversations_actor_switch.py::test_actor_switch_on_the_conversations_route_rebinds_the_page`
   - `tests/e2e-unified/tests/test_media_actor_switch.py::test_actor_switch_on_the_media_route_rebinds_the_page`
   - `tests/e2e-unified/tests/test_content_policy_actor_switch.py::test_actor_switch_on_the_feed_route_does_not_leak_the_outgoing_wards_content_floor`
6. [app] Removing an identity from this device deletes that identity's own data here and leaves your other identities' data untouched — `docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy (iron-clad for new app state)
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_removing_an_identity_deletes_its_data_and_leaves_the_others`
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_removing_an_identity_deletes_its_data_and_leaves_the_others`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_removing_an_identity_deletes_its_data_and_leaves_the_others`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_removing_an_identity_deletes_its_data_and_leaves_the_others`
7. [app] Turning the confirmation off for an identity keeps it off, even once the app learns that identity is the admin one — `docs/goal/architecture/long-term-store.md` § Multi-account evolution (target state)
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_admin_auto_default_flags_admin_and_explicit_off_sticks`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_admin_auto_default_flags_admin_and_explicit_off_sticks`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_admin_auto_default_flags_admin_and_explicit_off_sticks`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_admin_auto_default_flags_admin_and_explicit_off_sticks`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_admin_auto_default_flags_admin_and_explicit_off_sticks`
8. [app] An identity you started creating and then abandoned never shows up among your identities — `docs/goal/architecture/long-term-store.md` § Multi-account evolution (target state)
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_abandoned_create_identity_does_not_ghost_the_switcher`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_abandoned_created_identity_never_shows_among_your_identities`
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_abandoned_created_identity_never_shows_among_your_identities`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_abandoned_created_identity_never_shows_among_your_identities`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_abandoned_created_identity_never_shows_among_your_identities`
9. [app] Switching to an identity this device can no longer sign in as is refused, and you stay on the identity you were using — `docs/goal/architecture/long-term-store.md` § Multi-account evolution (target state)
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused`
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused`
   - `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused`
10. [app] Settings that describe this device stay as they are when you switch identity, while drafts and other choices tied to an identity follow it — `docs/goal/architecture/apps/account-scoping-dispositions.md` § Serialized switching — completing the isolation contract
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_switching_identity_keeps_device_settings_and_moves_drafts`
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_switching_identity_keeps_device_settings_and_moves_drafts`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_switching_identity_keeps_device_settings_and_moves_drafts`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_switching_identity_keeps_device_settings_and_moves_drafts`
11. [app] Switching identity leaves this device's notifications on or off, exactly as you set them — `docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy (iron-clad for new app state)
   - `tests/e2e-unified/tests/test_push_settings.py::test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_windows_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
12. [app] An identity another Fauna window on this device is using cannot be removed — nothing is erased and you are told to close that window first — and the identity this window is using is never offered for removal — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_remove_account_refused_other_tab_web.py::test_web_remove_account_refuses_an_account_another_tab_serves`
   - `tests/e2e-unified/tests/test_erase_refused_other_instance.py::test_remove_account_refuses_while_another_instance_serves_it`
13. [app] Adding an identity that sets up its own nest keeps you on the identity you had until that setup finishes, and a setup you walked away from waits in the switcher instead of taking over — `docs/goal/behavior/onboarding.md` § Multi-account (add-account = append-mode onboarding)
   - `tests/e2e-unified/tests/test_add_account_provisioning.py::test_an_add_account_provisioning_run_holds_custody_without_hijacking_the_live_session`
   - `tests/e2e-unified/tests/test_add_account_provisioning.py::test_an_add_account_deferred_dns_exit_switches_to_the_appended_identity_and_survives_a_relaunch`
14. [app] Two Fauna windows on this device changing your identities at the same time never lose each other's change — one waits for the other — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_account_registry_mutation_lock_web.py::test_a_registry_write_queues_behind_a_sibling_tabs_mutation_lock`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.2-dev+78e73031 standalone |
| windows | ⚠ partial | 0.1.2-dev+a3582c8b.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+4bba6f8f standalone |
| ios | ⚠ partial | 0.1.2-dev+4bba6f8f standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+95b53391 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_account_switcher_lists_switches_and_reveals_admin` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_account_switcher_lists_switches_and_reveals_admin` | macos (macos): passed, ios (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_account_switcher_lists_switches_and_reveals_admin` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_account_switcher_lists_switches_and_reveals_admin` | web (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_account_switcher_lists_switches_and_reveals_admin` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_add_account_appends_second_identity_to_registry` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_add_account_abandon_recovers_prior_identity_on_relaunch` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_add_account_pending_invite_submit_switches_the_live_session` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_add_account_appends_second_identity_to_registry` | macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_add_account_pending_invite_submit_switches_the_live_session` | macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_add_account_appends_second_identity_to_registry` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_add_account_abandon_recovers_prior_identity_on_relaunch` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_add_account_appends_second_identity_to_registry` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_add_account_abandon_recovers_prior_identity_on_current_load` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_add_account_appends_second_identity_to_registry` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_add_account_abandon_after_import_recovers_prior_identity` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_add_account_pending_invite_submit_switches_the_live_session` | windows (windows): failed |
| 3 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_remove_account_shrinks_switcher_live` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_remove_account_shrinks_switcher_live` | macos (macos): passed, ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_remove_account_shrinks_switcher_live` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_remove_account_shrinks_switcher_live` | windows (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_remove_account_shrinks_switcher_live` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_require_confirm_gates_switch_decline_then_approve` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_admin_auto_default_flags_admin_and_explicit_off_sticks` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_require_confirm_gates_switch_decline_then_approve` | macos (macos): passed, ios (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_require_confirm_gates_switch_decline_then_approve` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_require_confirm_gates_switch_decline_then_approve` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_require_confirm_gates_switch_decline_then_approve` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_switch_scopes_account_state_dirs_and_preserves_the_outgoing_account` | linux (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_account_switch_scopes_mls_state_per_actor` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_switching_back_to_an_account_serves_its_conversations_again` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_mls_store_is_account_scoped_and_never_adopts_a_flat_store` | — |
| 5 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_account_switch_isolates_scoped_state_on_disk` | windows (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_conversations_actor_switch.py::test_actor_switch_on_the_conversations_route_rebinds_the_page` | web (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_media_actor_switch.py::test_actor_switch_on_the_media_route_rebinds_the_page` | web (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_content_policy_actor_switch.py::test_actor_switch_on_the_feed_route_does_not_leak_the_outgoing_wards_content_floor` | web (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_removing_an_identity_deletes_its_data_and_leaves_the_others` | tui (linux): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_removing_an_identity_deletes_its_data_and_leaves_the_others` | linux (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_removing_an_identity_deletes_its_data_and_leaves_the_others` | macos (macos): passed, ios (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_removing_an_identity_deletes_its_data_and_leaves_the_others` | windows (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_admin_auto_default_flags_admin_and_explicit_off_sticks` | linux (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_admin_auto_default_flags_admin_and_explicit_off_sticks` | macos (macos): passed, ios (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_admin_auto_default_flags_admin_and_explicit_off_sticks` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_admin_auto_default_flags_admin_and_explicit_off_sticks` | web (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_admin_auto_default_flags_admin_and_explicit_off_sticks` | windows (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_abandoned_create_identity_does_not_ghost_the_switcher` | windows (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_abandoned_created_identity_never_shows_among_your_identities` | tui (linux): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_abandoned_created_identity_never_shows_among_your_identities` | linux (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_abandoned_created_identity_never_shows_among_your_identities` | macos (macos): passed, ios (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_abandoned_created_identity_never_shows_among_your_identities` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused` | tui (linux): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused` | linux (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused` | macos (macos): passed, ios (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_switcher_web.py::test_web_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused` | windows (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_switching_identity_keeps_device_settings_and_moves_drafts` | tui (linux): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_switching_identity_keeps_device_settings_and_moves_drafts` | linux (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_switching_identity_keeps_device_settings_and_moves_drafts` | macos (macos): passed, ios (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_switching_identity_keeps_device_settings_and_moves_drafts` | windows (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_push_settings.py::test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | tui (linux): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_push_settings.py::test_windows_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | windows (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_remove_account_refused_other_tab_web.py::test_web_remove_account_refuses_an_account_another_tab_serves` | web (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_erase_refused_other_instance.py::test_remove_account_refuses_while_another_instance_serves_it` | linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_add_account_provisioning.py::test_an_add_account_provisioning_run_holds_custody_without_hijacking_the_live_session` | linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_add_account_provisioning.py::test_an_add_account_deferred_dns_exit_switches_to_the_appended_identity_and_survives_a_relaunch` | linux (linux): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_account_registry_mutation_lock_web.py::test_a_registry_write_queues_behind_a_sibling_tabs_mutation_lock` | — |
<!-- features-render:end -->

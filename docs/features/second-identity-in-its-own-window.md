---
slug: second-identity-in-its-own-window
title: A second identity in its own window
section: your data and devices
goal: docs/goal/architecture/apps/account-scoping.md § Concurrent instances
guide: docs/guides/identity-and-devices.md § Several identities on one device
absences:
  ios: "docs/goal/architecture/apps/account-scoping.md § Concurrent instances"
  android: "docs/goal/architecture/apps/account-scoping.md § Concurrent instances"
---

## What a user gets

On the desktop, open a second identity in a window of its own and use both at
once. Launching the app while it is already open offers to pick an identity or
takes you straight back to what is running; two windows never fight over one
identity.

## Coverage contract

Stamped 2026-09-19 at acb58fddf3.

1. [app] The switcher opens a second identity in a place of its own — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_open_as_new_instance_spawns_a_bound_sibling`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_open_as_new_instance_spawns_a_bound_sibling`
   - `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_open_as_new_instance_spawns_a_bound_sibling`
   - `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_open_as_new_instance_copies_the_launch_command`
   - `tests/e2e-unified/tests/test_account_tab_pin_web.py::test_web_new_tab_switcher_opens_the_other_identity_in_that_tab`
2. [app] Two windows on different identities run at the same time, each as its own — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_bound_launch_for_another_account_coexists_as_that_account`
   - `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_bound_launch_for_another_account_coexists_as_that_account`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_instances_on_different_accounts_run_concurrently`
   - `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_bound_launch_for_another_account_coexists_as_that_account`
   - `tests/e2e-unified/tests/test_account_tab_pin_web.py::test_web_second_tab_switch_does_not_drag_the_first_tab_along`
3. [app] Launching the app again while it is open offers the other identities, or takes you back to what is already running — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_launch_instance_chooser_linux.py::test_linux_second_plain_launch_renders_chooser_and_pick_completes_as_that_account`
   - `tests/e2e-unified/tests/test_launch_instance_chooser_linux.py::test_linux_focus_existing_raises_a_bound_sibling`
   - `tests/e2e-unified/tests/test_launch_instance_chooser_windows.py::test_windows_second_plain_launch_renders_chooser_and_pick_completes_as_that_account`
   - `tests/e2e-unified/tests/test_launch_instance_chooser_windows.py::test_windows_focus_existing_raises_a_bound_sibling_over_the_per_account_channel`
   - `tests/e2e-unified/tests/test_launch_instance_chooser_tui.py::test_tui_second_plain_launch_renders_chooser_and_pick_completes_as_that_account`
   - `tests/e2e-unified/tests/artifact/test_macos_app_bundle.py::test_launching_the_bundle_again_lands_on_the_running_instance`
   - `tests/e2e-unified/tests/test_account_tab_pin_web.py::test_web_launching_again_comes_up_on_the_running_identity_and_offers_the_others`
4. [app] Opening an identity that is already open gives you a second window on it, and both keep running — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_bound_launch_onto_the_served_account_coexists`
   - `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_bound_launch_onto_the_served_account_coexists`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_bound_launch_onto_the_served_account_coexists`
   - `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_bound_launch_onto_the_served_account_coexists`
   - `tests/e2e-unified/tests/test_engine_role_election_web.py::test_a_second_tab_on_one_account_runs_no_second_mls_engine`
5. [app] When one identity is open in two places, only one of them runs your conversations, and the other tells you they are being served elsewhere — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_engine_role_election_web.py::test_a_second_tab_on_one_account_runs_no_second_mls_engine`
   - `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_bound_launch_onto_the_served_account_coexists`
   - `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_bound_launch_onto_the_served_account_coexists`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_bound_launch_onto_the_served_account_coexists`
   - `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_bound_launch_onto_the_served_account_coexists`
6. [app] If the place you asked to go back to is already gone, the app starts normally instead of failing — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_launch_instance_chooser_tui.py::test_tui_focus_existing_onto_an_instance_that_has_gone_starts_normally`
   - `tests/e2e-unified/tests/test_launch_instance_chooser_linux.py::test_linux_focus_existing_onto_an_instance_that_has_gone_starts_normally`
   - `tests/e2e-unified/tests/test_launch_instance_chooser_windows.py::test_windows_focus_existing_onto_an_instance_that_has_gone_starts_normally`
   - `tests/e2e-unified/tests/artifact/test_macos_app_bundle.py::test_relaunching_the_bundle_after_quitting_starts_normally_as_the_same_account`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+e7095c29 standalone |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.3-dev+3a518178 standalone |
| macos | ✅ full | |
| ios | — absent | |
| android | — absent | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_linux.py::test_linux_open_as_new_instance_spawns_a_bound_sibling` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_open_as_new_instance_spawns_a_bound_sibling` | macos (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_windows.py::test_windows_open_as_new_instance_spawns_a_bound_sibling` | windows (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_switcher_tui.py::test_tui_open_as_new_instance_copies_the_launch_command` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_account_tab_pin_web.py::test_web_new_tab_switcher_opens_the_other_identity_in_that_tab` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_bound_launch_for_another_account_coexists_as_that_account` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_bound_launch_for_another_account_coexists_as_that_account` | windows (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_instances_on_different_accounts_run_concurrently` | macos (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_bound_launch_for_another_account_coexists_as_that_account` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_account_tab_pin_web.py::test_web_second_tab_switch_does_not_drag_the_first_tab_along` | web (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_linux.py::test_linux_second_plain_launch_renders_chooser_and_pick_completes_as_that_account` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_linux.py::test_linux_focus_existing_raises_a_bound_sibling` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_windows.py::test_windows_second_plain_launch_renders_chooser_and_pick_completes_as_that_account` | windows (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_windows.py::test_windows_focus_existing_raises_a_bound_sibling_over_the_per_account_channel` | windows (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_tui.py::test_tui_second_plain_launch_renders_chooser_and_pick_completes_as_that_account` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/artifact/test_macos_app_bundle.py::test_launching_the_bundle_again_lands_on_the_running_instance` | macos (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_account_tab_pin_web.py::test_web_launching_again_comes_up_on_the_running_identity_and_offers_the_others` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_bound_launch_onto_the_served_account_coexists` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_bound_launch_onto_the_served_account_coexists` | windows (windows): failed |
| 4 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_bound_launch_onto_the_served_account_coexists` | macos (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_bound_launch_onto_the_served_account_coexists` | tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_engine_role_election_web.py::test_a_second_tab_on_one_account_runs_no_second_mls_engine` | web (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_engine_role_election_web.py::test_a_second_tab_on_one_account_runs_no_second_mls_engine` | web (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_bound_launch_onto_the_served_account_coexists` | linux (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_bound_launch_onto_the_served_account_coexists` | windows (windows): failed |
| 5 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_bound_launch_onto_the_served_account_coexists` | macos (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_bound_launch_onto_the_served_account_coexists` | tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_tui.py::test_tui_focus_existing_onto_an_instance_that_has_gone_starts_normally` | tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_linux.py::test_linux_focus_existing_onto_an_instance_that_has_gone_starts_normally` | linux (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_launch_instance_chooser_windows.py::test_windows_focus_existing_onto_an_instance_that_has_gone_starts_normally` | windows (windows): passed |
| 6 | app | `tests/e2e-unified/tests/artifact/test_macos_app_bundle.py::test_relaunching_the_bundle_after_quitting_starts_normally_as_the_same_account` | macos (macos): passed |
<!-- features-render:end -->

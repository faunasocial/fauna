---
slug: devices
title: Your devices
section: your data and devices
goal: docs/goal/ui/devices.md § Goal
guide: docs/guides/identity-and-devices.md § Adding a device
absences:
  web (outcome 27): "docs/goal/architecture/apps/sync-agent-credentials.md § Credential model"
  web (outcome 12): "docs/goal/ui/devices.md § Where logic lives"
  android (outcome 27): "docs/goal/architecture/apps/sync-agent-credentials.md § Credential model"
  ios (outcome 27): "docs/goal/architecture/apps/sync-agent-credentials.md § Credential model"
---

## What a user gets

The Devices page lists every device signed in as you, exactly as your nest
knows them, marks the one you are holding, shows which folders each takes part in,
and lets you remove one. Your own identity is one tap away to hand to a new device.

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

1. [app] The page lists exactly the devices your nest holds for you, and a new device appears once it registers — `docs/goal/ui/devices.md` § Layout & flow
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_devices_page_matches_nest_roster`
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_shows_after_register`
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_nameless_device_card_is_still_counted`
2. [app] The device you are holding is marked, and each device shows its folder roles — `docs/goal/behavior/devices.md` § This-device marker (client-side, no wire change)
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_marks_this_device`
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_fileset_role_badge_shows_role_chip`
3. [app] Each device can be removed, and your own identity is one copy away — `docs/goal/ui/devices.md` § User actions
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_remove_button_visible`
   - `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_peer_actor_id_copy_button_is_single_instance`
4. [nest] A device that registers for sync is listed for its owner, with its folder roles — `docs/goal/behavior/devices.md` § Listing Devices
   - `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_registered_device_appears_in_ws_rpc_list`
   - `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_device_folder_role_appears_in_ws_rpc_list`
5. [app] The device you are signed in on reads Online, and a device nothing is connected as reads Offline — `docs/goal/behavior/devices.md` § Listing Devices
   - `tests/e2e-unified/tests/test_device_online.py::test_the_signed_in_apps_own_device_reads_online`
6. [app] When your account is at its tier's device limit, the page says so and names both remedies — remove a device here, or ask the admin for a bigger tier — and the notice clears once a slot frees — `docs/goal/ui/devices.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_device_cap_refusal.py::test_a_capped_account_is_told_on_the_devices_page_and_the_remedy_clears_it`
   - `tests/e2e-unified/tests/test_device_cap_refusal.py::test_a_capped_account_removes_a_dead_enrolled_seat_holding_its_slot`
7. [nest] Your nest refuses a new device past the tier's limit and enrolls nothing, while a device you already have re-registers freely — `docs/goal/behavior/devices.md` § Step 4 — Register for sync
   - `tests/e2e-unified/tests/api/test_device_cap_api.py::test_a_new_device_past_the_tier_cap_is_refused_and_a_held_one_re_registers`
8. [app] A removal that cannot be done safely deletes nothing and says why — the device in your hand, one the app cannot verify yet, or a row that disagrees with the device's own record — `docs/goal/ui/devices.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_device_removal_refusal.py::test_removing_the_device_in_hand_is_refused_and_deletes_nothing`
9. [app] A device your nest still counts but no row here can name is shown by its fingerprint and can be removed — `docs/goal/architecture/account-data-taxonomy.md` § The generation machinery
   - `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_whose_entry_was_deleted_elsewhere_is_listed_by_fingerprint_and_removed`
   - `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_that_misstates_its_row_is_refused_on_its_row_and_removed_by_its_card`
10. [app] A device of yours that holds no keys is marked relay-only — a fact shown, never a switch — `docs/goal/ui/devices.md` § Custody facet
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_device_that_holds_no_keys_is_marked_relay_only`
11. [app] You ask a friend from your conversations to hold copies for you, and are told what they would see before you confirm — `docs/goal/ui/nests.md` § Trust facet — custody rows
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_web_owner_mints_an_offer_a_native_friend_accepts`
12. [app] What you hold for someone is metered against a budget you picked when you accepted, and you can change it here — `docs/goal/ui/devices.md` § Custody facet
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
13. [app] An offer to hold copies that you do not want is declined and goes away — `docs/goal/ui/devices.md` § Where logic lives
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_declined_custody_offer_goes_away`
14. [app] Removing a copy you hold for someone frees its space, where pausing only stops it — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts`
15. [app] A copy you declined or removed does not come back when its owner sends it again, and your pause and allowance survive the owner's refresh — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_removed_custody_stays_gone_when_its_owner_sends_again`
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_declined_custody_offer_stays_gone_when_its_owner_sends_again`
16. [nest] Removing a device ends every sign-in that device made, for good — `docs/goal/behavior/devices.md` § What a session is, and what revoking one does
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_removing_a_device_ends_every_sign_in_it_made_for_good`
17. [app] A friend you asked from one of your devices shows as holding your copies on every other device of yours, and you can stop trusting them from any of them — `docs/goal/ui/devices.md` § Custody facet
    - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_custody_minted_on_another_device_renders_and_revokes_here`
18. [app] A device you have not named is listed under its app's name — a phone that syncs no folder included — `docs/goal/behavior/devices.md` § Device names
    - `tests/e2e-unified/tests/test_device_names.py::test_a_phone_that_syncs_no_folder_is_named_for_its_app_on_a_siblings_devices_page`
    - `tests/e2e-unified/tests/test_device_names.py::test_an_unnamed_desktop_device_is_named_for_its_app_on_a_siblings_devices_page`
19. [app] You can rename any of your devices from another of your devices, and the new name shows on both — `docs/goal/behavior/devices.md` § Device names
    - `tests/e2e-unified/tests/test_device_names.py::test_a_device_renamed_on_one_seat_reads_its_new_name_on_both`
20. [app] A device you removed and then sign in on again comes back as a new device in the list, and the changes it made that never reached your nest are kept — `docs/goal/architecture/account-data-plane.md` § Implementation status today
    - `tests/e2e-unified/tests/test_device_removed_returns_as_successor.py::test_a_removed_device_signed_in_again_is_one_new_device_and_keeps_its_offline_change`
21. [nest] A device you remove or sign out of while it is bound to one of your linked nests is dropped on your other linked nest too, which stops accepting it — `docs/goal/architecture/account-sync-plane.md` § Implementation status today
    - `tests/e2e-unified/tests/api/test_device_removal_linked_nests.py::test_a_device_leaving_at_one_linked_nest_is_dropped_on_the_other`
22. [nest] A device you removed can no longer pull your copies from a friend who holds them for you — `docs/goal/architecture/account-sync-plane.md` § Implementation status today
    - `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_the_nest_hands_a_held_copy_back_only_to_the_owners_own_devices`
23. [nest] A device you removed cannot read what your account writes after the removal — `docs/goal/architecture/account-data-taxonomy.md` § Implementation status today
    - `tests/e2e-unified/tests/api/test_device_removal_account_plane.py::test_a_removed_device_cannot_read_what_the_account_writes_after_the_removal`
24. [nest] Removing a device keeps for your other devices what only it had recorded, such as the conversations it alone marked read — `docs/goal/architecture/delegable-scope-reclamation.md` § Implementation status today
    - `tests/e2e-unified/tests/api/test_device_removal_account_plane.py::test_removing_a_device_keeps_for_the_others_what_only_it_had_recorded`
25. [app] Signing out of a device keeps its one row on the Devices list of your other devices, and signing back in brings it back to that row, not a second one — `docs/goal/architecture/apps/sync-agent-credentials.md` § Credential model
   - `tests/e2e-unified/tests/test_device_roster_sign_out.py::test_a_sign_out_and_back_in_keeps_one_row_on_a_siblings_devices_page`
26. [app] A device renamed or removed from another of your devices shows that change on an open Devices page without leaving it — `docs/goal/behavior/devices.md` § Listing Devices
   - `tests/e2e-unified/tests/test_device_names.py::test_an_open_devices_page_repaints_a_sibling_rename_and_removal`
27. [app] A computer running both the app and its sync helper appears once in your Devices, never once for each, and a sign-out cut short part-way still retires that entry — `docs/goal/architecture/apps/sync-agent-credentials.md` § Credential model
   - `tests/e2e-unified/tests/test_device_roster_sign_out.py::test_an_app_and_its_helper_are_one_row_and_a_cut_short_sign_out_still_retires_it`
28. [nest] Removing one of your devices closes every connection it still had open, at once — `docs/goal/architecture/transport-connection.md` § Connection lifecycle
   - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_removing_a_device_closes_every_connection_it_still_had_open_at_once`
29. [app] A name you gave a device stays when its app re-registers under its default name, and two devices with the same name are told apart by a short code beside each — `docs/goal/behavior/devices.md` § Device names
   - `tests/e2e-unified/tests/test_device_name_rules.py::test_a_chosen_name_survives_its_apps_default_reregister_and_alike_names_carry_a_code`
30. [app] A rename that cannot be made is refused in words and sends nothing: this device holds no owner key, or the name you typed is another app's default name — `docs/goal/behavior/devices.md` § Device names
   - `tests/e2e-unified/tests/test_device_name_rules.py::test_a_rename_to_another_apps_default_name_is_refused_in_words_and_sends_nothing`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+c9e7fe5d standalone |
| linux | ⚠ partial | 0.1.5 docker |
| windows | ⚠ partial | 0.1.2-dev+4bfe5053.dirty standalone |
| macos | ⚠ partial | 0.1.5-dev+7b3786bb standalone |
| ios | ⚠ partial | 0.1.5-dev+c9e7fe5d standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+c9e7fe5d standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_devices_page_matches_nest_roster` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_shows_after_register` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_nameless_device_card_is_still_counted` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_card_marks_this_device` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_fileset_role_badge_shows_role_chip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_device_remove_button_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_device_cards.py::TestDeviceCards::test_peer_actor_id_copy_button_is_single_instance` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_registered_device_appears_in_ws_rpc_list` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_sync_devices_api.py::test_device_folder_role_appears_in_ws_rpc_list` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_device_online.py::test_the_signed_in_apps_own_device_reads_online` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_device_cap_refusal.py::test_a_capped_account_is_told_on_the_devices_page_and_the_remedy_clears_it` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_device_cap_refusal.py::test_a_capped_account_removes_a_dead_enrolled_seat_holding_its_slot` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_device_cap_api.py::test_a_new_device_past_the_tier_cap_is_refused_and_a_held_one_re_registers` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_device_removal_refusal.py::test_removing_the_device_in_hand_is_refused_and_deletes_nothing` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_whose_entry_was_deleted_elsewhere_is_listed_by_fingerprint_and_removed` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_device_member_removal.py::test_a_device_that_misstates_its_row_is_refused_on_its_row_and_removed_by_its_card` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_device_that_holds_no_keys_is_marked_relay_only` | ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_web_owner_mints_an_offer_a_native_friend_accepts` | web (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | absent by design on web | — |
| 13 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_declined_custody_offer_goes_away` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 14 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_two_accounts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_removed_custody_stays_gone_when_its_owner_sends_again` — requires `custody-ceremony-resend` (test harness) (not built: web) | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_declined_custody_offer_stays_gone_when_its_owner_sends_again` — requires `custody-ceremony-resend` (test harness) (not built: web) | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_removing_a_device_ends_every_sign_in_it_made_for_good` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 17 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_custody_minted_on_another_device_renders_and_revokes_here` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 18 | app | `tests/e2e-unified/tests/test_device_names.py::test_a_phone_that_syncs_no_folder_is_named_for_its_app_on_a_siblings_devices_page` | ios (macos): passed |
| 18 | app | `tests/e2e-unified/tests/test_device_names.py::test_an_unnamed_desktop_device_is_named_for_its_app_on_a_siblings_devices_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 19 | app | `tests/e2e-unified/tests/test_device_names.py::test_a_device_renamed_on_one_seat_reads_its_new_name_on_both` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 20 | app | `tests/e2e-unified/tests/test_device_removed_returns_as_successor.py::test_a_removed_device_signed_in_again_is_one_new_device_and_keeps_its_offline_change` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_device_removal_linked_nests.py::test_a_device_leaving_at_one_linked_nest_is_dropped_on_the_other` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_the_nest_hands_a_held_copy_back_only_to_the_owners_own_devices` | nest (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/api/test_device_removal_account_plane.py::test_a_removed_device_cannot_read_what_the_account_writes_after_the_removal` | nest (linux): passed, nest (macos): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_device_removal_account_plane.py::test_removing_a_device_keeps_for_the_others_what_only_it_had_recorded` | nest (linux): passed, nest (macos): passed |
| 25 | app | `tests/e2e-unified/tests/test_device_roster_sign_out.py::test_a_sign_out_and_back_in_keeps_one_row_on_a_siblings_devices_page` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 26 | app | `tests/e2e-unified/tests/test_device_names.py::test_an_open_devices_page_repaints_a_sibling_rename_and_removal` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 27 | app | `tests/e2e-unified/tests/test_device_roster_sign_out.py::test_an_app_and_its_helper_are_one_row_and_a_cut_short_sign_out_still_retires_it` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 27 | app | absent by design on web, ios, android, android-kids, ios-kids | — |
| 28 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_removing_a_device_closes_every_connection_it_still_had_open_at_once` | nest (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_device_name_rules.py::test_a_chosen_name_survives_its_apps_default_reregister_and_alike_names_carry_a_code` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 30 | app | `tests/e2e-unified/tests/test_device_name_rules.py::test_a_rename_to_another_apps_default_name_is_refused_in_words_and_sends_nothing` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->

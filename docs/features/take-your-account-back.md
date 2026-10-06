---
slug: take-your-account-back
title: Take your account back
section: your data and devices
goal: docs/goal/behavior/identity-succession.md § Goal
guide: docs/guides/identity-and-devices.md § If a device is lost or stolen
---

## What a user gets

If your identity key is stolen, the recovery kit lets you succeed it: the old
key stops working everywhere, your groups re-point to you, and any device still on
the old key is told what happened and routed to import the new one. Your data,
backups, conversations, drafts and mail come back under the new key, the old mail
passwords are burned, and anything the thief could have changed is raised for your
review.

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] A kit holder takes the account back and comes back up as the successor; an unconfirmed attempt is refused out loud — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_kit_holder_takes_the_account_back_and_comes_back_up_as_the_successor`
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_confirm_gate_refuses_rather_than_dropping_the_command`
2. [app] A device still on the old key is refused and routed to import the new one — `docs/goal/behavior/identity-succession.md` § Propagation
   - `tests/e2e-unified/tests/test_identity_succession_refusal.py::test_a_succeeded_device_is_refused_and_routed_to_import`
3. [app] A group you hold is re-pointed to you and the stolen key is evicted, on your app and on a member's — `docs/goal/behavior/identity-succession.md` § Propagation
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_ceremony_re_points_a_real_group_and_evicts_the_stolen_leaf`
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_real_member_re_points_the_succeeded_participant_on_their_own_client`
   - `tests/e2e-unified/tests/test_succession_witness_registered.py::test_a_conversations_seat_registers_a_succession_witness`
4. [app] Your settings, files, backups, conversations and drafts come back under the new key without a command — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_still_read_the_config_it_inherited`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_open_muted_words_page_paints_what_it_inherited`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_read_the_corpus_it_inherited`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_backups_restart_without_a_user_command`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_conversations_unlock_without_a_user_command`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_unsent_drafts_come_back`
5. [app] What the thief could have changed is raised for your review: trusts, backup destinations, group members, a linked Nostr key — `docs/goal/behavior/succession-aftermath.md` § Adjudicating what the aftermath carries across
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_about_the_trust_it_inherited`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_about_the_destination_it_inherited`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_first_session_raises_the_member_it_cannot_vouch_for`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_to_confirm_its_npub`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_dismiss_npub_confirm_into_a_fresh_key`
6. [app] Every mail password from before stops working — `docs/goal/behavior/mail-credentials.md` § Succession (every pre-succession credential is burned)
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_mail_passwords_are_burned`
   - `tests/e2e-unified/tests/test_identity_succession_mail_auth.py::test_a_succession_stops_the_predecessors_mail_password_authenticating`
7. [nest] Your nest records a valid succession, refuses a forged one, and lets the real owner act even while the thief has locked the account — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
   - `tests/e2e-unified/tests/api/test_succession_fixture.py::test_the_fixture_succeeds_an_identity_and_the_nest_records_it`
   - `tests/e2e-unified/tests/api/test_recovery_pre_identity_remedies.py::test_a_succession_lands_through_an_active_lockout_with_no_session`
   - `tests/e2e-unified/tests/api/test_recovery_pre_identity_remedies.py::test_the_key_holder_vetoes_a_seed_alone_replacement_over_an_anonymous_connection`
   - `tests/e2e-unified/tests/api/test_succession_lost_reply.py::test_a_dropped_submit_reply_fails_at_transport_while_the_succession_lands`
8. [app] When the group re-pointing did not finish, the app offers to finish it and answers you when you press — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_succession_sweep_retry.py::test_a_half_done_sweep_renders_the_retry_and_the_press_is_honoured`
   - `tests/e2e-unified/tests/test_succession_sweep_retry_web.py::test_a_half_done_sweep_answers_no_old_state_on_web`
9. [app] A second window of the same account can run the ceremony too — it stays up, shows you the new kit and comes back as the successor, and its offer to finish the group re-pointing survives — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_a_bound_instance_survives_its_own_succession`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_a_bound_instance_survives_its_own_succession`
   - `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_a_bound_instance_survives_its_own_succession`
   - `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_a_bound_instance_survives_its_own_succession`
   - `tests/e2e-unified/tests/test_succession_second_tab_web.py::test_web_a_second_tab_runs_the_ceremony_and_comes_back_as_the_successor`
10. [app] A second-window shortcut created before the recovery still opens your account afterwards — the launch follows your identity's chain to where the account is now — `docs/goal/architecture/apps/account-scoping.md` § Concurrent instances
   - `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_a_launch_bound_to_a_retired_id_comes_up_as_the_successor`
   - `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_a_launch_bound_to_a_retired_id_comes_up_as_the_successor`
   - `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_a_launch_bound_to_a_retired_id_comes_up_as_the_successor`
   - `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_a_launch_bound_to_a_retired_id_comes_up_as_the_successor`
   - `tests/e2e-unified/tests/test_succession_second_tab_web.py::test_web_a_tab_pinned_to_the_retired_id_comes_up_as_the_successor`
11. [app] After you take your account back you are handed a fresh recovery kit straight away, without asking for one — `docs/goal/behavior/identity-succession.md` § The RecoveryKey
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_kit_holder_takes_the_account_back_and_comes_back_up_as_the_successor`
12. [app] You can watch your account come back part by part, and a part only another of your devices can finish says so rather than reading as damage — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_backups_restart_without_a_user_command`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_conversations_unlock_without_a_user_command`
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_mail_passwords_are_burned`
13. [nest] Your account comes back under the new key at the same handle, keeping your tier and any admin role — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
    - `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_the_account_comes_back_at_the_same_handle_with_tier_and_admin_role`
14. [app] Mail rules that existed before the recovery are flagged for you to keep or remove, and you are told where to find them — `docs/goal/behavior/succession-aftermath.md` § Adjudicating what the aftermath carries across
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_mail_rules_from_before_the_recovery_are_flagged_to_keep_or_remove`
15. [app] Each person the recovery cannot vouch for is offered to you to keep or remove, and anything you put off waits for you in one place — `docs/goal/behavior/succession-aftermath.md` § Adjudicating what the aftermath carries across
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_first_session_raises_the_member_it_cannot_vouch_for`
16. [app] After you take your account back you can still edit your profile, and your name, bio, picture and links come with the account — `docs/goal/ui/profile.md` § After an identity succession, the successor RE-PUBLISHES; it does not rewrite
   - `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_a_device_that_never_held_the_predecessor_saves_a_profile_edit`
   - `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_a_profile_edit_still_saves_after_the_retired_account_is_removed`
17. [app] If your account moves but the new key cannot be stored on this device, the app shows you that key with what to do, and nothing else on the page wipes it before you leave — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_key_this_device_cannot_store_stays_on_screen_until_you_leave`
18. [app] If the connection drops while the account is being taken back, you are told plainly that the result is unknown and how to get back in — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_reply_you_cannot_check_says_so_and_reopening_signs_you_in`
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_submit_reply_still_brings_you_back_as_the_successor`
19. [nest] Messages and posts still signed with the stolen key are refused, so whoever holds it can no longer send anything as you — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
    - `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_content_still_signed_with_the_superseded_key_is_refused`
20. [nest] Someone who only knows your old key, or your handle, is pointed at the account's new key — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
    - `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_an_old_key_or_handle_is_pointed_at_the_successor`
21. [app] If you lose every device after taking your account back, the phrase brings back the account and the data still locked to the old key — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
   - `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_after_a_succession_the_successors_own_phrase_restores_the_predecessor_corpus`
22. [app] When the ceremony finishes it tells you how many of your groups the old key was removed from and, as its own statement, which members it cannot vouch for — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_ceremony_tells_you_what_it_swept_and_whom_it_cannot_vouch_for`
23. [app] After the recovery people can still add you to conversations — an invitation reaches the new key instead of silently going nowhere — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_after_the_recovery_a_contact_can_still_add_you_to_a_conversation`
24. [app] Your subscription tiers, your subscribers and the posts they paid for come with the account, and what you publish afterwards cannot be read by whoever held the old key — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
    - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_subscriber_tier_is_re_keyed`
25. [nest] Apps you had authorized to act as you are disconnected by the recovery and must be authorized again — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
    - `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_the_recovery_disconnects_the_apps_you_had_authorized`
26. [nest] Taking your account back lands you in an account that is not locked, even when the old one was — `docs/goal/behavior/devices.md` § The two panic buttons
    - (none)
27. [nest] After you take your account back, the stolen key can no longer lock the account: the nest tells it where the account went instead — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+d034eec4.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_kit_holder_takes_the_account_back_and_comes_back_up_as_the_successor` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_confirm_gate_refuses_rather_than_dropping_the_command` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_succession_refusal.py::test_a_succeeded_device_is_refused_and_routed_to_import` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_ceremony_re_points_a_real_group_and_evicts_the_stolen_leaf` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_real_member_re_points_the_succeeded_participant_on_their_own_client` | web (linux): skipped, linux (linux): passed, windows (windows): skipped, macos (macos): skipped, ios (macos): skipped, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_succession_witness_registered.py::test_a_conversations_seat_registers_a_succession_witness` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_still_read_the_config_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_open_muted_words_page_paints_what_it_inherited` | web (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_read_the_corpus_it_inherited` | web (linux): failed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (windows): failed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_backups_restart_without_a_user_command` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_conversations_unlock_without_a_user_command` | web (linux): error, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_unsent_drafts_come_back` | linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_about_the_trust_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_about_the_destination_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_first_session_raises_the_member_it_cannot_vouch_for` | web (linux): failed, linux (linux): skipped, windows (windows): failed, macos (macos): failed, ios (macos): failed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_to_confirm_its_npub` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_dismiss_npub_confirm_into_a_fresh_key` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_mail_passwords_are_burned` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_identity_succession_mail_auth.py::test_a_succession_stops_the_predecessors_mail_password_authenticating` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_succession_fixture.py::test_the_fixture_succeeds_an_identity_and_the_nest_records_it` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_recovery_pre_identity_remedies.py::test_a_succession_lands_through_an_active_lockout_with_no_session` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_recovery_pre_identity_remedies.py::test_the_key_holder_vetoes_a_seed_alone_replacement_over_an_anonymous_connection` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_succession_lost_reply.py::test_a_dropped_submit_reply_fails_at_transport_while_the_succession_lands` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_succession_sweep_retry.py::test_a_half_done_sweep_renders_the_retry_and_the_press_is_honoured` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_succession_sweep_retry_web.py::test_a_half_done_sweep_answers_no_old_state_on_web` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_a_bound_instance_survives_its_own_succession` | tui (linux): passed, tui (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_a_bound_instance_survives_its_own_succession` | macos (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_a_bound_instance_survives_its_own_succession` | windows (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_a_bound_instance_survives_its_own_succession` | linux (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_succession_second_tab_web.py::test_web_a_second_tab_runs_the_ceremony_and_comes_back_as_the_successor` | web (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | tui (linux): passed, tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | macos (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | windows (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | linux (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_succession_second_tab_web.py::test_web_a_tab_pinned_to_the_retired_id_comes_up_as_the_successor` | web (linux): failed |
| 11 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_kit_holder_takes_the_account_back_and_comes_back_up_as_the_successor` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_backups_restart_without_a_user_command` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_conversations_unlock_without_a_user_command` | web (linux): error, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_mail_passwords_are_burned` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_the_account_comes_back_at_the_same_handle_with_tier_and_admin_role` | nest (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_mail_rules_from_before_the_recovery_are_flagged_to_keep_or_remove` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): skipped |
| 15 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_first_session_raises_the_member_it_cannot_vouch_for` | web (linux): failed, linux (linux): skipped, windows (windows): failed, macos (macos): failed, ios (macos): failed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_a_device_that_never_held_the_predecessor_saves_a_profile_edit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_a_profile_edit_still_saves_after_the_retired_account_is_removed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_key_this_device_cannot_store_stays_on_screen_until_you_leave` | web (linux): skipped, linux (linux): skipped, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_reply_you_cannot_check_says_so_and_reopening_signs_you_in` | web (linux): failed, linux (linux): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_submit_reply_still_brings_you_back_as_the_successor` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_content_still_signed_with_the_superseded_key_is_refused` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_an_old_key_or_handle_is_pointed_at_the_successor` | nest (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_after_a_succession_the_successors_own_phrase_restores_the_predecessor_corpus` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_ceremony_tells_you_what_it_swept_and_whom_it_cannot_vouch_for` | web (linux): skipped, linux (linux): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_after_the_recovery_a_contact_can_still_add_you_to_a_conversation` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_subscriber_tier_is_re_keyed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_the_recovery_disconnects_the_apps_you_had_authorized` | nest (linux): passed |
| 26 | nest | (none) | — |
| 27 | nest | (none) | — |
<!-- features-render:end -->

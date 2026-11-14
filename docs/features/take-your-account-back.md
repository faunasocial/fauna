---
slug: take-your-account-back
title: Take your account back
section: your data and devices
goal: docs/goal/behavior/identity-succession.md § Goal
guide: docs/guides/identity-and-devices.md § If a device is lost or stolen
absences:
  ios (outcomes 9, 10): "docs/goal/architecture/apps/account-scoping.md § Concurrent instances"
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

If your identity key is stolen, the recovery kit moves your account to a fresh
identity the thief cannot follow: the old key stops working, at your nest at
once and at each other nest your account is linked to once one of your devices
next syncs with it; your groups move with you (and if some do not, you can
finish moving them yourself); and any device still on the old key is told what
happened and asked to import the new one. Your data, backups, conversations,
drafts and mail come back under the new key, the old mail passwords stop
working, and anything the thief could have changed is raised for your review.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

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
   - `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_web_learns_the_predecessor_link_only_under_a_sibling_tabs_registry_lock`
17. [app] If your account moves but the new key cannot be stored on this device, the app shows you that key with what to do, and nothing else on the page wipes it before you leave — `docs/goal/ui/settings.md` § Recovery kit
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_key_this_device_cannot_store_stays_on_screen_until_you_leave`
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_reply_you_cannot_check_or_store_keeps_the_only_key_on_screen`
18. [app] If the connection drops while the account is being taken back, you are told plainly that the result is unknown and how to get back in — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_reply_you_cannot_check_says_so_and_reopening_signs_you_in`
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_submit_reply_still_brings_you_back_as_the_successor`
   - `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_relaunch_adoption_writes_its_link_only_under_a_sibling_tabs_registry_lock`
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
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_taking_the_account_back_lands_in_an_account_that_is_not_locked`
27. [nest] After you take your account back, the stolen key can no longer lock the account: the nest tells it where the account went instead — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
    - `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_after_taking_it_back_the_stolen_key_cannot_lock_the_account_and_is_told_where_it_went`
28. [app] After you take your account back, the stolen key also stops signing in at every other nest you had linked, as soon as one of your devices reaches that nest — `docs/goal/behavior/succession-linked-nests.md` § Every nest the identity is linked to
    - `tests/e2e-unified/tests/test_identity_succession_refusal.py::test_the_successors_device_retires_the_stolen_key_at_a_linked_nest`
29. [app] Linking two nests that hold different recovery keys for you is refused, and you are told why — `docs/goal/behavior/succession-linked-nests.md` § Every nest the identity is linked to
    - `tests/e2e-unified/tests/test_linked_nests.py::test_link_refuses_two_nests_holding_different_recovery_keys`
30. [nest] A nest accepts a recovery only when it checks out against the records that nest holds itself; one relayed from another nest is refused — `docs/goal/behavior/succession-linked-nests.md` § Every nest the identity is linked to
    - `tests/e2e-unified/tests/api/test_succession_linked_nests.py::test_a_nest_accepts_a_recovery_only_against_its_own_records`
31. [nest] A linked nest the recovery has not reached yet stays owed until one of your devices gets through to it — `docs/goal/behavior/succession-linked-nests.md` § Every nest the identity is linked to
    - `tests/e2e-unified/tests/api/test_succession_linked_nests.py::test_a_linked_nest_the_recovery_has_not_reached_stays_owed_until_a_device_gets_through`
32. [app] After you take your account back, a Nostr app the stolen key had connected shows on Connected apps as revoked, with its own line saying so, never as an ordinary disconnected app — `docs/goal/ui/nostr.md` § Key succession and rotation
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_burned_signer_connection_reads_revoked`
33. [nest] After the account you took back is deleted, the stolen key still cannot sign up, ask for an invite or claim admin on that nest — `docs/goal/architecture/account-data-plane.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_a_retired_key_is_refused_at_every_registration_door_after_its_successor_is_deleted`
34. [app] A version of a file saved before you took your account back is still listed in its version history and restores — `docs/goal/architecture/succession-cut.md` § Implementation status today
   - `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_read_the_corpus_it_inherited`
35. [app] Files added to a shared folder you inherited after the recovery cannot be opened with the stolen key, while the folder's other members still read them — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
   - `tests/e2e-unified/tests/test_identity_succession_shared_folder_rekey.py::test_a_file_added_after_the_recovery_does_not_open_under_the_stolen_key`
36. [nest] Data a connected app kept under your account comes with it, and an app you authorize again finds it where it was — `docs/goal/behavior/succession-aftermath.md` § Re-key scope
   - `tests/e2e-unified/tests/api/test_succession_connected_app_carry.py::test_a_connected_apps_data_comes_with_the_account_and_the_app_authorized_again_finds_it`
37. [app] A keep or remove you answer on one device is settled on your other devices too — `docs/goal/behavior/succession-aftermath.md` § Adjudicating what the aftermath carries across
   - `tests/e2e-unified/tests/test_identity_succession_cross_device.py::test_a_keep_on_one_device_is_settled_on_the_other`
38. [app] A recovery opened at any nest you are linked to raises the alarm on your devices, and one veto clears it at every nest — `docs/goal/behavior/succession-linked-nests.md` § Every nest the identity is linked to
   - `tests/e2e-unified/tests/test_recovery_linked_nest_alarm.py::test_a_recovery_opened_at_a_linked_nest_raises_the_alarm_and_one_veto_clears_it_at_both`
39. [app] Someone you had blocked stays blocked after the recovery, whatever the thief changed — `docs/goal/behavior/succession-propagation.md` § Propagation
   - `tests/e2e-unified/tests/test_identity_succession_carryover.py::test_someone_you_blocked_stays_blocked_after_the_recovery`
40. [nest] Anything the thief scheduled under your account — a handle change, a deletion — is cancelled by the recovery — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_what_the_thief_scheduled_is_cancelled_by_the_recovery`
41. [nest] Mail and requests addressed to your old identity reach you as its successor instead of going nowhere — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_mail_and_requests_to_the_old_identity_reach_the_successor`
42. [app] Subscriptions you pay for as a reader come with the account — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/test_identity_succession_carryover.py::test_a_readers_subscriptions_come_with_the_account`
43. [nest] Your payment-provider connections, device authorizations and unredeemed membership codes from before the recovery are burned — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_payment_connections_device_authorizations_and_unredeemed_codes_are_burned`
44. [nest] The devices that held the stolen key stop receiving your notifications, and the device you recover on signs up for them again — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_the_stolen_keys_push_subscriptions_are_burned_and_the_successor_subscribes`
45. [app] You can delete posts you made before the recovery — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/test_identity_succession_carryover.py::test_posts_from_before_the_recovery_can_still_be_deleted`
46. [nest] A share link minted before the recovery but never registered stops working, and an export link from before is burned — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_an_unregistered_share_link_from_before_the_recovery_stops_working`
   - `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_an_export_link_from_before_the_recovery_is_burned`
47. [nest] Your mailing lists, custom feeds and labeler subscriptions stay yours, and the anti-abuse meters are not reset — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_lists_feeds_and_labeler_subscriptions_stay_yours_and_the_meter_is_not_reset`
48. [nest] Your fediverse identity carries on under your handle, while bridges to outside networks the account had set up are cut — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_bridge_planes.py::test_the_fediverse_identity_carries_on_under_the_same_handle`
   - `tests/e2e-unified/tests/api/test_succession_bridge_planes.py::test_a_bridge_to_an_outside_network_the_account_set_up_is_cut`
49. [nest] The calendars and contacts your standard apps used come with the account; their app passwords stop working — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
   - `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_calendars_and_contacts_come_with_the_account_and_their_app_passwords_stop_working`
50. [app] If another holder of the same kit already took the account the ceremony says so as final, never as try again, and a kit belonging to a different account is refused before anything is minted — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
   - `tests/e2e-unified/tests/test_identity_succession_two_holders.py::test_when_another_holder_of_the_kit_got_there_first_the_ceremony_says_so_as_final`
   - `tests/e2e-unified/tests/test_identity_succession_two_holders.py::test_a_kit_of_another_account_is_refused_before_anything_moves`
   - `tests/e2e-unified/tests/test_identity_succession_two_holders.py::test_a_kit_naming_another_account_says_so_rather_than_calling_it_stale`
51. [nest] The first succession wins: a later holder of the old kit cannot re-point the account away, and an admin's suspension carries to the successor — `docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
   - `tests/e2e-unified/tests/api/test_succession_first_wins.py::test_a_later_holder_of_the_old_kit_cannot_re_point_the_account_away`
   - `tests/e2e-unified/tests/api/test_succession_first_wins.py::test_an_admins_suspension_carries_to_the_successor`
52. [app] Something you did while offline before taking your account back — a like you gave — still goes out afterwards, once, under the new key — `docs/goal/behavior/succession-aftermath.md` § Adjudicating what the aftermath carries across
   - `tests/e2e-unified/tests/test_identity_succession_outbox_carry.py::test_a_like_queued_before_the_recovery_lands_once_as_the_successor`
   - `tests/e2e-unified/tests/test_identity_succession_outbox_carry.py::test_a_message_queued_before_the_recovery_is_delivered_once_after_it`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+3b591c2e standalone |
| linux | ✅ full | 0.1.5-dev+3b591c2e standalone |
| windows | ⚠ partial | 0.1.5-dev+3b591c2e standalone |
| macos | ✅ full | 0.1.5-dev+3b591c2e standalone |
| ios | ✅ full | 0.1.5-dev+3b591c2e standalone |
| android | 🚧 unbuilt | 0.1.5-dev+3b591c2e standalone |
| tui | ⚠ partial | 0.1.5-dev+3b591c2e standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_kit_holder_takes_the_account_back_and_comes_back_up_as_the_successor` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_confirm_gate_refuses_rather_than_dropping_the_command` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_identity_succession_refusal.py::test_a_succeeded_device_is_refused_and_routed_to_import` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_ceremony_re_points_a_real_group_and_evicts_the_stolen_leaf` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_real_member_re_points_the_succeeded_participant_on_their_own_client` — requires `succession-witness-member-seat` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_succession_witness_registered.py::test_a_conversations_seat_registers_a_succession_witness` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_still_read_the_config_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_open_muted_words_page_paints_what_it_inherited` | web (linux): passed, linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_read_the_corpus_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_backups_restart_without_a_user_command` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_conversations_unlock_without_a_user_command` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_unsent_drafts_come_back` | linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_about_the_trust_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_about_the_destination_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_first_session_raises_the_member_it_cannot_vouch_for` — requires `member-review-defer-button` (not built: android, android-kids) | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_is_asked_to_confirm_its_npub` | web (linux): passed, linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_dismiss_npub_confirm_into_a_fresh_key` | web (linux): passed, linux (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_mail_passwords_are_burned` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_identity_succession_mail_auth.py::test_a_succession_stops_the_predecessors_mail_password_authenticating` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_succession_fixture.py::test_the_fixture_succeeds_an_identity_and_the_nest_records_it` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_recovery_pre_identity_remedies.py::test_a_succession_lands_through_an_active_lockout_with_no_session` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_recovery_pre_identity_remedies.py::test_the_key_holder_vetoes_a_seed_alone_replacement_over_an_anonymous_connection` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_succession_lost_reply.py::test_a_dropped_submit_reply_fails_at_transport_while_the_succession_lands` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_succession_sweep_retry.py::test_a_half_done_sweep_renders_the_retry_and_the_press_is_honoured` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_a_bound_instance_survives_its_own_succession` | tui (linux): passed, tui (macos): failed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_a_bound_instance_survives_its_own_succession` | macos (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_a_bound_instance_survives_its_own_succession` | windows (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_a_bound_instance_survives_its_own_succession` | linux (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_succession_second_tab_web.py::test_web_a_second_tab_runs_the_ceremony_and_comes_back_as_the_successor` | web (linux): passed |
| 9 | app | absent by design on ios | — |
| 10 | app | `tests/e2e-unified/tests/test_account_instance_lock_tui.py::test_tui_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_switcher_apple.py::test_apple_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | macos (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_instance_lock_windows.py::test_windows_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | windows (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_account_instance_lock_linux.py::test_linux_a_launch_bound_to_a_retired_id_comes_up_as_the_successor` | linux (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_succession_second_tab_web.py::test_web_a_tab_pinned_to_the_retired_id_comes_up_as_the_successor` | web (linux): passed |
| 10 | app | absent by design on ios | — |
| 11 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_kit_holder_takes_the_account_back_and_comes_back_up_as_the_successor` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_backups_restart_without_a_user_command` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_conversations_unlock_without_a_user_command` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_mail_passwords_are_burned` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_the_account_comes_back_at_the_same_handle_with_tier_and_admin_role` | nest (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_mail_rules_from_before_the_recovery_are_flagged_to_keep_or_remove` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_first_session_raises_the_member_it_cannot_vouch_for` — requires `member-review-defer-button` (not built: android, android-kids) | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_a_device_that_never_held_the_predecessor_saves_a_profile_edit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_a_profile_edit_still_saves_after_the_retired_account_is_removed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_profile_linkless_successor.py::test_web_learns_the_predecessor_link_only_under_a_sibling_tabs_registry_lock` | web (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_key_this_device_cannot_store_stays_on_screen_until_you_leave` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 17 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_reply_you_cannot_check_or_store_keeps_the_only_key_on_screen` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 18 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_reply_you_cannot_check_says_so_and_reopening_signs_you_in` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 18 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_a_lost_submit_reply_still_brings_you_back_as_the_successor` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 18 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_relaunch_adoption_writes_its_link_only_under_a_sibling_tabs_registry_lock` | web (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_content_still_signed_with_the_superseded_key_is_refused` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_an_old_key_or_handle_is_pointed_at_the_successor` | nest (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_recovery_kit_restore.py::test_after_a_succession_the_successors_own_phrase_restores_the_predecessor_corpus` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 22 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_the_ceremony_tells_you_what_it_swept_and_whom_it_cannot_vouch_for` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 23 | app | `tests/e2e-unified/tests/test_identity_succession_ceremony.py::test_after_the_recovery_a_contact_can_still_add_you_to_a_conversation` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 24 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_subscriber_tier_is_re_keyed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_the_recovery_disconnects_the_apps_you_had_authorized` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_taking_the_account_back_lands_in_an_account_that_is_not_locked` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_sign_in_ending_and_lockout.py::test_after_taking_it_back_the_stolen_key_cannot_lock_the_account_and_is_told_where_it_went` | nest (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_identity_succession_refusal.py::test_the_successors_device_retires_the_stolen_key_at_a_linked_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 29 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_link_refuses_two_nests_holding_different_recovery_keys` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_succession_linked_nests.py::test_a_nest_accepts_a_recovery_only_against_its_own_records` | nest (linux): passed |
| 31 | nest | `tests/e2e-unified/tests/api/test_succession_linked_nests.py::test_a_linked_nest_the_recovery_has_not_reached_stays_owed_until_a_device_gets_through` | nest (linux): passed |
| 32 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_burned_signer_connection_reads_revoked` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 33 | nest | `tests/e2e-unified/tests/api/test_succession_nest_enforcement.py::test_a_retired_key_is_refused_at_every_registration_door_after_its_successor_is_deleted` | nest (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successor_can_read_the_corpus_it_inherited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 35 | app | `tests/e2e-unified/tests/test_identity_succession_shared_folder_rekey.py::test_a_file_added_after_the_recovery_does_not_open_under_the_stolen_key` — requires `succession-witness-member-seat` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): failed |
| 36 | nest | `tests/e2e-unified/tests/api/test_succession_connected_app_carry.py::test_a_connected_apps_data_comes_with_the_account_and_the_app_authorized_again_finds_it` | nest (linux): passed |
| 37 | app | `tests/e2e-unified/tests/test_identity_succession_cross_device.py::test_a_keep_on_one_device_is_settled_on_the_other` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 38 | app | `tests/e2e-unified/tests/test_recovery_linked_nest_alarm.py::test_a_recovery_opened_at_a_linked_nest_raises_the_alarm_and_one_veto_clears_it_at_both` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 39 | app | `tests/e2e-unified/tests/test_identity_succession_carryover.py::test_someone_you_blocked_stays_blocked_after_the_recovery` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 40 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_what_the_thief_scheduled_is_cancelled_by_the_recovery` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_mail_and_requests_to_the_old_identity_reach_the_successor` | nest (linux): passed |
| 42 | app | `tests/e2e-unified/tests/test_identity_succession_carryover.py::test_a_readers_subscriptions_come_with_the_account` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 43 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_payment_connections_device_authorizations_and_unredeemed_codes_are_burned` | nest (linux): passed |
| 44 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_axis.py::test_the_stolen_keys_push_subscriptions_are_burned_and_the_successor_subscribes` | nest (linux): passed |
| 45 | app | `tests/e2e-unified/tests/test_identity_succession_carryover.py::test_posts_from_before_the_recovery_can_still_be_deleted` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 46 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_an_unregistered_share_link_from_before_the_recovery_stops_working` | nest (linux): passed, nest (macos): passed |
| 46 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_an_export_link_from_before_the_recovery_is_burned` | nest (linux): passed, nest (macos): passed |
| 47 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_lists_feeds_and_labeler_subscriptions_stay_yours_and_the_meter_is_not_reset` | nest (linux): passed, nest (macos): passed |
| 48 | nest | `tests/e2e-unified/tests/api/test_succession_bridge_planes.py::test_the_fediverse_identity_carries_on_under_the_same_handle` | nest (linux): passed, nest (macos): passed |
| 48 | nest | `tests/e2e-unified/tests/api/test_succession_bridge_planes.py::test_a_bridge_to_an_outside_network_the_account_set_up_is_cut` | nest (linux): passed, nest (macos): passed |
| 49 | nest | `tests/e2e-unified/tests/api/test_succession_repoint_planes.py::test_calendars_and_contacts_come_with_the_account_and_their_app_passwords_stop_working` | nest (linux): passed, nest (macos): passed |
| 50 | app | `tests/e2e-unified/tests/test_identity_succession_two_holders.py::test_when_another_holder_of_the_kit_got_there_first_the_ceremony_says_so_as_final` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 50 | app | `tests/e2e-unified/tests/test_identity_succession_two_holders.py::test_a_kit_of_another_account_is_refused_before_anything_moves` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 50 | app | `tests/e2e-unified/tests/test_identity_succession_two_holders.py::test_a_kit_naming_another_account_says_so_rather_than_calling_it_stale` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 51 | nest | `tests/e2e-unified/tests/api/test_succession_first_wins.py::test_a_later_holder_of_the_old_kit_cannot_re_point_the_account_away` | nest (linux): passed |
| 51 | nest | `tests/e2e-unified/tests/api/test_succession_first_wins.py::test_an_admins_suspension_carries_to_the_successor` | nest (linux): passed |
| 52 | app | `tests/e2e-unified/tests/test_identity_succession_outbox_carry.py::test_a_like_queued_before_the_recovery_lands_once_as_the_successor` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 52 | app | `tests/e2e-unified/tests/test_identity_succession_outbox_carry.py::test_a_message_queued_before_the_recovery_is_delivered_once_after_it` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->

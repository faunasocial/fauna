---
slug: family-safety
title: Supervised accounts for children
section: family and personalization
goal: docs/goal/behavior/family-safety.md § Goal
guide: docs/guides/cloud-sharing-family.md § A family on one nest
---

## What a user gets

A guardian supervises a child's account: sees it on the Family page, decides
whether unknown people may message the child, approves the contacts the child asks
for, blocks content categories on the child's feed and conversations, hears how
often that fired, sets screen-time windows and a daily budget, and marks the
child's devices. Guardianship can be handed to another adult who must agree first.
Mail to a supervised child is gated at the nest.

## Coverage contract

Stamped 2026-10-01 at e5a7e5d758.

1. [app] A guardian sees their child and the child sees they are supervised — `docs/goal/behavior/family-safety.md` § The guardianship link
   - `tests/e2e-unified/tests/test_family.py::test_family_admission_and_ward_view`
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_sees_ward`
2. [app] Guardianship transfers only when the other adult accepts, and a decline leaves it as it was — `docs/goal/behavior/family-safety.md` § Graduation & transfer
   - `tests/e2e-unified/tests/test_family.py::test_family_transfer_accept_journey`
   - `tests/e2e-unified/tests/test_family.py::test_family_transfer_decline_journey`
3. [app] The guardian chooses whether a conversation from an unknown person on a linked network is held for the guardian to approve, and the child asks for contacts in-app — `docs/goal/behavior/family-safety.md` § Child-initiated contact requests
   - `tests/e2e-unified/tests/test_family.py::test_family_bridge_dm_knob_round_trips`
   - `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_cold_peers_bridged_room_is_marked_held_for_a_ward_and_stays_readable`
   - `tests/e2e-unified/tests/test_family.py::test_family_ward_asks_guardian_for_a_contact`
4. [app] A content category the guardian blocks is hidden on the child's feed and conversations, and the guardian hears how often — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_content_floor_blocks_flagged_feed_post`
   - `tests/e2e-unified/tests/test_family.py::test_family_content_floor_binds_on_ws_reconnect_without_relaunch`
   - `tests/e2e-unified/tests/test_family.py::test_family_content_floor_blocks_flagged_conversation_message`
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_notify_surfaces_flagged_count`
   - `tests/e2e-unified/tests/test_family_notify_actor_switch.py::test_actor_switch_mid_accrual_does_not_leak_the_outgoing_wards_pending_notify_count`
5. [app] Screen-time windows lock the child's app outside them, and a daily budget shows its use on both sides — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_window_lock`
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_and_usage_readouts`
6. [app] A guardian marks a child's device, the child sees the mark, and cannot remove the device — `docs/goal/behavior/family-safety.md` § Full visibility for young children
   - `tests/e2e-unified/tests/test_family.py::test_family_device_marker_badge_and_ward_delete_refusal`
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_device_mark_toggle`
7. [nest] Mail to a supervised child from a stranger is refused or held, and a bounce cannot be used to sneak past — `docs/goal/behavior/family-safety.md` § The mail gate
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_reject_ward_refuses_cold_sender_adult_still_receives`
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_hold_ward_null_path_nondsn_held`
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_genuine_bounce_delivers_forged_report_held`
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_reply_to_correlated_report_never_allowlists_and_mint_verifies`
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_correlation_budget_exhaustion_holds`
8. [app] The admin sees whether a join request carried an app age check and admits the child at a band; the guardian sees each child's band on their row and the child sees their own, with how it was set — and an account admitted without one shows nothing — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_pending_invite_journey.py::test_admin_admits_a_request_at_a_band_the_row_shows_the_claim`
   - `tests/e2e-unified/tests/test_family.py::test_family_age_band_readouts`
9. [app] The child asks the guardian for a source they were blocked from adding, and an approval lets exactly one retry through — `docs/goal/behavior/family-safety.md` § Feed-source approvals
   - `tests/e2e-unified/tests/test_bridges.py::test_family_ward_feed_source_ask_redeems_once`
10. [nest] When a guardian graduates a child, the account carries on as an ordinary one with the same handle and data, and anything still waiting for the guardian is handed to the child — `docs/goal/behavior/family-safety.md` § Graduation & transfer
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_graduation_keeps_the_account_and_hands_held_mail_to_the_child`
11. [app] A guardian sees a hand-over they proposed waiting for the other adult, and can withdraw it — `docs/goal/behavior/family-safety.md` § Graduation & transfer
   - `tests/e2e-unified/tests/test_family.py::test_family_transfer_withdraw_journey`
12. [app] With contact approval on, a stranger's request to contact the child waits for the guardian, who accepts or refuses it for the child — `docs/goal/behavior/family-safety.md` § Guardian policy — the enforcement split
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_decides_a_strangers_contact_request`
13. [app] While a stranger's request waits for the guardian, the child sees it marked as waiting for the guardian's approval — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_family.py::test_family_ward_sees_a_strangers_request_waiting_for_the_guardian`
14. [nest] With contact from other nests switched off, people on other nests cannot start contact with the child, while contacts already accepted there keep getting through — `docs/goal/behavior/family-safety.md` § Guardian policy — the enforcement split
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_federation_contact_off_turns_away_strangers_on_other_nests_but_not_accepted_contacts`
15. [nest] With contact approval on, an invitation into a conversation from someone not approved does not reach the child — `docs/goal/behavior/family-safety.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_contact_approval_keeps_an_unapproved_persons_conversation_invitation_from_the_child`
16. [app] The guardian releases or discards each held mail; a release puts it in the child's inbox and lets that sender's later mail through — `docs/goal/behavior/family-safety.md` § Reach approvals
   - `tests/e2e-unified/tests/test_family_mail_hold.py::test_family_guardian_releases_and_discards_held_mail_seeing_only_sender_and_time`
17. [app] The guardian sees only who sent a held mail and when, never what it says — `docs/goal/behavior/family-safety.md` § Guardian policy — the enforcement split
   - `tests/e2e-unified/tests/test_family_mail_hold.py::test_family_guardian_releases_and_discards_held_mail_seeing_only_sender_and_time`
18. [nest] The child can read mail that is held for the guardian, but cannot move it or delete it while it is held — `docs/goal/behavior/family-safety.md` § The mail gate
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_ward_reads_held_mail_but_cannot_move_or_delete_it`
19. [nest] Someone on the same nest who mails a child that accepts only known senders is told the mail needs the guardian's approval, and nothing is delivered — `docs/goal/behavior/family-safety.md` § Guardian policy — the enforcement split
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_same_nest_sender_is_told_the_mail_needs_guardian_approval`
20. [nest] Once the guardian refuses someone who messaged the child through a linked account on another network, that person's new messages never arrive — `docs/goal/behavior/family-safety.md` § The bridge-DM gate
   - `tests/e2e-unified/tests/api/test_family_bridge_dm_gate.py::test_a_bridged_peer_the_guardian_refused_never_gets_another_message_through`
21. [app] When the guardian lets a held conversation from a linked account on another network through, it opens and that person's later messages arrive — `docs/goal/behavior/family-safety.md` § The bridge-DM gate
   - `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_guardian_lets_a_held_bridged_conversation_through_and_later_messages_arrive`
22. [app] The guardian gets a notification when the child asks for a contact — `docs/goal/behavior/family-safety.md` § Child-initiated contact requests
   - `tests/e2e-unified/tests/test_family.py::test_family_ward_asks_guardian_for_a_contact`
23. [nest] A guardian's no to a contact the child asked for does not block that person, and the child can ask again — `docs/goal/behavior/family-safety.md` § Child-initiated contact requests
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_guardians_no_to_a_contact_ask_blocks_nobody_and_the_child_can_ask_again`
24. [nest] A supervised child cannot delete their own account — `docs/goal/behavior/family-safety.md` § Lifecycle gates
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervised_child_cannot_delete_their_own_account`
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervising_account_cannot_be_removed_until_its_child_is_graduated`
25. [nest] An account that supervises someone cannot be removed until each child is handed over or graduated, and the refusal names those children — `docs/goal/behavior/family-safety.md` § Lifecycle gates
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervising_account_cannot_be_removed_until_its_child_is_graduated`
26. [nest] Taking back a lost account keeps the family link as it was, whether the child or the guardian lost theirs — `docs/goal/behavior/family-safety.md` § Lifecycle gates
   - `tests/e2e-unified/tests/api/test_family_succession.py::test_taking_back_a_lost_account_keeps_the_family_link_on_either_side`
27. [nest] A supervised child can still change their own plan — `docs/goal/behavior/family-safety.md` § Lifecycle gates
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervised_child_can_still_change_their_own_plan`
28. [nest] A child admitted at an age band starts with that band's suggested settings, which the guardian can then change — `docs/goal/behavior/family-safety.md` § The account age band
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_child_admitted_at_an_age_band_starts_on_that_bands_settings`
29. [app] Before a supervised invite code is redeemed, the person joining is told the age band the account will have — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_family.py::test_family_supervised_notice_names_the_codes_age_band`
30. [app] A join request that carried an app age check shows the band it claimed, and the admin's band choice starts there — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_pending_invite_journey.py::test_a_requests_claimed_age_band_shows_and_the_band_select_starts_there`
31. [app] The Family page appears only for someone who supervises, is supervised, or has a hand-over waiting for them — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_family.py::test_family_page_hidden_for_an_unrelated_account`
   - `tests/e2e-unified/tests/test_family.py::test_family_transfer_withdraw_journey`
32. [app] A content category the guardian sets to collapse is folded away on the child's feed and conversations, and the child can still choose to show it — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_collapse_floor_folds_content_the_child_can_still_show`
33. [app] The child can make their own content filter stricter than the guardian's, never looser — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_ward_can_make_their_own_filter_stricter_than_the_guardians_never_looser`
34. [app] The child's own read-only summary lists each content category the guardian set — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_ward_summary_lists_content_floors_and_notify`
35. [app] The guardian hears about hidden content only after turning that on, and the child's summary shows whether it is on — `docs/goal/behavior/family-client-enforcement.md` § Guardian Notify
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_hears_nothing_until_notify_is_on`
   - `tests/e2e-unified/tests/test_family.py::test_family_ward_summary_lists_content_floors_and_notify`
36. [app] The guardian gets one notification per child, per category, per day when hidden content was seen, and opening it leads to the Family page — `docs/goal/behavior/family-client-enforcement.md` § Guardian Notify
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_notify_surfaces_flagged_count`
37. [nest] The guardian is told only the category and how many times, never what the content was or which item — `docs/goal/behavior/family-client-enforcement.md` § Guardian Notify
   - `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_the_guardian_is_told_only_the_category_and_the_count`
38. [app] Only what the guardian's own limits hid is counted for the guardian; what the child's own stricter settings hid is never reported — `docs/goal/behavior/family-client-enforcement.md` § Implementation status today
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_notify_counts_only_what_the_guardians_limits_hid`
39. [app] The child's content limits and screen-time lock stay in force when the app starts or reconnects without reaching the nest — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_limits_and_lock_hold_while_the_app_cannot_reach_the_nest`
40. [app] The child's daily screen-time budget counts use on all their devices together — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_counts_use_on_every_device_of_the_ward`
41. [app] The daily budget starts over at the child's local midnight — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_starts_over_at_the_wards_local_midnight`
42. [app] The guardian removes a screen-time limit by leaving its field empty, and a budget of zero keeps the child's app locked all day — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_empty_field_clears_and_zero_budget_locks`
43. [app] A screen-time window that is half filled in, or starts and ends at the same time, is refused with the reason — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_editor_refuses_half_or_empty_window`
44. [app] The guardian's editor says these limits are enforced by the apps on the child's own devices — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_editor_states_screen_time_is_enforced_on_the_childs_devices`
45. [app] The guardian's editor says which content categories cannot be detected yet, so a limit on them waits until they can — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_editor_states_which_content_categories_are_not_detected_yet`
46. [app] Before joining with a supervised invite code, the person joining is told who will supervise the account — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_family.py::test_family_admission_and_ward_view`
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_sees_ward`
47. [app] The guardian sees the people they refused from a linked account on another network, and can let any one of them through again — `docs/goal/behavior/family-safety.md` § The bridge-DM gate
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_un_denies_the_second_blocked_peer`
48. [app] After the child's app restarts, it shows who supervises them straight away, before it has heard from the nest — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_supervised_indicator_shows_restored_guardian_while_status_is_pending`
49. [app] A content limit the guardian sets while the child's app is open takes effect when the app next reconnects, without restarting it — `docs/goal/behavior/family-client-enforcement.md` § Content policy
   - `tests/e2e-unified/tests/test_family.py::test_family_content_floor_binds_on_ws_reconnect_without_relaunch`
50. [app] While the child's app is locked, it says which guardian set the limit, and the Family page stays open to read — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_window_lock`
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_and_usage_readouts`
51. [app] When the child has used up the day's budget, their app locks and says the budget ran out — `docs/goal/behavior/family-client-enforcement.md` § Screen time
   - `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_and_usage_readouts`
52. [nest] When strangers' mail to a child is refused, the sender is told only that the address does not exist, so the refusal does not reveal a supervised account — `docs/goal/behavior/family-safety.md` § The mail gate
   - `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_reject_ward_refuses_cold_sender_adult_still_receives`
53. [app] A guardian limits or turns off a feature for one child from the child's policy screen, the child's own row shows it binding with the guardian named, and a looser value than what already applies says it has no effect — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_feature_limits.py::test_a_guardian_limits_a_feature_for_one_child_and_the_child_sees_who_set_it`
54. [app] An admin directly admitting a child picks the guardian beside the tier, and the child's account is supervised by that guardian from the start — `docs/goal/behavior/family-safety.md` § The guardianship link
   - `tests/e2e-unified/tests/test_admin_users_admit.py::test_admit_with_a_guardian_yields_a_supervised_ward`
55. [app] An admin directly admitting a child can also pick the child's age band beside the guardian, and the child's account carries that band from the start — `docs/goal/behavior/family-safety.md` § App surface
   - `tests/e2e-unified/tests/test_admin_users_admit.py::test_admit_with_a_guardian_and_band_records_the_band_on_the_ward`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+4baae748 docker |
| linux | ⚠ partial | 0.1.2-dev+4baae748 docker |
| windows | ⚠ partial | 0.1.2-dev+f9031bce standalone |
| macos | ⚠ partial | 0.1.2-dev+5893bba8 standalone |
| ios | ⚠ partial | 0.1.2-dev+5893bba8 standalone |
| android | ⚠ partial | 0.1.2-dev+4baae748 docker |
| tui | ✅ full | 0.1.2-dev+4baae748 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_family.py::test_family_admission_and_ward_view` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_sees_ward` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_family.py::test_family_transfer_accept_journey` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_family.py::test_family_transfer_decline_journey` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_family.py::test_family_bridge_dm_knob_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_cold_peers_bridged_room_is_marked_held_for_a_ward_and_stays_readable` | web (linux): passed, linux (linux): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_family.py::test_family_ward_asks_guardian_for_a_contact` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_family.py::test_family_content_floor_blocks_flagged_feed_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_family.py::test_family_content_floor_binds_on_ws_reconnect_without_relaunch` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_family.py::test_family_content_floor_blocks_flagged_conversation_message` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_notify_surfaces_flagged_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_family_notify_actor_switch.py::test_actor_switch_mid_accrual_does_not_leak_the_outgoing_wards_pending_notify_count` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_window_lock` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_and_usage_readouts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_family.py::test_family_device_marker_badge_and_ward_delete_refusal` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_device_mark_toggle` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_reject_ward_refuses_cold_sender_adult_still_receives` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_hold_ward_null_path_nondsn_held` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_genuine_bounce_delivers_forged_report_held` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_reply_to_correlated_report_never_allowlists_and_mint_verifies` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_correlation_budget_exhaustion_holds` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_pending_invite_journey.py::test_admin_admits_a_request_at_a_band_the_row_shows_the_claim` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_family.py::test_family_age_band_readouts` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_bridges.py::test_family_ward_feed_source_ask_redeems_once` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_graduation_keeps_the_account_and_hands_held_mail_to_the_child` | nest (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_family.py::test_family_transfer_withdraw_journey` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_decides_a_strangers_contact_request` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_family.py::test_family_ward_sees_a_strangers_request_waiting_for_the_guardian` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_federation_contact_off_turns_away_strangers_on_other_nests_but_not_accepted_contacts` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_contact_approval_keeps_an_unapproved_persons_conversation_invitation_from_the_child` | nest (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_family_mail_hold.py::test_family_guardian_releases_and_discards_held_mail_seeing_only_sender_and_time` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_family_mail_hold.py::test_family_guardian_releases_and_discards_held_mail_seeing_only_sender_and_time` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_ward_reads_held_mail_but_cannot_move_or_delete_it` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_same_nest_sender_is_told_the_mail_needs_guardian_approval` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_family_bridge_dm_gate.py::test_a_bridged_peer_the_guardian_refused_never_gets_another_message_through` | nest (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_guardian_lets_a_held_bridged_conversation_through_and_later_messages_arrive` | web (linux): passed, linux (linux): passed, ios (macos): passed, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_family.py::test_family_ward_asks_guardian_for_a_contact` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_guardians_no_to_a_contact_ask_blocks_nobody_and_the_child_can_ask_again` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervised_child_cannot_delete_their_own_account` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervising_account_cannot_be_removed_until_its_child_is_graduated` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervising_account_cannot_be_removed_until_its_child_is_graduated` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/api/test_family_succession.py::test_taking_back_a_lost_account_keeps_the_family_link_on_either_side` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_supervised_child_can_still_change_their_own_plan` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_a_child_admitted_at_an_age_band_starts_on_that_bands_settings` | nest (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_family.py::test_family_supervised_notice_names_the_codes_age_band` | web (linux): passed, linux (linux): passed, android (linux): error, tui (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_pending_invite_journey.py::test_a_requests_claimed_age_band_shows_and_the_band_select_starts_there` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_family.py::test_family_page_hidden_for_an_unrelated_account` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_family.py::test_family_transfer_withdraw_journey` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 32 | app | `tests/e2e-unified/tests/test_family.py::test_family_collapse_floor_folds_content_the_child_can_still_show` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 33 | app | `tests/e2e-unified/tests/test_family.py::test_family_ward_can_make_their_own_filter_stricter_than_the_guardians_never_looser` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_family.py::test_family_ward_summary_lists_content_floors_and_notify` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_hears_nothing_until_notify_is_on` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_family.py::test_family_ward_summary_lists_content_floors_and_notify` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_notify_surfaces_flagged_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 37 | nest | `tests/e2e-unified/tests/api/test_family_nest_witnesses.py::test_the_guardian_is_told_only_the_category_and_the_count` | nest (linux): passed |
| 38 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_notify_counts_only_what_the_guardians_limits_hid` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 39 | app | `tests/e2e-unified/tests/test_family.py::test_family_limits_and_lock_hold_while_the_app_cannot_reach_the_nest` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_counts_use_on_every_device_of_the_ward` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 41 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_starts_over_at_the_wards_local_midnight` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 42 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_empty_field_clears_and_zero_budget_locks` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 43 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_editor_refuses_half_or_empty_window` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 44 | app | `tests/e2e-unified/tests/test_family.py::test_family_editor_states_screen_time_is_enforced_on_the_childs_devices` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 45 | app | `tests/e2e-unified/tests/test_family.py::test_family_editor_states_which_content_categories_are_not_detected_yet` | tui (linux): passed |
| 46 | app | `tests/e2e-unified/tests/test_family.py::test_family_admission_and_ward_view` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 46 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_sees_ward` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 47 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_un_denies_the_second_blocked_peer` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 48 | app | `tests/e2e-unified/tests/test_family.py::test_family_supervised_indicator_shows_restored_guardian_while_status_is_pending` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 49 | app | `tests/e2e-unified/tests/test_family.py::test_family_content_floor_binds_on_ws_reconnect_without_relaunch` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 50 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_window_lock` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 50 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_and_usage_readouts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 51 | app | `tests/e2e-unified/tests/test_family.py::test_family_screen_time_budget_and_usage_readouts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 52 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_family_gate_docker.py::test_reject_ward_refuses_cold_sender_adult_still_receives` | nest (linux): passed |
| 53 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_a_guardian_limits_a_feature_for_one_child_and_the_child_sees_who_set_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 54 | app | `tests/e2e-unified/tests/test_admin_users_admit.py::test_admit_with_a_guardian_yields_a_supervised_ward` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 55 | app | `tests/e2e-unified/tests/test_admin_users_admit.py::test_admit_with_a_guardian_and_band_records_the_band_on_the_ward` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->

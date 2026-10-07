---
slug: admin-users
title: Members: requests, registration, invites and actions
section: admin area
goal: docs/goal/behavior/admin.md § 2. Users
guide: docs/guides/admin-tour.md § Users
---

## What a user gets

One page for who is on, who wants on and how to let someone on: approve join
requests at a tier, mint invite codes, admit a person by handle, change a member's
tier, suspend or evict them with a countdown you can cancel, and hand out or take
back the admin role, never leaving the nest with no admin. You set whether
registration is open, invite-only or closed, right here.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] A join request is approved at a tier and the member appears; invite codes mint and copy — `docs/goal/behavior/admin.md` § 2. Users
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_approve_invite_request_at_tier`
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_mint_code_and_copy`
   - `tests/e2e-unified/tests/test_admin.py::test_admin_invite_codes`
   - `tests/e2e-unified/tests/test_admin.py::test_admin_user_list`
2. [app] A person is admitted by handle and has a working account at once — `docs/goal/architecture/nest/public-mode.md` § Registration & Identity
   - `tests/e2e-unified/tests/test_admin_users_admit.py::test_admit_with_a_handle_yields_a_working_account`
3. [app] A member's tier changes from their row, and the list pages — `docs/goal/behavior/admin.md` § 2. Users
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_change_user_tier`
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_users_pagination_controls`
4. [app] From a member's row the admin starts an eviction and calls it off, suspends the member and restores them, and an admin's own row offers neither control — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_evict_and_cancel_eviction`
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_suspend_and_restore_user`
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_admin_row_offers_no_cut_off_controls`
5. [app] Granting admin schedules it with a delay; revoking the last admin is refused — `docs/goal/behavior/admin.md` § Admin continuity and succession
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_make_admin_offers_and_schedules_without_error`
   - `tests/e2e-unified/tests/test_admin_users_hub.py::test_remove_admin_refused_at_last_superadmin`
6. [app] Registration posture is set from the app and bites on the next stranger; the free-tier ceiling saves with it — `docs/goal/architecture/nest/public-mode.md` § Registration Modes
   - `tests/e2e-unified/tests/test_admin_registration_posture.py::test_admin_reads_back_the_posture_the_nest_is_running`
   - `tests/e2e-unified/tests/test_admin_registration_posture.py::test_admin_closes_registration_from_their_client_and_a_stranger_is_refused`
   - `tests/e2e-unified/tests/test_admin_registration_posture.py::test_the_free_tier_ceiling_saves_alongside_the_mode`
7. [app] Each member's row shows whether their mailbox is being served — `docs/goal/behavior/admin.md` § 2. Users
   - `tests/e2e-unified/tests/test_admin_serving_indicator.py::test_admin_sees_user_serving_status_read_only`
8. [app] A failed action shows on the page's own error line — `docs/goal/behavior/admin.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_admin_users_action_error.py::test_users_action_error_routes_to_dedicated_element`
9. [nest] A change to who is an admin waits out a delay as a pending action and can be called off before it runs — `docs/goal/behavior/admin.md` § Admin continuity and succession
   - `tests/e2e-unified/tests/api/test_admin_auth.py::test_admin_management`
10. [app] Two members who share a display name still appear as distinct, unambiguous choices in the guardian picker — `docs/goal/behavior/admin.md` § 2. Users
    - `tests/e2e-unified/tests/test_admin_picker_label_collision.py::test_admin_users_guardian_picker_stays_injective_when_labels_collide`
11. [app] Any member can be chosen as a guardian, however many members the nest holds — `docs/goal/behavior/admin.md` § 2. Users
    - `tests/e2e-unified/tests/test_admin_picker_all_accounts.py::test_admin_users_guardian_picker_offers_an_account_older_than_the_newest_page`
12. [app] An admin action waiting to run is listed with what it does, when it runs and how many approvals it still needs — none for a lone admin — and the admin calls it off in one click; approving your own action is refused on the page — `docs/goal/behavior/admin.md` § Pending admin actions
    - `tests/e2e-unified/tests/test_admin_pending_actions.py::test_a_scheduled_grant_is_listed_and_cancellable_from_the_admin_console`
13. [nest] A member an admin schedules for deletion is told at once, sees it among their own pending actions, and can cancel it; the admin is told who did — `docs/goal/behavior/notifications.md` § Security notices
    - `tests/e2e-unified/tests/api/test_pending_action_audience.py::test_the_target_of_an_admin_deletion_is_told_and_can_cancel_it`
14. [app] A child's age band is set where the guardian is set — the band picker only opens once a guardian is chosen, the minted code carries it, and the admin can require app-verified age at sign-up from the same Registration section, saved together — `docs/goal/behavior/family-safety.md` § App surface
    - `tests/e2e-unified/tests/test_admin_users_hub.py::test_mint_code_with_guardian_and_age_band`
    - `tests/e2e-unified/tests/test_admin_registration_posture.py::test_age_verification_knob_rides_the_section_save_and_bites`
15. [app] The admin turns a join request down from the page, with a reason if they give one — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
16. [app] Approving a request whose handle has been taken since it was made fails with a message on the page, and the request stays waiting — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
17. [nest] A refusal is not for ever: ninety days after it the request is cleared and the person can ask again, with nobody doing anything — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
18. [app] An invite code the admin minted is deleted from the page and leaves the list — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
19. [nest] Closing registration never locks out a member who already has an account — `docs/goal/architecture/nest/public-mode.md` § Registration Modes
    - (none)
20. [nest] A new nest lets nobody register themselves until its admin opens registration — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
21. [nest] While registration is closed the admin still admits a person by approving their request — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
22. [nest] An invite code admits its holder while registration is open or invite-only and is refused while it is closed; a code minted while closed works once registration opens — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
23. [nest] A member who already has an account redeems an invite code to move to that code's tier, whatever the registration mode — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
24. [nest] Once the cap on free accounts is reached the next free sign-up is refused, whatever the registration mode — `docs/goal/architecture/nest/public-mode.md` § Registration Modes
    - (none)
25. [nest] While app-verified age is required for sign-ups, the admin can still approve a join request that carries no age check — `docs/goal/architecture/nest/public-mode.md` § Registration Modes
    - (none)
26. [app] A mistyped person id in the admit form is caught on the page and nothing is sent — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
27. [app] Admitting a person with the handle left blank gives them an account that signs in and cannot send mail from the nest's own addresses — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
28. [app] A suspended member is never offered as a guardian — `docs/goal/behavior/admin.md` § 2. Users
    - (none)
29. [nest] An evicted member keeps full access for fourteen days to take their data out, is cut off for fourteen more, and only then is the account deleted — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
    - (none)
30. [nest] Suspending a member who is being evicted cuts them off at once and calls the deletion off — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
    - (none)
31. [app] Once an evicted account's deletion has begun it can no longer be restored, and its row offers no restore — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
    - (none)
32. [nest] An admin's account cannot be deleted, by another admin or by its owner, until the admin role is removed — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
    - (none)
33. [nest] A scheduled removal that would leave the nest with no admin does not run; it waits until another admin exists — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
    - (none)
34. [nest] With two admins neither changes who is an admin alone: the other approves a grant, a removal needs the approval of the admin being removed, and an action left short of its approvals lapses without effect — `docs/goal/behavior/admin.md` § Pending admin actions
    - (none)
35. [nest] The other admins are told when an admin action is scheduled — who scheduled it, whom it names, when it runs and how many approvals it still needs — and again when it runs, is called off or lapses — `docs/goal/behavior/notifications.md` § Security notices
    - (none)
36. [nest] A member is told when an admin action against their account takes effect: a suspension, or the admin role granted, removed or changed — `docs/goal/behavior/notifications.md` § Security notices
    - (none)
37. [nest] A suspended member is cut off at once: the nest answers nothing they ask, reads included, and closes the connections they had open — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
    - (none)
38. [nest] A restored member comes back as they were — same handle, same data, same tier — with nothing to set up again — `docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension
    - (none)
39. [nest] A join request someone submits appears in the admin's list of waiting requests — `docs/goal/behavior/admin.md` § 2. Users
    - `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_list_shows_submitted`
40. [app] Another admin sees who scheduled a waiting action and approves it in one click — `docs/goal/behavior/admin.md` § Pending admin actions
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+f3c1e99a standalone |
| linux | ⚠ partial | 0.1.2-dev+78e73031 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_approve_invite_request_at_tier` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_mint_code_and_copy` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin.py::test_admin_invite_codes` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin.py::test_admin_user_list` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_users_admit.py::test_admit_with_a_handle_yields_a_working_account` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_change_user_tier` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_users_pagination_controls` | web (linux): passed, linux (linux): passed, windows (windows): failed, tui (linux): failed |
| 4 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_evict_and_cancel_eviction` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_suspend_and_restore_user` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_admin_row_offers_no_cut_off_controls` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_make_admin_offers_and_schedules_without_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_remove_admin_refused_at_last_superadmin` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_admin_registration_posture.py::test_admin_reads_back_the_posture_the_nest_is_running` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_admin_registration_posture.py::test_admin_closes_registration_from_their_client_and_a_stranger_is_refused` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_admin_registration_posture.py::test_the_free_tier_ceiling_saves_alongside_the_mode` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_admin_serving_indicator.py::test_admin_sees_user_serving_status_read_only` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_admin_users_action_error.py::test_users_action_error_routes_to_dedicated_element` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_admin_auth.py::test_admin_management` | nest (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_admin_picker_label_collision.py::test_admin_users_guardian_picker_stays_injective_when_labels_collide` | web (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_admin_picker_all_accounts.py::test_admin_users_guardian_picker_offers_an_account_older_than_the_newest_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_admin_pending_actions.py::test_a_scheduled_grant_is_listed_and_cancellable_from_the_admin_console` | linux (linux): skipped, tui (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_pending_action_audience.py::test_the_target_of_an_admin_deletion_is_told_and_can_cancel_it` | nest (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_admin_users_hub.py::test_mint_code_with_guardian_and_age_band` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_admin_registration_posture.py::test_age_verification_knob_rides_the_section_save_and_bites` | web (linux): passed, linux (linux): passed, windows (windows): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | nest | (none) | — |
| 18 | app | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | (none) | — |
| 21 | nest | (none) | — |
| 22 | nest | (none) | — |
| 23 | nest | (none) | — |
| 24 | nest | (none) | — |
| 25 | nest | (none) | — |
| 26 | app | (none) | — |
| 27 | app | (none) | — |
| 28 | app | (none) | — |
| 29 | nest | (none) | — |
| 30 | nest | (none) | — |
| 31 | app | (none) | — |
| 32 | nest | (none) | — |
| 33 | nest | (none) | — |
| 34 | nest | (none) | — |
| 35 | nest | (none) | — |
| 36 | nest | (none) | — |
| 37 | nest | (none) | — |
| 38 | nest | (none) | — |
| 39 | nest | `tests/e2e-unified/tests/api/test_invite_requests.py::test_admin_list_shows_submitted` | nest (linux): passed |
| 40 | app | (none) | — |
<!-- features-render:end -->

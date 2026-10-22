---
slug: admin-tiers
title: Tiers and paid membership
section: admin area
goal: docs/goal/behavior/admin.md § 3. Settings
guide: docs/guides/admin-tour.md § Tiers
---

## What a user gets

Tiers set what a member may store and do. Edit each tier's caps in place, and
point paid membership at one of your own subscription tiers so access to the nest
can be sold.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] Tier definitions list and their caps edit in place — `docs/goal/behavior/admin.md` § 3. Settings
   - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_definitions`
   - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_edit`
   - `tests/e2e-unified/tests/test_admin_settings.py::test_admin_settings_loads`
2. [app] Paid membership points one of your subscription tiers at the quota tier its members get and the tier they drop to when it lapses, and clears again — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/test_admin_settings.py::test_admin_designates_a_membership_tier_re_points_it_and_clears_it`
3. [nest] A tier's limits bind every member on it — inbox, storage, devices, upload size and feeds — and a limit the admin changes applies from then on — `docs/goal/architecture/nest/public-mode.md` § Tiers
   - `tests/e2e-unified/tests/api/test_tier_limits_bind.py::test_the_upload_size_and_inbox_caps_bind_and_a_raised_cap_applies_at_once`
   - `tests/e2e-unified/tests/api/test_tier_limits_bind.py::test_the_storage_cap_binds_and_a_changed_cap_applies_at_once`
   - `tests/e2e-unified/tests/api/test_tier_limits_bind.py::test_the_feed_cap_binds_and_a_changed_cap_applies_at_once`
   - `tests/e2e-unified/tests/api/test_device_cap_api.py::test_a_new_device_past_the_tier_cap_is_refused_and_a_held_one_re_registers`
4. [nest] A person who pays for membership is put on the quota tier the admin linked to it, and a member who buys it is moved to that tier at once — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_paid_membership_code_admits_like_an_invite_code`
   - `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_member_who_buys_membership_is_moved_to_its_tier_at_once`
5. [nest] When a membership runs out the member drops to the tier chosen for lapsed members: they keep reading, keep everything already stored and can still export it, and are never suspended, evicted or deleted for it — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_lapsed_member_drops_to_the_lapse_tier_and_keeps_everything`
6. [nest] A lapsed member who renews is put back on the paid tier — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_lapsed_member_who_renews_is_put_back_on_the_paid_tier`
7. [nest] A tier the admin gave a member by hand is never undone by a membership running out — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_tier_given_by_hand_is_never_undone_by_a_lapse`
8. [nest] Re-pointing or clearing a membership link never changes the terms for members who already joined under it — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_lapse.py::test_re_pointing_or_clearing_the_membership_never_changes_admitted_members_terms`
9. [nest] People who join by paying do not count against the cap on free accounts — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/api/test_membership_lapse.py::test_people_who_join_by_paying_do_not_count_against_the_free_account_cap`
10. [app] The admin changes which quota tier and which lapse tier a paid membership points at, without clearing it first — `docs/goal/behavior/monetization.md` § Pillar 4
    - `tests/e2e-unified/tests/test_admin_settings.py::test_admin_re_points_a_designated_membership_without_clearing_it`
11. [app] The admin defines a new tier from the page — `docs/goal/behavior/admin.md` § 3. Settings
    - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_new`
    - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_refuses_empty_and_duplicate_names`
12. [nest] A tier nobody is on can be removed; one a member, an invite code or a membership link still names — and the default tier `free` — is refused and stays — `docs/goal/behavior/admin.md` § 3. Settings
    - `tests/e2e-unified/tests/api/test_tier_removal_api.py::test_a_tier_is_removable_only_once_nothing_names_it`
13. [app] The admin deletes a tier nobody is on from the page; deleting one a member is on is refused with the reason shown, and the tier stays — `docs/goal/behavior/admin.md` § 3. Settings
    - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_delete`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+110d1248 standalone |
| linux | ✅ full | 0.1.4-dev+ea391d07 standalone |
| windows | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+cab1c46a standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_definitions` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_edit` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_settings.py::test_admin_settings_loads` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_settings.py::test_admin_designates_a_membership_tier_re_points_it_and_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_tier_limits_bind.py::test_the_upload_size_and_inbox_caps_bind_and_a_raised_cap_applies_at_once` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_tier_limits_bind.py::test_the_storage_cap_binds_and_a_changed_cap_applies_at_once` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_tier_limits_bind.py::test_the_feed_cap_binds_and_a_changed_cap_applies_at_once` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_device_cap_api.py::test_a_new_device_past_the_tier_cap_is_refused_and_a_held_one_re_registers` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_membership_admission.py::test_a_paid_membership_code_admits_like_an_invite_code` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_member_who_buys_membership_is_moved_to_its_tier_at_once` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_lapsed_member_drops_to_the_lapse_tier_and_keeps_everything` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_lapsed_member_who_renews_is_put_back_on_the_paid_tier` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_membership_lapse.py::test_a_tier_given_by_hand_is_never_undone_by_a_lapse` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_membership_lapse.py::test_re_pointing_or_clearing_the_membership_never_changes_admitted_members_terms` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_membership_lapse.py::test_people_who_join_by_paying_do_not_count_against_the_free_account_cap` | nest (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_admin_settings.py::test_admin_re_points_a_designated_membership_without_clearing_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_new` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_refuses_empty_and_duplicate_names` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_tier_removal_api.py::test_a_tier_is_removable_only_once_nothing_names_it` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_delete` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->

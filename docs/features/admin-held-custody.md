---
slug: admin-held-custody
title: Held custody
section: admin area
goal: docs/goal/architecture/account-replica-posture.md § The custody grant + ceremony
guide: docs/guides/admin-tour.md § Held Custody
---

## What a user gets

A page listing the sealed copies this nest holds for people from elsewhere,
honest when there are none.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] With nothing held for anyone, the page opens with no copies listed and no error — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_admin_custody_hosting_page_reachable_and_honestly_empty`
2. [app] The page lists every copy the nest holds for people elsewhere, whichever member set it up, each with that member and the address the nest fetches from — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
   - `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_an_admin_lists_and_removes_a_copy_another_member_holds`
3. [app] The admin removes any held copy from the page, and it leaves the list — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
   - `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_an_admin_lists_and_removes_a_copy_another_member_holds`
4. [nest] Removing the last copy held for a person deletes their stored copy and frees its space; removing one of several keeps it — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_removing_the_last_copy_for_a_person_drops_their_store_and_one_of_several_keeps_it`
5. [nest] A copy its holder has paused keeps its space until it is removed — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_a_paused_copy_keeps_its_space_until_it_is_removed`
6. [nest] A member can have the nest hold only a bounded number of copies: one more is refused, and an allowance asked too large is honoured at the nest's ceiling rather than refused — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/api/test_custody_hosting_register_bounds.py::test_a_host_holds_a_bounded_number_of_copies_and_an_oversized_allowance_is_clamped`
7. [nest] What a member's held copies use is bounded by the storage allowance of that member's tier: past it a new copy is refused, and the member's own files are never refused because of copies held for others — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_held_copies_are_bounded_by_the_hosts_tier_and_never_refuse_its_own_files`
8. [nest] A held copy whose permission has expired is no longer fetched, and a month after the expiry its stored bytes are reclaimed while its entry stays on the list — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_an_expired_copy_is_no_longer_fetched_and_a_month_on_its_bytes_are_reclaimed`
9. [nest] When the owner takes their permission back, the nest stops fetching and serving that copy — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_when_the_owner_takes_the_permission_back_the_nest_stops_fetching`
   - `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_when_the_owner_takes_the_permission_back_the_nest_stops_serving`
10. [nest] A copy the nest holds for someone elsewhere is sealed: the nest stores and returns it and can never read it — `docs/goal/architecture/account-replica-posture.md` § Replica posture
    - `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_a_held_copy_comes_back_sealed_and_byte_identical_and_the_nest_never_held_a_key`
11. [nest] The nest hands a held copy back only to its owner's own devices — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_the_nest_hands_a_held_copy_back_only_to_the_owners_own_devices`
12. [nest] What the nest holds for someone never grows past the allowance set for it — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_what_the_nest_holds_never_grows_past_the_allowance_set_for_it`
13. [app] The page says that nothing is held only once the nest has answered, never while it is still loading — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_the_page_says_nothing_is_held_only_once_the_nest_has_answered`
    - `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_tui_names_no_count_while_the_registry_read_is_held`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.4-dev+b811e865.dirty standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows | ⚠ partial | 0.1.2-dev+ab96a0f8.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+923cec2d standalone |
| ios | ⚠ partial | 0.1.2-dev+923cec2d standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+5cdec819.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_admin_custody_hosting_page_reachable_and_honestly_empty` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_an_admin_lists_and_removes_a_copy_another_member_holds` | web (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_an_admin_lists_and_removes_a_copy_another_member_holds` | web (linux): passed, tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_removing_the_last_copy_for_a_person_drops_their_store_and_one_of_several_keeps_it` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_a_paused_copy_keeps_its_space_until_it_is_removed` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_register_bounds.py::test_a_host_holds_a_bounded_number_of_copies_and_an_oversized_allowance_is_clamped` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_held_copies_are_bounded_by_the_hosts_tier_and_never_refuse_its_own_files` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_an_expired_copy_is_no_longer_fetched_and_a_month_on_its_bytes_are_reclaimed` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_when_the_owner_takes_the_permission_back_the_nest_stops_fetching` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_when_the_owner_takes_the_permission_back_the_nest_stops_serving` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_a_held_copy_comes_back_sealed_and_byte_identical_and_the_nest_never_held_a_key` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_serve.py::test_the_nest_hands_a_held_copy_back_only_to_the_owners_own_devices` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_custody_hosting_pump.py::test_what_the_nest_holds_never_grows_past_the_allowance_set_for_it` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_the_page_says_nothing_is_held_only_once_the_nest_has_answered` | web (linux): passed, tui (linux): failed |
| 13 | app | `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_tui_names_no_count_while_the_registry_read_is_held` | tui (linux): passed |
<!-- features-render:end -->

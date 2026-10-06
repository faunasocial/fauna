---
slug: task-delegation
title: Choose which device does the heavy lifting
section: your data and devices
goal: docs/goal/behavior/participants.md § Task delegation (Q-B/Q-C)
guide: docs/guides/app-tour.md § Settings
---

## What a user gets

Some jobs, like building your search index, should run on exactly one of your
devices. The Task delegation page shows each job, which device is doing it right now,
and lets you pin one to a particular device or leave it automatic; automatic works with no
set-up at all.

## Coverage contract

Stamped 2026-09-26 at ef1315baff.

1. [app] The page lists every job with its runner and an assignment that defaults to automatic — `docs/goal/behavior/participants.md` § The assignment picker
   - `tests/e2e-unified/tests/test_task_delegation.py::test_task_delegation_page_lists_the_live_task_kinds`
2. [app] A device that is building the index is named as doing so — `docs/goal/behavior/participants.md` § Coordination primitive (Q-C — ratified + built)
   - `tests/e2e-unified/tests/test_task_delegation.py::test_the_index_row_names_this_device_as_builder_of_record`
   - `tests/e2e-unified/tests/test_task_delegation.py::test_web_names_the_other_device_building_the_index`
3. [nest] Your nest arbitrates who holds a job: a live holder keeps it, a stale one is taken over — `docs/goal/behavior/participants.md` § Coordination primitive (Q-C — ratified + built)
   - `tests/e2e-unified/tests/api/test_delegation.py::test_lease_acquire_observe_takeover`
   - `tests/e2e-unified/tests/api/test_delegation.py::test_renew_resets_age_keeping_a_live_holder_fresh`
   - `tests/e2e-unified/tests/api/test_delegation.py::test_leases_are_actor_scoped`
4. [app] Left on automatic, a job runs on your always-on nest when it can, otherwise on a plugged-in computer, and never on a phone or tablet — it waits instead — `docs/goal/behavior/participants.md` § Concepts
   - `tests/e2e-unified/tests/test_task_delegation.py::test_on_automatic_the_policy_prefers_the_nest_then_this_desktop_never_a_phone`
5. [app] A job can only be assigned to a device that can actually run it — `docs/goal/behavior/participants.md` § The assignment picker
   - `tests/e2e-unified/tests/test_task_delegation.py::test_only_a_kind_this_device_can_run_offers_this_device`
6. [app] Assignments are the same on every device you own, and one made on another device shows here and can be cleared here — `docs/goal/behavior/participants.md` § Coordination primitive (Q-C — ratified + built)
   - `tests/e2e-unified/tests/test_task_delegation.py::test_an_assignment_made_on_another_device_shows_here_and_clears_here`
7. [app] A job with no live device to run it says it is waiting, instead of naming a device that is not working — `docs/goal/behavior/participants.md` § Coordination primitive (Q-C — ratified + built)
   - `tests/e2e-unified/tests/test_task_delegation.py::test_a_kind_whose_runner_went_away_says_waiting_not_its_name`
8. [app] A job you assign to one device waits for that device rather than moving to another — `docs/goal/behavior/participants.md` § The assignment picker
   - `tests/e2e-unified/tests/test_task_delegation.py::test_a_kind_pinned_to_another_device_waits_for_it_rather_than_moving`
9. [nest] Your nest runs a job only while it holds the trust the job needs: granting it makes the nest the runner, and taking it back frees the job at once — `docs/goal/behavior/participants.md` § Coordination primitive (Q-C — ratified + built)
    - `tests/e2e-unified/tests/test_capability_rescore_drain.py::test_rescore_drain_fires_on_model_bump_via_user_minted_capability`
10. [nest] A nest whose attempts keep failing hands the job back to your devices — `docs/goal/behavior/participants.md` § Coordination primitive (Q-C — ratified + built)
    - `tests/e2e-unified/tests/api/test_backup_lease_handback.py::test_repeated_failures_hand_the_backup_job_back`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+05497413 docker |
| linux | ✅ full | 0.1.2-dev+05497413 docker |
| windows | ✅ full | 0.1.2-dev+05497413 docker |
| macos | ✅ full | 0.1.2-dev+05497413 docker |
| ios | ⚠ partial | 0.1.2-dev+05497413 docker |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+05497413 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_task_delegation_page_lists_the_live_task_kinds` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_the_index_row_names_this_device_as_builder_of_record` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_web_names_the_other_device_building_the_index` | web (linux): failed |
| 3 | nest | `tests/e2e-unified/tests/api/test_delegation.py::test_lease_acquire_observe_takeover` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_delegation.py::test_renew_resets_age_keeping_a_live_holder_fresh` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_delegation.py::test_leases_are_actor_scoped` | nest (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_on_automatic_the_policy_prefers_the_nest_then_this_desktop_never_a_phone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_only_a_kind_this_device_can_run_offers_this_device` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_an_assignment_made_on_another_device_shows_here_and_clears_here` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_a_kind_whose_runner_went_away_says_waiting_not_its_name` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_a_kind_pinned_to_another_device_waits_for_it_rather_than_moving` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/test_capability_rescore_drain.py::test_rescore_drain_fires_on_model_bump_via_user_minted_capability` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_backup_lease_handback.py::test_repeated_failures_hand_the_backup_job_back` | nest (linux): passed |
<!-- features-render:end -->

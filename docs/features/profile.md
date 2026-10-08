---
slug: profile
title: Your profile and other people's
section: everyday
goal: docs/goal/ui/profile.md § Goal
guide: docs/guides/app-tour.md § Profile
---

## What a user gets

Edit your display name, avatar and banner and they publish for everyone. Open
another person's profile from your contacts to message them, subscribe to what they
offer, or block them.

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] Editing your display name publishes it — `docs/goal/ui/profile.md` § User actions
   - `tests/e2e-unified/tests/test_profile.py::test_profile_edit_publishes_and_renders_display_name`
2. [app] You can set and clear an avatar and a banner — `docs/goal/ui/profile.md` § User actions
   - `tests/e2e-unified/tests/test_profile.py::test_profile_edit_sets_and_clears_avatar_and_banner`
3. [app] Another person's profile opens from your contacts and offers to message them — `docs/goal/ui/profile.md` § Relationship to Contacts
   - `tests/e2e-unified/tests/test_profile.py::test_other_profile_start_dm_opens_compose`
   - `tests/e2e-unified/tests/test_profile.py::test_state_protocol_actor_nav_opens_that_actor_then_normalizes_self`
4. [app] Block and unblock toggle from their profile — `docs/goal/ui/profile.md` § User actions
   - `tests/e2e-unified/tests/test_profile.py::test_other_profile_block`
5. [app] You can keep a private nickname, notes and labels on another person, visible only to you; their nickname shows in place of their name while their public name stays on screen — `docs/goal/ui/profile.md` § Layout & flow
   - `tests/e2e-unified/tests/test_contact_overlay.py::test_a_nickname_notes_and_label_paint_on_the_roster_and_profile_and_survive_a_relaunch`
   - `tests/e2e-unified/tests/test_contact_overlay.py::test_two_seats_of_one_account_converge_on_the_overlay_per_field`
6. [app] A person's actor id can be copied from their profile — `docs/goal/ui/profile.md` § User actions
   - `tests/e2e-unified/tests/test_profile.py::test_profile_copy_button_copies_the_viewed_actors_id`
7. [app] Requesting contact from another person's profile sends them a contact request — `docs/goal/ui/profile.md` § User actions
   - `tests/e2e-unified/tests/test_profile.py::test_other_profile_request_contact_lands_a_knock`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ✅ full | 0.1.2-dev+2354a342.dirty standalone |
| windows | ⚠ partial | 0.1.2-dev+068e152c.dirty standalone |
| macos | ✅ full | |
| ios | ✅ full | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_profile.py::test_profile_edit_publishes_and_renders_display_name` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_profile.py::test_profile_edit_sets_and_clears_avatar_and_banner` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_profile.py::test_other_profile_start_dm_opens_compose` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_profile.py::test_state_protocol_actor_nav_opens_that_actor_then_normalizes_self` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_profile.py::test_other_profile_block` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_contact_overlay.py::test_a_nickname_notes_and_label_paint_on_the_roster_and_profile_and_survive_a_relaunch` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_contact_overlay.py::test_two_seats_of_one_account_converge_on_the_overlay_per_field` | linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_profile.py::test_profile_copy_button_copies_the_viewed_actors_id` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_profile.py::test_other_profile_request_contact_lands_a_knock` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->

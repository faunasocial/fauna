---
slug: offline-sharing
title: Share a folder in person, with no nest in between
section: your data and devices
goal: docs/goal/behavior/p2p-shared-set.md § Offline share initiation
guide: docs/guides/cloud-sharing-family.md § Sharing a folder with someone
absences:
  web: "docs/goal/behavior/p2p-shared-set.md § Wormability walk"
---

## What a user gets

Two people sitting together can share a folder device to device: each copies the
other's code from the screen in front of them, one gets ready to receive, the
other begins, and the folder is listed on both sides. Once each of you picks a place for the folder on your own device, a file either of you adds there reaches the other person straight from your device while both apps are open, with no nest involved — and in a folder you share through your nest, a file added while the nest is unreachable can reach the other person the same way. Nobody can start a share with you that you did not
ask for. This is about giving a folder to another person face to face; your own
devices moving your own files between themselves is [Files move straight between
your own devices](device-to-device-file-transfer.md).

## Coverage contract

Stamped 2026-10-21 at 77c610862e.

1. [app] The code the app shows is this device's own; a bad or self-addressed code is refused; cancel is always available — `docs/goal/behavior/p2p-shared-set.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_compare_code_the_app_shows_is_this_actors_own_key`
   - `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_act_button_refuses_an_unusable_code_and_accepts_a_real_one`
   - `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_two_panels_are_exclusive_and_cancel_returns_to_the_entry`
2. [app] Two seats complete the ceremony and both list the folder; a decline lists nothing; an uninvited initiator is refused — `docs/goal/behavior/p2p-shared-set.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_lists_the_shared_set_on_both_seats`
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_declined_invitation_lists_nothing_and_stops_knocking`
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_stranger_cannot_start_a_ceremony_nobody_asked_for`
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_the_two_seats_never_show_each_other_their_own_code`
3. [app] A file added to a folder you shared in person reaches the other person straight from your device — `docs/goal/behavior/p2p-shared-set.md` § Cross-user shared-set transfer
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_file_added_to_a_set_shared_in_person_arrives_with_the_nest_stopped`
4. [app] Two people sitting together complete the share while their nest cannot be reached — `docs/goal/behavior/p2p-shared-set.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_completes_while_the_nest_is_unreachable`
5. [app] If the connection between the two devices drops part-way through, the share picks up again without either person entering the code a second time — `docs/goal/behavior/p2p-shared-set.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_connection_dropped_part_way_picks_the_same_share_up_again`
6. [app] Being ready to receive a share lasts only a short while; someone arriving after it has lapsed is refused like a stranger — `docs/goal/behavior/p2p-shared-set.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_receive_window_that_lapsed_refuses_the_initiator_like_a_stranger`
7. [app] When two people save the same file at once, the one whose save lost sees it in their review list and can bring their version back on both devices — `docs/goal/behavior/group-set.md` § Build design — the group set: a folder bound under an in-person share (design pass 2026-10-29; refutable at build time)
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_concurrent_edit_is_reviewed_on_the_losing_seat_and_its_version_restored`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ✅ full | 0.1.5-dev+0cfdd6c6 standalone |
| windows | ✅ full | 0.1.6-dev+00ef95a1 standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | 🚧 unbuilt | 0.1.5-dev+2e009c0f standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.6 docker |
| android-kids |  no run recorded | |
| ios-kids | 🚧 unbuilt | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_compare_code_the_app_shows_is_this_actors_own_key` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_act_button_refuses_an_unusable_code_and_accepts_a_real_one` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_two_panels_are_exclusive_and_cancel_returns_to_the_entry` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_lists_the_shared_set_on_both_seats` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_declined_invitation_lists_nothing_and_stops_knocking` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_stranger_cannot_start_a_ceremony_nobody_asked_for` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_the_two_seats_never_show_each_other_their_own_code` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_file_added_to_a_set_shared_in_person_arrives_with_the_nest_stopped` — requires `group-set-feed` (not built: web, ios, android, android-kids, ios-kids) | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_completes_while_the_nest_is_unreachable` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_connection_dropped_part_way_picks_the_same_share_up_again` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_receive_window_that_lapsed_refuses_the_initiator_like_a_stranger` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_concurrent_edit_is_reviewed_on_the_losing_seat_and_its_version_restored` — requires `group-set-feed` (not built: web, ios, android, android-kids, ios-kids) | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
<!-- features-render:end -->

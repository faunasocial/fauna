---
slug: offline-sharing
title: Share a folder in person, with no nest in between
section: your data and devices
goal: docs/goal/behavior/p2p.md § Offline share initiation
guide: docs/guides/cloud-sharing-family.md § Sharing a folder with someone
absences:
  web: "docs/goal/behavior/p2p.md § Wormability walk"
---

## What a user gets

Two people sitting together can share a folder device to device: one begins,
the other receives, both compare a short code on screen, and the folder is listed on
both sides. A file added while the nest is unreachable still reaches the other
person, peer to peer. Nobody can start a ceremony you did not ask for.

## Coverage contract

Stamped 2026-09-19 at 25eff180d4.

1. [app] The code the app shows is this device's own; a bad or self-addressed code is refused; cancel is always available — `docs/goal/behavior/p2p.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_compare_code_the_app_shows_is_this_actors_own_key`
   - `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_act_button_refuses_an_unusable_code_and_accepts_a_real_one`
   - `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_two_panels_are_exclusive_and_cancel_returns_to_the_entry`
2. [app] Two seats complete the ceremony and both list the folder; a decline lists nothing; an uninvited initiator is refused — `docs/goal/behavior/p2p.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_lists_the_shared_set_on_both_seats`
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_declined_invitation_lists_nothing_and_stops_knocking`
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_stranger_cannot_start_a_ceremony_nobody_asked_for`
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_the_two_seats_never_show_each_other_their_own_code`
3. [app] A file added while the nest is down arrives on the other person's device peer to peer — `docs/goal/behavior/p2p.md` § Cross-user shared-set transfer
   - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_member_pulls_offline_authored_file_from_owner_peer`
4. [app] Two people sitting together complete the share while their nest cannot be reached — `docs/goal/behavior/p2p.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_completes_while_the_nest_is_unreachable`
5. [app] If the connection between the two devices drops part-way through, the share picks up again without either person entering the code a second time — `docs/goal/behavior/p2p.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_connection_dropped_part_way_picks_the_same_share_up_again`
6. [app] Being ready to receive a share lasts only a short while; someone arriving after it has lapsed is refused like a stranger — `docs/goal/behavior/p2p.md` § Offline share initiation
   - `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_receive_window_that_lapsed_refuses_the_initiator_like_a_stranger`
7. [app] A person the folder was never shared with gets nothing readable from your device, even on the same network — `docs/goal/behavior/p2p.md` § Cross-user shared-set transfer
   - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_person_the_folder_was_never_shared_with_gets_nothing_readable`
8. [app] A device-to-device transfer interrupted part-way picks up where it stopped, without sending again what already arrived — `docs/goal/behavior/p2p.md` § Cross-user shared-set transfer
   - `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.2-dev+2a213a2d.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+3fd2455c standalone |
| ios | ⚠ partial | 0.1.2-dev+7f22a381 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_compare_code_the_app_shows_is_this_actors_own_key` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_act_button_refuses_an_unusable_code_and_accepts_a_real_one` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_offline_share_initiation.py::test_the_two_panels_are_exclusive_and_cancel_returns_to_the_entry` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_lists_the_shared_set_on_both_seats` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_declined_invitation_lists_nothing_and_stops_knocking` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_stranger_cannot_start_a_ceremony_nobody_asked_for` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_the_two_seats_never_show_each_other_their_own_code` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_member_pulls_offline_authored_file_from_owner_peer` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_completes_while_the_nest_is_unreachable` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): failed |
| 5 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_connection_dropped_part_way_picks_the_same_share_up_again` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_receive_window_that_lapsed_refuses_the_initiator_like_a_stranger` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_person_the_folder_was_never_shared_with_gets_nothing_readable` | linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->

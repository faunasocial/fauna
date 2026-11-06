---
slug: device-to-device-file-transfer
title: Files move straight between your own devices
section: your data and devices
goal: docs/goal/behavior/p2p.md § Goal
guide: docs/guides/cloud-sync.md § What syncing feels like
absences:
  web: "docs/goal/behavior/p2p-shared-set.md § Wormability walk"
  android (outcomes 1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 16, 17, 18, 19): "docs/goal/behavior/p2p.md § Implementation status today"
  ios (outcomes 1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 16, 17, 18, 19): "docs/goal/behavior/p2p.md § Implementation status today"
---

## What a user gets

When two of your devices are on the same network, a file syncing between them
travels directly from one to the other instead of through your nest, so it
arrives faster and your nest never sees its contents. There is nothing to set
up and nothing to switch on, and if the direct path is ever unavailable the
file still arrives the ordinary way, through your nest.

This is about your own devices passing your own files to each other; handing a
folder to someone else in person is [Share a folder in person, with no nest in
between](offline-sharing.md).

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] A file syncing between two of your devices on the same local network moves straight between them, and the nest never sees the file's data — `docs/goal/behavior/p2p.md` § Goal
   - `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_moves_straight_between_two_of_your_devices`
2. [app] Nothing is set up first: no pairing step, no network configuration, and no switch a transfer waits on before it takes the direct path — `docs/goal/behavior/p2p.md` § Goal
   - `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_moves_straight_between_two_of_your_devices`
3. [app] A transfer never fails for want of a direct path: when the direct path is unavailable or drops part-way, the file still arrives through your nest — `docs/goal/behavior/p2p.md` § Goal
   - `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_still_arrives_through_the_nest_when_the_direct_path_is_gone`
4. [app] What travels directly between your devices is encrypted on the way, so nothing else on the same network can read it — `docs/goal/behavior/p2p.md` § Goal
   - `tests/e2e-unified/tests/test_device_direct_path_admits_only_your_devices.py::test_a_device_that_is_not_yours_fetches_nothing_and_yours_fetch_only_ciphertext`
5. [app] When a file takes the ordinary path through your nest instead of the direct one, nothing asks you anything or warns you: it simply arrives — `docs/goal/behavior/p2p.md` § 5. Routing and fallback
   - `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_still_arrives_through_the_nest_when_the_direct_path_is_gone`
6. [app] A file arriving straight from another of your devices is checked exactly as one from your nest, and a damaged or tampered piece is refused rather than written — `docs/goal/behavior/p2p.md` § Wormability posture (ratified 2026-08-10)
   - `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_tampered_piece_from_your_other_device_is_refused`
7. [app] Two of your devices on the same network keep passing files to each other while your nest, or your internet connection, is down — `docs/goal/behavior/p2p.md` § Same-account nest-down folder transfer
   - `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_two_devices_keep_passing_a_file_while_the_nest_is_down`
8. [app] Your devices never broadcast, scan your network or announce themselves on it to find each other — `docs/goal/behavior/p2p.md` § LAN detection
   - `tests/e2e-unified/tests/test_device_direct_path_never_announces_or_scans.py::test_your_devices_never_broadcast_scan_or_announce_themselves`
9. [app] Each of your devices has its own switch for taking part in direct transfers, on unless you turn it off, on that device's card on the Devices page — `docs/goal/behavior/p2p.md` § Per-device participation
   - `tests/e2e-unified/tests/test_p2p_participation.py::test_this_devices_peer_transfer_switch_flips_and_rests`
10. [app] Turning a device's direct transfers off leaves its syncing untouched: its files still come and go through your nest — `docs/goal/behavior/p2p.md` § Per-device participation
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched`
11. [app] A device with direct transfers turned off opens nothing on the network for other devices to reach: it serves nothing, fetches nothing directly and advertises no address — `docs/goal/behavior/p2p.md` § Per-device participation
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched`
12. [app] From another of your devices you can only ask a device to stop taking part in direct transfers, never switch it on: only the device itself turns them back on — `docs/goal/behavior/p2p.md` § Per-device participation
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched`
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns`
13. [app] A request to turn another device's direct transfers off, made while that device was away, takes effect when it next comes back — `docs/goal/behavior/p2p.md` § Per-device participation
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched`
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns`
14. [app] Each device's card shows whether that device last said it takes part in direct transfers — `docs/goal/behavior/p2p.md` § Per-device participation
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched`
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns`
15. [app] A device's direct-transfer setting stays as you left it across a restart of the app — `docs/goal/behavior/p2p.md` § Per-device participation
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched`
    - `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns`
16. [app] A device you remove from your account can no longer fetch anything directly from your other devices — `docs/goal/architecture/account-sync-plane.md` § The peer leg — device↔device sync over iroh (requirement 7)
    - `tests/e2e-unified/tests/test_device_removal_severs_direct_path.py::test_a_removed_device_fetches_nothing_directly_from_your_other_devices`
17. [app] Only your own devices can fetch files from your device over the direct path: a device that is not yours gets nothing, even on the same network — `docs/goal/behavior/p2p.md` § Wormability posture (ratified 2026-08-10)
    - `tests/e2e-unified/tests/test_device_direct_path_admits_only_your_devices.py::test_a_device_that_is_not_yours_fetches_nothing_and_yours_fetch_only_ciphertext`
18. [app] A device holding a file only as an on-demand placeholder never downloads it just to hand it to another device — `docs/goal/behavior/relay-serving.md` § Relay serving
    - `tests/e2e-unified/tests/test_device_placeholder_serves_no_sibling.py::test_a_placeholder_is_never_downloaded_to_hand_to_another_device`
19. [app] A folder whose contents you keep off your nest still passes its files directly between your devices, so the nest never holds them even in passing — `docs/goal/behavior/file-sync.md` § Content residency
    - `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_moves_straight_between_two_of_your_devices`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ✅ full | 0.1.5-dev+cbd813be standalone |
| windows | ⚠ partial | |
| macos | 🚧 unbuilt | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+662ece79 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_moves_straight_between_two_of_your_devices` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | absent by design on ios, android | — |
| 2 | app | `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_moves_straight_between_two_of_your_devices` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | absent by design on ios, android | — |
| 3 | app | `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_still_arrives_through_the_nest_when_the_direct_path_is_gone` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | absent by design on ios, android | — |
| 4 | app | `tests/e2e-unified/tests/test_device_direct_path_admits_only_your_devices.py::test_a_device_that_is_not_yours_fetches_nothing_and_yours_fetch_only_ciphertext` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | absent by design on ios, android | — |
| 5 | app | `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_still_arrives_through_the_nest_when_the_direct_path_is_gone` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | absent by design on ios, android | — |
| 6 | app | `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_tampered_piece_from_your_other_device_is_refused` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | app | absent by design on ios, android | — |
| 7 | app | `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_two_devices_keep_passing_a_file_while_the_nest_is_down` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 7 | app | absent by design on ios, android | — |
| 8 | app | `tests/e2e-unified/tests/test_device_direct_path_never_announces_or_scans.py::test_your_devices_never_broadcast_scan_or_announce_themselves` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): failed |
| 8 | app | absent by design on ios, android | — |
| 9 | app | `tests/e2e-unified/tests/test_p2p_participation.py::test_this_devices_peer_transfer_switch_flips_and_rests` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | absent by design on ios, android | — |
| 11 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | absent by design on ios, android | — |
| 12 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns` | ios (macos): passed |
| 13 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 13 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns` | ios (macos): passed |
| 14 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 14 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns` | ios (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_device_to_stop_and_syncing_is_untouched` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_device_direct_transfer_switch.py::test_a_sibling_can_only_ask_a_phone_to_stop_and_it_lands_when_the_phone_returns` | ios (macos): passed |
| 16 | app | `tests/e2e-unified/tests/test_device_removal_severs_direct_path.py::test_a_removed_device_fetches_nothing_directly_from_your_other_devices` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | app | absent by design on ios, android | — |
| 17 | app | `tests/e2e-unified/tests/test_device_direct_path_admits_only_your_devices.py::test_a_device_that_is_not_yours_fetches_nothing_and_yours_fetch_only_ciphertext` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 17 | app | absent by design on ios, android | — |
| 18 | app | `tests/e2e-unified/tests/test_device_placeholder_serves_no_sibling.py::test_a_placeholder_is_never_downloaded_to_hand_to_another_device` — requires `on-demand-replica-serve-seat` (not built: web, macos, ios, android) | linux (linux): passed, windows (windows): failed, macos (macos): skipped, tui (linux): passed, tui (macos): failed, tui (windows): failed |
| 18 | app | absent by design on ios, android | — |
| 19 | app | `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_a_file_moves_straight_between_two_of_your_devices` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 19 | app | absent by design on ios, android | — |
<!-- features-render:end -->

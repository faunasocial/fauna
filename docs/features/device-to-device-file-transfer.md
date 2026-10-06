---
slug: device-to-device-file-transfer
title: Files move straight between your own devices
section: your data and devices
goal: docs/goal/behavior/p2p.md § Goal
guide: docs/guides/cloud-sync.md § What syncing feels like
---

## What a user gets

When two of your devices are on the same network, a file syncing between them
travels directly from one to the other instead of through your nest, so it
arrives faster and your nest never sees its contents. There is nothing to set
up and nothing to switch on, and if the direct path is ever unavailable the
file still arrives the ordinary way, through your nest.

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] A file syncing between two of your devices on the same local network moves straight between them, and the nest never sees the file's data — `docs/goal/behavior/p2p.md` § Goal
   - (none)
2. [app] Nothing is set up first: no pairing step, no network configuration, and no switch a transfer waits on before it takes the direct path — `docs/goal/behavior/p2p.md` § Goal
   - (none)
3. [app] A transfer never fails for want of a direct path: when the direct path is unavailable or drops part-way, the file still arrives through your nest — `docs/goal/behavior/p2p.md` § Goal
   - (none)
4. [app] What travels directly between your devices is encrypted on the way, so nothing else on the same network can read it — `docs/goal/behavior/p2p.md` § Goal
   - (none)
5. [app] When a file takes the ordinary path through your nest instead of the direct one, nothing asks you anything or warns you: it simply arrives — `docs/goal/behavior/p2p.md` § 5. Routing and fallback
   - (none)
6. [app] A file arriving straight from another of your devices is checked exactly as one from your nest, and a damaged or tampered piece is refused rather than written — `docs/goal/behavior/p2p.md` § Wormability posture (ratified 2026-08-10)
   - (none)
7. [app] Two of your devices on the same network keep passing files to each other while your nest, or your internet connection, is down — `docs/goal/architecture/account-sync-plane.md` § The peer leg — device↔device sync over iroh (requirement 7)
   - (none)
8. [app] Your devices never broadcast, scan your network or announce themselves on it to find each other — `docs/goal/behavior/p2p.md` § LAN detection
   - (none)
9. [app] Each of your devices has its own switch for taking part in direct transfers, on unless you turn it off, on that device's card on the Devices page — `docs/goal/behavior/p2p.md` § Per-device participation
   - `tests/e2e-unified/tests/test_p2p_participation.py::test_this_devices_peer_transfer_switch_flips_and_rests`
10. [app] Turning a device's direct transfers off leaves its syncing untouched: its files still come and go through your nest — `docs/goal/behavior/p2p.md` § Per-device participation
    - (none)
11. [app] A device with direct transfers turned off opens nothing on the network for other devices to reach: it serves nothing, fetches nothing directly and advertises no address — `docs/goal/behavior/p2p.md` § Per-device participation
    - (none)
12. [app] From another of your devices you can only ask a device to stop taking part in direct transfers, never switch it on: only the device itself turns them back on — `docs/goal/behavior/p2p.md` § Per-device participation
    - (none)
13. [app] A request to turn another device's direct transfers off, made while that device was away, takes effect when it next comes back — `docs/goal/behavior/p2p.md` § Per-device participation
    - (none)
14. [app] Each device's card shows whether that device last said it takes part in direct transfers — `docs/goal/behavior/p2p.md` § Per-device participation
    - (none)
15. [app] A device's direct-transfer setting stays as you left it across a restart of the app — `docs/goal/behavior/p2p.md` § Per-device participation
    - (none)
16. [app] A device you remove from your account can no longer fetch anything directly from your other devices — `docs/goal/architecture/account-sync-plane.md` § The peer leg — device↔device sync over iroh (requirement 7)
    - (none)
17. [app] Only your own devices can fetch files from your device over the direct path: a device that is not yours gets nothing, even on the same network — `docs/goal/behavior/p2p.md` § Wormability posture (ratified 2026-08-10)
    - (none)
18. [app] A device holding a file only as an on-demand placeholder never downloads it just to hand it to another device — `docs/goal/behavior/file-sync.md` § Relay serving
    - (none)
19. [app] A folder whose contents you keep off your nest still passes its files directly between your devices, so the nest never holds them even in passing — `docs/goal/behavior/file-sync.md` § Content residency
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | (none) | — |
| 2 | app | (none) | — |
| 3 | app | (none) | — |
| 4 | app | (none) | — |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | `tests/e2e-unified/tests/test_p2p_participation.py::test_this_devices_peer_transfer_switch_flips_and_rests` | linux (linux): passed, tui (linux): passed |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | app | (none) | — |
| 13 | app | (none) | — |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | app | (none) | — |
| 18 | app | (none) | — |
| 19 | app | (none) | — |
<!-- features-render:end -->

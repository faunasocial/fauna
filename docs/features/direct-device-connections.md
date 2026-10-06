---
slug: direct-device-connections
title: Direct connections between your devices
section: your data and devices
goal: docs/goal/behavior/p2p.md § Goal
guide: docs/guides/cloud-sync.md § What syncing feels like
absences:
  web: "docs/goal/behavior/p2p.md § RATIFIED 2026-08-24"
  windows: "docs/goal/behavior/p2p.md § RATIFIED 2026-08-24"
  macos: "docs/goal/behavior/p2p.md § RATIFIED 2026-08-24"
  ios: "docs/goal/behavior/p2p.md § RATIFIED 2026-08-24"
  android: "docs/goal/behavior/p2p.md § RATIFIED 2026-08-24"
  tui: "docs/goal/behavior/p2p.md § RATIFIED 2026-08-24"
---

## What a user gets

The Linux app has a page for direct device-to-device connections: start and
stop the tunnel, copy this device's address and its addresses on the local network,
and see and remove the devices it has talked to. On every other app the same
peer-to-peer transfer runs behind the folders page with no page of its own.

## Coverage contract

Stamped 2026-09-19 at f62a5e4e1c.

1. [app] The tunnel starts and stops, and a failure to start is shown on the page — `docs/goal/behavior/p2p.md` § Tunnel lifecycle
   - `tests/e2e-unified/tests/test_p2p.py::test_p2p_page_renders`
   - `tests/e2e-unified/tests/test_p2p.py::test_tunnel_start_stop_round_trip`
   - `tests/e2e-unified/tests/test_p2p.py::test_start_failure_renders_error_message`
2. [app] This device's local addresses are found and copied — `docs/goal/behavior/p2p.md` § LAN detection
   - `tests/e2e-unified/tests/test_p2p.py::test_lan_addresses_copy_round_trip`
3. [app] Known peers are listed and can be removed — `docs/goal/behavior/p2p.md` § Goal
   - `tests/e2e-unified/tests/test_p2p.py::test_p2p_contact_list_renders_and_removes`
4. [app] This device's own connection identity can be copied while the tunnel is up, and not while it is down — `docs/goal/behavior/p2p.md` § Element IDs
   - `tests/e2e-unified/tests/test_p2p.py::test_tunnel_start_stop_round_trip`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | — absent | |
| macos | — absent | |
| ios | — absent | |
| android | — absent | |
| tui | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_p2p.py::test_p2p_page_renders` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_p2p.py::test_tunnel_start_stop_round_trip` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_p2p.py::test_start_failure_renders_error_message` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_p2p.py::test_lan_addresses_copy_round_trip` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_p2p.py::test_p2p_contact_list_renders_and_removes` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_p2p.py::test_tunnel_start_stop_round_trip` | linux (linux): passed |
<!-- features-render:end -->

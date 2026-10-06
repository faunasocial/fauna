---
slug: offline-aware-controls
title: The app tells you what needs a connection
section: everyday
goal: docs/goal/architecture/account-offline-mutation.md § The offline-mutation contract (W4)
guide: docs/guides/app-tour.md § When the app cannot reach your nest
---

## What a user gets

When your nest is out of reach the app keeps working for everything it can do
locally, greys out the controls that need the nest and says why beside each one, and
shows the connection state in the shell. The moment the nest is back, everything
comes alive again.

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] A control that needs the nest is disabled while it is unreachable, says why, and is live again when it returns — `docs/goal/architecture/account-offline-mutation.md` § The offline-mutation contract (W4)
   - `tests/e2e-unified/tests/test_offline_gate.py::test_online_only_control_desensitizes_with_no_nest_and_says_why`
   - `tests/e2e-unified/tests/test_offline_gate.py::test_a_user_facing_page_desensitizes_with_no_nest`
   - `tests/e2e-unified/tests/test_offline_gate.py::test_the_admin_plane_desensitizes_with_no_nest`
2. [app] A control that works offline stays live beside one that does not — `docs/goal/architecture/account-offline-mutation.md` § The offline-mutation contract (W4)
   - `tests/e2e-unified/tests/test_offline_gate.py::test_an_offline_capable_sibling_stays_live_beside_the_gated_one`
3. [app] The connection indicator follows the real state of the connection — `docs/goal/architecture/transport-connection.md` § Connection-status indicator (app UI)
   - `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_connection_status_flips_while_nest_down`
4. [app] A connection that keeps failing stops reading as a passing gap and says the nest cannot be reached, until a connection actually succeeds — `docs/goal/architecture/transport-connection.md` § Connection-status indicator (app UI)
   - `tests/e2e-unified/tests/test_connection_gap_rules.py::test_a_failing_connection_reads_cannot_connect_until_one_succeeds`
5. [app] A passing connection gap raises no error anywhere — the connection indicator is the only place it shows — `docs/goal/architecture/transport-connection.md` § Connection-status indicator (app UI)
   - `tests/e2e-unified/tests/test_connection_gap_rules.py::test_a_passing_gap_raises_no_error_anywhere`

## Workstream

W4 in `docs/goal/architecture/account-data-plane.md` § Workstreams; that table's own
"Contract section" column points here for the deliverable this page's contract covers.

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+72d6a508 standalone |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ✅ full | |
| ios | ✅ full | |
| android |  no run recorded | |
| tui | ❌ failing | 0.1.2-dev+f872d502 live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_offline_gate.py::test_online_only_control_desensitizes_with_no_nest_and_says_why` | web (linux): passed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_offline_gate.py::test_a_user_facing_page_desensitizes_with_no_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_offline_gate.py::test_the_admin_plane_desensitizes_with_no_nest` | web (linux): passed, linux (linux): skipped, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_offline_gate.py::test_an_offline_capable_sibling_stays_live_beside_the_gated_one` | web (linux): passed, linux (linux): failed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_connection_status_flips_while_nest_down` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_connection_gap_rules.py::test_a_failing_connection_reads_cannot_connect_until_one_succeeds` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_connection_gap_rules.py::test_a_passing_gap_raises_no_error_anywhere` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (macos): passed |
<!-- features-render:end -->

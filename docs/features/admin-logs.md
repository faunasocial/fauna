---
slug: admin-logs
title: Logs
section: admin area
goal: docs/goal/architecture/apps/observability.md § 3. Surfaces
guide: docs/guides/admin-tour.md § Logs
---

## What a user gets

The nest's recent activity, newest first, filterable by severity, with copy.
The nest's helpers report their events into the same log.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] The Logs page shows the nest's activity, with a severity filter and a copy button — `docs/goal/architecture/apps/observability.md` § 3. Surfaces
   - `tests/e2e-unified/tests/test_admin_logs.py::test_admin_logs_page_renders_nest_ring`
   - `tests/e2e-unified/tests/test_admin_logs.py::test_admin_logs_level_filter_narrows`
   - `tests/e2e-unified/tests/test_admin_logs.py::test_admin_logs_copy_present`
2. [nest] A helper's event reaches the admin log under that helper's own name — `docs/goal/architecture/apps/observability.md` § The sidecar log plane (nest-side sources beyond the nest process)
   - `tests/e2e-unified/tests/platform/docker/test_sidecar_log_plane_docker.py::test_bridge_ready_event_reaches_admin_logs`
3. [app] The newest activity is listed first — `docs/goal/architecture/apps/observability.md` § 3. Surfaces
   - (none)
4. [app] A helper's event appears among the nest's own lines in time order, labelled with the helper that reported it — `docs/goal/architecture/apps/observability.md` § Admission, attribution, and the remote ring (nest-side)
   - (none)
5. [nest] A chatty or misbehaving helper can never push the nest's own history out of the log — `docs/goal/architecture/apps/observability.md` § Admission, attribution, and the remote ring (nest-side)
   - (none)
6. [nest] When a helper floods the log the excess is dropped, and a warning — at most one a minute for each helper — says how many of its events were lost — `docs/goal/architecture/apps/observability.md` § Admission, attribution, and the remote ring (nest-side)
   - (none)
7. [nest] A helper's line reaches the log stripped of control characters, cut to a bounded length, and only as an error, a warning or information — `docs/goal/architecture/apps/observability.md` § Admission, attribution, and the remote ring (nest-side)
   - (none)
8. [nest] The nest's log never carries message text, keys, tokens or claim codes — only what happened — `docs/goal/architecture/apps/observability.md` § 2. Persistence & privacy
   - (none)
9. [nest] While the nest keeps failing to bring members' published pages back, each retry leaves a warning in the nest's log — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - (none)
10. [app] Choosing a severity shows only the lines of that severity — `docs/goal/architecture/apps/observability.md` § 3. Surfaces
    - (none)
11. [app] Copy puts the lines shown on the clipboard — `docs/goal/architecture/apps/observability.md` § 3. Surfaces
    - (none)
12. [nest] A helper cannot make its events appear under another helper's name or the nest's own — `docs/goal/architecture/apps/observability.md` § The sidecar log plane (nest-side sources beyond the nest process)
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+f3c1e99a standalone |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+ab96a0f8.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_logs.py::test_admin_logs_page_renders_nest_ring` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_logs.py::test_admin_logs_level_filter_narrows` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_logs.py::test_admin_logs_copy_present` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_sidecar_log_plane_docker.py::test_bridge_ready_event_reaches_admin_logs` | nest (linux): passed |
| 3 | app | (none) | — |
| 4 | app | (none) | — |
| 5 | nest | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | (none) | — |
| 9 | nest | (none) | — |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | nest | (none) | — |
<!-- features-render:end -->

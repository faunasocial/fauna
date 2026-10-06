---
slug: admin-dashboard
title: The admin area
section: admin area
goal: docs/goal/behavior/admin.md § 1. Dashboard
guide: docs/guides/admin-tour.md § Getting in — and back out
---

## What a user gets

If you are the nest's admin, the app shows an Admin entry that others never
see. It opens a separate area with its own navigation, a dashboard of member and
version counts, and one way back out. A failed admin action tells you on the page it
happened on.

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] The admin sees the entry, a non-admin does not, and the back affordance returns to the app — `docs/goal/behavior/admin.md` § Navigation model
   - `tests/e2e-unified/tests/test_admin_nav.py::test_admin_tab_visible_for_admin`
   - `tests/e2e-unified/tests/test_admin_nav.py::test_admin_tab_hidden_for_non_admin`
   - `tests/e2e-unified/tests/test_admin_nav.py::test_admin_nav_back_present`
   - `tests/e2e-unified/tests/test_admin_nav.py::test_admin_nav_back_exits_shell`
   - `tests/e2e-unified/tests/test_admin_nav.py::test_admin_nav_back_lands_on_primary_view`
2. [app] The dashboard shows member count and version — `docs/goal/behavior/admin.md` § 1. Dashboard
   - `tests/e2e-unified/tests/test_admin.py::test_admin_dashboard_loads`
   - `tests/e2e-unified/tests/test_admin.py::test_admin_dashboard_user_count`
   - `tests/e2e-unified/tests/test_admin.py::test_admin_dashboard_version`
3. [app] A non-admin who reaches an admin page sees the refusal on that page — `docs/goal/behavior/admin.md` § Navigation model
   - `tests/e2e-unified/tests/test_admin_error_surfacing.py::test_admin_mail_page_surfaces_fetch_error`
4. [nest] Only an admin may call admin operations — `docs/goal/architecture/nest/public-mode.md` § Admin Surface
   - `tests/e2e-unified/tests/api/test_admin_auth.py::test_am_i_admin_non_admin`
   - `tests/e2e-unified/tests/test_ws_rpc_admin_client.py::test_non_admin_actor_is_permission_denied_on_admin_kind`
5. [app] Inside the admin area its own navigation takes the place of the app's, and switches between the admin pages — `docs/goal/behavior/admin.md` § Navigation model
   - (none)
6. [app] Entering the admin area always opens on the dashboard, never on the page left open last time — `docs/goal/behavior/admin.md` § Navigation model
   - `tests/e2e-unified/tests/test_macos_shell_canonical_entry.py::test_admin_shell_reentry_after_leaving_a_deep_sub_page_lands_on_dashboard`
7. [app] The dashboard shows how much storage the nest is using — `docs/goal/behavior/admin.md` § 1. Dashboard
   - (none)
8. [app] The dashboard also shows suspended members, the members on each tier, how the storage splits between inboxes and files, live connections, the state of mail, whether connections are secure, the nest's domain and how people may join — `docs/goal/behavior/admin.md` § 1. Dashboard
   - (none)
9. [app] A dashboard read that fails shows the error and drops only its own cards, leaving the rest of the dashboard in place — `docs/goal/behavior/admin.md` § 1. Dashboard
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+f3c1e99a standalone |
| linux | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| windows | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+c438a386 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_nav.py::test_admin_tab_visible_for_admin` | windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_nav.py::test_admin_tab_hidden_for_non_admin` | web (linux): passed, web (windows): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_nav.py::test_admin_nav_back_present` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_nav.py::test_admin_nav_back_exits_shell` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_nav.py::test_admin_nav_back_lands_on_primary_view` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin.py::test_admin_dashboard_loads` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin.py::test_admin_dashboard_user_count` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin.py::test_admin_dashboard_version` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_error_surfacing.py::test_admin_mail_page_surfaces_fetch_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_admin_auth.py::test_am_i_admin_non_admin` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/test_ws_rpc_admin_client.py::test_non_admin_actor_is_permission_denied_on_admin_kind` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | app | (none) | — |
| 6 | app | `tests/e2e-unified/tests/test_macos_shell_canonical_entry.py::test_admin_shell_reentry_after_leaving_a_deep_sub_page_lands_on_dashboard` | — |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
<!-- features-render:end -->

---
slug: admin-tiers
title: Tiers and paid membership
section: admin area
goal: docs/goal/behavior/admin.md § 3. Settings
guide: docs/guides/admin-tour.md § Tiers
---

## What a user gets

Tiers set what a member may store and do. Edit each tier's caps in place, and
point paid membership at one of your own subscription tiers so access to the nest
can be sold.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] Tier definitions list and their caps edit in place — `docs/goal/behavior/admin.md` § 3. Settings
   - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_definitions`
   - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_edit`
   - `tests/e2e-unified/tests/test_admin_settings.py::test_admin_settings_loads`
2. [app] Paid membership points one of your subscription tiers at the quota tier its members get and the tier they drop to when it lapses, and clears again — `docs/goal/behavior/monetization.md` § Pillar 4
   - `tests/e2e-unified/tests/test_admin_settings.py::test_admin_designates_a_membership_tier_re_points_it_and_clears_it`
3. [nest] A tier's limits bind every member on it — inbox, storage, devices, upload size and feeds — and a limit the admin changes applies from then on — `docs/goal/architecture/nest/public-mode.md` § Tiers
   - (none)
4. [nest] A person who pays for membership is put on the quota tier the admin linked to it, and a member who buys it is moved to that tier at once — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
5. [nest] When a membership runs out the member drops to the tier chosen for lapsed members: they keep reading, keep everything already stored and can still export it, and are never suspended, evicted or deleted for it — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
6. [nest] A lapsed member who renews is put back on the paid tier — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
7. [nest] A tier the admin gave a member by hand is never undone by a membership running out — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
8. [nest] Re-pointing or clearing a membership link never changes the terms for members who already joined under it — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
9. [nest] People who join by paying do not count against the cap on free accounts — `docs/goal/behavior/monetization.md` § Pillar 4
   - (none)
10. [app] The admin changes which quota tier and which lapse tier a paid membership points at, without clearing it first — `docs/goal/behavior/monetization.md` § Pillar 4
    - (none)
11. [app] The admin defines a new tier from the page — `docs/goal/behavior/admin.md` § 3. Settings
    - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_new`
    - `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_refuses_empty_and_duplicate_names`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+f3c1e99a standalone |
| linux | ⚠ partial | 0.1.2-dev+78e73031 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_definitions` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_edit` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_settings.py::test_admin_settings_loads` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_settings.py::test_admin_designates_a_membership_tier_re_points_it_and_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | (none) | — |
| 4 | nest | (none) | — |
| 5 | nest | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | (none) | — |
| 9 | nest | (none) | — |
| 10 | app | (none) | — |
| 11 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_new` | linux (linux): failed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_admin.py::test_settings_tier_define_refuses_empty_and_duplicate_names` | linux (linux): failed, tui (linux): passed |
<!-- features-render:end -->

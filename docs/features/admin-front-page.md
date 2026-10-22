---
slug: admin-front-page
title: Web: whose site is the front page
section: admin area
goal: docs/goal/behavior/admin.md § 7. Web
guide: docs/guides/admin-tour.md § Web
---

## What a user gets

Pick one member whose published site serves at the nest's bare address, and
clear the choice again.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] The front-page member is designated and cleared from the page — `docs/goal/behavior/admin.md` § 7. Web
   - `tests/e2e-unified/tests/test_web_authoring.py::test_admin_apex_designate_and_clear`
2. [nest] The designated member's site serves at the bare address, only an admin may designate, and clearing stops it — `docs/goal/behavior/web-content-hosting.md` § Admin apex hosting
   - `tests/e2e-unified/tests/api/test_web_apex_hosting.py::test_admin_apex_hosting_serves_then_clears`
   - `tests/e2e-unified/tests/api/test_web_apex_hosting.py::test_set_apex_actor_is_admin_only`
3. [app] Two members who share a display name still appear as distinct, unambiguous choices in the front-page picker — `docs/goal/behavior/admin.md` § 2. Users
   - `tests/e2e-unified/tests/test_admin_picker_label_collision.py::test_admin_web_apex_picker_stays_injective_when_labels_collide`
4. [app] Any member can be chosen as the front page, however many members the nest holds — `docs/goal/behavior/admin.md` § 2. Users
   - `tests/e2e-unified/tests/test_admin_picker_all_accounts.py::test_admin_web_apex_picker_offers_an_account_older_than_the_newest_page`
5. [app] The page shows the address the chosen member's site serves at — `docs/goal/behavior/admin.md` § 7. Web
   - `tests/e2e-unified/tests/test_web_authoring.py::test_admin_front_page_shows_the_address_it_serves_at`
6. [app] Opening the page shows who is the front page now, or that nobody is — `docs/goal/behavior/admin.md` § 7. Web
   - `tests/e2e-unified/tests/test_web_authoring.py::test_admin_front_page_opens_on_who_is_the_front_page_now`
7. [nest] With no front page chosen, and again after the choice is cleared, the bare address shows the nest's own information page — `docs/goal/behavior/web-content-hosting.md` § Admin apex hosting
   - `tests/e2e-unified/tests/api/test_web_apex_hosting.py::test_admin_apex_hosting_serves_then_clears`
8. [nest] The chosen front page keeps serving after the nest restarts — `docs/goal/behavior/web-content-hosting.md` § Admin apex hosting
   - `tests/e2e-unified/tests/api/test_front_page_durability.py::test_the_chosen_front_page_keeps_serving_after_a_nest_restart`
9. [nest] When the front-page member's account passes to a successor identity, the front page keeps serving that account's site — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/api/test_front_page_durability.py::test_the_front_page_follows_its_member_to_a_successor_key`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+110d1248 standalone |
| linux | ✅ full | 0.1.4-dev+ea391d07 standalone |
| windows | ⚠ partial | 0.1.2-dev+c04c2468 standalone |
| macos | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| ios | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_admin_apex_designate_and_clear` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_web_apex_hosting.py::test_admin_apex_hosting_serves_then_clears` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_web_apex_hosting.py::test_set_apex_actor_is_admin_only` | nest (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_picker_label_collision.py::test_admin_web_apex_picker_stays_injective_when_labels_collide` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_admin_picker_all_accounts.py::test_admin_web_apex_picker_offers_an_account_older_than_the_newest_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_admin_front_page_shows_the_address_it_serves_at` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_admin_front_page_opens_on_who_is_the_front_page_now` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_web_apex_hosting.py::test_admin_apex_hosting_serves_then_clears` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_front_page_durability.py::test_the_chosen_front_page_keeps_serving_after_a_nest_restart` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_front_page_durability.py::test_the_front_page_follows_its_member_to_a_successor_key` | nest (linux): passed |
<!-- features-render:end -->

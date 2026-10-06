---
slug: admin-held-custody
title: Held custody
section: admin area
goal: docs/goal/architecture/account-replica-posture.md § The custody grant + ceremony
guide: docs/guides/admin-tour.md § Held Custody
---

## What a user gets

A page listing the sealed copies this nest holds for people from elsewhere,
honest when there are none.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] With nothing held for anyone, the page opens with no copies listed and no error — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_admin_custody_hosting_page_reachable_and_honestly_empty`
2. [app] The page lists every copy the nest holds for people elsewhere, whichever member set it up, each with that member and the address the nest fetches from — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
3. [app] The admin removes any held copy from the page, and it leaves the list — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
4. [nest] Removing the last copy held for a person deletes their stored copy and frees its space; removing one of several keeps it — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - (none)
5. [nest] A copy its holder has paused keeps its space until it is removed — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - (none)
6. [nest] A member can have the nest hold only a bounded number of copies: one more is refused, and an allowance asked too large is honoured at the nest's ceiling rather than refused — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - (none)
7. [nest] What a member's held copies use is bounded by the storage allowance of that member's tier: past it a new copy is refused, and the member's own files are never refused because of copies held for others — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - (none)
8. [nest] A held copy whose permission has expired is no longer fetched, and a month after the expiry its stored bytes are reclaimed while its entry stays on the list — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - (none)
9. [nest] When the owner takes their permission back, the nest stops fetching and serving that copy — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
   - (none)
10. [nest] A copy the nest holds for someone elsewhere is sealed: the nest stores and returns it and can never read it — `docs/goal/architecture/account-replica-posture.md` § Replica posture
    - (none)
11. [nest] The nest hands a held copy back only to its owner's own devices — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - (none)
12. [nest] What the nest holds for someone never grows past the allowance set for it — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - (none)
13. [app] The page says that nothing is held only once the nest has answered, never while it is still loading — `docs/goal/architecture/account-replica-posture.md` § The custody grant + ceremony
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+5cdec819.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.2-dev+ab96a0f8.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+923cec2d standalone |
| ios | ⚠ partial | 0.1.2-dev+923cec2d standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+5cdec819.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_custody_hosting.py::test_admin_custody_hosting_page_reachable_and_honestly_empty` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed |
| 4 | nest | (none) | — |
| 5 | nest | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | (none) | — |
| 9 | nest | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
| 13 | app | (none) | — |
<!-- features-render:end -->

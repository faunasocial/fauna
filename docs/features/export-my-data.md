---
slug: export-my-data
title: Export everything
section: your data and devices
goal: docs/goal/ui/settings.md § Data export
guide: docs/guides/your-own-cloud.md § The trust model, in plain words
---

## What a user gets

One button in Settings gives you an archive of everything your nest holds for
you.

## Coverage contract

Stamped 2026-09-19 at f62a5e4e1c.

1. [app] The export button delivers a complete, well-formed archive — `docs/goal/ui/settings.md` § Data export
   - `tests/e2e-unified/tests/test_export_my_data_journey.py::test_export_button_delivers_a_wellformed_archive`
2. [app] The archive holds your actual content — the message and post bodies, the files themselves — not just an index of what the nest has — `docs/goal/ui/settings.md` § Data export
   - `tests/e2e-unified/tests/test_export_my_data_journey.py::test_export_archive_carries_the_body_the_user_wrote`
3. [nest] The archive names what it does not contain: anything the nest holds back is listed with the reason, so "everything" is never a guess — `docs/goal/architecture/account-data-plane.md` § Nest-side requirements
   - `tests/e2e-unified/tests/api/test_export_coverage.py::test_archive_declares_what_it_withholds_by_name_and_reason`
4. [nest] Your keys, passphrases and escrowed secrets are never in the archive — `docs/goal/architecture/account-data-plane.md` § Nest-side requirements
   - `tests/e2e-unified/tests/api/test_export_coverage.py::test_archive_never_carries_key_or_escrow_material`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+72d6a508 standalone |
| linux | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| windows | ✅ full | 0.1.2-dev+920556fa standalone |
| macos | ✅ full | 0.1.2-dev+83158a20 standalone |
| ios | ✅ full | 0.1.2-dev+83158a20 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_export_my_data_journey.py::test_export_button_delivers_a_wellformed_archive` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_export_my_data_journey.py::test_export_archive_carries_the_body_the_user_wrote` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_export_coverage.py::test_archive_declares_what_it_withholds_by_name_and_reason` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_export_coverage.py::test_archive_never_carries_key_or_escrow_material` | nest (linux): passed |
<!-- features-render:end -->

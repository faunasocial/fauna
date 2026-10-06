---
slug: status-quotas-and-limits
title: Status, quotas and limits
section: your data and devices
goal: docs/goal/ui/status.md § Goal
guide: docs/guides/app-tour.md § Settings
---

## What a user gets

The Status page shows how much of your quota you use and every limit that
applies to you, which feature it binds, how much headroom is left, and who set it,
in readable units. No limit is ever silent.

## Coverage contract

Stamped 2026-09-22 at e9f79fdf25.

1. [app] Your quota shows with live values from the nest — `docs/goal/ui/status.md` § Layout & flow
   - `tests/e2e-unified/tests/test_settings.py::test_quota_section`
2. [app] Every limit names its feature, its headroom and the tier that set it, in readable units, with no raw placeholders — `docs/goal/architecture/dynamic-features.md` § Transparency & auditability
   - `tests/e2e-unified/tests/test_feature_limits.py::test_feature_limits_section_names_the_gated_members`
   - `tests/e2e-unified/tests/test_feature_limits.py::test_a_quota_cell_shows_the_limit_the_headroom_and_the_binding_tier`
   - `tests/e2e-unified/tests/test_feature_limits.py::test_a_byte_volume_cell_is_readable_rather_than_a_raw_count`
   - `tests/e2e-unified/tests/test_feature_limits.py::test_no_raw_i18n_key_reaches_the_feature_limits_screen`
3. [app] A limit an admin tightened shows up and says the admin set it — `docs/goal/architecture/dynamic-features.md` § Transparency & auditability
   - `tests/e2e-unified/tests/test_feature_limits.py::test_an_admin_limit_reaches_the_screen_naming_the_admin`
4. [app] You can copy your identity and your nest's address from the page — `docs/goal/ui/status.md` § Layout & flow
   - `tests/e2e-unified/tests/test_settings.py::test_status_copy_buttons_copy_your_identity_and_your_nests_address`
5. [app] The page names the nest you are on and its version — `docs/goal/ui/status.md` § Layout & flow
   - `tests/e2e-unified/tests/test_settings.py::test_status_names_your_nest_and_its_version`
6. [app] The page says how your file sync is doing — what is still pending and when the last pass finished — `docs/goal/ui/status.md` § Layout & flow
   - `tests/e2e-unified/tests/test_settings.py::test_status_says_when_your_last_sync_pass_finished`
   - `tests/e2e-unified/tests/test_settings.py::test_status_shows_your_pending_sync_backlog`
7. [app] Your encryption state shows read-only — key packages published and secure channels open — `docs/goal/ui/status.md` § Encryption / MLS key packages
   - `tests/e2e-unified/tests/test_settings.py::test_status_shows_your_encryption_state_read_only`
8. [app] The page names the build you are running — `docs/goal/ui/status.md` § Build
   - `tests/e2e-unified/tests/test_settings.py::test_status_names_the_build_you_are_running`
9. [nest] Your storage number is one figure covering mail and calendar together — `docs/goal/behavior/caldav-server.md` § QUOTA — shared with IMAP
   - `tests/e2e-unified/tests/api/test_shared_storage_quota.py::test_an_events_bytes_move_the_one_storage_number_mail_reads`
10. [app] A feature you may not use, or have used up, is shown disabled with its reason and who set it — never hidden — `docs/goal/architecture/dynamic-features.md` § What this is NOT (hard boundaries)
   - `tests/e2e-unified/tests/test_feature_limits.py::test_a_feature_the_admin_turned_off_shows_restricted_with_its_reason`
11. [app] A limit your guardian set shows here too and says it was your guardian — `docs/goal/architecture/dynamic-features.md` § Transparency & auditability
   - `tests/e2e-unified/tests/test_family.py::test_family_guardian_feature_limit_reaches_the_wards_status`
12. [app] An admin sets, reads back and removes a limit for everyone on the nest from the Nest page, and a looser value than what already applies is never refused but says it has no effect — `docs/goal/architecture/dynamic-features.md` § Authoring surfaces
   - `tests/e2e-unified/tests/test_feature_limits.py::test_an_admin_authors_a_limit_on_the_nest_page_and_it_binds_with_attribution`
   - `tests/e2e-unified/tests/test_feature_limits.py::test_a_looser_value_is_never_refused_but_says_it_has_no_effect`
13. [app] You can limit, turn off or remove a limit on a feature for yourself right from its row, the row shows it binding straight away, and the save is unavailable while you are offline — `docs/goal/architecture/dynamic-features.md` § Authoring surfaces
   - `tests/e2e-unified/tests/test_feature_limits.py::test_you_set_your_own_limit_and_the_row_above_binds_without_renavigating`
   - `tests/e2e-unified/tests/test_feature_limits.py::test_the_editor_writes_disable_offline_and_are_never_queued`
14. [nest] Once your storage is full, a new calendar event is refused and saves nothing, while making one smaller still works — `docs/goal/behavior/caldav-server.md` § Enforcement points
   - `tests/e2e-unified/tests/api/test_shared_storage_quota.py::test_an_event_past_the_storage_ceiling_is_refused_and_moves_nothing`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_settings.py::test_quota_section` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_feature_limits_section_names_the_gated_members` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_a_quota_cell_shows_the_limit_the_headroom_and_the_binding_tier` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_a_byte_volume_cell_is_readable_rather_than_a_raw_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_no_raw_i18n_key_reaches_the_feature_limits_screen` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_an_admin_limit_reaches_the_screen_naming_the_admin` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_settings.py::test_status_copy_buttons_copy_your_identity_and_your_nests_address` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_settings.py::test_status_names_your_nest_and_its_version` | web (linux): skipped, linux (linux): passed, windows (windows): skipped, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_settings.py::test_status_says_when_your_last_sync_pass_finished` | web (linux): skipped, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_settings.py::test_status_shows_your_pending_sync_backlog` | web (linux): skipped, linux (linux): skipped, windows (windows): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_settings.py::test_status_shows_your_encryption_state_read_only` | web (linux): skipped, linux (linux): skipped, windows (windows): skipped, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_settings.py::test_status_names_the_build_you_are_running` | web (linux): passed, linux (linux): skipped, windows (windows): skipped, tui (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_shared_storage_quota.py::test_an_events_bytes_move_the_one_storage_number_mail_reads` | nest (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_a_feature_the_admin_turned_off_shows_restricted_with_its_reason` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_family.py::test_family_guardian_feature_limit_reaches_the_wards_status` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_an_admin_authors_a_limit_on_the_nest_page_and_it_binds_with_attribution` | linux (linux): skipped, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_a_looser_value_is_never_refused_but_says_it_has_no_effect` | linux (linux): skipped, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_you_set_your_own_limit_and_the_row_above_binds_without_renavigating` | linux (linux): skipped, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_feature_limits.py::test_the_editor_writes_disable_offline_and_are_never_queued` | linux (linux): skipped, tui (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_shared_storage_quota.py::test_an_event_past_the_storage_ceiling_is_refused_and_moves_nothing` | nest (linux): passed |
<!-- features-render:end -->

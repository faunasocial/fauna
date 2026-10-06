---
slug: upgrades-never-lose-data
title: Upgrades never lose your data
section: your nest
goal: docs/goal/architecture/version-compatibility.md § 1. The compatibility contract (absolute invariants)
guide: docs/guides/install.md § What "alpha" means for you
---

## What a user gets

An older app talks to a newer nest and a newer app to an older nest, in the
same major version, and each app's own data opens under the next build and the
previous one. A nest handed data from a newer, incompatible build stays up but
serves nothing from that data rather than damage it, tells every app it needs an
update, and recovers untouched when the newer build returns. An app that cannot read the list of accounts on
your device never offers to start you over as if you had none.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [app] The previous release's app completes a full journey against a new nest, and a new app against the previous nest — `docs/goal/architecture/version-compatibility.md` § Dimension 6 — Support matrix & version-skew testing
   - (maintainer-only)
   - (maintainer-only)
   - (maintainer-only)
2. [app] An app's own data opens intact under the next build, and the previous build does not damage the next build's data — `docs/goal/architecture/version-compatibility.md` § I1 — Never destroy user-irrecoverable data (iron-clad)
   - (maintainer-only)
   - (maintainer-only)
3. [app] A nest that needs updating shows the app a message saying so, with no retry offered and the choice of another nest kept — `docs/goal/architecture/version-compatibility.md` § Dimension 4 — Honest version / compatibility error surfacing
   - `tests/e2e-unified/tests/test_version_mismatch_launch.py::test_outdated_nest_launch_shows_non_retry_update_surface`
   - `tests/e2e-unified/tests/test_version_mismatch_launch.py::test_outdated_nest_launch_shows_non_retry_update_surface_web`
4. [nest] A newer app's extra fields are ignored, on a read and on a save alike, and an older app's minimal requests are accepted; an unknown call gets a typed refusal and the connection stays usable — `docs/goal/architecture/version-compatibility.md` § Dimension 2 — Wire / WS-RPC evolution
   - `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_old_client_minimal_payloads_accepted_by_new_nest`
   - `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_new_client_extra_fields_ignored_by_old_nest`
   - `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_new_client_unknown_kind_gets_graceful_unknown_kind`
   - `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_new_app_policy_save_with_future_field_accepted_by_old_nest`
5. [nest] Handed data from an incompatible newer build, the nest stays up, serves nothing from that data and answers that it needs an update; newer data that only adds to what it knows is served normally — `docs/goal/architecture/version-compatibility.md` § 2.2 Schema version & honest downgrade detection
   - `tests/e2e-unified/tests/api/test_schema_version_compat.py::test_incompatible_schema_boots_degraded_and_serves_outdated`
   - `tests/e2e-unified/tests/api/test_schema_version_compat.py::test_newer_but_additive_schema_still_serves`
   - `tests/e2e-unified/tests/api/test_schema_version_compat.py::test_old_binary_serves_new_db_with_extra_table`
6. [app] An app that finds your list of accounts written by a newer version tells you to update it and offers nothing else — never a fresh start that would lose the accounts an update brings back — `docs/goal/behavior/onboarding.md` § App-launch routing
   - `tests/e2e-unified/tests/test_account_index_unreadable_launch.py::test_a_newer_builds_index_tells_the_user_to_update_and_offers_nothing_else`
7. [app] A list of accounts no version can read is never silently replaced: the app says nothing has been changed, and starting over is offered only behind a confirmation that first says what will be lost — `docs/goal/behavior/onboarding.md` § App-launch routing
   - `tests/e2e-unified/tests/test_account_index_unreadable_launch.py::test_a_malformed_index_reaches_the_floor_only_through_a_confirm_that_states_the_residual`
8. [app] One app keeps working with several nests at once when they run different versions of the same major version — `docs/goal/architecture/version-compatibility.md` § I2 — Full bidirectional compatibility within a major version
   - (none)
9. [app] A feature your nest is too old for is hidden or switched off in the app, instead of failing with a raw error when you try it — `docs/goal/architecture/version-compatibility.md` § Dimension 3 — Version & capability negotiation
   - (none)
10. [app] When someone on a newer app sends a kind of message your app does not know, you miss only that one message and the conversation keeps working — `docs/goal/architecture/version-compatibility.md` § MLS application-message payloads (client↔client)
   - (none)
11. [app] A send your nest refuses because it needs updating shows that reason and offers no retry — `docs/goal/architecture/version-compatibility.md` § Dimension 4 — Honest version / compatibility error surfacing
   - (none)
12. [nest] A request that fails because the nest's stored data does not match its software is answered as the nest needing an update, in your language, never with a database message — `docs/goal/architecture/version-compatibility.md` § Dimension 4 — Honest version / compatibility error surfacing
   - (none)
13. [nest] Once the newer build is back, the nest serves normally again with its admin and all its data intact — `docs/goal/architecture/version-compatibility.md` § 2.2 Schema version & honest downgrade detection
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_schema_upgrade.py::test_incompatible_schema_boots_degraded_then_recovers`
14. [app] A refusal from your nest that your app is too old to recognise is shown as a refusal, and the app does not keep retrying — `docs/goal/architecture/version-compatibility.md` § Dimension 4 — Honest version / compatibility error surfacing
   - (none)
15. [app] An app and a nest too far apart in version to work together tell you what to update, never fail with a raw error — `docs/goal/architecture/version-compatibility.md` § Dimension 6 — Support matrix & version-skew testing
   - (none)
16. [app] An app too old to read its own saved settings tells you to update it and that the settings are intact, instead of calling them damaged — `docs/goal/architecture/version-compatibility.md` § Dimension 1 — At-rest schema evolution (the nest DB + client-local stores)
   - (none)
17. [nest] Anyone can ask a nest its version without signing in, and is told only the major and minor number, never the exact build — `docs/goal/architecture/version-compatibility.md` § Dimension 3 — Version & capability negotiation
   - (none)
18. [nest] Upgrading your nest to a newer build keeps everything it already held; no upgrade removes data you cannot recreate — `docs/goal/architecture/version-compatibility.md` § I1 — Never destroy user-irrecoverable data (iron-clad)
   - (none)
19. [nest] A nest tells apps which features it has as a short list of plain names, and lists none it does not have — `docs/goal/architecture/version-compatibility.md` § Dimension 3 — Version & capability negotiation
   - `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_nest_info_advertises_capability_set`
20. [nest] When something you do with a person on another nest fails because their nest is too old for it, your nest says so in your language instead of failing with a raw error — `docs/goal/architecture/version-compatibility.md` § Dimension 5 — Federation across nest versions
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+29d63861 standalone |
| linux | ⚠ partial | 0.1.2-dev+29d63861 standalone |
| windows | ⚠ partial | 0.1.2-dev+29d63861 standalone |
| macos | ⚠ partial | 0.1.2-dev+29d63861 standalone |
| ios | ⚠ partial | 0.1.2-dev+29d63861 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+29d63861 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | (maintainer-only) | linux (linux): passed, windows (windows): passed |
| 1 | app | (maintainer-only) | linux (linux): passed, windows (windows): passed, macos (macos): passed |
| 1 | app | (maintainer-only) | — |
| 2 | app | (maintainer-only) | linux (linux): passed |
| 2 | app | (maintainer-only) | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_version_mismatch_launch.py::test_outdated_nest_launch_shows_non_retry_update_surface` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_version_mismatch_launch.py::test_outdated_nest_launch_shows_non_retry_update_surface_web` | web (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_old_client_minimal_payloads_accepted_by_new_nest` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_new_client_extra_fields_ignored_by_old_nest` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_new_client_unknown_kind_gets_graceful_unknown_kind` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_new_app_policy_save_with_future_field_accepted_by_old_nest` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_schema_version_compat.py::test_incompatible_schema_boots_degraded_and_serves_outdated` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_schema_version_compat.py::test_newer_but_additive_schema_still_serves` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_schema_version_compat.py::test_old_binary_serves_new_db_with_extra_table` | nest (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_account_index_unreadable_launch.py::test_a_newer_builds_index_tells_the_user_to_update_and_offers_nothing_else` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_account_index_unreadable_launch.py::test_a_malformed_index_reaches_the_floor_only_through_a_confirm_that_states_the_residual` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | nest | (none) | — |
| 13 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_schema_upgrade.py::test_incompatible_schema_boots_degraded_then_recovers` | — |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | nest | (none) | — |
| 18 | nest | (none) | — |
| 19 | nest | `tests/e2e-unified/tests/api/test_wire_version_skew.py::test_nest_info_advertises_capability_set` | nest (linux): passed |
| 20 | nest | (none) | — |
<!-- features-render:end -->

---
slug: encryption-settings
title: Ready for new conversations
section: everyday
goal: docs/goal/behavior/direct-messages.md § Key Package Management
guide: docs/guides/app-tour.md § Settings
---

## What a user gets

The people you let reach you can start an encrypted conversation with you at
any time, even while your devices are off: your app keeps a supply of one-use
starter keys waiting on your nest, and tops it up each time you sign in. The Encryption page in Settings shows how many are left, warns
you when the supply runs low, and tops it up when you ask.

## Coverage contract

Stamped 2026-10-13 at 899b58ab1b.

1. [app] Someone can start a new encrypted conversation with you without waiting for you to come online, because your app leaves a supply of starter keys on your nest and tops it up each time you sign in — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_each_sign_in_tops_up_the_supply_so_a_peer_can_start_while_you_are_away`
2. [app] Settings has an Encryption page showing how many starter keys are waiting for people who want to start a conversation with you — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_the_page_shows_the_nests_count_and_calls_it_low_below_five`
3. [app] When the supply runs low, the Encryption page says so — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_the_page_shows_the_nests_count_and_calls_it_low_below_five`
   - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_opening_the_page_never_tops_up_and_refresh_does`
4. [app] Refresh on the Encryption page tops the supply up on demand and shows the new count — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_opening_the_page_never_tops_up_and_refresh_does`
5. [nest] Each starter key is handed out once: when someone uses one to start a conversation with you, it is gone from your supply — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/api/test_mls_channels.py::test_key_package_lifecycle`
6. [nest] When your one-use starter keys run out, people can still start a conversation with you through a reusable last-resort key that is never used up — `docs/goal/architecture/federation.md` § Key packages — privacy & exhaustion
   - `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_once_the_one_use_keys_run_out_the_last_resort_key_keeps_you_reachable`
7. [nest] A starter key expires after thirty days: an expired one is never handed out and no longer counts in your supply — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_a_starter_key_expires_after_thirty_days`
8. [app] The Encryption page calls the supply low when fewer than five starter keys are left — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_the_page_shows_the_nests_count_and_calls_it_low_below_five`
9. [app] Opening the Encryption page only shows the count: the supply changes there only when you press Refresh — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_opening_the_page_never_tops_up_and_refresh_does`
10. [app] A new identity can be reached as soon as it is set up, because the app publishes its first starter keys right then — `docs/goal/behavior/direct-messages.md` § Key Package Management
    - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_a_new_identity_is_reachable_as_soon_as_it_is_set_up`
11. [app] Signing in on another device publishes fresh starter keys from that device — `docs/goal/behavior/devices.md` § User Experience
    - `tests/e2e-unified/tests/test_encryption_settings_page.py::test_signing_in_on_another_device_publishes_fresh_keys_from_it`
12. [nest] Someone looking you up learns only whether you can be reached, never how many starter keys you have left — `docs/goal/architecture/federation.md` § Key packages — privacy & exhaustion
    - `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_a_lookup_learns_only_whether_you_can_be_reached`
    - `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_a_signed_in_neighbour_learns_only_whether_you_can_be_reached`
13. [nest] Nobody can take your starter keys anonymously, and how fast any one nest can take them is limited, so another nest cannot drain your supply — `docs/goal/architecture/federation.md` § Key packages — privacy & exhaustion
    - `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_nobody_takes_starter_keys_anonymously_and_another_nest_cannot_drain_them`
14. [nest] Your nest refuses a starter key published under someone else's identity, so nobody starting a conversation with you is ever handed a key that is not yours — `docs/goal/ui/conversations.md` § Reactions & message delete
    - `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_the_nest_refuses_a_starter_key_published_under_someone_elses_identity`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.4-dev+52904896 standalone |
| linux | ⚠ partial | 0.1.4-dev+52904896 standalone |
| windows | ⚠ partial | 0.1.4-dev+52904896 standalone |
| macos | ⚠ partial | 0.1.4-dev+52904896 standalone |
| ios | ⚠ partial | 0.1.4-dev+52904896 standalone |
| android | ⚠ partial | 0.1.4-dev+52904896 standalone |
| tui | ✅ full | 0.1.4-dev+52904896 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_each_sign_in_tops_up_the_supply_so_a_peer_can_start_while_you_are_away` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_the_page_shows_the_nests_count_and_calls_it_low_below_five` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_the_page_shows_the_nests_count_and_calls_it_low_below_five` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_opening_the_page_never_tops_up_and_refresh_does` | tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_opening_the_page_never_tops_up_and_refresh_does` | tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mls_channels.py::test_key_package_lifecycle` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_once_the_one_use_keys_run_out_the_last_resort_key_keeps_you_reachable` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_a_starter_key_expires_after_thirty_days` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_the_page_shows_the_nests_count_and_calls_it_low_below_five` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_opening_the_page_never_tops_up_and_refresh_does` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_a_new_identity_is_reachable_as_soon_as_it_is_set_up` | tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_encryption_settings_page.py::test_signing_in_on_another_device_publishes_fresh_keys_from_it` | tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_a_lookup_learns_only_whether_you_can_be_reached` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_a_signed_in_neighbour_learns_only_whether_you_can_be_reached` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_nobody_takes_starter_keys_anonymously_and_another_nest_cannot_drain_them` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_starter_key_supply.py::test_the_nest_refuses_a_starter_key_published_under_someone_elses_identity` | nest (linux): passed |
<!-- features-render:end -->

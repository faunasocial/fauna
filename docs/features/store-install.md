---
slug: store-install
title: store-install
section: money
goal: docs/goal/architecture/dynamic-features.md § The store flavors
guide: docs/guides/install.md § Checking you have the official app
absences:
  web: "docs/goal/architecture/dynamic-features.md § The store flavors"
  linux: "docs/goal/architecture/dynamic-features.md § The store flavors"
  macos: "docs/goal/architecture/dynamic-features.md § The store flavors"
  tui: "docs/goal/architecture/dynamic-features.md § The store flavors"
---

## What a user gets

This text is being written and will appear here once it has been reviewed.

## Coverage contract

Stamped 2026-11-08 at 06de328c09.

1. [app] Installed from the store, Settings offers no payment provider to connect and no code to hand out or redeem for a paid tier — `docs/goal/architecture/dynamic-features.md` § Implementation status today
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_offers_no_payment_provider_or_code`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_offers_no_payment_provider_or_code`
2. [app] A tier you create or change has no price, price hint or payment link to fill in, and a tier that carries a price elsewhere shows as an ordinary tier to subscribe to, with no price — `docs/goal/architecture/dynamic-features.md` § Platform-family surface excision
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_tier_editor_has_no_price`
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_shows_a_priced_tier_without_its_price`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_tier_editor_has_no_price`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_shows_a_priced_tier_without_its_price`
3. [app] Writing a post never offers to sell it, and a post sold elsewhere shows as a gated post with its badge and no price, payment link or buy button — `docs/goal/architecture/dynamic-features.md` § Platform-family surface excision
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_never_offers_to_sell_a_post`
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_shows_a_sold_post_without_its_price`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_never_offers_to_sell_a_post`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_shows_a_sold_post_without_its_price`
4. [app] A post shows no tip total, tip count or tippers, and nobody can be named as your zap signer — `docs/goal/architecture/dynamic-features.md` § Implementation status today
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_shows_no_tips_or_zap_signer`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_shows_no_tips_or_zap_signer`
5. [app] Sharing a folder in person with someone else's device stays in the Google Play and Microsoft Store builds and is left out of the App Store build — `docs/goal/architecture/dynamic-features.md` § The store flavors
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_leaves_out_the_in_person_share`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_keeps_the_in_person_share`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | — absent | |
| windows |  no run recorded | |
| macos | — absent | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | — absent | |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_offers_no_payment_provider_or_code` | — |
| 1 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_offers_no_payment_provider_or_code` | — |
| 2 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_tier_editor_has_no_price` | — |
| 2 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_shows_a_priced_tier_without_its_price` | — |
| 2 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_tier_editor_has_no_price` | — |
| 2 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_shows_a_priced_tier_without_its_price` | — |
| 3 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_never_offers_to_sell_a_post` | — |
| 3 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_shows_a_sold_post_without_its_price` | — |
| 3 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_never_offers_to_sell_a_post` | — |
| 3 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_shows_a_sold_post_without_its_price` | — |
| 4 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_shows_no_tips_or_zap_signer` | — |
| 4 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_shows_no_tips_or_zap_signer` | — |
| 5 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_install_leaves_out_the_in_person_share` | — |
| 5 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_install_keeps_the_in_person_share` | — |
<!-- features-render:end -->

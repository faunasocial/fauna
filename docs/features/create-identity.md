---
slug: create-identity
title: Create your identity
section: getting in
goal: docs/goal/behavior/onboarding.md § 1. Identity
guide: docs/guides/getting-started.md § Step 1 — Create your identity
---

## What a user gets

The first thing the app does is make you a key. There is no account form, no
email address to confirm and no password: the key is your identity, it is created on
your device, and it never leaves it. You confirm you have saved it and move on to
choosing a handle.

## Coverage contract

Stamped 2026-09-19 at 0bb8814071.

1. [app] A fresh install offers to create a new identity or bring an existing one — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_onboarding.py::test_onboarding_identity_choice`
   - `tests/e2e-unified/tests/test_sp_onboarding.py::test_sign_in_button_visible`
2. [app] Creating an identity takes you on to the handle step — `docs/goal/behavior/onboarding.md` § 2. Handle entry
   - `tests/e2e-unified/tests/test_onboarding.py::test_web_onboarding_generate_identity`
   - `tests/e2e-unified/tests/test_handle_first_back_buttons.py::test_identity_created_back_returns_to_identity_choice`
3. [app] Confirming the identity records it durably on this device, so the next launch finds it — `docs/goal/behavior/onboarding.md` § Long-term store contract
   - `tests/e2e-unified/tests/test_confirmed_identity_survives_relaunch.py::test_a_confirmed_identity_is_found_by_the_next_launch`
   - `tests/e2e-unified/tests/test_onboarding.py::test_confirm_identity_commits_through_the_shared_registry`
4. [app] A malformed key is refused with a plain message, never a crash — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_onboarding_errors.py::test_invalid_secret_shows_i18n_error`
5. [app] Looking at the create screen and going back never swaps out a key you brought, and opening it again shows the same new key rather than a different one — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_identity_create_detour.py::test_the_create_screen_detour_still_signs_you_in_as_the_key_you_brought`
   - `tests/e2e-unified/tests/test_identity_create_detour.py::test_re_entering_the_create_screen_reshows_the_same_secret`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+4bc2efab standalone |
| linux | ✅ full | 0.1.2-dev+0207d4f9 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_onboarding.py::test_onboarding_identity_choice` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_sp_onboarding.py::test_sign_in_button_visible` | web (linux): passed, linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_onboarding.py::test_web_onboarding_generate_identity` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_handle_first_back_buttons.py::test_identity_created_back_returns_to_identity_choice` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_confirmed_identity_survives_relaunch.py::test_a_confirmed_identity_is_found_by_the_next_launch` | web (linux): passed, linux (linux): passed, windows (windows): failed, tui (linux): passed, tui (macos): passed, tui (windows): failed |
| 3 | app | `tests/e2e-unified/tests/test_onboarding.py::test_confirm_identity_commits_through_the_shared_registry` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_onboarding_errors.py::test_invalid_secret_shows_i18n_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_create_detour.py::test_the_create_screen_detour_still_signs_you_in_as_the_key_you_brought` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_identity_create_detour.py::test_re_entering_the_create_screen_reshows_the_same_secret` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->

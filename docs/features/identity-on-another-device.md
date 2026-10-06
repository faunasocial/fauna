---
slug: identity-on-another-device
title: Use your identity on another device
section: getting in
goal: docs/goal/ui/settings.md § Identity export
guide: docs/guides/identity-and-devices.md § Adding a device
---

## What a user gets

Your identity is a key, and a second device needs a copy. Settings shows it as a
code you reveal on purpose, with a warning that whoever scans it becomes you; the new
device pastes or scans it and is signed in as you. The code hides again as soon as
you are done.

## Coverage contract

Stamped 2026-09-19 at 0bb8814071.

1. [app] Your identity code stays hidden until you ask for it, appears together with its warning, and hides again on a second press — `docs/goal/ui/settings.md` § Identity export
   - `tests/e2e-unified/tests/test_identity_export.py::test_identity_qr_is_hidden_until_the_user_asks_for_it`
   - `tests/e2e-unified/tests/test_identity_export.py::test_identity_qr_and_its_warning_appear_together_on_show`
   - `tests/e2e-unified/tests/test_identity_export.py::test_identity_qr_hides_again_on_second_press`
2. [app] A new device takes the pasted key and lands you on the handle step, and going back returns to the choice screen — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_onboarding.py::test_identity_import`
   - `tests/e2e-unified/tests/test_handle_first_back_buttons.py::test_handle_entry_back_after_import_returns_to_identity_choice`
   - `tests/e2e-unified/tests/test_handle_first_back_buttons.py::test_identity_import_back_returns_to_identity_choice`
3. [app] Importing a different key restarts the handle check from scratch — `docs/goal/behavior/onboarding.md` § 2. Handle entry
   - `tests/e2e-unified/tests/test_identity_reimport_resets_handle_check.py::test_reimporting_a_different_key_restarts_the_handle_check`
   - `tests/e2e-unified/tests/test_onboarding_handle_check_reset.py::test_handle_check_resets_on_identity_reimport`
4. [app] An identity code that carries your handle fills the handle step in for you — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_identity_uri_handle_prefill.py::test_an_identity_code_carrying_a_handle_prefills_the_handle_step`
   - `tests/e2e-unified/tests/test_identity_uri_handle_prefill.py::test_an_identity_code_without_a_handle_leaves_the_handle_step_empty`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+4bc2efab standalone |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_identity_export.py::test_identity_qr_is_hidden_until_the_user_asks_for_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_identity_export.py::test_identity_qr_and_its_warning_appear_together_on_show` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_identity_export.py::test_identity_qr_hides_again_on_second_press` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_onboarding.py::test_identity_import` | web (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_handle_first_back_buttons.py::test_handle_entry_back_after_import_returns_to_identity_choice` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_handle_first_back_buttons.py::test_identity_import_back_returns_to_identity_choice` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_identity_reimport_resets_handle_check.py::test_reimporting_a_different_key_restarts_the_handle_check` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_onboarding_handle_check_reset.py::test_handle_check_resets_on_identity_reimport` | web (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_uri_handle_prefill.py::test_an_identity_code_carrying_a_handle_prefills_the_handle_step` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_identity_uri_handle_prefill.py::test_an_identity_code_without_a_handle_leaves_the_handle_step_empty` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->

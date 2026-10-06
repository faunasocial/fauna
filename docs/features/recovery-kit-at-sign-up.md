---
slug: recovery-kit-at-sign-up
title: Recovery kit at sign-up
section: getting in
goal: docs/goal/behavior/onboarding.md § 1. Identity
guide: docs/guides/identity-and-devices.md § Your recovery kit
---

## What a user gets

Right after your identity is created, the app offers to make a recovery kit: a
phrase you write down that can take your account back if the key is ever lost or
stolen. You can skip it in one tap and make one later from Settings; skipping never
blocks sign-up.

## Coverage contract

Stamped 2026-09-19 at 0bb8814071.

1. [app] A kit you make during sign-up is registered with your nest once you are signed in — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_the_kit_minted_at_onboarding_is_registered_by_the_signed_in_handoff`
2. [app] Skipping the kit registers nothing, and Settings still offers to make one — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_skipping_the_kit_registers_nothing_and_says_so`
3. [app] The sign-up kit screen says the kit becomes active once your account is online, and never that you are already protected — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_the_kit_minted_at_onboarding_is_registered_by_the_signed_in_handoff`
4. [app] If registering the kit fails, or you never finish signing in, Settings shows the kit as not active instead of claiming it — `docs/goal/behavior/onboarding.md` § 1. Identity
   - `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_confirming_the_kit_then_never_signing_in_leaves_settings_honest`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ❌ failing | 0.1.2-dev+45b4b867 standalone |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ✅ full | 0.1.2-dev+3f8c34d8.dirty standalone |
| macos | ✅ full | 0.1.2-dev+abeb05ca standalone |
| ios | ✅ full | 0.1.2-dev+abeb05ca standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_the_kit_minted_at_onboarding_is_registered_by_the_signed_in_handoff` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_skipping_the_kit_registers_nothing_and_says_so` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_the_kit_minted_at_onboarding_is_registered_by_the_signed_in_handoff` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_recovery_kit_create_journey.py::test_confirming_the_kit_then_never_signing_in_leaves_settings_honest` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->

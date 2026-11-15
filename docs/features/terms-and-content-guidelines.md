---
slug: terms-and-content-guidelines
title: terms-and-content-guidelines
section: getting in
goal: docs/goal/behavior/terms-and-content-guidelines.md § The decisions
guide: docs/guides/getting-started.md § Step 2 — Enter your handle
---

## What a user gets

This text is being written and will appear here once it has been reviewed.

## Coverage contract

Stamped 2026-10-27 at 7942c4870f.

1. [app] Asking to join a nest, redeeming a claim code and setting up a nest each tell you that continuing means agreeing to the Fauna terms and content guidelines, with a link to them — `docs/goal/behavior/terms-and-content-guidelines.md` § The decisions
   - `tests/e2e-unified/tests/test_terms_notice.py::test_invite_request_shows_the_terms_notice`
   - `tests/e2e-unified/tests/test_terms_notice.py::test_claim_code_shows_the_terms_notice`
   - `tests/e2e-unified/tests/test_terms_notice.py::test_nest_provisioning_shows_the_terms_notice`
2. [app] Entering a handle shows the notice exactly when continuing will sign you in to a nest — `docs/goal/behavior/terms-and-content-guidelines.md` § The decisions
   - `tests/e2e-unified/tests/test_terms_notice.py::test_handle_entry_shows_the_notice_only_when_continue_signs_in`
3. [app] Settings has a link to the terms and content guidelines next to the app's version — `docs/goal/behavior/terms-and-content-guidelines.md` § The decisions
   - `tests/e2e-unified/tests/test_terms_link_settings.py::test_settings_links_the_terms_and_content_guidelines`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+f43f877a standalone |
| linux | ✅ full | 0.1.5-dev+cbd813be standalone |
| windows |  no run recorded | |
| macos | ✅ full | 0.1.5-dev+3ef45f1b standalone |
| ios | ✅ full | 0.1.5-dev+52eff9ec standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.6-dev+2f2257c1 standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_terms_notice.py::test_invite_request_shows_the_terms_notice` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_terms_notice.py::test_claim_code_shows_the_terms_notice` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_terms_notice.py::test_nest_provisioning_shows_the_terms_notice` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_terms_notice.py::test_handle_entry_shows_the_notice_only_when_continue_signs_in` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_terms_link_settings.py::test_settings_links_the_terms_and_content_guidelines` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->

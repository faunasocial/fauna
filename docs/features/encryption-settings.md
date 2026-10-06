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

Stamped 2026-10-01 at dfa27a899f.

1. [app] Someone can start a new encrypted conversation with you without waiting for you to come online, because your app leaves a supply of starter keys on your nest and tops it up each time you sign in — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - (none)
2. [app] Settings has an Encryption page showing how many starter keys are waiting for people who want to start a conversation with you — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - (none)
3. [app] When the supply runs low, the Encryption page says so — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - (none)
4. [app] Refresh on the Encryption page tops the supply up on demand and shows the new count — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - (none)
5. [nest] Each starter key is handed out once: when someone uses one to start a conversation with you, it is gone from your supply — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - `tests/e2e-unified/tests/api/test_mls_channels.py::test_key_package_lifecycle`
6. [nest] When your one-use starter keys run out, people can still start a conversation with you through a reusable last-resort key that is never used up — `docs/goal/architecture/federation.md` § Key packages — privacy & exhaustion
   - (none)
7. [nest] A starter key expires after thirty days: an expired one is never handed out and no longer counts in your supply — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - (none)
8. [app] The Encryption page calls the supply low when fewer than five starter keys are left — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - (none)
9. [app] Opening the Encryption page only shows the count: the supply changes there only when you press Refresh — `docs/goal/behavior/direct-messages.md` § Key Package Management
   - (none)
10. [app] A new identity can be reached as soon as it is set up, because the app publishes its first starter keys right then — `docs/goal/behavior/direct-messages.md` § Key Package Management
    - (none)
11. [app] Signing in on another device publishes fresh starter keys from that device — `docs/goal/behavior/devices.md` § User Experience
    - (none)
12. [nest] Someone looking you up learns only whether you can be reached, never how many starter keys you have left — `docs/goal/architecture/federation.md` § Key packages — privacy & exhaustion
    - (none)
13. [nest] Nobody can take your starter keys anonymously, and how fast any one nest can take them is limited, so another nest cannot drain your supply — `docs/goal/architecture/federation.md` § Key packages — privacy & exhaustion
    - (none)
14. [nest] Your nest refuses a starter key published under someone else's identity, so nobody starting a conversation with you is ever handed a key that is not yours — `docs/goal/ui/conversations.md` § Reactions & message delete
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| tui | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | (none) | — |
| 2 | app | (none) | — |
| 3 | app | (none) | — |
| 4 | app | (none) | — |
| 5 | nest | `tests/e2e-unified/tests/api/test_mls_channels.py::test_key_package_lifecycle` | nest (linux): passed |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | nest | (none) | — |
| 13 | nest | (none) | — |
| 14 | nest | (none) | — |
<!-- features-render:end -->

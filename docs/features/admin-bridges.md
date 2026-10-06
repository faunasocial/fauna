---
slug: admin-bridges
title: Bridges: approve and rotate keys
section: admin area
goal: docs/goal/behavior/admin.md § Approved-bridges roster
guide: docs/guides/admin-tour.md § Bridges
---

## What a user gets

The nest's own mail helpers enrol themselves and are approved by themselves once
mail is on; anything else, the Bluesky helper included, waits here for you. Each approved helper
shows what it does, and its key can be rotated in one click with a warning about
what that costs.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] A pending helper shows as a card naming what it does, and approving it takes it off the list — `docs/goal/behavior/admin.md` § Bridge display naming
   - `tests/e2e-unified/tests/test_admin_bridges_pending.py::test_admin_bridges_pending_lists_and_approves`
   - `tests/e2e-unified/tests/test_admin_bridges_pending.py::test_mda_card_names_mail_and_calendar`
2. [app] Rotating an approved helper's key from its row shows a warning first, on every rotation, and never one about signing records (a re-key touches no signing key); on confirming, the helper leaves the approved list — `docs/goal/behavior/admin.md` § Approved-bridges roster
   - `tests/e2e-unified/tests/test_admin_bridges_rotate.py::test_admin_rotates_approved_mta_service_user_key`
   - `tests/e2e-unified/tests/test_admin_bridges_rotate.py::test_mda_rotation_hides_dkim_warning`
3. [nest] A mail helper that asks to join while mail is off waits as pending; once mail is on it is approved by itself, and one that asks after that is approved at once without ever showing as pending. A waiting helper is listed, approved or rejected by an admin only, and a rejected one is recorded as revoked — `docs/goal/behavior/mail-bridge-lifecycle.md` § Onboarding auto-approval (the box's own bridges)
   - `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_request_enrollment_auto_approves_when_mail_enabled`
   - `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_list_then_approve_flow`
   - `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_reject_flow`
   - `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_non_admin_denied`
4. [nest] After an admin revokes a mail helper's key, the helper makes itself a new key and is approved again with nobody touching the server: the old key stays revoked, the other mail helper is left as it was, and the mail server answers connections again — `docs/goal/behavior/mail-bridge-lifecycle.md` § Service-user re-keying
   - `tests/e2e-unified/tests/platform/docker/test_bridge_rekey_rotation.py::test_admin_revoke_re_keys_mta_hands_free`
   - `tests/e2e-unified/tests/test_mail_bridge_rekey.py::test_revoked_keyfile_re_keys_in_process`
5. [app] Each approved helper is listed with its technical role and its key — `docs/goal/behavior/admin.md` § Approved-bridges roster
   - `tests/e2e-unified/tests/test_admin_bridges_rotate.py::test_admin_rotates_approved_mta_service_user_key`
6. [app] A pending helper's card shows its technical role and its key, to check against the helper's own log — `docs/goal/behavior/mail-bridge-lifecycle.md` § Pending approval
   - `tests/e2e-unified/tests/test_admin_bridges_pending.py::test_admin_bridges_pending_lists_and_approves`
7. [app] The admin rejects a pending helper from its card, after confirming that it will not be able to connect again, and it leaves the list — `docs/goal/behavior/mail-bridge-lifecycle.md` § Pending approval
   - (none)
8. [nest] A helper the admin rejected stays rejected: it never returns as pending or approves itself, even once mail is on — `docs/goal/behavior/mail-bridge-lifecycle.md` § Onboarding auto-approval
   - (none)
9. [app] Cancelling a key rotation leaves the helper approved with its key unchanged — `docs/goal/behavior/admin.md` § Approved-bridges roster
   - (none)
10. [nest] With only calendar, contacts or files switched on and mail off, the mail-and-calendar helper still approves itself, and the mail-sending helper waits for mail — `docs/goal/behavior/mail-bridge-lifecycle.md` § Onboarding auto-approval
    - (none)
11. [nest] The Bluesky helper, and any helper other than the two mail ones, always waits for the admin's approval, whatever is switched on — `docs/goal/behavior/mail-bridge-lifecycle.md` § Onboarding auto-approval
    - (none)
12. [nest] The moment a helper's key is rotated or revoked, the old key loses its connection and its access to the nest — `docs/goal/behavior/mail-bridge-lifecycle.md` § Service-user re-keying
    - (none)
13. [app] After the mail-sending helper's key is rotated, each domain's signing record shows as out of date on the DNS page until it is republished — `docs/goal/behavior/mail-bridge-lifecycle.md` § Service-user re-keying
    - (none)
14. [nest] A helper still waiting for approval serves no mail — `docs/goal/behavior/mail-bridge-lifecycle.md` § Don't do these
    - (none)
15. [nest] On a nest with mail off, a re-keyed helper comes back as a new pending card and waits for the admin instead of approving itself — `docs/goal/behavior/mail-bridge-lifecycle.md` § Service-user re-keying
    - (none)
16. [app] Each approved helper's row also shows its name and when it was approved — `docs/goal/behavior/admin.md` § Approved-bridges roster
    - (none)
17. [app] A pending helper's card also shows when it first asked to join — `docs/goal/behavior/mail-bridge-lifecycle.md` § Pending approval
    - (none)
18. [nest] After a mail helper's key is rotated, mail flows again: a message arrives and a message leaves — `docs/goal/behavior/mail-bridge-lifecycle.md` § Service-user re-keying
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+cdb1bad0 standalone |
| linux | ⚠ partial | 0.1.2-dev+d4125106 standalone |
| windows | ⚠ partial | 0.1.2-dev+a704dbe4.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+9746302c.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_bridges_pending.py::test_admin_bridges_pending_lists_and_approves` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_bridges_pending.py::test_mda_card_names_mail_and_calendar` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_bridges_rotate.py::test_admin_rotates_approved_mta_service_user_key` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_bridges_rotate.py::test_mda_rotation_hides_dkim_warning` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_request_enrollment_auto_approves_when_mail_enabled` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_list_then_approve_flow` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_reject_flow` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_mail_bridge_approval.py::test_non_admin_denied` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_rekey_rotation.py::test_admin_revoke_re_keys_mta_hands_free` | — |
| 4 | nest | `tests/e2e-unified/tests/test_mail_bridge_rekey.py::test_revoked_keyfile_re_keys_in_process` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_bridges_rotate.py::test_admin_rotates_approved_mta_service_user_key` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_admin_bridges_pending.py::test_admin_bridges_pending_lists_and_approves` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 7 | app | (none) | — |
| 8 | nest | (none) | — |
| 9 | app | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
| 13 | app | (none) | — |
| 14 | nest | (none) | — |
| 15 | nest | (none) | — |
| 16 | app | (none) | — |
| 17 | app | (none) | — |
| 18 | nest | (none) | — |
<!-- features-render:end -->

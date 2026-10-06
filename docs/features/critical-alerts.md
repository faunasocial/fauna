---
slug: critical-alerts
title: Loud warnings when something is very wrong
section: everyday
goal: docs/goal/behavior/critical-alerts.md § Goal
guide: docs/guides/identity-and-devices.md § If a device is lost or stolen
---

## What a user gets

Some things are too important for a page you might never open: someone parked
a replacement of your recovery kit, your domain is about to lapse, your hosted
AT Protocol identity was moved. The app checks for these every time it starts and shows
a banner on every page until the condition clears; it never treats "could not check"
as "all clear".

## Coverage contract

Stamped 2026-09-19 at 8ac16765e4.

1. [app] A pending replacement of your recovery kit is announced on the feed after the next start — `docs/goal/behavior/critical-alerts.md` § Mechanism
   - `tests/e2e-unified/tests/test_session_start_alert_sweep.py::test_session_start_sweep_raises_the_pending_replacement_on_a_page_it_never_visited`
2. [app] A domain about to lapse is announced, a renewal clears it, and a failed check leaves a standing warning alone — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Domain loss: lapse and seizure
   - `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it`
   - `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_an_rdap_failure_leaves_a_standing_alarm_alone`
3. [app] A hosted AT Protocol identity that was moved away from you, or whose handle binding changed, is announced — `docs/goal/behavior/atproto-identity-custody.md` § The audit floor and the departed-DID alarm
   - `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_directory_feeders_alarm_via_session_start_sweep`
   - `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_did_web_identity_never_triggers_handle_binding_alarm`
   - `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_custody_alarm_reaches_a_fresh_sign_in_before_the_runtime_assembles`
   - `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_genesis_seniority_alarm_end_to_end`
4. [app] A standing alert carries no way to dismiss it: it stays on every page until the condition is checked again and found resolved — `docs/goal/behavior/critical-alerts.md` § Goal
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_standing_alert_carries_no_way_to_dismiss_it`
5. [app] Signing out or switching accounts takes every standing alert with it — the next identity starts with a clean banner — `docs/goal/behavior/critical-alerts.md` § Mechanism
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_signing_out_takes_every_standing_alert_with_it`
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_switching_accounts_takes_every_standing_alert_with_it`
6. [app] A condition that arises while the app is already open is announced without a restart — `docs/goal/behavior/critical-alerts.md` § Mechanism
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_condition_arising_mid_session_is_announced_without_a_restart`
7. [app] A hosted identity your app itself knows about is still checked when the nest stops naming it, and a standing identity alarm is never cleared by the nest's silence or an unreadable directory — `docs/goal/behavior/atproto-identity-custody.md` § The audit floor and the departed-DID alarm
   - `tests/e2e-unified/tests/test_audit_floor_nest_silence.py::test_the_audit_floor_outlives_the_nests_silence`
8. [app] A lapsing deployment domain warns people who are not admins too, with the remedy that is theirs — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Domain loss: lapse and seizure
   - `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it`
   - `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_an_admin_is_warned_with_the_remedy_that_is_theirs`
9. [app] A member whose account is being evicted is told on every page when they will lose access and when their data will be deleted, for as long as the eviction stands — `docs/goal/behavior/critical-alerts.md` § Feeders
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_session_start_alert_sweep.py::test_session_start_sweep_raises_the_pending_replacement_on_a_page_it_never_visited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_an_rdap_failure_leaves_a_standing_alarm_alone` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_directory_feeders_alarm_via_session_start_sweep` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_did_web_identity_never_triggers_handle_binding_alarm` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_custody_alarm_reaches_a_fresh_sign_in_before_the_runtime_assembles` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_genesis_seniority_alarm_end_to_end` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_standing_alert_carries_no_way_to_dismiss_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_signing_out_takes_every_standing_alert_with_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_switching_accounts_takes_every_standing_alert_with_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_condition_arising_mid_session_is_announced_without_a_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_audit_floor_nest_silence.py::test_the_audit_floor_outlives_the_nests_silence` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_an_admin_is_warned_with_the_remedy_that_is_theirs` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | (none) | — |
<!-- features-render:end -->

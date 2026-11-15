---
slug: critical-alerts
title: Loud warnings when something is very wrong
section: everyday
goal: docs/goal/behavior/critical-alerts.md § Goal
guide: docs/guides/identity-and-devices.md § If a device is lost or stolen
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Some things are too important for a page you might never open: someone has
started replacing your recovery kit, your domain is about to lapse, your hosted
AT Protocol identity was moved. The app checks for these every time it starts
and shows a banner on every page until the condition clears; it never treats
"could not check" as "all clear".

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

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
5. [app] Whenever your identity leaves the app — signing out, switching accounts, untrusting the nest, a superseded identity or a factory reset — every standing alert goes with it, and the next identity starts with a clean banner — `docs/goal/behavior/critical-alerts.md` § Mechanism
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_signing_out_takes_every_standing_alert_with_it`
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_switching_accounts_takes_every_standing_alert_with_it`
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_nest_the_app_no_longer_trusts_takes_every_standing_alert_with_it`
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_superseded_identity_takes_every_standing_alert_with_it`
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_factory_reset_takes_every_standing_alert_with_it`
6. [app] A condition that arises while the app is already open is announced without a restart — `docs/goal/behavior/critical-alerts.md` § Mechanism
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_condition_arising_mid_session_is_announced_without_a_restart`
   - `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_condition_arising_after_a_same_identity_re_establish_is_announced`
7. [app] A hosted identity your app itself knows about is still checked when the nest stops naming it, and a standing identity alarm is never cleared by the nest's silence or an unreadable directory — `docs/goal/behavior/atproto-identity-custody.md` § The audit floor and the departed-DID alarm
   - `tests/e2e-unified/tests/test_audit_floor_nest_silence.py::test_the_audit_floor_outlives_the_nests_silence`
8. [app] A lapsing deployment domain warns people who are not admins too, with the remedy that is theirs — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Domain loss: lapse and seizure
   - `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it`
   - `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_an_admin_is_warned_with_the_remedy_that_is_theirs`
9. [app] A member whose account is being evicted is told on every page when they will lose access and when their data will be deleted, for as long as the eviction stands — `docs/goal/behavior/critical-alerts.md` § Feeders
   - `tests/e2e-unified/tests/test_eviction_warning_alert_e2e.py::test_an_evicted_member_is_warned_with_both_dates_and_a_cancel_clears_it`
10. [app] A pending replacement of your recovery kit at any linked nest raises the one alert, which clears only once every nest reads nothing pending; an unreachable nest never clears it — `docs/goal/behavior/critical-alerts.md` § Feeders
   - `tests/e2e-unified/tests/test_recovery_linked_nest_alarm.py::test_the_alarm_stands_while_any_nest_holds_a_window_and_clears_once_none_does`
   - `tests/e2e-unified/tests/test_recovery_linked_nest_alarm.py::test_an_unreachable_linked_nest_never_clears_the_alarm`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.5-dev+cbd813be standalone |
| windows | ⚠ partial | 0.1.6-dev+6313dfdf.dirty standalone |
| macos | ⚠ partial | 0.1.5-dev+3ef45f1b standalone |
| ios | ⚠ partial | 0.1.5-dev+17b5832c standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+646e0070 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_session_start_alert_sweep.py::test_session_start_sweep_raises_the_pending_replacement_on_a_page_it_never_visited` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_an_rdap_failure_leaves_a_standing_alarm_alone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_directory_feeders_alarm_via_session_start_sweep` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_did_web_identity_never_triggers_handle_binding_alarm` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_alert_sweep_directory_feeders_e2e.py::test_custody_alarm_reaches_a_fresh_sign_in_before_the_runtime_assembles` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_genesis_seniority_alarm_end_to_end` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_standing_alert_carries_no_way_to_dismiss_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_signing_out_takes_every_standing_alert_with_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_switching_accounts_takes_every_standing_alert_with_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_nest_the_app_no_longer_trusts_takes_every_standing_alert_with_it` | tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_superseded_identity_takes_every_standing_alert_with_it` | tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_factory_reset_takes_every_standing_alert_with_it` | tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_condition_arising_mid_session_is_announced_without_a_restart` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_critical_alert_lifetime.py::test_a_condition_arising_after_a_same_identity_re_establish_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_audit_floor_nest_silence.py::test_the_audit_floor_outlives_the_nests_silence` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_a_lapsing_registration_reaches_the_every_page_banner_and_a_renewal_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_domain_expiry_alert_e2e.py::test_an_admin_is_warned_with_the_remedy_that_is_theirs` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_eviction_warning_alert_e2e.py::test_an_evicted_member_is_warned_with_both_dates_and_a_cancel_clears_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_recovery_linked_nest_alarm.py::test_the_alarm_stands_while_any_nest_holds_a_window_and_clears_once_none_does` | tui (linux): passed, tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_recovery_linked_nest_alarm.py::test_an_unreachable_linked_nest_never_clears_the_alarm` | tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->

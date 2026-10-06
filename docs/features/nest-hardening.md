---
slug: nest-hardening
title: Each part of the nest is walled off
section: your nest
goal: docs/goal/architecture/security.md § Co-resident process trust boundary (UID isolation)
guide: docs/guides/your-own-cloud.md § The trust model, in plain words
---

## What a user gets

There is nothing to choose: inside the released image every network-facing part runs as its own user,
the parts that parse hostile mail are sandboxed, none can read another's keys or
your sealed data, a helper cannot forge its own enrolment, and the nest reports all
of that to its admin over its own connection, so nobody needs a shell to check it.
A stop is a clean goodbye to every connected app.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [nest] The nest, its two mail helpers and its router each run as their own user, and the mail helpers run in a sandbox that keeps the nest's sealed data out of their reach — `docs/goal/architecture/security.md` § Co-resident process trust boundary (UID isolation)
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_each_service_runs_under_its_own_uid`
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_mail_bridges_run_under_fauna_sandbox`
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_landlock_denies_node_db_in_real_bridge_context`
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_landlock_denies_sealed_store_inside_sandbox_domain`
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_all_services_still_serve_after_split`
2. [nest] A helper cannot read another helper's key, the nest's sealed store, or the router's secret — `docs/goal/architecture/security.md` § Co-resident process trust boundary (UID isolation)
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_bridge_uid_cannot_read_peer_key_or_sealed_store`
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_key_files_and_dirs_have_expected_owner_and_mode`
   - `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_router_secret_file_is_root_only_and_bridges_denied`
   - `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_secret_not_on_any_world_readable_cmdline`
   - `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_secret_env_channel_is_non_dumpable_and_isolated`
   - `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_mda_env_channel_is_non_dumpable_and_isolated`
3. [nest] A mail helper cannot enrol under a forged identity or swap the identity the nest was given for it; the two real mail helpers enrol and are approved, and a request to enrol from outside the nest is refused — `docs/goal/architecture/security.md` § Enrollment proof-of-possession contract (slice 2)
   - `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_forged_fresh_pubkey_enrollment_is_rejected`
   - `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_bridge_uid_can_read_but_not_substitute_blessed_pubkey`
   - `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_both_blessed_and_signing_bridges_auto_approve`
   - `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_external_request_enrollment_refused_over_router`
4. [nest] The confinement is reported to the admin over the wire, with no shell — `docs/goal/architecture/security.md` § Confinement self-probe (the no-SSH observable for slices 1 + 4)
   - `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_bridges_self_report_their_confinement_over_the_wire`
   - `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_enrollment_strict_reported_on_provisioned_box`
5. [nest] Stopping the nest tells a connected app that the nest is going away, so the app reconnects by itself instead of treating it as an error — `docs/goal/architecture/transport-connection.md` § Graceful shutdown
   - `tests/e2e-unified/tests/platform/docker/test_graceful_shutdown.py::test_docker_stop_emits_ws_1001`
6. [nest] A mail helper that starts without its walls, its sandbox off or the nest's sealed store within its reach, shows up as a warning in the admin's log that names what is wrong — `docs/goal/architecture/security.md` § Confinement self-probe
   - (none)
7. [nest] A nest set up without its helpers' identities says so: the admin's read-out reports that helper enrolment is not strict, and the nest's log carries a warning — `docs/goal/architecture/security.md` § Implementation status — UID isolation
   - `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_lenient_box_forged_enrollment_is_accepted`
8. [nest] A request an app had already sent when the nest begins to stop is still finished and answered before the connection closes — `docs/goal/architecture/transport-connection.md` § Graceful shutdown
   - (none)
9. [nest] A compromised helper cannot fake where a connection comes from, so the per-address limits and the mail sign-in lockout cannot be dodged from inside the nest — `docs/goal/architecture/security.md` § Co-resident process trust boundary (UID isolation)
   - (none)
10. [nest] The relay and the helper that connects the nest to another network each run as their own user too — `docs/goal/architecture/security.md` § Co-resident process trust boundary (UID isolation)
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| macos | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| ios | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| android | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| tui | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_each_service_runs_under_its_own_uid` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_mail_bridges_run_under_fauna_sandbox` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_landlock_denies_node_db_in_real_bridge_context` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_landlock_denies_sealed_store_inside_sandbox_domain` | — |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_all_services_still_serve_after_split` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_bridge_uid_cannot_read_peer_key_or_sealed_store` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_key_files_and_dirs_have_expected_owner_and_mode` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_router_secret_file_is_root_only_and_bridges_denied` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_secret_not_on_any_world_readable_cmdline` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_secret_env_channel_is_non_dumpable_and_isolated` | — |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_proxy_router_auth.py::test_mda_env_channel_is_non_dumpable_and_isolated` | — |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_forged_fresh_pubkey_enrollment_is_rejected` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_bridge_uid_can_read_but_not_substitute_blessed_pubkey` | — |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_both_blessed_and_signing_bridges_auto_approve` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_external_request_enrollment_refused_over_router` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py::test_bridges_self_report_their_confinement_over_the_wire` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_enrollment_strict_reported_on_provisioned_box` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_graceful_shutdown.py::test_docker_stop_emits_ws_1001` | nest (linux): passed |
| 6 | nest | (none) | — |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_enrollment_pop.py::test_lenient_box_forged_enrollment_is_accepted` | — |
| 8 | nest | (none) | — |
| 9 | nest | (none) | — |
| 10 | nest | (none) | — |
<!-- features-render:end -->

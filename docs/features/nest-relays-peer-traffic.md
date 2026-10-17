---
slug: nest-relays-peer-traffic
title: Your nest relays your devices' direct traffic
section: your nest
goal: docs/goal/behavior/p2p.md § Architecture
guide: docs/guides/cloud-sync.md § What syncing feels like
---

## What a user gets

There is nothing to choose: a nest with a public name of its own runs a relay for your devices'
direct connections, at its own address on the standard port, and tells your apps it
is there. When two of your devices cannot reach each other directly, their traffic
passes through the relay, encrypted from one device to the other, so the relay
cannot read it; where there is no relay, your apps go through the nest as usual.
Only the devices of your nest's own members can use it.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [nest] Once its relay is running, the relay serves at its own address on the standard port without being able to read the nest's own certificate key, and the nest advertises it — `docs/goal/behavior/p2p.md` § Architecture
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_iroh_relay_sidecar_fetches_sealed_cert_and_serves`
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_relay_reachable_via_sni_router_at_443`
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_nest_info_advertises_relay_capability_and_url`
2. [nest] A nest that is not running its relay offers none, and your apps use the ordinary path through the nest — `docs/goal/behavior/p2p.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_relay_advertisement.py::test_a_nest_running_no_relay_advertises_none`
3. [nest] A nest without a public name of its own gives your devices no relay address, even with its relay switched on — `docs/goal/behavior/p2p.md` § Implementation status today
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_a_nest_without_a_public_name_gives_no_relay_address`
4. [nest] After the nest restarts, its relay reconnects to it by itself and goes on serving, with nobody stepping in — `docs/goal/architecture/transport.md` § Future directions
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_the_relay_reconnects_by_itself_after_the_nest_restarts`
5. [nest] The released image runs its relay on a nest with a public name of its own, with nobody switching anything on — `docs/goal/behavior/p2p.md` § The relay
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_image_runs_its_relay_with_nothing_switched_on`
6. [nest] Two of your devices that cannot reach each other directly still connect through your nest's relay, which passes their traffic on without being able to read it — `docs/goal/behavior/p2p.md` § The relay
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_two_member_devices_with_no_direct_path_connect_through_the_relay`
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_two_seats_of_one_account_walk_through_the_relay_with_no_direct_path`
7. [nest] The relay serves only the devices of your nest's own members; any other device is refused — `docs/goal/behavior/p2p.md` § The relay
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_the_relay_turns_away_a_key_the_nest_does_not_know`
8. [nest] A nest with a public name of its own tells each of your devices its own public address, with nobody switching anything on, so two of them on different home networks can often connect straight to each other instead of through the relay — `docs/goal/behavior/p2p.md` § The relay
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_published_discovery_port_answers_from_outside_the_container`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+4baae748 docker |
| linux | ✅ full | 0.1.2-dev+4baae748 docker |
| windows | ✅ full | 0.1.2-dev+4baae748 docker |
| macos | ✅ full | 0.1.2-dev+4baae748 docker |
| ios | ✅ full | 0.1.2-dev+4baae748 docker |
| android | ✅ full | 0.1.2-dev+4baae748 docker |
| tui | ✅ full | 0.1.2-dev+4baae748 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_iroh_relay_sidecar_fetches_sealed_cert_and_serves` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_relay_reachable_via_sni_router_at_443` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_nest_info_advertises_relay_capability_and_url` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_relay_advertisement.py::test_a_nest_running_no_relay_advertises_none` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_a_nest_without_a_public_name_gives_no_relay_address` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_the_relay_reconnects_by_itself_after_the_nest_restarts` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_image_runs_its_relay_with_nothing_switched_on` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_two_member_devices_with_no_direct_path_connect_through_the_relay` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_two_seats_of_one_account_walk_through_the_relay_with_no_direct_path` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_the_relay_turns_away_a_key_the_nest_does_not_know` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_published_discovery_port_answers_from_outside_the_container` | nest (linux): passed |
<!-- features-render:end -->

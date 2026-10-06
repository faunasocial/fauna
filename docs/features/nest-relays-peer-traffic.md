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
   - (none)
3. [nest] A nest without a public name of its own gives your devices no relay address, even with its relay switched on — `docs/goal/behavior/p2p.md` § Implementation status today
   - (none)
4. [nest] After the nest restarts, its relay reconnects to it by itself and goes on serving, with nobody stepping in — `docs/goal/architecture/transport.md` § Future directions
   - (none)
5. [nest] The released image runs its relay on a nest with a public name of its own, with nobody switching anything on — `docs/goal/behavior/p2p.md` § The relay
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_image_runs_its_relay_with_nothing_switched_on`
6. [nest] Two of your devices that cannot reach each other directly still connect through your nest's relay, which passes their traffic on without being able to read it — `docs/goal/behavior/p2p.md` § The relay
   - (none)
7. [nest] The relay serves only the devices of your nest's own members; any other device is refused — `docs/goal/behavior/p2p.md` § The relay
   - (none)
8. [nest] A nest with a public name of its own tells each of your devices its own public address, with nobody switching anything on, so two of them on different home networks can often connect straight to each other instead of through the relay — `docs/goal/behavior/p2p.md` § The relay
   - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_published_discovery_port_answers_from_outside_the_container`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ❌ failing | 0.1.2-dev+b3cb40c5 docker |
| linux | ❌ failing | 0.1.2-dev+b3cb40c5 docker |
| windows | ❌ failing | 0.1.2-dev+b3cb40c5 docker |
| macos | ❌ failing | 0.1.2-dev+b3cb40c5 docker |
| ios | ❌ failing | 0.1.2-dev+b3cb40c5 docker |
| android | ❌ failing | 0.1.2-dev+b3cb40c5 docker |
| tui | ❌ failing | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_iroh_relay_sidecar_fetches_sealed_cert_and_serves` | nest (linux): error |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_relay_reachable_via_sni_router_at_443` | nest (linux): error |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_nest_info_advertises_relay_capability_and_url` | nest (linux): error |
| 2 | nest | (none) | — |
| 3 | nest | (none) | — |
| 4 | nest | (none) | — |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_image_runs_its_relay_with_nothing_switched_on` | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_published_discovery_port_answers_from_outside_the_container` | — |
<!-- features-render:end -->

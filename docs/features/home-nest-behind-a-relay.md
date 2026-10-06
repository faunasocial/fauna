---
slug: home-nest-behind-a-relay
title: A home nest behind a public relay
section: your nest
goal: docs/goal/architecture/nest/deployment-home-with-public-relay.md § Goal
guide: docs/guides/nest-relay-setup.md § Before you start
---

## What a user gets

Keep your data at home and still receive mail at a domain: a small public nest
relays sealed mail to the home nest and never keeps a readable copy. The two link
from the app in one step, and it works across real machines on the real internet.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [app] A private home nest behind a real public relay receives mail across real infrastructure: linked once from the app, mail from an outside sender is readable at home and gone from the relay — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Topology
   - `tests/e2e-unified/tests/live/test_private_relay_hetzner.py::test_private_relay_behind_hetzner_public_node`
2. [nest] Mail that arrives at the relay reaches the home nest and can be read there, and the relay holds no readable copy afterwards — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Inbound mail
   - `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest.py::test_two_nest_mail_relay_round_trip_and_no_readable_copy`
3. [app] Linking the two nests once also sets up your mailbox on the home nest, so relayed mail is readable there straight away, in the app and in a mail app, with no second step to turn mail on — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Topology
   - `tests/e2e-unified/tests/live/test_private_relay_hetzner.py::test_private_relay_behind_hetzner_public_node`
4. [nest] Mail that arrives while the home nest is switched off or unreachable waits on the relay, still sealed, and comes home once the home nest is back — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Inbound mail
   - (none)
5. [nest] Mail is handed to the home nest only once both nests are linked to each other; until then it stays on the relay — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Topology
   - `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest.py::test_two_nest_mail_relay_round_trip_and_no_readable_copy`
6. [nest] Mail you send appears in your Sent folder on the home nest — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Outbound mail
   - (none)
7. [nest] Mail the relay judged to be spam arrives in the Junk folder on the home nest, and other mail in the inbox — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Inbound mail
   - (none)
8. [nest] The home nest serves your mail and calendar to apps on your own network only, never on the public internet and never through the relay — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § MUA reach (IMAP / CalDAV)
   - (none)
9. [nest] The home nest takes in no mail from the internet and accepts no outgoing mail itself; both go through the relay — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Outbound mail
   - `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest.py::test_two_nest_mail_relay_round_trip_and_no_readable_copy`
10. [nest] A very large incoming message still reaches the home nest, and never holds up the mail that arrives after it — `docs/goal/architecture/nest/deployment-home-with-public-relay.md` § Relay frame budget
   - (none)
11. [nest] A certificate the admin's app obtains for the home nest reaches it through the relay and is installed there, and the relay can neither read nor alter it — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
   - (none)
12. [nest] The home nest installs a delivered certificate only if one of its own admins signed it; one sent by anyone else is never served — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux |  no run recorded | |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/live/test_private_relay_hetzner.py::test_private_relay_behind_hetzner_public_node` | — |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest.py::test_two_nest_mail_relay_round_trip_and_no_readable_copy` | nest (linux): failed |
| 3 | app | `tests/e2e-unified/tests/live/test_private_relay_hetzner.py::test_private_relay_behind_hetzner_public_node` | — |
| 4 | nest | (none) | — |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest.py::test_two_nest_mail_relay_round_trip_and_no_readable_copy` | nest (linux): failed |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | (none) | — |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest.py::test_two_nest_mail_relay_round_trip_and_no_readable_copy` | nest (linux): failed |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
<!-- features-render:end -->

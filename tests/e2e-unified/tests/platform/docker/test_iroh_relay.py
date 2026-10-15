"""tier_4 — the self-hosted iroh P2P relay sidecar over the real Docker image.

Proves the deployment/packaging/supervision half of the relay sidecar
(`behavior/p2p.md` § Architecture, `transport.md` § Future directions;
§ Piece 5c) that the tier_3 channel tests
(`bins/fauna-nest/src/sidecar_channel.rs` relay tests +
`bins/fauna-iroh-relay/src/lib.rs`) cannot — the sealed-cert fetch + the relay
serving are proven in-process there, but only the real image shows that

  1. the `fauna-iroh-relay` binary is actually packaged in the image (its
     SEPARATE `--features relay` cargo build, strip + COPY),
  2. the image runs it with NOTHING switched on (`p2p.md` § The relay): the s6
     service is up from boot under the non-root `fauna-relay` UID, with the
     boot-minted `/data/sidecar-token-relay` injected as FAUNA_SIDECAR_TOKEN; it
     stands by — connected to nest, handed no cert — until the box is claimed with
     a public name, and serves from that moment with no restart,
  3. the relay dials a deploy nest that serves HTTPS (its always-live self-signed
     floor cert, ACME off for the test domain) over `wss://`, completes the
     `fauna.sidecar.hello` handshake ATTESTING its X25519, then
  4. fetches its `relay.<domain>` TLS cert as an HPKE-sealed blob, opens it with
     its own X25519, and serves the relay protocol — WITHOUT ever reading
     `/data/acme` (security.md § UID isolation).

The two log lines below are the end-to-end proof that all four held: nest's
listener-side `relay sidecar channel established` (the channel bound + the X25519
was attested) and the relay's own `serving` line (it fetched + unsealed its cert
and is serving — it retries forever otherwise, so `serving` means the seal path
worked end to end).

Then the CLIENT-FACING half (the artifact-completeness leg): nest derives the
public relay URL from its CLAIMED identity (`state.handle_domain()`), so the box is
claimed with a real orderable domain (no FAUNA_DOMAIN boot env — retired):

  5. `fauna.nest.info` advertises the `relay` capability AND `iroh_relay_url ==
     https://relay.<domain>` — what a NAT'd client reads to learn it may dial the
     relay via `RelayMode::Custom`. `discovery_core::nest_info_core` derives it from
     `handle_domain()` gated on a relay sidecar being connected AND an orderable
     apex, so it appears only on a claimed real-domain box that runs its relay.
  6. a client reaching the published :443 with SNI `relay.<domain>` is L4-spliced
     by `fauna-sni-router` to the relay's loopback HTTPS port (8445, WITHOUT
     PROXY-v2 — D.1) and the relay answers its probe (`GET /ping` → 200), while
     `GET /api/v1/health` over that SNI is NOT nest's 200 — proof the byte stream
     reached the relay, not nest. This is the real client dial path: a relay URL
     of `https://relay.<domain>` (no port) resolves to :443 → the router → the
     relay. The relay round-trip itself (two iroh endpoints forwarding through it)
     is proven at the binary level (`fauna_iroh_relay::tests::
     binary_relay_carries_relay_only_peers`); this asserts the image/router path
     that the binary test bypasses.

And the ADDRESS-DISCOVERY leg (`p2p.md` § The relay → *Address discovery*):

  7. the relay's discovery UDP port, published like `docker-compose.yml`
     publishes it, answers a QUIC probe from OUTSIDE the container — the relay
     binds it on every interface, with nothing switched on. Before the claim (no
     public name, no certificate) the port already answers QUIC version
     negotiation, which carries no address; the discovery handshake itself takes
     the relay's certificate, so a standing-by relay reflects nobody's address.
     That the served discovery makes a cone pair's path direct is the NAT
     probe's to show (`just p2p-nat-probe`), not this module's.

And the RELAY-PROTOCOL leg (`p2p.md` § The relay, *What it carries* and *Who
may use it*) — the image carrying real peer traffic, which the legs above
only approach (a `/ping` is not a relayed connection):

  8. two devices the nest enrolled for its admin (through the real kinds,
     `fauna.sync.register` + `fauna.sync.device_grant.register`) each run the
     production peer-leg endpoint with its own device key and the relay URL the
     nest advertises, from two containers on two `--internal` docker networks
     whose only common member is the nest's container — so no direct path
     exists, as between two home networks. Both reach `relay.<domain>` on :443,
     the SNI router splices them to the relay, the relay asks the nest
     (`fauna.relay.admit`) and serves them, and one dials the other: the
     connection opens, stays on the relay to the end, the listener counts every
     byte the dialer sent, and it names the dialer's own device key — the key
     the end-to-end handshake proved, which the relay forwarding the packets
     never holds.
  9. a key the nest does not know gets the same path up to the question and a
     no: dialing a member device fails, a member device dialing it fails, and
     the relay's own log records the refusal of that key.

And the PEER-LEG leg — the account's own sync carried by the relay:

 10. two seats of the admin's account — the shared account runtime every
     native app hosts, its peer leg bound by the factory body the apps hand it
     — run on the same two isolated networks, each learning the relay URL from
     the nest: each dials the other, admits it while the connection runs over
     the relay, and walks the account-state rows there, and the account's post
     (indexed without its bytes) is left to the nest path because the
     connection is relayed and the nest answers (`p2p.md` § The relay,
     ruling 4).

**The drivers, decided (cheapest first):** legs 8–9's endpoint is
`bins/fauna-iroh-relay/examples/nat_probe.rs` (`just nat-probe`) — the
production peer-leg builder (`IrohTransport::builder(key).relay_url(..)`) as a
command-line program, already the NAT probe's — run from the host's own `/usr`
inside throwaway containers, exactly as `scripts/p2p-nat-probe.py` runs it. Leg
10 drives the account runtime app-less through the e2e harness library
(`fauna_ffi.harness_peer_seat_hold`, `feature-catalog.md` § The two surfaces,
the rule for a `[nest]` outcome the shared client code keeps), the host's interpreter borrowed into each box the same
way (`helpers/peer_seat_box.py`); the one test-only difference from an app's
leg is the relay root it trusts (`fauna_iroh::peer_leg_transport_trusting`),
since no public CA issues a certificate for the test domain. No app test runs
with a relay address. A foreign member of a set this nest hosts (*Across nests:
nobody*) is refused by the same `relay_admission::key_is_known` the stranger
here meets; planting one through real kinds takes a second nest, so it stays
proven in-process (`sidecar_channel::tests::relay_admit_serves_members_devices_and_nobody_else`).
"""

import contextlib
import json
import os
import secrets
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest


from .helpers import (
    IMAGE_TAG,
    admin_ws,
    claim_admin_api,
    create_network,
    docker_build,
    fence_cpuset_args,
    find_free_ports,
    get_repo_root,
    quic_version_negotiation,
    remove_container,
    remove_network,
    sni_https_request,
    start_container_with_ports,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

# Domainless boot (no FAUNA_DOMAIN — retired). The box learns its domain at CLAIM,
# and nest then derives the relay URL from that claimed identity
# (`state.handle_domain()`), so the box is claimed with a real, *orderable* domain
# below (`mail_domain=DOMAIN`): a localhost/IP apex is non-orderable, so
# `iroh_relay_public_url` (matching the relay cert SAN gate) would advertise `None`.
# The sidecar s6 run-scripts no longer read `${FAUNA_DOMAIN}`: `fauna-nest/run` drops
# `FAUNA_IROH_RELAY_URL` (nest self-derives it), and `fauna-iroh-relay/run`
# dials `wss://` unconditionally (the nest is always-HTTPS via
# its floor). The sni-router routes `relay.*` domain-agnostically. This was the last
# `FAUNA_DOMAIN` consumer.
DOMAIN = "nest.example.com"

# The relay's sealed-cert fetch (`Storage::seal_current_tls_cert_for_x25519`) is
# only implemented by the committed-storage-mode impls (`PlaintextStorage` /
# `EncryptedStorage`); the pre-claim `UnconfiguredStorage` returns `Ok(None)` by
# design (it has no `acme_dir` to read), so the relay would retry forever. A real
# deploy ALWAYS commits a storage mode at admin claim, so the fixture claims admin
# and commits `plaintext` (the "I trust the box" deploy default) — exactly as
# `test_caldav_sni_router.py` does for the MDA cert fetch.
CLAIM_CODE = "RELAY5"

# nest's listener-side line once the relay's token handshake binds the channel
# (`sidecar_channel::serve_relay_channel`) — proves the channel bound + the X25519
# attestation was captured.
CHANNEL_ESTABLISHED_LOG = "relay sidecar channel established (listener side)"

# The relay's own line once it has fetched + unsealed its FIRST cert and is
# serving (`fauna_iroh_relay::refresh_cert`). Until nest hands it a cert it
# stands by, so reaching `serving` is proof the whole sealed-cert path worked.
RELAY_SERVING_LOG = "fauna-iroh-relay serving"

# The relay's line at process start, before nest has handed it anything.
RELAY_STANDING_BY_LOG = "fauna-iroh-relay standing by"

# iroh-relay's HTTPS probe path (`RELAY_PROBE_PATH`, served by `probe_handler` →
# 200) — what a `GET` from the SNI-routed :443 path hits to prove the relay is
# reachable client-facing. nest has no such route (it serves `/api/v1/health`).
RELAY_PROBE_PATH = "/ping"

# `fauna_iroh_relay::DISCOVERY_PORT` — the UDP port the relay serves address
# discovery on, published by `docker-compose.yml` and opened by the guide.
DISCOVERY_PORT = 7842

# QUIC version 1 (RFC 9000) — what the discovery server must offer back.
QUIC_V1 = 0x00000001


@pytest.fixture(scope="module")
def docker_image():
    """Build (or reuse, via the warm buildx cache) the nest image."""
    yield docker_build(get_repo_root())
    # Leave the image tag for sibling docker modules to reuse.


@pytest.fixture(scope="module")
def relay_nest(docker_image):
    """A container with nest on :3000 and the router's :443 both mapped to host
    ports, booted DOMAINLESS (no FAUNA_DOMAIN — retired) and then claimed with a
    real orderable domain (`mail_domain=DOMAIN`) so the nest's claimed identity
    (`handle_domain()`) drives both the derived relay URL and the `relay.<domain>`
    cert SAN; the nest serves HTTPS via its always-live floor throughout. NOTHING
    enables the relay: the image runs it, and the claim alone brings it all the way
    to `serving`. Yields {name, http_port, router_port, served_before_claim}. Module-scoped so the three relay tests
    share one (expensive) bring-up."""
    http_port, router_port, discovery_port = find_free_ports(3)
    name = f"fauna-nest-relay-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, 443: router_port},
        udp_port_map={DISCOVERY_PORT: discovery_port},
        env={
            # No FAUNA_DOMAIN — the box boots domainless and learns its domain at
            # the claim below (`mail_domain=DOMAIN`).
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            "FAUNA_MODE": "public",
            # The relay logs each refused endpoint at debug — leg 9 reads it.
            "RUST_LOG": "info,fauna_iroh_relay=debug",
        },
    )
    try:
        wait_for_health(http_port, name)
        # The relay's sealed-cert fetch needs a committed storage mode (see
        # CLAIM_CODE above): claim admin and commit `plaintext` the way a real
        # deploy does at admin claim. The claim carries a
        # real orderable `mail_domain` — that domained claim IS how a domainless box
        # acquires its identity (claim_admin_api docstring), so `handle_domain()`
        # then yields DOMAIN and the derived relay URL is `https://relay.<DOMAIN>`.
        # Before the claim the box has no public name: the relay must be up and
        # connected (standing by) and must NOT be serving.
        standing_by = _poll_logs_for(name, RELAY_STANDING_BY_LOG, timeout=60.0)
        _poll_logs_for(name, CHANNEL_ESTABLISHED_LOG, timeout=60.0)
        served_before_claim = _logs_contain(name, RELAY_SERVING_LOG)
        discovery_before_claim = quic_version_negotiation("127.0.0.1", discovery_port)
        info_before_claim = _nest_info(http_port)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin",
                                mail_domain=DOMAIN)

        channel_up = _poll_logs_for(name, CHANNEL_ESTABLISHED_LOG, timeout=90.0)
        serving = _poll_logs_for(name, RELAY_SERVING_LOG, timeout=30.0)
        if not (channel_up and serving):
            logs = subprocess.run(
                ["docker", "logs", "--tail", "120", name],
                capture_output=True, text=True, timeout=15,
            )
            raise AssertionError(
                f"relay sidecar did not come up end to end within the timeout "
                f"(channel_established={channel_up}, serving={serving}). Expected "
                f"both {CHANNEL_ESTABLISHED_LOG!r} (nest) and {RELAY_SERVING_LOG!r} "
                "(relay) — i.e. the relay, with nothing switched on, dialed nest "
                "over wss://, attested its X25519, and after the claim fetched + "
                "unsealed its relay.<domain> cert and is serving.\n"
                f"Container logs (tail):\n{logs.stdout[-3000:]}\n{logs.stderr[-1500:]}"
            )
        yield {"name": name, "http_port": http_port, "router_port": router_port,
               "admin": admin,
               "discovery_port": discovery_port,
               "standing_by": standing_by,
               "served_before_claim": served_before_claim,
               "discovery_before_claim": discovery_before_claim,
               "info_before_claim": info_before_claim}
    finally:
        remove_container(name)


def _nest_info(http_port: int) -> dict:
    """The anonymous `fauna.nest.info` a device reads at sign-in."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(f"https://127.0.0.1:{http_port}") as anon:
        return anon.call("fauna.nest.info", {})


def _log_count(name: str, needle: str) -> int:
    """How many times `needle` appears in the container's log stream right now."""
    logs = subprocess.run(
        ["docker", "logs", name],
        capture_output=True, text=True, timeout=15,
    )
    return logs.stdout.count(needle) + logs.stderr.count(needle)


def _logs_contain(name: str, needle: str) -> bool:
    """Whether the container's log stream holds `needle` right now."""
    logs = subprocess.run(
        ["docker", "logs", name],
        capture_output=True, text=True, timeout=15,
    )
    return needle in logs.stdout or needle in logs.stderr


def _poll_logs_for(name: str, needle: str, timeout: float = 90.0) -> bool:
    """Poll `docker logs` until `needle` appears (s6 services log to container
    stdout), or the timeout elapses."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        logs = subprocess.run(
            ["docker", "logs", name],
            capture_output=True, text=True, timeout=15,
        )
        if needle in logs.stdout or needle in logs.stderr:
            return True
        time.sleep(2.0)
    return False


@pytest.mark.feature("nest-relays-peer-traffic")
def test_iroh_relay_sidecar_fetches_sealed_cert_and_serves(relay_nest):
    """The relay dialed nest over `wss://`, attested its X25519, fetched + unsealed
    its `relay.<domain>` cert, and is serving (asserted by the `relay_nest` fixture
    reaching the `serving` log) — and it never reads `/data/acme`, the Option-A
    channel-sidecar UID-isolation boundary."""
    name = relay_nest["name"]

    # The relay must never touch /data/acme (the UID-isolation boundary): it runs
    # as fauna-relay, which has no read access to the fauna-owned acme dir. A
    # successful `serving` (the fixture) already implies it got its cert the sealed
    # way; this asserts the negative directly.
    denied = subprocess.run(
        ["docker", "exec", "-u", "fauna-relay", name, "sh", "-c",
         "cat /data/acme/privkey.pem >/dev/null 2>&1 && echo READABLE || echo DENIED"],
        capture_output=True, text=True, timeout=30,
    )
    assert "DENIED" in denied.stdout, (
        "fauna-relay must NOT be able to read /data/acme/privkey.pem "
        f"(UID-isolation boundary); got: {denied.stdout!r} {denied.stderr!r}"
    )


@pytest.mark.feature("nest-relays-peer-traffic")
def test_image_runs_its_relay_with_nothing_switched_on(relay_nest):
    """The released image runs its relay on a nest with a public name of its own,
    with nobody switching anything on (`p2p.md` § The relay): the fixture wrote no
    flag and called no RPC — it only claimed the box — and the relay is serving.
    Before the claim (no public name) the same relay was up and standing by, not
    serving. And there is no flag for anyone to set: `services.json` carries no
    `iroh_relay` key, and `fauna.admin.services.update` has no such name."""
    name = relay_nest["name"]

    assert relay_nest["standing_by"], (
        "the image must start the relay by itself, before any claim: expected "
        f"{RELAY_STANDING_BY_LOG!r} in the container log with nothing enabled"
    )
    assert not relay_nest["served_before_claim"], (
        "a nest with no public name of its own must not have a serving relay: "
        f"{RELAY_SERVING_LOG!r} appeared before the claim gave the box its name"
    )
    # The fixture only yields once `serving` was reached after the claim.

    flag = subprocess.run(
        ["docker", "exec", name, "sh", "-c",
         "jq -r '.services | has(\"iroh_relay\")' /data/services.json"],
        capture_output=True, text=True, timeout=30,
    )
    assert flag.returncode == 0, f"could not read services.json:\n{flag.stderr}"
    assert flag.stdout.strip() == "false", (
        "nothing may switch the relay on, so there is no switch: services.json "
        f"must carry no `iroh_relay` key; got has(iroh_relay)={flag.stdout.strip()!r}"
    )


@pytest.mark.feature("nest-relays-peer-traffic")
def test_nest_info_advertises_relay_capability_and_url(relay_nest):
    """`fauna.nest.info` advertises the `relay` capability AND the
    `iroh_relay_url` once the relay sidecar is connected — the artifact-completeness leg:
    `nest_info_core` derives `https://relay.<domain>` from the nest's claimed
    identity (`handle_domain()` == DOMAIN, set by the domained claim), gated on a
    connected relay sidecar AND an orderable apex."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(f"https://127.0.0.1:{relay_nest['http_port']}") as anon:
        reply = anon.call("fauna.nest.info", {})

    assert "relay" in reply.get("capabilities", []), (
        "a nest whose relay sidecar is connected must advertise the `relay` capability on "
        f"fauna.nest.info; got capabilities={reply.get('capabilities')!r}"
    )
    assert reply.get("iroh_relay_url") == f"https://relay.{DOMAIN}", (
        "nest.info must surface the public relay URL derived from the claimed "
        f"identity (handle_domain()); expected https://relay.{DOMAIN}, got "
        f"{reply.get('iroh_relay_url')!r}"
    )


@pytest.mark.feature("nest-relays-peer-traffic")
def test_relay_reachable_via_sni_router_at_443(relay_nest):
    """A client dialing the published :443 with SNI `relay.<domain>` is L4-spliced
    by `fauna-sni-router` to the relay's loopback HTTPS port and the relay answers
    — the client-facing dial path a `https://relay.<domain>` URL (no port → :443)
    resolves to. `GET /api/v1/health` over the same SNI is NOT nest's 200, proving
    the split is by SNI to a *different* backend (the relay), not nest."""
    rp = relay_nest["router_port"]

    # (5) SNI relay.<domain> → the relay's HTTPS probe (probe_handler → 200). This
    # is exactly what a client gets dialing `https://relay.<domain>` (:443).
    status, _headers, body = sni_https_request(
        rp, f"relay.{DOMAIN}", "GET", RELAY_PROBE_PATH)
    assert status == 200, (
        f"SNI relay.{DOMAIN} {RELAY_PROBE_PATH} on the router :443 must reach the "
        f"relay's probe handler (200); got {status}, body={body[:200]!r}. A "
        "connection error means the sni-router route or the relay's HTTPS bind is "
        "unwired; a non-200 means the byte stream did not reach the relay."
    )

    # (5, negative) The SAME path that returns nest's 200 over SNI <domain> must NOT
    # return 200 over SNI relay.<domain> — confirms the split is by SNI to the relay
    # backend, not a single backend (nest) answering both.
    status, _headers, _body = sni_https_request(
        rp, f"relay.{DOMAIN}", "GET", "/api/v1/health")
    assert status != 200, (
        f"SNI relay.{DOMAIN} must route to the relay, which has no /api/v1/health; "
        f"a 200 means it reached nest (router did not split by SNI). got {status}"
    )


@pytest.mark.feature("nest-relays-peer-traffic")
def test_published_discovery_port_answers_from_outside_the_container(relay_nest):
    """The relay's address-discovery port, published as `docker-compose.yml`
    publishes it, answers a QUIC probe from outside the container on a nest with
    a public name — nobody switched it on (`p2p.md` § The relay → *Address
    discovery*, rulings 1–2). Before the claim the same port answered only QUIC
    version negotiation: a reply that names no address, from a relay holding no
    certificate to complete a discovery handshake under."""
    answer = quic_version_negotiation("127.0.0.1", relay_nest["discovery_port"])
    assert answer is not None, (
        f"UDP {DISCOVERY_PORT} published from the container must reach the relay's "
        "discovery server: nothing answered. Either the relay does not bind it on "
        "every interface (`fauna_iroh_relay::production_options`) or the image "
        "does not serve discovery."
    )
    versions, reply_len = answer
    assert QUIC_V1 in versions, f"the discovery server must speak QUIC v1; offered {versions!r}"
    assert reply_len < 1200, (
        f"an unsolicited 1200-byte datagram drew a {reply_len}-byte reply: the port "
        "must answer smaller than it is asked"
    )

    before = relay_nest["discovery_before_claim"]
    assert before is not None and QUIC_V1 in before[0], (
        "before the claim the relay is already bound (it stands by with no "
        "certificate), so the port answers version negotiation and nothing more; "
        f"got {before!r}"
    )


@pytest.mark.feature("nest-relays-peer-traffic")
def test_a_nest_without_a_public_name_gives_no_relay_address(relay_nest):
    """Before the claim the box has no public name of its own, while its relay
    is running and connected (the fixture saw it stand by and bind its channel).
    The nest then knows it has a relay — and still gives a device no relay
    address, because the relay has no name to serve under (`p2p.md` § The relay;
    `discovery_core::iroh_relay_public_url`). The claim that gives the box its
    name is what brings the address (`test_nest_info_advertises_relay_capability_and_url`)."""
    assert relay_nest["standing_by"], (
        "the premise: the relay must be running before the claim, or a missing "
        "address proves nothing about the missing name"
    )
    info = relay_nest["info_before_claim"]
    assert "relay" in (info.get("capabilities") or []), (
        "the premise: with its relay connected the nest must know it has one "
        f"(the `relay` capability); got {info.get('capabilities')!r}"
    )
    assert not info.get("iroh_relay_url"), (
        "a nest with no public name of its own advertised a relay address: "
        f"{info.get('iroh_relay_url')!r} (capabilities={info.get('capabilities')!r})"
    )


# A documentation-range address (RFC 5737) the admin says the nest is reached at.
_NEST_IPV4 = "203.0.113.10"


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_connected_relay_adds_its_address_record_to_the_primary(relay_nest):
    """Outcome 26, the relay half: with the relay running and connected to the
    nest, the primary domain's record list carries ``relay.<primary>``'s address
    record pointing at the nest — the record the admin must publish before the
    relay's name joins the certificate (`dns-management.md` § Records covered;
    `tls-certificates.md` § HTTP-01). The api tier cannot run the relay, so this
    is the tier that can see it."""
    nest = {"url": f"https://127.0.0.1:{relay_nest['http_port']}",
            "admin": relay_nest["admin"]}
    with admin_ws(nest) as admin:
        admin.call("fauna.dns.set_host_address", {
            "nest_ipv4": _NEST_IPV4,
            "nest_ipv6": None,
            "mail_ipv4": _NEST_IPV4,
            "mail_ipv6": None,
        })
        matrix = admin.call("fauna.dns.list_records", {})["domains"]
    rows = [d for d in matrix if d["domain"] == DOMAIN]
    assert rows, f"{DOMAIN} has no record matrix: {[d['domain'] for d in matrix]!r}"
    addresses = {
        r["name"]: r["expected"] for r in rows[0]["records"] if r["record_type"] == "A"
    }
    assert addresses.get(f"relay.{DOMAIN}") == _NEST_IPV4, (
        f"a connected relay must add relay.{DOMAIN}'s address record at the nest "
        f"address; the primary's address records are {addresses!r}"
    )


# ── The relay-protocol leg (8, 9) ─────────────────────────────────────────────

# What the nest advertises (`test_nest_info_advertises_relay_capability_and_url`)
# and what a device dials: no port, so :443, so the SNI router.
RELAY_URL = f"https://relay.{DOMAIN}"

# The relay's line for an endpoint the nest said no to (`fauna_iroh_relay`,
# debug — the fixture turns it on).
RELAY_REFUSED_LOG = "relay: endpoint refused"

# How long the dialer streams through the relay: long enough for the relayed
# path to have had every chance to turn direct, were one makeable.
STREAM_SECS = 5

# A dial the relay will not carry ends at the probe's own 30 s dial bound;
# the docker exec around it gets that plus headroom.
DIAL_EXEC_TIMEOUT_S = 90

# The host's dynamic loader, by architecture: the probe runs from the host's
# own /usr inside the boxes (`scripts/p2p-nat-probe.py` runs it the same way).
_LOADERS = {"aarch64": "ld-linux-aarch64.so.1", "x86_64": "ld-linux-x86-64.so.2"}


def _host_cmd(binary: str, *args: str) -> list[str]:
    """Run a host binary inside a box through the host's own loader."""
    arch = os.uname().machine
    libdir = f"/hostusr/lib/{arch}-linux-gnu"
    return [f"{libdir}/{_LOADERS[arch]}", "--library-path", libdir, binary, *args]


def _build_probe() -> Path:
    """`just nat-probe` (the build goes through `just`, as every fixture build
    does) and the binary it names on its last line."""
    out = subprocess.run(
        ["just", "nat-probe"], cwd=get_repo_root(),
        capture_output=True, text=True, timeout=3600,
    )
    assert out.returncode == 0, (
        f"just nat-probe failed:\n{out.stdout[-3000:]}\n{out.stderr[-3000:]}"
    )
    line = next((ln for ln in reversed(out.stdout.splitlines())
                 if ln.startswith("NAT_PROBE ")), None)
    assert line, f"just nat-probe named no binary:\n{out.stdout[-2000:]}"
    probe = Path(line.split(" ", 1)[1].strip())
    assert probe.is_file(), f"just nat-probe named {probe}, which does not exist"
    return probe


def _served_relay_cert(router_port: int) -> bytes:
    """The certificate the relay serves at `relay.<domain>` on :443, as DER —
    what the devices trust. ACME is off for the test domain, so the box serves
    the nest's self-signed floor, which carries `relay.<apex>`; pinning it here
    stands in for the platform roots a CA-issued cert is checked against."""
    from cryptography.hazmat.primitives.serialization import Encoding

    from helpers.tls_spki import served_leaf

    # The one home of "read a served leaf" (`helpers/tls_spki.py`); it raises
    # when the listener presents no certificate.
    der = served_leaf(
        "127.0.0.1", router_port, f"relay.{DOMAIN}"
    ).public_bytes(Encoding.DER)
    assert der, "the relay served no certificate"
    return der


def _container_ip(container: str, network: str) -> str:
    out = subprocess.run(
        ["docker", "inspect", "-f",
         f'{{{{(index .NetworkSettings.Networks "{network}").IPAddress}}}}', container],
        capture_output=True, text=True, timeout=30,
    )
    ip = out.stdout.strip()
    assert out.returncode == 0 and ip, f"no address for {container} on {network}: {out.stderr}"
    return ip


def _public_hex(seed: bytes) -> str:
    """The Ed25519 public key of a 32-byte secret — the device's endpoint key."""
    from nacl.signing import SigningKey

    return bytes(SigningKey(seed).verify_key).hex()


@pytest.fixture(scope="module")
def relay_peers(relay_nest):
    """Two of the admin's enrolled devices, each in a box of its own on an
    `--internal` network the nest's container also joins — the nest is the only
    thing both can reach — resolving `relay.<domain>` to the nest there, with
    the probe and the relay's served certificate mounted. Device A listens for
    the whole module. Yields the boxes, the keys and the paths."""
    from helpers.enrollment import register_granted_device

    nest_name = relay_nest["name"]
    nest_url = f"https://127.0.0.1:{relay_nest['http_port']}"
    probe = _build_probe()

    # Two of one member's devices, enrolled through the real kinds.
    seed_a, seed_b = secrets.token_bytes(32), secrets.token_bytes(32)
    admin = relay_nest["admin"]
    _, principal_a = register_granted_device(nest_url, admin, "relay-a", principal_seed=seed_a)
    _, principal_b = register_granted_device(nest_url, admin, "relay-b", principal_seed=seed_b)
    assert principal_a == _public_hex(seed_a) and principal_b == _public_hex(seed_b)

    out = _shared_out_dir("relay-peers-", relay_nest)
    mounts = ["-v", f"{probe}:/probe/nat_probe:ro"]
    with _boxes_with_no_path_between(nest_name, "peer", relay_nest["http_port"],
                                     out, mounts) as (boxes, _nest_ips):
        peers = {
            "boxes": boxes, "out": out,
            "seed_a": seed_a, "seed_b": seed_b,
            "pub_a": principal_a, "pub_b": principal_b,
        }
        _listen(peers, "a", seed_a.hex(), "listen-a.log")
        yield peers


def _shared_out_dir(prefix: str, relay_nest: dict) -> Path:
    """A host dir the boxes write into (world-writable: they run as root), with
    the relay's served certificate in it as `relay-root.der`."""
    out = Path(tempfile.mkdtemp(prefix=prefix, dir=os.environ.get("TMPDIR")))
    os.chmod(out, 0o777)
    (out / "relay-root.der").write_bytes(_served_relay_cert(relay_nest["router_port"]))
    return out


@contextlib.contextmanager
def _boxes_with_no_path_between(nest_name: str, kind: str, tag: int, out: Path,
                                mounts: list[str]):
    """Two boxes, `a` and `b`, each alone on an `--internal` network the nest's
    container also joins — the nest is the only thing both can reach, as for two
    devices on two home networks — resolving `relay.<domain>` to the nest there
    (`--add-host`), with the host's `/usr` at `/hostusr`, `out` at `/out` and
    `mounts` (more `-v` pairs). Yields ({side: box}, {side: the nest's address
    on that side's network}); removes boxes and networks after."""
    boxes = {side: f"fauna-relay-{kind}-{side}-{tag}" for side in ("a", "b")}
    nets = {side: f"fauna-relay-{kind}-lan-{side}-{tag}" for side in ("a", "b")}
    nest_ips: dict[str, str] = {}
    try:
        for side in ("a", "b"):
            create_network(nets[side], internal=True)
            subprocess.run(["docker", "network", "connect", nets[side], nest_name],
                           check=True, capture_output=True, timeout=30)
            nest_ips[side] = _container_ip(nest_name, nets[side])
            remove_container(boxes[side])
            subprocess.run(
                ["docker", "run", "-d", *fence_cpuset_args(), "--name", boxes[side],
                 "--network", nets[side],
                 "--add-host", f"relay.{DOMAIN}:{nest_ips[side]}", "--user", "0:0",
                 "-v", "/usr:/hostusr:ro", "-v", f"{out}:/out", *mounts,
                 "--entrypoint", "/bin/sleep", IMAGE_TAG, "infinity"],
                check=True, capture_output=True, timeout=60,
            )
        yield boxes, nest_ips
    finally:
        for box in boxes.values():
            remove_container(box)
        for net in nets.values():
            subprocess.run(["docker", "network", "disconnect", "-f", net, nest_name],
                           capture_output=True, timeout=30)
            remove_network(net)


def _listen(peers: dict, side: str, key_hex: str, log: str) -> None:
    """Start a listener in `side`'s box under `key_hex`, logging to `/out/<log>`,
    and wait until it is listening."""
    from helpers.waiting import wait_until

    cmd = " ".join(_host_cmd("/probe/nat_probe", "listen", key_hex, RELAY_URL,
                             "/out/relay-root.der"))
    subprocess.run(["docker", "exec", "-d", peers["boxes"][side], "sh", "-c",
                    f"exec {cmd} > /out/{log} 2>&1"], check=True, timeout=30)
    path = peers["out"] / log
    wait_until(lambda: path.exists() and "LISTENING" in path.read_text(), 60,
               diagnose=lambda: path.read_text() if path.exists() else "(no log)")


def _dial(peers: dict, side: str, key_hex: str, peer_hex: str) -> tuple[int, str]:
    """Dial `peer_hex` from `side`'s box under `key_hex`, streaming for
    STREAM_SECS. Returns (exit status, output)."""
    out = subprocess.run(
        ["docker", "exec", peers["boxes"][side],
         *_host_cmd("/probe/nat_probe", "dial", key_hex, RELAY_URL,
                    "/out/relay-root.der", peer_hex, str(STREAM_SECS))],
        capture_output=True, text=True, timeout=DIAL_EXEC_TIMEOUT_S,
    )
    return out.returncode, out.stdout + out.stderr


def _field(text: str, prefix: str, key: str) -> str | None:
    """`key=<value>` from the last line of `text` starting with `prefix`."""
    for ln in reversed(text.splitlines()):
        if ln.startswith(prefix) and f"{key}=" in ln:
            return ln.split(f"{key}=", 1)[1].split()[0]
    return None


def _relay_log_lines(name: str) -> list[str]:
    logs = subprocess.run(["docker", "logs", name], capture_output=True, text=True, timeout=30)
    return (logs.stdout + logs.stderr).splitlines()


@pytest.mark.feature("nest-relays-peer-traffic")
def test_two_member_devices_with_no_direct_path_connect_through_the_relay(relay_peers):
    """Two of the admin's enrolled devices, on two networks with no path between
    them, connect through the nest's relay at `relay.<domain>` on :443 and move
    a stream end to end: the connection stays on the relay to the end, the
    listener counts every byte sent, and it names the dialer's own device key —
    proved in the end-to-end handshake the relay only forwards (`p2p.md`
    § The relay, *What it carries*)."""
    from helpers.waiting import wait_until

    listen_log = relay_peers["out"] / "listen-a.log"
    status, dialed = _dial(relay_peers, "b", relay_peers["seed_b"].hex(), relay_peers["pub_a"])
    assert status == 0 and "CONNECTED" in dialed, (
        "device B could not connect to device A through the nest's relay at "
        f"{RELAY_URL}.\ndialer:\n{dialed[-2000:]}\nlistener:\n{listen_log.read_text()[-1500:]}"
    )
    final, sent = _field(dialed, "FINAL", "kind"), _field(dialed, "FINAL", "bytes")
    assert final == "relay", (
        "with no path between the two networks the connection must stay on the "
        f"relay; it ended on {final!r}:\n{dialed[-2000:]}"
    )
    assert sent and int(sent) > 0, f"the dialer sent nothing:\n{dialed[-2000:]}"

    def _done():
        text = listen_log.read_text()
        return text if "LISTENER_DONE" in text else None

    heard = wait_until(_done, 60, diagnose=lambda: listen_log.read_text()[-2000:])
    assert _field(heard, "ACCEPTED", "peer") == relay_peers["pub_b"], (
        "the listener must see the dialer's own device key, proved end to end: "
        f"{heard[-1500:]}"
    )
    assert _field(heard, "LISTENER_DONE", "kind") == "relay", heard[-1500:]
    assert _field(heard, "LISTENER_DONE", "bytes") == sent, (
        f"the relay must carry every byte: the dialer sent {sent}, the listener "
        f"counted {_field(heard, 'LISTENER_DONE', 'bytes')}:\n{heard[-1500:]}"
    )


@pytest.mark.feature("nest-relays-peer-traffic")
def test_the_relay_turns_away_a_key_the_nest_does_not_know(relay_nest, relay_peers):
    """A key no account on the nest enrolled reaches the relay by the same path
    as a member's device and is refused (`p2p.md` § The relay, *Who may use it*):
    it cannot dial a member's device, a member's device cannot dial it, and the
    relay's log records the refusal of that very key — while the member's own
    dial over the same boxes goes through, so the network is not what failed."""
    stranger = secrets.token_bytes(32)
    stranger_pub = _public_hex(stranger)

    status, dialed = _dial(relay_peers, "b", stranger.hex(), relay_peers["pub_a"])
    assert status != 0 and "CONNECTED" not in dialed, (
        "a key the nest does not know connected to a member's device through "
        f"the relay:\n{dialed[-2000:]}"
    )

    _listen(relay_peers, "b", stranger.hex(), "listen-stranger.log")
    status, dialed = _dial(relay_peers, "a", relay_peers["seed_a"].hex(), stranger_pub)
    assert status != 0 and "CONNECTED" not in dialed, (
        "a member's device reached a key the nest does not know through the "
        f"relay:\n{dialed[-2000:]}"
    )

    refused = [ln for ln in _relay_log_lines(relay_nest["name"]) if RELAY_REFUSED_LOG in ln]
    assert any(stranger_pub[:10] in ln for ln in refused), (
        f"the relay must record refusing the stranger's key {stranger_pub}; its "
        "refusal lines:\n" + "\n".join(refused[-10:])
    )
    members = (relay_peers["pub_a"][:10], relay_peers["pub_b"][:10])
    assert not any(m in ln for ln in refused for m in members), (
        "the relay refused a member's device:\n" + "\n".join(refused[-10:])
    )

    status, dialed = _dial(relay_peers, "b", relay_peers["seed_b"].hex(), relay_peers["pub_a"])
    assert status == 0 and "CONNECTED" in dialed, (
        "the control failed: the member's own dial over the same boxes did not "
        f"connect, so the refusals above prove nothing:\n{dialed[-2000:]}"
    )


# How long a peer seat may run at most — a ceiling a green run never meets:
# the test releases both seats the moment their reports show what it asserts.
SEAT_BUDGET_S = 600

# How long the test waits for both seats' reports to show the relayed walk.
SEATS_CONVERGE_TIMEOUT_S = 300


def _seat_cmd(nest_url: str, seed: bytes, side: str) -> str:
    """The shell line that runs side `side`'s peer seat in its box: the host's
    interpreter through the host's loader, the harness library the host test
    loaded, logging to `/out/seat-<side>.log`."""
    arch = os.uname().machine
    libdir = f"/hostusr/lib/{arch}-linux-gnu"
    python = "/hostusr" + os.path.realpath(sys.executable).removeprefix("/usr")
    args = [
        f"{libdir}/{_LOADERS[arch]}", "--library-path", libdir, python,
        "/e2e/helpers/peer_seat_box.py", nest_url, seed.hex(), f"relay-seat-{side}",
        "/out/relay-root.der", f"/out/seat-{side}.json", f"/out/release-{side}",
        str(SEAT_BUDGET_S),
    ]
    return f"exec {' '.join(args)} > /out/seat-{side}.log 2>&1"


@pytest.fixture(scope="module")
def relay_seats(relay_nest):
    """Two **peer seats** of the admin's account — the production account
    runtime with its peer leg (`fauna_ffi.harness_peer_seat_hold`, the factory
    body the native apps hand it, trusting the relay's served certificate) —
    each in a box on a network of its own with no path to the other
    (`_boxes_with_no_path_between`), talking to the nest at its address there.
    The account holds one post before either starts, so a seat's content walk
    indexes a record whose bytes it lacks. Yields the out dir; releases both
    seats and removes the boxes after."""
    from helpers.waiting import wait_until

    import fauna_ffi

    if fauna_ffi.LIBRARY_PATH is None or not fauna_ffi._HAS_PEER_SEAT:
        pytest.fail("the e2e-harness fauna_ffi library with fauna_harness_peer_seat_hold "
                    "did not load — run `just e2e-ffi`")
    python = os.path.realpath(sys.executable)
    if not python.startswith("/usr/"):
        pytest.skip(f"skip_environment: the seats borrow the host's /usr interpreter, and "
                    f"this run's is {python}")
    import site

    site_packages = next(p for p in site.getsitepackages() if p.endswith("site-packages"))
    seed = bytes(relay_nest["admin"]["signing_key"])

    nest = {"url": f"https://127.0.0.1:{relay_nest['http_port']}", "admin": relay_nest["admin"]}
    with admin_ws(nest) as client:
        # The free tier holds three devices, and `relay_peers` may already
        # have enrolled two: a seat past the cap is refused enrollment and
        # never binds. The admin moves its own account to a tier with room,
        # as an admin of a real nest does.
        client.call("fauna.admin.users.update",
                    {"actor_id": bytes(relay_nest["admin"]["signing_key"].verify_key),
                     "tier": "community", "label": "admin"})
        # One post on the account, published as the admin over the real kind
        # — the record both seats' content walks index without its bytes.
        client.call("fauna.posts.create",
                    {"body": fauna_ffi.build_post(seed, "carried by the relay", None)})

    out = _shared_out_dir("relay-seats-", relay_nest)
    lib = Path(fauna_ffi.LIBRARY_PATH)
    mounts = [
        "-v", f"{site_packages}:/site:ro",
        "-v", f"{get_repo_root() / 'tests' / 'e2e-unified'}:/e2e:ro",
        "-v", f"{lib.parent}:/ffi:ro",
    ]
    env = {
        "PYTHONHOME": "/hostusr",
        "PYTHONPATH": "/e2e:/site",
        "PYTHONDONTWRITEBYTECODE": "1",
        "FAUNA_E2E_FFI_LIBRARY": f"/ffi/{lib.name}",
    }
    with _boxes_with_no_path_between(relay_nest["name"], "seat", relay_nest["http_port"],
                                     out, mounts) as (boxes, nest_ips):
        try:
            for side in ("a", "b"):
                home = out / f"home-{side}"
                (home / "tmp").mkdir(parents=True)
                os.chmod(home, 0o777)
                os.chmod(home / "tmp", 0o777)
                side_env = {**env, "HOME": f"/out/home-{side}",
                            "TMPDIR": f"/out/home-{side}/tmp"}
                cmd = _seat_cmd(f"https://{nest_ips[side]}:3000", seed, side)
                subprocess.run(
                    ["docker", "exec", "-d",
                     *[a for k, v in side_env.items() for a in ("-e", f"{k}={v}")],
                     boxes[side], "sh", "-c", cmd],
                    check=True, timeout=30,
                )
            yield {"out": out, "boxes": boxes}
        finally:
            for side in ("a", "b"):
                (out / f"release-{side}").touch()
            for side in ("a", "b"):
                log = out / f"seat-{side}.log"
                # The seat ends its pass and stops once released; its last line
                # is its report, or its failure.
                wait_until(lambda log=log: log.exists() and log.read_text().strip()
                           and (log.read_text().strip().splitlines()[-1].startswith("{")
                                or "PEER_SEAT_FAILED" in log.read_text()),
                           120, diagnose=lambda log=log: log.read_text()[-2000:]
                           if log.exists() else "(no log)")


def _seat_report(out: Path, side: str) -> dict | None:
    path = out / f"seat-{side}.json"
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def _seat_diagnosis(out: Path) -> str:
    parts = []
    for side in ("a", "b"):
        log = out / f"seat-{side}.log"
        parts.append(f"seat {side} report: {_seat_report(out, side)}")
        parts.append(f"seat {side} log tail:\n{log.read_text()[-1500:] if log.exists() else '(none)'}")
    return "\n".join(parts)


@pytest.mark.feature("nest-relays-peer-traffic")
def test_two_seats_of_one_account_walk_through_the_relay_with_no_direct_path(relay_seats):
    """Two seats of the admin's account — the shared account runtime every
    native app hosts, with the peer leg the apps bind — on two networks with no
    path between them, each reading its nest's relay URL from the nest itself:
    each dials the other, the connection the substrate makes runs over the
    nest's relay at `relay.<domain>`, and both admit each other there and walk
    the account-state rows (`p2p.md` § The relay, *What it carries*). The
    account's post, indexed without its bytes, is left to the nest path while
    the connection is relayed and the nest answers (§ The relay, ruling 4):
    rows ride the relay, bytes do not.

    An admission is counted only once the sibling's account-state walks have
    completed over its connection, so a relayed admission is a walk the relay
    carried. How many rows that walk *applied* is not asserted: the nest leg
    delivers the same rows first, in the same pass, and the walk finds nothing
    new — the relayed walk matters when the nest cannot answer, which this
    test does not stage."""
    from helpers.waiting import wait_until

    out = relay_seats["out"]

    def _both_walked():
        a, b = _seat_report(out, "a"), _seat_report(out, "b")
        if a and b and a["admitted_relayed"] >= 1 and b["admitted_relayed"] >= 1 \
                and max(a["blocks_relay_deferred"], b["blocks_relay_deferred"]) >= 1:
            return a, b
        return None

    a, b = wait_until(_both_walked, SEATS_CONVERGE_TIMEOUT_S,
                      diagnose=lambda: _seat_diagnosis(out))
    for side, report in (("a", a), ("b", b)):
        assert report["bound"], f"seat {side}'s peer leg never bound:\n{_seat_diagnosis(out)}"
        assert report["targets"] == 1, (
            f"seat {side} must dial exactly its one sibling, the other seat:\n"
            f"{_seat_diagnosis(out)}"
        )
        assert report["admitted"] == report["admitted_relayed"], (
            f"with no path between the networks every admission is relayed; seat "
            f"{side} admitted {report['admitted']}, {report['admitted_relayed']} of "
            f"them relayed:\n{_seat_diagnosis(out)}"
        )
        assert report["blocks_fetched"] == 0, (
            f"seat {side} pulled bytes over the relay while its nest answered "
            f"(ruling 4):\n{_seat_diagnosis(out)}"
        )


# Must stay LAST in this module: it restarts the nest inside the shared
# container, and every test above reads the box as the fixture left it.
@pytest.mark.feature("nest-relays-peer-traffic")
def test_the_relay_reconnects_by_itself_after_the_nest_restarts(relay_nest):
    """The nest process restarts (s6 brings it back, as after a crash or an
    in-place update); nobody touches the relay. The relay must dial the nest
    again by itself (`transport.md` § Future directions: it re-dials its channel
    when it drops), so the nest once more knows it has a relay and advertises
    it, and a device dialing `relay.<domain>` on :443 is still served."""
    from helpers.budgets import SERVICE_BOOT_S
    from helpers.waiting import wait_until

    from .helpers import restart_service

    name, http_port = relay_nest["name"], relay_nest["http_port"]
    channels_before = _log_count(name, CHANNEL_ESTABLISHED_LOG)

    restart_service(name, "fauna-nest")
    wait_for_health(http_port, name)

    def _relay_back():
        if _log_count(name, CHANNEL_ESTABLISHED_LOG) <= channels_before:
            return None
        try:
            info = _nest_info(http_port)
        except Exception:
            return None
        if "relay" in (info.get("capabilities") or []) and (
            info.get("iroh_relay_url") == f"https://relay.{DOMAIN}"
        ):
            return info
        return None

    def _diagnose():
        logs = subprocess.run(
            ["docker", "logs", "--tail", "80", name],
            capture_output=True, text=True, timeout=15,
        )
        return (
            f"channel lines before={channels_before} now="
            f"{_log_count(name, CHANNEL_ESTABLISHED_LOG)}\n"
            f"{logs.stdout[-2500:]}\n{logs.stderr[-1000:]}"
        )

    wait_until(_relay_back, SERVICE_BOOT_S, interval=2.0, diagnose=_diagnose)

    status, _headers, body = sni_https_request(
        relay_nest["router_port"], f"relay.{DOMAIN}", "GET", RELAY_PROBE_PATH)
    assert status == 200, (
        f"after the nest restarted, the relay must go on serving at relay.{DOMAIN}; "
        f"got {status}, body={body[:200]!r}"
    )

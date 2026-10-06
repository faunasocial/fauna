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
"""

import subprocess
import time

import pytest


from .helpers import (
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    quic_version_negotiation,
    remove_container,
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
            "RUST_LOG": "info",
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
               "discovery_port": discovery_port,
               "standing_by": standing_by,
               "served_before_claim": served_before_claim,
               "discovery_before_claim": discovery_before_claim}
    finally:
        remove_container(name)


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

"""tier_4 e2e: the two-box home-with-public-relay mail round-trip in the real
Docker image — **Slice 6**, the final § Done-definition checkbox of
``docs/goal/architecture/nest/deployment-home-with-public-relay.md`` (and the
owed tier_4 validation of ``docs/goal/architecture/installers/home-relay.md``).

Stands up the topology on ONE machine as TWO ``fauna-nest`` containers on a
shared docker network:

  * **public box** — a normal public-axis relay nest: MTA (inbound SMTP) + MDA,
    the standard image shape. Receives mail, HPKE-seals it to the user, holds it
    transiently.
  * **private/home box** — ``FAUNA_MODE=private`` + ``FAUNA_LAN_BIND_IP`` (the
    ``docker-compose.home.yml`` shape): **no MTA**, MDA serves IMAP on the
    LAN-bind. It sits on a LAN network of its own, its pinned address there
    being the LAN-bind, and is attached to the shared network as its route out
    (``_bring_up_topology``). Its admin — the box's owner and the mail user — pairs it with
    the public box from their app, recording the public box's on-network URL
    on their pairing row; the home box pulls the relayed mail from that URL over
    the federation channel. No env var names the public box.

What it proves (the production data flow checkbox 5 asserts):

  1. inbound delivered to the **public** MTA is HPKE-sealed to the user and lands
     in the public box's ``__mail``;
  2. with only the public-side pairing row, the home box's relay worker does
     **not** fire (its own ``nest_pairings`` is empty, so it has no target) —
     the public box still holds the mail (mirrors the tier_3
     ``worker_cycle_relays_only_after_both_pairing_rows_seeded`` ``Processed(0)``
     phase);
  3. once the user also links from the home box (the private-side pairing row,
     carrying the public box's URL — a docker-bridge address the dial is allowed
     to reach because the row's actor is the home box's admin), the worker
     relays public→private over ``fauna.federation.sync.mail_pull`` /
     ``.mail_ack``, the home box's MDA serves the mail **decrypted** over IMAP,
     and the public box holds **no readable/persistent copy** afterwards (its
     INBOX goes 1→0 — the acked records are tombstoned, which the IMAP fetch path
     filters immediately);
  4. the home box's **MTA s6 service stays DOWN** the whole time (no-MTA boot —
     the ``mta_should_run`` nest gate + the ``FAUNA_MODE=private`` s6 run-script
     re-down), validating the "Perimeter parser process: NOT started" row of
     § Plaintext-mode behavior in the real image.

This simultaneously discharges the home-bundle tier_4 validation owed from Slice
5 (IMAP LAN exposure via ``FAUNA_LAN_BIND_IP``, no-MTA boot, the full
external→public→home→MUA round-trip over the real packaged binaries + s6
supervision — the layer the in-process tier_3 relay tests bypass).

**Storage mode (RETIRED, no-modes retirement ratified 2026-07-12).** This test
used to be parametrized over the public box's storage mode
(``public-plaintext`` / ``public-encrypted``) to demonstrate the relay is
storage-mode-independent. That axis is gone along with the deployment-wide
storage-mode question itself: every nest — public and home alike — is sealed
at rest unconditionally now (mail records are EncryptToRecipient-sealed at
rest regardless of any former mode, deployment-home-with-public-relay.md §
Inbound mail step 5; the MDA AUTH/unwrap step is uniform,
storage-modes.md rule 5), so there is no second arm left to compare against.
Collapsed to a single un-parametrized topology.

**Why loopback delivery + enforced perimeter (Gap 2g prod parity).** Inbound is
delivered from *inside* the public container over loopback
(``deliver_inbound_loopback_curl``): the DNS-dependent HELO/FCrDNS perimeter
checks are loopback-exempt, exactly as the single-box
``test_mail_deploy_inbound_round_trip`` does. But the public relay box runs the
**live-box posture**, not the relaxed opt-out: the inbound AUTH perimeter is
**ENFORCED** (``enforce_dmarc=true``/``log_only=false``) via the shared
``enforce_mail_perimeter`` + ``publish_passing_mail_dns`` helpers (``testing.md``
§ Gap 2 Target). The sender ``external-sender@sender.test`` publishes a passing
SPF (``ip4:127.0.0.1`` — the loopback client IP) + ``_dmarc … p=reject`` through a
fake_dns sidecar (the public box's ``--dns``), so acceptance is earned by an
aligned DMARC pass, not a no-policy pass — the enforced gate still fires on
loopback (the ``550`` negative control in ``test_mail_security_accept.py``). Only
the orthogonal spam gates a synthetic peer can't satisfy (DNSBL/FCrDNS) stay
relaxed. The HOME box needs no perimeter change — it receives over the federation
pull, never the SMTP gate. The fail-closed clamd/rspamd scan gate is satisfied by
the fake-scanner sidecars on the shared network.
"""

import datetime
import email.message
import email.utils
import os
import subprocess
import time
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

from .helpers import (
    IMAGE_TAG,
    ROLES,
    _provision_submission_token,
    add_user_pairing,
    bridge_diag,
    bring_mda_only_to_serving,
    bring_bridges_to_serving,
    claim_admin_api,
    container_ip,
    create_network,
    create_subnet_network,
    deliver_inbound_loopback_curl,
    docker_build,
    enforce_mail_perimeter,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    imap_inbox_count,
    imap_wait_for_subject,
    node_id,
    provision_relay_user,
    publish_passing_mail_dns,
    register_primary_domain,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    submit_message_tls,
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

DOMAIN = "localhost"
SENDER_DOMAIN = "sender.test"      # the inbound sender's domain (publishes passing SPF + _dmarc)
PUBLIC_CLAIM_CODE = "RLAYP1"
PRIVATE_CLAIM_CODE = "RLAYH1"
RECIPIENT_LOCAL = "relay-user"
RECIPIENT_PASSWORD = "relay-round-trip-pw-1"  # gitleaks:allow
# The full self-sync capability set (default_self_sync) — the public-side row
# MUST include `mail_pull` (the relay grant the `mail_pull_handler` gates on).
RELAY_CAPS = ["mls_pull", "namespace_sync", "post_forward", "mail_pull"]


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def relay_net(docker_image, tmp_path):
    """A fresh user-defined network hosting BOTH nests + the public box's fake
    clamd/rspamd scan sidecars and a ``records_dir``-backed fake_dns resolver. One
    network so the home box can reach the public box by IP (the federation pull
    target) and the public MTA can reach the scanners by IP. The fake_dns sidecar
    (the PUBLIC box's ``--dns``) lets the inbound sender domain publish a PASSING
    SPF + ``_dmarc … p=reject`` for the ENFORCED DMARC perimeter the public relay
    box now runs (Gap 2g prod parity — ``testing.md`` § Gap 2 Target). Yields
    {network, clamd_addr, rspamd_url, dns_ip, records_dir}."""
    http_port = find_free_ports(1)[0]  # unique-enough token for the names
    network = f"fauna-relay-net-{http_port}"
    clamd_name = f"fauna-relay-clamd-{http_port}"
    rspamd_name = f"fauna-relay-rspamd-{http_port}"
    dns_name = f"fauna-relay-dns-{http_port}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    records_dir = str(tmp_path)
    os.chmod(records_dir, 0o777)  # the fake_dns sidecar reads it through a bind mount
    create_network(network)
    try:
        addrs = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name, txt_records={}, records_dir=records_dir)
        yield {"network": network, "dns_ip": dns_ip, "records_dir": records_dir, **addrs}
    finally:
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_container(dns_name)
        remove_network(network)


def _bring_up_topology(relay_net, *, one_admin: bool):
    """Bring up the two boxes and claim both. Every nest is sealed at rest
    unconditionally now (no-modes retirement, ratified 2026-07-12 — see the
    module docstring § Storage mode), so there is no storage-mode axis to
    parametrize the public box over. The public box starts first so its
    on-network URL ``https://<public-container-ip>:3000`` (the nest's internal
    listener — FAUNA_PORT, kept off 8443 so 8443 is the MDA's user-facing CalDAV
    port; the federation WS is a nest route, not the SNI-router/MDA :8444) is
    known before the home box pairs with it — the URL the home-side pairing row
    records (``public["federation_url"]``).

    **Two networks, as at a real home.** The home box starts on a network of its
    own — its LAN — with a pinned address there, and that address is its
    ``FAUNA_LAN_BIND_IP``: the MDA binds IMAP/CalDAV to it and to nothing else
    (``deployment-home-with-public-relay.md`` § MUA reach). It is then attached
    to the shared relay network too, so it can dial the public box — the route
    out a home router gives it. The LAN network's name sorts before the relay
    network's (``lan`` < ``net``), which keeps it the home box's default route,
    so the ports published to the host are the LAN path: a mail app on the
    host is a device on the home's LAN.

    ``one_admin`` claims the public box with the home box's admin identity —
    one person setting up both boxes from one app, the shape the home-relay
    installer describes; otherwise each box has an admin of its own.
    Yields {public, private}."""
    net = relay_net["network"]

    # ── Public relay box: all four mail ports + the nest's :3000 published; scan
    # gate pointed at the sidecars; resolver (--dns) pointed at the fake_dns sidecar
    # so the inbound sender domain (sender.test) resolves a PASSING SPF + `_dmarc
    # p=reject` for the ENFORCED DMARC perimeter this box now runs (Gap 2g). The
    # loopback inbound is still exempt from the reachability-gated HELO/FCrDNS
    # checks (deliver_inbound_loopback_curl), so no host.docker.internal add-host.
    # nest is on 3000 (FAUNA_PORT), kept off 8443 so the MDA can own 8443.
    pub_http, p25, p465, p587, p993 = find_free_ports(5)
    pub_name = f"fauna-relay-public-{pub_http}"
    lan = f"fauna-relay-lan-{pub_http}"
    start_container_with_ports(
        pub_name,
        {443: pub_http, 25: p25, 465: p465, 587: p587, 993: p993},
        env={
            "FAUNA_CLAIM_CODE": PUBLIC_CLAIM_CODE,
            "FAUNA_PORT": "3000",
            "FAUNA_CLAMD_ADDR": relay_net["clamd_addr"],
            "FAUNA_RSPAMD_URL": relay_net["rspamd_url"],
        },
        dns=relay_net["dns_ip"],
        network=net,
    )
    priv_name = None
    lan_made = False
    try:
        wait_for_health(pub_http, pub_name)
        # The home box reaches the public nest's federation WS at its on-network
        # IP : the nest's internal port (3000), NOT the published host port —
        # the URL the home-side pairing row records.
        federation_url = f"https://{container_ip(pub_name)}:443"

        # ── Private/home box: FAUNA_MODE=private (the pre-claim NAT seed) on its
        # own LAN network, FAUNA_LAN_BIND_IP = its pinned LAN address. No MTA
        # ports (the home box runs no MTA).
        # ⚠ nest MUST be on 3000 (FAUNA_PORT), NOT 8443: with FAUNA_LAN_BIND_IP set
        # the MDA binds CalDAV directly on <LAN-IP>:8443 (the admin default port),
        # so a nest on 0.0.0.0:8443 would EADDRINUSE-collide and crash-loop the MDA
        # (the home-relay deploy bug fixed 2026-06-20 by moving nest off 8443).
        subnet = create_subnet_network(lan)
        lan_made = True
        home_lan_ip = str(subnet.network_address + 10)
        priv_http, priv_993 = find_free_ports(2)
        priv_name = f"fauna-relay-private-{priv_http}"
        start_container_with_ports(
            priv_name,
            {443: priv_http, 993: priv_993},
            env={
                "FAUNA_MODE": "private",
                "FAUNA_LAN_BIND_IP": home_lan_ip,
                "FAUNA_CLAIM_CODE": PRIVATE_CLAIM_CODE,
                "FAUNA_PORT": "3000",
            },
            network=lan,
            ip=home_lan_ip,
        )
        _network_attach("connect", net, priv_name)
        wait_for_health(priv_http, priv_name)
        priv_admin = claim_admin_api(priv_http, PRIVATE_CLAIM_CODE, handle="admin")
        pub_admin = claim_admin_api(
            pub_http, PUBLIC_CLAIM_CODE, handle="admin",
            signing_key=priv_admin["signing_key"] if one_admin else None)
        public = {
            "name": pub_name, "port": pub_http,
            "url": f"https://127.0.0.1:{pub_http}", "admin": pub_admin,
            "mail_ports": {25: p25, 465: p465, 587: p587, 993: p993},
            "records_dir": relay_net["records_dir"],
            "federation_url": federation_url, "network": net,
        }
        private = {
            "name": priv_name, "port": priv_http,
            "url": f"https://127.0.0.1:{priv_http}", "admin": priv_admin,
            "imap_port": priv_993, "lan": lan, "lan_ip": home_lan_ip,
        }
        yield {"public": public, "private": private}
    finally:
        if priv_name is not None:
            remove_container(priv_name)
        remove_container(pub_name)
        if lan_made:
            remove_network(lan)


@pytest.fixture()
def topology(docker_image, relay_net):
    """The two boxes, each claimed by an admin of its own."""
    yield from _bring_up_topology(relay_net, one_admin=False)


@pytest.fixture()
def topology_one_admin(docker_image, relay_net):
    """The two boxes, both claimed by the home box's admin — the person who set
    up both, and so an admin wherever the relayed-certificate route asks."""
    yield from _bring_up_topology(relay_net, one_admin=True)


def _network_attach(verb: str, network: str, name: str) -> None:
    """``docker network connect|disconnect`` — the home box joining or leaving
    the relay network, as a home loses and regains its route out."""
    r = subprocess.run(["docker", "network", verb, network, name],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, f"docker network {verb} {network} {name}: {r.stderr}"


def _ip_on(name: str, network: str) -> str:
    """The container's IPv4 on one named network (it has two)."""
    r = subprocess.run(
        ["docker", "inspect", "-f",
         f'{{{{(index .NetworkSettings.Networks "{network}").IPAddress}}}}', name],
        capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and r.stdout.strip(), f"no address for {name} on {network}: {r.stderr}"
    return r.stdout.strip()


def _link(topo, run_seal_helper, *, owner_admins_public: bool) -> dict:
    """The deployment once its user has linked the two boxes: one user (one
    actor, one MSEK) on both, mail serving on both (MTA + MDA on the relay, MDA
    alone at home), and the pairing row on BOTH boxes — the end state of the
    round-trip test's two phases, for the witnesses that start from a working
    relay. Returns {public, private, user}."""
    public, private = topo["public"], topo["private"]
    enforce_mail_perimeter(public, domain=DOMAIN)
    register_primary_domain(private, DOMAIN)
    publish_passing_mail_dns(public["records_dir"], {SENDER_DOMAIN: "127.0.0.1"},
                             spf_ip="127.0.0.1")
    user = provision_relay_user(public, private, run_seal_helper,
                                domain=DOMAIN, local_part=RECIPIENT_LOCAL,
                                password=RECIPIENT_PASSWORD,
                                home_owner=private["admin"],
                                owner_admins_public=owner_admins_public)
    bring_bridges_to_serving(public["name"], public, public["mail_ports"], DOMAIN)
    bring_mda_only_to_serving(private["name"], private,
                              ("127.0.0.1", private["imap_port"]), DOMAIN)
    add_user_pairing(public, actor_id=user["actor_id"], signing_key=user["signing_key"],
                     other_nest_id=node_id(private), capabilities=RELAY_CAPS)
    add_user_pairing(private, actor_id=user["actor_id"], signing_key=user["signing_key"],
                     other_nest_id=node_id(public), capabilities=RELAY_CAPS,
                     nest_url=public["federation_url"])
    return {"public": public, "private": private, "user": user}


@pytest.fixture()
def linked(topology, run_seal_helper):
    return _link(topology, run_seal_helper, owner_admins_public=False)


@pytest.fixture()
def linked_one_admin(topology_one_admin, run_seal_helper):
    return _link(topology_one_admin, run_seal_helper, owner_admins_public=True)


# ── Failure diagnosis ─────────────────────────────────────────────────


# The nest log lines that say what the relay did: the home box's sync/relay
# worker (its boot line, a refused or failed pull/ack, the peer URL it dials) and
# either end's federation channel (the dial, the hello, a `mail_pull` gate
# refusal). `bridge_diag`'s keep list is the mail bridges' and matches none of
# these, which is why a relay red used to dump a healthy MDA and nothing else.
_RELAY_KEEP = ("mail relay", "sync worker", "sync push", "sync pull", "pair",
               "federation", "peer url", "non-global", "mail_pull", "mail_ack")


def relay_diag(name: str, roles=("mda",)) -> str:
    """The relay's own story from one box — its relay-worker and federation log
    lines plus the mail-bridge s6 state — for a failure message (e2e convention
    6). Reads `docker logs` whole, not the bridges' tail, because the worker's
    boot line is at the top and its per-cycle warning repeats every 10 s."""
    from .helpers import svstat
    logs = subprocess.run(["docker", "logs", name],
                          capture_output=True, text=True, timeout=20)
    lines = [ln for ln in (logs.stdout + logs.stderr).splitlines()
             if any(k in ln.lower() for k in _RELAY_KEEP)]
    st = {r: svstat(name, f"fauna-mail-bridge-{r}") for r in roles}
    return f"svstat={st!r}\n  " + "\n  ".join(lines[-30:] or ["(no relay/federation log lines)"])


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("home-nest-behind-a-relay")
def test_two_nest_mail_relay_round_trip_and_no_readable_copy(topology, run_seal_helper):
    public = topology["public"]
    private = topology["private"]
    pub_993 = public["mail_ports"][993]
    priv_993 = private["imap_port"]

    # ── Provision: one user (one actor + one MSEK) on BOTH boxes; register the
    # mail domain on both (RCPT on the public MTA, IMAP-login alias on the home
    # box). The PUBLIC box runs the live-box ("prod-parity") inbound AUTH posture:
    # `enforce_mail_perimeter` registers the primary domain, relaxes ONLY the
    # orthogonal spam gates a synthetic loopback peer can't satisfy (DNSBL/greylist/
    # FCrDNS/conn-rate), and ENFORCES the DMARC gate (enforce_dmarc=true,
    # log_only=false). `cross_container_peer` stays False — the inbound is
    # loopback-delivered (`deliver_inbound_loopback_curl`), so HELO identity is
    # loopback-exempt, but the enforced DMARC gate still fires (the `550` negative
    # control in `test_mail_security_accept.py`), so the perimeter is non-vacuous
    # (Gap 2g — `testing.md` § Gap 2 Target). The HOME box needs only its domain
    # registered (IMAP-login alias) — it receives the mail over the federation
    # pull, never the SMTP perimeter, so no enforced gate applies there.
    enforce_mail_perimeter(public, domain=DOMAIN)
    register_primary_domain(private, DOMAIN)
    # Publish the inbound sender domain's PASSING perimeter into the public box's
    # fake_dns: a passing SPF authorizing the loopback (`spf_ip=127.0.0.1` — the
    # loopback delivery connects from inside the container, so the MTA sees
    # 127.0.0.1 as the client IP) + `_dmarc … p=reject`. Under the enforced gate
    # the inbound is accepted ONLY because it DMARC-passes via the aligned SPF pass
    # (From:/envelope both on `sender.test` ⇒ aspf-aligned).
    publish_passing_mail_dns(public["records_dir"], {SENDER_DOMAIN: "127.0.0.1"},
                             spf_ip="127.0.0.1")
    # The user is the home box's own admin (its owner): the home-side row then
    # names the deployment's topology, so the dial to the public box's
    # docker-bridge address is exempt from the address guard's global arm.
    user = provision_relay_user(public, private, run_seal_helper,
                                domain=DOMAIN, local_part=RECIPIENT_LOCAL,
                                password=RECIPIENT_PASSWORD,
                                home_owner=private["admin"])

    # ── Bring both boxes' bridges to serving. Public: MTA + MDA (all four
    # listeners). Private/home: MDA only — and assert the MTA stays DOWN
    # (no-MTA boot).
    bring_bridges_to_serving(public["name"], public, public["mail_ports"], DOMAIN)
    bring_mda_only_to_serving(private["name"], private, ("127.0.0.1", priv_993), DOMAIN)

    # ── Seed ONLY the public-side pairing row first: it authorizes the home box
    # (by nest id) to pull, with the `mail_pull` grant. The home box's relay
    # worker still won't fire — its own `nest_pairings` is empty — so the public
    # box keeps the mail (the deterministic Processed(0) phase).
    priv_nest_id = node_id(private)
    pub_nest_id = node_id(public)
    add_user_pairing(public, actor_id=user["actor_id"], signing_key=user["signing_key"],
                     other_nest_id=priv_nest_id, capabilities=RELAY_CAPS)

    # ── Deliver one inbound message to the PUBLIC box's MTA (loopback). The MTA
    # validates the recipient, clears the ENFORCED DMARC gate via the aligned SPF
    # pass (sender.test's published `ip4:127.0.0.1` SPF + `_dmarc p=reject`) and
    # the scan gate, HPKE-seals the body to the user's MSEK-derived pubkey, and
    # ingests it.
    subject = "Slice-6 two-nest relay round-trip"
    body_marker = "home-relay deploy-image two-box round-trip"
    msg_id = deliver_inbound_loopback_curl(
        public["name"],
        mail_from=f"external-sender@{SENDER_DOMAIN}",
        rcpt_to=user["username"],
        subject=subject,
        body_text=f"Hello from the {body_marker} test.\n",
    )

    # The mail is readable on the PUBLIC box now (it lands there first) and the
    # relay is NOT yet firing (home box has no local pairing row), so the public
    # INBOX holds exactly the one message. Poll: the MTA ingest → INBOX placement
    # is async after the DATA accept.
    try:
        deadline = time.monotonic() + 90
        pub_count = 0
        while time.monotonic() < deadline:
            pub_count = imap_inbox_count("127.0.0.1", pub_993, DOMAIN,
                                         user["username"], user["password"], timeout=30)
            if pub_count >= 1:
                break
            time.sleep(2.0)
        assert pub_count == 1, (
            f"the inbound message must land on the public relay box before the "
            f"home box is linked; public INBOX shows {pub_count}")
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── public bridge diag ──\n{bridge_diag(public['name'])}") from e

    # ── Now the user links the public box FROM the home box: the private-side
    # pairing row, carrying the public box's URL, makes the home box's relay
    # worker fire (10s cycle) against that URL. It pulls the sealed record over
    # the federation channel, appends it verbatim to its own __mail, and acks →
    # the public box tombstones + purges.
    add_user_pairing(private, actor_id=user["actor_id"], signing_key=user["signing_key"],
                     other_nest_id=pub_nest_id, capabilities=RELAY_CAPS,
                     nest_url=public["federation_url"])

    # ── Read it back DECRYPTED from the HOME box's MDA over IMAP (the canonical
    # store). Generous timeout to cover the worker's ~10s poll cycle + the
    # pull/append/ack round-trip. Same MSEK on both ends ⇒ the open succeeds.
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", priv_993, DOMAIN, user["username"], user["password"],
            timeout=120.0)
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(
            f"{e}\n\n── home-box relay worker ──\n{relay_diag(private['name'])}"
            f"\n\n── public-box federation ──\n{relay_diag(public['name'], ROLES)}"
            f"\n\n── home-box bridge diag ──\n{bridge_diag(private['name'], ('mda',))}"
            f"\n\n── public-box bridge diag ──\n{bridge_diag(public['name'])}") from e

    text = raw.decode("utf-8", errors="replace")
    assert subject in text, (
        f"home box must serve the decrypted Subject; got first 400B: {text[:400]!r}")
    assert body_marker in text, (
        f"home box must serve the decrypted body; got first 400B: {text[:400]!r}")
    assert msg_id.strip("<>") in text, "home box must serve the decrypted Message-ID"

    # ── The core user property (§ Done definition checkbox 5): after relay + ack
    # + purge the PUBLIC relay box holds no readable/persistent copy — its INBOX
    # is empty (the acked records are tombstoned, which the IMAP fetch path
    # filters). Poll briefly: the public-side tombstone lands on the home box's
    # ack, an instant after the home box stored the record we just read.
    deadline = time.monotonic() + 60
    remaining = None
    while time.monotonic() < deadline:
        remaining = imap_inbox_count("127.0.0.1", pub_993, DOMAIN,
                                     user["username"], user["password"], timeout=30)
        if remaining == 0:
            break
        time.sleep(2.0)
    assert remaining == 0, (
        f"public relay box must hold NO readable copy after the home box acked "
        f"the relay (no-readable/persistent-copy property); public INBOX still "
        f"shows {remaining} message(s)"
        f"\n\n── home-box relay worker ──\n{relay_diag(private['name'])}"
        f"\n\n── public-box federation ──\n{relay_diag(public['name'], ROLES)}"
        f"\n\n── public bridge diag ──\n{bridge_diag(public['name'])}")

    # ── No-MTA boot, re-checked at the end: the home box never started its
    # perimeter parser even though mail is enabled and serving.
    from .helpers import is_commanded_up, svstat
    mta_state = svstat(private["name"], "fauna-mail-bridge-mta")
    assert not is_commanded_up(mta_state), (
        f"home box MTA must remain DOWN throughout (no-MTA boot); got: {mta_state!r}")


# ── Witnesses that start from a linked relay ──────────────────────────
#
# Each outcome of `docs/features/home-nest-behind-a-relay.md` the round trip
# above does not reach, on a deployment already linked (`linked`): the user on
# both boxes, mail serving on both, the pairing row on both. Every read of the
# home box is the LAN path — the host is a device on the home's network.


def _deliver(linked, subject: str, body: str, headers: dict | None = None) -> str:
    """One inbound message from outside, to the relay's MTA (loopback, the
    enforced perimeter cleared by the sender's published SPF/DMARC)."""
    return deliver_inbound_loopback_curl(
        linked["public"]["name"],
        mail_from=f"external-sender@{SENDER_DOMAIN}",
        rcpt_to=linked["user"]["username"],
        subject=subject,
        body_text=body,
        extra_headers=headers,
    )


def _diag(linked) -> str:
    public, private = linked["public"], linked["private"]
    return (f"\n\n── home-box relay worker ──\n{relay_diag(private['name'])}"
            f"\n\n── public-box federation ──\n{relay_diag(public['name'], ROLES)}"
            f"\n\n── home-box bridge diag ──\n{bridge_diag(private['name'], ('mda',))}"
            f"\n\n── public-box bridge diag ──\n{bridge_diag(public['name'])}")


def _home_reads(linked, subject: str, *, mailbox: str = "INBOX",
                timeout: float = 120.0) -> bytes:
    """The message whose Subject carries `subject`, read decrypted from the
    home box's `mailbox` over its LAN IMAP."""
    user = linked["user"]
    try:
        return imap_wait_for_subject(
            "127.0.0.1", linked["private"]["imap_port"], DOMAIN,
            user["username"], user["password"], subject,
            mailbox=mailbox, timeout=timeout)
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}{_diag(linked)}") from e


def _await_relay_inbox(linked, want: int, what: str, timeout: float = 90.0) -> None:
    """Wait for the relay's INBOX (as its MDA serves it) to hold `want`."""
    from helpers.waiting import wait_until

    user = linked["user"]
    seen: list[int] = []

    def holds() -> bool:
        seen.append(imap_inbox_count("127.0.0.1", linked["public"]["mail_ports"][993], DOMAIN,
                                     user["username"], user["password"], timeout=30))
        return seen[-1] == want

    wait_until(holds, timeout, interval=2.0,
               diagnose=lambda: f"{what}: the relay's INBOX shows {seen[-1:]}, wanted {want}"
                                f"{_diag(linked)}")


@pytest.mark.feature("home-nest-behind-a-relay")
def test_mail_waits_on_the_relay_while_the_home_nest_is_unreachable(linked):
    """Outcome 4: mail that arrives while the home box cannot be reached stays
    on the relay, and comes home once the home box is back."""
    public, private = linked["public"], linked["private"]
    subject = f"held while home is away {uuid.uuid4().hex[:8]}"

    # The home box loses its route out before the mail exists, so it cannot
    # have pulled it: whatever the relay shows next, it is holding.
    _network_attach("disconnect", public["network"], private["name"])
    _deliver(linked, subject, "This waited on the relay.\n")
    _await_relay_inbox(linked, 1, "mail for an unreachable home box must wait on the relay")

    _network_attach("connect", public["network"], private["name"])
    raw = _home_reads(linked, subject, timeout=240.0)
    assert b"This waited on the relay." in raw, raw[:400]
    _await_relay_inbox(linked, 0, "once home has it, the relay must hold no copy")


@pytest.mark.feature("home-nest-behind-a-relay")
def test_mail_you_send_lands_in_the_home_sent_folder(linked, run_seal_helper):
    """Outcome 6: a message the user submits (to the relay, the one box that
    takes submission) leaves its Sent copy in the home box's Sent folder."""
    public, user = linked["public"], linked["user"]
    _provision_submission_token(public, run_seal_helper, actor_id=user["actor_id"],
                                signing_seed=user["signing_key"],
                                credential=user["password"])
    subject = f"sent from the relay user {uuid.uuid4().hex[:8]}"
    msg = email.message.EmailMessage()
    msg["From"] = user["username"]
    msg["To"] = user["username"]
    msg["Subject"] = subject
    msg["Message-ID"] = f"<{uuid.uuid4().hex}@e2e.test>"
    msg["Date"] = email.utils.formatdate(localtime=False, usegmt=True)
    msg.set_content("A note to myself.\n")
    submit_message_tls("127.0.0.1", public["mail_ports"][465], DOMAIN,
                       sender=user["username"], password=user["password"],
                       rcpt=user["username"], raw_message=msg.as_bytes())

    raw = _home_reads(linked, subject, mailbox="Sent")
    assert b"A note to myself." in raw, raw[:400]


@pytest.mark.feature("home-nest-behind-a-relay")
def test_mail_the_relay_judged_spam_lands_in_junk_at_home(linked):
    """Outcome 7: what the relay's spam scoring files as spam arrives in the
    home box's Junk; other mail arrives in its INBOX. The fake rspamd scores a
    message by its `X-Test-Rspamd-Score` header (`fakes/fake_rspamd.py`): 16 is
    a combined 8, over the default Junk threshold of 5."""
    tag = uuid.uuid4().hex[:8]
    spam, ham = f"spam for home {tag}", f"ham for home {tag}"
    _deliver(linked, spam, "Scored as spam.\n", headers={"X-Test-Rspamd-Score": "16"})
    _deliver(linked, ham, "Scored as fine.\n")

    assert b"Scored as spam." in _home_reads(linked, spam, mailbox="Junk")
    assert b"Scored as fine." in _home_reads(linked, ham, mailbox="INBOX")


@pytest.mark.feature("home-nest-behind-a-relay")
def test_the_home_nest_serves_mail_on_its_lan_only(linked):
    """Outcome 8: the home box's IMAP/CalDAV answer on its LAN address and on
    no other — its address on the relay's network refuses the connection — and
    once the user turns serving off on the relay, the relay serves them nothing
    while the home box still does."""
    public, private, user = linked["public"], linked["private"], linked["user"]
    home_imap = ("127.0.0.1", private["imap_port"])

    # The LAN path (the host reaches the home box through its LAN network).
    imap_inbox_count(*home_imap, DOMAIN, user["username"], user["password"])

    # From the relay box, the home box's address on the shared network: nothing
    # listens there, so the connection is refused (curl exit 7), not answered.
    home_on_relay_net = _ip_on(private["name"], public["network"])
    for port in (993, 8443):
        probe = subprocess.run(
            ["docker", "exec", public["name"], "curl", "--silent", "-k",
             "--connect-timeout", "15", f"https://{home_on_relay_net}:{port}/"],
            capture_output=True, text=True, timeout=60)
        assert probe.returncode == 7, (
            f"the home box must not listen on its relay-network address "
            f"{home_on_relay_net}:{port}; curl from the relay exited "
            f"{probe.returncode}: {probe.stderr!r}{_diag(linked)}")

    # Never through the relay: the user turns serving off there.
    with WsRpcAdminClient(public["url"], actor_id=user["actor_id"],
                          signing_key=user["signing_key"]) as me:
        me.call("fauna.bridges.set_mail_serving_enabled", {"enabled": False})
    with pytest.raises(AssertionError, match="SELECT INBOX failed|AUTH PLAIN must succeed"):
        imap_inbox_count("127.0.0.1", public["mail_ports"][993], DOMAIN,
                         user["username"], user["password"])
    imap_inbox_count(*home_imap, DOMAIN, user["username"], user["password"])


@pytest.mark.feature("home-nest-behind-a-relay")
def test_a_message_over_the_relay_frame_budget_reaches_home_and_holds_up_nothing(linked):
    """Outcome 10: a message larger than one relay frame (2 MiB) reaches the
    home box whole, and the message after it arrives too."""
    tag = uuid.uuid4().hex[:8]
    big, small = f"big message {tag}", f"after the big one {tag}"
    line = "relay frame budget filler " + "x" * 48 + "\n"
    body = "Start of the big message.\n" + line * (3 * 1024 * 1024 // len(line))
    _deliver(linked, big, body + "End of the big message.\n")
    _deliver(linked, small, "Right behind it.\n")

    assert b"Right behind it." in _home_reads(linked, small, timeout=240.0)
    raw = _home_reads(linked, big, timeout=240.0)
    assert len(raw) > 2 * 1024 * 1024, f"the big message arrived at {len(raw)} bytes"
    assert b"End of the big message." in raw, raw[-400:]
    _await_relay_inbox(linked, 0, "once home has both, the relay must hold no copy")


def _self_signed(hostname: str) -> tuple[str, str, bytes]:
    """(cert PEM, key PEM, cert DER) — a stand-in for the CA-issued cert the
    admin's app obtains; the relay route carries whatever it is handed."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, hostname)])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
            .public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(minutes=5))
            .not_valid_after(now + datetime.timedelta(days=30))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName(hostname)]), False)
            .sign(key, hashes.SHA256()))
    key_pem = key.private_bytes(serialization.Encoding.PEM,
                                serialization.PrivateFormat.PKCS8,
                                serialization.NoEncryption()).decode()
    return (cert.public_bytes(serialization.Encoding.PEM).decode(), key_pem,
            cert.public_bytes(serialization.Encoding.DER))


@pytest.mark.feature("home-nest-behind-a-relay")
def test_a_certificate_for_the_home_nest_reaches_it_through_the_relay(linked_one_admin):
    """Outcome 11: a certificate the admin's app obtains for the home box,
    sealed to the home box and published to the relay, is installed at home on
    its next pull and served by its listener — while the relay, which cannot
    open it, installs nothing and keeps serving its own.

    The request is built by `fauna_ffi.build_publish_cert`: the shared
    `seal_issued_cert_for_nest` the admin-dns machine's delivery calls."""
    from cryptography.hazmat.primitives import serialization
    from fauna_ffi import build_publish_cert
    from helpers.tls_spki import served_leaf

    linked = linked_one_admin
    public, private, user = linked["public"], linked["private"], linked["user"]
    host = "home.example.com"
    cert_pem, key_pem, der = _self_signed(host)

    request = build_publish_cert(cert_pem, key_pem, host, node_id(private),
                                 bytes(user["signing_key"]))
    with WsRpcAdminClient(public["url"], actor_id=user["actor_id"],
                          signing_key=user["signing_key"]) as admin:
        reply = admin.call("fauna.tls.publish_cert", request)
    assert reply.get("ok") is True, f"the relay must store the entry; got {reply!r}"
    assert reply.get("installed") is False, (
        f"the relay must not be able to open a cert sealed to the home box; got {reply!r}")

    def served(port: int) -> bytes:
        return served_leaf("127.0.0.1", port, host).public_bytes(serialization.Encoding.DER)

    from helpers.waiting import wait_until
    wait_until(lambda: served(private["port"]) == der, 120,
               diagnose=lambda: _diag(linked))
    assert served(public["port"]) != der, "the relay must never serve the home box's cert"

"""The non-loopback mail-perimeter venue the tier_4 perimeter witnesses share.

The process-level `mail_bridge_mta` venue dials from 127.0.0.1 and resolves
through the dev machine's own resolver, and the bridge exempts loopback from
every greeting-name, sender-domain, blocklist and per-IP rule
(`internal/mta/policy.go`, `server.go`). This venue makes both controllable
against the deploy image:

  * **The resolver.** The container's `--dns` points at a `fakes/fake_dns`
    sidecar backed by a records file the test rewrites, so a blocklist zone, a
    greeting name's address, a reverse name (PTR) and a SERVFAIL are whatever
    the test publishes. Docker's embedded resolver forwards a query for a
    non-container name or address to that sidecar and passes its answer back.
  * **The peer.** The SMTP client is the test process on the host, which
    reaches the published port 25 through docker-proxy, so the MTA sees the
    network's gateway address as the connecting server: not loopback, so every
    perimeter gate is live.

DATA-time sender checks (SPF, DKIM, DMARC) run the same on a loopback delivery,
so witnesses of those use the in-container `curl` path
(`deliver_inbound_raw_loopback_curl`), with the sidecar publishing the records.

Each test writes the whole spam or auth policy it needs (both puts replace the
whole group), then POLLS the verdict an outside server sees until the saved
value shows — a save reaches the running bridge by the `config_changed` push and
applies at the next connection, so a sleep would be either flaky or slow.

The port-25 per-IP message rate (50 accepted MAIL FROMs an hour, held for the
bridge process's life) counts every accepted probe from the gateway, so one
venue serves one module, and a test that spends the budget opens its own.

Used by `test_mail_perimeter_dns.py` (admin-mail-policy) and
`test_mail_abuse_perimeter.py` (mail-abuse-refused).
"""

import contextlib
import os
import smtplib
import ssl
import subprocess
import time

from .helpers import (
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    create_network,
    docker_build,
    find_free_port,
    find_free_ports,
    get_repo_root,
    provision_mail_recipient,
    put_auth_policy,
    register_primary_domain,
    relax_spam_policy,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    wait_for_health,
    write_dns_records,
)

DOMAIN = "localhost"
RECIPIENT_PASSWORD = "perimeter-dns-password-1"

# How long a saved value may take to reach the running bridge (push → re-fetch
# → swap at the next connection). A budget for the poll, not a sleep.
APPLY_BUDGET_S = 60.0

# The names the host peer can present. `.test` is reserved (RFC 6761).
PEER_NAME = "peer.example.test"          # A → the peer's own address
LIAR_NAME = "liar.example.test"          # A → somewhere else
SENDER_DOMAIN = "sender.example.test"    # A → the peer (sender-domain check passes)
ELSEWHERE_IP = "198.51.100.7"            # TEST-NET-2: never the peer

# The spam-group posture every perimeter test starts from: every DNS gate off,
# so each test turns on exactly the gate it witnesses. A conn-rate high enough
# for the polls; greylist off so a MAIL FROM is the whole exchange.
QUIET_SPAM_POLICY = {
    "dnsbl_servers": [],
    "greylist_enabled": False,
    "greylist_delay_secs": 0,
    "fcrdns_mode": "off",
    "reject_fcrdns_fail": False,
    "reject_no_rdns": False,
    "helo_identity_required": False,
    "max_conn_per_min": 1000,
    "baseline_standing_publish": False,
}


def put_spam_policy(nest, *, omit: tuple[str, ...] = (), **overrides) -> None:
    """Write the quiet policy with `overrides`; a field named in `omit` is left
    out of the put, so the nest applies its shipped default for it."""
    policy = {**QUIET_SPAM_POLICY, **overrides}
    for field in omit:
        policy.pop(field, None)
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.put_spam_policy", policy)


def poll(probe, want, what: str, diag):
    """Run `probe()` until `want(result)` holds or the apply budget runs out."""
    deadline = time.monotonic() + APPLY_BUDGET_S
    seen = []
    while True:
        got = probe()
        if want(got):
            return got
        seen.append(got)
        if time.monotonic() > deadline:
            raise AssertionError(
                f"{what}: never observed within {APPLY_BUDGET_S}s; saw {seen[-5:]!r}{diag()}")
        time.sleep(0.5)


def network_gateway(network: str) -> str:
    r = subprocess.run(
        ["docker", "network", "inspect", "-f",
         "{{range .IPAM.Config}}{{.Gateway}}{{end}}", network],
        capture_output=True, text=True, timeout=10,
    )
    gw = r.stdout.strip()
    if not gw:
        raise RuntimeError(f"no gateway for network {network}: {r.stderr!r}")
    return gw


def reversed_ip(ip: str) -> str:
    return ".".join(reversed(ip.split(".")))


# ── The host peer's SMTP exchanges ────────────────────────────────────


def _reply(code: int, text: bytes) -> str:
    return f"{code} {text.decode(errors='replace')}"


def _quit(client: smtplib.SMTP) -> None:
    try:
        client.quit()
    except (smtplib.SMTPException, OSError):
        pass


def session_opening(port: int, *, helo: str) -> str:
    """Connect and greet as `helo`; return the first non-220 reply, or the EHLO
    reply when every one so far said yes. The MTA opens its session — and so runs
    its connect-time gates — when the client greets, so a refused connection is
    answered there, before any mail command."""
    client = smtplib.SMTP(timeout=30)
    try:
        code, text = client.connect("127.0.0.1", port)
        if code != 220:
            return _reply(code, text)
        return _reply(*client.ehlo(helo))
    finally:
        _quit(client)


def _starttls(client: smtplib.SMTP, helo: str) -> str | None:
    """Greet as `helo` (sent verbatim — empty and garbled names included), then
    STARTTLS (port 25 requires it) and greet again. Returns the first refusal as
    `<command> <code> <text>`, or None when every step said yes."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    code, text = client.docmd("EHLO", helo)
    if code != 250:
        return f"EHLO {_reply(code, text)}"
    code, text = client.docmd("STARTTLS")
    if code != 220:
        return f"STARTTLS {_reply(code, text)}"
    client.sock = ctx.wrap_socket(client.sock)
    client.file = None
    code, text = client.docmd("EHLO", helo)
    if code != 250:
        return f"EHLO {_reply(code, text)}"
    return None


def mail_from_reply(port: int, *, helo: str, mail_from: str) -> str:
    """Greet as `helo`, STARTTLS, then MAIL FROM; return that command's reply as
    `<code> <text>` — the first reply the greeting-name, identity, reverse-name
    and sender-domain gates answer on. A refusal before MAIL FROM is returned as
    the reply it was (prefixed with the command), so a poll waiting for a saved
    value to apply sees it rather than dying on it."""
    client = smtplib.SMTP(timeout=30)
    try:
        code, text = client.connect("127.0.0.1", port)
        if code != 220:
            return f"CONNECT {_reply(code, text)}"
        refused = _starttls(client, helo)
        if refused:
            return refused
        return _reply(*client.docmd("MAIL", f"FROM:<{mail_from}>"))
    finally:
        _quit(client)


def mail_from_replies(port: int, *, helo: str, mail_from: str, count: int) -> list[str]:
    """On ONE connection, greet as `helo`, STARTTLS, then send `count` MAIL FROM
    commands, each followed by RSET; return every MAIL FROM reply. One connection
    keeps the per-IP connection rate out of the picture: what is counted is the
    message transaction."""
    client = smtplib.SMTP(timeout=30)
    try:
        code, text = client.connect("127.0.0.1", port)
        if code != 220:
            return [f"CONNECT {_reply(code, text)}"]
        refused = _starttls(client, helo)
        if refused:
            return [refused]
        replies = []
        for _ in range(count):
            replies.append(_reply(*client.docmd("MAIL", f"FROM:<{mail_from}>")))
            client.docmd("RSET")
        return replies
    finally:
        _quit(client)


# ── The venue ─────────────────────────────────────────────────────────


@contextlib.contextmanager
def perimeter_venue(tmp_path_factory, run_seal_helper, *, label: str, claim_code: str,
                    recipients: tuple[str, ...], txt_records: dict[str, str] | None = None):
    """One claimed, serving container on a user-defined network carrying a
    records-file-backed `fake_dns` resolver (constructed with `txt_records`) and
    the fail-closed scan sidecars (an accepted message reaches the scan gate),
    with a mail recipient provisioned for each local part in `recipients`.
    Yields the nest handle plus the network's gateway — the address the host peer
    arrives as — the records directory each test publishes into, and
    `recipients` mapped local part → recipient record."""
    docker_build(get_repo_root())
    suffix = find_free_port()
    network = f"fauna-{label}-net-{suffix}"
    dns_name = f"fauna-{label}-dns-{suffix}"
    clamd_name = f"fauna-{label}-clamd-{suffix}"
    rspamd_name = f"fauna-{label}-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    records_dir = str(tmp_path_factory.mktemp(label))
    os.chmod(records_dir, 0o777)  # the sidecar reads it through a bind mount
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-mail-{label}-{http_port}"
    create_network(network)
    try:
        gateway = network_gateway(network)
        scanners = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name, records_dir=records_dir,
            txt_records=txt_records or {},
        )
        start_container_with_ports(
            name,
            {3000: http_port, **mail_ports},
            env={
                "FAUNA_CLAIM_CODE": claim_code,
                "FAUNA_PORT": "3000",
                "FAUNA_CLAMD_ADDR": scanners["clamd_addr"],
                "FAUNA_RSPAMD_URL": scanners["rspamd_url"],
            },
            network=network,
            dns=dns_ip,
        )
        try:
            wait_for_health(http_port, name)
            admin = claim_admin_api(http_port, claim_code, handle="admin")
            nest = {
                "name": name,
                "port": http_port,
                "url": f"https://127.0.0.1:{http_port}",
                "admin": admin,
                "mail_ports": mail_ports,
            }
            register_primary_domain(nest, DOMAIN)
            relax_spam_policy(nest)
            put_auth_policy(nest, log_only=False, enforce_dmarc=True,
                            enforce_spf_hardfail=True, enforce_dkim=False)
            provisioned = {
                local: provision_mail_recipient(
                    nest, run_seal_helper, domain=DOMAIN, local_part=local,
                    password=RECIPIENT_PASSWORD,
                )
                for local in recipients
            }
            bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
            yield {
                **nest,
                "gateway": gateway,
                "records_dir": records_dir,
                "recipients": provisioned,
                "diag": lambda: f"\n\n── bridge diagnostics ──\n{bridge_diag(name)}",
            }
        finally:
            remove_container(name)
    finally:
        remove_container(dns_name)
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_network(network)


def publish(nest, *, a: dict[str, str] | None = None, ptr_name: str | None = None,
            servfail: list[str] | None = None) -> None:
    """Publish the host peer's DNS: its greeting names (`PEER_NAME` resolves to
    it, `LIAR_NAME` elsewhere), its sender domain, the extra `a` records, its
    reverse name when `ptr_name` is given, and the `servfail` names. One file
    per call (the sidecar reloads the whole file)."""
    gw = nest["gateway"]
    write_dns_records(
        nest["records_dir"],
        a={PEER_NAME: gw, LIAR_NAME: ELSEWHERE_IP, SENDER_DOMAIN: gw, **(a or {})},
        ptr={gw: ptr_name} if ptr_name else None,
        servfail=servfail,
    )

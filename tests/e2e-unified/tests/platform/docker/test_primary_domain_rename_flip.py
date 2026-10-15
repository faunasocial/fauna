"""tier_4 e2e: a primary-domain rename driven THROUGH its anchor flip on the
Docker image, against an in-network ``pebble`` test CA — what the Domains page
promises once the new domain has become the primary
(``mail-primary-domain-rename.md`` § Lifecycle, § Goal, § Wire shapes,
§ Renaming away from a dead domain).

Why this tier. A rename enters ``cert_issuance`` at start and flips only after
the deployment's managed certificate is re-issued to cover ``mail.<new>``
(SLICE 2 → SLICE 3, ``acme_http01::cert_lifecycle_loop``). A harness nest has
no certificate authority to order from, so it rests in ``cert_issuance`` for
ever; the image here orders from pebble (``FAUNA_ACME_DIRECTORY_URL``), which
validates every SAN over HTTP-01 against the nest's challenge listener. The
pre-flip refusals stay at the api tier
(``tests/api/test_primary_domain_rename_lifecycle.py``).

Two resolvers read one ``fake_dns`` sidecar: pebble's (``-dnsserver``), to
reach the nest for each SAN, and the nest's own (``--dns``), whose strong
resolve gates decide which names enter the all-or-nothing order — the
secondary apex against the persisted nest address, ``mail.<new>`` against the
persisted mail address (strong-check-or-DROP, so ``set_host_address`` comes
first). Both addresses are the nest container's: pebble validates
``mail.<new>`` on the same listener.

The flip lands on the loop's tick AFTER the one that stamps ``cert_ready``
(``cert_ready`` stays an observable resting state), and nothing an admin does
wakes that tick, so each flip waits out the loop's steady poll.
``_FLIP_CEILING_S`` is a ceiling, never a target (convention 14).

The mail-serving variant of the stack (``rename_mail_stack``) adds the image's
own mail bridges, the fake scanners an enforced inbound perimeter needs, and a
stub external MX the outbound worker relays ``external.test`` to — what the
grace window promises peers (§ MX invalidation, § Behavior — RFC 8058
unsubscribe endpoint continuation) is read off that stack.
"""

from __future__ import annotations

import http.client
import json
import os
import re
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.parse
from contextlib import contextmanager

import pytest
from cryptography import x509

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import (
    create_actor_and_register,
    register_handled_actor,
    set_registration_mode,
)
from helpers.local_domains import active_domains, add_local_domain, set_host_address
from helpers.mail_wire import _connect_smtp_starttls
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.tls_spki import served_leaf
from helpers.waiting import wait_until

from .helpers import (
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    container_ip,
    create_network,
    deliver_inbound_loopback_curl,
    docker_build,
    enforce_mail_perimeter,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_mail_recipient,
    read_stub_mx_message,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    start_stub_mx_sidecar,
    wait_for_health,
    write_dns_records,
)
from .test_acme_http01_pebble_issuance import (
    PEBBLE_CONFIG,
    _extract_pebble_ca,
    _image_ca_bundle,
    _leaf_is_pebble,
    _sans,
    _start_pebble,
    _wait_tcp,
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

OLD = "nest.test"
NEW = "renamed.test"
CLAIM_CODE = "RENAME51"

# The outside sender of the inbound message (outcome 45): delivered over the
# container's loopback, so its published SPF authorizes 127.0.0.1 under a
# `p=reject` DMARC policy the enforced perimeter checks for real.
SENDER_DOMAIN = "sender.test"
# The list members' domain (outcome 46), relayed to the stub external MX.
EXTERNAL_DOMAIN = "external.test"

# The loop's steady poll (five minutes, `acme_http01.rs`) plus pebble's
# validation of the widened order and one paced retry.
_FLIP_CEILING_S = 480


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield


@contextmanager
def _rename_stack(*, mail: bool):
    """pebble + ``fake_dns`` + a fresh claimed nest ordering from pebble and
    resolving through the sidecar, with the old primary and the new domain
    both hosted and published. Yields the nest dict, carrying the sidecar's
    ``records_dir`` and the ``nest_ip`` its records point at; tears the whole
    stack down after.

    ``mail`` adds the four mail ports (``nest["mail_ports"]``), the fake
    clamd/rspamd scanners, and a stub external MX on the same network that the
    MTA relays ``external.test`` to; its deliveries land in ``nest["out_dir"]``.
    The bridges stay down — the test brings them up."""
    pid = os.getpid()
    net = f"fauna-rename-net-{pid}"
    dns_name = f"fauna-rename-dns-{pid}"
    pebble_name = "pebble"  # the directory cert's SAN and the URL below
    (api_port, pebble_host_port, *mail_host_ports) = find_free_ports(6 if mail else 2)
    mail_ports = dict(zip((25, 465, 587, 993), mail_host_ports))
    nest_name = f"fauna-rename-nest-{api_port}"

    workdir = tempfile.mkdtemp(prefix="fauna-rename-")
    records_dir = os.path.join(workdir, "dns")
    os.makedirs(records_dir, exist_ok=True)
    out_dir = os.path.join(workdir, "stub-mx")

    started = []
    try:
        create_network(net)
        fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
        dns_ip = start_fake_dns_sidecar(
            net, fakes_dir, name=dns_name, txt_records={}, records_dir=records_dir
        )
        started.append(dns_name)

        mail_env = {}
        if mail:
            clamd_name = f"fauna-rename-clamd-{pid}"
            rspamd_name = f"fauna-rename-rspamd-{pid}"
            scan = start_fake_scanner_sidecars(
                net, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name
            )
            started += [clamd_name, rspamd_name]
            # The sidecar (container root, possibly userns-remapped) writes its
            # deliveries through the bind mount.
            os.makedirs(out_dir)
            os.chmod(out_dir, 0o777)
            stub_name = f"fauna-rename-stubmx-{pid}"
            helpers_dir = str(get_repo_root() / "tests" / "e2e-unified" / "helpers")
            stub_target = start_stub_mx_sidecar(net, helpers_dir, out_dir, name=stub_name)
            started.append(stub_name)
            mail_env = {
                "FAUNA_MODE": "public",
                "FAUNA_CLAMD_ADDR": scan["clamd_addr"],
                "FAUNA_RSPAMD_URL": scan["rspamd_url"],
                "FAUNA_MTA_MX_OVERRIDE": f"{EXTERNAL_DOMAIN}={stub_target}",
            }

        config_path = os.path.join(workdir, "pebble-config.json")
        with open(config_path, "w", encoding="utf-8") as f:
            json.dump(PEBBLE_CONFIG, f)
        _start_pebble(net, pebble_name, config_path, f"{dns_ip}:53", pebble_host_port)
        started.append(pebble_name)
        _wait_tcp("127.0.0.1", pebble_host_port, timeout=30)

        bundle_path = os.path.join(workdir, "ssl-bundle.pem")
        with open(bundle_path, "wb") as f:
            f.write(_image_ca_bundle())
            f.write(b"\n")
            f.write(_extract_pebble_ca(pebble_name))
        os.chmod(bundle_path, 0o644)

        start_container_with_ports(
            nest_name,
            {3000: api_port, **mail_ports},
            env={
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_ACME_DIRECTORY_URL": f"https://{pebble_name}:14000/dir",
                "SSL_CERT_FILE": "/etc/fauna/ssl-bundle.pem",
                **mail_env,
            },
            network=net,
            dns=dns_ip,
            mounts=[(bundle_path, "/etc/fauna/ssl-bundle.pem")],
        )
        started.append(nest_name)

        nest_ip = container_ip(nest_name)
        _publish(records_dir, nest_ip, [OLD, NEW])

        wait_for_health(api_port, nest_name)
        admin = claim_admin_api(api_port, CLAIM_CODE, handle="admin")
        nest = {
            "name": nest_name,
            "port": api_port,
            "url": f"https://127.0.0.1:{api_port}",
            "admin": admin,
            "records_dir": records_dir,
            "nest_ip": nest_ip,
            "mail_ports": mail_ports,
            "out_dir": out_dir,
        }
        with admin_ws(nest) as ws:
            set_host_address(ws, nest_ipv4=nest_ip, mail_ipv4=nest_ip)
            assert add_local_domain(ws, OLD)["domain"]["is_primary"] is True
            add_local_domain(ws, NEW)
        yield nest
    finally:
        for name in reversed(started):
            remove_container(name)
        remove_network(net)
        subprocess.run(["rm", "-rf", workdir], capture_output=True, timeout=10)


@pytest.fixture()
def rename_stack(docker_image):
    with _rename_stack(mail=False) as nest:
        yield nest


@pytest.fixture()
def rename_mail_stack(docker_image):
    with _rename_stack(mail=True) as nest:
        yield nest


def _publish(records_dir: str, nest_ip: str, domains: list[str]) -> None:
    """The zone as the sidecar serves it: each domain and its ``mail.`` host
    point at the nest; a domain left out does not resolve at all. The outside
    sender's passing SPF and DMARC policy ride along on every write (the
    sidecar reloads the file whole)."""
    a = {name: nest_ip for domain in domains for name in (domain, f"mail.{domain}")}
    write_dns_records(
        records_dir,
        a={**a, SENDER_DOMAIN: "127.0.0.1"},
        mx={SENDER_DOMAIN: f"0 {SENDER_DOMAIN}"},
        txt={
            SENDER_DOMAIN: "v=spf1 ip4:127.0.0.1 -all",
            f"_dmarc.{SENDER_DOMAIN}": "v=DMARC1; p=reject",
        },
    )


def _lifecycle_log(nest) -> str:
    logs = subprocess.run(
        ["docker", "logs", nest["name"]], capture_output=True, text=True, timeout=30)
    lines = [
        line for line in (logs.stdout + logs.stderr).splitlines()
        if "rename" in line or "ACME" in line or "cert-lifecycle" in line
    ]
    return "\n".join(lines[-40:])


def _domains(ws) -> dict[str, dict]:
    return {d["domain_name"]: d for d in active_domains(ws)}


def _start_rename(ws, grace_days: int) -> bytes:
    new_id = bytes(_domains(ws)[NEW]["domain_id"])
    rename = ws.call("fauna.bridges.start_primary_domain_rename", {
        "new_primary_domain_id": new_id,
        "grace_days": grace_days,
    })["rename"]
    return bytes(rename["rename_id"])


def _wait_for_grace(nest) -> dict:
    """Poll the rename until the anchor flip has put it in ``grace``; on the
    ceiling, fail with the row, the certificate status and the loop's log."""
    last = None

    def in_grace():
        nonlocal last
        with admin_ws(nest) as ws:
            last = ws.call("fauna.bridges.get_primary_domain_rename_status", {})["rename"]
        return last if last and last["state"] == "grace" else None

    def diagnose() -> str:
        with admin_ws(nest) as ws:
            cert = ws.call("fauna.tls.cert_status", {"domains": [OLD, NEW, f"mail.{NEW}"]})
        return (
            f"the rename never reached grace; last row={last!r}\ncert_status={cert!r}\n"
            f"--- nest cert-lifecycle log ---\n{_lifecycle_log(nest)}"
        )

    return wait_until(in_grace, _FLIP_CEILING_S, interval=5, diagnose=diagnose)


def _primary(ws) -> str:
    return next(name for name, d in _domains(ws).items() if d["is_primary"])


@pytest.mark.feature("admin-dns-and-certificates")
def test_aborting_a_rename_during_grace_makes_the_old_domain_primary_again(rename_stack):
    """Outcome 42: a rename aborted after its anchor flip — during grace, when
    the new domain is already the primary — puts the old domain back as the
    primary; the new domain stays an ordinary hosted domain."""
    nest = rename_stack
    with admin_ws(nest) as ws:
        rename_id = _start_rename(ws, grace_days=7)
    _wait_for_grace(nest)
    with admin_ws(nest) as ws:
        assert _primary(ws) == NEW, _domains(ws)
        aborted = ws.call("fauna.bridges.abort_primary_domain_rename", {
            "rename_id": rename_id,
        })["rename"]
        assert aborted["state"] == "aborted", aborted
        assert _primary(ws) == OLD, _domains(ws)
        assert NEW in _domains(ws), _domains(ws)
        assert ws.call("fauna.bridges.get_primary_domain_rename_status", {})["rename"] is None


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups(rename_stack):
    """Outcomes 41, 43, 44 and 48.

    41 — during grace, finishing without the override is refused
    ``grace_period_not_expired`` and leaves the rename in grace; with it, the
    rename completes. 44 — the completed rename leaves the old domain an
    ordinary active hosted domain. 43 — a member whose address was on the old
    domain keeps exactly that address and gets none on the new one. 48 — a
    signup afterwards lands on the new domain, whichever hosted domain the
    newcomer found the nest through."""
    nest = rename_stack
    member = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"])

    def member_ws() -> WsRpcAdminClient:
        return WsRpcAdminClient(
            nest["url"], actor_id=member["actor_id_bytes"],
            signing_key=bytes(member["signing_key"]))

    def member_address_domains() -> set[str]:
        with member_ws() as ws:
            return {a["local_domain"] for a in ws.call(
                "fauna.bridges.list_account_aliases", {})["aliases"]}

    with member_ws() as ws:
        provision_recipient_seal_key(ws, member["actor_id_bytes"])
    assert member_address_domains() == {OLD}

    with admin_ws(nest) as ws:
        rename_id = _start_rename(ws, grace_days=7)
    _wait_for_grace(nest)

    with admin_ws(nest) as ws:
        with pytest.raises(RpcCallError) as refused:
            ws.call("fauna.bridges.complete_primary_domain_rename", {"rename_id": rename_id})
        assert refused.value.code == "fauna.bridges.grace_period_not_expired", refused.value
        status = ws.call("fauna.bridges.get_primary_domain_rename_status", {})["rename"]
        assert status["state"] == "grace", status

        done = ws.call("fauna.bridges.complete_primary_domain_rename", {
            "rename_id": rename_id, "force": True,
        })["rename"]
        assert done["state"] == "completed", done

        domains = _domains(ws)
        assert domains[NEW]["is_primary"] is True, domains
        assert OLD in domains and domains[OLD]["is_primary"] is False, domains
        assert domains[OLD]["removed_at"] is None, domains[OLD]

    assert member_address_domains() == {OLD}

    set_registration_mode(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"])
    # Found through either hosted domain, a newcomer is placed on the new one.
    for handle, found_via in (("joins-via-old", OLD), ("joins-via-new", NEW)):
        joined = register_handled_actor(nest["port"], handle, found_via, base_url=nest["url"])
        assert joined["registered_domain"] == NEW, joined


def _poll(what: str, ceiling_s: int, probe):
    """Re-run ``probe`` until it returns a truthy verdict; on the ceiling, fail
    with its last answer. ``probe`` returns ``(ok, last)``."""
    seen = None

    def check():
        nonlocal seen
        ok, seen = probe()
        return ok

    wait_until(check, ceiling_s, interval=3, diagnose=lambda: f"{what}; last={seen!r}")
    return seen


def _old_primary_trusted(nest):
    with admin_ws(nest) as ws:
        status = ws.call("fauna.tls.cert_status", {"domains": [OLD]})["statuses"][0]
    return status["state"] == "ValidTrusted" and status["is_floor"] is False, status


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_rename_away_from_a_dead_primary_completes(rename_stack):
    """Outcome 51: the box holds a trusted certificate for its primary, then
    the primary's zone lapses — its names stop resolving, and the nest's own
    resolver says so — yet the rename to the new domain starts, flips and
    completes, with the minimum grace the dead-domain case calls for.

    The lapse comes after the first certificate on purpose: a box whose
    primary never resolved has already spent an attempt of its
    failed-validation budget before any rename exists, and the budget then
    holds the rename's order back for a quarter of an hour — correct pacing,
    but not this outcome."""
    nest = rename_stack
    _poll("a trusted certificate for the old primary", 180, lambda: _old_primary_trusted(nest))

    _publish(nest["records_dir"], nest["nest_ip"], [NEW])

    def old_primary_missing():
        with admin_ws(nest) as ws:
            domains = ws.call("fauna.dns.verify_records", {"domain": OLD})["domains"]
        rows = [r for d in domains for r in d["records"]
                if r["name"] == OLD and r["record_type"] == "A"]
        return bool(rows) and all(r["status"] == "missing" for r in rows), rows

    _poll("the nest seeing the old primary's address record gone", 240, old_primary_missing)

    with admin_ws(nest) as ws:
        rename_id = _start_rename(ws, grace_days=1)
    _wait_for_grace(nest)
    with admin_ws(nest) as ws:
        done = ws.call("fauna.bridges.complete_primary_domain_rename", {
            "rename_id": rename_id, "force": True,
        })["rename"]
        assert done["state"] == "completed", done
        assert _primary(ws) == NEW, _domains(ws)


def _smtp_starttls_leaf(port: int, sni: str) -> x509.Certificate:
    """The leaf the inbound MTA presents on port 25 after STARTTLS for ``sni`` —
    what a peer MTA delivering to that mail host is handed. No MAIL FROM is
    sent, so the HELO gates (they fire at the first MAIL FROM) never run."""
    deadline = time.monotonic() + 30
    with _connect_smtp_starttls(port, sni, deadline) as conn:
        der = conn._sock.getpeercert(binary_form=True)
    return x509.load_der_x509_certificate(der)


def _restore_trusted_cert(nest) -> None:
    """The mail bring-up overrides the nest's certificate with a self-signed one
    (``provision_self_signed_cert``, which keeps the real one aside) — a harness
    step no real deploy takes. Undo it the way an admin would ("switch back to a
    real certificate"), until the old primary is served its trusted one again."""
    def restored():
        with admin_ws(nest) as ws:
            ws.call("fauna.bridges.restore_real_tls_cert", {})
        return _leaf_is_pebble(served_leaf("127.0.0.1", nest["port"], OLD))

    wait_until(
        restored, 120, interval=5,
        diagnose=lambda: (
            "the old primary is still served a self-signed certificate after restoring "
            "the real one; issuer="
            f"{served_leaf('127.0.0.1', nest['port'], OLD).issuer.rfc4514_string()!r}\n"
            f"--- bridge_diag ---\n{bridge_diag(nest['name'])}"),
    )


def _member(ws, list_id) -> dict:
    (row,) = ws.call("fauna.bridges.list_list_members", {
        "list_id": list_id, "include_unsubscribed": True,
    })["members"]
    return row


def _one_click_url(raw: bytes) -> str:
    """The https half of a delivered ``List-Unsubscribe`` header, unfolded."""
    unfolded = re.sub(rb"\r?\n[ \t]+", b" ", raw)
    m = re.search(rb"<(https://[^>]+/list/unsubscribe\?t=[A-Za-z0-9_\-]+)>", unfolded)
    assert m, f"no List-Unsubscribe https link in the delivered list mail:\n{raw[:800]!r}"
    return m.group(1).decode()


def _post_one_click_on_its_host(nest, url: str) -> tuple[int, x509.Certificate]:
    """POST the RFC 8058 one-click body to ``url`` the way a mail client does —
    TLS with the link's host as SNI and as ``Host`` — with the connection pinned
    to the nest's listener (the link's host resolves nowhere on the test host).
    Returns the HTTP status and the leaf served for that host."""
    parts = urllib.parse.urlsplit(url)
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", nest["port"]), timeout=15)
    tls = ctx.wrap_socket(raw, server_hostname=parts.hostname)
    leaf = x509.load_der_x509_certificate(tls.getpeercert(binary_form=True))
    conn = http.client.HTTPSConnection(parts.hostname, nest["port"], timeout=15, context=ctx)
    conn.sock = tls
    try:
        conn.request("POST", f"{parts.path}?{parts.query}", body=b"List-Unsubscribe=One-Click",
                     headers={"Host": parts.hostname,
                              "Content-Type": "application/x-www-form-urlencoded"})
        return conn.getresponse().status, leaf
    finally:
        conn.close()


@pytest.mark.feature("admin-dns-and-certificates")
def test_during_grace_the_old_mail_host_delivers_and_old_unsubscribe_links_keep_working(
        rename_mail_stack, run_seal_helper):
    """Outcomes 45 and 46, on one box serving mail for the old primary.

    45 — during the rename's grace period (the new domain already the primary),
    a peer still delivering to ``mail.<old>`` is handed a trusted certificate
    that names ``mail.<old>`` on port 25, and a message it delivers there to a
    member's ``@<old>`` address reaches that member's inbox
    (`mail-primary-domain-rename.md` § MX invalidation — the grace keep-alive
    holds ``mail.<old>`` on the certificate).

    46 — list mail sent before the rename carries a one-click unsubscribe link
    on the old domain; POSTed to that domain during grace, and (for a second
    list's member) after the rename completes, each unsubscribes its member
    (§ Behavior — RFC 8058 unsubscribe endpoint continuation)."""
    nest = rename_mail_stack
    ports = nest["mail_ports"]
    old_mx = f"mail.{OLD}"

    _poll("a trusted certificate for the old primary", 180, lambda: _old_primary_trusted(nest))
    enforce_mail_perimeter(nest, domain=OLD)
    reader = provision_mail_recipient(
        nest, run_seal_helper, domain=OLD, local_part="reader", password="rename-reader-pw")
    bring_bridges_to_serving(nest["name"], nest, ports, OLD)
    _restore_trusted_cert(nest)

    # Before the rename: one list mail out per list, each to one outside member.
    owner = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"])

    def owner_ws() -> WsRpcAdminClient:
        return WsRpcAdminClient(
            nest["url"], actor_id=owner["actor_id_bytes"],
            signing_key=bytes(owner["signing_key"]))

    lists = {}
    with owner_ws() as ws:
        for phase in ("grace", "completed"):
            local_part = f"news-{phase}"
            list_id = ws.call("fauna.bridges.create_account_list", {
                "local_part": local_part, "local_domain": OLD,
            })["list_id"]
            ws.call("fauna.bridges.add_list_member", {
                "list_id": list_id, "recipient_address": f"sub-{phase}@{EXTERNAL_DOMAIN}",
            })
            tag = f"rename-list-{phase}-{os.getpid()}"
            message = (
                f"From: {local_part}@{OLD}\r\nTo: {local_part}@{OLD}\r\n"
                f"Subject: Issue {tag}\r\nMessage-ID: <{tag}@{OLD}>\r\n"
                "Date: Wed, 14 Oct 2026 12:00:00 +0000\r\nMIME-Version: 1.0\r\n"
                "Content-Type: text/plain; charset=utf-8\r\n\r\n"
                "A list issue sent before the rename.\r\n"
            ).encode()
            assert ws.call("fauna.bridges.send_list_message", {
                "list_id": list_id, "message": message,
            })["queued_count"] == 1
            lists[phase] = (list_id, tag)

    links = {}
    for phase, (_, tag) in lists.items():
        # The list fan-out rides the outbound worker's 30 s poll.
        raw = read_stub_mx_message(nest["out_dir"], tag, timeout=120)
        assert raw is not None, (
            f"the list mail tagged {tag} never reached the outside MX\n"
            f"--- bridge_diag ---\n{bridge_diag(nest['name'])}")
        links[phase] = _one_click_url(raw)
        assert urllib.parse.urlsplit(links[phase]).hostname == OLD, links[phase]

    with admin_ws(nest) as ws:
        rename_id = _start_rename(ws, grace_days=7)
    _wait_for_grace(nest)
    with admin_ws(nest) as ws:
        assert _primary(ws) == NEW, _domains(ws)

    # 45 — the old mail host still serves a certificate a peer accepts…
    def old_mx_covered():
        leaf = _smtp_starttls_leaf(ports[25], old_mx)
        return _leaf_is_pebble(leaf) and old_mx in _sans(leaf), (
            leaf.issuer.rfc4514_string(), _sans(leaf))

    _poll(f"port 25 serving a trusted certificate naming {old_mx} during grace", 90,
          old_mx_covered)

    # …and mail delivered to it reaches the member on the old domain.
    subject = "Delivered to the old mail host during grace"
    msg_id = deliver_inbound_loopback_curl(
        nest["name"],
        mail_from=f"outside-sender@{SENDER_DOMAIN}",
        rcpt_to=reader["username"],
        subject=subject,
        body_text="Sent by a server that still has the old MX cached.\n",
        server_name=old_mx,
    )
    try:
        delivered = imap_fetch_only_inbox_message(
            "127.0.0.1", ports[993], old_mx, reader["username"], reader["password"])
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(nest['name'])}") from e
    text = delivered.decode("utf-8", errors="replace")
    assert subject in text and msg_id.strip("<>") in text, text[:400]

    # 46 — during grace, the pre-rename link on the old domain unsubscribes.
    def unsubscribe(phase: str) -> None:
        status, leaf = _post_one_click_on_its_host(nest, links[phase])
        assert _leaf_is_pebble(leaf) and OLD in _sans(leaf), (
            f"{OLD} is not served a trusted certificate naming it: "
            f"issuer={leaf.issuer.rfc4514_string()!r} sans={_sans(leaf)!r}")
        assert status == 200, f"one-click POST to {links[phase]} answered {status}"
        list_id, _ = lists[phase]
        with owner_ws() as ws:
            member = _member(ws, list_id)
        assert member["unsubscribed_at"] is not None, member

    unsubscribe("grace")

    with admin_ws(nest) as ws:
        done = ws.call("fauna.bridges.complete_primary_domain_rename", {
            "rename_id": rename_id, "force": True,
        })["rename"]
        assert done["state"] == "completed", done
        assert OLD in _domains(ws), _domains(ws)

    # …and after the rename completes, while the old domain stays hosted.
    unsubscribe("completed")

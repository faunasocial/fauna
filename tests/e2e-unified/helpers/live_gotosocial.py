"""A **real, public** GoToSocial peer on its own Hetzner VPS, for the live
ActivityPub federation e2e (``tests/live/test_activitypub_federation_live.py``).

Why this exists separately from ``helpers/fediverse_peer.py``'s
``GoToSocialPeer``: that class drives a *local* compose stack behind a per-run
nginx TLS front, so every REST call is ``curl --resolve <domain>:<port>:127.0.0.1
--cacert <per-run CA>`` and every account is minted via ``docker compose exec``.
None of that reaches a genuinely public box with a genuinely trusted cert — and
a genuinely public peer is the whole point of the live test: the production nest
image compiles OUT the DNS/CA test hooks
(``docs/goal/behavior/activitypub.md`` § Implementation status today), so it can
only federate with a peer that resolves in real DNS and presents a
publicly-trusted certificate.

So ``LiveGoToSocialPeer`` **reuses** the proven black-box REST + OAuth surface of
``GoToSocialPeer`` (``mint_token``'s three-legged browser flow, ``follow``,
``home_timeline``, ``resolve_status``, …) and overrides only the transport
(``api``/``_curl_flow`` → real DNS + system trust store, no ``--resolve`` cheat,
no per-run CA) and provisioning (the account is created in cloud-init at boot,
because a provisioned box has no ``docker compose`` to ``exec`` into; the peer log
is read over SSH — a server's own operational log is output, not code, design
D5). Its two load-bearing OAuth constraints are unchanged and hold over a real
cert: the session cookie is ``Secure`` + ``Domain``-scoped (works only through
TLS, which a public box is), and the initial *unauthenticated* ``GET
/oauth/authorize`` seeds the OAuth params into the session that the consent POST
reads.

**We never read GoToSocial's source (design D5).** Every knob below comes from
its documented environment variables (the same ones the local compose stack
sets), its documented ``admin account`` CLI, and its black-box REST API. A
failure diagnosable only by reading its code is a ``NEEDS FROM USER:`` gate.
"""
from __future__ import annotations

import os
import subprocess
import tempfile
import time
import urllib.parse

import requests

from helpers.fediverse_peer import GoToSocialPeer

HETZNER_API = "https://api.hetzner.cloud/v1"

# Pinned to the exact tag the local interop harness runs, so the live peer and
# the headless peer are the same implementation build (bumping it is a
# deliberate commit, like the local compose pin — fediverse_peer.py).
GTS_IMAGE = "superseriousbusiness/gotosocial:0.22.1"

# Docker-preinstalled Hetzner marketplace image (verified present on the project:
# `type=app` name `docker-ce`), so cloud-init only runs GoToSocial + the admin
# CLI — no `curl get.docker.com | sh` step to flake. cx23 (2 vCPU / 4 GB, x86) is
# ample for one Go binary on SQLite. Try each of these DCs in turn until one
# accepts the placement: Hetzner returns a transient `412 resource_unavailable /
# "error during placement"` when a type is momentarily out of capacity in a DC
# (seen at both fsn1 and nbg1 on different runs), so a single fixed DC flakes.
# All are NON-fsn1 to stay decoupled from the fauna public nest's fsn1 placement
# (fsn1 is pinned in live_provision.py; two cx23 in one DC contend). cx23 is
# offered at fsn1/hel1/nbg1 (cheaper cpx-types are not offered at fsn1).
PEER_IMAGE = "docker-ce"
PEER_SERVER_TYPE = "cx23"
PEER_LOCATIONS = ["nbg1", "hel1"]

# The one shared password for the provisioned peer account (per-run, ephemeral,
# public only for the ~minutes the box lives). Mirrors GoToSocialPeer.PASSWORD.
PEER_PASSWORD = GoToSocialPeer.PASSWORD
PEER_USERNAME = "peer"


def _hetzner_headers(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}


def _cloud_init(peer_fqdn: str, ssh_pubkey: str) -> str:
    """cloud-init user-data that brings GoToSocial up with its own built-in
    Let's Encrypt and creates one confirmed local account — everything the test
    then needs is reachable over public HTTPS (no SSH required for the happy
    path; the SSH key is injected purely for diagnosing a bring-up failure).

    GoToSocial serves TLS *directly* here (no nginx front, unlike the local
    compose stack), with its own built-in Let's Encrypt. It binds UNPRIVILEGED
    high ports INSIDE the container (8443 HTTPS + 8080 ACME) and Docker maps the
    public privileged ports to them (``-p 443:8443 -p 80:8080``) — so the bind
    never needs root inside the container (the image may run non-root, which
    cannot bind :80/:443 directly), while external clients still reach the
    standard 443/80 the AP ids and Let's Encrypt HTTP-01 challenge require.
    ``GTS_HOST`` + ``GTS_PROTOCOL`` (not ``GTS_PORT``) determine the ``https://``
    AP ids, so the internal port is invisible to peers. Hetzner Cloud opens every
    port by default, so the ACME challenge succeeds once the A record resolves.
    The admin CLI runs in a retry loop because it must wait for the server's
    first-boot DB migration; ``docker exec`` inherits the container env (the
    ``GTS_DB_*`` vars), the same way the local ``_compose_exec`` account path
    relies on."""
    env_lines = "\n".join([
        f"GTS_HOST={peer_fqdn}",
        "GTS_PROTOCOL=https",
        "GTS_BIND_ADDRESS=0.0.0.0",
        "GTS_PORT=8443",
        "GTS_DB_TYPE=sqlite",
        "GTS_DB_ADDRESS=/gotosocial/storage/sqlite.db",
        "GTS_STORAGE_LOCAL_BASE_PATH=/gotosocial/storage",
        "GTS_LETSENCRYPT_ENABLED=true",
        "GTS_LETSENCRYPT_PORT=8080",
        f"GTS_LETSENCRYPT_EMAIL_ADDRESS=admin@{peer_fqdn}",
        "GTS_LETSENCRYPT_CERT_DIR=/gotosocial/storage/certs",
        "GTS_ACCOUNTS_REGISTRATION_OPEN=false",
        "GTS_LOG_LEVEL=info",
    ])
    # NB: keep this valid YAML. The account-create loop writes a marker file the
    # SSH diagnostic can read; the happy path never needs it.
    return f"""#cloud-config
ssh_authorized_keys:
  - {ssh_pubkey}
write_files:
  - path: /root/gts.env
    permissions: '0600'
    content: |
{_indent(env_lines, 6)}
  - path: /root/bringup.sh
    permissions: '0755'
    content: |
      #!/bin/bash
      set -x
      # The docker-ce image starts dockerd at boot, but guard the race anyway.
      for i in $(seq 1 60); do docker info >/dev/null 2>&1 && break; sleep 2; done
      docker volume create gts_storage
      docker rm -f gts 2>/dev/null || true
      docker run -d --name gts --restart unless-stopped \\
        -p 443:8443 -p 80:8080 \\
        --env-file /root/gts.env \\
        -v gts_storage:/gotosocial/storage \\
        {GTS_IMAGE}
      for i in $(seq 1 60); do
        if docker exec gts /gotosocial/gotosocial admin account create \\
             --username {PEER_USERNAME} --email {PEER_USERNAME}@{peer_fqdn} \\
             --password '{PEER_PASSWORD}' >>/root/acct.log 2>&1; then
          docker exec gts /gotosocial/gotosocial admin account confirm \\
             --username {PEER_USERNAME} >>/root/acct.log 2>&1
          docker exec gts /gotosocial/gotosocial admin account promote \\
             --username {PEER_USERNAME} >>/root/acct.log 2>&1 || true
          echo ok > /root/gts-account-ready
          break
        fi
        sleep 5
      done
runcmd:
  - bash /root/bringup.sh > /root/bringup.log 2>&1
"""


def _indent(text: str, spaces: int) -> str:
    pad = " " * spaces
    return "\n".join(pad + line for line in text.splitlines())


def provision_gotosocial_peer(token: str, zone: dict, runid: str,
                              *, scratch_dir: str) -> "LiveGoToSocialPeer":
    """Create VPS #2 (the GoToSocial peer) + its public A record, wait for the
    real Let's Encrypt cert to serve, and return a driven peer.

    Teardown is deliberately NOT this function's job: the box is named
    ``e2e-<runid>-peer-…`` and the A record ``e2e-<runid>-peer``, both carrying
    the ``e2e-<runid>`` anchor the ``box`` fixture's teardown already sweeps
    (server by name anchor + ``fauna-e2e=1`` label; RRset by name anchor) — so
    naming discipline makes cleanup free (verified: tests/live/conftest.py
    ``_this_run_servers`` / ``_teardown`` DNS sweep). The ephemeral SSH key lives
    only in ``scratch_dir`` (no Hetzner ssh-key *resource* to clean up)."""
    peer_fqdn = f"e2e-{runid}-peer.{zone['name']}"
    server_name = peer_fqdn.replace(".", "-")
    rrset_name = f"e2e-{runid}-peer"  # zone-relative; carries the run anchor

    # Ephemeral SSH keypair (diagnostics only — the happy path is pure HTTPS).
    key_path = os.path.join(scratch_dir, f"gts-peer-{runid}")
    subprocess.run(
        ["ssh-keygen", "-t", "ed25519", "-N", "", "-C", f"gts-e2e-{runid}", "-f", key_path],
        check=True, capture_output=True,
    )
    with open(key_path + ".pub") as f:
        ssh_pubkey = f.read().strip()

    # Create the server with docker preinstalled + our cloud-init, trying each DC
    # in turn until one accepts the placement (a 412 "error during placement" is a
    # transient per-DC capacity blip — fall through to the next DC).
    user_data = _cloud_init(peer_fqdn, ssh_pubkey)
    server = None
    last_err = None
    for loc in PEER_LOCATIONS:
        body = {
            "name": server_name,
            "server_type": PEER_SERVER_TYPE,
            "image": PEER_IMAGE,
            "location": loc,
            "user_data": user_data,
            # Same label the e2e orphan sweep selects on (conftest E2E_LABEL_*),
            # so a crashed run's peer box is reaped by the 1h age-gated sweep too.
            "labels": {"fauna-e2e": "1", "fauna-e2e-role": "gts-peer"},
            "public_net": {"enable_ipv4": True, "enable_ipv6": True},
        }
        r = requests.post(f"{HETZNER_API}/servers", headers=_hetzner_headers(token),
                          json=body, timeout=60)
        if r.status_code in (200, 201, 202):
            server = r.json()["server"]
            print(f"[gts-peer] placed at {loc}")
            break
        last_err = f"HTTP {r.status_code} {r.text[:300]}"
        placement = r.status_code == 412 or "placement" in r.text or "resource_unavailable" in r.text
        if placement:
            print(f"[gts-peer] {loc} placement unavailable ({r.status_code}); trying next DC")
            continue
        # A non-placement error (bad image/type/label) won't be fixed by another
        # DC — fail fast with the body so the parameter bug is legible.
        raise AssertionError(f"creating the GoToSocial peer VPS failed: {last_err}")
    if server is None:
        raise AssertionError(
            f"could not place the GoToSocial peer VPS in any of {PEER_LOCATIONS} "
            f"(all capacity-unavailable): {last_err}")
    server_id = server["id"]
    ipv4 = server["public_net"]["ipv4"]["ip"]
    print(f"[gts-peer] created server {server_id} ({server_name}) ip={ipv4}")

    # Publish the public A record so the ACME HTTP-01 challenge resolves. The
    # RRset name carries the run anchor, so the box fixture's DNS sweep clears it.
    _publish_a(token, zone["id"], rrset_name, ipv4)
    print(f"[gts-peer] published A {rrset_name}.{zone['name']} -> {ipv4}")

    # Return WITHOUT blocking on the cert: the caller kicks off VPS #1's
    # provisioning next, so both boxes' ACME waits overlap — the caller then
    # calls `peer.wait_until_serving()` once VPS #1 is up.
    return LiveGoToSocialPeer(domain=peer_fqdn, ipv4=ipv4, server_id=server_id,
                              ssh_key_path=key_path)


def _publish_a(token: str, zone_id, name: str, ip: str, ttl: int = 120) -> None:
    """Create an A RRset (Hetzner Cloud DNS). Mirrors the conftest
    ``HetznerApi.publish_txt`` create branch, for A records (no zone-file
    quoting)."""
    r = requests.post(
        f"{HETZNER_API}/zones/{zone_id}/rrsets",
        headers=_hetzner_headers(token),
        json={"name": name, "type": "A", "ttl": ttl, "records": [{"value": ip}]},
        timeout=30,
    )
    if r.status_code not in (200, 201, 202):
        raise AssertionError(
            f"publishing A {name} -> {ip} failed: HTTP {r.status_code} {r.text[:300]}"
        )


class LiveGoToSocialPeer(GoToSocialPeer):
    """``GoToSocialPeer`` retargeted from the local compose front to a real
    public VPS: same REST/OAuth contract, real DNS + system trust store.

    Only the transport and provisioning differ (the class's whole reason to
    exist per its base docstring): ``api``/``_curl_flow`` drop the
    ``--resolve …:127.0.0.1`` cheat and the per-run ``--cacert`` (a real
    Let's Encrypt cert validates against the system store; we still pin
    ``--resolve <domain>:443:<ip>`` to the box's own IP so the test does not
    wait on public-DNS propagation for its *own* calls), the account is created
    in cloud-init (so ``new_user`` only mints a token), and ``peer_log`` reads
    the container log over SSH.
    """

    name = "gotosocial-live"

    def __init__(self, *, domain: str, ipv4: str, server_id, ssh_key_path: str):
        # Deliberately bypass FediversePeer.__init__: there is no compose
        # project / per-run CA / nest handle here. Set only what the reused
        # methods touch.
        self.domain = domain
        self.ipv4 = ipv4
        self.server_id = server_id
        self.ssh_key_path = ssh_key_path
        self.nginx_port = 443           # real HTTPS port
        self.ca_path = None             # system trust store
        self.authorized_fetch = True    # GoToSocial is unconditionally strict
        self.project = None
        self.compose_dir = None
        self._env = None
        self.nest = None

    # ── transport overrides (real DNS + system trust) ──────────────────────

    def _resolve_arg(self) -> list[str]:
        return ["--resolve", f"{self.domain}:443:{self.ipv4}"]

    def api(self, method, path, *, token=None, params=None, json_body=None,
            form=None, timeout=30):
        import json as _json

        url = f"https://{self.domain}{path}"
        if params:
            url += "?" + urllib.parse.urlencode(params, doseq=True)
        cmd = [
            "curl", "-s", "-w", "\n%{http_code}", "--max-time", str(timeout),
            *self._resolve_arg(), "-X", method, url,
        ]
        if token:
            cmd += ["-H", f"Authorization: Bearer {token}"]
        if json_body is not None:
            cmd += ["-H", "Content-Type: application/json", "-d", _json.dumps(json_body)]
        for key, value in (form or {}).items():
            cmd += ["--data-urlencode", f"{key}={value}"]
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout + 10)
        body, _, code = out.stdout.rpartition("\n")
        status = int(code) if code.strip().isdigit() else 0
        data = None
        if body.strip():
            try:
                data = _json.loads(body)
            except ValueError:
                data = body
        return status, data

    def _curl_flow(self, cookie_jar, args, timeout=GoToSocialPeer._AUTH_TIMEOUT_S):
        cmd = [
            "curl", "-s", "-o", "-", "-w", "\n%{http_code}\n%{redirect_url}",
            "--max-time", str(timeout),
            *self._resolve_arg(),
            "-b", cookie_jar, "-c", cookie_jar,
            *args,
        ]
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout + 10)
        parts = out.stdout.rsplit("\n", 2)
        if len(parts) != 3:
            return 0, "", out.stdout
        body, code, redirect = parts
        return (int(code) if code.strip().isdigit() else 0), redirect.strip(), body

    def mint_token(self, username, scopes="read write follow"):
        # The base flow builds `base` from `self.nginx_port`; here HTTPS is on the
        # implicit 443, so re-implement the base URL as `https://{domain}` (no
        # :port) while keeping every leg + assertion identical.
        client_id, client_secret = self._register_app(scopes)
        redirect_uri = "urn:ietf:wg:oauth:2.0:oob"
        base = f"https://{self.domain}"
        authorize_qs = urllib.parse.urlencode({
            "response_type": "code",
            "client_id": client_id,
            "redirect_uri": redirect_uri,
            "scope": scopes,
        })
        with tempfile.NamedTemporaryFile(suffix=".cookies") as jar_file:
            jar = jar_file.name
            status, redirect, body = self._curl_flow(
                jar, [f"{base}/oauth/authorize?{authorize_qs}"])
            assert status in (302, 303), (
                f"unauthenticated authorize did not redirect to sign-in: "
                f"HTTP {status} redirect={redirect!r}\n{body[:500]}")
            status, _redirect, body = self._curl_flow(jar, [
                "-X", "POST", f"{base}/auth/sign_in",
                "--data-urlencode", f"username={username}@{self.domain}",
                "--data-urlencode", f"password={self.PASSWORD}"])
            assert status in (302, 303), (
                f"sign-in for {username} failed: HTTP {status}\n{body[:500]}")
            status, redirect, body = self._curl_flow(jar, [f"{base}/oauth/authorize"])
            if "code=" not in (redirect or ""):
                status, redirect, body = self._curl_flow(
                    jar, ["-X", "POST", f"{base}/oauth/authorize"])
            assert "code=" in (redirect or ""), (
                f"no authorization code for {username}: HTTP {status} "
                f"redirect={redirect!r}\n{body[:500]}")
            code = urllib.parse.parse_qs(
                urllib.parse.urlparse(redirect).query)["code"][0]
        status, data = self.api("POST", "/oauth/token", json_body={
            "grant_type": "authorization_code",
            "code": code,
            "client_id": client_id,
            "client_secret": client_secret,
            "redirect_uri": redirect_uri,
            "scope": scopes,
        })
        assert status == 200 and isinstance(data, dict) and data.get("access_token"), (
            f"code exchange for {username} failed: HTTP {status} {data}")
        return data["access_token"]

    def new_user(self, username=PEER_USERNAME):
        """The account is created in cloud-init at boot; here we only mint a
        token (retrying, since first-boot account creation may lag the cert)."""
        deadline = time.monotonic() + 240.0
        last = None
        while time.monotonic() < deadline:
            try:
                return username, self.mint_token(username)
            except AssertionError as e:
                last = e
                time.sleep(8.0)
        raise AssertionError(
            f"could not mint a token for the pre-provisioned peer account "
            f"{username!r} within 240s — cloud-init account creation may have "
            f"failed. Last: {last}\n--- bringup log ---\n{self.diag()}")

    # ── liveness + diagnostics ─────────────────────────────────────────────

    def wait_until_serving(self, timeout: float = 900.0) -> None:
        """Poll the peer's own ``/nodeinfo/2.0`` over real HTTPS until it serves
        with a publicly-trusted cert — i.e. GoToSocial booted AND its Let's
        Encrypt cert issued. First cert issuance dominates (ACME + DNS), so the
        budget is generous; every failure carries the last curl error so a wedge
        self-diagnoses without SSH."""
        deadline = time.monotonic() + timeout
        last = "<never probed>"
        # The RFC-standard discovery endpoint every fediverse server serves
        # publicly; a 200 over the system trust store proves BOTH that GoToSocial
        # booted AND that its Let's Encrypt cert issued (a self-signed floor cert
        # fails the system-CA validation with a cert error, not a 200).
        url = f"https://{self.domain}/.well-known/nodeinfo"
        while time.monotonic() < deadline:
            out = subprocess.run(
                ["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                 "--max-time", "20", *self._resolve_arg(), url],
                capture_output=True, text=True, timeout=30)
            last = out.stdout.strip() or (out.stderr.strip()[-200:] if out.stderr else "")
            if last == "200":
                print(f"[gts-peer] serving with a trusted cert at {self.domain}")
                return
            time.sleep(10.0)
        raise AssertionError(
            f"GoToSocial peer {self.domain} never served a trusted /nodeinfo/2.0 "
            f"within {timeout:.0f}s (last: {last!r}).\n--- bringup diag ---\n{self.diag()}")

    def _ssh(self, argv: list[str], timeout: float = 30) -> str:
        out = subprocess.run(
            ["ssh", "-i", self.ssh_key_path,
             "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
             "-o", "ConnectTimeout=10", "-o", "BatchMode=yes",
             f"root@{self.ipv4}", *argv],
            capture_output=True, text=True, timeout=timeout)
        return (out.stdout or "") + (out.stderr or "")

    def diag(self, *, tail: int = 120) -> str:
        """Best-effort bring-up diagnostics over SSH — cloud-init + account log +
        docker log tail. A server's own operational output is not its source
        (design D5)."""
        try:
            return self._ssh([
                "sh", "-c",
                "echo '== bringup.log =='; tail -n 60 /root/bringup.log 2>&1; "
                "echo '== acct.log =='; tail -n 20 /root/acct.log 2>&1; "
                "echo '== account-ready =='; cat /root/gts-account-ready 2>&1; "
                f"echo '== gts docker log =='; docker logs --tail {tail} gts 2>&1"],
                timeout=45)
        except Exception as e:  # noqa: BLE001 — diagnostics must never mask the real failure
            return f"(ssh diag failed: {e!r})"

    def peer_log(self, *, tail=400, timeout=60):
        """The peer's own server log (the outbound-direction witness), read over
        SSH instead of ``docker compose logs``. Reading an operational log is not
        reading peer source (design D5)."""
        try:
            return self._ssh(["docker", "logs", "--tail", str(tail), "gts"],
                             timeout=timeout)
        except Exception as e:  # noqa: BLE001
            return f"(peer_log ssh failed: {e!r})"

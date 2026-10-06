"""Third-party fediverse servers, driven black-box, as one interchangeable contract.

The interop harness federates our nest against a *real* implementation, because
our own in-test fediverse server (`tests/api/test_activitypub_federation.py`)
accepts our output by construction and so cannot prove a mainstream peer would
(`docs/goal/behavior/activitypub.md` § Implementation status today, gap 4).

**Why a contract and not two suites.** One peer proves only that *that* peer is
happy. The conformance value comes from running the *same* assertions against a
second, stricter implementation — so the F1-F9 flows live once and take a `peer`
fixture, and everything implementation-specific is confined to a subclass here.
A second copy of the assertions would drift, and a drifted assertion that passes
proves nothing (priorities #1/#3: same concepts, no per-implementation shape).

**The contract is the Mastodon client REST API**, which both peers speak — that
is GoToSocial's stated compatibility target, and it is why `api()` and everything
built on it is shared rather than abstract. Only two things genuinely differ, and
those are the subclass's whole job:

  * **provisioning** — how you conjure an account + an OAuth token (`new_user`);
  * **actor URI shape** — what AP id a local username has (`actor_uri`).

**We never read peer source (design D5).** Every method here drives a documented
public surface: the REST API, or the server's own admin CLI. Third-party source
is untrusted input, and reading it to debug a test is how a reviewer gets
prompt-injected — so a failure that could only be diagnosed by reading the peer's
code is a question for a human, never a licence to open it.
"""

from __future__ import annotations

import json
import subprocess
import urllib.parse


class FediversePeer:
    """A running third-party fediverse server + the host nest it federates with.

    Subclasses supply bring-up-specific provisioning; everything here is the
    shared Mastodon-client-REST surface, spoken over the harness's real TLS
    front so no test ever takes a loopback shortcut past nginx.
    """

    #: Short identifier used in fixture ids, project names and failure messages.
    name = "fediverse"

    def __init__(self, *, project, compose_dir, compose_env, nginx_port, ca_path,
                 nest, domain, authorized_fetch):
        self.project = project
        self.compose_dir = compose_dir
        self._env = compose_env
        self.nginx_port = nginx_port
        self.ca_path = ca_path
        self.nest = nest  # the start_ap_nest handle (proc, port, url, domain, admin, …)
        #: The peer's own vhost — the domain its local actors' AP ids live under.
        self.domain = domain
        #: True when this peer refuses UNSIGNED fetches of its own objects, so
        #: our outbound actor GET must carry an HTTP Signature (the instance
        #: actor). Configurable on Mastodon (`AUTHORIZED_FETCH`), unconditional
        #: on GoToSocial — see `GoToSocialPeer`.
        self.authorized_fetch = authorized_fetch

    # ── the shared REST surface (black-box, over the real TLS front) ──

    def api(self, method, path, *, token=None, params=None, json_body=None,
            form=None, timeout=30):
        """Call the peer's REST API through the nginx TLS front; return (status, data).

        Connects to the loopback-published port but presents SNI/Host as the
        peer's real domain and verifies against the per-run CA — the same path a
        remote server would take, minus public DNS.
        """
        url = f"https://{self.domain}:{self.nginx_port}{path}"
        if params:
            url += "?" + urllib.parse.urlencode(params, doseq=True)
        cmd = [
            "curl", "-s", "-w", "\n%{http_code}", "--max-time", str(timeout),
            "--resolve", f"{self.domain}:{self.nginx_port}:127.0.0.1",
            "--cacert", self.ca_path, "-X", method, url,
        ]
        if token:
            cmd += ["-H", f"Authorization: Bearer {token}"]
        if json_body is not None:
            cmd += ["-H", "Content-Type: application/json", "-d", json.dumps(json_body)]
        for key, value in (form or {}).items():
            cmd += ["--data-urlencode", f"{key}={value}"]
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout + 10)
        body, _, code = out.stdout.rpartition("\n")
        status = int(code) if code.strip().isdigit() else 0
        data = None
        if body.strip():
            try:
                data = json.loads(body)
            except json.JSONDecodeError:
                data = body
        return status, data

    def fetch_nest_url(self, url, *, timeout=30):
        """GET a nest URL over the SAME path a remote peer would use.

        Through the nginx front (SNI + `Host: nest.test`, per-run CA), not the
        nest's loopback address — so a route that works only when dialed
        directly, or a document whose links name a host the front does not
        serve, fails here rather than in production. Returns parsed JSON.
        """
        nest_domain = self.nest["domain"]
        cmd = [
            "curl", "-s", "-w", "\n%{http_code}", "--max-time", str(timeout),
            "--resolve", f"{nest_domain}:{self.nginx_port}:127.0.0.1",
            "--cacert", self.ca_path,
            "-H", "Accept: application/activity+json",
            url.replace(f"https://{nest_domain}/",
                        f"https://{nest_domain}:{self.nginx_port}/"),
        ]
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout + 10)
        body, _, code = out.stdout.rpartition("\n")
        assert code.strip() == "200", f"GET {url} → HTTP {code.strip()}: {body[:500]}"
        return json.loads(body)

    def search(self, query, token, *, resolve=True, kind=None):
        """`GET /api/v2/search` — the discovery + remote-fetch entry point."""
        params = {"q": query, "resolve": "true" if resolve else "false"}
        if kind:
            params["type"] = kind
        status, data = self.api("GET", "/api/v2/search", token=token, params=params)
        assert status == 200, f"search {query!r} failed: HTTP {status} {data}"
        return data

    def resolve_account(self, acct, token):
        """Resolve `@user@nest.test` to a peer-local account record, or None."""
        data = self.search(acct, token, kind="accounts")
        for account in data.get("accounts", []):
            if account.get("acct", "").lower() == acct.lstrip("@").lower():
                return account
        return data.get("accounts", [None])[0] if data.get("accounts") else None

    def resolve_status(self, url, token):
        """Dereference a remote status URL into a peer-local status, or None."""
        data = self.search(url, token, kind="statuses")
        statuses = data.get("statuses", [])
        return statuses[0] if statuses else None

    def account_locked(self, token):
        """Does this peer-local account require manual approval of followers?

        `manuallyApprovesFollowers` in AS2; `locked` in the client REST API. It
        decides whether an inbound Follow is auto-accepted or parked as a
        request, so any flow that needs *us* to end up followed depends on it —
        and it is a per-implementation provisioning default, not a protocol
        constant, which is exactly the kind of difference a second peer exposes.
        """
        status, data = self.api("GET", "/api/v1/accounts/verify_credentials", token=token)
        assert status == 200, f"verify_credentials failed: HTTP {status} {data}"
        return bool(data.get("locked"))

    def set_account_locked(self, token, locked):
        """Set this account's manual-follower-approval flag; return the new value."""
        status, data = self.api(
            "PATCH", "/api/v1/accounts/update_credentials",
            token=token, form={"locked": "true" if locked else "false"},
        )
        assert status == 200, f"update_credentials failed: HTTP {status} {data}"
        return bool(data.get("locked"))

    def follow(self, account_id, token):
        status, data = self.api("POST", f"/api/v1/accounts/{account_id}/follow", token=token)
        assert status == 200, f"follow {account_id} failed: HTTP {status} {data}"
        return data

    def relationship(self, account_id, token):
        status, data = self.api(
            "GET", "/api/v1/accounts/relationships",
            token=token, params={"id[]": account_id},
        )
        assert status == 200, f"relationships failed: HTTP {status} {data}"
        return data[0] if data else {}

    def home_timeline(self, token, limit=40):
        status, data = self.api(
            "GET", "/api/v1/timelines/home", token=token, params={"limit": limit}
        )
        assert status == 200, f"home timeline failed: HTTP {status} {data}"
        return data

    def post_status(self, text, token, *, in_reply_to_id=None):
        body = {"status": text, "visibility": "public"}
        if in_reply_to_id:
            body["in_reply_to_id"] = in_reply_to_id
        status, data = self.api("POST", "/api/v1/statuses", token=token, json_body=body)
        assert status == 200, f"post_status failed: HTTP {status} {data}"
        return data

    def status_context(self, status_id, token):
        """`GET /api/v1/statuses/{id}/context` — the thread around a status
        (`ancestors` + `descendants`), as the peer's own UI would render it."""
        status, data = self.api("GET", f"/api/v1/statuses/{status_id}/context", token=token)
        assert status == 200, f"context {status_id} failed: HTTP {status} {data}"
        return data

    def favourite(self, status_id, token):
        status, data = self.api("POST", f"/api/v1/statuses/{status_id}/favourite", token=token)
        assert status == 200, f"favourite failed: HTTP {status} {data}"
        return data

    def unfavourite(self, status_id, token):
        status, data = self.api("POST", f"/api/v1/statuses/{status_id}/unfavourite", token=token)
        assert status == 200, f"unfavourite failed: HTTP {status} {data}"
        return data

    def reblog(self, status_id, token):
        status, data = self.api("POST", f"/api/v1/statuses/{status_id}/reblog", token=token)
        assert status == 200, f"reblog failed: HTTP {status} {data}"
        return data

    def unreblog(self, status_id, token):
        status, data = self.api("POST", f"/api/v1/statuses/{status_id}/unreblog", token=token)
        assert status == 200, f"unreblog failed: HTTP {status} {data}"
        return data

    # ── per-implementation surface ──

    def actor_uri(self, username):
        """The AP id of one of the peer's own local accounts.

        Needed by the flow that makes *us* follow *them*: the outbound Follow
        names this URI, so a wrong shape fails as "the follow was never
        accepted" rather than as a 404 — hence it is peer-declared, not guessed.
        """
        raise NotImplementedError

    def new_user(self, username):
        """Create a local account + return (username, OAuth access token)."""
        raise NotImplementedError

    #: The compose service whose stdout is the peer's own server log — the
    #: witness for anything it rejected. Subclasses name their own.
    log_service = None

    def peer_log(self, *, tail=400, timeout=60):
        """The peer's own server log — the mirror of `ap_nest.nest_log`.

        **Why this is part of the contract.** An activity we send is accepted or
        refused *inside the peer*, and its answer to us is a bare status code; an
        activity it declines to send us produces no observation on our side at
        all. So "our Follow was never delivered" and "it arrived and the peer
        rejected it" are indistinguishable from anything the harness can see —
        exactly the asymmetry that makes our own nest log a first-class test
        surface for the inbound direction (`docs/goal/behavior/activitypub.md`
        § Implementation status today, gap 4). This is that surface for the
        outbound direction, and reading it is emphatically *not* reading peer
        source (design D5): a server's operational log is output, not code.
        """
        r = subprocess.run(
            ["docker", "compose", "-p", self.project, "logs", "--no-color",
             "--tail", str(tail), self.log_service or ""],
            cwd=str(self.compose_dir), env=self._env,
            capture_output=True, text=True, timeout=timeout,
        )
        return (r.stdout or "") + (r.stderr or "")

    def _compose_exec(self, service, argv, timeout=180):
        """Run a command inside one of this peer's compose services."""
        return subprocess.run(
            ["docker", "compose", "-p", self.project, "exec", "-T", service, *argv],
            cwd=str(self.compose_dir), env=self._env,
            capture_output=True, text=True, timeout=timeout,
        )


class MastodonPeer(FediversePeer):
    """Mastodon — the mainstream, lenient peer (pinned v4.3.15).

    Provisioning goes through `tootctl` / `rails runner`, whose Doorkeeper model
    names shift between majors, so both are pinned to the compose stack's
    version rather than written defensively.
    """

    name = "mastodon"
    log_service = "web"

    def actor_uri(self, username):
        return f"https://{self.domain}/users/{username}"

    def create_account(self, username, email):
        """Create a confirmed, approved local Mastodon account via tootctl."""
        r = self._compose_exec(
            "web",
            ["bin/tootctl", "accounts", "create", username,
             "--email", email, "--confirmed", "--approve"],
        )
        if r.returncode != 0:
            raise RuntimeError(f"tootctl accounts create {username} failed:\n{r.stderr}\n{r.stdout}")
        return username

    def mint_token(self, email, scopes="read write follow"):
        """Mint an OAuth access token for `email` via Doorkeeper (pinned to 4.3)."""
        ruby = (
            "app = Doorkeeper::Application.find_or_create_by!(name: 'fauna-e2e') do |a|\n"
            "  a.redirect_uri = 'urn:ietf:wg:oauth:2.0:oob'\n"
            f"  a.scopes = '{scopes}'\n"
            "end\n"
            f"u = User.find_by!(email: '{email}')\n"
            "t = Doorkeeper::AccessToken.create!(application: app, resource_owner_id: u.id, "
            f"scopes: '{scopes}')\n"
            "puts t.token\n"
        )
        r = self._compose_exec("web", ["bin/rails", "runner", ruby])
        if r.returncode != 0:
            raise RuntimeError(f"token mint for {email} failed:\n{r.stderr}\n{r.stdout}")
        lines = [ln.strip() for ln in r.stdout.splitlines() if ln.strip()]
        if not lines:
            raise RuntimeError(f"token mint for {email} produced no token:\n{r.stdout}\n{r.stderr}")
        return lines[-1]

    def new_user(self, username):
        email = f"{username}@{self.domain}"
        self.create_account(username, email)
        return username, self.mint_token(email)


class GoToSocialPeer(FediversePeer):
    """GoToSocial — the strict, single-binary peer (pinned 0.22.1).

    **Why it earns its place beside Mastodon.** Mastodon accepted our JSON-LD,
    addressing and signatures from the first run; a peer that agrees with you is
    a weak oracle. GoToSocial is a different implementation in a different
    language, and — the part that matters — it **requires HTTP-signed inbound
    fetches unconditionally**. Mastodon's `AUTHORIZED_FETCH` is a deployment
    choice most instances leave off; GoToSocial 0.22.1 exposes no equivalent
    knob at all (`--instance-federation-mode` is domain allow/blocklisting, a
    different axis). So `authorized_fetch` is hard-wired True here, and the
    secure-mode assertions apply to an ordinary GoToSocial run — our instance
    actor is load-bearing from the very first activity it receives.

    **Provisioning is the real difference.** There is no `tootctl`; accounts come
    from the server's own `admin account` CLI, run inside the container against
    the same SQLite file the server holds (WAL + a 30-minute busy timeout make
    that safe). Tokens are worse: 0.22.1 supports only the `authorization_code`
    and `client_credentials` grants — the password grant Mastodon-era tooling
    reaches for is gone — so `new_user` drives the actual browser sign-in flow
    with a cookie jar. That flow only works over the TLS front (the session
    cookie is `Secure` and scoped `Domain=<peer domain>`), which is what we use
    everywhere anyway.
    """

    name = "gotosocial"
    log_service = "gts"

    #: Every provisioned account shares one password; the peer is per-run,
    #: ephemeral, and reachable only on a loopback-published port.
    PASSWORD = "Fauna-e2e-Testpass1!"

    #: The GoToSocial session cookie is short-lived (Max-Age=120), so sign-in →
    #: authorize → token must not dawdle. Bounded like every other wait here.
    _AUTH_TIMEOUT_S = 30

    def actor_uri(self, username):
        return f"https://{self.domain}/users/{username}"

    def create_account(self, username, email):
        """Create + confirm a local account through the server's admin CLI."""
        create = self._compose_exec(
            "gts",
            ["/gotosocial/gotosocial", "admin", "account", "create",
             "--username", username, "--email", email, "--password", self.PASSWORD],
        )
        if create.returncode != 0:
            raise RuntimeError(
                f"gotosocial admin account create {username} failed:\n"
                f"{create.stderr}\n{create.stdout}"
            )
        confirm = self._compose_exec(
            "gts",
            ["/gotosocial/gotosocial", "admin", "account", "confirm", "--username", username],
        )
        if confirm.returncode != 0:
            raise RuntimeError(
                f"gotosocial admin account confirm {username} failed:\n"
                f"{confirm.stderr}\n{confirm.stdout}"
            )
        return username

    def _register_app(self, scopes):
        status, data = self.api(
            "POST", "/api/v1/apps",
            json_body={
                "client_name": "fauna-e2e",
                "redirect_uris": "urn:ietf:wg:oauth:2.0:oob",
                "scopes": scopes,
            },
        )
        assert status == 200 and isinstance(data, dict) and data.get("client_id"), (
            f"registering the OAuth app failed: HTTP {status} {data}"
        )
        return data["client_id"], data["client_secret"]

    def _curl_flow(self, cookie_jar, args, timeout=_AUTH_TIMEOUT_S):
        """One step of the sign-in/authorize flow, carrying the session cookie.

        Returns (status, redirect_url, body). Never follows redirects: the
        authorization code arrives *in* a redirect target, so following it would
        throw away the one value we need.
        """
        cmd = [
            "curl", "-s", "-o", "-", "-w", "\n%{http_code}\n%{redirect_url}",
            "--max-time", str(timeout),
            "--resolve", f"{self.domain}:{self.nginx_port}:127.0.0.1",
            "--cacert", self.ca_path,
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
        """Drive the authorization_code flow end to end and return the token.

        0.22.1 removed the password grant, so this is the browser flow: sign in
        for a session cookie, ask for an authorization code, exchange it. Each
        step asserts its own failure so a break names the leg that broke rather
        than surfacing as an empty token three calls later.
        """
        import tempfile

        client_id, client_secret = self._register_app(scopes)
        redirect_uri = "urn:ietf:wg:oauth:2.0:oob"
        base = f"https://{self.domain}:{self.nginx_port}"
        authorize_qs = urllib.parse.urlencode({
            "response_type": "code",
            "client_id": client_id,
            "redirect_uri": redirect_uri,
            "scope": scopes,
        })

        with tempfile.NamedTemporaryFile(suffix=".cookies") as jar_file:
            jar = jar_file.name

            # 1. Ask for a code while signed OUT. This leg looks pointless and is
            #    not: the redirect to the sign-in page is also where GoToSocial
            #    stashes `response_type`/`client_id`/`scope` **in the session**.
            #    The consent POST in step 4 reads them from there, not from its
            #    own form, so skipping straight to sign-in yields a 500,
            #    "key response_type not found in session".
            status, redirect, body = self._curl_flow(
                jar, [f"{base}/oauth/authorize?{authorize_qs}"]
            )
            assert status in (302, 303), (
                f"GoToSocial did not redirect an unauthenticated authorize "
                f"request for {username} to sign-in: HTTP {status}, "
                f"redirect={redirect!r}. That redirect is what seeds the OAuth "
                f"params into the session.\n{body[:500]}"
            )

            # 2. Sign in — establishes the `Secure`, domain-scoped session cookie
            #    on the session that now carries the OAuth params.
            status, _redirect, body = self._curl_flow(jar, [
                "-X", "POST", f"{base}/auth/sign_in",
                "--data-urlencode", f"username={username}@{self.domain}",
                "--data-urlencode", f"password={self.PASSWORD}",
            ])
            assert status in (302, 303), (
                f"GoToSocial sign-in for {username} failed: HTTP {status}. The "
                f"account exists (admin CLI succeeded), so this is the sign-in "
                f"leg itself.\n{body[:500]}"
            )

            # 3. Re-enter the authorize flow, now authenticated. This either
            #    redirects straight to the redirect_uri with a code, or renders
            #    the consent page — handle both rather than pinning one UX.
            status, redirect, body = self._curl_flow(jar, [f"{base}/oauth/authorize"])
            if "code=" not in (redirect or ""):
                # 4. Consent. Deliberately carries NO parameters: the handler
                #    takes them from the session seeded in step 1.
                status, redirect, body = self._curl_flow(
                    jar, ["-X", "POST", f"{base}/oauth/authorize"]
                )
            assert "code=" in (redirect or ""), (
                f"GoToSocial never issued an authorization code for {username}: "
                f"HTTP {status}, redirect={redirect!r}. The session cookie is "
                f"`Secure` and scoped to the peer domain, so this leg only works "
                f"through the TLS front.\n{body[:500]}"
            )
            code = urllib.parse.parse_qs(
                urllib.parse.urlparse(redirect).query
            )["code"][0]

        # 3. Exchange the code for a bearer token.
        status, data = self.api("POST", "/oauth/token", json_body={
            "grant_type": "authorization_code",
            "code": code,
            "client_id": client_id,
            "client_secret": client_secret,
            "redirect_uri": redirect_uri,
            "scope": scopes,
        })
        assert status == 200 and isinstance(data, dict) and data.get("access_token"), (
            f"exchanging the authorization code for {username} failed: "
            f"HTTP {status} {data}"
        )
        return data["access_token"]

    def new_user(self, username):
        self.create_account(username, f"{username}@{self.domain}")
        token = self.mint_token(username)
        # GoToSocial provisions accounts that MANUALLY APPROVE followers, where
        # Mastodon provisions them open. That is a provisioning default, not a
        # protocol difference — but it silently changes what a shared assertion
        # means: an inbound Follow becomes a parked *request*, the peer sends no
        # `Accept`, and `followed_by` never flips, which reads exactly like our
        # outbound Follow having been rejected. (Observed 2026-07-22: the peer
        # logged our Follow `202 Accepted` + "processing from fedi API" and then
        # sent nothing back.) Normalising it here is the subclass's job — the
        # F1-F9 flows must mean the same thing against either peer.
        was_locked = self.account_locked(token)
        if was_locked:
            still_locked = self.set_account_locked(token, False)
            assert not still_locked, (
                f"{username} still requires manual follower approval after "
                f"update_credentials(locked=false) — every flow that needs this "
                f"account to accept a follow would park as a pending request"
            )
        return username, token

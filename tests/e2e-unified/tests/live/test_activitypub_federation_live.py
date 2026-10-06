"""tier_4 (live-remote, OPT-IN): the ActivityPub **private → public → fediverse**
chain across THREE parties on REAL infrastructure, ending at a genuinely public
GoToSocial instance with a real Let's Encrypt certificate.

Why this test exists — the coverage nothing else gives (verified recon
2026-07-23; docs/goal/behavior/activitypub.md § Implementation status today gap
4, docs/goal/architecture/nest/private-mode.md § Post Forwarding step 6):

  * The production nest image compiles OUT the DNS/CA test hooks
    (`Dockerfile` builds only `bluesky,nostr,activitypub`; the resolve/CA
    overrides are `#[cfg(feature = "test-hooks")]`), so a deployed nest can only
    federate with a peer that resolves in real DNS and presents a
    publicly-trusted cert. The local docker Mastodon/GoToSocial interop harness
    (which cheats the resolver + trust store) therefore cannot exercise a real
    deployment at all — hence a real public peer VPS.
  * The `private → public → fediverse` chain (`private-mode.md:117` step 6: a
    forwarded post, iff newly stored, runs the same create-side bridge fan-out —
    an AP `Create`-push — self-gated on the author's AP enablement on the public
    nest) is today pinned ONLY against our own in-test fediverse server, which
    accepts our output by construction. This runs it against a real, stricter
    implementation (GoToSocial requires HTTP-signed inbound fetches
    unconditionally — our instance actor is load-bearing from the first activity).

✅ THIS TEST ALREADY PAID FOR ITSELF (2026-07-23). Its first runs FAILED at
WebFinger, and that failure was a REAL nest bug it caught, not a test defect:
the ActivityPub domain was snapshotted ONCE at boot from `node.domain`, which is
`None` on a domainless-booted (provisioned) box, and never followed the
claim-registered `identity_domain` cache the way mail + the TLS apex do
(`domains-and-tls-bootstrap.md` § claim-sets-identity mandates "no split-brain
where a domainless box thinks it is localhost while mail knows the real
domain"). On any provisioned+claimed box that left WebFinger answering
"ActivityPub not configured" and every other AP route serving `localhost` URLs,
until a restart baked the domain into nest.toml — which is why example.com
(long-restarted) worked and a fresh box did not. **FIXED 2026-07-23**: every AP
surface now resolves the domain per request via `AppState::handle_domain()`, and
the same class of bug was fixed in nostr, video and federated Welcome delivery.
Headless pins: `actor_routes::tests::ap_surfaces_follow_the_claimed_domain_without_a_restart`
(the domainless-boot-then-claim flow) and, for the class,
`state::tests::no_runtime_handler_reads_a_domain_boot_seed`.

✅ RE-VALIDATED GREEN against a fixed image 2026-07-23 (450 s, end to end): the
box self-reported the fixed build from its own health endpoint, WebFinger
resolved on the claimed FQDN, the `rel=self` actor came back on the claimed
domain rather than `localhost`, a real strict GoToSocial peer resolved *and*
followed it (so the instance actor minted and signed our fetch), and the
`private->public->fediverse` `Create` landed in that peer's timeline with a note
id on the claimed domain. `activitypub.md` § Implementation status today (gap 4)
records the close-out. NB the default `ghcr.io/faunasocial/nest:latest` is
normally the right image: `FAUNA_E2E_IMAGE_TAG` exists to pin an OLD build (the
security review's live-negative), not to opt in to a current one — and leaving
it unset keeps the PRIVATE nest below, which hard-codes `:latest`, on the same
build as the public one, so a run carries no private/public image skew.

Topology (2 paid Hetzner VPSes + one local Docker container):

  * PRIVATE nest — local Docker, production `ghcr.io/faunasocial/nest:latest`,
    `FAUNA_MODE=private`; genuinely NAT'd (it dials OUT). Where it forwards is
    the user's own pairing row on it, not an env var: one LinkBoth from its
    Nests page records the public nest's URL with `post_forward` (the default
    self-sync set), after which a single post-create is the whole forward
    trigger (the public nest's default `open` submission policy accepts any
    validly-authored forward; federation_handlers.rs post_forward_handler).
  * PUBLIC nest — VPS #1, provisioned through the real client onboarding path
    (the `box` fixture + `helpers/live_provision`), claimed on the public NAT
    axis, ActivityPub enabled for the admin actor through the Bridges UI.
  * GoToSocial peer — VPS #2 (the only genuinely new infra), a real public box
    with its own Let's Encrypt cert, provisioned by `helpers/live_gotosocial`.

Chain proven end to end:

  1. GoToSocial (real strict peer) discovers + follows the fauna public actor —
     exercises the outbound Follow, fauna's *signed* fetch of a strict peer's
     actor (the instance actor + User-Agent), and the returned Accept.
  2. A post authored on the PRIVATE nest through the client UI auto-forwards to
     the PUBLIC nest, which AP-pushes a `Create` that lands in the real
     GoToSocial follower's home timeline.

Gating (all pre-existing conventions):
  * `HETZNER_API_TOKEN` + `FAUNA_E2E_LIVE=1` — the live-provision gate
    (`tests/live/conftest.py`; real € cost). This test does NOT touch example.com,
    so it takes no `live_box` flock.
  * Docker (the private nest container).

Teardown: the `box` fixture tears down BOTH VPSes (VPS #2 carries the
`e2e-<runid>-peer` name/label anchor the fixture's sweep already matches) + all
DNS; this test's `finally` removes the private container. `FAUNA_E2E_KEEP=1`
leaves the boxes for inspection.

Test taxonomy: tier_4 (a real deployment artifact — the production Docker image —
under real supervision on a real box; live-remote ruling 2026-07-22).
"""
from __future__ import annotations

import json
import os
import subprocess
import time
import urllib.error
import urllib.request
import uuid
from urllib.parse import urlencode

import pytest

from nacl.signing import SigningKey  # noqa: F401 — kept for parity with sibling live tests

from helpers.live_gotosocial import PEER_USERNAME, provision_gotosocial_peer
from helpers.live_provision import (
    await_provisioning,
    drive_provisioning,
    finish_provisioned_wizard,
    retry_get,
    skip_unless_fresh_box_reach_app,
)
from tests.platform.docker.helpers import (
    find_free_ports,
    remove_container,
    start_container_with_ports,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

# Hand-added tier marker (tag-test-tiers.py duplicates list-form markers — do
# NOT run it on this file).
pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_provisioning,  # gates on HETZNER_API_TOKEN + FAUNA_E2E_LIVE
    # No client platform marker — the app axis is decided in-body by
    # `skip_unless_fresh_box_reach_app`, so an app that cannot drive this yet is
    # TALLIED as unbuilt debt (convention 7) rather than silently deselected.
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available (private nest container)"),
]

GHCR_IMAGE = "ghcr.io/faunasocial/nest:latest"
PRIVATE_CLAIM_CODE = "APFED1"
AP_ACCEPT = "application/activity+json"
# VPS create + cloud-init + ACME + boot (same headroom the provisioning test uses).
PROVISION_TIMEOUT_S = 1200


# ── production :latest for the private nest (PULL, never build) ──────────────
@pytest.fixture(scope="module")
def ghcr_latest_image():
    proc = subprocess.run(
        ["docker", "pull", GHCR_IMAGE], capture_output=True, text=True, timeout=900
    )
    if proc.returncode != 0:
        pytest.skip(f"cannot pull {GHCR_IMAGE}: {proc.stderr.strip()[-300:]}")
    return GHCR_IMAGE


# ── client-drive helpers (duplicated from test_private_relay_hetzner.py per the
# per-live-test client-drive-helper ownership precedent — see that file's
# `_settled_mail_enabled` comment; a future session that can co-verify the paid
# private-relay test should lift the shared set into helpers/live_provision.py) ─
def _relaunch(app, extra: dict | None = None) -> None:
    config = dict(app.driver._launch_config)
    config.pop("seed_credentials", None)
    if extra:
        config.update(extra)
    app.driver.teardown()
    app.driver.launch(config)


def _set_state_resilient(app, state: dict, *, attempts: int = 6) -> None:
    last: Exception | None = None
    for i in range(attempts):
        try:
            app.driver.set_state(state)
            return
        except TimeoutError as e:
            last = e
            if not app.driver.is_app_alive():
                raise
            time.sleep(2.0 * (i + 1))
    raise AssertionError(
        f"the client never acknowledged a session/nav patch across {attempts} "
        f"attempts (loaded box?); last: {last!r}"
    )


def _reassert_session(app, *, nest_url: str, secret_hex: str, view: str) -> None:
    _set_state_resilient(app, {
        "session": {"authenticated": True, "node_url": nest_url, "secret_hex": secret_hex},
        "nav": {"stack": [{"view": view}]},
    })
    time.sleep(2.0)


def _await_authenticated(app, timeout: float = 90.0) -> None:
    """Wait for a SEEDED relaunch's silent challenge to complete
    (``session.authenticated`` in the state snapshot) — a full re-auth onto a
    different nest URL, which a plain ``set_state`` session patch does NOT do."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            sess = (app.driver.get_state() or {}).get("session") or {}
            if sess.get("authenticated"):
                return
            last = sess
        except Exception as e:  # agent still booting
            last = repr(e)
        time.sleep(1.0)
    raise AssertionError(
        f"seeded client never reached an authenticated session within {timeout}s "
        f"(last session state: {last}; error: {app.error_text()!r})")


def _ui_claim(app, *, nest_url: str, claim_code: str, handle: str,
              secret_hex: str, nat_mode: str | None = None) -> None:
    """Claim a nest through the REAL client UI: paste-key import, known-nest jump
    to the claim page, type + submit the claim code, confirm nat_mode_choice."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=20)
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest", json.dumps([nest_url, handle])
    )
    app.driver.wait_for("claim-code-input", timeout=30)
    app.driver.clear_and_type("claim-code-input", claim_code)
    app.driver.click("claim-code-submit-button")
    app.driver.wait_for("nat-mode-confirm-button", timeout=60)
    app.onboarding.finish_nat_mode(mode=nat_mode)


def _wait_connected(app, timeout: float = 90.0) -> None:
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            last = app.driver.get_text("connection-status") or ""
        except Exception:
            last = ""
        if last.startswith("Connected"):
            return
        time.sleep(1.5)
    raise AssertionError(
        f"WS-RPC never reached Connected within {timeout:.0f}s (last: {last!r})")


# ── public HTTPS reads against the provisioned public nest (stdlib only) ─────
def _get(url: str, accept: str | None = None, timeout: float = 20):
    req = urllib.request.Request(url, headers={"Accept": accept} if accept else {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, (e.read().decode("utf-8", "replace") if e.fp else "")
    except urllib.error.URLError as e:
        return 0, repr(e)


def _webfinger_resolves(public_url: str, handle: str, timeout: float = 90.0) -> dict:
    wf = f"{public_url}/.well-known/webfinger?{urlencode({'resource': f'acct:{handle}'})}"
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        status, body = _get(wf)
        if status == 200:
            try:
                return json.loads(body)
            except json.JSONDecodeError:
                pass
        last = f"status={status} body={body[:200]!r}"
        time.sleep(3.0)
    raise AssertionError(f"webfinger for acct:{handle} never resolved (last: {last})")


def _self_actor_url(wf_doc: dict) -> str:
    for link in wf_doc.get("links") or []:
        if link.get("rel") == "self" and (link.get("type") or "").startswith(AP_ACCEPT):
            if link.get("href"):
                return link["href"]
    raise AssertionError(f"webfinger has no rel=self activity+json link: {wf_doc!r}")


# ── the test ─────────────────────────────────────────────────────────────────
@pytest.mark.feature("fediverse")
def test_activitypub_federation_live_three_party(app, box, ghcr_latest_image,
                                                 request, tmp_path):
    skip_unless_fresh_box_reach_app(
        app.driver, surface="the live 3-party ActivityPub federation observer drive"
    )

    token = os.environ["HETZNER_API_TOKEN"].strip()
    secret_hex = box.identity_secret_hex
    handle = box.handle                       # admin@e2e-<runid>.example.com
    ap_username = handle.split("@", 1)[0]     # "admin" — handle-verbatim
    marker = uuid.uuid4().hex

    # ── Phase 1: create VPS #2 (GoToSocial peer) FIRST so its cert issuance
    #    overlaps VPS #1's provisioning; then drive VPS #1 provisioning. ──────
    print(f"\n[apfed-live] creating GoToSocial peer VPS for run {box.runid} ...")
    peer = provision_gotosocial_peer(token, box.zone, box.runid,
                                     scratch_dir=str(tmp_path))
    print(f"[apfed-live] peer VPS created: {peer.domain} ({peer.ipv4})")

    _relaunch(app)  # provisioning drive runs on a clean onboarding machine
    print(f"[apfed-live] provisioning public nest {box.subdomain} ...")
    drive_provisioning(app, box, token)
    snap = await_provisioning(app, PROVISION_TIMEOUT_S)
    assert snap.get("overall") == "Succeeded", (
        f"public-nest provisioning did not succeed: overall={snap.get('overall')!r} "
        f"final_error={snap.get('final_error')!r} steps={snap.get('steps')!r}"
    )
    result = snap.get("result") or {}
    box.server_id = result.get("server_id")
    box.ipv4 = result.get("ipv4")
    box.fqdn = result.get("domain")
    claim_code = result.get("claim_code")
    assert box.server_id and box.ipv4 and box.fqdn and claim_code, (
        f"incomplete provisioning result: {result}")
    public_ip_url = f"https://{box.ipv4}"
    public_fqdn_url = f"https://{box.fqdn}"
    print(f"[apfed-live] public nest: server={box.server_id} ip={box.ipv4} fqdn={box.fqdn}")

    # Health by IP (boots domainless on its self-signed floor; DNS lags).
    health = retry_get(f"{public_ip_url}/api/v1/health", verify=False, timeout=15,
                       attempts=20, delay=6)
    hj = health.json()
    assert hj.get("status") == "ok", f"public nest health not ok: {hj}"
    print(f"[apfed-live] public nest health ok (build {hj.get('commit') or hj.get('version')})")

    priv_name = None
    try:
        # ── Phase 2: finish the wizard (box claimed by IP) → FQDN gate → enable AP ─
        # The claim is what registers the box's domain and triggers its ACME cert
        # acquisition for the FQDN — an UNCLAIMED box stays domainless on its
        # self-signed floor cert and never requests the FQDN cert (run6 spun the
        # FQDN gate 40min on the floor cert when it ran before the claim). So the
        # claim MUST precede the FQDN gate — and it already has: provisioning's
        # own Online step claimed the box by IP as its last substep
        # (onboarding.md § 6 *Provisioning = build + claim*), so `Succeeded`
        # above means built and claimed. Walk the wizard's tail here (Continue →
        # nat_mode_choice → LoggedIn) in the same launch; a second claim through
        # the claim-code page is refused `already_claimed` (see
        # `finish_provisioned_wizard`). The private container below is still
        # claimed through the claim-code page — it is claimed outside any run.
        finish_provisioned_wizard(app)

        # FQDN TLS gate — the real ACME cert now appears (post-claim); after this
        # the box (and the peer, and the private container) all resolve + serve on
        # the FQDN with a publicly-trusted cert.
        retry_get(f"{public_fqdn_url}/api/v1/health", verify=True, timeout=15,
                  attempts=240, delay=10)
        print("[apfed-live] public nest FQDN TLS gate OK")

        # Enable AP over the FQDN session: a full authenticated relaunch onto the
        # FQDN (a set_state URL switch does NOT re-authenticate an already-running
        # client — only a relaunch runs the real launch-routing), so the client
        # dials the box by its FQDN when it enables the bridge. (NB: the WebFinger
        # "ActivityPub not configured" this originally chased down turned out to be
        # a nest-side bug — the AP domain is read once at boot from node.domain and
        # never follows the claim-registered identity domain — not a session-URL
        # issue; see the module docstring / activitypub.md § Implementation status.)
        _relaunch(app, extra={"seed_credentials": {
            "secret_key": secret_hex, "node_url": public_fqdn_url,
            "device_id": "apfed-public"}})
        _await_authenticated(app)
        _wait_connected(app)
        app.bridges.navigate()
        app.bridges.link("activitypub")  # zero-field enable mode (link opens the row itself)
        print("[apfed-live] AP enabled on the public nest (FQDN session)")
        wf_doc = _webfinger_resolves(public_fqdn_url, handle)
        actor_url = _self_actor_url(wf_doc)
        print(f"[apfed-live] public actor resolvable: {actor_url}")

        # ── Phase 3: GoToSocial (real peer) follows the fauna public actor ──
        peer.wait_until_serving()
        _, peer_token = peer.new_user(PEER_USERNAME)
        acct = f"{ap_username}@{box.fqdn}"
        account = peer.resolve_account(acct, peer_token)
        assert account and account.get("id"), (
            f"GoToSocial could not resolve the fauna actor @{acct}: {account!r}\n"
            f"--- peer log ---\n{peer.peer_log(tail=80)}")
        peer.follow(account["id"], peer_token)
        # Fauna auto-accepts follows (ap_accounts.auto_accept_follows=1); poll the
        # relationship until the Accept round-trips back to GoToSocial.
        deadline = time.monotonic() + 120.0
        rel = {}
        while time.monotonic() < deadline:
            rel = peer.relationship(account["id"], peer_token)
            if rel.get("following"):
                break
            time.sleep(4.0)
        assert rel.get("following"), (
            f"GoToSocial never became a confirmed follower of @{acct} "
            f"(relationship={rel!r}) — the Accept did not round-trip.\n"
            f"--- peer log ---\n{peer.peer_log(tail=120)}")
        print(f"[apfed-live] GoToSocial now follows @{acct}")

        # ── Phase 4: PRIVATE nest (local docker), same identity, paired out ──
        (priv_http,) = find_free_ports(1)
        priv_name = f"fauna-apfed-private-{priv_http}"
        private_url = f"https://127.0.0.1:{priv_http}"
        start_container_with_ports(
            priv_name,
            {3000: priv_http},
            env={
                "FAUNA_MODE": "private",
                "FAUNA_CLAIM_CODE": PRIVATE_CLAIM_CODE,
                "FAUNA_PORT": "3000",
            },
            image=ghcr_latest_image,
        )
        wait_for_health(priv_http, priv_name)
        print(f"[apfed-live] private nest up: {priv_name} -> {private_url}")

        _relaunch(app)
        _ui_claim(app, nest_url=private_url, claim_code=PRIVATE_CLAIM_CODE,
                  handle=handle, secret_hex=secret_hex, nat_mode="private")
        _reassert_session(app, nest_url=private_url, secret_hex=secret_hex, view="feed")
        _wait_connected(app)

        # ── Phase 4b: link the PUBLIC nest from this one (the Nests page) ────
        # Forwarding is the user's own pairing row on the private nest
        # (private-mode.md § Implementation status today): one LinkBoth writes
        # it — the public nest's URL + `post_forward` — and the reciprocal row
        # on the public nest. Nothing else names the public nest.
        app.linked_nests.navigate()
        assert app.linked_nests.is_page_visible(), (
            f"Nests page unreachable on the private nest. error: {app.error_text()!r}")
        app.linked_nests.link(public_fqdn_url)
        assert app.linked_nests.wait_for_pairing_count(1, timeout=60.0), (
            "linking the public nest from the private one should add one pairing. "
            f"error: {app.linked_nests.page_error_text()!r}")
        print(f"[apfed-live] private nest linked to {public_fqdn_url} (post_forward)")

        # ── Phase 5: post on the PRIVATE nest → auto-forward → public AP push ─
        marker_text = f"AP federation live e2e {marker}"
        app.driver.navigate_to("feed")
        app.feed.create_post(marker_text)
        posts = app.feed._feed_posts_from_state()
        ours = next((p for p in posts if marker in p.get("body", "")), None)
        assert ours is not None, (
            f"our post (marker {marker!r}) not found in private-nest feed state: {posts!r}")
        post_id_hex = ours["post_id"]
        note_url = f"{actor_url}/notes/{post_id_hex}"
        print(f"[apfed-live] posted on private nest; expecting AP note {note_url}")

        # ── Phase 6: assert it lands in the real GoToSocial follower's HOME
        #    timeline — a status only reaches the home timeline if it was PUSHED
        #    to the follower's inbox (private→public forward + public AP Create). ─
        deadline = time.monotonic() + 240.0
        landed = None
        last_dump = "<none>"
        while time.monotonic() < deadline:
            timeline = peer.home_timeline(peer_token, limit=40)
            landed = next(
                (s for s in timeline if marker in (s.get("content") or "")), None)
            if landed:
                break
            last_dump = json.dumps(
                [{"acct": (s.get("account") or {}).get("acct"),
                  "content": (s.get("content") or "")[:80]} for s in timeline])[:600]
            time.sleep(6.0)
        assert landed is not None, (
            f"the private-nest post (marker {marker!r}) never reached the "
            f"GoToSocial follower's home timeline within 240s — the "
            f"private->public forward or the public AP Create-push did not "
            f"deliver.\n  last timeline: {last_dump}\n"
            f"  --- peer log ---\n{peer.peer_log(tail=160)}")
        assert (landed.get("account") or {}).get("acct", "").lower().startswith(
            ap_username.lower()), (
            f"the federated status is attributed to the wrong account: "
            f"{landed.get('account')!r}")
        print(f"[apfed-live] SUCCESS — private->public->fediverse chain delivered "
              f"to GoToSocial (status {landed.get('uri')})")

        # ── Phase 7: best-effort delete of the test post through the UI ──────
        _reassert_session(app, nest_url=private_url, secret_hex=secret_hex, view="feed")
        posts_now = app.feed._feed_posts_from_state()
        idx = next((i for i, p in enumerate(posts_now)
                    if marker in p.get("body", "")), None)
        if idx is not None:
            app.feed.delete_post(idx)
            print("[apfed-live] deleted the test post on the private nest")

    finally:
        if priv_name:
            remove_container(priv_name)

"""tier_4 (live-remote, OPT-IN, NON-DESTRUCTIVE): bootstrap a claim-fresh
deployed nest (example.com) to a serving production box — claimed by the fleet's
live admin identity, its Let's Encrypt cert served, its mail ports up — through
the linux app UI and release-artifact surfaces only.

Why this exists (user ruling 2026-09-26: *"use the dedicated e2e tests against
example.com for that (or add a new such test if needed)"*): a genesis-fresh box
boots UNCLAIMED on the self-signed floor cert with its mail bridges parked at
"await bridge approval", so strict TLS fails and the release pipeline's serving
gate (health + the TLS mail ports) cannot pass. Nothing
else takes it from there without a human at the app.

The flow — no nest API, no test-hooks route (convention 15), every mutation an
app UI action (convention 8), every wait a poll on observable state
(convention 14):

  1. **Reach the admin shell** (``helpers.live_admin.reach_admin_shell``): sign
     in if the box already knows the identity, else claim it with
     ``FAUNA_LIVE_CLAIM_CODE`` under an explicit ``FAUNA_LIVE_HANDLE``, choosing
     NAT mode ``public`` (a box serving the public web and mail).
  2. **Wait for the domain cert.** The claim's ``@domain`` suffix is the sole
     determinant of the deployment domain and triggers ACME for it
     (``bins/fauna-nest/src/claim_core.rs`` — ``ensure_mail_domain_registered``;
     ``tls-certificates.md`` § B), so there is no separate domain-config action:
     this step is a pure state wait until ``https://<domain>/api/v1/health``
     verifies under strict TLS with a certificate naming the domain.
  3. **Enable mail** (PLAIN password) through the mail-settings UI —
     a no-op when a credential already exists.
  4. **Approve the co-located mail bridges** through the admin UI — skipped
     when the mail ports already serve.
  5. **Wait for the mail ports** — ``mail.<domain>`` :993 (IMAPS) and :465
     (SMTPS) complete a strict-TLS handshake and greet; :587 upgrades via
     STARTTLS under the same verification.

Blast-radius argument (testing.md § The shared-box rule, non-destructive
carve-out — required on every non-destructive ``live_box`` test):
  - **What it mutates:** on an ALREADY-CLAIMED box, nothing — sign-in is a read
    (``helpers/live_handle.py``), and steps 3–4 are skipped by their own
    idempotency checks when mail is up. On an UNCLAIMED box it performs the
    deployment's first-admin setup: the claim (admin + handle + domain), the
    NAT-mode choice, one mail credential, the bridges' approval. That setup is
    the box's intended durable state, the thing the user asked this test to do,
    so there is deliberately no teardown.
  - **Why invisible to the human:** a claimed box sees one more sign-in; an
    unclaimed box has no human on it (no account exists to be signed into).
  - **Why it must NEVER factory-reset:** a reset rotates the AP instance
    actor's key under every real peer (``activitypub.md`` § Architecture) and
    wipes whatever the human holds on the box. There is no reset path here at
    all, and none must be added.

Preconditions (env):
  FAUNA_LIVE_NEST_URL       e.g. https://example.com — or the run's own box on a
                            `--nest live:URL` run (``live_box_door.live_box_url``)
  FAUNA_LIVE_SECRET_HEX     the fleet admin's ed25519 32-byte seed hex — an
                            OVERRIDE: by default the seed is resolved per box
                            (``live_box_door.admin_seed``: this var > the box's
                            ``~/.config/fauna/staging-box/<host>.json`` >
                            ``~/.fauna-id``), so a staging box this fleet
                            provisioned runs with nothing exported. Safe as an
                            ambient default because this test is
                            non-destructive (above); the ``live_box`` opt-in
                            (``--nest live`` / ``FAUNA_E2E_LIVE=1``) is what
                            selects it.
Needed only for the step that consumes it (failed with a named reason there):
  FAUNA_LIVE_HANDLE         the handle to CLAIM under, e.g. test@example.com — an
                            unclaimed box only; claiming bakes it in as the
                            admin's real handle and its suffix as the domain, so
                            it is never a probe. On a claimed box the handle is
                            derived from the secret.
  FAUNA_LIVE_CLAIM_CODE     an unclaimed box only — the code the nest prints in
                            its startup banner (stderr, i.e. the container log).
                            A read-only harness input, never a hook.
  FAUNA_LIVE_MAIL_PASSWORD  only when mail is not yet enabled — the PLAIN
                            password mail is enabled with (= the IMAP login the
                            mail live tests use).

Test taxonomy: tier_4 (live-remote — the deployed production image under real
supervision).
"""

from __future__ import annotations

import json
import os
import ssl
import time
import urllib.error
import urllib.request

import pytest

from helpers import live_box_door
from helpers.app_surface import skip_unbuilt
from helpers.live_admin import (
    approve_pending_bridges,
    enable_mail,
    reach_admin_shell,
    strict_mail_ports as _mail_ports,
    wait_connected,
)
from helpers.live_handle import derive_handle

# The run's box on a `--nest live` run, else FAUNA_LIVE_NEST_URL
# (`live_box_door.live_box_url`).
URL = live_box_door.live_box_url()
SECRET, SECRET_SOURCE = live_box_door.admin_seed(URL)
CLAIM_CODE = os.environ.get("FAUNA_LIVE_CLAIM_CODE", "")
PASSWORD = os.environ.get("FAUNA_LIVE_MAIL_PASSWORD", "")

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_nest,
    pytest.mark.live_box,
    # Class (3)'s admin-shell scan would otherwise deselect this on live (its
    # recipe runs `--nest live`): the unclaimed-box path approves bridges on
    # the admin Bridges page. Re-admitted under the shared-box rule's
    # non-destructive carve-out: the blast-radius argument above.
    pytest.mark.live_ok,
    pytest.mark.skipif(
        not (URL and SECRET),
        reason="live-box bootstrap test: name the box (`--nest live:URL`, or FAUNA_LIVE_NEST_URL) and provide the "
        f"box's admin seed ({live_box_door.SEED_SOURCES}) to run (plus FAUNA_LIVE_HANDLE + FAUNA_LIVE_CLAIM_CODE on an "
        "unclaimed box, FAUNA_LIVE_MAIL_PASSWORD while mail is off; hits a live "
        "external nest; opt-in, non-destructive — see module docstring)",
    ),
]

# ACME HTTP-01 on a public box is typically well under a minute; the ceiling
# covers a Let's Encrypt queue and the nest's own retry backoff.
CERT_TIMEOUT = 600.0
MAIL_TIMEOUT = 300.0


# ── strict-TLS probes (stdlib, default trust store, full verification) ──────


def _strict_ctx() -> ssl.SSLContext:
    return ssl.create_default_context()


def _https_health(domain: str) -> tuple[bool, str]:
    """``https://<domain>/api/v1/health`` under full verification. The
    hostname check is what proves the served certificate names the domain."""
    try:
        with urllib.request.urlopen(
            f"https://{domain}/api/v1/health", timeout=15, context=_strict_ctx()
        ) as resp:
            body = resp.read().decode("utf-8", "replace")
    except (urllib.error.URLError, ssl.SSLError, OSError) as e:
        return False, f"{type(e).__name__}: {e}"
    try:
        status = json.loads(body).get("status")
    except (json.JSONDecodeError, AttributeError):
        return False, f"unparseable health body: {body[:200]!r}"
    return status == "ok", body[:200]


def _poll(fn, desc: str, timeout: float, interval: float = 5.0):
    deadline = time.monotonic() + timeout
    observed = "<never evaluated>"
    while time.monotonic() < deadline:
        ok, observed = fn()
        if ok:
            return observed
        time.sleep(interval)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass
    pytest.fail(f"{desc} — not observed within {timeout:.0f}s (last: {observed})")


# ── the test ────────────────────────────────────────────────────────────────


def test_live_box_bootstrap(app):
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the live-box bootstrap drive (claim → cert → mail up)",
            detail="drives the linux Fauna app UI; the other apps are the "
            "remaining cross-app follow-on",
            tracked="docs/goal/architecture/nest/tls-certificates.md",
        )

    # 0) Preflight: a claimed box must know the admin seed, else it is the wrong
    #    box's seed (or a reset box) — environment, named by its source, not a
    #    failure of the bootstrap. Skipped with a claim code: an unclaimed box
    #    is then the expected start.
    if not CLAIM_CODE:
        live_box_door.preflight_admin(URL, SECRET, SECRET_SOURCE)

    # 1) Claim or sign in, UI-only.
    state = reach_admin_shell(
        app, nest_url=URL, secret_hex=SECRET, claim_code=CLAIM_CODE, nat_mode="public"
    )
    print(f"\n[bootstrap] reached admin shell via: {state}")
    wait_connected(app)
    handle, _local = derive_handle(app, URL)
    domain = handle.split("@", 1)[1]
    mail_host = f"mail.{domain}"
    print(f"[bootstrap] admin {handle}; domain {domain}")

    # 2) The claim named the domain and ordered its cert; wait on the state.
    observed = _poll(
        lambda: _https_health(domain),
        f"https://{domain}/api/v1/health verifying under strict TLS with a "
        f"certificate naming {domain}",
        CERT_TIMEOUT,
    )
    print(f"[bootstrap] strict-TLS health ok: {observed}")

    # 3) + 4) Mail up. Idempotent: already-serving mail skips both mutations.
    ports = _mail_ports(mail_host)
    if all(ok for ok, _ in ports.values()):
        print(f"[bootstrap] mail already serving on {mail_host}: {ports}")
        return

    if not PASSWORD:
        # Only a no-op enable is possible without it; fail if a credential
        # would have to be minted.
        app.mail_settings.navigate()
        if not app.mail_settings.wait_for_credential_count_at_least(1, timeout=12.0):
            pytest.fail(
                "mail is not enabled on the box and FAUNA_LIVE_MAIL_PASSWORD is "
                f"not set — cannot enable it. Mail ports now: {ports}"
            )
    else:
        enabled = enable_mail(app, PASSWORD)
        print(f"[bootstrap] mail {'enabled now' if enabled else 'already enabled'}")

    approved = approve_pending_bridges(app)
    print(f"[bootstrap] bridge approvals clicked: {approved}")

    def _all_up():
        now = _mail_ports(mail_host)
        return all(ok for ok, _ in now.values()), now

    ports = _poll(_all_up, f"{mail_host} :993/:465/:587 serving under strict TLS", MAIL_TIMEOUT)
    print(f"[bootstrap] mail serving: {ports}")

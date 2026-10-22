"""Live, opt-in tier_4: **re-issue a nest's TLS certificate by DNS-01 ACME,
driving the whole thing through the client UI** — the trusted-cert path for a
nest that cannot use HTTP-01 (`tls-certificates.md` § B tier 2).

This is the test that fixes the live nest, and it is written to fix *any* nest whose
domain sits in the Hetzner account the token belongs to: nothing here is
box-specific. Point `FAUNA_LIVE_NEST_URL` / `FAUNA_LIVE_MAIL_ADDRESS` at
another deployment and the same run renews that one.

**Why DNS-01 rather than the nest's own HTTP-01.** HTTP-01 needs inbound :80 at
the nest. A cloud firewall, an ISP block, or plain NAT removes that, and the nest
then has no route to a trusted cert at all — it drops to the self-signed floor,
which native Fauna apps tolerate (they pin the identity, `security.md`
§ Transport trust) but browsers, MUAs, and MTA-STS senders do not. DNS-01 needs
only DNS control, which the admin's client has and the nest deliberately never
does (`dns-management.md` § Where the credential lives: the nest holds no
DNS-provider key, ever).

**What "client UI only" means here, precisely.** Every mutation is a real UI
gesture on `admin-dns` — no WS-RPC shortcut stands in for the admin:

  * sign-in is the real onboarding flow (import key → handle check over live DoH
    → submit), not a `set_state` session patch;
  * the API token is typed into the write-only add-credential form
    (`admin-dns-add-credential-*`), the same form a real admin uses;
  * managed mode is the per-domain toggle (`admin-dns-domain-mode`);
  * issuance is the per-domain button (`admin-dns-cert-issue-button`);
  * **verification reads the badge** (`admin-dns-cert-status`) and nothing else —
    the test asserts the *rendered expiry date moved forward*. That is the honest
    end-to-end signal: the date can only advance if the client really drove an
    ACME order, really published `_acme-challenge` at Hetzner, really got a cert,
    really sealed it to the nest, and the nest really installed it and hot-reloaded
    its listener. Reading the cert off :443 with `openssl` would prove the same
    thing without proving the *admin* can see it, which is the product claim.

**Credential hygiene (the reason this test is safe to run against production).**
The client starts with **no** DNS credential — asserted, not assumed — and the
credential is removed again in a `finally`, along with any managed-mode flip the
test made. Both live in `fauna.state.dns`, which is BackupKey-sealed and *synced*,
so leaving either behind would durably change the admin's real account state on
every device. The token is never printed, never written to a file, and never
reaches the nest (it is used only inside the client, by design).

What this test does leave behind, by design: a **new certificate**. That is the
point.

Gating — all four are required, and the test skips loudly without them:
  * ``HETZNER_API_TOKEN``   — or the nest's *dns* project token file (the zone
                              project's under fauna.build —
                              ``helpers.live_provision.hetzner_token``,
                              ``scripts/_cloud_project.py``): a Hetzner **Cloud**
                              API token with Read&Write on the project holding
                              the zone (zones became Cloud resources when the
                              standalone DNS API went read-only 2026-05-20;
                              `i18n/providers.yaml` hetzner `api-token`). The
                              credential an admin app holds is that project's
                              (``deployment-threat-model.md`` § Concern 2, path 5).
  * ``FAUNA_E2E_LIVE=1``    — explicit opt-in. This spends real Let's Encrypt
                              issuance budget against a real domain.
  * ``FAUNA_LIVE_NEST_URL`` — e.g. https://dev.fauna.build
  * ``FAUNA_LIVE_MAIL_ADDRESS`` — the nest's admin handle (not derivable here:
                              `test_live_handle_derivation.py` classifies why).
                              The admin identity SEED is required too —
                              `fauna.tls.publish_cert` is Admin-only — but it is
                              resolved per box (``live_box_door.admin_seed``:
                              ``FAUNA_LIVE_SECRET_HEX`` > the box's staging-box
                              file > ``~/.fauna-id``), never demanded. Safe as an
                              ambient default: the seed is not this test's
                              opt-in — ``FAUNA_E2E_LIVE=1`` and the token are.
Optional:
  * ``FAUNA_E2E_CERT_DOMAIN`` — which `admin-dns` domain row to renew. Defaults
                              to the handle's domain (the deployment's primary).

⚠ **Rate limits are real.** Let's Encrypt allows 5 duplicate certificates per
exact SAN set per week; a renewal that changes the SAN set is not a duplicate.
Do not loop this test. `tests/live/conftest.py` gives live tests a 90-minute
ceiling; a healthy run is minutes.
"""
from __future__ import annotations

import hashlib
import os
import re
import ssl

import pytest

from helpers import live_box_door
from helpers.waiting import wait_until
from helpers.live_provision import hetzner_token, hetzner_token_hint, skip_unless_live_drive_app

_REQUIRED = (
    "FAUNA_LIVE_NEST_URL",
    "FAUNA_LIVE_MAIL_ADDRESS",
)


def _truthy(v: str | None) -> bool:
    return (v or "").strip().lower() in {"1", "true", "yes", "on"}


NEST_URL = os.environ.get("FAUNA_LIVE_NEST_URL", "").rstrip("/")
SECRET, SECRET_SOURCE = live_box_door.admin_seed(NEST_URL)
ADDRESS = os.environ.get("FAUNA_LIVE_MAIL_ADDRESS", "").strip()
HANDLE_DOMAIN = ADDRESS.split("@", 1)[-1] if "@" in ADDRESS else ADDRESS
TARGET_DOMAIN = os.environ.get("FAUNA_E2E_CERT_DOMAIN", "").strip() or HANDLE_DOMAIN
#: The zone's token — the credential the admin app holds — never the box's.
DNS_TOKEN = hetzner_token(NEST_URL or None, purpose="dns")

pytestmark = [
    # A real deployment artifact (the production nest image under s6 on its own
    # box) plus real DNS and a real CA — nothing here is locally built but the
    # client. testing.md § The four-tier taxonomy.
    pytest.mark.tier_4,
    pytest.mark.live_box,   # machine-wide flock on the shared live nest
    pytest.mark.live_nest,  # builds/starts NO local nest (conftest `_live_nest_session`)
    # No client platform marker — the app axis is decided in-body by
    # `skip_unless_live_drive_app`, so an app that cannot drive this yet is
    # TALLIED as unbuilt debt (convention 7) rather than silently deselected.
    pytest.mark.skipif(
        not (all(os.environ.get(k, "").strip() for k in _REQUIRED) and SECRET),
        reason="live DNS-01 cert renewal: set " + ", ".join(_REQUIRED)
        + f" and provide the box's admin seed ({live_box_door.SEED_SOURCES}) (opt-in)",
    ),
    pytest.mark.skipif(
        not DNS_TOKEN,
        reason="live DNS-01 cert renewal needs the zone's Cloud R/W token: "
        + hetzner_token_hint(NEST_URL or None, purpose="dns"),
    ),
    pytest.mark.skipif(
        not _truthy(os.environ.get("FAUNA_E2E_LIVE")),
        reason="live DNS-01 cert renewal spends real Let's Encrypt issuance budget "
        "against a real domain: set FAUNA_E2E_LIVE=1",
    ),
]

# The `admin-dns-cert-status` badge renders "<label> <state> — Expires
# YYYY-MM-DD" (linux `cert_status_text` → `format_cert_expiry`). Match the date
# alone so the assertion is independent of the surrounding i18n strings, which
# differ per locale and per client.
_EXPIRY_RE = re.compile(r"(\d{4}-\d{2}-\d{2})")


def _badge_expiry(badge: str) -> str | None:
    """The expiry date rendered in a cert-status badge, or `None` when the badge
    shows no date — the floor case (`cert_status_view` withholds a self-signed
    cert's own far-future expiry) or a status that has not loaded yet."""
    m = _EXPIRY_RE.search(badge or "")
    return m.group(1) if m else None


def _domain_index(app, domain: str) -> int:
    names = [d.strip().lower() for d in app.admin.dns_domain_names()]
    assert domain.lower() in names, (
        f"{domain!r} is not one of the deployment's domains on admin-dns: {names!r}. "
        f"Set FAUNA_E2E_CERT_DOMAIN to one of them."
    )
    return names.index(domain.lower())


def _badge_for(app, domain: str) -> str:
    """This domain's rendered cert-status badge. Re-resolves the row index every
    read: the page rebuilds on each refresh and rows can reorder."""
    return app.admin.cert_statuses()[_domain_index(app, domain)]


def _served_cert_fingerprint(domain: str) -> str | None:
    """SHA-256 of the certificate the nest serves for ``domain`` on :443, or
    ``None`` when it cannot be fetched. Read without verification: the floor case
    serves a self-signed cert, and *which* cert is served is the question."""
    try:
        pem = ssl.get_server_certificate((domain, 443), timeout=20)
    except Exception:  # noqa: BLE001 — an unreachable :443 reads as "no cert"
        return None
    return hashlib.sha256(ssl.PEM_cert_to_DER_cert(pem)).hexdigest()


def _wait_cert_status_loaded(app, domain: str, timeout: float = 120.0) -> str:
    """Wait until `domain`'s cert-status badge leaves the transient 'Checking…'
    loading state and reflects the actually-served certificate, then return it.

    This is load-bearing for correctness, not just tidiness. `cert_status` is an
    async round trip the page kicks on navigate/refresh; the badge shows 'Checking…'
    (no date → `_badge_expiry` is `None`) until it resolves. If the *baseline* read
    below catches that transient `None`, the strict-advance assertion degenerates to
    "any date appears" — because `before_expiry is None` short-circuits it — so a
    deployment that already serves a cert (the normal case) reads as a successful
    'renewal' the instant the badge first shows the *existing* expiry, without any
    re-issue having happened. That exact false-green fired live on 2026-07-23. Assert
    latency-independent state: wait for the status to load, then compare.

    A genuinely cert-less deployment (the self-signed floor) never leaves a date and
    is not stuck 'Checking…' — it settles on a terminal no-cert status — so this
    returns as soon as the badge is no longer loading, preserving the intended
    `before_expiry is None` floor case.
    """
    def _loaded() -> str | None:
        badge = _badge_for(app, domain)
        if _badge_expiry(badge) is not None or "checking" not in badge.lower():
            return badge
        app.admin.refresh_dns()
        return None

    return wait_until(
        _loaded,
        timeout,
        interval=3.0,
        diagnose=lambda: f"{domain}'s cert-status badge stayed loading: {_badge_for(app, domain)!r}",
    )


def _open_dns_page(app, timeout: float = 90.0) -> None:
    """Navigate to `admin-dns` and wait for the domain matrix to render (the nav
    kicks async `list_records` + `cert_status` + `verify_records` round trips)."""
    app.admin.navigate_dns()
    wait_until(
        lambda: any(d.strip() for d in app.admin.dns_domain_names()),
        timeout,
        interval=2.0,
        diagnose=lambda: f"admin-dns never rendered a domain row; error={app.error_text()!r}",
    )


def _sign_in_as_admin(app, timeout: float = 180.0) -> None:
    """Real onboarding UI sign-in: import the admin seed, run the live DoH handle
    check, submit. Never claims — an unclaimed box is a precondition failure here,
    not something a cert test should quietly fix."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(SECRET)
    ob.fill_handle(ADDRESS)
    ob.run_handle_check(timeout=60)
    ob.submit_handle()

    def _signed_in() -> bool:
        if app.driver.is_visible("feed-view") or app.driver.is_visible("feed-tab"):
            return True
        if app.driver.is_visible("claim-code-input"):
            pytest.fail(
                f"the nest at {ADDRESS!r} is UNCLAIMED — claim it from a client first. "
                "This test renews an existing deployment's cert; it never claims."
            )
        if app.driver.is_visible("launch-retry-button"):
            try:
                app.driver.click("launch-retry-button")
            except Exception:
                pass
        return False

    wait_until(
        _signed_in,
        timeout,
        interval=2.0,
        diagnose=lambda: f"sign-in never reached the feed; error={app.error_text()!r}",
    )


def _clear_all_credentials(app) -> None:
    """Remove every held DNS credential through the UI. Used both as the
    precondition (the client must start with none) and as teardown."""
    for _ in range(10):
        if app.admin.dns_credential_count() == 0:
            return
        app.admin.clear_dns_credential(0)
    pytest.fail(
        "could not clear the held DNS credentials through the UI; "
        f"{app.admin.dns_credential_count()} still shown"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_dns01_reissues_the_nest_certificate_through_the_client_ui(app):
    """Renew `TARGET_DOMAIN`'s certificate by DNS-01, entirely through the admin
    UI, and prove it by the badge's expiry date advancing.

    The assertion is deliberately a *strict* advance rather than "a trusted cert
    is served": the nest already serves a trusted cert on the day this runs, so
    `valid-trusted` would pass without anything having happened. Only a later
    `notAfter` distinguishes a real re-issue from a no-op. When the badge showed
    no date before (the self-signed floor withholds one), there is no earlier
    date to beat, so the certificate served on :443 must itself have changed —
    a date merely appearing is not proof that anything was issued.
    """
    skip_unless_live_drive_app(
        app.driver, surface="the live DNS-01 cert-renewal drive (native order core)"
    )

    token = DNS_TOKEN
    nest_url = NEST_URL
    print(f"\n[dns01] renewing {TARGET_DOMAIN} on {nest_url}")
    # The box must know the resolved seed before anything is driven: a wrong
    # box's seed (or a reset box) skips as environment, named by its source.
    live_box_door.preflight_admin(nest_url, SECRET, SECRET_SOURCE)

    _sign_in_as_admin(app)
    _open_dns_page(app)

    idx = _domain_index(app, TARGET_DOMAIN)
    # Wait for cert_status to actually load before snapshotting the baseline — a
    # transient 'Checking…' (expiry None) here silently defeats the strict-advance
    # assertion below (the 2026-07-23 false-green). See `_wait_cert_status_loaded`.
    before_badge = _wait_cert_status_loaded(app, TARGET_DOMAIN)
    before_expiry = _badge_expiry(before_badge)
    before_served = _served_cert_fingerprint(TARGET_DOMAIN)
    print(f"[dns01] before: {before_badge!r} (expiry {before_expiry}; served {before_served})")

    # Precondition, asserted not assumed: the client holds NO DNS credential, so
    # this run genuinely exercises "admin supplies the token for the first time".
    _clear_all_credentials(app)
    assert app.admin.dns_credential_count() == 0, "the client must start with no DNS credential"

    mode_before = app.admin.dns_domain_modes()[idx]
    flipped_mode = False
    try:
        # ── 1. Type the API token into the write-only add-credential form ──────
        # `PutCredentials` verifies against the provider API before storing, so a
        # bad token never lands — and the zone list that comes back is what
        # decides which domains are manageable.
        app.admin.add_dns_credential("hetzner", {"api-token": token})
        wait_until(
            lambda: app.admin.dns_credential_count() >= 1,
            60.0,
            interval=2.0,
            diagnose=lambda: (
                "the Hetzner credential was not stored — verify() failed against the "
                f"provider API. error={app.error_text()!r}"
            ),
        )
        assert "hetzner" in " ".join(app.admin.dns_credential_providers()).lower(), (
            f"expected a hetzner credential row; got {app.admin.dns_credential_providers()!r}"
        )
        zones = app.admin.dns_credential_zones()
        assert any(TARGET_DOMAIN.lower() in z.lower() for z in zones), (
            f"the token's zones {zones!r} do not cover {TARGET_DOMAIN!r}, so DNS-01 "
            "cannot publish its _acme-challenge. Is the domain in this Hetzner account?"
        )
        print(f"[dns01] credential stored; zones={zones!r}")

        # ── 2. Put the domain in Fauna-managed mode ───────────────────────────
        # Managed (or CNAME-delegated) is what makes the issue button a single
        # auto-publishing dispatch rather than the manual paste flow
        # (`build_cert_issuance`'s `single_issue`).
        idx = _domain_index(app, TARGET_DOMAIN)
        if app.admin.dns_domain_modes()[idx].strip().lower() != "fauna-managed":
            app.admin.toggle_domain_mode(idx)
            flipped_mode = True
        wait_until(
            lambda: app.admin.dns_domain_modes()[_domain_index(app, TARGET_DOMAIN)]
            .strip()
            .lower()
            == "fauna-managed",
            60.0,
            interval=2.0,
            diagnose=lambda: (
                f"{TARGET_DOMAIN} did not switch to Fauna-managed; "
                f"modes={app.admin.dns_domain_modes()!r} error={app.error_text()!r}"
            ),
        )
        idx = _domain_index(app, TARGET_DOMAIN)

        # ── 3. Click get/renew certificate ────────────────────────────────────
        # One dispatch runs the whole order client-side: create/reuse the ACME
        # account, publish `_acme-challenge.<name>` TXT via the Hetzner API, wait
        # for propagation, let the CA validate, finalize, fetch, seal the result to
        # the nest's identity key, and deliver it over `fauna.tls.publish_cert`.
        app.admin.issue_cert(idx)
        print("[dns01] issue dispatched — ACME order + DNS-01 publish in flight")

        # ── 4. Verify through the badge, and only the badge ───────────────────
        # Budget: the client's propagation gate actively polls the authoritative
        # NS for up to DEFAULT_RESOLVABILITY_DEADLINE (45 min — Hetzner's zone
        # publish is batchy and irregular, measured 15 min / >21 min / one 5 h
        # outlier) before the CA even validates, so the badge can legitimately
        # move only after that. Deadline-poll: a fast provider run pays none of
        # this. Stays under the live conftest's 5400 s per-test cap with room
        # for sign-in + validation + install.
        seen = {"badge": "", "expiry": None, "served": before_served}

        def _reissued() -> bool:
            app.admin.refresh_dns()  # re-runs list_records + cert_status + verify
            seen["badge"] = _badge_for(app, TARGET_DOMAIN)
            seen["expiry"] = _badge_expiry(seen["badge"])
            err = app.error_text()
            if err:
                print(f"[dns01] page error while polling: {err!r}")
            if not seen["expiry"]:
                return False
            if before_expiry is not None:
                return seen["expiry"] > before_expiry
            # No earlier date to beat: the served certificate itself must change.
            seen["served"] = _served_cert_fingerprint(TARGET_DOMAIN)
            return seen["served"] is not None and seen["served"] != before_served

        wait_until(
            _reissued,
            3000.0,
            interval=15.0,
            diagnose=lambda: (
                f"the served certificate did not change: {TARGET_DOMAIN}'s badge reads "
                f"{seen['badge']!r} (expiry {seen['expiry']}, was {before_expiry}; served "
                f"{seen['served']}, was {before_served}). The ACME order, the "
                f"_acme-challenge publish, the seal-to-nest delivery, or the nest-side "
                f"install did not complete. error={app.error_text()!r}"
            ),
        )
        print(f"[dns01] after: {seen['badge']!r} — expiry {before_expiry} → {seen['expiry']}")

    finally:
        # ── 5. Leave no durable trace but the new certificate ─────────────────
        # `fauna.state.dns` is synced to every one of the admin's devices, so both
        # the credential and the mode flip must be undone even when the test fails
        # mid-flight.
        try:
            if flipped_mode:
                i = _domain_index(app, TARGET_DOMAIN)
                if app.admin.dns_domain_modes()[i].strip().lower() != mode_before.strip().lower():
                    app.admin.toggle_domain_mode(i)
        except Exception as e:  # noqa: BLE001 — teardown must never mask the real failure
            print(f"[dns01] teardown WARNING: restoring domain mode failed: {e!r}")
        try:
            _clear_all_credentials(app)
            print("[dns01] teardown: DNS credential removed from the client")
        except Exception as e:  # noqa: BLE001
            print(f"[dns01] teardown WARNING: clearing the credential failed: {e!r}")

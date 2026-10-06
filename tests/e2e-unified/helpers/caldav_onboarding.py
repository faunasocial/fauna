"""UI-driven onboarding-with-CalDAV scaffolding for the tier_3 caldav matrix.

No-modes retirement (ratified 2026-07-12): every nest is sealed at rest
unconditionally now, and the four deployment-toggle checkboxes
(`onboarding-enable-{email,caldav,carddav,webdav}-checkbox`) are RETIRED —
mail/CalDAV enablement is a MACHINE-DERIVED default (ON iff the handle targets
a real registerable domain; OFF for `localhost` / a bare IP), applied by the
post-claim launch glue with no onboarding-time choice
(`docs/goal/behavior/onboarding.md` § 3b). There is no storage-mode question
either, so a UI claim now runs straight to `nat_mode_choice`, the terminal
admin-path step.

`claim_through_onboarding` drives a fresh UNCLAIMED nest (the
`unclaimed_caldav_nest` Task-2 factory fixture) through the client's real
onboarding — import-key → handle (`test@<domain>`) → claim-code →
nat_mode_choice (dismissed) — all the way to LoggedIn, so the post-claim launch
glue has fired the handle-derived mail/caldav defaults.

`onboard_and_enable` carries that further: it reads the derived mail/caldav
state once the post-claim step's completion anchor has passed, then — for any axis that doesn't already match the cell's
desired `enable_mail`/`enable_caldav` — flips it explicitly through the admin
Mail / Calendar settings pages (the real "a user changes it after the fact"
path, `toggle_mail_enabled` / `toggle_caldav_enabled`). It then provisions the
bits the UI can't express for a test domain (the MTA's sealed TLS
blob + the MDA self-signed CalDAV cert + a spam policy that clears the
DNS-perimeter gates) over Admin WS-RPC with the admin key the test holds,
approves the self-enrolled MTA + MDA on admin-bridges-pending, and — when mail
or caldav is enabled — mints the shared MUA credential. It returns
``{password, handle, domain, caldav_default}`` for the round-trip (Task 4) to
drive a CalDAV MUA with.

Reuses the shared claim sub-step from `mail_client_ui`
(`claim_to_nat_mode_page`) — priority #2/#4: one claim path, not a copy.
"""

from __future__ import annotations

import ipaddress
import sqlite3
import time

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_client_ui import claim_to_nat_mode_page, wait_tcp_accept
from helpers.waiting import await_serving_enablement_for
from helpers.mail_aliases import add_exact_alias_as

# The handle local part the caldav-onboarding flow claims with. Like
# `mail_client_ui.HANDLE_LOCAL` ("alice"), the handle CARRIES its domain
# (`test@<domain>`); the matrix uses "test" to mirror the live-box CalDAV proof
# (`test_caldav_live_nest.py`, which factory-resets + claims handle `test`).
HANDLE_LOCAL = "test"

# The PLAIN mail credential the onboarding flow mints and every CalDAV/IMAP MUA
# in the matrix authenticates with. A KNOWN value (typed, auto-generate OFF) so
# the round-trip holds the exact password — mirrors the proven green tier_3
# CalDAV tests' `_PASSWORD` (test_caldav_mkcalendar_create). ~144 bits, no auth
# policy edge cases.
_MUA_PASSWORD = "CalDavOnboardingVariantPw0042Hh"

# The display name + derived credential_id of the post-onboarding MUA credential
# we add with the known password. `derive_credential_id("Mua") == "mua"`; the MUA
# authenticates as `<handle>+mua@<domain>` (RFC-5233 sub-addressing,
# mail-credentials.md § MUA-username). A SECOND credential (the onboarding-minted
# `default` is auto-generated and uncaptured), so it never collides with `default`.
_MUA_CRED_NAME = "Mua"
_MUA_CRED_ID = "mua"


def _admin_keys(admin_secret_hex: str):
    """The (actor_id, signing_key) byte pair the Admin WS-RPC client needs, from
    the test-held admin secret."""
    from nacl.signing import SigningKey

    sk = SigningKey(bytes.fromhex(admin_secret_hex))
    return bytes(sk.verify_key), bytes(sk)


def claim_through_onboarding(app, nest, *, node_url):
    """Drive a fresh unclaimed `nest` (a `CalDavVariantNestHandle`) through the
    client's real onboarding all the way to LoggedIn — import-key → handle →
    claim-code → nat_mode_choice (dismissed, confirm-only common case) — so the
    post-claim launch glue has fired the handle-derived mail/caldav-enablement
    defaults by the time this returns.

    `node_url` is the address the app dials — `nest.nest_url` for a native app,
    the nest's SPA proxy for web (`mail_dedicated_nest.dedicated_node_url`;
    `mail_client_ui.claim_to_nat_mode_page` says why it is the caller's to
    pass). The harness's own admin WS-RPC reads stay on `nest.nest_url`: Python
    needs no CORS.

    The handle is `test@<nest.domain>` (the domain the nest's `handle_domain`
    encodes for this address type — `fauna.test` / `localhost` / `127.0.0.1:port`),
    so the launch glue derives the enable-caldav default from it (ON iff a real
    registerable domain).

    Generates a fresh admin identity and imports it through the paste-key flow
    (the same real UI claim `claim_enable_and_ready` runs), so a later
    `onboard_and_enable` continuation would already hold the key; this
    function returns the secret hex for that continuation.
    """
    from nacl.signing import SigningKey

    domain = nest.domain
    handle = f"{HANDLE_LOCAL}@{domain}"
    admin_sk = SigningKey.generate()
    admin_secret_hex = bytes(admin_sk).hex()

    claim_to_nat_mode_page(
        app, nest, handle=handle, admin_secret_hex=admin_secret_hex,
        node_url=node_url,
    )
    # Dismiss the terminal nat_mode_choice step — this is what exits the
    # wizard to LoggedIn and fires the post-claim launch glue (the
    # handle-derived mail/caldav-enablement defaults).
    app.onboarding.finish_nat_mode()
    time.sleep(2.0)  # let the authenticated shell + WS session settle
    return {"admin_secret_hex": admin_secret_hex, "handle": handle, "domain": domain}


def _is_domainless_locator(domain: str) -> bool:
    """True when `domain` is a bare locator (IP / localhost / .local), NOT a
    registerable mail-domain — mirroring nest's
    ``fauna_provisioning::probe::resolve_handle_domain(d).is_local``.

    On such a nest there is no ``mail_domains`` row, so the domain-scoped
    serving-provision steps don't apply (any-locator design 2026-06-18,
    tracked internally):

    - local IMAP/CalDAV login resolves the bare handle via the handle→actor
      store (Change A, ``validate_recipient`` fallback), so NO account alias is
      written — writing one would mask the very path this matrix proves; and
    - the MDA serves nest's self-signed FLOOR cert (Change B/C — the Go MDA
      fetches under a floor sentinel when ``PrimaryDomain`` is empty), so the
      per-domain ``provision_self_signed_cert`` (which 404s without a
      ``mail_domains`` row) is skipped.

    Email, if enabled, has no external deliverability on such a box (spec § 6);
    the matrix exercises only the CalDAV calendar round-trip, for which the MTA
    just needs to bind (TLS off the same floor cert)."""
    host = domain
    # Strip a trailing :port (the matrix uses 127.0.0.1:<port> and bare
    # hostnames; no bracketed IPv6 literal is produced by the fixture).
    if host.count(":") == 1:
        host = host.rsplit(":", 1)[0]
    host = host.lower()
    if host == "localhost" or host.endswith(".localhost") or host.endswith(".local"):
        return True
    try:
        ipaddress.ip_address(host)
        return True
    except ValueError:
        return False


def _auth_username(handle: str, credential_id: str | None = None) -> str:
    """Build the CalDAV/IMAP Basic-auth user-id for `handle`, optionally RFC-5233
    sub-addressed with `+credential_id` (`test@<loc>` + `mua` → `test+mua@<loc>`).

    Strips a trailing ``:port`` from the locator: a Basic-auth user-id MUST NOT
    contain a colon (RFC 7617 — the server splits ``username:password`` on the
    FIRST colon), so a bare-locator nest's ``host:port`` (``127.0.0.1:37253``)
    left in the username would swallow the port digits into the password
    (``…:37253:`` + the real password → a wrong, longer secret → AEAD-unwrap
    fails with a 401). A real MUA never puts the port in the username — it lives
    in the server URL — and nest's handle→actor fallback resolves ``test@127.0.0.1``
    identically to ``test@127.0.0.1:37253`` (the domain just has to be
    unregistered). Single trailing ``:port`` only; an IPv6 literal (multiple
    colons) is left untouched, as is a real domain (no colon)."""
    local, sep, dom = handle.partition("@")
    if credential_id:
        local = f"{local}+{credential_id}"
    if not sep:
        return local
    if dom.count(":") == 1:
        dom = dom.rsplit(":", 1)[0]
    return f"{local}@{dom}"


def _hand_poke_mda_x25519(nest):
    """Hand-poke the MDA bridge's `x25519_pubkey` into the nest's
    `bridge_service_users` row so a subsequent `provision_self_signed_cert` seals
    the CalDAV cert to it. Mirrors `_spawn_mda_bridge` step 3 (which is SKIPPED
    for `pre_approve=False` — an unclaimed nest had no admin to do it at spawn);
    we replay it post-claim, now that the nest is claimed and serving CalDAV is
    wanted. Tracked alongside the production-path note in the `mail_bridge_mda`
    docstring."""
    mda = nest.mda
    db_path = nest.nest["db_path"]
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "UPDATE bridge_service_users SET x25519_pubkey = ?"
            " WHERE ed25519_pubkey = ? AND status != 'revoked'",
            (mda.x25519_pubkey, mda.ed25519_pubkey),
        )
        conn.commit()
    finally:
        conn.close()


def _provision_serving_bits(app, nest, admin_secret_hex, *, enable_mail, enable_caldav):
    """Provision, over Admin WS-RPC with the test-held admin key, the bits a test
    domain has no real cert/DNS for: the MTA's sealed TLS blob (mail),
    the MDA's self-signed CalDAV cert (caldav), a routable account alias for the
    auth address (`test@<domain>` → the claimed actor), and a spam policy that
    clears the DNS-perimeter gates. Only the bits the requested enablement needs
    are provisioned."""
    actor_id, signing_key = _admin_keys(admin_secret_hex)
    domain = nest.domain
    mta = nest.mta
    # A domainless / bare-IP / localhost nest has no registered mail-domain, so
    # the two domain-scoped provision steps below (account alias + per-domain
    # self-signed cert) don't apply: login resolves by handle (Change A) and the
    # MDA serves the self-signed floor cert (Change B/C). See
    # `_is_domainless_locator` for the full rationale (any-locator design).
    domainless = _is_domainless_locator(domain)

    admin_ws = WsRpcAdminClient(nest.nest_url, actor_id=actor_id, signing_key=signing_key)
    with admin_ws:
        # Clear the DNS-perimeter gates so the approved MTA accepts inbound and
        # outbound without real DNS (mirrors `claim_enable_and_ready` step 2).
        admin_ws.call(
            "fauna.bridges.put_spam_policy",
            {
                "baseline_standing_publish": False,
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "max_conn_per_min": 1000,
            },
        )
        if (enable_mail or enable_caldav) and not domainless:
            # Route the auth address `test@<domain>` to the claimed actor. EnableMail
            # provisions the per-actor mail key material but NEVER an alias — mapping
            # an address to an actor is an admin/onboarding concern (mirrors
            # `mail_dedicated_nest.alias_admin_to_address`, which the proven-green
            # CalDAV tier_3 tests use). Without it the CalDAV/IMAP AUTH `resolveActor`
            # (and the MTA `validate_recipient`) can't map `test@<domain>` to the
            # actor, so the MUA's PROPFIND returns 401 even though the credential is
            # valid. The handle local part is `HANDLE_LOCAL` ("test"); the actor is
            # the one this same admin key claimed.
            #
            # SKIPPED for a domainless nest: there `<domain>` is a bare locator
            # (IP/localhost), not a registered mail-domain, so login resolves the
            # bare handle via the handle→actor store (Change A,
            # `validate_recipient` fallback) — writing an alias would mask that
            # path. The any-locator matrix proves exactly the no-alias handle
            # resolution, so the absence here is the point.
            add_exact_alias_as(admin_ws, domain, HANDLE_LOCAL)
        if enable_mail:
            # The MTA's outbound delivery needs its sealed TLS cert.
            if getattr(mta, "tls_blob", None) is not None:
                admin_ws.provision_tls_cert_blob(mta.tls_blob)
        if enable_caldav:
            # The MDA's CalDAV-HTTPS listener fetches its TLS cert sealed to its
            # x25519 at startup; hand-poke the x25519 so the seal can fan out / be
            # sealed-on-read to it (needed for BOTH the per-domain and the floor
            # cert paths below).
            _hand_poke_mda_x25519(nest)
            if not domainless:
                # Registered-domain nest: seal a per-domain self-signed cert
                # (CN/SAN = `mail.<domain>`) to the MDA.
                admin_ws.call(
                    "fauna.bridges.provision_self_signed_cert",
                    {"domain": domain, "additional_dns_sans": []},
                )
            # Domainless nest: NO per-domain cert. `provision_self_signed_cert`
            # would 404 (`fauna.bridges.not_found` — no `mail_domains` row), and
            # production never auto-calls it. Instead the MDA fetches under the
            # floor sentinel and nest's `fetch_tls_cert_blob` seals its self-signed
            # FLOOR cert (CN "fauna-nest", SANs localhost/127.0.0.1) to the MDA's
            # poked x25519, so CalDAV serves TLS off the floor (Change B/C,
            # any-locator design 2026-06-18). MUAs connect with `verify=False`.


def _approve_pending_bridge(app, pubkey_hex, *, timeout=20.0):
    """Approve one self-enrolled bridge by its ed25519 pubkey hex on the
    admin-bridges-pending UI, addressed by pubkey so both the MTA and the MDA can
    be approved. (`claim_enable_and_ready` no longer approves anything: its one
    real-domain cell is auto-approved by the claim's own mail enable.)

    The matrix approves TWO bridges (MTA + MDA) that are pending SIMULTANEOUSLY,
    so the pending list has multiple cards. The earlier copy of this loop read
    the target's pubkey from the FLAT `admin-bridges-pending-pubkey-hex` list at
    index `i`, then clicked the FLAT `admin-bridges-pending-approve-button` list
    at the same `i` — which assumes the two flat per-id lists enumerate in the
    same card order. They don't reliably (different testid, separate tree
    traversals), so a 2-card list approved the WRONG bridge (the MDA when the MTA
    was asked for) — the approve button captures its card's pubkey at build time
    (`admin.rs build_pending_bridge_card`), so the click approved whatever card
    the desync'd index landed on, and the sibling call then found its row already
    gone and returned False. With a SINGLE pending bridge the desync can't
    manifest; the matrix is the first 2-pending-bridge caller.

    Fix: address the CARD, not two parallel flat lists. Each
    `admin-bridges-pending-card` wraps both its own `pubkey-hex` and its own
    `approve-button`, so scoping every query to one card index makes the read and
    the click target the SAME card — no cross-list index assumption. Re-navigate
    (re-fetch) at the top of every poll cycle so a card removed by the sibling
    approval doesn't leave a stale index.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        # Fresh fetch each cycle: after the sibling bridge is approved the list
        # re-renders, so a stale card index from a prior cycle must not be reused.
        app.admin.navigate_bridges_pending()
        n = app.driver.count("admin-bridges-pending-card")
        for i in range(n):
            scope = f"admin-bridges-pending-card[{i}]"
            try:
                txt = app.driver.get_text(
                    "admin-bridges-pending-pubkey-hex", scope=scope
                ) or ""
                if pubkey_hex in txt:
                    app.driver.click("admin-bridges-pending-approve-button", scope=scope)
                    return True
            except LookupError:
                # The card vanished between `count` and this read: the list
                # re-rendered under us, most often because a bridge
                # AUTO-APPROVED on its enrollment poll (`onboard_and_enable`
                # step 6). Re-fetch instead of indexing a stale list — measured
                # 2026-09-21 on tui, where three matrix cells died here with
                # `LookupError: not found` before reaching the round trip.
                break
        time.sleep(0.5)
    return False


def _read_mail_config(admin_secret_hex: str, nest_url: str) -> dict:
    """One `fauna.bridges.get_mail_config` read — the Admin twin the admin
    Mail/Calendar pages hydrate from."""
    actor_id, signing_key = _admin_keys(admin_secret_hex)
    admin_ws = WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=signing_key)
    with admin_ws:
        return admin_ws.call("fauna.bridges.get_mail_config", {})


def _derived_enablement(app, admin_secret_hex: str, nest_url: str) -> tuple[bool, bool]:
    """Return the handle-derived ``(mail_enabled, caldav_enabled)`` the
    post-claim serving-enablement step left behind — read ONCE, after the step's
    completion anchor (`fauna_e2e_agent::SERVING_ENABLEMENT_KEY`) shows this
    admin's run finished (`helpers.waiting.await_serving_enablement_for`). Past
    that anchor the glue has nothing left to write, so the read is final in both
    directions: a slow real-domain enable cannot miss it, and the "stays OFF"
    cases (a domainless / localhost / ip handle) are a verdict rather than a
    guess that nothing came within some window (convention 14)."""
    actor_id, _ = _admin_keys(admin_secret_hex)
    await_serving_enablement_for(app.driver, actor_id.hex())
    cfg = _read_mail_config(admin_secret_hex, nest_url)
    return bool(cfg.get("mail_enabled")), bool(cfg.get("caldav_enabled"))


def _toggle_axis_until_settled(
    admin_secret_hex: str, nest_url: str,
    *, navigate, toggle, config_key: str, want: bool, budget_s: float = 15.0,
) -> None:
    """Navigate to a settings page and flip its toggle to `want`, re-clicking
    if the first click misses.

    The toggle widget (`admin_mail.rs`/`admin_calendar.rs` `switch_row`) mounts
    at its GTK builder default (OFF) and only reflects the true persisted
    value once the page's async hydrate lands and calls `render()` — there is
    no driver-visible signal for "hydrate landed" to poll before clicking (a
    `gtk::Switch` exposes no readable text, `find.rs::text_of`). A click that
    races hydrate flips the WIDGET's still-default OFF state rather than the
    true one, so when the true state is already ON the click is a same-value
    no-op instead of the intended OFF flip — ground truth stays unchanged. By
    the time a re-click lands, `render()` has long since synced the widget to
    the true value, so the retry always flips the right direction. Same
    race-tolerant idiom `AdminActions.add_forwarder` uses for the hosted-domain
    picker."""
    navigate()
    deadline = time.monotonic() + budget_s
    next_click = time.monotonic()
    while time.monotonic() < deadline:
        if bool(_read_mail_config(admin_secret_hex, nest_url).get(config_key)) == want:
            return
        if time.monotonic() >= next_click:
            toggle()
            next_click = time.monotonic() + 3.0
        time.sleep(0.5)
    cfg = _read_mail_config(admin_secret_hex, nest_url)
    raise AssertionError(
        f"admin toggle for {config_key!r} did not settle to {want}; "
        f"get_mail_config still reports {cfg!r}"
    )


def _ensure_enablement(
    app, admin_secret_hex: str, nest_url: str,
    *, want_mail: bool, want_caldav: bool, have_mail: bool, have_caldav: bool,
) -> None:
    """Drive each axis that doesn't already match `want_*` to the desired
    end-state through the REAL admin Mail / Calendar settings toggle — the
    production path a user takes to change deployment-wide enablement after
    onboarding, now that there is no onboarding-time checkbox for it."""
    if want_mail != have_mail:
        _toggle_axis_until_settled(
            admin_secret_hex, nest_url,
            navigate=app.admin.navigate_mail, toggle=app.admin.toggle_mail_enabled,
            config_key="mail_enabled", want=want_mail,
        )
    if want_caldav != have_caldav:
        _toggle_axis_until_settled(
            admin_secret_hex, nest_url,
            navigate=app.admin.navigate_calendar, toggle=app.admin.toggle_caldav_enabled,
            config_key="caldav_enabled", want=want_caldav,
        )


def onboard_and_enable(
    app, nest, *, node_url: str, enable_mail: bool, enable_caldav: bool
) -> dict:
    """Full UI onboarding-with-CalDAV against a fresh unclaimed `nest`, for one
    matrix cell. Returns ``{password, handle, domain, caldav_default}``.

    `node_url` is the address the APP dials — the claim and both session
    re-asserts below use it — while every harness-side admin WS-RPC call stays
    on `nest.nest_url` (see `claim_through_onboarding`).

    Steps:
      1. `claim_through_onboarding` (import-key → handle `test@<domain>` →
         claim-code → nat_mode_choice, dismissed → LoggedIn — the post-claim
         launch glue fires the handle-derived mail/caldav defaults);
      2. (re-)assert the admin session so the admin shell is reachable;
      3. once the post-claim step's completion anchor passes, read the derived
         mail/caldav state (`caldav_default` — the
         handle-derived default the caller asserts against, mirroring the old
         checkbox-default read, now taken post-hoc since there's no longer an
         onboarding-time checkbox to read it from);
      4. for any axis the cell wants that doesn't already match the derived
         default, flip it through the REAL admin Mail / Calendar settings
         toggle (`_ensure_enablement`) — there is no onboarding-time
         independent-enablement choice any more, so reaching a non-default
         cell now goes through the same settings surface a real user would
         use after onboarding;
      5. provision the serving bits the UI can't express (TLS/cert/spam);
      6. bring the self-enrolled MTA + MDA to serving — they AUTO-APPROVE on
         enrollment-poll once the subsystem is enabled (loopback onboarding
         auto-approval), so the admin-bridges-pending click is best-effort and
         the real gate is the serving-wait (`wait_tcp_accept` / the caller's
         `wait_caldav_serving`);
      7. if mail or caldav is enabled, ADD a known-password PLAIN credential
         (`add_credential_plain`) — onboarding already minted an auto-generated
         `default` we can't authenticate with — and return `password` plus the
         RFC-5233 sub-addressed MUA username (`<handle>+mua@<domain>`) in the
         `handle` field; else `password=None` and `handle` is the bare handle.
    """
    # ── 1. Claim all the way to LoggedIn (nat_mode_choice dismissed).
    pre = claim_through_onboarding(app, nest, node_url=node_url)
    admin_secret_hex = pre["admin_secret_hex"]
    handle = pre["handle"]
    domain = pre["domain"]
    # The auth user-id (port-stripped per RFC 7617); overwritten with the
    # `+credential` form below when a MUA credential is added.
    mua_username = _auth_username(handle)

    # ── 2. (Re-)assert the admin session so the admin shell is built (the same
    # established admin-e2e nudge `claim_enable_and_ready` uses).
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": admin_secret_hex,
        },
        "nav": {"stack": [{"view": "admin"}]},
    })
    app.driver.wait_for("admin-dashboard-heading", timeout=20.0)

    # ── 3. Read the handle-derived defaults once the post-claim step is done.
    mail_derived, caldav_derived = _derived_enablement(app, admin_secret_hex, nest.nest_url)
    caldav_default = caldav_derived

    # ── 4. Drive each axis to the cell's desired end-state via the admin
    # Mail / Calendar toggle wherever the derived default doesn't match.
    _ensure_enablement(
        app, admin_secret_hex, nest.nest_url,
        want_mail=enable_mail, want_caldav=enable_caldav,
        have_mail=mail_derived, have_caldav=caldav_derived,
    )

    # ── 5. Provision the serving bits the UI can't express for a test domain.
    _provision_serving_bits(
        app, nest, admin_secret_hex,
        enable_mail=enable_mail, enable_caldav=enable_caldav,
    )

    # ── 6. Bring the self-enrolled bridge(s) to APPROVED + serving.
    #
    # The nest's `request_enrollment` handler AUTO-APPROVES a loopback bridge once
    # the deployment subsystem it serves is enabled (`mail-bridge-lifecycle.md`
    # § Onboarding auto-approval; bridge_blob_handlers.rs `request_enrollment`):
    # the MTA auto-approves iff `mail_enabled`, the MDA iff
    # `mail_enabled || caldav_enabled`. Because step 4 already settled those
    # subsystems to this cell's desired state (via the derived default or the
    # admin toggle) BEFORE the bridges' next enrollment poll, both bridges
    # self-heal `pending → approved` on poll — the pending card vanishes
    # before any admin click. So the manual `admin-bridges-pending` approval is
    # BEST-EFFORT (it covers a bridge that's still pending when we look — e.g. a
    # cell with neither axis enabled, or a slow poll); a `False` return just means
    # "already auto-approved, nothing to click". The real success gate is the
    # bridge SERVING, which `wait_tcp_accept` (MTA port 25) and the caller's
    # `wait_caldav_serving` (MDA CalDAV) assert — not the click.
    domainless = _is_domainless_locator(domain)
    if enable_mail and not domainless:
        _approve_pending_bridge(app, nest.mta.ed25519_pubkey.hex())
        assert wait_tcp_accept(nest.mx_port, time.monotonic() + 45.0), (
            f"the MTA {nest.mta.ed25519_pubkey.hex()[:16]}… never bound its port-25 "
            f"listener on {nest.mx_port} after enable+approve (auto-approve or "
            f"manual) (error: {app.error_text()!r}; bridge log: "
            f"{nest.mta.log_file_path})"
        )
    # A domainless / bare-IP / localhost nest with mail enabled does NOT serve the
    # MTA: external mail genuinely needs a domain, so the MTA keeps its own
    # no-domain idle gate (any-locator design 2026-06-18 § 6 — decided: CalDAV+IMAP
    # serve, the MTA idles, don't assert it). The local read surface is the MDA's
    # IMAP, and the matrix's round-trip exercises only CalDAV (served by the MDA off
    # the floor cert), so the MTA's absence here is correct, not a failure — skip
    # both its approval and its port-25 serving-wait.
    if enable_caldav:
        # The MDA's serving is asserted by the caller's `wait_caldav_serving`
        # against `nest.caldav_port`; here we only nudge a still-pending card.
        _approve_pending_bridge(app, nest.mda.ed25519_pubkey.hex())

    # ── 7. Mint the shared MUA credential (one credential serves both IMAP/SMTP
    # and CalDAV AUTH). Only when something is enabled (a no-enablement cell has
    # nothing to authenticate).
    password = None
    if enable_mail or enable_caldav:
        # Re-assert the session on a non-admin view so the mail-settings page is
        # reachable (the account cache was populated by the claim's silent
        # challenge).
        app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": admin_secret_hex,
            },
            "nav": {"stack": [{"view": "conversations"}]},
        })
        time.sleep(2.0)
        actor_has_mailbox = mail_derived or caldav_derived
        if actor_has_mailbox:
            # The post-claim launch glue ALREADY minted the actor's mailbox +
            # MSEK + a `default` credential the moment LoggedIn was reached
            # with a handle-derived mail-enable signal (`onboarding/mod.rs
            # provision_mail_at_first_setup`) — but with an AUTO-GENERATED
            # password we never captured, so we can't authenticate the MUA as
            # the bare `<handle>@<domain>` (the MSEK wrapped under that
            # unknown secret → "unwrap_mls_blob: AEAD verify failed"). So we
            # ADD a SECOND PLAIN credential with a KNOWN password
            # (`add_credential_plain`, auto-generate OFF, typed) — the
            # realistic "I added a MUA app-password in the client" gesture —
            # and the MUA authenticates with the RFC-5233 sub-addressed
            # username `<handle>+<credential_id>@<domain>` (mail-credentials.md
            # § MUA-username). `add_credential_plain` derives the
            # credential_id from the display name, so "Mua" → "mua".
            app.mail_settings.navigate()
            app.mail_settings.add_credential_plain(_MUA_CRED_NAME, _MUA_PASSWORD)
            assert app.mail_settings.wait_for_credential_count_at_least(2, timeout=15.0), (
                "adding the MUA PLAIN credential must grow the credential list to 2 "
                f"(default + mua), but it never reached 2 (error: {app.error_text()!r})"
            )
            mua_credential_id = _MUA_CRED_ID
        else:
            # A localhost/bare-IP handle gets NO claim-time mailbox — the
            # post-claim glue's auto-mint fires only for a real registerable
            # domain — and step 4's admin toggle is deployment-wide only (it
            # flips `fauna.bridges.set_mail_enabled`/`set_caldav_enabled`, never
            # this actor's own `fauna.state.mail` plane entry). Reaching this actor's own
            # mailbox therefore needs an explicit per-actor mint, and there is
            # no app UI for it yet on the CalDAV-only axis — the toggle-driven
            # "Enable mail" form is the only real UI mint path today
            # (`enable_caldav_mailbox_for_test`'s own doc: "the not-yet-built
            # non-admin CalDAV auto-enable policy", caldav-server.md §
            # Independent enablement). Mail (if wanted) mints first via the
            # real UI form; CalDAV rides the sanctioned test-agent recipe
            # `mail_dedicated_nest.mint_caldav_mailbox` (the same one
            # `test_caldav_autoschedule_mailbox_less.py` uses) — additive onto
            # the same shared MSEK per mail-settings.md's "one MSEK, one
            # `default` credential" model, so calling it after `enable_mail`
            # flips `caldav_enabled` on the existing mailbox rather than
            # minting a second one.
            if enable_mail:
                app.mail_settings.navigate()
                app.mail_settings.enable_mail_plain(_MUA_PASSWORD, _MUA_CRED_NAME)
                assert app.mail_settings.wait_for_credential_count_at_least(
                    1, timeout=app.mail_settings.ENABLE_SETTLE_S
                ), (
                    "enable_mail_plain must mint the first credential, but the "
                    f"list never reached 1 (error: {app.error_text()!r})"
                )
                # `enable_dav_mailbox`'s idempotency gate (machine.rs) makes this
                # a same-MSEK flag-flip, not a second mint, whenever an MSEK
                # already exists — safe to call unconditionally.
                mua_credential_id = _MUA_CRED_ID
            else:
                # No MSEK exists yet, so `enable_caldav_mailbox_for_test`
                # (client.rs) takes the fresh-mint path — it hardcodes the
                # display name "Default", never `_MUA_CRED_NAME`, so the minted
                # credential_id is "default", not "mua".
                mua_credential_id = "default"
            if enable_caldav:
                from helpers.mail_dedicated_nest import (
                    mint_caldav_mailbox,
                    require_caldav_mailbox_mint_supported,
                )

                require_caldav_mailbox_mint_supported(app.driver)
                mint_caldav_mailbox(app.driver, password=_MUA_PASSWORD)
        password = _MUA_PASSWORD
        # The MUA username carries the credential_id as an RFC-5233 +suffix so the
        # MDA's AUTH selects the blob this known password wraps. `_auth_username`
        # also strips any `:port` from a bare-locator nest (RFC 7617 — a user-id
        # can't contain a colon).
        mua_username = _auth_username(handle, mua_credential_id)

    return {
        "password": password,
        "handle": mua_username,
        "domain": domain,
        "caldav_default": caldav_default,
    }

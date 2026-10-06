"""Authentication helpers for Fauna E2E tests."""

import atexit
import http.client
import ipaddress
import ssl
import struct
import time
import urllib.request

# Nest ports known to serve HTTPS (their always-live self-signed floor cert) on
# their API listener, rather than the usual plain-HTTP `FAUNA_INSECURE_DISABLE_TLS`
# tier_3 posture. `mark_tls_nest(port)` records one; `port_base_url(port)` (and,
# through it, `_resolve_base` here + `ws_api._base_url`) then defaults that port's
# dials to `https://` — which the WS-RPC core turns into `wss://` + CERT_NONE and
# urllib into an unverified context. The real-Mastodon interop harness marks its
# nest (`helpers.ap_nest.start_ap_nest(serve_tls=True)`), because a *public* AP
# domain (`nest.test`) forces the nest to serve its own TLS end-to-end (the boot
# guard `refuse_plain_http_for_public_domain` refuses plain HTTP there). The
# federation suite and every other tier_3 nest serve plain HTTP and stay `http://`.
_TLS_NEST_PORTS: set[int] = set()


def mark_tls_nest(port: int) -> None:
    """Record that the nest on ``port`` serves HTTPS (see ``_TLS_NEST_PORTS``).

    Also installs the floor-cert opener (below), so a module that dials the nest
    through raw ``urllib`` rather than through this file's helpers inherits the
    same posture instead of tripping over the self-signed cert.
    """
    _TLS_NEST_PORTS.add(port)
    _install_floor_cert_opener()


def unmark_tls_nest(port: int) -> None:
    """Record that the nest on ``port`` serves plain HTTP — the converse of
    ``mark_tls_nest``, for a port an earlier TLS nest held. A port outlives its
    nest (``find_free_port`` hands it out again once the nest stops), so the
    scheme must follow whichever nest serves the port now."""
    _TLS_NEST_PORTS.discard(port)


# The HOST each nest port is reached at — the other half of the authority beside
# `_TLS_NEST_PORTS`'s scheme. Every local nest is loopback, so the default holds
# for them; a live box is not, and a port-keyed dial that assumed loopback built
# `https://127.0.0.1:443` for `https://dev.example.com` (measured 2026-10-04: every
# `create_actor_and_register(port)` without a `base_url` was refused on live).
# Recorded by `conftest._as_nest_handle` from the nest's own `url`. The host is
# addressing only — never trust: `_is_floor_cert_authority` still keys on a
# loopback host, so a recorded real box stays certificate-verified.
_NEST_HOSTS: dict[int, str] = {}


def mark_nest_authority(port: int, host: str) -> None:
    """Record that the nest on ``port`` is reached at ``host`` (see ``_NEST_HOSTS``)."""
    _NEST_HOSTS[port] = host


def forget_nest_authority(port: int) -> None:
    """Drop ``port``'s recorded host, so it defaults to loopback again."""
    _NEST_HOSTS.pop(port, None)


# ── Raw urllib dials to a self-signed loopback nest ──────────────────────────
#
# `_resolve_base` below skips verification for the dials THIS file owns, but a
# test module that builds its own `urllib.request.Request` inherits nothing —
# and the resulting failure is one line of SSL text that names no nest, no mode
# and no remedy:
#
#     ssl.SSLCertVerificationError: [SSL: CERTIFICATE_VERIFY_FAILED]
#         certificate verify failed: self-signed certificate
#
# Measured 2026-08-28: ten of the fifteen `tests/api/` failures under
# `--nest docker` were exactly this, across six modules, every one of which had
# already built the right `https://` URL. So the posture is granted once, at the
# transport, rather than re-derived per module: an opener that supplies an
# unverified context for the nests the harness itself started.
#
# The rule is a CONJUNCTION, and both halves are load-bearing:
#
#   * **a loopback host** — `_LiveProvider` also declares `serve_tls`, so a real
#     box at `https://example.com` holding a real ACME cert marks its port too.
#     Keying on the port alone would stop verifying the one mode where a cert
#     failure is a genuine production incident.
#   * **a port marked by `mark_tls_nest`** — a TLS listener this harness never
#     declared as a nest (a stub MX, a fake IMAP source) keeps full verification;
#     those already pass their own contexts, and a future one that wants the
#     relaxation says so by marking its port.
#
# A dial that passes an explicit ``context=`` bypasses all of this: CPython's
# ``urlopen`` builds a fresh opener for that case, never consulting the installed
# one. That is the escape hatch every negative control needs — a test asserting
# the floor cert is NOT WebPKI-valid still gets a verifying client by asking for
# one. Pinned by `tests/test_nest_mode_axis.py`.
_opener_installed = False


def _split_authority(authority: str) -> str:
    """The host half of a ``host[:port]`` authority, brackets stripped (``[::1]``)."""
    if authority.startswith("["):
        return authority[1:].partition("]")[0]
    host, sep, _port = authority.rpartition(":")
    return host if sep else authority


def _is_loopback_host(host: str) -> bool:
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return host == "localhost"


def _is_floor_cert_authority(authority: str) -> bool:
    """True when ``authority`` names a loopback nest this harness marked as
    TLS-serving — i.e. one presenting a self-signed floor cert by construction."""
    host = _split_authority(authority)
    if not _is_loopback_host(host):
        return False
    port = authority.rpartition(":")[2]
    return port.isdigit() and int(port) in _TLS_NEST_PORTS


class _FloorCertHTTPSHandler(urllib.request.HTTPSHandler):
    """Skips verification for a marked loopback nest; verifies everything else."""

    def https_open(self, req):
        if _is_floor_cert_authority(req.host):
            return self.do_open(
                http.client.HTTPSConnection, req,
                context=ssl._create_unverified_context(),
            )
        return super().https_open(req)


def _install_floor_cert_opener() -> None:
    """Install the opener once, on the first TLS nest this run declares."""
    global _opener_installed
    if _opener_installed:
        return
    urllib.request.install_opener(
        urllib.request.build_opener(_FloorCertHTTPSHandler()),
    )
    _opener_installed = True


def ws_sslopt(url: str) -> dict | None:
    """``sslopt`` for a RAW ``websocket.create_connection`` dial, under the SAME
    rule ``_FloorCertHTTPSHandler`` applies to urllib.

    The installed opener is a urllib mechanism, so a module that reaches the
    nest over a raw WebSocket inherits nothing from it and meets the self-signed
    floor cert bare — with the same unhelpful one-liner urllib used to produce.
    Measured 2026-08-30 under ``--nest docker``: exactly one such dialer had
    survived the opener (``tests/api/test_nest_rotation_chain.py``), and it was
    the entire remaining SSL failure in that slice.

    So the fact is granted once, from the file that owns it, for BOTH transports
    — rather than each raw dialer hand-rolling ``{"cert_reqs": ssl.CERT_NONE}``
    and thereby disabling verification for every nest it will ever dial. The
    conjunction is the opener's, unchanged and load-bearing in both halves: a
    **loopback host** AND a **port marked by** ``mark_tls_nest``. A real box on a
    real ACME cert is marked too (``_LiveProvider`` declares ``serve_tls``), so
    keying on the port alone would stop verifying the one mode where a cert
    failure is a genuine production incident.

    Returns ``None`` — full verification, the ``websocket-client`` default — for
    anything else, including a plain ``ws://`` tier_3 nest. ``None`` is what
    ``create_connection(sslopt=...)`` wants for "no override", so callers pass
    this unconditionally, exactly as they already pass ``_resolve_base``'s
    context to ``urlopen``.
    """
    authority = url.partition("://")[2].partition("/")[0]
    return {"cert_reqs": ssl.CERT_NONE} if _is_floor_cert_authority(authority) else None


def upstream_tls_context(url: str) -> ssl.SSLContext | None:
    """The SSL context for a hop the harness makes **on a browser's behalf** —
    the SPA proxy's WebSocket splice (``conftest._serve_spa_proxy``) — under the
    SAME rule ``_FloorCertHTTPSHandler`` applies to urllib and ``ws_sslopt`` to a
    raw WebSocket dial.

    A browser reaches a nest only through that proxy (the raw nest sends no CORS
    headers), and the proxy's two hops used to disagree: its HTTP hop rides the
    installed opener and so already spoke TLS to a marked loopback nest, while
    its WS hop opened a bare socket and spoke plaintext into the TLS listener.
    Every docker-mode nest is ``https://`` by construction, so the disagreement
    cost the whole web column under ``--nest docker`` — every app-driven journey
    died at login, below application logging, with the nest's own log clean
    (measured 2026-09-01, the lesson-(5) shape once more). The proxy is a CORS
    shim the harness stands up in every mode; giving its second hop the posture
    its first hop already had is wiring, not a new trust decision.

    Returns ``None`` for a plain ``http://``/``ws://`` upstream — the splice stays
    a bare socket, exactly as it is on the standalone path. For ``https://``/
    ``wss://`` it is a context either way, and the CONJUNCTION decides which: a
    **loopback host** AND a **port marked by** ``mark_tls_nest`` gets the
    unverified context the floor cert needs; anything else — a real box on a real
    ACME cert included (``_LiveProvider`` marks its port too) — gets the default
    verifying one, so the one mode where a cert failure is a genuine production
    incident keeps failing.
    """
    scheme, _, rest = url.partition("://")
    if scheme not in ("https", "wss"):
        return None
    authority = rest.partition("/")[0]
    if _is_floor_cert_authority(authority):
        return ssl._create_unverified_context()
    return ssl.create_default_context()


def port_base_url(port: int, host: str | None = None) -> str:
    """Default base URL for a nest ``port``: ``https://`` iff ``mark_tls_nest`` was
    called for it (a TLS-serving nest), else ``http://``, at the host
    ``mark_nest_authority`` recorded for it (loopback by default). The one place
    the port→authority fact lives, so ``_resolve_base`` and ``ws_api._base_url``
    agree.

    An explicit ``host`` is for the handful of helpers that dial a nest by a name
    of their own; the scheme is still read from here rather than spelled by the
    caller, which is the whole point.
    """
    scheme = "https" if port in _TLS_NEST_PORTS else "http"
    host = host or _NEST_HOSTS.get(port, "127.0.0.1")
    return f"{scheme}://{host}:{port}"


def _resolve_base(port: int, base_url: str | None) -> tuple[str, ssl.SSLContext | None]:
    """Resolve the nest base URL + a urllib SSL context.

    Defaults to ``port_base_url(port)`` — plain ``http://127.0.0.1:<port>`` for the
    usual tier_3 binary nests, or ``https://`` for a nest marked via
    ``mark_tls_nest`` (the real-Mastodon harness's TLS-serving nest). A tier_4
    docker caller passes the ``https://`` base of a self-signed *deploy* nest
    explicitly. For any ``https://`` base we skip cert verification (a
    self-signed/LAN/floor cert cannot chain a public root). Returns
    ``(base_url, ssl_context_or_None)``; the context is ``None`` for http so
    callers pass it to ``urlopen`` unconditionally.
    """
    base = (base_url or port_base_url(port)).rstrip("/")
    ctx = ssl._create_unverified_context() if base.startswith("https://") else None
    return base, ctx


def make_keypair() -> tuple[str, str]:
    """Generate an Ed25519 keypair. Returns (actor_id_hex, secret_hex)."""
    from nacl.signing import SigningKey
    sk = SigningKey.generate()
    actor_id_hex = bytes(sk.verify_key).hex()
    secret_hex = bytes(sk).hex()
    return actor_id_hex, secret_hex


def claim_admin(
    port: int,
    claim_code: str,
    base_url: str | None = None,
    handle: str | None = "admin",
    mail_domain: str | None = None,
    signing_key=None,
) -> dict:
    """Claim admin on a nest using a claim code. Returns admin actor info + Bearer token.

    ``signing_key`` (a PyNaCl ``SigningKey``) re-claims with an EXISTING identity
    — the factory-reset → "re-claim (same identity)" cycle that
    ``nest.factory_reset_and_restart`` drives. When ``None`` (the default) a fresh
    admin keypair is minted, matching first-claim onboarding.

    Drives the pre-identity ``fauna.auth.claim_admin`` WS-RPC kind over the
    anonymous connection — the `POST /api/v1/claim-admin` HTTP twin was removed
    in S4d. The connection carries no bearer; the
    Ed25519 signature over ``actor_id ‖ timestamp_be`` (seconds) *is* the auth.

    ``handle`` is **required by the wire** since the claim type-promotion
    (`fauna-protocol` ``ClaimAdminRequest.handle: String`` — a handle-less admin
    is unrepresentable; a request omitting it fails to *decode* →
    ``fauna.protocol.malformed``). It must pass ``validate_handle`` (3–63 chars,
    lowercase alphanumeric or hyphens, no leading/trailing hyphen). Pass
    ``handle=None`` only to omit the field deliberately — i.e. to assert that
    very decode refusal. ``mail_domain``
    optionally auto-registers the ``@domain`` at claim so the handle is a routable
    mail address; ``None`` registers no domain. Raises ``RpcCallError`` on a
    server-side rejection (the old twin's non-200).

    The returned dict is the **one admin contract** every nest mode answers —
    `nest_instance["admin"]`, whether the nest is a local binary, a container or
    a live box. `tests/platform/docker/helpers.py::claim_admin_api` is a thin
    wrapper over this function for exactly that reason: it used to be a second
    implementation, and the two return dicts drifted until a docker-mode run
    died on `KeyError: 'actor_id_bytes'` in every mail-bridge fixture
    (2026-08-28). Pinned by `test_nest_mode_axis.py::
    test_both_claim_admin_helpers_answer_one_admin_contract`.
    """
    from nacl.signing import SigningKey
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    from common.sig_domain import claim_admin_signed_message

    sk = signing_key if signing_key is not None else SigningKey.generate()
    actor_id_hex = bytes(sk.verify_key).hex()
    timestamp = int(time.time())  # seconds; signature is over the raw value

    msg = claim_admin_signed_message(bytes(sk.verify_key), timestamp)
    sig = sk.sign(msg).signature

    payload = {
        "claim_code": claim_code,
        "actor_id": actor_id_hex,
        "signature": sig.hex(),
        "timestamp": timestamp,
    }
    if handle is not None:
        payload["handle"] = handle
    if mail_domain is not None:
        payload["mail_domain"] = mail_domain

    base, _ = _resolve_base(port, base_url)
    with WsRpcAnonClient(base) as anon:
        reply = anon.call("fauna.auth.claim_admin", payload)

    return {
        "signing_key": sk,
        "actor_id_hex": actor_id_hex,
        "actor_id_bytes": bytes(sk.verify_key),
        # The raw seed, for a caller that has to hand the identity to another
        # process (a driver's credential store, a re-claim) rather than sign
        # in-process.
        "secret_hex": sk.encode().hex(),
        "token": reply["token"],
        # `state.handle_domain()` — the primary identity the claim just
        # registered, so a domained claim's caller need not re-derive it.
        "domain": reply.get("domain"),
        # The nest hands off its deployment signing seed (64-char hex) for off-box
        # custody / total-box-loss recovery — `box-recovery.md` § Mechanism (claim
        # path). `None` from a pre-recovery nest (the field is `Option`, omitted on
        # the wire). The client custodies it after deriving + verifying it matches
        # the connection's `nest_id` (BR-2).
        "deployment_seed": reply.get("deployment_seed"),
    }


def _coerce_admin_signing_key(admin_signing_key):
    """Accept a PyNaCl ``SigningKey``, a 32-byte seed (bytes), or a hex seed
    string and return a ``SigningKey``."""
    from nacl.signing import SigningKey
    if isinstance(admin_signing_key, SigningKey):
        return admin_signing_key
    if isinstance(admin_signing_key, str):
        return SigningKey(bytes.fromhex(admin_signing_key))
    return SigningKey(bytes(admin_signing_key)[:32])


# (base_url, actor_id_hex) → one open, reused WsRpcAdminClient. The client is
# actor-agnostic (it challenge/verifies as *whatever* key it is handed), so this
# cache serves both the Admin-class convenience helpers (`register_user` + the
# `fauna.admin.folders.*` trio) and the user-class helpers (`user_create_folder`
# + the `fauna.sync.*` / `fauna.filesync.snapshot.*` callers) below — keyed by the
# signing actor, so an admin and a user against the same base get distinct cached
# connections. Each call would otherwise pay a full challenge/verify + WS-upgrade
# handshake; against a session-scoped shared nest the back-to-back handshakes
# saturate the nest's per-IP request budget (429). Caching one connection per
# (base, actor) collapses N calls to a single handshake — mirrors the same pattern
# in `tests/e2e-unified/tests/api/{conv_api,ws_api}.py`. Closed at process exit
# (and defensively re-opened if a cached socket died).
_AUTHED_CLIENTS: dict = {}


def _authed_client_for(base: str, sk):
    """Return an open, cached ``WsRpcAdminClient`` signing as key ``sk`` against
    ``base``. Reuses one connection per (base, actor) across calls. ``sk`` may be
    an admin OR a plain user key — the client authenticates as whichever actor."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    key = (base.rstrip("/"), bytes(sk.verify_key).hex())
    client = _AUTHED_CLIENTS.get(key)
    if client is None:
        client = WsRpcAdminClient(
            base, actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
        )
        client.__enter__()
        _AUTHED_CLIENTS[key] = client
    return client


def close_all_authed_ws() -> None:
    """Close every cached authed WS-RPC connection. Safe to call repeatedly."""
    for client in list(_AUTHED_CLIENTS.values()):
        try:
            client.close()
        except Exception:
            pass
    _AUTHED_CLIENTS.clear()


atexit.register(close_all_authed_ws)


def _authed_call(base: str, sk, kind: str, payload: dict):
    """Call a WS-RPC ``kind`` over the cached connection signing as ``sk``,
    retrying once on a fresh connection if the cached socket was dropped (nest
    idle-timeout / restart on the same base/port). Returns the decoded reply
    payload. The single chokepoint every convenience helper — Admin-class
    (``register_user`` + the ``fauna.admin.folders.*`` trio) and user-class
    (``user_create_folder`` + the sync/snapshot callers) — routes through, so
    the cache/retry policy lives in exactly one place. ``sk`` is an admin or a
    user signing key; the kind's own caller-class gate (server-side) decides
    whether that actor may invoke it."""
    client = _authed_client_for(base, sk)
    try:
        return client.call(kind, payload)
    except Exception:
        _AUTHED_CLIENTS.pop((base.rstrip("/"), bytes(sk.verify_key).hex()), None)
        try:
            client.close()
        except Exception:
            pass
        return _authed_client_for(base, sk).call(kind, payload)


def register_user(
    port: int,
    actor_id: str,
    *,
    base_url: str | None = None,
    admin_signing_key,
    handle: str | None = None,
    label: str = "e2e-test",
):
    """Register a user on a node via the WS-RPC admin API.

    Pass ``admin_signing_key`` (the admin's PyNaCl ``SigningKey``, a 32-byte seed,
    or a hex seed): registers over the WS-RPC ``fauna.admin.users.create`` kind via
    the shared ``WsRpcAdminClient`` (the challenge/verify handshake the admin's key
    authenticates). This is the **only** registration transport — the legacy
    ``POST /admin/api/users`` HTTP twin was ripped (Bucket-A admin
    HTTP twins → WS-RPC); the old ``admin_token`` Bearer path was deleted with the
    last token-only callers.

    The admin connection is **cached and reused** per (base, admin actor) — see
    ``_authed_client_for`` — so a register-heavy suite pays one handshake, not one
    per user, keeping the shared nest's per-IP request budget out of 429.

    Credentials are keyword-only so a stale positional ``admin_token`` caller fails
    loudly (``TypeError``) instead of silently binding the value to ``base_url``.

    ``handle`` admits the actor **under a handle**, the way the other two
    account-creation paths do (``public-mode.md`` § Registration & Identity —
    registering *is* choosing a handle). Omit it only when a test specifically
    wants the handle-less shape: such an actor cannot send email, because the
    nest's sender-handle verification has no handle to match the ``From:``
    against (``mail-app-surface.md`` § First-party client send).

    ``label`` is the display name the nest stores, and it defaults to a shared
    constant on purpose — most tests never read it. **Pass the handle whenever
    the actor must be SELECTABLE in an admin picker.** The admin guardian
    pickers are label pickers (tui ``admin/users.rs::guardian_label``, linux
    ``GuardianSelect``, android ``GuardianDropdown``): a user left at the
    default label is painted ``e2e-test`` there, so several of them are
    indistinguishable and ``select(..., handle)`` is refused as not-offered.
    A user admitted with a handle and **no** label gets ``label = handle`` from
    the nest (``create_user_with_handle``), which is the shape the pickers were
    designed around — this helper only diverges from it because a recognisable
    constant is useful when an admin screen is being watched by a human.
    """
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    body = {
        "actor_id": bytes.fromhex(actor_id),
        "tier": "free",
        "label": label,
    }
    if handle:
        body["handle"] = handle
    _authed_call(base, sk, "fauna.admin.users.create", body)


def set_cors_origins(
    port: int,
    *,
    admin_signing_key,
    origins: list[str],
    base_url: str | None = None,
):
    """Set the nest's browser-origin allow-list over the wire, as an admin's app
    does (``fauna.admin.set_cors_origins`` — the live choice surface; the
    ``--cors-origin`` boot seed is only its artifact-set default). Replaces the
    whole list.

    For a nest a browser must dial **raw** that was not started with
    ``cors_origins`` — the session ``nest_instance`` cannot be, because the SPA
    proxy whose origin it would allow is itself built on top of it. Admin-class,
    so call it after the claim; DB state, so a factory reset wipes it.
    """
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    _authed_call(base, sk, "fauna.admin.set_cors_origins", {"origins": list(origins)})


def set_registration_mode(
    port: int,
    *,
    admin_signing_key,
    mode: str = "open",
    max_free_users: int | None = None,
    base_url: str | None = None,
):
    """Set the nest's registration posture over the wire, as an admin's app does.

    ``mode`` is ``"open"`` / ``"invite_required"`` / ``"closed"``
    (``RegistrationMode::from_wire_str``); ``max_free_users`` is the nullable cap
    that rides the same kind.

    This is the harness's ONLY way to open self-service
    ``fauna.account.register``, and it is deliberately a post-boot wire call
    rather than a nest-start knob (``testing.md`` § Default app and nest mode,
    ruling (3)). The posture is a choice an admin makes in the app, so it is
    neither artifact wiring nor a start option: expressing it as one meant a
    ``[nest] registration_mode`` seed written into a rewritten ``nest.toml`` —
    configuration-file theatre (``principles.md`` § One configuration surface) —
    and, downstream, a nest mode that could not honour it excluding every
    fixture that wanted an open nest, for a knob every mode could always have
    served over the wire.

    Call it **after the claim**: the kind is Admin-class, so it needs the admin
    identity the claim mints. There is no reboot — ``apply_registration_mode_change``
    upserts the ``nest_registration_mode`` row and swaps the live
    ``AppState.registration_mode``, so the very next ``fauna.account.register``
    follows the new posture.

    ⚠ Unlike the config seed it replaces, this is DB state: a
    ``factory_reset_and_restart`` wipes it, so a fixture that resets and still
    wants an open nest must call this again after the re-claim. No fixture does
    today (checked 2026-08-29: the one module holding both an open nest and a
    reset test resets a *different*, zero-option nest).
    """
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    body = {"mode": mode}
    if max_free_users is not None:
        # Omitted rather than sent as an explicit null: the field is an
        # `Option<u64>` on the wire and every hand-written caller in the suite
        # omits it, so sending one shape from the helper and another from the
        # tests would be a difference with no reason behind it.
        body["max_free_users"] = max_free_users
    _authed_call(base, sk, "fauna.admin.set_registration_mode", body)


#: Every cap column ``fauna.admin.tiers.update`` takes, beside the tier's name.
_TIER_CAPS = (
    "max_inbox_bytes", "max_storage_bytes", "max_devices", "max_blob_size", "max_feeds",
)


# The nest's own ceiling on an admin-settable `max_devices`
# (`admin_ws_handlers::MAX_TIER_MAX_DEVICES`), the largest value that validates.
# A fixture admitting an account at a device cap that does not bind lifts to
# this, so it never has to be re-tuned as a suite registers more devices.
UNBINDING_MAX_DEVICES = 64


def set_tier_caps(
    port: int,
    *,
    admin_signing_key,
    tier: str = "free",
    base_url: str | None = None,
    **caps: int,
):
    """Raise (or lower) some of one tier's caps over the wire, as an admin's
    Settings page does (``fauna.admin.tiers.update``) — e.g.
    ``set_tier_caps(port, admin_signing_key=sk, max_devices=64)``.

    The suite needs this because a multi-tenant nest **enforces** its tier
    quotas (the standalone binary passes ``enforce_tier_quotas``; ``admin.md``
    § 2 Users owns the quota), and the shipped ``free`` seed is sized for one
    person — 2 devices, 5 feeds — while the session-scoped ``test_user``
    identity stands in for every test's account at once and accumulates both
    across a whole run *by design*. So the fixture admits it at caps that do not
    bind rather than distorting per-test intent. Lifting a cap here costs no
    coverage only because the cap is proven nest-side — which is why each one
    the fixture lifts names its proof:

    * ``max_devices`` — ``bins/fauna-nest/tests/conformance_device_tier_cap.rs``
    * ``max_feeds`` — ``bins/fauna-nest/tests/conformance_feed_tier_cap.rs``

    A cap with no such proof must not be lifted here: the fixture would become
    the only place the refusal was ever reached, and it would never be reached.

    ``tiers.update`` takes the whole row, so this reads ``tiers.list`` first and
    re-sends every cap not named unchanged — never a hand-written row, which
    would silently reset the caps a fixture or a test had already set.
    """
    unknown = sorted(set(caps) - set(_TIER_CAPS))
    if unknown:
        raise ValueError(f"not a tier cap: {unknown!r} (caps: {_TIER_CAPS!r})")
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    tiers = _authed_call(base, sk, "fauna.admin.tiers.list", {}).get("tiers", [])
    row = next((t for t in tiers if t.get("name") == tier), None)
    if row is None:
        raise AssertionError(
            f"no {tier!r} tier on this nest; got {[t.get('name') for t in tiers]!r}"
        )
    body = {k: row[k] for k in ("name", *_TIER_CAPS)}
    body.update(caps)
    _authed_call(base, sk, "fauna.admin.tiers.update", body)


def ensure_tier(
    port: int,
    *,
    admin_signing_key,
    name: str,
    base_url: str | None = None,
    like: str = "free",
    **caps: int,
):
    """Make sure a tier named ``name`` exists, as an admin's Settings page
    would create it (``fauna.admin.tiers.create``), with every cap copied from
    the ``like`` tier except those named — e.g.
    ``ensure_tier(port, admin_signing_key=sk, name="one-device", max_devices=1)``.

    Idempotent: an existing tier of that name is left exactly as it is (a
    second test in the same run, or a re-run against a kept nest, must not
    fail on "already exists" and must not silently re-cap it either). The
    complement of :func:`set_tier_caps`: that one edits the tier every test
    user shares and so may only LIFT a cap; a test that wants to **observe** a
    cap binding admits its own dedicated actor onto a tier of its own
    (:func:`set_user_tier`), leaving the shared tier alone.
    """
    unknown = sorted(set(caps) - set(_TIER_CAPS))
    if unknown:
        raise ValueError(f"not a tier cap: {unknown!r} (caps: {_TIER_CAPS!r})")
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    tiers = _authed_call(base, sk, "fauna.admin.tiers.list", {}).get("tiers", [])
    if any(t.get("name") == name for t in tiers):
        return
    template = next((t for t in tiers if t.get("name") == like), None)
    if template is None:
        raise AssertionError(
            f"no {like!r} tier to copy on this nest; got {[t.get('name') for t in tiers]!r}"
        )
    body = {k: template[k] for k in _TIER_CAPS}
    body["name"] = name
    body.update(caps)
    _authed_call(base, sk, "fauna.admin.tiers.create", body)


#: The tier a live run moves its own account onto, when the box's admin has
#: created one of this name. The harness never creates it: `tiers.create` is
#: global and has no delete to reap it with, and `free`, the alternative, is
#: every real user's tier. Absent, the account keeps the box's `free` caps.
LIVE_HARNESS_TIER = "e2e-harness"


def tier_exists(
    port: int,
    *,
    admin_signing_key,
    name: str,
    base_url: str | None = None,
) -> bool:
    """Whether this nest has a tier named ``name`` (``fauna.admin.tiers.list``,
    a read)."""
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    tiers = _authed_call(base, sk, "fauna.admin.tiers.list", {}).get("tiers", [])
    return any(t.get("name") == name for t in tiers)


def set_user_tier(
    port: int,
    actor_id: str,
    tier: str,
    *,
    admin_signing_key,
    base_url: str | None = None,
    label: str = "",
):
    """Move one user onto ``tier`` over the wire, as the admin Users page's
    tier select does (``fauna.admin.users.update``). ``actor_id`` is hex."""
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    _authed_call(
        base,
        sk,
        "fauna.admin.users.update",
        {"actor_id": bytes.fromhex(actor_id), "tier": tier, "label": label},
    )


def open_registration(nest):
    """Open self-service registration on an already-claimed nest **handle**.

    The one-liner every fixture that used to pass ``registration_open=True`` now
    calls instead, and the reason it takes the handle rather than a port is that
    the handle is the mode-agnostic thing: it carries the ``url`` the run's
    provider published (``https://`` in docker, a LAN authority under
    ``dial_host``), so a caller never has to reconstruct an authority from a port
    and guess the scheme.

    Raises ``KeyError`` on an ``unclaimed=True`` nest, which is correct and
    deliberate: the kind is Admin-class, so there is no one to sign the call
    until the claim has happened.
    """
    set_registration_mode(
        nest["port"],
        admin_signing_key=nest["admin"]["signing_key"],
        base_url=nest["url"],
    )


def create_folder(
    port: int,
    name: str,
    actor_id: str,
    *,
    admin_signing_key,
    node_cache: bool = False,
    base_url: str | None = None,
):
    """Create a folder over the WS-RPC ``fauna.admin.folders.create`` kind.

    ``actor_id`` is a lowercase hex string (64 chars); it
    rides the wire as a raw 32-byte value (the ``fauna.admin.*`` ByteBuf
    convention). A folder has no source seat: its places are named afterwards,
    each with its own flags (``add_folder_member``). Replaces the deleted
    ``POST /admin/api/file-sets`` HTTP twin. Returns the decoded reply
    (``{id, name}``).
    """
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    payload: dict = {"name": name, "actor_id": bytes.fromhex(actor_id)}
    if node_cache:
        payload["node_cache"] = node_cache
    return _authed_call(base, sk, "fauna.admin.folders.create", payload)


# A device's place in a folder is exactly its three flags (`folders.md` § the
# `role` contraction). Named triples for the shapes the suites seat, so a call
# site reads as intent rather than three bare booleans.
PLACE_SYNC = {"originates": True, "accepts": True, "applies_deletes": True}
"""Two-way place: uploads its changes, takes the folder's, mirrors deletes."""
PLACE_BACKUP = {"originates": True, "accepts": True, "applies_deletes": False}
"""Archive place: like ``PLACE_SYNC`` but a peer's delete never deletes here."""
PLACE_ORIGINATES_ONLY = {"originates": True, "accepts": False, "applies_deletes": False}
"""Upload-only place: its changes go out, nothing remote lands here."""


def add_folder_member(
    port: int,
    name: str,
    device_id: str,
    flags: dict,
    *,
    admin_signing_key,
    actor_id: str | None = None,
    base_url: str | None = None,
):
    """Seat a device in a folder with its place ``flags`` over
    ``fauna.admin.folders.add_member``.

    ``device_id`` is a hex string (rides as raw bytes). ``flags`` is the
    ``{originates, accepts, applies_deletes}`` triple — normally one of
    ``PLACE_SYNC`` / ``PLACE_BACKUP`` / ``PLACE_ORIGINATES_ONLY``. ``actor_id``
    (hex) scopes the by-name lookup to one owner — required on a multi-user nest
    where two actors own same-named sets (a bare ambiguous name errors).
    Replaces the deleted ``POST /admin/api/file-sets/{name}/members`` HTTP twin.
    """
    base, _ = _resolve_base(port, base_url)
    sk = _coerce_admin_signing_key(admin_signing_key)
    payload: dict = {
        "name": name,
        "device_id": bytes.fromhex(device_id),
        "flags": place_flags_payload(flags),
    }
    if actor_id is not None:
        payload["actor_id"] = bytes.fromhex(actor_id)
    return _authed_call(base, sk, "fauna.admin.folders.add_member", payload)


def place_flags_payload(flags: dict) -> dict:
    """The wire ``PlaceFlags`` map for a flag triple (a fresh dict, so a caller
    never aliases the shared ``PLACE_*`` constants)."""
    return {
        "originates": bool(flags["originates"]),
        "accepts": bool(flags["accepts"]),
        "applies_deletes": bool(flags["applies_deletes"]),
    }




# ── user-class (caller-scoped) file-sync helpers ───────────────────────────────
# The Admin-class helpers above set an owner / manage *another* actor's folders
# via the `fauna.admin.*` kinds. These three drive the **user-class** kinds — the
# WS-RPC successors of the deleted bearer twins `POST /api/v1/file-sets`,
# `GET /api/v1/sync/changes`, and `POST /api/v1/snapshots` — scoped on the calling
# actor itself. Each takes the user's 32-byte hex `secret_key` (the form the
# `fauna-sync` TOML configs carry) and routes through the same cached authed-WS
# connection (`_authed_call`), signed by that user. The actor must already be
# registered (every platform test calls `register_user` first) — the WS
# challenge/verify needs a known actor.


# MUST match `fauna_core::path_crypto::set_name_hash` (pinned there by
# `set_name_hash_is_the_pinned_derivation`, and again at import by
# `tests/e2e-unified/helpers/set_names.py`).
_SET_NAME_CONTEXT = "fauna.set-name.v1"


def _addressed(payload: dict, field: str = "folder") -> dict:
    """Address ``payload``'s set by hash and take the plaintext name off it —
    the harness twin of ``fauna_protocol::folders::addressed``. Since schema 114
    an app-created (sealed) set rests no plaintext name, so a by-name request
    answers ``not_found``; the nest stores ``name_hash`` for every set, so the
    hash finds a harness-created one too. A reserved ``__`` set and an unnamed
    request are left as they are, as the Rust funnel leaves them."""
    name = payload.get(field)
    if not name or name.startswith("__") or "name_hash" in payload:
        return payload
    import blake3

    out = {k: v for k, v in payload.items() if k != field}
    out["name_hash"] = blake3.blake3(
        name.encode("utf-8"), derive_key_context=_SET_NAME_CONTEXT
    ).digest()
    return out


def _user_call(port: int, secret_key: str, kind: str, payload: dict,
               base_url: str | None = None):
    """Route a user-class WS-RPC ``kind`` through the cached connection signed by
    the user ``secret_key`` (32-byte hex seed). Returns the decoded reply."""
    from nacl.signing import SigningKey
    base, _ = _resolve_base(port, base_url)
    sk = SigningKey(bytes.fromhex(secret_key))
    return _authed_call(base, sk, kind, payload)


def user_create_folder(
    port: int,
    name: str,
    *,
    secret_key: str,
    retention_policy: str | None = None,
    include_paths: list[str] | None = None,
    exclude_paths: list[str] | None = None,
    base_url: str | None = None,
):
    """Create a folder the calling user owns, **custody-first**, over the
    user-class ``fauna.folders.create`` kind (scoped on the connection actor).
    Replaces the deleted bearer ``POST /api/v1/file-sets`` twin. A folder has no
    type (``folders.md`` § Target re-model), so there is no mode to send.
    Returns the decoded reply (``{id, name, ...}``).

    Sent through ``fauna_ffi.harness_create_set`` — the shared ``create_set``
    that mints the set nonce into the owner's custody before the create — never
    a raw ``fauna.folders.create``: a set with no custody entry has its nonce
    re-minted by the owner app's launch reconcile, and every row a seed signed
    under the old one (:func:`sync_changes_record`) then stops verifying
    (``mls-group-key-material.md`` § *Custody shape of the set nonce*)."""
    import fauna_ffi

    base, _ = _resolve_base(port, base_url)
    payload: dict = {
        "name": name,
    }
    if retention_policy is not None:
        payload["retention_policy"] = retention_policy
    if include_paths is not None:
        payload["include_paths"] = include_paths
    if exclude_paths is not None:
        payload["exclude_paths"] = exclude_paths
    return fauna_ffi.harness_create_set(base, bytes.fromhex(secret_key), payload)


# ``set_folder_schedule`` (the user-class ``fauna.folders.schedule.set`` write)
# retired 2026-08-20 with phase 5 of the folders re-model: no seat reads the
# row's cadence any more — the reconcile backstop is a hard-coded constant
# (``file-sync.md`` § Config, the phase-5 block), and a test that wants a
# different one sets the engine's compile-gated ``FAUNA_E2E_RESCAN_MS`` seam on
# the seat it launches (``helpers.sync_seats.make_seat(rescan_secs=...)``). The
# kind itself stays registered nest-side for released apps
# (``conformance_folders.rs``).


def set_folder_residency(
    port: int,
    name: str,
    *,
    secret_key: str,
    residency: str,
    base_url: str | None = None,
):
    """Set an owned folder's **content residency** on its nest row, over the
    user-class ``fauna.folders.update`` kind (the seam ``folder-nest-residency-
    select`` / ``folder-residency-confirm`` drives).

    ``residency`` is ``"full"`` or ``"metadata_only"`` (``file-sync.md``
    § Content residency). Its own wire field, never folded into ``nest_place``.
    Owner-scoped on the nest (``WHERE name = ? AND actor_id = ?``), so
    ``secret_key`` must be the seed of the actor that owns ``name``. Returns the
    decoded reply; the nest answers ``not_found`` when no owned row matches, and
    refuses the value on reserved rails or against any serving surface.
    """
    return _user_call(
        port,
        secret_key,
        "fauna.folders.update",
        {"name": name, "residency": residency},
        base_url,
    )


# Sentinel distinguishing "caller said nothing" (seal through the real funnel)
# from an explicit ``None`` (register/record sealless — the keyless-writer
# shape: nameless on the device plane, the ``path_seal_required`` refusal on the
# path one).
# Defined here rather than beside `sync_changes_record` because it is a default
# argument of `sync_register` below, which Python evaluates at def time.
_SEAL_DEFAULT = object()


def sync_register(
    port: int,
    *,
    secret_key: str,
    device_id: str,
    label: str = "e2e-device",
    capabilities: str = "read,write",
    label_sealed: bytes | None = _SEAL_DEFAULT,  # type: ignore[assignment]
    base_url: str | None = None,
):
    """Register a sync device for the calling user over the user-class
    ``fauna.sync.register`` kind (scoped on the connection actor). Replaces the
    deleted bearer ``POST /api/v1/sync/register`` twin — the twin's explicit
    ``actor_id`` field is dropped (it is implicit in the authenticated
    connection, per ``fauna-protocol::sync::SyncRegisterRequest``).

    ``device_id`` is a hex-encoded 32-byte id (64 lowercase hex chars);
    the nest parses it via ``parse_device_id``. ``capabilities`` is the
    comma-separated set (``"read"``, ``"write"``, or both) the ``sync_devices``
    row stores. Returns the decoded reply (``{device_id}`` — the nest echoes the
    id it stored, the WS-RPC successor of the twin's ``{ok: true}``).

    **S9 flip (2026-08-02):** the nest rests **no plaintext label** for a
    user-chosen one (``register_sync_device`` scrubs it,
    ``bins/fauna-nest/src/db/sync_storage.rs``), so a *sealless* register rests
    no label at all and the row is nameless forever — every ``device-name``
    assertion over it then reads ``''``. This seam therefore seals through the
    **real funnel** by default (``fauna_ffi.seal_device_label`` →
    ``fauna_core::label_custody::seal_device_label``, the registering owner's
    root, salted by the raw ``device_id``), exactly as the client sync daemons do
    (``fauna-sync-engine``'s ``register_device``).

    ⚠ **This plane's degrade is the WEAK one — do not carry the path plane's
    conclusion here.** An unopenable device label renders as an **empty name on a
    KEPT row**, not a dropped one (``DevicesMachine::render_devices``), because a
    device stays actionable by ``device_id`` and hiding one the user may need to
    revoke is worse. So a missing seal breaks *name* assertions only;
    ``device-card`` counts stay right — the opposite of
    :func:`sync_changes_record`'s plane, where a bad seal takes the count to 0.

    A machine-authored label (``is_synthetic_device_label`` — ``"fauna"``,
    ``"WebDAV"``) seals to nothing by ratified
    design and registers sealless; the funnel handles that, no caller check
    needed. Pass ``label_sealed=b"..."`` to control the envelope (e.g. to assert
    the unopenable-seal degrade on purpose), or ``label_sealed=None`` explicitly
    to register sealless — the keyless-writer shape, whose row rests
    nameless."""
    if label_sealed is _SEAL_DEFAULT:
        # The real funnel, not a look-alike: a seal under a drifted root or tag
        # fails SILENTLY (as `Omit` → an empty name), never as an error.
        from fauna_ffi import seal_device_label as _seal_device_label

        label_sealed = _seal_device_label(bytes.fromhex(secret_key),
                                          bytes.fromhex(device_id), label)
    payload: dict = {
        "device_id": device_id,
        "label": label,
        "capabilities": capabilities,
    }
    if label_sealed is not None:
        payload["label_sealed"] = label_sealed
    return _user_call(port, secret_key, "fauna.sync.register", payload, base_url)


def sync_devices_list(
    port: int,
    *,
    secret_key: str,
    base_url: str | None = None,
):
    """List the calling actor's registered sync devices over the user-class
    ``fauna.sync.devices.list`` kind — the read half of :func:`sync_register`,
    and the reply every app's devices page renders.

    ⚠ **Do not assert on a row's ``label``.** Post-S9-flip the nest rests no
    plaintext label for a user-chosen one, so that field is ``''`` and the name
    lives in ``label_sealed``. Render it the way an app does::

        from fauna_ffi import open_device_label
        name = open_device_label(bytes.fromhex(secret_key),
                                 bytes.fromhex(row["device_id"]),
                                 row.get("label_sealed"), row.get("label", ""))

    The reply is ``WHERE actor_id = ?1`` nest-side, so the caller is always the
    seal's audience — the seal opens for exactly the actor who can call this."""
    return _user_call(port, secret_key, "fauna.sync.devices.list", {}, base_url)


def sync_changes_list(
    port: int,
    *,
    secret_key: str,
    folder: str | None = None,
    since: int = 0,
    device_id: str | None = None,
    base_url: str | None = None,
):
    """List a folder's recorded changes over the user-class
    ``fauna.sync.changes.list`` kind. Replaces the deleted bearer
    ``GET /api/v1/sync/changes`` / ``GET /api/v1/file-sets/{name}/changes`` twins.
    ``device_id`` (hex) excludes a device's own changes (honoured only with
    ``folder``). Returns the decoded reply (``{changes: [...]}``); each change's
    ``change_type`` is the canonical lowercase verb ``"create"`` | ``"modify"`` |
    ``"delete"`` — the value the engine emits, surfaced
    verbatim by ``changes.list`` (the deleted twin returned the same)."""
    payload: dict = {"since": since}
    if folder is not None:
        payload["folder"] = folder
        # A sealed set rests no plaintext name on the nest (`path-sealing.md`
        # § the set-name plane), so a by-name read finds no app-created set:
        # address it by hash too, as every app does (`folders::addressed`).
        # A reserved `__` set is routed by its literal name and carries none.
        if not folder.startswith("__"):
            import blake3

            payload["name_hash"] = blake3.blake3(
                folder.encode("utf-8"), derive_key_context="fauna.set-name.v1"
            ).digest()
    if device_id is not None:
        payload["device_id"] = device_id
    return _user_call(
        port, secret_key, "fauna.sync.changes.list", _addressed(payload), base_url
    )


def account_state_changes(
    port: int,
    *,
    secret_key: str,
    scope: str = "state",
    since: int = 0,
    base_url: str | None = None,
):
    """The caller's OWN account-state feed for ``scope`` over the same
    ``fauna.sync.changes.list`` kind — its sealed-state-entry arm
    (``item_class: "state-entry"``), which the nest resolves from the connection
    actor alone (``sync_handlers.rs::serve_account_state_feed``).

    ``scope`` is ``"state"`` (the delegable account plane) or ``"state-fleet"``
    (the fleet-only plane). Each change's ``entry`` is the sealed envelope,
    echoed byte-for-byte; the nest holds no key that opens it, but its first
    byte names its form — see ``is_generation_sealed``. Returns the decoded reply
    (``{changes: [...]}``)."""
    payload = {"since": since, "item_class": "state-entry", "scope": scope}
    return _user_call(port, secret_key, "fauna.sync.changes.list", payload, base_url)


SEALED_ENTRY_V2 = 2
"""The sealed account-state entry form that names its generation in the clear
(`fauna_core::account_entry_crypto::SEALED_ENTRY_V2`; the gen-0 form is 1).
Held to the Rust constant by `tests/e2e-unified/tests/test_r14_trust_seed_default.py`."""


def is_generation_sealed(change: dict) -> bool:
    """Whether an ``account_state_changes`` row is sealed under a generation TIP.

    A tip-sealed kind (``SealingEpoch::GenerationTip`` in ``merge_policy.rs``)
    seals envelope form v2, and the writer door seals one only under an
    admissible, escrow-acked tip, so a v2 envelope on the nest means a tip
    resolved for that account. A non-empty ``state-fleet`` page does NOT: the
    generation machinery itself (device set, escrow target, mint, wraps,
    receipt) is gen-0 sealed, form v1, and fills that plane on an account's
    first pass whether or not any tip ever resolves."""
    entry = change.get("entry")
    if isinstance(entry, (bytes, bytearray, list)) and len(entry) > 0:
        return entry[0] == SEALED_ENTRY_V2
    return False


def media_list(
    port: int,
    *,
    secret_key: str,
    limit: int = 0,
    cursor: str | None = None,
    cursor_version: int = 2,
    base_url: str | None = None,
):
    """One keyset page of the caller's cross-set media over the user-class
    ``fauna.media.list`` kind. Returns the decoded reply (``{items: [...],
    next_cursor?, cursor_version}``).

    Works for every folder mode: since the phase 3 head unification
    (2026-08-17, ``file-sync.md`` § Membership → *Target state — head
    unification*) every ordinary folder records into the same
    ``sync_changes`` head feed as Sync sets, and ``fauna.media.list`` reads
    the one ``get_files_for_folder`` projection
    (``bins/fauna-nest/src/media_handlers.rs``). Only reserved ``__*``
    destination sets still live in ``backup_custody`` — and those never
    surface in Media at all.

    ⚠ ``cursor`` is opaque and nest-sealed: replay a prior reply's
    ``next_cursor`` verbatim, never construct or edit one.
    """
    # `cursor_version` is required on the wire; 2 (the hash order) is the only
    # order a nest serves.
    payload: dict = {"limit": limit, "cursor_version": cursor_version}
    if cursor is not None:
        payload["cursor"] = cursor
    return _user_call(port, secret_key, "fauna.media.list", payload, base_url)


def sealed_path(secret_key: str, path: str) -> bytes:
    """The ``path_sealed`` envelope a writer holding ``secret_key`` produces for
    ``path`` — the **read-side** key for asserting on recorded changes.

    Since the S9 flip a ``fauna.sync.changes.list`` row rests ``path: None``; the
    label is the ``(path_sealed, path_hash)`` pair. A test that still compares
    ``c["path"] == "notes.txt"`` silently matches nothing — which reads as "the
    daemon never recorded it" and sends the reader hunting a sync bug that is not
    there (it cost four platform/sync suites exactly that, 2026-08-02).

    The seal is **convergent** — a pure function of (root, path) — so sealing the
    path you expect gives a total, byte-exact match against the recorded row:

        assert any(c.get("path_sealed") == sealed_path(sk, "notes.txt")
                   for c in changes)

    ``secret_key``: the account's 32-byte Ed25519 seed as hex — the daemon's
    ``secret_key`` config value. Two devices of one account seal identically
    (same root), which is what makes this work across a multi-device fixture.
    Owner-root sets only, per :func:`fauna_ffi.seal_path`.
    """
    from fauna_ffi import seal_path as _seal_path

    return _seal_path(bytes.fromhex(secret_key), path)


def sync_changes_record(
    port: int,
    *,
    secret_key: str,
    folder: str,
    device_id: str,
    path: str,
    manifest_hash: str | None = None,
    size_bytes: int = 0,
    change_type: str = "create",
    content_key_version: int | None = None,
    path_sealed: bytes | None = _SEAL_DEFAULT,  # type: ignore[assignment]
    base_url: str | None = None,
):
    """Record a file change against an owned folder over the user-class
    ``fauna.sync.changes.record`` kind — the *production* RPC the ``fauna-sync``
    daemon itself uses (``libs/fauna-sync-engine::engine``), not a test backdoor.
    This is the cross-process Python seam for seeding ``sync_changes`` rows
    without spawning a sync daemon: a ``create`` with a non-null ``manifest_hash``
    makes the file appear in ``fauna.sync.changes.list`` and the cross-set
    ``fauna.media.list`` aggregate (``get_files_for_folder`` reads
    ``change_type != 'delete' AND manifest_hash IS NOT NULL``); ``change_type``
    ``"delete"`` (or a ``None`` manifest) tombstones it.

    Prerequisites the handler enforces (``bins/fauna-nest/src/sync_handlers.rs``):
    ``device_id`` (hex) must be a **registered device holding the ``write``
    capability** (call :func:`sync_register` first) and ``folder`` must be
    **owned by the connection actor** (:func:`user_create_folder`).
    ``manifest_hash`` is any hex BLAKE3 — the media-list read path never
    dereferences it, so no real chunk/blob upload is needed (mirrors the Rust
    ``conformance_media_list`` seed). Returns the decoded reply (``{seq}`` — the
    monotonic sequence assigned; ``0`` for a custody-copy destination).

    **S9 flip (2026-08-02):** a sealless record on a sealed plane is refused
    (``path_seal_required``), and the nest now rests **no plaintext ``path``** —
    so this seam seals through the **real funnel** by default
    (``fauna_ffi.seal_path`` → ``fauna_core::label_custody::seal_path``, owner
    root), producing a row the seeding actor's own app can open and render.

    ⚠ **A synthetic envelope is not a usable default, and this is not a
    style preference.** With the plaintext column gone, a reader that cannot
    open the seal has nothing to fall back on, so the row degrades to
    ``SealedLabelRender::Omit`` — which **drops the item from the snapshot
    entirely** (``libs/fauna-media-machine/src/machine.rs``), not renders it
    nameless. Seeding synthetic blobs therefore yields rows *no app can ever
    see*: every UI assertion over them fails, count-based ones included, for a
    reason that has nothing to do with what the test is checking. (That is
    exactly what happened to the whole ``seeded_media_app`` cluster the day the
    flip landed.)

    ⚠ **Owner root** — correct for a set created via
    :func:`user_create_folder` and never bound to a folder. A *bound* set
    seals under its M2 content-key generation; an owner-rooted blob there opens
    for nobody.

    Pass ``path_sealed=b"..."`` to control the envelope (e.g. to assert the
    unopenable-seal degrade on purpose), or ``path_sealed=None`` explicitly to
    exercise the ``path_seal_required`` refusal itself.

    **Signed (writer-signed change records, ruling (4)):** the record goes out
    through ``fauna_ffi.harness_record_change`` — the set nonce read from the
    recorder's own custody, the request signed by the shared ``RecordSigning``
    — because the nest refuses an unsigned record ``signature_required``. So
    ``folder`` must have been born custody-first (:func:`user_create_folder`);
    a set with no nonce in the recorder's custody raises here, before sending."""
    import fauna_ffi

    if path_sealed is _SEAL_DEFAULT:
        # The real funnel, not a look-alike: a seed sealed under a drifted root
        # or tag fails SILENTLY (as `Omit`, never as an error), so the one thing
        # this must not be is a second implementation. Convergent by
        # construction, so an idempotent re-seed writes byte-identical rows.
        from fauna_ffi import seal_path as _seal_path

        path_sealed = _seal_path(bytes.fromhex(secret_key), path)
    payload: dict = {
        "folder": folder,
        "device_id": device_id,
        "path": path,
        "size_bytes": size_bytes,
        "change_type": change_type,
    }
    if path_sealed is not None:
        payload["path_sealed"] = path_sealed
    if manifest_hash is not None:
        payload["manifest_hash"] = manifest_hash
    if content_key_version is not None:
        payload["content_key_version"] = content_key_version
    base, _ = _resolve_base(port, base_url)
    return fauna_ffi.harness_record_change(base, bytes.fromhex(secret_key), payload)


def sync_status(
    port: int,
    *,
    secret_key: str,
    folder: str,
    base_url: str | None = None,
):
    """Get a folder's sync status over the user-class ``fauna.sync.status`` kind
    (scoped on the connection actor, ownership-checked). Replaces the deleted bearer
    ``GET /api/v1/sync/status`` twin — whose handler skipped the folder ownership
    check the kind restores (api-layers.md § auth, the latent-authorization fix).
    Returns the decoded reply: ``{folder, source_online,
    destinations: [{kind, device_id, sync_mode, online}]}``. ``source_online`` is
    the folder's content-reachability verdict (``file-sync.md`` § Content
    reachability), the same one ``fauna.media.list`` stamps on each item."""
    return _user_call(
        port, secret_key, "fauna.sync.status", _addressed({"folder": folder}), base_url
    )


def sync_conflicts_list(
    port: int,
    *,
    secret_key: str,
    include_resolved: bool = False,
    base_url: str | None = None,
):
    """List the calling actor's sync conflicts over the user-class
    ``fauna.sync.conflicts.list`` kind. Default = unresolved only (the chooser
    surface); ``include_resolved=True`` also returns auto-resolved rows (the
    review list — file-sync.md § Conflicts, ratified 2026-07-10). Each conflict
    carries its ``candidates`` (the diverging versions)."""
    payload = {"include_resolved": True} if include_resolved else {}
    return _user_call(port, secret_key, "fauna.sync.conflicts.list", payload, base_url)


def user_folders_list(
    port: int,
    *,
    secret_key: str,
    base_url: str | None = None,
):
    """List the calling actor's folders over the user-class
    ``fauna.folders.list`` kind (scoped on the connection actor). Replaces the
    deleted bearer ``GET /api/v1/file-sets`` twin. Returns the decoded reply
    (``{folders: [...]}``); each row carries ``retention_policy`` / ``include_paths`` /
    ``exclude_paths`` — the operating config a headless ``fauna-sync`` daemon
    reflects into its row at register (file-sync.md § Control Plane Principle,
    Config reflection)."""
    return _user_call(port, secret_key, "fauna.folders.list", {}, base_url)


def user_folder_ref(
    port: int,
    name: str,
    *,
    secret_key: str,
    base_url: str | None = None,
) -> str:
    """The ``FolderRef`` wire form (``local:<id>``) of the calling actor's
    folder ``name`` on this nest — the identity an app's bind gesture takes from
    ``folder_ref_for_row``, and what the ``sync_add_location`` test command must
    carry: a folder binding is keyed by its ref alone since the name-keyed bind
    was retired (2026-09-24). Raises when no row, or more than one, wears the
    name — a name alone cannot say which set a binding means."""
    rows = [
        r
        for r in user_folders_list(port, secret_key=secret_key, base_url=base_url)[
            "folders"
        ]
        if r.get("name") == name
    ]
    if len(rows) != 1:
        raise AssertionError(
            f"expected exactly one folder named {name!r} on this nest, found {len(rows)}"
        )
    return f"local:{rows[0]['id']}"


def create_folder_snapshot(
    port: int,
    folder: str,
    *,
    secret_key: str,
    device_id: str | None = None,
    base_url: str | None = None,
):
    """Capture a point-in-time snapshot of a synced folder over the user-class
    ``fauna.filesync.snapshot.create_folder`` kind. Replaces the deleted bearer
    ``POST /api/v1/snapshots`` twin. Returns the decoded reply
    (``{id, file_count, total_bytes, ...}``).

    ``device_id`` (hex string, rides as raw bytes) attributes the snapshot to
    the capturing device — omitted, the snapshot is nest-side "unattributed"
    (``device_id: None`` in ``fauna.filesync.snapshot.get``'s reply). Every
    apple/windows single-file-restore client reads ``snapshot.deviceId`` to
    key the download's decrypt, so an unattributed snapshot silently can't be
    downloaded through that path (found via `test_download_single_file_bytes_
    roundtrip`/`test_snapshot_file_download_button_downloads_sealed_bytes`
    failing on macOS/iOS with zero error surfaced — a guard-let on a nil
    deviceId, not a rendering or FFI bug). Pass the seeding device's id here
    so seeded snapshots are attributed like a real client-created one."""
    payload: dict = {"folder": folder}
    if device_id is not None:
        payload["device_id"] = bytes.fromhex(device_id)
    return _user_call(
        port, secret_key, "fauna.filesync.snapshot.create_folder",
        _addressed(payload), base_url,
    )


def upload_single_chunk_manifest(base: str, token: str, data: bytes,
                                 ssl_ctx=None) -> str:
    """Upload ``data`` as one chunk plus its single-chunk ``ChunkManifest`` over
    the kept HTTP byte routes (``POST /api/v1/chunks`` + ``POST /api/v1/manifests``),
    returning the manifest's BLAKE3 hex.

    This is what makes a seeded file **byte-servable**: the client-side download
    walk dereferences the ``sync_changes``/``snapshot_files`` ``manifest_hash`` to
    a real manifest blob and fetches its chunks by content address — unlike the
    fake-hash seed :func:`sync_changes_record` documents for list-only reads,
    which has no manifest to fetch.

    Wire shape: the manifest is canonical dag-cbor of
    ``fauna_core::chunk::ChunkManifest`` with the optional fields omitted
    (``serde(default)`` on decode); a ``ContentHash`` rides as a tag-42 link to
    the 36-byte raw-codec CID (``01 55 1e 20 || digest``; ``cid_link``). ``cbor2.dumps(..., canonical=True)``
    is byte-identical to ``encode_canonical`` (see ``common/envelope.py``). The
    chunk POST carries no ``X-Content-Hash`` header, so the nest stores it under
    ``blake3(body)`` — exactly the plaintext hash the manifest references.
    """
    import json
    import urllib.request

    import blake3
    import cbor2

    def _post(url_path: str, body: bytes) -> dict:
        req = urllib.request.Request(
            f"{base}{url_path}", data=body, method="POST",
            headers={
                "Content-Type": "application/octet-stream",
                "Authorization": f"Bearer {token}",
            },
        )
        with urllib.request.urlopen(req, context=ssl_ctx) as resp:
            return json.loads(resp.read())

    from common.envelope import CID_RAW_PREFIX, cid_link

    chunk_digest = blake3.blake3(data).digest()
    uploaded = _post("/api/v1/chunks", data)
    assert uploaded["hash"] == chunk_digest.hex(), (
        f"chunk stored under unexpected key: {uploaded} != {chunk_digest.hex()}"
    )
    manifest = {
        "file_hash": cid_link(CID_RAW_PREFIX + chunk_digest),
        "total_size": len(data),
        "chunk_hashes": [cid_link(CID_RAW_PREFIX + chunk_digest)],
        "chunk_sizes": [len(data)],
    }
    manifest_bytes = cbor2.dumps(manifest, canonical=True)
    return _post("/api/v1/manifests", manifest_bytes)["hash"]


def seed_snapshot_with_file_bytes(
    port: int,
    *,
    secret_key: str,
    folder: str,
    path: str,
    data: bytes,
    device_id: str | None = None,
    base_url: str | None = None,
) -> dict:
    """Seed ``folder`` with one file whose bytes are REALLY downloadable, then
    snapshot it. The fastest daemon-free route to "a snapshot containing a known
    file with known bytes" for single-file-restore tests:

    1. registers a write-capable device (:func:`sync_register`),
    2. uploads the chunk + manifest blobs (:func:`upload_single_chunk_manifest`),
    3. records the path with the REAL manifest hash (:func:`sync_changes_record`),
    4. snapshots the set (:func:`create_folder_snapshot`).

    The caller creates ``folder`` first (``user_create_folder`` or the admin
    surface) — it must be owned by ``secret_key``'s actor, and must be a plain
    (non-E2E-sealed) set: the download route refuses ``stored_hashes`` manifests.
    Any ordinary mode works — since the phase 3 head unification (2026-08-17)
    folders all record into ``sync_changes``, so the
    snapshot captures them too. The helper still asserts the snapshot really
    captured the file, so a seeding mistake (e.g. a reserved ``__*`` name, or
    a record that never landed) fails loudly here, at the seed.
    Returns ``{"snapshot_id", "manifest_hash", "token", "device_id"}``.
    """
    from nacl.signing import SigningKey

    base, ssl_ctx = _resolve_base(port, base_url)
    sk = SigningKey(bytes.fromhex(secret_key))
    token = mint_token_via_handshake(base, sk)
    if device_id is None:
        import secrets as _secrets
        device_id = _secrets.token_hex(32)
    sync_register(port, secret_key=secret_key, device_id=device_id,
                  base_url=base_url)
    manifest_hash = upload_single_chunk_manifest(base, token, data, ssl_ctx)
    sync_changes_record(
        port, secret_key=secret_key, folder=folder, device_id=device_id,
        path=path, manifest_hash=manifest_hash, size_bytes=len(data),
        base_url=base_url,
    )
    reply = create_folder_snapshot(port, folder, secret_key=secret_key,
                                     device_id=device_id, base_url=base_url)
    assert reply.get("file_count", 0) >= 1, (
        f"seeded snapshot {reply.get('id')} captured 0 files — the recorded "
        f"change never reached sync_changes (every ordinary mode records "
        f"there since the 2026-08-17 head unification). Check the record "
        f"call's folder name and that the set is not a reserved '__' rail."
    )
    return {
        "snapshot_id": reply["id"],
        "manifest_hash": manifest_hash,
        "token": token,
        "device_id": device_id,
    }


def mint_token_via_handshake(base: str, sk) -> str:
    """Mint a bearer over the pre-identity WS-RPC ``fauna.auth.handshake`` kind —
    the replacement for the legacy HTTP ``POST /api/v1/auth/token`` direct-auth
    mint (``transport.md`` § Pre-identity; ``api-layers.md`` § auth). Reads the
    nest's identity off the connection first and signs the tagged, nest-bound
    handshake message (``common.sig_domain``; ``login.md`` § Binding the
    nest). The nest mints with the same side effects (lockout / private-nest
    reject). ``sk`` is a PyNaCl ``SigningKey``. Returns the opaque bearer
    string."""
    import secrets

    from clients.ws_rpc_anon_client import WsRpcAnonClient

    from common.nest_identity import read_nest_identity
    from common.sig_domain import handshake_signed_message
    actor_id_bytes = bytes(sk.verify_key)
    with WsRpcAnonClient(base) as anon:
        nest_id = read_nest_identity(anon)
        timestamp = int(time.time() * 1000)
        client_nonce = secrets.token_bytes(32)
        sig = sk.sign(
            handshake_signed_message(actor_id_bytes, timestamp, nest_id, client_nonce)
        ).signature
        payload = {
            "actor_id": actor_id_bytes.hex(),
            "timestamp": timestamp,
            "signature": sig.hex(),
            "client_nonce": client_nonce,
            "nest_id": nest_id.hex(),
        }
        reply = anon.call("fauna.auth.handshake", payload)
    return reply["token"]


def create_actor_and_register(
    port, *, base_url: str | None = None, admin_signing_key=None, with_handle: bool = True
):
    """Create a new Ed25519 keypair, register on the nest, get auth token.

    Registration uses the WS-RPC ``fauna.admin.users.create`` kind when
    ``admin_signing_key`` is given (see :func:`register_user`). ``admin_signing_key``
    is keyword-only so a stale positional ``admin_token`` caller fails loudly.

    **Pass ``admin_signing_key``.** Omitting it mints a token for an actor with no
    ``users`` row, and a handshake no longer provisions one — the auto-provision
    branch is deleted, so an unregistered actor is refused ``fauna.auth.not_registered``
    in *every* registration mode. There is no ``--no-require-registration`` escape
    hatch any more. The two admission paths a test may use:

    * **admin admits the actor** — this helper with ``admin_signing_key`` (works on
      a ``closed`` nest, which is the default posture);
    * **the actor registers itself** — :func:`register_handled_actor`, against a
      nest whose posture an admin has opened (:func:`open_registration`).
    """
    from nacl.signing import SigningKey
    sk = SigningKey.generate()
    actor_id_hex = bytes(sk.verify_key).hex()
    base, _ = _resolve_base(port, base_url)

    # Admit under a handle derived from the actor id: unique on the nest by
    # construction (so repeated calls on one shared nest never collide), and a
    # valid handle (3–63 chars, lowercase alphanumeric + hyphens, no leading or
    # trailing hyphen — `public-mode.md` § User Registration step 1).
    #
    # A handle is not cosmetic here. Without one the actor is a *handle-less*
    # account, and the nest refuses every `fauna.email.send` it makes
    # ("you must set a handle before sending email") — so every mail journey on
    # every app was testing an actor that production users are not. The apps
    # that appeared to pass were the ones resolving an EMPTY `From:`, which
    # evaded the gate rather than satisfying it.
    # `with_handle=False` opts into the handle-LESS admission on purpose — the
    # shape a test needs when it is exercising what a missing handle does
    # (e.g. the subscriptions creator column's empty-handle→hex fallback).
    handle = (
        f"e2e-{actor_id_hex[:12]}"
        if admin_signing_key is not None and with_handle
        else None
    )

    # Register via the WS-RPC admin API when we have the admin signing key.
    if admin_signing_key is not None:
        register_user(
            port, actor_id_hex, base_url=base,
            admin_signing_key=admin_signing_key,
            handle=handle,
        )

    # Mint an auth token over the WS-RPC `fauna.auth.handshake` kind (replaces
    # the retired HTTP `POST /api/v1/auth/token`).
    token = mint_token_via_handshake(base, sk)

    return {
        "signing_key": sk,
        "actor_id_hex": actor_id_hex,
        "actor_id_bytes": bytes(sk.verify_key),
        "token": token,
        "handle": handle,
    }


def register_handled_actor(
    port: int,
    handle: str,
    domain: str,
    invite_code: str | None = None,
    base_url: str | None = None,
) -> dict:
    """Provision a *handled* actor over self-service WS-RPC registration.

    Registers a fresh Ed25519 actor with ``handle`` via the pre-identity kind
    ``fauna.account.register`` (the sole register transport; ``register_core`` →
    ``create_user_with_handle``), then mints an auth token. Returns an actor dict
    shaped like ``create_actor_and_register`` (``signing_key`` / ``actor_id_hex``
    / ``actor_id_bytes`` / ``token``) plus the assigned ``handle``.

    The nest MUST have an OPEN registration posture — :func:`open_registration`
    after the claim, not a start option (arm 4) — and its resolved handle domain
    MUST equal ``domain``: the registration signature is over
    ``actor_id || handle || domain || timestamp_be`` and the nest recomputes
    ``domain`` from its own resolved handle domain, so a mismatch is a
    ``signature_failed`` reject. Three things give the nest that domain, and any
    of them will do — ``start_nest(claim_domain=domain)`` (the claim registers it
    as the primary mail domain, which IS the deployment identity), the fixture's
    own ``fauna.bridges.add_local_domain(domain)`` post-claim (the same
    registration by the admin's own door), or ``handle_domain_seed=domain`` where
    neither is available. Pick exactly one: two doors onto one domain make the
    second an idempotent no-op that silently drops its own arguments
    (``testing.md`` § Default app and nest mode, ruling (3)). This is the production-faithful, wire-only way
    to seed the *handled foreign actor* a cross-nest conversation resolves to
    (the in-process conformance test uses ``db.create_user_with_handle`` directly
    — the same DB write ``register_core`` performs).

    Distinct from ``register_self_service`` in
    ``tests/e2e-unified/tests/api/test_onboarding.py``, which maps rejects to
    legacy HTTP statuses for *negative* tests; this is the happy-path
    provisioning helper and raises on any failure.
    """
    from nacl.signing import SigningKey
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    sk = SigningKey.generate()
    actor_id_bytes = bytes(sk.verify_key)
    actor_id_hex = actor_id_bytes.hex()
    base, _ = _resolve_base(port, base_url)

    ts_ms = int(time.time() * 1000)
    # The tagged, length-prefixed register message, matching
    # account_core::register_core (common.sig_domain).
    from common.sig_domain import register_signed_message

    msg = register_signed_message(actor_id_bytes, handle, domain, ts_ms)
    sig = sk.sign(msg).signature
    body = {
        "actor_id": actor_id_hex,
        "handle": handle,
        "timestamp": ts_ms,
        "signature": sig.hex(),
    }
    if invite_code is not None:
        body["invite_code"] = invite_code
    with WsRpcAnonClient(base) as anon:
        anon.call("fauna.account.register", body)

    # Mint an auth token over the WS-RPC `fauna.auth.handshake` kind (replaces
    # the retired HTTP `POST /api/v1/auth/token`); same direct-auth contract as
    # create_actor_and_register's token step.
    token = mint_token_via_handshake(base, sk)
    return {
        "signing_key": sk,
        "actor_id_hex": actor_id_hex,
        "actor_id_bytes": actor_id_bytes,
        "token": token,
        "handle": handle,
    }


def admin_session(port: int) -> str:
    """REMOVED: admin_session() no longer works with claim-code auth.

    Use nest_info["admin"]["token"] from start_nest() instead.
    """
    raise RuntimeError(
        "admin_session() is removed. Use nest['admin']['token'] from start_nest() "
        "or call claim_admin(port, claim_code) directly."
    )

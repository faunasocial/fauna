//! Transport-agnostic public-discovery core. The single source of the
//! node-info / handle-availability / node-resolution / handle-resolution /
//! setup-status reads, served by the pre-identity WS-RPC handlers
//! (`discovery_handlers::register_discovery_handlers`). The node-info / handle /
//! resolution reads also still have HTTP twins
//! (`registration::{get_node_info,handle_available,resolve_nest,
//! resolve_handle_endpoint}`); the `setup-status` HTTP twin was removed in
//! S4c2, so `fauna.setup.status` is its sole transport.
//! Each transport is a thin adapter mapping `DiscoveryError` to its own error
//! shape (HTTP status vs. `RpcError`) and the core result struct to its reply
//! (HTTP JSON vs. the `fauna_protocol::discovery` wire type).
//!
//! These are pure reads with no side effects — preserved exactly as the HTTP
//! routes did them (see `docs/goal/architecture/api-layers.md` § public API).
//! The module is deliberately free of any `fauna_protocol` dependency so the
//! HTTP twins need not pull in the wire types; the mapping core → wire happens
//! in `discovery_handlers`. Mirrors `auth_core`. Part of the
//! WS-RPC-everywhere migration (tracked internally).
//!
//! NB this is unrelated to `crate::discovery` (the feed-contributor poller) —
//! the shared name is incidental; this is the public *bootstrap* discovery
//! surface that rides the anonymous WS connection alongside `auth_core`.

use crate::registration::validate_handle;
use crate::routes::AppState;

/// Transport-agnostic discovery failure. Adapters map each variant to an HTTP
/// status or an `RpcError` code.
#[derive(Debug)]
pub enum DiscoveryError {
    /// Handle failed the format rules (`handle_available`). HTTP 400; WS
    /// `fauna.handle.invalid`. Carries the human-readable reason.
    InvalidHandle(&'static str),
    /// Domain rejected by `resolve_nest` (bare IP / `localhost` / `.local` /
    /// `.internal` / path chars). HTTP 400; WS `fauna.nest.invalid_domain`.
    InvalidDomain(&'static str),
    /// Handle is not assigned to any actor (`resolve_handle`). HTTP 404; WS
    /// `fauna.actor.not_found`.
    HandleNotFound,
    /// A `by_handle` request named a `domain` this nest does **not** serve (not
    /// an active `local_domains` entry). WS `fauna.actor.domain_not_local`. The
    /// nest only echoes a domain it owns (multi-domain handles —
    /// `mail-multidomain.md` § Resolution).
    DomainNotLocal,
    /// Server-side failure. HTTP 500; WS `fauna.protocol.internal`.
    Internal(String),
}

// ── fauna.nest.info ─────────────────────────────────────────────────────────

/// Self-service registration policy (present only on nests with a handle domain).
pub struct NodeRegistration {
    pub tiers: Vec<String>,
    pub handle_domain: Option<String>,
}

/// Public node metadata — the body of `GET /api/v1/node-info`.
pub struct NestInfo {
    pub domain: String,
    /// The domain the web [`HostResolver`](crate::web_content::HostResolver)
    /// routes user content on — [`AppState::web_serving_domain`], verbatim,
    /// **empty string included**. This is the only value a client may compose a
    /// `<handle>.<domain>` site URL from; see
    /// [`fauna_protocol::discovery::NestInfoReply::web_serving_domain`] for the
    /// three near-miss values that each produce a dead link.
    pub web_serving_domain: String,
    /// 32-byte nest public key (adapters hex-encode).
    pub nest_id: [u8; 32],
    /// This nest's **room-read** X-Wing reception public key (adapters
    /// hex-encode) — the wrap target that makes it a readable member of the
    /// community rooms it homes
    /// (`../../docs/goal/architecture/key-material-hierarchy.md` § Audience:
    /// deployment infrastructure → *Room-read keypair*).
    ///
    /// `None` when the nest holds no deployment signing key to seal the ikm
    /// under, or when the read failed — which reads correctly as *this nest
    /// cannot be a community room's reader*, so a client must not offer to
    /// home a community room here. Deliberately never fatal to `nest.info`:
    /// this is the pre-identity surface a stranger reads to discover the nest
    /// at all, and one unreadable key must not take it off the air (the
    /// issuer JWKS's own rule, `oauth_issuer_key::public_key_set`).
    pub room_read_pubkey: Option<Vec<u8>>,
    pub version: &'static str,
    pub software: &'static str,
    pub protocols: Vec<String>,
    /// Build/version feature-capability tokens (`fauna_protocol::discovery::capability`)
    /// — which fauna-native feature-families this *build* supports, so a newer
    /// client can hide a feature an older nest predates. Distinct from `protocols`
    /// (federation/bridge protocols) and from `SetupStatusReply` runtime flags.
    pub capabilities: Vec<String>,
    pub subhandles: bool,
    pub registration: Option<NodeRegistration>,
    /// Public URL of this nest's self-hosted iroh relay (`https://relay.<domain>`),
    /// present only when the iroh relay sidecar is enabled AND this nest has a
    /// claimed, *orderable* domain — the URL is derived from the nest's claimed
    /// identity (`handle_domain()`), not any boot env (there is no `FAUNA_DOMAIN`;
    /// domains-and-tls-bootstrap.md § Env contract). `None` on a nest with no iroh
    /// relay, or a domainless/localhost/IP box (incl. a WireGuard-only relay, which
    /// advertises `RELAY` but routes via the peer registry, not a URL). The seam's
    /// client-side `RelayMode::Custom` dials this when no direct path establishes
    /// (`fauna_iroh::IrohTransport`).
    pub iroh_relay_url: Option<String>,
}

/// The build/version capability set this nest advertises on
/// `NestInfoReply.capabilities` (`docs/goal/architecture/version-compatibility.md`
/// § Dimension 3). A token is advertised iff this *build* carries the feature's
/// kind-family — the coarse version gate a newer client uses to hide a feature an
/// older nest predates (orthogonal to `protocols` = federation, and to the
/// `setup.status` runtime "admin enabled it" flags). The first four families
/// are always-compiled in this major version (the additive baseline); a future
/// within-major feature appends its token here when its kind-family lands (older
/// nests omit it → newer clients read it as unsupported, I2 backward-compat).
/// Coarse names only — no patch/build fingerprint. Bridge capabilities are NOT
/// duplicated here — clients gate bridge UIs on the server-filtered
/// `fauna.bridges.list`.
///
/// NOTE (post-quantum, goal `architecture/security/post-quantum.md` § Capability
/// negotiation): no post-quantum token is advertised. The `pq-hybrid` and
/// `mail-epoch-schedule` tokens were always-on here, so their client degrade arms
/// served only a pre-sweep nest; the 2026-09-24 ruling retired both (names never
/// reused). Clients publish their ML-KEM ek and epoch schedule unconditionally;
/// a mail recipient's seal key always carries its ek, and a subscriber or grant
/// holder is sealed X-Wing when it has published one.
///
/// NOTE (P2P relay, goal `behavior/p2p.md` § Architecture): the `capability::RELAY`
/// ("relay") token is advertised when the P2P relay backend can serve. Since the
/// WireGuard stack's deletion (2026-08-23) there is exactly ONE backend: the
/// self-hosted **iroh** relay sidecar (`bins/fauna-iroh-relay`), a runtime artifact
/// decision — the image builds it (the `relay` build feature) and always runs
/// it; the caller passes whether one is connected to this nest right now
/// ([`relay_sidecar_connected`], see [`nest_info_core`]).
///
/// The shipping image ships the iroh relay dormant, so a default production nest
/// omits `relay` → clients read it as unsupported and use the always-available
/// nest-mediated fallback. It lights up automatically once an artifact enables the
/// iroh relay sidecar. This is the artifact-decision form of the seam's substrate
/// negotiation ("a capability the nest advertises"), never a human config knob.
///
/// NOTE (paid-tier subscriptions, goal `architecture/dynamic-features.md`
/// § Wire & data shape): the `capability::SUBSCRIPTIONS` token is advertised iff
/// this build compiles the `payments` registry member. It follows the `relay`
/// shape above minus the runtime half — payments has no runtime flag, and
/// inventing one would be a knob no human ever chose (§ Product invariants).
/// A store-safe nest therefore omits the token and a full client hides its
/// paid-tier surfaces, degrading exactly as against an older nest: *a peer
/// without a capability, never a fork of the wire*.
pub fn advertised_capabilities(relay_runtime_enabled: bool) -> Vec<String> {
    capabilities_for(
        cfg!(feature = "payments"),
        relay_runtime_enabled,
        cfg!(feature = "p2p-share"),
    )
}

/// The cfg-free body of [`advertised_capabilities`] — the flavor decisions taken
/// as plain booleans so **both** answers are assertable from a single build.
///
/// This shape is load-bearing, not stylistic. `payments` is default-ON, so
/// neither arm of `nest-lib-test-check` (bare-default, then the all-features
/// union) ever compiles the excised nest: an assertion written directly against
/// `cfg!(feature = "payments")` would be executed by no gate at all. That is the
/// dark-arm class, and its ratified remedy is exactly this — name the
/// body the shipped build compiles to and test *that*
/// (`merge-gate-check.md` § Merge-gate check → *Feature scope*; the in-repo template
/// is `activitypub::outbound::ap_outbound_client_strict`).
pub fn capabilities_for(payments: bool, relay: bool, p2p_share: bool) -> Vec<String> {
    let mut caps = vec![
        fauna_protocol::discovery::capability::MAIL.to_string(),
        fauna_protocol::discovery::capability::CALENDAR.to_string(),
        fauna_protocol::discovery::capability::FILE_SYNC.to_string(),
        // Always-on: build-item 2 (opaque spam-model fetch-verbatim + fail-closed
        // server writes) is unconditionally compiled. The client-write surface
        // (1d) gates on this token before writing
        // a sealed model, so it can't double-seal a pre-build-item-2 nest into
        // data loss (see the token's doc-comment).
        fauna_protocol::discovery::capability::SPAM_MODEL_SEALED_AT_REST.to_string(),
        // Always-on: the same-account peer-leg fleet brake (wormability rule 7,
        // `account-data-plane.md` § Wormability walk). The nest carries no
        // peer-leg traffic — the token is *permission*: clients run the leg only
        // while it is advertised, and pulling it in a release is the emergency
        // stop. Advertised ahead of the first client consumer (W3 (account-data-plane.md § Workstreams)) so that
        // consumer needs no nest release to light up.
        fauna_protocol::discovery::capability::PEER_SYNC.to_string(),
        // Always-on: `subscribe` refuses a hidden tier and `tiers.create`
        // accepts the reserved-name followers create (archive-import slice 3,
        // `monetization.md` § The unifying model). The archive-import machine
        // imports non-public categories only against a nest advertising this.
        fauna_protocol::discovery::capability::HIDDEN_TIERS.to_string(),
        // Always-on for HIDDEN_TIERS' reason: `key_blob.rotate` is key
        // management, not money, so it serves in a payments-excised build too.
        // The token is the version brake the post-succession rotation leg needs
        // — that leg persists the fresh period key before uploading the blob,
        // so it must be able to tell "this nest has no such door" from "the
        // call failed" and skip cleanly rather than rotate into unreadability
        // (see the token's doc-comment).
        fauna_protocol::discovery::capability::SUBSCRIPTION_PERIOD_ROTATE.to_string(),
    ];
    // RELAY (P2P relay path): advertised iff the relay backend can actually serve.
    // Since the WireGuard stack's deletion (2026-08-23) that means iroh alone:
    // `relay` is `relay_sidecar_connected` — a relay sidecar holds its channel to
    // this nest. The nest reads that live fact (not a compile gate, not a flag)
    // because running the relay is an artifact decision the image makes, and the
    // connection is the proof it was made. A client that sees
    // `relay` gets the iroh relay URL via `NestInfoReply.iroh_relay_url`; omission
    // ⇒ unsupported.
    if relay {
        caps.push(fauna_protocol::discovery::capability::RELAY.to_string());
    }
    // SUBSCRIPTIONS: the paid-tier plane. Conditional since the nest-side
    // `payments` excision — the token means "paid tiers work here", and in a
    // store-safe build they do not: no provider config, no claim mint or
    // redemption, no tips, no purchase. The `fauna.subscriptions.*` kinds that
    // are NOT about money keep working, and a stored `asking_price` is still
    // served untouched (excision removes the ability to operate a feature, never
    // the data at rest) — but a client must not conclude from those that it can
    // sell anything here, which is exactly what an unconditional token would say.
    if payments {
        caps.push(fauna_protocol::discovery::capability::SUBSCRIPTIONS.to_string());
    }
    // P2P_SHARE: the cross-user share leg's version brake (wormability rule 7,
    // `p2p.md` § Cross-user shared-set transfer). Conditional on the build
    // compiling the `p2p-share` registry member — the `SUBSCRIPTIONS` shape, not
    // the always-on `PEER_SYNC` one, and the asymmetry is the point: the nest
    // carries no peer-LEG traffic, but it does own the share leg's fan-out
    // chokepoint (`p2p-share.member.admit`, composed at the roster write), which
    // is what holds against a non-conforming client. A nest that compiled the
    // plane away is not policing that bound, so it must not invite a client to
    // bind the listener. No evidence ⇒ the client refuses; omission is the brake.
    if p2p_share {
        caps.push(fauna_protocol::discovery::capability::P2P_SHARE.to_string());
    }
    caps
}

/// Whether this deployment runs its relay sidecar: one is connected to the nest
/// over the sidecar channel right now ([`AppState::relay_channels`]).
///
/// The image always starts the relay (`docker/s6/fauna-iroh-relay`), so in the
/// image this is true moments after boot; where no relay binary runs beside the
/// nest (a desktop-served nest, a bare binary) it is never true, and the nest
/// never advertises a relay it does not have. There is no flag behind it —
/// nothing a person, an app or an RPC can set (`p2p.md` § The relay). This gates
/// the `RELAY` capability and the `iroh_relay_url` advertisement in
/// [`nest_info_core`], the `relay.<apex>` DNS row, and the `relay.<apex>`
/// **ACME** SAN gate in `acme_http01` / `cert_nudge`.
pub(crate) fn relay_sidecar_connected(state: &AppState) -> bool {
    state
        .relay_channels
        .load(std::sync::atomic::Ordering::Relaxed)
        > 0
}

/// Whether the relay should SERVE for a nest whose claimed identity domain is
/// `apex`: only a nest with a public name of its own (`p2p.md` § The relay).
/// The same test [`iroh_relay_public_url`] and the `relay.<apex>` cert SAN gate
/// apply — the relay is handed a certificate exactly when it has a name to
/// present it under (`sidecar_channel::relay_fetch_tls_cert`).
pub(crate) fn relay_wanted(apex: &str) -> bool {
    !apex.is_empty() && fauna_core::resolve::is_public_dns_name(apex)
}

/// The public `https://relay.<apex>` URL to advertise on `fauna.nest.info`
/// ([`NestInfo::iroh_relay_url`]), or `None`.
///
/// `enabled` is [`relay_sidecar_connected`]; `apex` is the
/// nest's claimed identity domain ([`AppState::handle_domain`]). The URL is
/// advertised iff the sidecar is enabled AND the apex is *orderable* — non-empty
/// and not a local/IP host — which is **exactly** the `apex_orderable` gate
/// (`acme_http01`) that decides whether the served cert carries the `relay.<apex>`
/// SAN. Keeping the two in lock-step is load-bearing: the advertised host must
/// match a name the relay's cert is valid for. A domainless/localhost/IP box (or a
/// disabled sidecar) yields `None` — the client keeps the `relay` capability but
/// has no dialable URL, so it uses the WireGuard path or the nest-mediated
/// fallback. Pure (no `AppState`, no I/O) so it unit-tests directly.
pub(crate) fn iroh_relay_public_url(enabled: bool, apex: &str) -> Option<String> {
    (enabled && relay_wanted(apex)).then(|| format!("https://relay.{apex}"))
}

/// Read the nest's public metadata. No input, no side effects, never fails.
pub async fn nest_info_core(state: &AppState) -> NestInfo {
    // The CLAIMED identity, never the `--handle-domain` boot seed. Both this
    // block's gate and its `handle_domain` field used to read
    // `state.auth.registration.handle_domain` directly, which nothing but that
    // CLI arg ever writes (`main.rs`) — and no deployment artifact passes it
    // (`docker/s6/fauna-nest/run` execs `fauna-nest --config /data/nest.toml`;
    // `domains-and-tls-bootstrap.md` § Env contract retired `FAUNA_DOMAIN` and
    // the box learns its domain at claim). So on every real box the seed stayed
    // `None` for life and this surface — the PRE-IDENTITY one a stranger reads
    // to discover that self-service registration is open and at which domain —
    // advertised `domain: "unknown"` and no registration block at all, however
    // the admin had claimed and configured it. `setup_status_core` below
    // already carried the fix and the reason; `handle_domain()`'s own docstring
    // already named `discovery_core` as a caller that used the accessor. It
    // was not one. Same defect, same shape, as
    // `resolve_handle_reports_live_identity_domain_not_stale_seed` pins one
    // resolution boundary over.
    let identity_domain = state.handle_domain_if_set();
    let registration = if identity_domain.is_some() {
        let tiers = match state.db.list_tiers().await {
            Ok(t) => t.into_iter().map(|t| t.name).collect::<Vec<_>>(),
            Err(_) => vec!["free".to_string()],
        };
        // The posture itself is `setup.status`'s `registration_mode`; the
        // legacy boolean projection left `nest.info` with the compat-remnant
        // sweep (`version-compatibility.md` § Dimension 2).
        Some(NodeRegistration {
            tiers,
            handle_domain: identity_domain.clone(),
        })
    } else {
        None
    };

    let domain = identity_domain.unwrap_or_else(|| "unknown".to_string());

    let subhandles = *state.subhandles.read().await;

    // `mut` is used only when a bridge feature is enabled; the default
    // (no-bridge) build never mutates, so silence the unused-mut lint there.
    #[allow(unused_mut)]
    let mut protocols = vec!["fauna".to_string()];
    #[cfg(feature = "nostr")]
    protocols.push("nostr".to_string());
    #[cfg(feature = "bluesky")]
    protocols.push("bluesky".to_string());
    #[cfg(feature = "activitypub")]
    protocols.push("activitypub".to_string());

    // Build/version capability set (`docs/goal/architecture/version-compatibility.md`
    // § Dimension 3). A token is advertised iff this *build* carries the feature's
    // kind-family — the coarse version gate a newer client uses to hide a feature an
    // older nest predates (orthogonal to `protocols` = federation, and to the
    // `setup.status` runtime "admin enabled it" flags). All four families below
    // are always-compiled in this major version, so the set is the additive baseline
    // declaration; a future within-major feature appends its token here when its
    // kind-family lands (older nests omit it → newer clients read it as unsupported,
    // I2 backward-compat). Coarse names only — no patch/build fingerprint (same
    // anti-fingerprint posture as the `major.minor` version coarsening below). Bridge
    // capabilities are NOT duplicated here — clients gate bridge UIs on the
    // server-filtered `fauna.bridges.list` (cfg-gated registration = the same build
    // gate as `protocols`, plus each bridge's runtime `available` flag; the feed
    // selector reads it via `FeedManager::refresh_available_bridges`).
    //
    // The `RELAY` token is the one runtime-gated capability: advertised when the
    // iroh relay sidecar is connected (`relay_sidecar_connected`) — the only
    // backend since the WireGuard stack's deletion. Read it once and reuse it for
    // both the capability set and the relay-URL field below.
    let iroh_relay_enabled = relay_sidecar_connected(state);
    let capabilities = advertised_capabilities(iroh_relay_enabled);

    // Public iroh relay URL — derived from the nest's *claimed identity*
    // (`handle_domain()`), surfaced only when the sidecar is enabled so a client can
    // dial `RelayMode::Custom` directly. The relay is SNI-routed at `relay.<domain>`
    // on :443 (`fauna-sni-router`'s domain-agnostic `relay.*` route, WITHOUT
    // `--send-proxy-to` — D.1), so the public URL is `https://relay.<domain>`.
    // Derived here — not read from a `FAUNA_DOMAIN`-keyed boot env (retired;
    // domains-and-tls-bootstrap.md § Env contract) — so a domainless box that
    // learns its domain at claim advertises the right URL with no reboot.
    let iroh_relay_url = iroh_relay_public_url(iroh_relay_enabled, &state.handle_domain());

    // The room-read reception public key — a lookup, never a mint. The row is
    // minted at boot, before this generation serves (`room_read_key`'s module
    // docs), because this surface is anonymous and a generation that a
    // deployment-seed rotation is about to tear down still answers it with the
    // retired seed in hand. A wrapping device needs the key BEFORE the
    // membership exists, which is why it rides the pre-identity surface rather
    // than a membership-gated read.
    //
    // Every failure lands on `None` rather than propagating: `nest.info` is
    // what a stranger reads to discover this nest at all, and a nest with no
    // deployment signing key — or one whose row will not open — is honestly
    // *not able to be a community room's reader*, which is exactly what `None`
    // tells the client. Taking the whole discovery surface off the air over it
    // would be the worse answer (the issuer JWKS's rule, one plane over).
    let room_read_pubkey = match state.nest_signing_key.as_ref().map(|k| k.to_bytes()) {
        Some(seed) => {
            let db = state.db.clone();
            match tokio::task::spawn_blocking(move || {
                crate::room_read_key::public_key(&db.conn_blocking(), &seed)
            })
            .await
            {
                Ok(Ok(pk)) => Some(pk),
                Ok(Err(e)) => {
                    tracing::warn!("room-read public key unavailable for nest.info: {e}");
                    None
                }
                Err(e) => {
                    tracing::warn!("room-read public key task failed: {e}");
                    None
                }
            }
        }
        None => None,
    };

    NestInfo {
        domain,
        // Read from the SAME accessor `HostResolver` and the per-subdomain cert
        // loop key off, so the URL a client composes and the host this nest
        // answers on can never be derived from two different chains. In
        // particular this follows a post-boot domain claim (top precedence is
        // the live `identity_domain`) and reports the EMPTY string — never the
        // `handle_domain()` `"localhost"` placeholder — on a domainless box,
        // which is what tells a client to disable its copy affordances instead
        // of composing `<handle>.localhost`.
        web_serving_domain: state.web_serving_domain(),
        nest_id: state.nest_identity.public_key_bytes(),
        room_read_pubkey,
        // Coarsened to the `major.minor` line — the anonymous `nest.info` reply
        // is world-readable, and exposing the exact patch-level
        // `CARGO_PKG_VERSION` to an unauthenticated caller is a version
        // fingerprint for targeted-CVE exploitation (spec § 8.1 / `federation.md`
        // § Security). The precise version stays nest-side (the update-check
        // loop's `current_version`, startup logs); no anonymous-wire consumer
        // parses the patch level.
        version: concat!(
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR")
        ),
        software: "fauna",
        protocols,
        capabilities,
        subhandles,
        registration,
        iroh_relay_url,
    }
}

// ── fauna.handle.available ──────────────────────────────────────────────────

/// Result of a handle-availability check.
pub struct HandleAvailability {
    pub available: bool,
    pub handle: String,
    pub domain: String,
    /// `true` iff unavailable specifically because the handle is in release
    /// cooldown for a different actor.
    pub cooldown: bool,
}

/// Check whether `handle` can be claimed — the body of
/// `GET /api/v1/handle-available/{handle}`.
pub async fn handle_available_core(
    state: &AppState,
    handle: &str,
) -> Result<HandleAvailability, DiscoveryError> {
    let domain = state.handle_domain();

    validate_handle(handle).map_err(DiscoveryError::InvalidHandle)?;

    let unavailable = |cooldown: bool| HandleAvailability {
        available: false,
        handle: handle.to_string(),
        domain: domain.clone(),
        cooldown,
    };

    if state
        .auth
        .registration
        .reserved_handles
        .iter()
        .any(|r| r == handle)
    {
        return Ok(unavailable(false));
    }

    match state.db.resolve_handle(handle).await {
        Ok(Some(_)) => return Ok(unavailable(false)),
        Ok(None) => {}
        Err(e) => {
            tracing::error!("handle check error: {e}");
            return Err(DiscoveryError::Internal("internal error".into()));
        }
    }

    // Cooldown probe with a zero actor_id — tests whether ANY non-owner can claim.
    let zero_actor = [0u8; 32];
    match state.db.check_handle_cooldown(handle, &zero_actor).await {
        Ok(false) => Ok(unavailable(true)),
        _ => Ok(HandleAvailability {
            available: true,
            handle: handle.to_string(),
            domain,
            cooldown: false,
        }),
    }
}

// ── fauna.nest.resolve ──────────────────────────────────────────────────────

/// Resolve a domain to its canonical fauna node URL — the body of
/// `GET /api/v1/resolve-node/{domain}`. No state, no side effects.
pub async fn resolve_nest_core(domain: &str) -> Result<String, DiscoveryError> {
    if domain.parse::<std::net::IpAddr>().is_ok() {
        return Err(DiscoveryError::InvalidDomain("IP addresses not allowed"));
    }
    let lower = domain.to_lowercase();
    if lower == "localhost" || lower.ends_with(".local") || lower.ends_with(".internal") {
        return Err(DiscoveryError::InvalidDomain(
            "internal domains not allowed",
        ));
    }
    if lower.contains('/') || lower.contains('\\') {
        return Err(DiscoveryError::InvalidDomain("invalid domain"));
    }
    Ok(fauna_core::resolve::resolve_full_url(&format!("https://{domain}")).await)
}

// ── fauna.actor.by_handle ───────────────────────────────────────────────────

/// Result of resolving a handle to an actor.
pub struct HandleResolution {
    /// 32-byte actor public key (adapters hex-encode).
    pub actor_id: [u8; 32],
    pub handle: String,
    pub domain: String,
    pub addresses: Vec<String>,
    /// Spec Y2 reachability probe: whether the actor has ≥1 usable key package
    /// (one-time or last-resort). Folded into the anonymous `by_handle` reply.
    pub addressable: bool,
}

/// Resolve a handle to its actor ID — the body of
/// `GET /api/v1/actor/by-handle/{handle}`.
pub async fn resolve_handle_core(
    state: &AppState,
    handle: &str,
    requested_domain: Option<&str>,
) -> Result<HandleResolution, DiscoveryError> {
    // The domain reported for the resolved handle. A request that names one of
    // the deployment's active local domains (`@bob@domain2`) is echoed verbatim
    // so a client can display `bob@domain2`; a named domain this nest does NOT
    // serve is rejected. A bare lookup (no domain) reports the deployment's
    // canonical identity domain — `handle_domain()` reads the live
    // `identity_domain` cache (the projection of the primary `mail_domains` row
    // set at claim) at top precedence, so a domainless-then-claimed box reports
    // the claimed domain, not the stale `--handle-domain` boot seed. A handle is
    // addressable on every active local domain but resolves to one actor
    // (mail-multidomain.md § Multi-domain handles § Resolution).
    let domain = match requested_domain {
        Some(requested) => match state.db.lookup_active_mail_domain(requested).await {
            Ok(Some(d)) => d.domain_name,
            Ok(None) => return Err(DiscoveryError::DomainNotLocal),
            Err(e) => {
                tracing::error!("resolve handle: active-domain lookup error: {e}");
                return Err(DiscoveryError::Internal("internal error".into()));
            }
        },
        None => state.handle_domain(),
    };

    match state.db.resolve_handle(handle).await {
        Ok(Some(actor_id)) => {
            // Reachability probe: ≥1 usable key package (one-time or
            // last-resort). Non-destructive; leaks yes/no, not a count.
            let addressable = state
                .db
                .has_usable_key_package(&actor_id)
                .await
                .unwrap_or(false);
            let subhandles = *state.subhandles.read().await;
            Ok(HandleResolution {
                actor_id,
                handle: handle.to_string(),
                addresses: crate::account_core::subhandle_addresses(subhandles, handle, &domain),
                domain,
                addressable,
            })
        }
        Ok(None) => Err(DiscoveryError::HandleNotFound),
        Err(e) => {
            tracing::error!("resolve handle error: {e}");
            Err(DiscoveryError::Internal("internal error".into()))
        }
    }
}

// ── fauna.setup.status ──────────────────────────────────────────────────────

/// Setup-wizard progress — the body of `GET /api/v1/setup-status`.
pub struct SetupStatus {
    pub domain: String,
    pub dns_configured: bool,
    pub tls_active: bool,
    pub email_enabled: bool,
    pub admin_exists: bool,
    pub claimed: bool,
    /// The resolved **NAT axis** (`AppState.node_mode`: the client-set
    /// `nest_nat_mode` row falling back to the `FAUNA_MODE` config seed).
    /// Always present nest-side and on the wire. Seeds the `nat_mode_choice` wizard pre-selection
    /// (`docs/goal/behavior/onboarding.md` § 3b-bis).
    pub node_mode: crate::config::NodeMode,
    pub version: &'static str,
    /// Whether the mail subsystem's config query is healthy (see
    /// [`SetupStatusReply::mail_subsystem_ok`]).
    pub mail_subsystem_ok: bool,
    /// Deployment policy: auto-enable mail for new users (see
    /// [`SetupStatusReply::auto_enable_mail_for_new_users`]). Effective ON when
    /// the admin has never toggled it.
    pub auto_enable_mail_for_new_users: bool,
    /// Deployment policy: the live client-set registration posture (wire string)
    /// + the orthogonal free-tier ceiling.
    pub registration_mode: Option<String>,
    pub max_free_users: Option<u64>,
    /// Deployment policy: the live client-set `subhandles` gate.
    pub subhandles: bool,
    /// Deployment policy: the live "accept only signups carrying app age
    /// verification" gate (default off). See
    /// [`SetupStatusReply::age_verification_required`].
    pub age_verification_required: bool,
    /// Deployment policy: the live client-set node-wide storage cap, in bytes
    /// (`Some(v)` = cap, `None` = no limit). See
    /// [`SetupStatusReply::max_storage_bytes`].
    pub max_storage_bytes: Option<u64>,
    /// Deployment policy: the live client-set CORS allow-list (empty = the
    /// built-in default origin only). See [`SetupStatusReply::cors_origins`].
    pub cors_origins: Vec<String>,
    /// Deployment policy: the admin's chosen client-facing API serving port
    /// (the `serving_port` singleton, default 443 when unset). See
    /// [`SetupStatusReply::serving_port`]. Read straight from the DB (it is
    /// apply-on-restart, not a live `AppState` cell — the nest cannot hot-rebind
    /// its own listener).
    pub serving_port: u16,
    /// Deployment wiring: whether this nest is fronted by the SNI router
    /// ([`crate::is_fronted_by_router`]) — `true` on Docker/cloud, `false` on a
    /// direct-listener. Surfaced so the admin client renders the serving-port
    /// field read-only on a fronted box (the chosen port is inert there and a
    /// `set_serving_port` is rejected). See [`SetupStatusReply::fronted_by_router`].
    pub fronted_by_router: bool,
    /// The nest's CURRENT DKIM records (public halves), read from
    /// `mail_dkim_keys` — surfaced so a credential-less deploy-verify gate can
    /// compare the published `<selector>._domainkey.<domain>` TXT against the live
    /// signing key (see [`SetupStatusReply::dkim_records`]). Empty when DKIM is
    /// unprovisioned or the read errors.
    pub dkim_records: Vec<fauna_protocol::discovery::DkimRecord>,
    /// Host-OS maintenance: pending security updates / a pending reboot / the
    /// reboot-deferred and last-patched timestamps on the host Ubuntu box, read
    /// from the `host-status` file in the root-owned `/data/maintenance-host` `:ro`
    /// bind mount (`host_maintenance::read_host_status`). All default to the
    /// "nothing pending" state on a nest without the mount (dev / desktop /
    /// bare-metal).
    /// See [`SetupStatusReply::os_security_updates_pending`] +
    /// `installers/vps.md` § Host OS Maintenance.
    pub os_security_updates_pending: u32,
    pub os_reboot_pending: bool,
    pub os_reboot_deferred_since: Option<i64>,
    pub os_last_patched_at: Option<i64>,
    /// The admin's web-app origin choice and what `/app/` answers because of
    /// it — the shared projection `fauna.admin.web_app_origin.get` answers
    /// (`web_app_origin::projection`). See [`SetupStatusReply::web_app_origin`].
    pub web_app_origin: fauna_protocol::web_app_origin::AdminWebAppOriginGetReply,
}

/// Report setup progress for the web wizard. No input, no side effects.
pub async fn setup_status_core(state: &AppState, caller_is_admin: bool) -> SetupStatus {
    // Claim-refreshed identity domain, not the `node.domain` boot seed: a
    // provisioned box boots domainless and learns its domain at claim, so the
    // seed would report `unknown` to the setup wizard on every claimed VPS
    // until a restart.
    let domain = state
        .handle_domain_if_set()
        .unwrap_or_else(|| "unknown".to_string());
    // DB-positive claimed-state: a box is claimed iff an admin actor exists
    // (`admin_actor_ids`), NOT iff the single-use claim-code file is absent. The
    // file can linger un-deleted on an already-claimed box (a read-only cloud-init
    // mount, an `EBUSY`/transient unlink failure), which file-absence would wedge
    // at `claimed=false` forever — the box re-advertising "claimable" while every
    // claim is rejected `already_claimed`, an off-box-only-fixable brick the
    // § Client-state recoverability invariant outlaws. Keyed on an *admin* row,
    // not `has_any_user`, so a half-completed claim (a `users` row written before
    // `add_admin_actor`) still reads `claimed=false` and its retry path stays open.
    let claimed = state.db.admin_count().await.unwrap_or(0) > 0;
    // `admin_exists` conveys the *same* claimed-state information as `claimed`
    // (see `test_nest.py` "same information"), so it keys on the same DB-positive
    // `admin_count > 0` signal — NOT `has_any_user`, which would diverge in the
    // half-completed-claim window (a `users` row before `add_admin_actor`) and
    // mislead a client reading `admin_exists` literally into thinking an admin
    // exists when only a pending user row does.
    let admin_exists = claimed;

    // The resolved NAT axis — the live `AppState.node_mode` cell (populated at
    // boot by `nat_mode_core::resolve_node_mode`, swapped in place by a
    // `fauna.setup.nat_mode` commit), so a commit is visible on the very next
    // setup-status read.
    let node_mode = *state.node_mode.read().await;

    // Reflects the admin's `fauna.bridges.set_mail_enabled` toggle — the db
    // `mail_enabled` singleton, the same signal the MDA boots from. NOT a legacy
    // in-nest-SMTP domain field: the boot-time `state.email.domain` (hard-coded
    // `None` since the I6 mail-bridge cutover) has since been removed, but the old
    // derivation that keyed on it reported `false` on every production deploy even
    // with mail fully operational. DB error or never-set → `false`, matching
    // `admin_exists`'s defaulting above.
    let email_enabled = state
        .db
        .get_mail_enabled()
        .await
        .ok()
        .flatten()
        .unwrap_or(false);

    // Mail-subsystem health (memory
    // `deploy-verify-gates-vs-bridge-up`). When mail is enabled the bridge boots
    // by fetching its config, whose `SELECT` reads the `mail_domains` schema via
    // `list_active_mail_domains`. A schema break there (the
    // missing-column outage) crash-loops the bridge — no mail ports — while nest
    // still reports `health: ok`, an off-box-only brick invisible to clients.
    // Running the same query here turns that silent failure into a
    // client-visible `mail_subsystem_ok: false`, so a schema break is surfaced
    // by the setup heartbeat (gate 1), not only by probing the mail ports
    // (gates 4-5). Disabled mail has no bridge to brick → `true`. This stays on
    // WS-RPC: the static unauth `/api/v1/health` must NOT gain a DB query
    // (production-HTTP carve-out + security review § L2).
    let mail_subsystem_ok = !email_enabled || state.db.list_active_mail_domains().await.is_ok();

    // Deployment-wide "auto-enable mail for new users" policy. Unset ⇒ ON (the
    // works-out-of-box default, extended to every user — `mail-policy-config.md`
    // § Tier-2 new-user mail defaults). The client gates its first-setup
    // auto-mint on this together with `email_enabled`; the nest cannot mint the
    // mailbox itself (the MSEK is client-held — `mail-credentials.md` § MSEK
    // lifecycle), so this knob is purely a client-read deployment default.
    let auto_enable_mail_for_new_users = state
        .db
        .get_auto_enable_mail_for_new_users()
        .await
        .ok()
        .flatten()
        .unwrap_or(true);

    // Client-set `[nest]`-policy gates — the live `AppState` values (boot-resolved
    // from the `nest_{registration_mode,subhandles,max_storage_bytes,cors_origins}`
    // DB rows, else the config seeds), surfaced so the admin client reads the
    // current value back. `cors_origins` lives in an ArcSwap (read by the sync CORS
    // predicate); `.load()` + clone the snapshot for the reply.
    let (registration_mode, max_free_users) = {
        let (m, cap) = *state.registration_mode.read().await;
        (Some(m.as_wire_str().to_string()), cap)
    };
    let subhandles = *state.subhandles.read().await;
    let age_verification_required = *state.age_verification_required.read().await;
    let max_storage_bytes = *state.max_storage_bytes.read().await;
    let cors_origins = state.cors_origins.load_full().to_vec();

    // The admin's chosen client-facing serving port — the `serving_port`
    // singleton, read straight from the DB (apply-on-restart, so there is no live
    // `AppState` cell to read). Defaults to the hard-coded
    // `node_policy::DEFAULT_SERVING_PORT` (443) when the admin has never set it:
    // setup.status reports the admin's *choice* (the uniform client-facing port),
    // NOT the per-deployment internal bind seed (e.g. `3000` behind the SNI
    // router). A read error degrades to the default rather than failing the whole
    // heartbeat. See `nest/common.md` § Serving ports.
    let serving_port = state
        .db
        .get_serving_port()
        .await
        .ok()
        .flatten()
        .unwrap_or(fauna_protocol::node_policy::DEFAULT_SERVING_PORT);

    // Whether this nest is fronted by the SNI router (the Docker/cloud image sets
    // `FAUNA_FRONTED_BY_ROUTER`; a direct-listener desktop / bare-metal does not).
    // Surfaced so the admin client gates the `admin-nest-serving-port` field
    // read-only on a fronted box — there the chosen `serving_port` is inert and a
    // `set_serving_port` is rejected (`serving_port_fronted`). A pure process-global
    // (artifact-wiring IPC), not DB state — read here so every setup.status field is
    // sourced in one place. See `nest/common.md` § Serving ports.
    let fronted_by_router = crate::is_fronted_by_router();

    // The nest's current DKIM records (public halves) for the credential-less
    // deploy-verify gate-6 DNS-vs-key check (see
    // memory `dkim-publish-goes-stale-silently`). `mail_dkim_keys.public_dns_value`
    // is the authoritative value the nest signs with — what the published TXT must
    // match. Exposed anonymously like the rest of this heartbeat: it carries no
    // key material and is meant to live in public DNS (`mail-bridge-lifecycle.md`
    // § DKIM provisioning). A read error degrades to empty rather than failing the
    // whole heartbeat, matching `serving_port`/`mail_subsystem_ok` above.
    let dkim_records = state
        .db
        .list_dkim_selectors(None)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|r| fauna_protocol::discovery::DkimRecord {
                    domain: r.domain,
                    selector: r.selector,
                    public_dns_value: r.public_dns_value,
                    extra: Default::default(),
                })
                .collect()
        })
        .unwrap_or_default();

    // Host-OS maintenance state, read from the `host-status` file the host
    // reboot-coordinator writes into the root-owned `/data/maintenance-host` `:ro`
    // bind mount (a VPS box has no operator to read apt state by hand; the `:ro`
    // root-owned dir means a compromised nest can't forge it). None-gated: a nest
    // without the mount (dev / desktop / bare-metal) reads the default "nothing
    // pending" status, so version skew / a desktop nest never raises a false alarm.
    //
    // ADMIN-GATED (OS-LEAK, 2026-06-28 second-pass review): `setup.status` is a
    // fully-anonymous, unthrottled pre-identity discovery kind. Host patch/reboot
    // posture (an "unpatched kernel, reboot pending, deferred N hours" targeting
    // oracle, each paired with its `domain`) is admin-only-relevant — the doc
    // surfaces it "to the admin" on the authenticated nest page — so it must NOT
    // ride the anonymous heartbeat, which this *same* function deliberately
    // fingerprint-minimizes (version coarsened to major.minor below). Non-admin /
    // anonymous callers get the default "nothing pending" `HostStatus`; only an
    // authenticated admin gets the real values. The wire fields stay additive (no
    // break) — only their *population* is gated. See `installers/vps.md` § Host OS
    // Maintenance + `nest/common.md` § `fauna.setup.status`.
    let os = if caller_is_admin {
        crate::host_maintenance::read_host_status_from_db_path(&state.config.nest.db_path)
    } else {
        crate::host_maintenance::HostStatus::default()
    };

    SetupStatus {
        domain,
        // The nest no longer manages or tracks DNS (only the client, which holds
        // the DNS-provider keys, writes DNS). Per-record DNS status now comes
        // from `fauna.dns.verify_records` (the admin-DNS red/green surface), not
        // this lightweight wizard heartbeat.
        dns_configured: false,
        tls_active: state.tls_enabled,
        email_enabled,
        admin_exists,
        claimed,
        node_mode,
        // Coarsened to `major.minor` — `setup.status` is anonymous like
        // `nest.info`, so the same version-fingerprinting rule applies (spec
        // § 8.1 / `federation.md` § Security).
        version: concat!(
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR")
        ),
        mail_subsystem_ok,
        auto_enable_mail_for_new_users,
        registration_mode,
        max_free_users,
        subhandles,
        age_verification_required,
        max_storage_bytes,
        cors_origins,
        serving_port,
        fronted_by_router,
        dkim_records,
        os_security_updates_pending: os.security_updates_pending,
        os_reboot_pending: os.reboot_pending,
        os_reboot_deferred_since: os.reboot_deferred_since,
        os_last_patched_at: os.last_patched_at,
        web_app_origin: crate::web_app_origin::projection(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::discovery::capability;

    /// The `subscriptions` token is advertised iff the build compiles the
    /// `payments` gated-feature member — success item 4 of the nest-side
    /// excision (`dynamic-features.md` § Wire & data shape + § Wire-compat
    /// posture).
    ///
    /// Driven through [`capabilities_for`] rather than
    /// [`advertised_capabilities`] deliberately: `payments` is default-ON, so
    /// neither arm of `nest-lib-test-check` compiles the excised nest and a
    /// `cfg!`-driven assertion would be executed by no gate at all (the
    /// dark-arm class). Both directions are asserted — the negative
    /// alone would pass just as well against a token that had been deleted
    /// outright, which is a different and much worse change.
    #[test]
    fn subscriptions_capability_advertised_iff_the_payments_member_is_built() {
        for relay in [false, true] {
            let excised = capabilities_for(false, relay, true);
            assert!(
                !capability::supports(&excised, capability::SUBSCRIPTIONS),
                "a store-safe nest must NOT advertise `subscriptions`: it can mint no \
                 claim, redeem none, and take no tip, so a client that saw the token \
                 would offer paid tiers the box cannot honour"
            );
            // The degrade is the ordinary absent-token one, not an error — this
            // is the exact call a client makes, so the assertion is the client's
            // own question rather than a restatement of the list.
            assert!(
                capability::supports(&excised, capability::MAIL)
                    && capability::supports(&excised, capability::FILE_SYNC),
                "excising `payments` must not disturb any other capability — an \
                 excised build is a peer without ONE capability, never a fork"
            );

            let full = capabilities_for(true, relay, true);
            assert!(
                capability::supports(&full, capability::SUBSCRIPTIONS),
                "the default nest build ships the payments plane, so it MUST advertise \
                 `subscriptions` — without this the assertion above is vacuous and \
                 would pass against a token nobody advertises any more"
            );
        }
    }

    /// **Row 159 leg 1 — the `p2p-share` version brake** (`p2p.md`
    /// § Cross-user shared-set transfer → *Wormability walk — the share leg*
    /// rule 7): the token is advertised iff this build compiles the
    /// `p2p-share` registry member. An excised nest omits it (excision
    /// criterion 4), and a client with no evidence refuses to bind.
    ///
    /// ⚠ **This brake is NOT the `peer-sync` one, and the difference is the
    /// whole reason it is conditional.** `peer-sync` is always-on because the
    /// nest carries no peer-leg traffic at all — the token there is pure
    /// permission, so advertising it costs a store-safe build nothing. The
    /// share plane, by contrast, has a **nest-side half**: rule 8's fan-out
    /// chokepoint is composed at the roster write, so a nest that compiled the
    /// plane away cannot enforce the counterparty bound that makes the plane
    /// lawful. Advertising the token there would invite clients to bind a
    /// listener whose structural anti-Pirate-Bay bound this box is not
    /// running. So it follows `subscriptions`, not `peer-sync`.
    ///
    /// Driven through [`capabilities_for`] for the same dark-arm
    /// reason its two siblings are, and asserted in both directions: the
    /// negative alone would pass equally against a token deleted outright.
    #[test]
    fn p2p_share_brake_advertised_iff_the_share_plane_is_built() {
        for payments in [false, true] {
            for relay in [false, true] {
                let excised = capabilities_for(payments, relay, false);
                assert!(
                    !capability::supports(&excised, capability::P2P_SHARE),
                    "a nest without the share plane must NOT advertise `p2p-share` \
                     (payments={payments}, relay={relay}): rule 8's fan-out chokepoint \
                     is composed nest-side at the roster write, so this box cannot \
                     enforce the counterparty bound a bound client would assume"
                );
                assert!(
                    capability::supports(&excised, capability::PEER_SYNC),
                    "excising the SHARE plane must not disturb the same-account peer \
                     leg — they are different planes behind different brakes, and \
                     conflating them would disable device sync fleet-wide"
                );

                let full = capabilities_for(payments, relay, true);
                assert!(
                    capability::supports(&full, capability::P2P_SHARE),
                    "a build that compiles the share plane MUST advertise the token — \
                     without this the assertion above is vacuous and would pass against \
                     a token nobody advertises any more. No evidence means a client \
                     refuses to bind, so an accidental omission disables the plane"
                );
            }
        }
    }

    /// The `peer-sync` fleet brake (wormability rule 7,
    /// `account-data-plane.md` § Wormability walk): always-on baseline in
    /// every flavor — the nest carries no peer-leg traffic, the token is
    /// *permission*, and the brake is thrown by a release that stops
    /// advertising it. Pinned per-flavor through [`capabilities_for`] so the
    /// store-safe arm is executed by a real gate (the dark-arm rule the
    /// sibling test documents).
    #[test]
    fn peer_sync_fleet_brake_is_always_on_in_every_flavor() {
        for payments in [false, true] {
            for relay in [false, true] {
                // The share axis is iterated here too, not just in its own test:
                // `peer-sync` and `p2p-share` are different planes behind different
                // brakes, and this is the assertion that would catch someone
                // "simplifying" the two into one token.
                for p2p_share in [false, true] {
                    let caps = capabilities_for(payments, relay, p2p_share);
                    assert!(
                        capability::supports(&caps, capability::PEER_SYNC),
                        "the peer-sync fleet brake must be advertised in every flavor \
                         (payments={payments}, relay={relay}, p2p_share={p2p_share}) — a \
                         client runs the peer leg only while its nest advertises this, so \
                         an accidental omission silently disables device↔device sync \
                         fleet-wide"
                    );
                }
            }
        }
    }

    /// `hidden-tiers` (`monetization.md` § The unifying model → *A tier may be
    /// hidden*): this build refuses `subscribe` to a hidden tier and accepts
    /// the reserved-name followers create, so it advertises the token
    /// unconditionally — like every always-on capability.
    #[test]
    fn hidden_tiers_is_advertised_by_every_flavor() {
        for (payments, relay, p2p) in [(true, true, true), (false, false, false)] {
            let caps = capabilities_for(payments, relay, p2p);
            assert!(
                fauna_protocol::discovery::capability::supports(
                    &caps,
                    fauna_protocol::discovery::capability::HIDDEN_TIERS
                ),
                "{caps:?}"
            );
        }
    }

    /// The `relay` capability is advertised when the relay backend can serve. Since
    /// the WireGuard stack's deletion (2026-08-23) that is the iroh relay sidecar
    /// alone (a connected relay sidecar, `relay_sidecar_connected`). The
    /// always-on families are advertised regardless.
    #[test]
    fn relay_capability_advertised_iff_a_backend_can_serve() {
        // The always-on baseline is present whatever the relay state.
        for relay_runtime in [false, true] {
            let caps = advertised_capabilities(relay_runtime);
            // NB `SUBSCRIPTIONS` left this list 2026-08-10: it is conditional on
            // the `payments` member now, and is pinned in both directions by
            // `subscriptions_capability_advertised_iff_the_payments_member_is_built`
            // below.
            for always in [
                capability::MAIL,
                capability::CALENDAR,
                capability::FILE_SYNC,
                capability::SPAM_MODEL_SEALED_AT_REST,
                capability::PEER_SYNC,
            ] {
                assert!(
                    capability::supports(&caps, always),
                    "baseline capability {always:?} must always be advertised"
                );
            }
        }

        // iroh relay sidecar ON at runtime ⇒ `relay` advertised on every build.
        assert!(
            capability::supports(&advertised_capabilities(true), capability::RELAY),
            "RELAY must be advertised when the iroh relay sidecar is enabled at runtime"
        );

        // Runtime relay OFF ⇒ no backend can serve, so `relay` must be absent.
        assert!(
            !capability::supports(&advertised_capabilities(false), capability::RELAY),
            "RELAY must NOT be advertised when the iroh relay sidecar is off"
        );
    }

    /// The advertised `iroh_relay_url` is derived from the claimed identity, gated
    /// on the sidecar being enabled AND the apex being *orderable* — the same gate
    /// (`apex_orderable`) the relay cert SAN uses, so the advertised host always
    /// matches a name the relay's cert covers. A domainless/localhost/IP box (or a
    /// disabled sidecar) advertises `None`; a claimed real-domain box advertises
    /// `https://relay.<domain>`. Pins the `FAUNA_DOMAIN`-retirement derivation.
    #[test]
    fn iroh_relay_url_derives_from_claimed_orderable_apex() {
        // Disabled sidecar ⇒ None regardless of the apex.
        assert_eq!(iroh_relay_public_url(false, "nest.example.com"), None);

        // Enabled + a real orderable apex ⇒ `https://relay.<apex>`.
        assert_eq!(
            iroh_relay_public_url(true, "nest.example.com"),
            Some("https://relay.nest.example.com".to_string()),
        );

        // Enabled but a non-orderable apex ⇒ None (matches the relay cert SAN gate:
        // no `relay.<apex>` SAN is minted for these, so no URL to advertise).
        for local in ["localhost", "127.0.0.1", "box.local", ""] {
            assert_eq!(
                iroh_relay_public_url(true, local),
                None,
                "a non-orderable apex {local:?} must advertise no relay URL"
            );
        }
    }

    /// `resolve_handle_core` (the `fauna.actor.by_handle` body) must report the
    /// **live** identity domain — `handle_domain()`, the projection of the primary
    /// `mail_domains` row set at claim — not the stale `--handle-domain` boot seed.
    /// A domainless-then-claimed box has an empty seed but a populated identity
    /// cache; reading the raw `registration.handle_domain` field made it report the
    /// wrong domain for a handle it actually serves (mail-multidomain.md
    /// § Multi-domain handles). This pins the fix at the resolution boundary.
    #[tokio::test]
    async fn resolve_handle_reports_live_identity_domain_not_stale_seed() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let mut state = crate::routes::AppState::for_test(db.clone());
        // A stale CLI seed that DIFFERS from the claimed identity — so the test
        // discriminates `handle_domain()` (cache-first) from the raw seed field.
        // seed-read-ok(test): seeding the stale field IS this fixture — the
        // test exists to prove the accessor prefers the cache over it.
        state.auth.registration.handle_domain = Some("stale-seed.invalid".to_string());
        state
            .identity_domain
            .store(Some(std::sync::Arc::new("claimed.example".to_string())));
        db.create_user_with_handle(&[3u8; 32], "free", "bob", None)
            .await
            .unwrap();

        let res = resolve_handle_core(&state, "bob", None).await.unwrap();
        assert_eq!(
            res.domain, "claimed.example",
            "by_handle must report the live identity domain (handle_domain()), \
             not the stale --handle-domain seed"
        );
    }

    /// `nest_info_core` must report the **claimed** identity, not the
    /// `--handle-domain` boot seed — the same defect its `resolve_handle_core`
    /// sibling above already fixed, one discovery surface over.
    ///
    /// This is the surface a stranger reads *before* they have an account, so
    /// getting it wrong hides self-service registration on every real box: no
    /// deployment artifact passes `--handle-domain` (the image runs
    /// `fauna-nest --config /data/nest.toml`; `domains-and-tls-bootstrap.md`
    /// § Env contract retired `FAUNA_DOMAIN` and the box learns its domain at
    /// claim), so the seed is `None` for life and the whole `registration`
    /// block — the posture AND the domain to register a handle at — read as
    /// absent while `domain` read `"unknown"`. The goal doc's claim is that
    /// "discovery/registration … follow the claimed domain with no restart"
    /// (`domains-and-tls-bootstrap.md` § the identity cache), and
    /// `handle_domain()`'s own docstring already names `discovery_core` as a
    /// caller; it was not one.
    #[tokio::test]
    async fn nest_info_reports_the_claimed_identity_not_the_boot_seed() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = crate::routes::AppState::for_test(db.clone());
        // The deployment shape: no boot seed at all, the domain arrives at claim.
        // seed-read-ok(test): asserting the seed is UNSET is the precondition
        // that makes this test the real deployment shape rather than a fixture.
        assert!(
            state.auth.registration.handle_domain.is_none(),
            "precondition"
        );
        state
            .identity_domain
            .store(Some(std::sync::Arc::new("claimed.example".to_string())));

        let info = nest_info_core(&state).await;
        assert_eq!(
            info.domain, "claimed.example",
            "nest.info must report the claimed identity domain, not the boot seed"
        );
        let registration = info.registration.expect(
            "a claimed box must advertise its registration posture — the block is \
             how a stranger learns self-service is open and at which domain",
        );
        assert_eq!(
            registration.handle_domain.as_deref(),
            Some("claimed.example"),
            "registration.handle_domain must follow the claim too"
        );
    }

    /// The other half of the contract above: a genuinely domainless box (no
    /// seed, no claim) still reports `"unknown"` and NO registration block, so
    /// the fix widens nothing — an unclaimed box advertises no posture.
    #[tokio::test]
    async fn nest_info_on_a_domainless_box_still_advertises_no_registration() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = crate::routes::AppState::for_test(db.clone());

        let info = nest_info_core(&state).await;
        assert_eq!(info.domain, "unknown");
        assert!(
            info.registration.is_none(),
            "a box with no identity at all advertises no registration posture"
        );
    }

    /// `resolve_handle_core` with a `domain` qualifier that names an **active
    /// local domain** echoes that domain (so `@bob@domain2` stays `bob@domain2`);
    /// a **non-local** named domain is rejected with `DomainNotLocal`; and the
    /// bare-handle path (no qualifier) still reports the identity domain
    /// (multi-domain handles — mail-multidomain.md § Resolution).
    #[tokio::test]
    async fn resolve_handle_echoes_active_local_domain_rejects_non_local() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = crate::routes::AppState::for_test(db.clone());
        state
            .identity_domain
            .store(Some(std::sync::Arc::new("primary.example".to_string())));
        db.add_mail_domain(
            "primary.example",
            true,
            "testing",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.add_mail_domain(
            "domain2.example",
            false,
            "testing",
            "expand_primary",
            None,
            None,
        )
        .await
        .unwrap();
        db.create_user_with_handle(&[3u8; 32], "free", "bob", None)
            .await
            .unwrap();

        // Named secondary active local domain → echoed verbatim.
        let res = resolve_handle_core(&state, "bob", Some("domain2.example"))
            .await
            .unwrap();
        assert_eq!(res.domain, "domain2.example");

        // Named primary → echoed (== identity domain).
        let res = resolve_handle_core(&state, "bob", Some("primary.example"))
            .await
            .unwrap();
        assert_eq!(res.domain, "primary.example");

        // Bare lookup (no qualifier) → identity domain.
        let res = resolve_handle_core(&state, "bob", None).await.unwrap();
        assert_eq!(res.domain, "primary.example");

        // Named domain this nest does NOT serve → rejected.
        let err = resolve_handle_core(&state, "bob", Some("not-local.invalid")).await;
        assert!(matches!(err, Err(DiscoveryError::DomainNotLocal)));
    }
}

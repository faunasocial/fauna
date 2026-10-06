//! `WsNestApi` — the production [`NestApi`](super::NestApi) impl over the
//! **pre-identity (anonymous) WS-RPC connection** (`GET /api/v1/ws`,
//! `Sec-WebSocket-Protocol: fauna.v1`, no bearer; transport.md § Pre-identity).
//! The onboarding machine is the first client consumer of that connection.
//!
//! ## Why this is separate from [`WsRpcNestApi`](super::WsRpcNestApi)
//!
//! `WsRpcNestApi<R>` is the *transport-generic* mapping core (kind → wire-type
//! composition + `RpcError.code` → per-endpoint error mapping), written once for
//! every app (priority #2). It cannot itself `impl NestApi`: `NestApi` is an
//! `#[async_trait]` (boxed `+ Send` futures on native, for the `Arc<dyn NestApi>`
//! the machine holds), but [`RpcRequester::request`] is an `async fn in trait`
//! whose future is `Send` only *per concrete impl* — not provably `Send` in a
//! generic context, and return-type-notation can't bound a method with generic
//! type params. So the dyn-compatible `NestApi` boxing must live on a
//! **concrete** per-target type (the `fauna-client-dns` `RpcDnsNest` precedent):
//! native over `fauna_anon_client::AnonymousNestClient`, wasm over
//! `fauna_rpc_wasm::AnonymousWsRpcClient`. Only the `connect` call differs per
//! target ([`WsNestApi::core`]); the struct, URL resolution, and the eight
//! `NestApi` methods are written once.
//!
//! ## One fresh connection per call
//!
//! Each method opens its **own** anonymous WS connection for that single
//! request, then drops it — mirroring the per-request independence of the
//! `reqwest`-based HTTP impl this replaced (removed in S4). The anonymous
//! connector carries no reconnect supervisor (unlike the authenticated
//! `NestClient`): a connection cached across the wizard's multi-second,
//! user-paced steps (probe → claim → storage-mode commit) can silently die
//! between calls — the nest may close it once a bearer is in hand
//! (transport.md), or a blip drops it — and the next request on a dead
//! connection surfaces a spurious transient error with no auto-recovery. A fresh
//! connection per call sidesteps that entire class for what is a one-time,
//! latency-tolerant bootstrap flow. `base_url` is resolved through the same
//! `provider_base_urls["nest"]` override the former HTTP impl honored, so E2E
//! fixtures redirecting "nest" to a local server keep working — including the
//! handle-domain probe path, which passes a raw `https://{domain}` the override
//! must replace.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use super::types::*;
use super::{NestApi, WsRpcNestApi};

/// Production `NestApi` over the pre-identity WS-RPC connection. Holds only the
/// optional `provider_base_urls["nest"]` redirect (so the struct is `Send +
/// Sync` on every target); each call opens its own short-lived connection.
#[derive(Debug)]
pub struct WsNestApi {
    /// The former HTTP impl's override: when set, every `base_url` resolves
    /// to this instead (E2E redirects the "nest" key to a local server).
    base_url_override: Option<String>,
    /// (host, ip) captured once `OnboardingMachine` learns a freshly-
    /// provisioned box's static IP (`stash_provisioning_result`) — the box is
    /// reachable on it immediately, but its DNS record may not have propagated
    /// yet (Hetzner beta DNS ~30 min). When a call's resolved `base_url` targets
    /// `host`, native `core()` dials `ip:443` directly instead of system DNS —
    /// SNI/`Host`/cert-identity still come from `host` (security.md § Transport
    /// trust Axis 1: the capturing verifier's channel binding authenticates the
    /// box by identity, so this is MITM-safe). `None` (the default, and always
    /// on wasm) leaves every connect on today's system-DNS path.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    resolve_override: Arc<RwLock<Option<(String, IpAddr)>>>,
    /// (host, expected `nest_actor_id`) held while a client-provisioned box's
    /// injected deployment seed is live (provision → claim) — the Axis-2
    /// pre-resolved identity root for that host's first contact (security.md
    /// § Transport trust; design tracked internally). Written by `OnboardingMachine` when it mints/resolves the
    /// seed it injects (`run_provisioning_inner`), cleared on `reset()`. When a
    /// call's resolved `base_url` targets `host`, `core()` verifies the fresh
    /// connection against this root before issuing the real request — so every
    /// pre-claim call verifies the box's identity from first contact, no TOFU
    /// window. `None` (the default) keeps the DNS-`self=`/TOFU ladder.
    ///
    /// **Both targets consult it, with different strength.**
    /// Native graduates the full Axis-1 channel binding — the nest's signature
    /// compared against the SPKI of the cert it actually received, which defeats
    /// a relay. A browser exposes no received certificate to WASM, so the wasm
    /// arm runs the possession-only proof instead
    /// (`fauna_client_core::nest_trust::prove_first_contact_identity_possession`):
    /// the box must sign a fresh client nonce as this identity. Weaker than
    /// native and documented as such in `security.md` § Two independent axes —
    /// but it *is* consulted. It previously was not on wasm at all: the pasted
    /// `fauna://claim` identity was stored here and then ignored, a silent no-op.
    expected_nest_id: ExpectedNestIdentity,
}

impl WsNestApi {
    pub fn new(
        provider_base_urls: Option<HashMap<String, String>>,
        resolve_override: Arc<RwLock<Option<(String, IpAddr)>>>,
        expected_nest_id: ExpectedNestIdentity,
    ) -> Self {
        let base_url_override = provider_base_urls
            .as_ref()
            .and_then(|m| m.get("nest").cloned());
        Self {
            base_url_override,
            resolve_override,
            expected_nest_id,
        }
    }

    /// The effective URL to connect to: the "nest" override if present, else the
    /// caller-supplied URL.
    fn resolve(&self, caller_url: &str) -> String {
        self.base_url_override
            .clone()
            .unwrap_or_else(|| caller_url.to_string())
    }

    /// The `SocketAddr` to dial directly instead of system DNS, if a resolve
    /// override is set and its host matches `resolved_url`'s host. Port is
    /// always 443 — mirrors the established reqwest precedent
    /// (`nest_probe_client_resolving`, `machine.rs`), since a wizard
    /// `nest_url` is always bare `https://{domain}`.
    #[cfg(not(target_arch = "wasm32"))]
    fn resolve_override_addr(&self, resolved_url: &str) -> Option<std::net::SocketAddr> {
        let (host, ip) = self
            .resolve_override
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        (Self::host_of(resolved_url)? == host).then_some(std::net::SocketAddr::new(ip, 443))
    }

    /// The URL to actually **dial** when the reach override targets
    /// `resolved_url`'s host — `https://{ip}`, the browser's only way to reach a
    /// box whose domain does not resolve yet (`onboarding.md` § 6 *Reaching the
    /// box*: "the web app can override neither DNS nor TLS, so it dials
    /// `https://{ip}` literally").
    ///
    /// ⚠ The **identity** URL is unchanged and stays the argument to every
    /// host-keyed check: [`Self::expected_root_for`] is asked about
    /// `resolved_url`, never about this. Dialling the address while asserting
    /// the root against the name is the whole point — resolving the root
    /// against the IP instead would find no held root for that "host" and
    /// silently drop to TOFU, the downgrade [`Self::host_of`]'s own note warns
    /// about.
    ///
    /// The literal dial rests on the box's **IP bridge cert**
    /// (`../architecture/nest/tls-certificates.md` § B-IP); until that cert
    /// exists the browser reaches nothing, and the wizard's door is the manual
    /// exception (§ "Almost ready" surface, *web fallback*). Native does not
    /// come through here: it keeps the name and overrides the socket target
    /// ([`Self::resolve_override_addr`]).
    ///
    /// Compiled on **every** target although only the wasm `core` calls it: the
    /// rule is pure string logic, and gating it to `wasm32` would put it where
    /// no test in this workspace can execute it — the exact combination that let
    /// the missing wasm first-contact check survive 51 passes
    /// (`both_core_arms_verify_the_held_first_contact_root`, below, is the
    /// source-level pin that the wasm arm actually *calls* this).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn dial_url(&self, resolved_url: &str) -> String {
        let Some((host, ip)) = self
            .resolve_override
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return resolved_url.to_string();
        };
        if Self::host_of(resolved_url).as_deref() != Some(host.as_str()) {
            return resolved_url.to_string();
        }
        match ip {
            IpAddr::V6(v6) => format!("https://[{v6}]"),
            v4 => format!("https://{v4}"),
        }
    }

    /// The pre-resolved Axis-2 identity root for `resolved_url`, if the held
    /// injected-seed identity targets its host (the same host-match guard as
    /// [`Self::resolve_override_addr`] — the root is only ever asserted against
    /// the box it was provisioned for, never a bystander domain).
    ///
    /// Consulted on **both** targets — see the field's own doc for the
    /// native/wasm strength split.
    fn expected_root_for(&self, resolved_url: &str) -> Option<[u8; 32]> {
        let (host, id) = self
            .expected_nest_id
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        (Self::host_of(resolved_url)? == host).then_some(id)
    }

    /// The bare host of an `http(s)://` URL (scheme, userinfo, port and path
    /// stripped) — [`crate::helpers::nest_host`], the crate's one host
    /// extractor.
    ///
    /// It used to hand-roll the extraction, and the two disagreed exactly where
    /// it mattered: `split(['/', ':'])` stopped at the *first* colon, so it
    /// never looked for a `userinfo@` and returned a bare `"["` for a bracketed
    /// IPv6 URL. Both host-match guards above are `.then_some(…)`, so a
    /// mismatch silently drops the injected-seed DNS override and the
    /// pre-resolved Axis-2 identity root and falls back to system DNS +
    /// TOFU-on-host — a trust *downgrade* a crafted `nest_url` could trigger,
    /// rather than a bypass, but a downgrade `helpers::nest_host` (which strips
    /// userinfo, and whose doc already claimed this function matched it) does
    /// not have.
    fn host_of(url: &str) -> Option<String> {
        crate::helpers::nest_host(url)
    }
}

/// What a failed [`WsNestApi::core`] connect means for the seam that requested
/// it — the `security.md` § Pre-claim surfacing classification, decided ONCE
/// here so no per-endpoint seam re-derives (or re-flattens) it.
enum CoreError {
    /// The typed identity verdict: first-contact trust graduation failed with
    /// an identity at stake — a pin-related failure (the `known_hosts` model),
    /// or ANY graduation failure while a pre-resolved first-contact root is
    /// held for this host (the § Transport trust client-provisioned row's
    /// unconditional hard-fail — refusals and withheld bindings included).
    /// Terminal at every seam; never mapped to a Transient variant.
    IdentityMismatch {
        host: String,
        pinned_hex: String,
        /// `None` when the box proved no identity at all (refusal, withheld
        /// binding, or a root mismatch — which does not name the seen key).
        seen_hex: Option<String>,
        /// Rotation-chain fork evidence (`box-recovery.md` § Client
        /// acceptance), carried for the challenge seam's verdict variant.
        fork: bool,
        reason: String,
    },
    /// Transport-level connect failure — the seams' retryable bucket, exactly
    /// the mapping every seam applied before the classification existed.
    Transient { reason: String },
}

/// Classify a native connect/graduation failure per `security.md` § Pre-claim
/// surfacing. Pure so the table is unit-testable without a connection.
///
/// The wasm `core()` below classifies its own connect failures via an
/// exhaustive match on `FirstContactError` — a different error type on a
/// different target, deliberately kept as its own table rather than unified
/// with this one, which would drag native-only semantics onto a target that
/// cannot express them.
#[cfg(not(target_arch = "wasm32"))]
fn classify_core_failure(
    err: fauna_anon_client::AnonClientError,
    host: &str,
    held_root: Option<[u8; 32]>,
) -> CoreError {
    use fauna_anon_client::{AnonClientError, classify_identity_changed};
    let te = match err {
        AnonClientError::Trust(te) => te,
        // WebSocket/RPC/timeout trouble during connect or the handshake RPC:
        // nothing was proven either way — retryable. Written as an exhaustive
        // match (not a `Trust`-only let-else) so a variant added to
        // `AnonClientError` later forces a decision here at compile time
        // instead of silently landing in this bucket.
        AnonClientError::Decode(_)
        | AnonClientError::WebSocket(_)
        | AnonClientError::Rpc(_)
        | AnonClientError::RpcDisconnected { .. }
        | AnonClientError::RpcTimeout => {
            return CoreError::Transient {
                reason: err.to_string(),
            };
        }
    };
    // Pin-related first: `classify_identity_changed` IS the rule (one copy,
    // shared with the launch machine's two channels) — PinChanged, PinForked,
    // and a binding absence while a pin exists.
    if let Some(v) = classify_identity_changed(&te, host) {
        return CoreError::IdentityMismatch {
            host: host.to_string(),
            pinned_hex: v.pinned_hex,
            seen_hex: v.seen_hex,
            fork: v.fork,
            reason: te.to_string(),
        };
    }
    // A held first-contact root makes EVERY remaining graduation failure the
    // verdict — root mismatch, refused kind, withheld binding alike: with an
    // injected/pasted root the client knows this exact box answers the
    // handshake, so "a rejection is a first-contact MITM's cheapest move, not
    // a benign old nest" (security.md § Transport trust, Axis 2).
    if let Some(root) = held_root {
        return CoreError::IdentityMismatch {
            host: host.to_string(),
            pinned_hex: hex::encode(root),
            seen_hex: None,
            fork: false,
            reason: te.to_string(),
        };
    }
    // Rootless, pin-free trust trouble (first-contact binding failure on the
    // TOFU ladder): nothing was pinned or injected, so there is no identity
    // to have mismatched — stay transient (a false-terminal is worse here).
    CoreError::Transient {
        reason: te.to_string(),
    }
}

// ── per-target `core`: open a fresh anonymous connection wrapped in the shared
//    mapping core. The only code that differs between native and wasm. ─────────

#[cfg(not(target_arch = "wasm32"))]
impl WsNestApi {
    /// Open a fresh native anonymous connection to the resolved URL and wrap it
    /// in the shared `WsRpcNestApi` mapping core.
    async fn core(
        &self,
        base_url: &str,
    ) -> Result<WsRpcNestApi<fauna_anon_client::AnonymousNestClient>, CoreError> {
        let resolved = self.resolve(base_url);
        let addr = self.resolve_override_addr(&resolved);
        let client = fauna_anon_client::AnonymousNestClient::connect_resolving(&resolved, addr)
            .await
            .map_err(|e| CoreError::Transient {
                reason: e.to_string(),
            })?;
        // First-contact trust: authenticate the nest's identity on this fresh
        // pre-identity connection before the real request rides it. On the
        // client-provisioned path the held injected-seed root makes this an
        // exact-match verification from the very first connect (no TOFU
        // window); otherwise the DNS-`self=`/TOFU ladder applies. A
        // graduation failure drops the connection here — nothing was sent —
        // and `classify_core_failure` decides whether it is the typed identity
        // verdict or a retryable transport fault (security.md § Pre-claim
        // surfacing).
        let held_root = self.expected_root_for(&resolved);
        if let Err(e) = client.graduate_first_contact(&resolved, held_root).await {
            let host = Self::host_of(&resolved).unwrap_or_else(|| resolved.clone());
            return Err(classify_core_failure(e, &host, held_root));
        }
        // Thread the received cert's SPKI (captured during the TLS handshake;
        // `None` over plaintext) so the mapping core's identity read — the
        // nest-bound mode commit's — is SPKI-bound, not possession-only.
        let captured_spki = client.captured_cert().spki;
        Ok(WsRpcNestApi::with_captured_spki(client, captured_spki))
    }
}

#[cfg(target_arch = "wasm32")]
impl WsNestApi {
    /// As the native arm, but the wasm `connect` is synchronous — the browser
    /// `WebSocket` completes its handshake asynchronously behind the gloo adapter
    /// (sends buffer until open), so there is nothing to `await`.
    ///
    /// **First-contact trust.** When a root is held for this
    /// host — the admin pasted the console's `fauna://claim` URI — the box must
    /// prove it holds that identity *before* the real request rides the
    /// connection, and a failure drops the connection with nothing sent. The
    /// proof is possession-only: a browser hands WASM no received certificate,
    /// so native's SPKI compare is unavailable here and the relay residual
    /// documented in `security.md` § Two independent axes remains. With **no**
    /// root held the wasm arm stays on the browser's own WebPKI trust, exactly
    /// as before — the DNS-`self=`/TOFU ladder is native-only, since neither leg
    /// of it is reachable from a browser.
    async fn core(
        &self,
        base_url: &str,
    ) -> Result<WsRpcNestApi<fauna_rpc_wasm::AnonymousWsRpcClient>, CoreError> {
        use fauna_client_core::nest_trust::FirstContactError;

        let resolved = self.resolve(base_url);
        // Dial the reach address when one is armed for this host; keep
        // `resolved` — the identity URL — for every host-keyed check below.
        let dial = self.dial_url(&resolved);
        let client = fauna_rpc_wasm::AnonymousWsRpcClient::connect(&dial).map_err(|e| {
            CoreError::Transient {
                reason: e.to_string(),
            }
        })?;
        if let Some(expected) = self.expected_root_for(&resolved) {
            fauna_client_core::nest_trust::prove_first_contact_identity_possession(
                &client, expected,
            )
            .await
            .map_err(|e| match e {
                // Nothing was proven either way — retryable.
                FirstContactError::Transport => CoreError::Transient {
                    reason: e.to_string(),
                },
                // A verdict: wrong identity, absent binding, or a refusal of
                // the handshake kind — each hard-fails with a held root
                // (security.md § Pre-claim surfacing).
                FirstContactError::BindingRequired | FirstContactError::Binding(_) => {
                    CoreError::IdentityMismatch {
                        host: Self::host_of(&resolved).unwrap_or_else(|| resolved.clone()),
                        pinned_hex: hex::encode(expected),
                        seen_hex: None,
                        fork: false,
                        reason: e.to_string(),
                    }
                }
            })?;
        }
        Ok(WsRpcNestApi::new(client))
    }
}

// The single, target-agnostic `NestApi` impl. `core()` resolves to one concrete
// type per build, so `#[async_trait]` sees a concrete — and therefore
// `Send`-known on native — future, and the AFIT-generic `Send` problem that
// blocks a generic `impl NestApi for WsRpcNestApi<R>` never arises. A *connect*
// failure arrives pre-classified (`CoreError`): the transport case maps to the
// same per-endpoint transient variant the core's `map_*_err` produces for a
// mid-request transport fault, and the identity verdict maps to the endpoint's
// `IdentityMismatch` (the challenge seam's existing `IdentityChanged`) — never
// to a transient (security.md § Pre-claim surfacing).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl NestApi for WsNestApi {
    async fn probe_setup_status(&self, base_url: &str) -> Result<SetupStatus, ProbeError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    ProbeError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => ProbeError::Transient { reason },
            })?
            .probe_setup_status()
            .await
    }

    async fn silent_challenge(&self, base_url: &str, secret_hex: &str) -> SilentChallengeOutcome {
        // A transport-class *connect* failure → the retryable `Transient`
        // bucket, the same place the ceremony funnels a mid-request transport
        // fault (it can't distinguish "couldn't open the socket" from "socket
        // died mid-ceremony", and both are retry-worthy). The identity verdict
        // → the outcome's own `IdentityChanged` variant — the same verdict the
        // launch machine's channels produce, here fed by the connect-stage
        // graduation (security.md § Pre-claim surfacing). Wizard consumers
        // treat it as terminal with NO re-trust affordance: with a held
        // first-contact root the remedy is re-provisioning, never
        // forget-the-pin.
        match self.core(base_url).await {
            Ok(core) => core.silent_challenge(secret_hex).await,
            Err(CoreError::IdentityMismatch {
                host,
                pinned_hex,
                seen_hex,
                fork,
                ..
            }) => SilentChallengeOutcome::IdentityChanged {
                host,
                pinned_hex,
                seen_hex,
                fork,
            },
            Err(CoreError::Transient { reason }) => {
                SilentChallengeOutcome::Transient { error: reason }
            }
        }
    }

    async fn claim_admin(
        &self,
        base_url: &str,
        code: &str,
        secret_hex: &str,
        handle: &str,
        mail_domain: Option<&str>,
    ) -> Result<ClaimAdminResponse, ClaimAdminError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    ClaimAdminError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => ClaimAdminError::Transient { cause: reason },
            })?
            .claim_admin(code, secret_hex, handle, mail_domain)
            .await
    }

    async fn submit_invite_request(
        &self,
        base_url: &str,
        body: InviteRequestBody,
    ) -> Result<InviteRequestResponse, InviteRequestError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    InviteRequestError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => InviteRequestError::Transient { cause: reason },
            })?
            .submit_invite_request(body)
            .await
    }

    async fn cancel_invite_request(
        &self,
        base_url: &str,
        secret_hex: &str,
    ) -> Result<(), InviteRequestError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    InviteRequestError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => InviteRequestError::Transient { cause: reason },
            })?
            .cancel_invite_request(secret_hex)
            .await
    }

    async fn recheck_invite_request(
        &self,
        base_url: &str,
        actor_id_hex: &str,
    ) -> Result<InviteRequestResponse, InviteRequestError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    InviteRequestError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => InviteRequestError::Transient { cause: reason },
            })?
            .recheck_invite_request(actor_id_hex)
            .await
    }

    async fn verify_invite_code(
        &self,
        base_url: &str,
        code: &str,
    ) -> Result<InviteCodeVerification, InviteCodeError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    InviteCodeError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => InviteCodeError::Transient { cause: reason },
            })?
            .verify_invite_code(code)
            .await
    }

    async fn age_nonce(&self, base_url: &str) -> Result<AgeNonce, AgeNonceError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    AgeNonceError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => AgeNonceError::Transient { cause: reason },
            })?
            .age_nonce()
            .await
    }

    async fn register(
        &self,
        base_url: &str,
        body: RegisterBody,
    ) -> Result<RegisterResponse, RegisterError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    RegisterError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => RegisterError::Failed { cause: reason },
            })?
            .register(body)
            .await
    }

    async fn submit_nat_mode(
        &self,
        base_url: &str,
        secret_hex: &str,
        mode: NodeMode,
    ) -> Result<(), NatModeError> {
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    NatModeError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => NatModeError::Transient { cause: reason },
            })?
            .submit_nat_mode(secret_hex, mode)
            .await
    }

    async fn restore_escrowed_seed(
        &self,
        base_url: &str,
        recovery_secret_hex: &str,
        actor_id_hex: Option<&str>,
        handle: Option<&str>,
    ) -> Result<RestoredIdentity, RestoreSeedError> {
        // A transport-class connect failure is a reachability fault, the one
        // bucket the screen may invite a retry from; the identity verdict is
        // terminal — the server reached is not the expected nest, and no
        // retry of the same kit changes that.
        self.core(base_url)
            .await
            .map_err(|e| match e {
                CoreError::IdentityMismatch { reason, .. } => {
                    RestoreSeedError::IdentityMismatch { reason }
                }
                CoreError::Transient { reason } => RestoreSeedError::Transient { reason },
            })?
            .restore_escrowed_seed(recovery_secret_hex, actor_id_hex, handle)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(
        urls: Option<HashMap<String, String>>,
        resolve_override: Option<(String, IpAddr)>,
        expected: Option<(String, [u8; 32])>,
    ) -> WsNestApi {
        WsNestApi::new(
            urls,
            Arc::new(RwLock::new(resolve_override)),
            Arc::new(RwLock::new(expected)),
        )
    }

    #[test]
    fn override_resolves_over_caller_url() {
        let mut urls = HashMap::new();
        urls.insert("nest".to_string(), "http://127.0.0.1:9".to_string());
        let with_urls = api(Some(urls), None, None);
        assert_eq!(
            with_urls.resolve("https://example.com"),
            "http://127.0.0.1:9"
        );

        let plain = api(None, None, None);
        assert_eq!(plain.resolve("https://example.com"), "https://example.com");
    }

    #[test]
    fn resolve_override_addr_matches_only_the_captured_host() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let with_ip = api(None, Some(("box.example.com".to_string(), ip)), None);
        assert_eq!(
            with_ip.resolve_override_addr("https://box.example.com"),
            Some(std::net::SocketAddr::new(ip, 443))
        );
        assert_eq!(
            with_ip.resolve_override_addr("https://other.example.com"),
            None
        );
    }

    #[test]
    fn resolve_override_addr_none_when_unset() {
        let unset = api(None, None, None);
        assert_eq!(unset.resolve_override_addr("https://box.example.com"), None);
    }

    /// The injected-seed identity root is asserted only against the host it was
    /// provisioned for — a bystander domain (a handle-availability probe of an
    /// unrelated nest mid-wizard) must never inherit it (it would hard-fail
    /// that nest's genuine identity against the wrong root).
    #[test]
    fn expected_root_matches_only_the_provisioned_host() {
        let id = [0x5au8; 32];
        let with_root = api(None, None, Some(("box.example.com".to_string(), id)));
        assert_eq!(
            with_root.expected_root_for("https://box.example.com"),
            Some(id)
        );
        assert_eq!(
            with_root.expected_root_for("https://box.example.com:443/x"),
            Some(id),
            "port/path must not defeat the host match"
        );
        assert_eq!(
            with_root.expected_root_for("https://other.example.com"),
            None
        );

        let unset = api(None, None, None);
        assert_eq!(unset.expected_root_for("https://box.example.com"), None);
    }

    /// The browser's reach dial (`onboarding.md` § 6 *Reaching the box*): with
    /// an override armed for this host, the wasm arm connects to `https://{ip}`
    /// literally, because it can override neither DNS nor TLS.
    #[test]
    fn dial_url_substitutes_the_armed_address_for_the_matching_host() {
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let armed = api(None, Some(("box.example.com".to_string(), ip)), None);

        assert_eq!(
            armed.dial_url("https://box.example.com"),
            "https://203.0.113.9"
        );
        assert_eq!(
            armed.dial_url("https://box.example.com:443/x"),
            "https://203.0.113.9",
            "port/path must not defeat the host match, as for the other two guards"
        );
        assert_eq!(
            armed.dial_url("https://other.example.com"),
            "https://other.example.com",
            "a bystander domain keeps its own address — the override belongs to \
             the box it was captured for"
        );
        assert_eq!(
            api(None, None, None).dial_url("https://box.example.com"),
            "https://box.example.com",
            "nothing armed dials the identity URL, which is the pre-slot behavior"
        );

        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(
            api(None, Some(("box.example.com".to_string(), v6)), None)
                .dial_url("https://box.example.com"),
            "https://[2001:db8::1]",
            "an IPv6 literal must be bracketed or the URL does not parse"
        );
    }

    /// ⚠ The dial substitution must never reach the **identity**-keyed checks.
    /// `expected_root_for` is asked about the identity URL, so an armed override
    /// leaves the held root findable; asking it about the dial URL instead would
    /// find no root for that "host" and silently drop to TOFU — a downgrade, on
    /// exactly the path (`security.md` § Transport trust, the
    /// *Client-provisioned box* row) whose whole claim is that there is no TOFU
    /// window.
    #[test]
    fn arming_the_dial_does_not_move_the_identity_the_root_is_keyed_by() {
        let id = [0x5au8; 32];
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let armed = api(
            None,
            Some(("box.example.com".to_string(), ip)),
            Some(("box.example.com".to_string(), id)),
        );

        let identity_url = "https://box.example.com";
        assert_eq!(armed.expected_root_for(identity_url), Some(id));
        assert_eq!(
            armed.expected_root_for(&armed.dial_url(identity_url)),
            None,
            "which is why the wasm core keeps `resolved` for this check and uses \
             the dial URL only to connect"
        );
    }

    /// The source-level companion to the test above — same reasoning as
    /// `both_core_arms_verify_the_held_first_contact_root`: the defect is a
    /// *swap* in an arm no native test can execute, so read the arm.
    #[test]
    fn the_wasm_core_dials_the_reach_address_and_keys_trust_on_the_identity_url() {
        let src = include_str!("ws_nest_api.rs");
        let (_, wasm) = src
            .split_once("#[cfg(target_arch = \"wasm32\")]\nimpl WsNestApi {")
            .expect("the wasm `impl WsNestApi` block is where the wasm core() lives");
        let wasm = wasm
            .split_once("\n#[cfg_attr(not(target_arch")
            .map(|(before, _)| before)
            .unwrap_or(wasm);
        assert!(
            wasm.contains("let dial = self.dial_url(&resolved);")
                && wasm.contains("AnonymousWsRpcClient::connect(&dial)"),
            "the wasm arm must CONNECT to the dial URL — otherwise a resumed \
             browser waits on DNS for a box that has been up since cloud-init"
        );
        assert!(
            wasm.contains("self.expected_root_for(&resolved)"),
            "…and must key the first-contact root on the IDENTITY url, never on \
             the dial URL (that lookup would silently find no root)"
        );
    }

    /// **Both** `core()` arms must consult the held root — row 147.
    ///
    /// This is a *source-level* pin, deliberately. The defect it guards is the
    /// absence of a call, and the wasm arm compiles only under
    /// `target_arch = "wasm32"`, so no native behavioural test can execute it —
    /// the very combination that let the gap survive from the 51st pass to now:
    /// `expected_root_for` was tested (above, and those tests passed the whole
    /// time) while the arm that was supposed to *use* it did not exist on wasm.
    /// A test that drives the check cannot see a caller that never calls it
    /// (the earlier lesson); this reads the module's own source instead.
    ///
    /// If this fails after a refactor, the fix is to keep each arm verifying the
    /// held root — not to relax the assertion.
    #[test]
    fn both_core_arms_verify_the_held_first_contact_root() {
        let src = include_str!("ws_nest_api.rs");
        let (native, wasm) = src
            .split_once("#[cfg(target_arch = \"wasm32\")]\nimpl WsNestApi {")
            .expect("the wasm `impl WsNestApi` block is where the wasm core() lives");
        // Everything after the wasm impl header up to the test module is the
        // wasm arm; the native arm is what precedes it.
        let wasm = wasm
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .unwrap_or(wasm);

        assert!(
            native.contains("let held_root = self.expected_root_for(&resolved);")
                && native.contains("client.graduate_first_contact(&resolved, held_root).await"),
            "the native core() must graduate the connection against the held root"
        );
        assert!(
            wasm.contains("self.expected_root_for(&resolved)")
                && wasm.contains("prove_first_contact_identity_possession"),
            "the wasm core() must prove the held root before any request rides \
             the connection — without this the pasted fauna://claim identity is \
             stored and never consulted"
        );
    }

    // ── `classify_core_failure` — the § Pre-claim surfacing table ─────────────

    use fauna_anon_client::{AnonClientError, IdentityError, TrustError};

    const HOST: &str = "box.example.com";

    /// With a held first-contact root, EVERY graduation failure is the typed
    /// identity verdict — root mismatch, refused kind, withheld binding alike
    /// (`security.md` § Transport trust: with an injected root a rejection is
    /// hostile-or-broken, never a benign old nest).
    #[test]
    fn held_root_makes_every_trust_failure_the_identity_verdict() {
        let root = [0x5au8; 32];
        for te in [
            TrustError::Identity(IdentityError::RootMismatch),
            TrustError::BindingRequired,
            TrustError::NoCapturedSpki,
        ] {
            let got = classify_core_failure(AnonClientError::Trust(te), HOST, Some(root));
            match got {
                CoreError::IdentityMismatch {
                    host,
                    pinned_hex,
                    seen_hex,
                    fork,
                    ..
                } => {
                    assert_eq!(host, HOST);
                    assert_eq!(pinned_hex, hex::encode(root));
                    assert_eq!(seen_hex, None);
                    assert!(!fork);
                }
                CoreError::Transient { reason } => {
                    panic!("held-root trust failure classified retryable: {reason}")
                }
            }
        }
    }

    /// A changed TOFU pin is the identity verdict even with no held root —
    /// the `known_hosts` model, via the shared `classify_identity_changed`
    /// table (one copy, shared with the launch machine's channels).
    #[test]
    fn pin_change_without_a_root_is_the_identity_verdict() {
        let (pinned, seen) = ([0x11u8; 32], [0x22u8; 32]);
        let got = classify_core_failure(
            AnonClientError::Trust(TrustError::Identity(IdentityError::PinChanged {
                pinned,
                seen,
            })),
            HOST,
            None,
        );
        match got {
            CoreError::IdentityMismatch {
                pinned_hex,
                seen_hex,
                fork,
                ..
            } => {
                assert_eq!(pinned_hex, hex::encode(pinned));
                assert_eq!(seen_hex, Some(hex::encode(seen)));
                assert!(!fork);
            }
            CoreError::Transient { reason } => {
                panic!("pin change classified retryable: {reason}")
            }
        }
    }

    /// Rootless, pin-free first-contact binding trouble has no identity at
    /// stake — nothing was pinned or injected — so it stays transient
    /// (a false-terminal is worse than a retry there). Non-trust connect
    /// errors stay transient regardless.
    #[test]
    fn rootless_binding_trouble_and_transport_stay_transient() {
        for err in [
            AnonClientError::Trust(TrustError::BindingRequired),
            AnonClientError::WebSocket("connection refused".into()),
            AnonClientError::RpcTimeout,
        ] {
            assert!(
                matches!(
                    classify_core_failure(err, HOST, None),
                    CoreError::Transient { .. }
                ),
                "rootless/pin-free failure must stay retryable"
            );
        }
    }

    /// Pins each of `AnonClientError`'s six variants to its current verdict —
    /// the equivalence the exhaustive-match refactor must
    /// preserve. A variant added to the enum without a matching entry here fails to
    /// compile, not just to pass: `classify_core_failure`'s match is
    /// exhaustive over the enum, so the compiler forces the new arm before
    /// this table can even be extended.
    #[test]
    fn all_six_variants_pin_to_their_current_verdict() {
        let non_trust_cases = [
            AnonClientError::Decode("bad cbor".into()),
            AnonClientError::WebSocket("connection refused".into()),
            AnonClientError::Rpc(fauna_protocol::RpcError::new("internal", "internal_error")),
            AnonClientError::RpcDisconnected {
                was_in_flight: true,
            },
            AnonClientError::RpcTimeout,
        ];
        for err in non_trust_cases {
            assert!(
                matches!(
                    classify_core_failure(err, HOST, None),
                    CoreError::Transient { .. }
                ),
                "every non-Trust variant must stay transient"
            );
        }
        assert!(matches!(
            classify_core_failure(
                AnonClientError::Trust(TrustError::Identity(IdentityError::RootMismatch)),
                HOST,
                Some([0x5au8; 32]),
            ),
            CoreError::IdentityMismatch { .. }
        ));
    }
}

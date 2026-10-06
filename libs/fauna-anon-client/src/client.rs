//! `AnonymousNestClient` — the native client of the **pre-identity (anonymous)
//! WS-RPC connection** (`GET /api/v1/ws`, `Sec-WebSocket-Protocol: fauna.v1`
//! with no bearer; transport.md § Pre-identity).
//!
//! Onboarding bootstrap — auth handshake, public discovery, account
//! registration, the one-time admin claim, invite requests, the storage-mode
//! commit — and the app-launch silent-challenge fallback all run *before any
//! bearer token exists*, so they cannot ride the actor-keyed authenticated
//! client. They ride this second, bearer-less connection instead. It carries no
//! actor, no push routing, and no reconnect supervisor: the dispatcher is
//! **fixed** at `connect` rather than a reconnect-managed slot, so a dropped
//! connection surfaces as `RpcDisconnected`. The pre-identity state machines
//! (`fauna-onboarding-machine`, `fauna-launch-machine`) already retry transient
//! nest errors at the UI level, so a connector-level supervisor would be
//! redundant complexity for a short-lived bootstrap flow — the connection is
//! dropped once a bearer is in hand and the client reopens the authenticated
//! `GET /api/v1/ws/{actor_id}`.
//!
//! It implements [`fauna_protocol::RpcRequester`] so pre-identity kind callers
//! share the same code path as the authenticated client (the wasm arm is
//! `fauna-rpc-wasm`'s anonymous client).

use std::sync::Arc;

use fauna_protocol::{KindRegistry, RpcDispatcher};
use tokio::task::JoinHandle;

use crate::error::AnonClientError;
use crate::trust::PinMinting;

/// What [`AnonymousNestClient::graduate_first_contact`] did with the
/// connection — either it *graduated* (channel binding verified against the
/// Axis-2 identity root) or it took one of the three sanctioned skips. Callers
/// that must *prove* graduation (the client-provisioned first-contact e2e)
/// assert `Graduated`; ordinary callers only branch on `Err`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstContactOutcome {
    /// The binding verified and the identity matched its root (pre-resolved
    /// exact match, or TOFU pin/confirm). The bound SPKI is pinned for the
    /// host's bearer connection.
    Graduated,
    /// WebPKI-valid cert — authenticated by public CA + hostname; no binding
    /// needed (the standard public-nest path).
    SkippedWebPki,
    /// Plaintext (`http://`/`ws://`) — no cert to bind (loopback dev/e2e).
    SkippedPlaintext,
}

pub struct AnonymousNestClient {
    dispatcher: Arc<RpcDispatcher>,
    kind_registry: Arc<KindRegistry>,
    /// What the capturing TLS verifier observed about the nest's leaf cert
    /// during the handshake (SPKI + WebPKI validity). Read after the auth
    /// handshake to graduate the channel binding (`crate::trust`). Default
    /// (no cert) on a plain `ws://` dev nest.
    captured: crate::tls_verify::CaptureHandle,
    /// Drives the inbound stream for the connection's lifetime; aborted by this
    /// type's [`Drop`] (so dropping the client closes the WS).
    ///
    /// The abort has to be explicit: the driver owns the whole adapter, and
    /// **dropping a `JoinHandle` detaches its task instead of aborting it**, so
    /// storing it as a bare `_driver` field left every connection running — and
    /// its socket `ESTABLISHED` — for the life of the process.
    driver: JoinHandle<()>,
}

impl Drop for AnonymousNestClient {
    /// Close the connection with the client, which is the teardown contract the
    /// whole type is written against: `graduate_handshake`'s
    /// § Connection-teardown rule tells callers to *drop the client* to tear a
    /// failed connection down, and every bearer mint
    /// (`WsDeviceHandshakeBearer::fetch_token` →
    /// [`crate::bearer::mint_bearer_over_device_handshake`]) opens one of these
    /// per refresh and drops it at the end of the call.
    ///
    /// Without this the contract was silently a no-op, so a client reconnecting
    /// in a loop leaked one permanently-`ESTABLISHED` socket per attempt, on
    /// both ends, owned by nothing that could ever close it — the accumulation
    /// behind the 2026-08-22 mac network exhaustion. Pinned by `tests/connection_teardown.rs`.
    fn drop(&mut self) {
        self.driver.abort();
    }
}

impl AnonymousNestClient {
    /// Open an anonymous connection to `nest_url` (an `http(s)://` base — the
    /// scheme is swapped to `ws(s)://` internally). Uses the default
    /// protocol-kind registry; pre-identity kinds carry no special metadata,
    /// so the § 1.4 default 30 s deadline applies to all of them.
    pub async fn connect(nest_url: &str) -> Result<Self, AnonClientError> {
        Self::connect_resolving(nest_url, None).await
    }

    /// As [`connect`](Self::connect), but dials `resolve` directly instead of
    /// resolving `nest_url`'s host via system DNS — SNI/`Host`/cert-identity
    /// still come from `nest_url` (security.md § Transport trust Axis 1: the
    /// capturing verifier's channel binding authenticates the box by identity,
    /// so overriding only the reach address is MITM-safe). `None` behaves
    /// exactly like [`connect`](Self::connect). Lets a caller reach a
    /// freshly-provisioned box by its known IP before its DNS record has
    /// propagated.
    pub async fn connect_resolving(
        nest_url: &str,
        resolve: Option<std::net::SocketAddr>,
    ) -> Result<Self, AnonClientError> {
        Self::connect_with_registry_resolving(nest_url, Arc::new(KindRegistry::full()), resolve)
            .await
    }

    /// As [`connect`](Self::connect), with a caller-supplied kind registry for
    /// per-kind deadline metadata.
    pub async fn connect_with_registry(
        nest_url: &str,
        kind_registry: Arc<KindRegistry>,
    ) -> Result<Self, AnonClientError> {
        Self::connect_with_registry_resolving(nest_url, kind_registry, None).await
    }

    /// As [`connect_with_registry`](Self::connect_with_registry), with the
    /// [`connect_resolving`](Self::connect_resolving) override.
    async fn connect_with_registry_resolving(
        nest_url: &str,
        kind_registry: Arc<KindRegistry>,
        resolve: Option<std::net::SocketAddr>,
    ) -> Result<Self, AnonClientError> {
        let (adapter, captured) = crate::ws::connect_anonymous(nest_url, resolve).await?;
        let (dispatcher, driver) = RpcDispatcher::new(adapter);
        // The wire `replay_forbidden` hint comes off this registry; a fresh
        // dispatcher has none attached, so without this every request would
        // omit it (`transport.md` § Idempotency and reconnect-with-resume).
        let _ = dispatcher.set_kind_registry((*kind_registry).clone());
        let driver = tokio::spawn(driver);
        Ok(Self {
            dispatcher: Arc::new(dispatcher),
            kind_registry,
            captured,
            driver,
        })
    }

    /// What the capturing TLS verifier observed about the nest's leaf cert
    /// during this connection's handshake. Read after `fauna.auth.handshake`
    /// to verify the channel binding (`crate::trust::graduate_handshake`).
    pub fn captured_cert(&self) -> crate::tls_verify::CapturedCert {
        self.captured.lock().unwrap().clone()
    }

    /// Authenticate the nest's identity on this **pre-identity** connection —
    /// before any actor exists — by running `fauna.auth.nest_handshake` and
    /// graduating the returned channel binding (security.md § Transport trust;
    /// design tracked internally).
    /// Call right after connect, before issuing any real request, so a failed
    /// graduation tears the connection down with nothing sent
    /// (§ Connection-teardown rule — drop the client on `Err`).
    ///
    /// `expected_root` is a pre-resolved identity root the caller already holds
    /// — on the client-provisioned-box path, the public key derived from the
    /// deployment seed the client injected at provision (exact match required:
    /// a freshly-provisioned box presenting any other identity is MITM/bug).
    /// `None` falls back to the DNS `self=` root, else TOFU-on-host — the same
    /// Axis-2 ladder as the bearer handshake path, with the installed pin
    /// store deciding whether TOFU may mint ([`PinMinting::StoreDecides`];
    /// [`Self::graduate_first_contact_minting`] is the form that decides it).
    ///
    /// **A rejection of the kind is a hard failure, on every host.** A box
    /// that will not prove its identity is hostile-or-broken, never a benign
    /// old nest: it hard-fails as
    /// [`TrustError::BindingRequired`](crate::trust::TrustError::BindingRequired)
    /// with nothing sent. (The pre-Track-2 "legacy nest" skip that once let a
    /// rejection on the pin-less DNS/TOFU ladder proceed ungraduated was removed 2026-09-24 by the
    /// compat-remnant sweep, `version-compatibility.md` § Dimension 2, and with
    /// it the MITM-mimics-the-rejection residual it carried.)
    pub async fn graduate_first_contact(
        &self,
        nest_url: &str,
        expected_root: Option<[u8; 32]>,
    ) -> Result<FirstContactOutcome, AnonClientError> {
        self.graduate_first_contact_minting(nest_url, expected_root, PinMinting::StoreDecides)
            .await
    }

    /// [`Self::graduate_first_contact`] with the TOFU-mint decision made
    /// explicit: [`PinMinting::Never`] verifies a held identity pin or a
    /// pre-resolved root and refuses everything else, whatever store the
    /// process installed — the bearer dial's fallback
    /// ([`Self::graduate_transport_trust`]).
    pub async fn graduate_first_contact_minting(
        &self,
        nest_url: &str,
        expected_root: Option<[u8; 32]>,
        minting: PinMinting,
    ) -> Result<FirstContactOutcome, AnonClientError> {
        use fauna_protocol::auth::{NEST_HANDSHAKE_KIND, NestHandshakeReply, NestHandshakeRequest};

        // Plaintext (`http://`/`ws://`, the loopback dev/e2e nest): no cert to
        // bind — the connection keeps the network-trust posture (the same
        // scheme guard every graduation caller applies).
        if nest_url.starts_with("http://") || nest_url.starts_with("ws://") {
            return Ok(FirstContactOutcome::SkippedPlaintext);
        }
        // WebPKI-valid cert → authenticated the boring way (public CA +
        // hostname); skip the extra round-trip, exactly like
        // `graduate_handshake_with_root`'s short-circuit — **but only when no
        // injected root is held.**
        //
        // ⚠ A held `expected_root` suppresses this arm, and that scoping is
        // load-bearing (ruled 2026-09-02). The arm used to fire unconditionally, safe on a
        // premise about *what a box serves* rather than about this function: a
        // box with no domain serves its self-signed floor, so `webpki_valid`
        // was false there and a held seed was always checked in the pre-claim
        // window it exists for. That is the premise a 2026-07-08 review relied
        // on to clear the arm — and
        // `../../../docs/goal/architecture/nest/tls-certificates.md` § B-IP (the
        // IP bridge cert) deletes it: a domainless box now serves a *publicly
        // trusted* `ip`-identifier cert pre-claim, so the arm would fire exactly
        // where the injected seed is the sole Axis-2 root.
        //
        // WebPKI is also simply the weaker authenticator here, B-IP or not: it
        // answers "is this the name/address I dialed", while the seed answers
        // "is this the nest I provisioned" — the distinction
        // [`Self::verify_nest_identity`] is built around. For an IP-SAN cert
        // there is not even a hostname: "holds a cert for this IP" is close to
        // "currently holds this IP", and a recycled cloud address is
        // HTTP-01-certifiable in seconds. So whenever the caller holds the
        // stronger authenticator, it is used; the cost is one round-trip on the
        // provisioned path only. This mirrors the rejection arm below, which
        // already branches on `expected_root.is_some()` to hard-fail.
        //
        // "Held" also covers an identity pin the install-scoped store already
        // names for this host — the claim-seeded pin on a provisioned box, or
        // TOFU — so a pinned public-CA box runs the handshake here too (the
        // consumer's-dial heal, `graduate_transport_trust`, reaches this arm
        // with no explicit root); `trust::webpki_waives_binding` is the one
        // decision point every graduation entrypoint shares. Past the waiver the binding is demanded outright: a
        // rejection below hard-fails on every host, so the box on the wire
        // cannot pick a weaker reading of "a root is held" by declining the
        // kind.
        let captured = self.captured_cert();
        let authority = crate::trust::authority_of(nest_url);
        if crate::trust::webpki_waives_binding(&authority, &captured, expected_root, None) {
            return Ok(FirstContactOutcome::SkippedWebPki);
        }

        let nonce = crate::trust::fresh_nonce();
        let reply: Result<NestHandshakeReply, AnonClientError> = self
            .request(
                NEST_HANDSHAKE_KIND,
                NestHandshakeRequest {
                    client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
                    extra: Default::default(),
                },
            )
            .await;
        let binding = match reply {
            Ok(r) => r.cert_binding,
            // A wire-level rejection (`fauna.protocol.unknown_kind` /
            // `unauthenticated`) is hostile-or-broken → hard-fail before
            // anything rides the channel. A transport fault stays an error
            // (retryable).
            Err(e) if fauna_protocol::RpcErrorClass::is_rejection(&e) => {
                return Err(AnonClientError::Trust(
                    crate::trust::TrustError::BindingRequired,
                ));
            }
            Err(e) => return Err(e),
        };

        let root = match expected_root {
            Some(r) => Some(r),
            None => crate::trust::resolve_dns_self_root(&authority).await,
        };
        crate::trust::graduate_handshake_with_root_minting(
            &authority,
            &captured,
            &nonce,
            binding.as_ref(),
            root,
            minting,
        )
        .map_err(AnonClientError::Trust)?;
        Ok(FirstContactOutcome::Graduated)
    }

    /// Prove that the endpoint at this connection holds the **expected nest
    /// identity** — the anchor-identity check identity-succession propagation
    /// runs before it will trust a peer's public registration
    /// chain.
    ///
    /// Unlike [`Self::graduate_first_contact`], this is **not** transport-trust
    /// graduation and it **never short-circuits on WebPKI**: WebPKI authenticates
    /// the URL's *domain*, but the succession anchor is reached by a URL resolved
    /// from residue an attacker can influence, so the question is not "is this the
    /// domain I dialed" but "does this box hold the *nest key* I pinned". It
    /// answers that the only way that survives a lying `nest.info` or a poisoned
    /// URL: the box must **sign a fresh nonce as `expected`**.
    ///
    /// Runs `fauna.auth.nest_handshake`, requires a channel binding, checks the
    /// claimed id equals `expected` *before* trusting its signature, then verifies
    /// possession — SPKI-bound against the received cert on a TLS connection
    /// (defeats a relay), possession-only over plaintext (the in-process/e2e
    /// nest, no cert to bind). A rejection, a missing binding, a wrong id, or a
    /// bad signature all fail closed: a peer that cannot prove the pinned identity
    /// is refused, never TOFU-downgraded.
    pub async fn verify_nest_identity(
        &self,
        nest_url: &str,
        expected: [u8; 32],
    ) -> Result<(), AnonClientError> {
        use fauna_protocol::auth::{NEST_HANDSHAKE_KIND, NestHandshakeReply, NestHandshakeRequest};

        let nonce = crate::trust::fresh_nonce();
        let reply: NestHandshakeReply = self
            .request(
                NEST_HANDSHAKE_KIND,
                NestHandshakeRequest {
                    client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
                    extra: Default::default(),
                },
            )
            .await?;
        let binding = reply.cert_binding.ok_or(AnonClientError::Trust(
            crate::trust::TrustError::BindingRequired,
        ))?;

        let captured = self.captured_cert();
        // Plaintext loopback (`http://`/`ws://`, the in-process/e2e nest) captures
        // no SPKI: possession-only, then the explicit id check. Otherwise bind the
        // signature to the cert we actually received (SPKI-compare inside
        // `verify_cert_binding`, which also enforces `expected`).
        let verified: [u8; 32] = if nest_url.starts_with("http://") || nest_url.starts_with("ws://")
        {
            let id = crate::cert_binding::verify_cert_binding_possession(&nonce, &binding)
                .map_err(|e| AnonClientError::Trust(e.into()))?;
            if id != expected {
                return Err(AnonClientError::Trust(crate::trust::TrustError::Binding(
                    crate::cert_binding::BindingError::IdentityMismatch,
                )));
            }
            id
        } else {
            let received_spki = captured.spki.ok_or(AnonClientError::Trust(
                crate::trust::TrustError::NoCapturedSpki,
            ))?;
            crate::cert_binding::verify_cert_binding(
                &received_spki,
                &nonce,
                &binding,
                Some(&expected),
            )
            .map_err(|e| AnonClientError::Trust(e.into()))?
        };
        debug_assert_eq!(verified, expected);
        Ok(())
    }

    /// One-shot transport-trust graduation behind the bearer dial's
    /// graduate-and-retry fallback (`tls_dial`; security.md § Pin custody
    /// across processes, *the consumer's dial needs its own graduation step*).
    /// A process that holds a **bearer but no identity key** — the apple File
    /// Provider extension, a background agent — never runs the signed
    /// `fauna.auth.handshake` mint (the app minted its bearer into shared
    /// storage), so nothing graduates a bound SPKI in-process and the bearer
    /// dial to a self-signed nest fails WebPKI forever. This opens a fresh
    /// anonymous connection, runs the pre-identity `fauna.auth.nest_handshake`
    /// via [`Self::graduate_first_contact_minting`], and drops the connection;
    /// on [`FirstContactOutcome::Graduated`] the bound SPKI is cached for the
    /// caller's bearer dial.
    ///
    /// **It never mints a pin** ([`PinMinting::Never`]): the Axis-2 ladder is
    /// a held identity pin or a DNS `self=` root, else refuse — whatever pin
    /// store the process installed. It runs on a *failed* dial with no user in
    /// the loop, and an identity-holding process is NOT normally past this
    /// point: a public-CA nest's own mint takes the WebPKI waiver and pins
    /// nothing, so any dial error there arrives here with no pin — where a
    /// store-decides graduation TOFU-minted whatever answered (an on-path box
    /// serving its own self-signed cert) and handed it the bearer on the
    /// retry. Pinning stays with the interactive signed
    /// mint and claim paths alone.
    pub async fn graduate_transport_trust(
        nest_url: &str,
    ) -> Result<FirstContactOutcome, AnonClientError> {
        let client = Self::connect(nest_url).await?;
        client
            .graduate_first_contact_minting(nest_url, None, PinMinting::Never)
            .await
    }

    /// Send a pre-identity RPC request and await the typed reply: encode the
    /// typed `Req` to a dag-cbor `Value`, dispatch, await the reply bounded by
    /// the kind's deadline, decode the typed `Reply`.
    ///
    /// A kind outside the pre-identity allowlist resolves to
    /// `AnonClientError::Rpc` with code `fauna.protocol.unauthenticated` (the
    /// nest keeps the connection open).
    pub async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, AnonClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        // The shared fixed-dispatcher core (one copy across the anonymous +
        // one-shot authed clients — the native mirror of `fauna-rpc-wasm`'s
        // `dispatch_typed`).
        crate::dispatch::request_typed(&self.dispatcher, &self.kind_registry, kind, payload).await
    }
}

/// The native arm of the shared `RpcRequester` seam for the pre-identity
/// connection — mirrors the authenticated client's impl so kind-calling glue is
/// generic over `R: RpcRequester` regardless of whether the connection is
/// anonymous or authenticated. The future is `Send` (the payload is encoded
/// before the first await), so native callers can `tokio::spawn` it.
impl fauna_protocol::RpcRequester for AnonymousNestClient {
    type Error = AnonClientError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, AnonClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        // Fully qualified so the path resolves to the inherent method, not back
        // into this trait method.
        AnonymousNestClient::request(self, kind, payload).await
    }
}

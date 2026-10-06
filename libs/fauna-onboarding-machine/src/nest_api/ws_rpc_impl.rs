//! WS-RPC impl of the wizard's nest surface over the **pre-identity (anonymous)
//! WS connection** (`GET /api/v1/ws`, `Sec-WebSocket-Protocol: fauna.v1`, no
//! bearer — transport.md § Pre-identity). The onboarding machine is the first
//! client consumer of that connection (transport.md:656).
//!
//! ## Why a generic `WsRpcNestApi<R>` and not a generic `impl NestApi`
//!
//! [`crate::nest_api::NestApi`] is an `#[async_trait]` so the machine can hold
//! `Arc<dyn NestApi>`; that boxes each method's future as `+ Send` on native.
//! But [`RpcRequester::request`] is an `async fn in trait` whose future is
//! `Send` only *per concrete impl* (native `AnonymousNestClient` yields a `Send`
//! future, the wasm anonymous client a `!Send` one) — it is **not provably
//! `Send` in a generic context**, and return-type-notation can't bound a method
//! with generic type params. So a single `impl<R: RpcRequester> NestApi for
//! WsRpcNestApi<R>` cannot compile. The established fix (see
//! `fauna-client-dns`'s `DnsAdminClient<R>` + per-target `RpcDnsNest`) is:
//!
//! - **This crate** holds the generic, target-agnostic `WsRpcNestApi<R>` — all
//!   the kind→wire-type composition and the `RpcError.code` → per-endpoint error
//!   mapping, written **once** for every app (priority #2). Its methods are
//!   inherent `async fn`s returning the same `Result`s as the `NestApi` trait
//!   (minus the HTTP-shaped `base_url` arg — the WS connection is connection-
//!   oriented, one nest per flow).
//! - A thin **per-target** `NestApi` wrapper (native over
//!   `WsRpcNestApi<AnonymousNestClient>`, wasm over the rpc-wasm anonymous
//!   client) does the dyn-compatible boxing and just forwards. That wrapper +
//!   the `machine.rs` construction flip land together (the wasm connector
//!   is a prerequisite for the *unconditional* flip, since the machine
//!   builds for wasm).
//!
//! ## Behavior preservation vs. the HTTP twin (`reqwest_impl.rs`)
//!
//! This is a transport migration, not a behavior change. Each `RpcError.code`
//! the WS handlers emit (`claim_handlers` / `invite_handlers` / `account_handlers`
//! / `storage_mode_handlers` / `discovery_handlers`) is mapped to the same
//! per-endpoint error variant the reqwest impl produced for the HTTP-status twin
//! of that condition. Two structural consequences of the anonymous WS path,
//! both pre-existing nest decisions (not regressions introduced here):
//!
//! - `InviteRequestError::{Closed, RateLimited}` are **unreachable** on the WS
//!   path. No `InviteError` maps to HTTP 403 in the twin (so `Closed` was
//!   already dead), and the per-IP rate-limit (429) is HTTP-only — the anonymous
//!   WS carries no peer IP yet (the A1.1 deferral, `invite_core.rs`). The shared
//!   mapping therefore never emits them.
//! - `claim_admin`'s server `fauna.auth.signature_failed` maps to
//!   `ClaimAdminError::Invalid` (matching the former HTTP twin's 403 →
//!   `Invalid`), not `InvalidIdentity` — that variant is reserved for a *local*
//!   bad-secret-hex failure, as the removed `reqwest` claim helper reserved it.
//!   (In practice `signature_failed` is unreachable via this client anyway: we
//!   always sign correctly from the supplied secret.)

use ed25519_dalek::Signer;
use fauna_core::data::Timestamp;
use fauna_protocol::{RpcErrorClass, RpcRequester, Value};

use super::types::*;

// Pre-identity onboarding kinds. Literals (not shared consts — the codebase has
// no kind-name const module; handlers + KindRegistry use literals too), kept in
// one place so the wire surface is auditable against `pre_identity_allowlist.rs`.
mod kinds {
    pub const SETUP_STATUS: &str = "fauna.setup.status";
    pub const CLAIM_ADMIN: &str = "fauna.auth.claim_admin";
    pub const INVITE_SUBMIT: &str = "fauna.account.invite_request.submit";
    pub const INVITE_STATUS: &str = "fauna.account.invite_request.status";
    pub const INVITE_CANCEL: &str = "fauna.account.invite_request.cancel";
    pub const INVITE_CODE_VERIFY: &str = "fauna.account.invite_code.verify";
    pub const ACCOUNT_AGE_NONCE: &str = "fauna.account.age_nonce";
    pub const REGISTER: &str = "fauna.account.register";
    pub const NAT_MODE: &str = "fauna.setup.nat_mode";
    pub const ACTOR_BY_HANDLE: &str = "fauna.actor.by_handle";
}

/// Shared, transport-generic WS-RPC mapping of the wizard's nest surface. Holds
/// the pre-identity connector `R` (native `fauna_anon_client::AnonymousNestClient`,
/// wasm `fauna-rpc-wasm`'s anonymous client) and turns each onboarding call into
/// a `kind` + `fauna-protocol` wire request, mapping replies and `RpcError`
/// codes back into the per-endpoint response/error types the wizard consumes.
///
/// The connection is fixed (one nest per onboarding flow), so there is no
/// `base_url` argument — the caller builds the connector with the nest URL.
pub struct WsRpcNestApi<R: RpcRequester> {
    requester: R,
    /// SHA-256 SPKI of the TLS cert the connection actually received, when the
    /// connector could capture one (native TLS; `None` on wasm and plaintext).
    /// Threaded into `nest_trust::read_login_binding` so the nest-bound mode
    /// commit's identity read is SPKI-bound wherever the platform allows —
    /// possession-only elsewhere, the documented residual.
    captured_spki: Option<[u8; 32]>,
}

impl<R> WsRpcNestApi<R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    pub fn new(requester: R) -> Self {
        Self {
            requester,
            captured_spki: None,
        }
    }

    /// [`Self::new`] with the received cert's SPKI captured by the connector —
    /// the native constructor (`WsNestApi::core`'s non-wasm arm).
    pub fn with_captured_spki(requester: R, captured_spki: Option<[u8; 32]>) -> Self {
        Self {
            requester,
            captured_spki,
        }
    }

    /// `fauna.setup.status` — claimed/unclaimed + the storage-mode pin (Phase C
    /// consent screen). Any error is transient: the reqwest twin treated every
    /// non-success status the same way, and a transport/decode fault is retryable
    /// at the UI level.
    pub async fn probe_setup_status(&self) -> Result<SetupStatus, ProbeError> {
        let reply: fauna_protocol::discovery::SetupStatusReply = self
            .requester
            .request(
                kinds::SETUP_STATUS,
                fauna_protocol::discovery::SetupStatusRequest::default(),
            )
            .await
            .map_err(|e| ProbeError::Transient {
                reason: e.to_string(),
            })?;
        Ok(SetupStatus {
            claimed: reply.claimed,
            // The resolved NAT axis rides as the lowercase wire string
            // (`"public"` / `"private"`); a spelling this build does not know
            // → None.
            node_mode: crate::state::NodeMode::from_wire_str(&reply.node_mode),
        })
    }

    /// `fauna.auth.{challenge,verify}` — the handle-check silent sign-in. Decode
    /// the secret hex (a bad hex is a *local* identity failure → `SecretInvalid`,
    /// the terminal bucket, never a transient retry), read the identity the
    /// signature binds off this connection
    /// (`fauna_client_core::nest_trust::read_login_binding`, SPKI-bound where
    /// the connector captured the received cert — `login.md` § Binding the
    /// nest), then drive the shared `fauna_protocol::auth::run_silent_challenge`
    /// ceremony over the pre-identity requester. The ceremony derives the actor
    /// id from the secret, so no `actor_id` arg is needed.
    pub async fn silent_challenge(&self, secret_hex: &str) -> SilentChallengeOutcome {
        use fauna_client_core::nest_trust::{LoginBindingError, read_login_binding};
        let secret = match hex::decode(secret_hex) {
            Ok(b) => b,
            Err(e) => {
                return SilentChallengeOutcome::SecretInvalid {
                    error: format!("secret hex: {e}"),
                };
            }
        };
        let nest_id = match read_login_binding(&self.requester, self.captured_spki.as_ref()).await {
            Ok(id) => id,
            Err(LoginBindingError::Refused(e)) | Err(LoginBindingError::Transport(e)) => {
                return fauna_protocol::auth::silent_challenge_error(&e);
            }
            Err(e) => {
                return SilentChallengeOutcome::Transient {
                    error: e.to_string(),
                };
            }
        };
        fauna_protocol::auth::run_silent_challenge(&self.requester, &secret, &nest_id).await
    }

    /// `fauna.auth.claim_admin` — the trait impl owns the Ed25519 signing:
    /// derive the actor id from `secret_hex` and sign the domain-separated
    /// `claim_admin_signed_message(actor_id, timestamp)` =
    /// `CLAIM_ADMIN_V1 ‖ actor_id ‖ timestamp_be` (the tag is
    /// what stops a captured login signature from ever being a valid claim
    /// signature; tagged-only since the 2026-08-17 no-existing-users
    /// ratification). A bad secret hex is a *local* identity failure
    /// (`InvalidIdentity`), distinct from any server rejection.
    pub async fn claim_admin(
        &self,
        code: &str,
        secret_hex: &str,
        handle: &str,
        mail_domain: Option<&str>,
    ) -> Result<ClaimAdminResponse, ClaimAdminError> {
        let secret_bytes: [u8; 32] = fauna_core::hex32::decode(secret_hex).map_err(|e| {
            ClaimAdminError::InvalidIdentity {
                cause: e.to_string(),
            }
        })?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&secret_bytes);
        let actor_id_bytes = signing_key.verifying_key().to_bytes();
        let timestamp_ms = Timestamp::now_millis();
        // Single-sourced through `claim_admin_signed_message` so the signer and
        // the nest verifier can never drift.
        let tagged =
            fauna_protocol::claim::claim_admin_signed_message(&actor_id_bytes, timestamp_ms);
        let signature = hex::encode(signing_key.sign(&tagged).to_bytes());

        let req = fauna_protocol::claim::ClaimAdminRequest {
            claim_code: code.to_string(),
            actor_id: hex::encode(actor_id_bytes),
            timestamp: timestamp_ms,
            signature,
            handle: handle.to_string(),
            mail_domain: mail_domain.map(str::to_string),
            extra: Default::default(),
        };
        let reply: fauna_protocol::claim::ClaimAdminReply = self
            .requester
            .request(kinds::CLAIM_ADMIN, req)
            .await
            .map_err(map_claim_err)?;
        // The reqwest impl wraps the populated happy-path fields in `Some` so the
        // trait response can also represent the (here-unused) `ok = false` branch.
        Ok(ClaimAdminResponse {
            ok: true,
            token: Some(reply.token),
            expires_at: Some(reply.expires_at as i64),
            domain: Some(reply.domain),
            // Hand-off of the deployment signing seed for total-box-loss recovery
            // (`box-recovery.md` § Mechanism). `None` when the nest holds no signing key.
            deployment_seed: reply.deployment_seed,
        })
    }

    /// `fauna.account.invite_request.submit` — the body is the canonical signed
    /// payload the caller built (`machine.rs`); we only re-shape it to the wire
    /// type. The `InviteRequestStatus` reply is projected to the wizard's
    /// `InviteRequestResponse` (no `quota` on this wire surface → `None`).
    pub async fn submit_invite_request(
        &self,
        body: InviteRequestBody,
    ) -> Result<InviteRequestResponse, InviteRequestError> {
        let req = fauna_protocol::invite::InviteRequestSubmit {
            actor_id: body.actor_id,
            handle: body.handle,
            message: body.message,
            timestamp: body.timestamp,
            signature: body.signature,
            // Whatever the app set on the machine (`set_age_claim`) — `None`
            // from every app without a store age signal (family-safety.md
            // § The account age band).
            age_claim: body.age_claim,
            extra: Default::default(),
        };
        let reply: fauna_protocol::invite::InviteRequestStatus = self
            .requester
            .request(kinds::INVITE_SUBMIT, req)
            .await
            .map_err(map_invite_err)?;
        Ok(invite_status_to_response(reply))
    }

    /// `fauna.account.invite_request.cancel` — withdraw this actor's own
    /// request. Like `claim_admin` and the submit path, this impl owns the
    /// Ed25519 signing (the crate stays wasm-clean by not taking a native
    /// `fauna-client-core` dependency); the canonical message is built once by
    /// [`crate::machine::invite_request_cancel_signed_message`].
    pub async fn cancel_invite_request(&self, secret_hex: &str) -> Result<(), InviteRequestError> {
        let req = crate::machine::build_invite_request_cancel(secret_hex).ok_or(
            InviteRequestError::Malformed {
                cause: "invalid identity secret".into(),
            },
        )?;
        let _: fauna_protocol::invite::InviteRequestCancelReply = self
            .requester
            .request(kinds::INVITE_CANCEL, req)
            .await
            .map_err(map_invite_err)?;
        Ok(())
    }

    /// `fauna.account.invite_request.status` — poll a pending request. A
    /// `fauna.account.invite_request_not_found` rejection becomes `NotFound`
    /// (the wizard resolves it via the registered-probe).
    pub async fn recheck_invite_request(
        &self,
        actor_id_hex: &str,
    ) -> Result<InviteRequestResponse, InviteRequestError> {
        let req = fauna_protocol::invite::InviteRequestStatusQuery {
            actor_id: actor_id_hex.to_string(),
            extra: Default::default(),
        };
        let reply: fauna_protocol::invite::InviteRequestStatus = self
            .requester
            .request(kinds::INVITE_STATUS, req)
            .await
            .map_err(map_invite_err)?;
        Ok(invite_status_to_response(reply))
    }

    /// `fauna.account.invite_code.verify` — exchange an OOB code for an
    /// invite_id. Every server rejection is `Invalid` (the reqwest twin treated
    /// any non-2xx body as "not recognized").
    pub async fn verify_invite_code(
        &self,
        code: &str,
    ) -> Result<InviteCodeVerification, InviteCodeError> {
        let req = fauna_protocol::invite::InviteCodeVerify {
            code: code.to_string(),
            extra: Default::default(),
        };
        let reply: fauna_protocol::invite::InviteCodeVerifyReply = self
            .requester
            .request(kinds::INVITE_CODE_VERIFY, req)
            .await
            .map_err(map_invite_code_err)?;
        Ok(InviteCodeVerification {
            invite_id: reply.invite_id,
            supervised_by: reply.supervised_by,
        })
    }

    /// `fauna.account.age_nonce` — mint the single-use nonce the mobile app's
    /// platform attestation binds. Pre-identity like every kind here.
    pub async fn age_nonce(&self) -> Result<AgeNonce, AgeNonceError> {
        let reply: fauna_protocol::age::AgeNonceReply = self
            .requester
            .request(
                kinds::ACCOUNT_AGE_NONCE,
                fauna_protocol::age::AgeNonceRequest::default(),
            )
            .await
            .map_err(map_age_nonce_err)?;
        Ok(AgeNonce {
            nonce_hex: reply.nonce,
            expires_in_secs: reply.expires_in_secs,
            attestation_platforms: reply.attestation_platforms,
        })
    }

    /// `fauna.account.register` — final commit. The wizard treats register as
    /// binary ("succeeded or didn't"), so every failure collapses to `Failed`.
    pub async fn register(&self, body: RegisterBody) -> Result<RegisterResponse, RegisterError> {
        let req = fauna_protocol::account::RegisterRequest {
            actor_id: body.actor_id,
            handle: body.handle,
            timestamp: body.timestamp,
            signature: body.signature,
            invite_code: body.invite_code,
            // Whatever the app set on the machine (`set_age_claim`) — `None`
            // from every app without a store age signal.
            age_claim: body.age_claim,
            extra: Default::default(),
        };
        // The reply (actor coordinates) is discarded — the wizard only needs
        // success/failure, exactly as the reqwest twin drained the HTTP body.
        let _reply: fauna_protocol::account::RegisterReply = self
            .requester
            .request(kinds::REGISTER, req)
            .await
            .map_err(|e| RegisterError::Failed {
                cause: e.to_string(),
            })?;
        Ok(RegisterResponse { ok: true })
    }

    /// `fauna.setup.nat_mode` — commit the deployment NAT axis, **signing on
    /// this connection**. The ceremony (`transport-connection.md`, the
    /// nest-bound mode commit): first learn the possession-proven identity of
    /// the box this connection reaches (`nest_trust::read_login_binding` —
    /// the one reader every signer binds with: fresh nonce, SPKI-bound when
    /// the connector captured the received cert), then sign the nest-bound
    /// body. Signing after the read is what closes PROBE-482-B without a
    /// prior pin: the blob names exactly the nest it is being submitted to,
    /// so it verifies at no other. A box that proves no identity gets no
    /// commit (the unbound V1 degrade retired 2026-09-24 with the
    /// compat-remnant sweep).
    ///
    /// Every 4xx-class rejection (`invalid_request` / `signature_failed` /
    /// `not_claimed` / `forbidden`) is `Invalid`; `internal` and transport
    /// faults are transient; a refused read, a withheld binding, or a binding
    /// that fails verification is the typed identity verdict. Mutable
    /// server-side — there is no `mode_conflict`.
    pub async fn submit_nat_mode(
        &self,
        secret_hex: &str,
        mode: NodeMode,
    ) -> Result<(), NatModeError> {
        use fauna_client_core::nest_trust::{self, LoginBindingError};

        let nest_id = nest_trust::read_login_binding(&self.requester, self.captured_spki.as_ref())
            .await
            .map_err(|e| match e {
                LoginBindingError::Transport(_) => NatModeError::Transient {
                    cause: e.to_string(),
                },
                // A refused read, a withheld binding, or a binding that failed
                // verification — a verdict about the box's identity, not a
                // fault to retry past.
                LoginBindingError::Refused(_)
                | LoginBindingError::NoBinding
                | LoginBindingError::Binding(_) => NatModeError::IdentityMismatch {
                    reason: e.to_string(),
                },
            })?;
        let body =
            crate::machine::build_signed_nat_mode_body(secret_hex, mode, &hex::encode(nest_id))
                .ok_or(NatModeError::Invalid {
                    reason: "invalid identity secret".into(),
                })?;
        let req = fauna_protocol::nat_mode::NatModeRequest {
            mode: body.mode.as_str().to_string(),
            actor_id: body.actor_id,
            timestamp: body.timestamp,
            signature: body.signature,
            nest_id: body.nest_id,
            extra: Default::default(),
        };
        let _reply: fauna_protocol::nat_mode::NatModeReply = self
            .requester
            .request(kinds::NAT_MODE, req)
            .await
            .map_err(map_nat_mode_err)?;
        Ok(())
    }

    /// The `recovery_entry` restore, driven over this one pre-identity
    /// connection: resolve the account if the kit did not name it, then hand
    /// the ceremony to the shared `fauna_client_recovery::restore_seed` rather
    /// than re-composing challenge/sign/fetch/unseal here (priority #2 — the
    /// mapping core owns *which connection*, the recovery crate owns *the
    /// ceremony*).
    ///
    /// Lending the requester by reference is what lets both surfaces share the
    /// connection: `RecoveryClient` is generic over `R: RpcRequester`, and
    /// `&R` is one (`fauna_protocol::requester`'s borrowed blanket impl).
    pub async fn restore_escrowed_seed(
        &self,
        recovery_secret_hex: &str,
        actor_id_hex: Option<&str>,
        handle: Option<&str>,
    ) -> Result<RestoredIdentity, RestoreSeedError> {
        let recovery =
            fauna_core::recovery::RecoveryKey::from_hex(recovery_secret_hex).map_err(|e| {
                RestoreSeedError::Refused {
                    reason: format!("recovery secret: {e}"),
                }
            })?;

        let actor_id = match actor_id_hex {
            Some(hex) => fauna_core::hex32::decode(hex)
                .map(fauna_core::identity::ActorId)
                .map_err(|e| RestoreSeedError::Refused {
                    reason: format!("actor id: {e}"),
                })?,
            // The kit named no account, so the handle must — and the only
            // party that can turn a handle into an actor id is the nest it
            // lives on, which is the connection we are already holding.
            None => {
                let handle = handle.ok_or(RestoreSeedError::AccountUnnamed)?;
                self.resolve_actor_by_handle(handle).await?
            }
        };

        let client = fauna_client_recovery::RecoveryClient::new(&self.requester);
        let kit = fauna_client_recovery::ParsedKit {
            recovery,
            actor_id: Some(actor_id),
            handle: None,
        };
        let restored = fauna_client_recovery::restore_seed(&client, &kit, None)
            .await
            .map_err(map_restore_err)?;

        // The predecessor section never fails the restore — the account is back
        // either way — but an `Unreadable` one is reported verbatim so the
        // screen can say what was lost (`identity-succession.md` § Seed escrow).
        let predecessors_unreadable = match &restored.predecessors {
            fauna_client_recovery::PredecessorsOutcome::Unreadable(e) => {
                tracing::warn!(error = %e, "the escrow blob's predecessor section did not open");
                Some(e.to_string())
            }
            fauna_client_recovery::PredecessorsOutcome::Absent
            | fauna_client_recovery::PredecessorsOutcome::Opened(_) => None,
        };
        Ok(RestoredIdentity {
            seed_hex: fauna_core::hex32::encode(&restored.seed),
            predecessors: restored
                .opened_predecessors()
                .iter()
                .map(|p| RestoredPredecessorSeed {
                    actor_id_hex: fauna_core::hex32::encode(&p.actor_id),
                    seed_hex: fauna_core::hex32::encode(&p.seed).into(),
                })
                .collect(),
            predecessors_unreadable,
        })
    }

    /// `fauna.actor.by_handle` — the anonymous handle→actor lookup. An unknown
    /// handle is an answer the screen renders (the account field is wrong),
    /// never a fault.
    async fn resolve_actor_by_handle(
        &self,
        handle: &str,
    ) -> Result<fauna_core::identity::ActorId, RestoreSeedError> {
        let reply: fauna_protocol::discovery::ActorByHandleReply = self
            .requester
            .request(
                kinds::ACTOR_BY_HANDLE,
                fauna_protocol::discovery::ActorByHandleRequest {
                    handle: handle.to_string(),
                    // Left unset so the nest answers under its own canonical
                    // identity domain. Naming the domain the user typed would
                    // be rejected outright (`fauna.actor.domain_not_local`)
                    // whenever it differs from what the box serves — which is
                    // exactly the E2E redirect case, and a restore must not
                    // depend on the two agreeing.
                    domain: None,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| match e.as_rpc_error() {
                Some(_) => RestoreSeedError::AccountUnknown {
                    handle: handle.to_string(),
                },
                None => RestoreSeedError::Transient {
                    reason: e.to_string(),
                },
            })?;
        fauna_core::hex32::decode(&reply.actor_id)
            .map(fauna_core::identity::ActorId)
            .map_err(|e| RestoreSeedError::Transient {
                reason: format!("nest returned an unreadable actor id: {e}"),
            })
    }
}

/// `RecoveryError` → the four routes a restore screen takes.
///
/// `NotRegistered` funnels into `NoEscrow` deliberately: the goal doc ratifies
/// exactly two refusals for this screen to distinguish, and "never registered a
/// kit" and "the blob is gone" leave the user with the identical and only
/// remedy — re-create the kit from a signed-in device. Splitting them here
/// would put a distinction on screen that changes nothing the user can do.
fn map_restore_err(e: fauna_client_recovery::RecoveryError) -> RestoreSeedError {
    use fauna_client_recovery::RecoveryError as E;
    match e {
        E::NoEscrow | E::NotRegistered => RestoreSeedError::NoEscrow,
        E::Superseded { new_actor_id } => RestoreSeedError::Superseded {
            successor: fauna_core::format::hex_full(&new_actor_id),
        },
        // A retired kit still signs structurally valid bytes; they simply
        // authorize nothing now. Same for a blob this root cannot open.
        E::SignatureFailed | E::Crypto(_) => RestoreSeedError::Refused {
            reason: e.to_string(),
        },
        // A spent/expired nonce is worth another go — restarting the ceremony
        // draws a fresh one, which is what makes it unlike the refusals above.
        E::InvalidNonce | E::Transport(_) | E::Malformed(_) => RestoreSeedError::Transient {
            reason: e.to_string(),
        },
        other => RestoreSeedError::Refused {
            reason: other.to_string(),
        },
    }
}

impl<R: RpcRequester> std::fmt::Debug for WsRpcNestApi<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `R` is the opaque WS connector (no useful Debug); name the type only.
        f.debug_struct("WsRpcNestApi").finish_non_exhaustive()
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// The server's human-facing reason for a rejection: prefer the `RpcError`'s
/// text `details` (the twin surfaced the nest's `{"error": ...}` message), else
/// its `code`, else the transport error's `Display`.
fn reason_of<E: RpcErrorClass + core::fmt::Display>(e: &E) -> String {
    if let Some(rpc) = e.as_rpc_error() {
        if let Some(Value::String(s)) = rpc.details.as_deref()
            && !s.is_empty()
        {
            return s.clone();
        }
        return rpc.code.clone();
    }
    e.to_string()
}

/// `InviteRequestStatus` (full row) → the wizard's `InviteRequestResponse`. The
/// wire surface carries no quota cell (the HTTP twin's `status_json` didn't
/// either), so `quota` is `None`.
fn invite_status_to_response(
    s: fauna_protocol::invite::InviteRequestStatus,
) -> InviteRequestResponse {
    InviteRequestResponse {
        id: s.id,
        status: s.status,
        denial_reason: s.denial_reason,
        quota: None,
    }
}

fn map_claim_err<E: RpcErrorClass + core::fmt::Display>(e: E) -> ClaimAdminError {
    match e.as_rpc_error().map(|r| r.code.as_str()) {
        // 5xx-class: a server fault, retryable.
        Some("fauna.protocol.internal") => ClaimAdminError::Transient {
            cause: e.to_string(),
        },
        // The nest can't READ its own claim-code file (present but unreadable — a
        // misprovisioned box). Terminal and NOT a wrong code, so it gets its own
        // dedicated message rather than the user-actionable `Invalid` rendering.
        Some("fauna.auth.claim_code_unreadable") => ClaimAdminError::Misprovisioned {
            cause: reason_of(&e),
        },
        // The code was fine — the nest just already has an admin. Its own
        // dedicated message, not the generic `Invalid` rendering (which would
        // otherwise show the raw wire code, since this RpcError sets no
        // `.details`).
        Some("fauna.auth.already_claimed") => ClaimAdminError::AlreadyClaimed,
        // Every other rejection (invalid_request / signature_failed /
        // invalid_claim_code / handle_taken / malformed) was a
        // 4xx the HTTP twin surfaced as user-actionable `Invalid`.
        Some(_) => ClaimAdminError::Invalid {
            reason: reason_of(&e),
        },
        // Transport fault (disconnect / timeout / framing / decode).
        None => ClaimAdminError::Transient {
            cause: e.to_string(),
        },
    }
}

fn map_invite_err<E: RpcErrorClass + core::fmt::Display>(e: E) -> InviteRequestError {
    match e.as_rpc_error().map(|r| r.code.as_str()) {
        Some("fauna.account.invite_request_not_found") => InviteRequestError::NotFound,
        // Permanent until the admin restores (a suspended key holder gets this
        // code too — `login.md` § Errors), so it gets its own terminal arm
        // rather than the catch-all's "try again".
        Some("fauna.account.actor_exists") => InviteRequestError::AlreadyRegistered,
        // No invite condition maps to HTTP 403 (`Closed`) and the 429 rate-limit
        // is HTTP-only, so every other reachable rejection (invalid_request /
        // signature_failed / handle_taken / invite_request_exists /
        // invite_code_invalid / internal) funnels to Transient, as the reqwest
        // twin did for those statuses.
        Some(_) => InviteRequestError::Transient {
            cause: e.to_string(),
        },
        None => InviteRequestError::Transient {
            cause: e.to_string(),
        },
    }
}

fn map_age_nonce_err<E: RpcErrorClass + core::fmt::Display>(e: E) -> AgeNonceError {
    match e.as_rpc_error() {
        Some(_) => AgeNonceError::Refused {
            reason: reason_of(&e),
        },
        None => AgeNonceError::Transient {
            cause: e.to_string(),
        },
    }
}

fn map_invite_code_err<E: RpcErrorClass + core::fmt::Display>(e: E) -> InviteCodeError {
    match e.as_rpc_error() {
        // Any server rejection = "not recognized" (the reqwest twin mapped every
        // non-2xx body to Invalid).
        Some(_) => InviteCodeError::Invalid {
            reason: reason_of(&e),
        },
        None => InviteCodeError::Transient {
            cause: e.to_string(),
        },
    }
}

fn map_nat_mode_err<E: RpcErrorClass + core::fmt::Display>(e: E) -> NatModeError {
    match e.as_rpc_error().map(|r| r.code.as_str()) {
        Some("fauna.protocol.internal") => NatModeError::Transient {
            cause: e.to_string(),
        },
        // invalid_request / signature_failed / not_claimed / forbidden /
        // malformed — the 4xx-class rejects (no mode_conflict: the NAT axis is
        // mutable).
        Some(_) => NatModeError::Invalid {
            reason: reason_of(&e),
        },
        None => NatModeError::Transient {
            cause: e.to_string(),
        },
    }
}

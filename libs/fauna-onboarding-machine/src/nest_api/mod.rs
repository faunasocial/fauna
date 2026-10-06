//! Trait abstraction for the wizard's nest-side HTTP surface.
//!
//! All onboarding-machine calls to the user's nest go through this trait.
//! Production code uses [`WsNestApi`] (the pre-identity WS-RPC connector — a
//! thin per-target wrapper over the shared [`WsRpcNestApi`] mapping core). The
//! legacy `reqwest`-based HTTP impl was removed once onboarding rode WS-RPC on
//! every app. Tests use `FakeNestApi` (gated
//! under `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`) to fixture responses
//! without any transport.
//!
//! Relationship to [`fauna_nest_http`] (the consolidated native nest-HTTP
//! crate): `NestApi` is intentionally **distinct** from that crate's generic
//! `NestContentApi` — a fixed nine-method shape with rich typed responses the
//! wizard's snapshot builder consumes directly, endpoint-specific status
//! handling, and it compiles to wasm. What `nest_api` *does* share: the
//! [`fauna_nest_http::ApiError`] taxonomy (the per-endpoint error enums in
//! `types.rs` carry `From<ApiError>`). (We depend on `fauna-nest-http` with
//! `default-features = false` — only the portable `error` + `paths` subset, not
//! its native `client` layer; the onboarding nest surface migrated to
//! pre-identity WS-RPC kinds, so the former `paths::onboarding` constants are
//! gone and only `ApiError` is used today.) Merging the two traits is
//! a possible future direction, not done (tracked internally).
//!
//! When adding a new method here, mirror it in `ws_rpc_impl.rs` (the live
//! transport) and `fake.rs`; add the kind to `ws_rpc_impl::kinds` + the nest's
//! `pre_identity_allowlist.rs`; and cover it in
//! `bins/fauna-nest/tests/onboarding_ws_rpc_roundtrip.rs`. The `WsNestApi`
//! wrapper forwards generically, so it needs no per-method edit.

pub mod fake;
pub mod types;
pub mod ws_nest_api;
pub mod ws_rpc_impl;

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use fake::{FAKE_NEST_ID, FakeNestApi};
// `types::*` carries `SilentChallengeOutcome` (re-exported there from
// `fauna-protocol::auth`) — the `NestApi::silent_challenge` return type.
pub use types::*;
pub use ws_nest_api::WsNestApi;
pub use ws_rpc_impl::WsRpcNestApi;

use async_trait::async_trait;

// On wasm32 each method of the production impl (`WsNestApi`) opens a fresh
// `Rc`-based anonymous WS-RPC connector (`fauna_rpc_wasm::AnonymousWsRpcClient`)
// and holds it across the request `await`, so the method future is `!Send`;
// `#[async_trait]`'s default boxing wraps it as `dyn Future + Send`, which it
// can't satisfy. Use the `?Send` variant on wasm. On native we keep the `Send`
// boxing — the wizard's background task is driven by `tokio::spawn` (requires
// `Send`); on wasm it's `wasm_bindgen_futures::spawn_local` (doesn't).
//
// The `Send + Sync` supertrait constrains the trait *object* (`dyn NestApi`,
// behind the `Arc<dyn NestApi>` the machine holds), which every impl struct
// satisfies on every target — `WsNestApi` holds only an `Option<String>`; the
// `!Send` connector lives only inside a method's future, never in the struct.
// Only the boxed futures differ per target (the `?Send` above).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait NestApi: Send + Sync + std::fmt::Debug {
    /// `GET /api/v1/setup-status` — discriminates claimed/unclaimed nest
    /// for the handle-check probe path, and (Phase C) carries the
    /// encryption-mode pin.
    async fn probe_setup_status(&self, base_url: &str) -> Result<SetupStatus, ProbeError>;

    /// `fauna.auth.{challenge,verify}` — the handle-check probe's silent
    /// sign-in: run the challenge/verify ceremony with the secret behind
    /// `secret_hex` to learn whether this keypair is registered on the nest at
    /// `base_url`, and under what handle. `SilentChallengeOutcome::Success`
    /// carries the registered handle (in the `VerifyReply`); `NotRegistered`
    /// means the actor is unknown there; `Transient`/`SecretInvalid` are the
    /// retryable / terminal failure buckets. Pre-identity, anonymous WS — no
    /// bearer. (Migrated off the HTTP twin `POST /api/v1/auth/{challenge,verify}`
    /// — `fauna-provisioning::probe::nest_challenge_at` — in the WS-RPC
    /// everywhere migration.)
    async fn silent_challenge(&self, base_url: &str, secret_hex: &str) -> SilentChallengeOutcome;

    /// `POST /api/v1/claim-admin` — wizard's `claim_code` page submits a
    /// one-time code + Ed25519-signed payload. The trait impl owns the
    /// signing. `handle` is **required** — it registers the admin's handle in
    /// the same request, so claiming an unclaimed nest makes that handle the
    /// admin's address; a handle-less admin is unrepresentable (the nest rejects
    /// an empty handle, and callers guard it before reaching here). `mail_domain`
    /// (when `Some`) is the `@domain` suffix the wizard handle carried
    /// (`alice@fauna.test` → `fauna.test`); the nest auto-registers it as a mail
    /// domain so the handle is a routable email with no manual admin-dns
    /// add-domain step.
    async fn claim_admin(
        &self,
        base_url: &str,
        code: &str,
        secret_hex: &str,
        handle: &str,
        mail_domain: Option<&str>,
    ) -> Result<ClaimAdminResponse, ClaimAdminError>;

    /// `POST /api/v1/invite-requests` — request admission to a claimed
    /// nest. Body is the canonical signed payload built by the caller.
    async fn submit_invite_request(
        &self,
        base_url: &str,
        body: InviteRequestBody,
    ) -> Result<InviteRequestResponse, InviteRequestError>;

    /// `fauna.account.invite_request.cancel` — withdraw this actor's own
    /// request, signed with the identity behind `secret_hex`.
    ///
    /// The nest refuses a second request while any row exists for the actor, so
    /// a **denied** requester can only re-apply by cancelling first: the wizard
    /// runs cancel-then-submit as one gesture (`onboarding.md` § The
    /// pending-invite surface — "re-submitting runs the cancel-then-submit
    /// sequence"). Idempotent by the kind's own contract — a replay finds no row
    /// and still succeeds — so a retry after a lost response is safe.
    async fn cancel_invite_request(
        &self,
        base_url: &str,
        secret_hex: &str,
    ) -> Result<(), InviteRequestError>;

    /// `GET /api/v1/invite-requests/{actor_id}/status` — poll the
    /// pending request. 404 maps to `InviteRequestError::NotFound`, which
    /// the wizard resolves via the registered-probe (approval vs. genuinely
    /// gone — `onboarding.md` § The pending-invite surface).
    async fn recheck_invite_request(
        &self,
        base_url: &str,
        actor_id_hex: &str,
    ) -> Result<InviteRequestResponse, InviteRequestError>;

    /// `POST /api/v1/invite-code/verify` — exchange an OOB code for an
    /// invite_id. Used by the OOB-code row on the invite-request page.
    async fn verify_invite_code(
        &self,
        base_url: &str,
        code: &str,
    ) -> Result<InviteCodeVerification, InviteCodeError>;

    /// `fauna.account.age_nonce` — mint the single-use nonce a mobile app's
    /// platform attestation binds (`family-safety.md` § The account age band).
    async fn age_nonce(&self, base_url: &str) -> Result<AgeNonce, AgeNonceError>;

    /// `POST /api/v1/register` — final commit step for both invite
    /// paths (Approved request: omit invite_code; OOB code: include it).
    async fn register(
        &self,
        base_url: &str,
        body: RegisterBody,
    ) -> Result<RegisterResponse, RegisterError>;

    /// `fauna.setup.nat_mode` — wizard's `nat_mode_choice` page commits the
    /// admin's NAT-axis choice (Public vs Private), Ed25519-signed. **Signs
    /// inside the impl, on the commit connection**: the nest-bound form needs
    /// the identity of the box the connection reaches (learned
    /// possession-proven, `read_login_binding`), which does not exist before
    /// the connection does — so the trait takes
    /// the signing secret and the mode, not a pre-signed body (the
    /// `restore_escrowed_seed` / `silent_challenge` hex-secret convention).
    /// **Mutable** server-side: any valid admin-signed set upserts the
    /// `nest_nat_mode` row (no conflict reply), so resubmit is always safe.
    /// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
    async fn submit_nat_mode(
        &self,
        base_url: &str,
        secret_hex: &str,
        mode: NodeMode,
    ) -> Result<(), NatModeError>;

    /// `fauna.actor.by_handle` + `fauna.recovery.escrow.{challenge,fetch}` —
    /// the `recovery_entry` screen's phrase-only restore (`onboarding.md` § 1
    /// Identity). Returns the recovered identity seed as 64-hex, ready to hand
    /// to [`crate::OnboardingMachine::confirm_imported_identity`].
    ///
    /// All of it rides **one** pre-identity connection, because by construction
    /// there is no device left to authenticate with — that is the whole point
    /// of the escrow path (`identity-succession.md` § Seed escrow →
    /// *Restore path*).
    ///
    /// `actor_id_hex` is the account the kit payload named, when it named one;
    /// it stays authoritative (the blob is AAD-bound to it). Only when it is
    /// absent does `handle` get resolved on the nest to find the account —
    /// `handle` is otherwise just how the *caller* found `base_url` in the
    /// first place, since a handle's `@domain` is the only half of a kit that
    /// locates a nest. Pass the bare local part: the nest stores handles
    /// without the `@domain` suffix.
    ///
    /// The seed crosses this boundary as a plain hex `String`, uniform with
    /// `silent_challenge`/`claim_admin` and with the wizard's own
    /// `imported_secret` — the identity secret's custody rules are the
    /// wizard's, and this path deliberately introduces no second set.
    ///
    /// [`RestoredIdentity`] carries the blob's predecessor section alongside
    /// the seed: a restore run inside a succession's corpus re-seal window
    /// recovers the material that opens the not-yet-re-sealed corpus, and a
    /// section that failed to open is reported rather than dropped.
    async fn restore_escrowed_seed(
        &self,
        base_url: &str,
        recovery_secret_hex: &str,
        actor_id_hex: Option<&str>,
        handle: Option<&str>,
    ) -> Result<RestoredIdentity, RestoreSeedError>;
}

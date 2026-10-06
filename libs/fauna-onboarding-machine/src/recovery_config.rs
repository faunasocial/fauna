//! Recovery-mode custody read — resolve a lost box's custodied deployment seed +
//! domain for the step-4 cloud re-provision drive (`box-recovery.md` § Recovery
//! UI (step 4), leg 3), through the pre-login resolver of
//! `box-recovery.md` § The plane-era recovery floor, *(b)*: **this device's own
//! account store joined with a cold read from a reachable nest**, never
//! either-or (`fauna_account_plane::deployment_seed_recovery`).
//!
//! ## Why this is a separate seam from `nest_api`
//!
//! The onboarding machine's `nest_api` is the **anonymous, pre-identity** WS-RPC
//! surface (claim / invite / register / handle-check) — one fresh bearer-less
//! connection per call, no retained session. The cold read is the opposite: the
//! fleet walk and the escrow get are not pre-identity kinds, so it needs an
//! **authenticated** connection. Recovery is inherently post-identity — the
//! admin has imported their identity (Q2-A: `identity_import → handle_entry →
//! AlreadyOnNest → nest_recovery`), so `state.imported_secret` (→ owner
//! keypair) and, when one was entered, `state.nest_url` (→ a surviving nest)
//! are in hand, but **no authed connection is retained** (the handle-check
//! silent-challenge token is transient). So this reader **mints its own
//! bearer** over the shared challenge/verify ceremony and opens a one-shot
//! authed read. With no nest URL, or when the nest cannot be reached, the local
//! read still answers — the case recovery exists for is the surviving device
//! whose saved nest is the dead box.
//!
//! ## Why a `#[cfg]`-selected reader, not an injected constructor param
//!
//! `OnboardingMachine::new` is a `#[uniffi::constructor]` — native apps
//! (apple/android/windows) construct the machine directly through the UniFFI
//! binding, so a new required constructor arg would be a fleet-breaking binding
//! change, and foreign code cannot inject a Rust `dyn` trait object anyway. So
//! the machine **self-wires** the per-target production reader in `new()` (via
//! [`production_reader`], over the platform store root); a sandboxed phone
//! shell re-roots it through `OnboardingMachine::set_store_container_dir`, and
//! tests override it with a fake through a crate-internal setter. The transport
//! dep for the authed read lives in the target's own leaf crate —
//! `fauna-rpc-wasm` on wasm, `fauna-anon-client` on native. The machine must
//! **not** pull the authed `NestClient` from `fauna-client`, which would form
//! the documented `fauna-client → fauna-nest-http → fauna-launch-machine` Cargo
//! cycle; `fauna-anon-client` is the leaf crate that already holds the
//! anonymous connect + the channel-binding graduation the native mint needs.
//!
//! ## Per-target transport trust
//!
//! Both readers share the resolution core ([`resolve_input`]) but legitimately
//! diverge in how they secure the transport, because the platforms have
//! different TLS-trust actors:
//!
//! - **wasm** relies on the **browser's WebPKI** for both the anonymous connect
//!   (bearer mint via `run_silent_challenge`) and the authed read — the browser
//!   owns cert acceptance, so the reader does no channel binding of its own.
//! - **native** has no such actor: `fauna-anon-client`'s anonymous connect uses a
//!   *capturing* (accept-any, encrypt-only) verifier, so a plain-WebPKI mirror of
//!   the wasm path would (a) leave the bearer mint MITM-exposed and (b) **reject**
//!   a self-signed / LAN / DNS-`self=` surviving nest outright. So the native
//!   reader follows the established native authed-read pattern: mint the bearer
//!   over **`fauna.auth.handshake`** (`mint_bearer_over_handshake`, which
//!   **graduates the channel binding** — pins the served SPKI for a self-signed
//!   nest, no-op for WebPKI), then open the authed read via
//!   [`fauna_anon_client::TokenNestClient`], whose `wss://` connect requires that
//!   graduated pin (else strict WebPKI). This is MITM-safe *and* works against a
//!   self-signed surviving box (`security.md` § Transport trust Axis 1).
//!
//! Which box the connection proves itself to be does not gate the read: the
//! cold read's rows certify themselves (a row names its box and carries a seed
//! that derives to it), so reading box A's seed from surviving box B is the
//! point, not a hazard.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_account_plane::deployment_seed_recovery::{StoreRoot, resolve_deployment_seeds_over};
use fauna_core::data::DeploymentSeedEntry;
use fauna_protocol::RpcRequester;

/// The seed + domain a recovery-mode cloud re-provision installs for the box
/// being recovered. Constructed by both target readers + the unit-test fake.
#[derive(Debug, Clone)]
pub(crate) struct RecoveryProvisionInput {
    /// The box's **custodied** deployment seed — installed as
    /// `CloudInitParams.deployment_seed` so the rebuilt box re-presents the same
    /// `nest_actor_id`. Resolved in Rust from the folded custody map
    /// (`DeploymentSeedEntry::seed_for`); **never surfaced to JS**. It ends up hex-rendered into cloud-init user-data at
    /// the provision boundary — the accepted exposure
    /// (`box-recovery.md` § Trust & audience) — so, like the cloud-generate
    /// path, it is held plain here (a wrap buys nothing when the destination is
    /// IMDS-readable user-data).
    pub deployment_seed: [u8; 32],
    /// The box's own handle domain (`DeploymentSeedEntry::domain_for`) — the
    /// cloud-init `server_name` + the A/AAAA DNS re-point target. `None` for a
    /// **domainless** (private home-relay) box, which cannot be
    /// cloud-re-provisioned (the caller surfaces that as an error → recover via
    /// self-hosted instead).
    pub domain: Option<String>,
}

/// Failure modes of a recovery-mode custody read. Both variants are constructed
/// on both targets.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RecoveryConfigError {
    /// No source answered — the local read failed and the nest could not be
    /// reached or read — or the box is absent locally and the nest could not
    /// be asked.
    #[error("{0}")]
    Failed(String),
    /// Every source asked answered, and none custodies a seed for this box.
    #[error("box {0} has no custodied deployment seed on this device or the reachable nest")]
    SeedNotFound(String),
}

/// Resolves a lost box's custodied seed + domain. The trait object
/// (`Arc<dyn RecoveryConfigReader>`) is `Send + Sync`; the `!Send` connector on
/// wasm lives only inside the method future (the `?Send` boxing below), never
/// in the impl struct — the same shape as [`crate::nest_api::NestApi`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub(crate) trait RecoveryConfigReader: Send + Sync + std::fmt::Debug {
    /// Resolve the custodied seed + domain for the box `nest_actor_id_hex`, as
    /// the owner behind `owner_secret_hex`: this device's own store, joined
    /// with a cold read from the nest at `nest_url` when one is given.
    async fn resolve(
        &self,
        nest_url: Option<&str>,
        owner_secret_hex: &str,
        nest_actor_id_hex: &str,
    ) -> Result<RecoveryProvisionInput, RecoveryConfigError>;
}

/// The per-target production reader over `root` — [`StoreRoot::platform`]
/// from `OnboardingMachine::new`, a phone's container once the shell names it.
pub(crate) fn production_reader(root: StoreRoot) -> Arc<dyn RecoveryConfigReader> {
    #[cfg(target_arch = "wasm32")]
    {
        Arc::new(WasmRecoveryConfigReader { root })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Arc::new(NativeRecoveryConfigReader { root })
    }
}

/// Decode a 64-char hex string to a 32-byte array (both target readers use it).
fn hex32(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s).map_err(|e| e.to_string())?;
    bytes
        .try_into()
        .map_err(|_| "expected 32 bytes (64 hex chars)".to_string())
}

/// The resolution core both readers share: the resolver over `cold` (the
/// authed connection, when the reader made one), then the resolution-point
/// reads for `box_id`. `unreached` is why no cold read was possible when a
/// nest URL was given — kept so a box the local read lacks reports that the
/// nest could not be asked, never that the box is uncustodied.
async fn resolve_input<R: RpcRequester>(
    root: &StoreRoot,
    secret: &[u8; 32],
    cold: Option<&R>,
    unreached: Option<String>,
    nest_actor_id_hex: &str,
) -> Result<RecoveryProvisionInput, RecoveryConfigError> {
    let box_id = hex32(nest_actor_id_hex)
        .map_err(|e| RecoveryConfigError::Failed(format!("box id: {e}")))?;
    let resolved = resolve_deployment_seeds_over(root, secret, cold).await;
    let cold_failure = unreached.or_else(|| {
        resolved
            .cold_failure
            .as_ref()
            .map(|e| format!("cold read: {e:#}"))
    });
    let seeds = resolved
        .into_result()
        .map_err(|e| RecoveryConfigError::Failed(format!("{e:#}")))?;
    match DeploymentSeedEntry::seed_for(&seeds, &box_id) {
        Some(deployment_seed) => Ok(RecoveryProvisionInput {
            deployment_seed,
            domain: DeploymentSeedEntry::domain_for(&seeds, &box_id),
        }),
        None => Err(match cold_failure {
            Some(why) => RecoveryConfigError::Failed(format!(
                "box {nest_actor_id_hex} is not custodied on this device, and the nest could \
                 not be read: {why}"
            )),
            None => RecoveryConfigError::SeedNotFound(nest_actor_id_hex.to_string()),
        }),
    }
}

// ── wasm production reader ──────────────────────────────────────────────────

/// Web reader: mint a bearer over the anonymous connection (the shared
/// challenge/verify ceremony), open a one-shot authed connection, and resolve.
/// This keeps the whole read — and crucially the resolved seed — inside the
/// `fauna-wasm-onboarding` cdylib, so the seed cannot cross via JS.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(crate) struct WasmRecoveryConfigReader {
    root: StoreRoot,
}

#[cfg(target_arch = "wasm32")]
impl WasmRecoveryConfigReader {
    /// Mint a bearer and open the authed connection to `nest_url`.
    async fn connect(
        nest_url: &str,
        secret: &[u8; 32],
    ) -> Result<fauna_rpc_wasm::TokenWsRpcClient, String> {
        use fauna_core::identity::ActorKeypair;
        use fauna_protocol::auth::{SilentChallengeOutcome, run_silent_challenge};
        use fauna_rpc_wasm::{AnonymousWsRpcClient, TokenWsRpcClient};

        let actor_id_hex = ActorKeypair::from_secret(*secret).actor_id_hex();
        // 1. Mint a bearer over the anonymous connection via the shared
        //    challenge/verify ceremony (priority #2 — same path the handle-check
        //    probe uses; the token is not retained in machine state), binding
        //    the identity read off the connection first (`login.md` § Binding
        //    the nest; possession-only on wasm).
        let anon = AnonymousWsRpcClient::connect(nest_url)
            .map_err(|e| format!("anon connect {nest_url}: {e}"))?;
        let nest_id = fauna_client_core::nest_trust::read_login_binding(&anon, None)
            .await
            .map_err(|e| format!("nest identity: {e}"))?;
        let token = match run_silent_challenge(&anon, secret, &nest_id).await {
            SilentChallengeOutcome::Success(reply) => reply.token,
            SilentChallengeOutcome::NotRegistered => {
                return Err("identity is not registered on the surviving nest".into());
            }
            SilentChallengeOutcome::Transient { error } => {
                return Err(format!("transient: {error}"));
            }
            SilentChallengeOutcome::NeedsUpdate { message } => return Err(message),
            SilentChallengeOutcome::SecretInvalid { error } => return Err(error),
            // The owner identity was succeeded, so it no longer owns the account
            // this read authenticates as. Terminal for the reason no retry can
            // change: the old key still signs perfectly, it simply earns the
            // same refusal every time (`identity-succession.md` § Propagation
            // → *Own device fleet*). The successor is reported as **claimed**,
            // never as fact — this path has no registration chain in hand to
            // verify it against, and the way out is importing the successor on
            // the launch surface, not anything a recovery read can offer.
            // The surviving nest refuses this account until `locked_until`;
            // nothing a recovery read can offer clears it before then.
            SilentChallengeOutcome::Locked { locked_until_secs } => {
                return Err(format!(
                    "this account is locked until {locked_until_secs} (Unix seconds); retry \
                     recovery after the lock ends"
                ));
            }
            SilentChallengeOutcome::Superseded { new_actor_id_hex } => {
                return Err(format!(
                    "this identity was succeeded — the account now claims to belong to \
                     {new_actor_id_hex}; import the successor identity and retry recovery"
                ));
            }
            // The surviving nest's pinned identity does not match what answered.
            // Terminal, exactly as the handle-check probe treats it (`machine.rs`,
            // the `IdentityChanged` arm): a retry cannot change the verdict, and
            // the re-trust affordance is the LAUNCH surface, not a recovery read.
            SilentChallengeOutcome::IdentityChanged {
                host,
                pinned_hex,
                seen_hex,
                fork: _,
            } => {
                return Err(format!(
                    "nest identity changed for {host}: pinned {pinned_hex}, saw {}",
                    seen_hex.as_deref().unwrap_or("unknown")
                ));
            }
        };
        drop(anon); // release the anonymous connection; the authed read follows.

        // 2. One-shot authed connection with that bearer.
        TokenWsRpcClient::connect(nest_url, &actor_id_hex, &token)
            .map_err(|e| format!("authed connect: {e}"))
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl RecoveryConfigReader for WasmRecoveryConfigReader {
    async fn resolve(
        &self,
        nest_url: Option<&str>,
        owner_secret_hex: &str,
        nest_actor_id_hex: &str,
    ) -> Result<RecoveryProvisionInput, RecoveryConfigError> {
        let secret = hex32(owner_secret_hex)
            .map_err(|e| RecoveryConfigError::Failed(format!("owner secret: {e}")))?;
        let (authed, unreached) = match nest_url {
            None => (None, None),
            Some(url) => match Self::connect(url, &secret).await {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(e)),
            },
        };
        resolve_input(
            &self.root,
            &secret,
            authed.as_ref(),
            unreached,
            nest_actor_id_hex,
        )
        .await
    }
}

// ── native production reader ────────────────────────────────────────────────

/// Native reader: mint a bearer over `fauna.auth.handshake` (which **graduates
/// the channel binding** — pins the served SPKI for a self-signed nest, no-op for
/// WebPKI), open a one-shot authed connection ([`fauna_anon_client::
/// TokenNestClient`], pinned-else-WebPKI), and resolve. The transport lives in
/// the `fauna-anon-client` leaf crate (not `fauna-client` — the documented Cargo
/// cycle); the resolution core is [`resolve_input`], the same the wasm reader
/// uses. See the module § *Per-target transport trust* for why native mints
/// over the handshake rather than the wasm reader's `run_silent_challenge`
/// (which cannot graduate).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct NativeRecoveryConfigReader {
    root: StoreRoot,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeRecoveryConfigReader {
    /// Mint a bearer over the handshake and open the authed connection.
    async fn connect(
        nest_url: &str,
        secret: &[u8; 32],
    ) -> Result<fauna_anon_client::TokenNestClient, String> {
        use fauna_anon_client::{TokenNestClient, mint_bearer_over_handshake};
        use fauna_core::identity::ActorKeypair;

        let keypair = ActorKeypair::from_secret(*secret);
        let minted =
            mint_bearer_over_handshake(nest_url, keypair.actor_id().0, keypair.signing_key())
                .await
                .map_err(|e| format!("handshake mint: {e}"))?;
        TokenNestClient::connect(nest_url, &keypair.actor_id_hex(), &minted.token)
            .await
            .map_err(|e| format!("authed connect: {e}"))
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl RecoveryConfigReader for NativeRecoveryConfigReader {
    async fn resolve(
        &self,
        nest_url: Option<&str>,
        owner_secret_hex: &str,
        nest_actor_id_hex: &str,
    ) -> Result<RecoveryProvisionInput, RecoveryConfigError> {
        let secret = hex32(owner_secret_hex)
            .map_err(|e| RecoveryConfigError::Failed(format!("owner secret: {e}")))?;
        // The requester's AFIT futures carry a `Send` the compiler cannot prove
        // for every lifetime, while this seam's future must be `Send`. So the
        // connect and the resolution run to completion on the runtime's
        // blocking pool over owned inputs, inside the same runtime (the
        // connection's I/O and timers stay on it).
        let root = self.root.clone();
        let nest_url = nest_url.map(str::to_owned);
        let box_hex = nest_actor_id_hex.to_string();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                let (authed, unreached) = match nest_url {
                    None => (None, None),
                    Some(url) => match Self::connect(&url, &secret).await {
                        Ok(c) => (Some(c), None),
                        Err(e) => (None, Some(e)),
                    },
                };
                resolve_input(&root, &secret, authed.as_ref(), unreached, &box_hex).await
            })
        })
        .await
        .map_err(|e| RecoveryConfigError::Failed(format!("custody read task: {e}")))?
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    type NoNest = fauna_anon_client::TokenNestClient;

    const BOX: &str = "11111111111111111111111111111111111111111111111111111111111111aa";

    /// A root no store was ever opened under — the local read opens existing
    /// stores only and creates nothing, so this is a device holding none.
    fn empty_device() -> StoreRoot {
        StoreRoot::at(std::env::temp_dir().join("fauna-recovery-config-test-no-store"))
    }

    /// No store on this device and no nest asked: every source answered, and
    /// none custodies the box — "not custodied", not a failure.
    #[tokio::test]
    async fn an_empty_device_with_no_nest_reports_the_box_uncustodied() {
        let err = resolve_input::<NoNest>(&empty_device(), &[7; 32], None, None, BOX)
            .await
            .unwrap_err();
        assert!(
            matches!(err, RecoveryConfigError::SeedNotFound(_)),
            "{err:?}"
        );
    }

    /// A nest URL was given but the nest could not be reached: a box the local
    /// read lacks reports that the nest could not be asked, never that the box
    /// is uncustodied (the surviving device's saved nest is the dead box).
    #[tokio::test]
    async fn an_unreached_nest_is_never_reported_as_an_uncustodied_box() {
        let err = resolve_input::<NoNest>(
            &empty_device(),
            &[7; 32],
            None,
            Some("authed connect: refused".into()),
            BOX,
        )
        .await
        .unwrap_err();
        match err {
            RecoveryConfigError::Failed(why) => assert!(why.contains("refused"), "{why}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}

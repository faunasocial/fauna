//! Clause (c) as one gesture: a seed-alone replacement **requested at every
//! linked nest**, and a veto that **contests the window at every nest that
//! holds one** (`identity-succession.md` § Enforcement on the home nest →
//! *Every nest the identity is linked to*).
//!
//! [`crate::replacement`] owns the per-nest halves
//! ([`request_seed_alone_replacement_at`], [`veto_pending_replacement`]); this
//! module is the composition every app's Settings gesture runs: list the
//! account's linked nests from its pairing rows on the bound session
//! (`fauna_client_core::linked_nests`, the secondary leg's own rule), open a
//! connection to each through the host's [`LinkedNestDial`], check that the
//! connection is bound to the identity the pairing row names, and call the
//! per-nest half. The bound nest's answer is the gesture's answer: a linked
//! nest that cannot be listed, reached or asked is reported
//! ([`LinkedFanOut`], and a `warn` log), never a failure of the gesture.
//!
//! Two connections, by design: the **request** is USER class, so it needs an
//! owner-authenticated connection ([`LinkedNestDial::owner`]); the **veto** is
//! pre-identity, so it rides an anonymous one ([`LinkedNestDial::anonymous`]),
//! never a logged-in session. A linked nest unreachable at request time is not
//! lost: the secondary leg sends the owed seed-alone link once the bound nest
//! lands it (`fauna_client_core::recovery_chain`, the `Owed` answer). An
//! unreachable nest at veto time keeps its window loud on the banner (the
//! leg's linked readings) until the next veto reaches it.
//!
//! Wasm-clean: the dial is the host's. Native hosts (tui, linux, the FFI)
//! take [`native::NativeLinkedNestDial`]; web implements its own over its
//! wasm transport.

use async_trait::async_trait;
use fauna_client_core::linked_nests::{
    KIND_PAIR_LIST, LinkedConnection, LinkedNestTarget, linked_targets,
};
use fauna_core::MaybeSendSync;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_protocol::pair::{PairListReply, PairListRequest};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::error::Result;
use crate::nest::RecoveryClient;
use crate::replacement::{
    PendingKit, request_seed_alone_replacement, request_seed_alone_replacement_at,
    veto_pending_replacement,
};
use crate::restore::ParsedKit;

/// The host's two connections to a linked nest. Each answers the requester
/// and the identity **that connection** is bound to (the origin's pin, else a
/// possession proof over it — never the nest's own `fauna.nest.info` claim);
/// the fan-out compares it with the pairing row's nest id and sends nothing
/// over a mismatch. An `Err` is "unreachable": the dial failed or timed out.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait LinkedNestDial: MaybeSendSync {
    /// The owner-authenticated (USER-class) connection's requester.
    type Owner: RpcRequester;
    /// The anonymous connection's requester.
    type Anonymous: RpcRequester;

    /// A connection to `target` signed in as the account's identity — what
    /// the seed-alone request needs.
    async fn owner(
        &self,
        target: &LinkedNestTarget,
    ) -> std::result::Result<LinkedConnection<Self::Owner>, String>;

    /// An anonymous connection to `target` — what the veto needs.
    async fn anonymous(
        &self,
        target: &LinkedNestTarget,
    ) -> std::result::Result<LinkedConnection<Self::Anonymous>, String>;
}

/// How one linked nest answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedNestOutcome<T> {
    /// The nest answered: the request's `lands_at`, or whether a veto
    /// cancelled anything.
    Answered(T),
    /// The host could not open the connection.
    Unreachable(String),
    /// The connection is bound to another identity than the pairing row's:
    /// nothing was sent.
    IdentityMismatch { presented: [u8; 32] },
    /// The nest refused, or the exchange failed.
    Failed(String),
}

/// One linked nest's answer, named by its pairing row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedNestAnswer<T> {
    pub nest_id: [u8; 32],
    pub nest_url: String,
    pub outcome: LinkedNestOutcome<T>,
}

/// What the gesture did at the linked nests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedFanOut<T> {
    /// The account's pairings could not be listed on the bound session:
    /// nothing was attempted at any linked nest.
    pub unlisted: Option<String>,
    /// Every linked nest the pairing rows name, in the list's order.
    pub nests: Vec<LinkedNestAnswer<T>>,
}

impl<T> Default for LinkedFanOut<T> {
    fn default() -> Self {
        Self {
            unlisted: None,
            nests: Vec::new(),
        }
    }
}

/// The linked nests the account's pairing rows name, listed on the bound
/// session (`fauna.pair.list`, USER class).
///
/// Nothing is excluded as "the bound nest": a nest's own pairing rows name the
/// *other* nests, and were one to name the bound nest itself, the request
/// there again is a replay it answers with the clock it already runs, and a
/// second veto an honest `false`.
pub async fn linked_nests<R>(
    bound: &RecoveryClient<R>,
) -> std::result::Result<Vec<LinkedNestTarget>, String>
where
    R: RpcRequester,
{
    let reply: PairListReply = bound
        .transport()
        .request(
            KIND_PAIR_LIST,
            PairListRequest {
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| format!("{KIND_PAIR_LIST}: {e}"))?;
    Ok(linked_targets(&reply, &[]))
}

/// Open a connection through `dial` and check its binding.
fn checked<R>(
    target: &LinkedNestTarget,
    dialled: std::result::Result<LinkedConnection<R>, String>,
) -> std::result::Result<R, LinkedNestOutcome<std::convert::Infallible>> {
    match dialled {
        Err(e) => Err(LinkedNestOutcome::Unreachable(e)),
        Ok(conn) if conn.bound_identity != target.nest_id => {
            Err(LinkedNestOutcome::IdentityMismatch {
                presented: conn.bound_identity,
            })
        }
        Ok(conn) => Ok(conn.rpc),
    }
}

fn widen<T>(outcome: LinkedNestOutcome<std::convert::Infallible>) -> LinkedNestOutcome<T> {
    match outcome {
        LinkedNestOutcome::Answered(never) => match never {},
        LinkedNestOutcome::Unreachable(e) => LinkedNestOutcome::Unreachable(e),
        LinkedNestOutcome::IdentityMismatch { presented } => {
            LinkedNestOutcome::IdentityMismatch { presented }
        }
        LinkedNestOutcome::Failed(e) => LinkedNestOutcome::Failed(e),
    }
}

fn log_answer<T: core::fmt::Debug>(gesture: &str, answer: &LinkedNestAnswer<T>) {
    match &answer.outcome {
        LinkedNestOutcome::Answered(value) => tracing::info!(
            nest = %answer.nest_url,
            ?value,
            "[recovery] {gesture} reached a linked nest"
        ),
        other => tracing::warn!(
            nest = %answer.nest_url,
            outcome = ?other,
            "[recovery] {gesture} did not reach a linked nest"
        ),
    }
}

/// The seed-alone replacement request, at the bound nest and then at every
/// linked nest (clause (c)). USER class at each.
///
/// The bound nest goes first and its answer is the gesture's: a refusal there
/// returns the error and asks no linked nest (no kit was shown, so nothing
/// may open a window anywhere). Each linked nest then gets the same parked
/// record over an owner-authenticated connection and runs its own 30-day
/// window; its answer is reported in the [`LinkedFanOut`].
pub async fn request_seed_alone_replacement_everywhere<R, D>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
    dial: &D,
) -> Result<(PendingKit, LinkedFanOut<i64>)>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    D: LinkedNestDial,
    <D::Owner as RpcRequester>::Error: RpcErrorClass,
{
    let pending = request_seed_alone_replacement(client, identity).await?;
    let targets = match linked_nests(client).await {
        Ok(targets) => targets,
        Err(e) => {
            tracing::warn!("[recovery] seed-alone request: linked nests unlisted: {e}");
            return Ok((
                pending,
                LinkedFanOut {
                    unlisted: Some(e),
                    nests: Vec::new(),
                },
            ));
        }
    };
    let mut nests = Vec::with_capacity(targets.len());
    for target in targets {
        let outcome = match checked(&target, dial.owner(&target).await) {
            Err(skipped) => widen(skipped),
            Ok(rpc) => {
                match request_seed_alone_replacement_at(&RecoveryClient::new(rpc), &pending).await {
                    Ok(lands_at) => LinkedNestOutcome::Answered(lands_at),
                    Err(e) => LinkedNestOutcome::Failed(e.to_string()),
                }
            }
        };
        let answer = LinkedNestAnswer {
            nest_id: target.nest_id,
            nest_url: target.nest_url,
            outcome,
        };
        log_answer("seed-alone request", &answer);
        nests.push(answer);
    }
    Ok((
        pending,
        LinkedFanOut {
            unlisted: None,
            nests,
        },
    ))
}

/// The veto at every **linked** nest, each over an anonymous connection and
/// its own challenge (clause (c)); the bound nest's veto is the caller's
/// ([`veto_pending_replacement`]) — the pairings are listed on its session.
///
/// Every reachable nest is asked whatever the others answered, so one
/// unreachable nest never leaves a window standing at the rest.
pub async fn veto_at_linked_nests<R, D>(
    client: &RecoveryClient<R>,
    kit: &ParsedKit,
    actor_id: ActorId,
    dial: &D,
) -> LinkedFanOut<bool>
where
    R: RpcRequester,
    D: LinkedNestDial,
    <D::Anonymous as RpcRequester>::Error: RpcErrorClass,
{
    let targets = match linked_nests(client).await {
        Ok(targets) => targets,
        Err(e) => {
            tracing::warn!("[recovery] veto: linked nests unlisted: {e}");
            return LinkedFanOut {
                unlisted: Some(e),
                nests: Vec::new(),
            };
        }
    };
    let mut nests = Vec::with_capacity(targets.len());
    for target in targets {
        // Each nest runs its own challenge, so each gets its own
        // `veto_pending_replacement` (what `veto_pending_replacement_everywhere`
        // loops over, here with a dial and a binding check per nest).
        let outcome = match checked(&target, dial.anonymous(&target).await) {
            Err(skipped) => widen(skipped),
            Ok(rpc) => {
                match veto_pending_replacement(&RecoveryClient::new(rpc), kit, Some(actor_id)).await
                {
                    Ok(cancelled) => LinkedNestOutcome::Answered(cancelled),
                    Err(e) => LinkedNestOutcome::Failed(e.to_string()),
                }
            }
        };
        let answer = LinkedNestAnswer {
            nest_id: target.nest_id,
            nest_url: target.nest_url,
            outcome,
        };
        log_answer("veto", &answer);
        nests.push(answer);
    }
    LinkedFanOut {
        unlisted: None,
        nests,
    }
}

/// The veto at the bound nest and every linked nest — the composition
/// [`crate::status::veto_with_status`] runs. Returns the bound nest's answer
/// (whether anything was pending there) beside the linked nests' report; the
/// linked nests are asked whatever the bound nest answered.
pub async fn veto_everywhere<R, D>(
    client: &RecoveryClient<R>,
    kit: &ParsedKit,
    actor_id: ActorId,
    dial: &D,
) -> (Result<bool>, LinkedFanOut<bool>)
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    D: LinkedNestDial,
    <D::Anonymous as RpcRequester>::Error: RpcErrorClass,
{
    let bound = veto_pending_replacement(client, kit, Some(actor_id)).await;
    let linked = veto_at_linked_nests(client, kit, actor_id, dial).await;
    (bound, linked)
}

/// The native hosts' dial (tui, linux, the FFI's apps): a second
/// `NestClient` signed in as the identity for the request — the connection
/// the account runtime's secondary leg opens
/// (`fauna_client_account_runtime::native_linked_nest_connector`) — and an
/// `AnonymousNestClient` for the veto, each bound-identity-checked through
/// the one shared door (`fauna_client::trust::connection_bound_identity`)
/// and bounded by [`native::LINKED_DIAL_BUDGET`], so an unreachable nest
/// costs the gesture seconds, not a hang.
#[cfg(not(target_arch = "wasm32"))]
pub mod native {
    use std::sync::Arc;
    use std::time::Duration;

    use fauna_anon_client::AnonymousNestClient;
    use fauna_client::NestClient;
    use fauna_client_core::linked_nests::{LinkedConnection, LinkedNestTarget};
    use fauna_core::identity::ActorKeypair;

    /// How long one linked nest's dial and binding check may take.
    pub const LINKED_DIAL_BUDGET: Duration = Duration::from_secs(10);

    /// [`super::LinkedNestDial`] over the native transports, for `identity`.
    pub struct NativeLinkedNestDial {
        identity: ActorKeypair,
    }

    impl NativeLinkedNestDial {
        #[must_use]
        pub fn new(identity: &ActorKeypair) -> Self {
            Self {
                identity: ActorKeypair::from_secret(*identity.secret_bytes()),
            }
        }
    }

    async fn bounded<T>(
        target: &LinkedNestTarget,
        dial: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        tokio::time::timeout(LINKED_DIAL_BUDGET, dial)
            .await
            .unwrap_or_else(|_| Err(format!("linked nest {}: timed out", target.nest_url)))
    }

    #[async_trait::async_trait]
    impl super::LinkedNestDial for NativeLinkedNestDial {
        type Owner = Arc<NestClient>;
        type Anonymous = AnonymousNestClient;

        async fn owner(
            &self,
            target: &LinkedNestTarget,
        ) -> Result<LinkedConnection<Arc<NestClient>>, String> {
            let keypair = ActorKeypair::from_secret(*self.identity.secret_bytes());
            bounded(target, async {
                let client = NestClient::new(target.nest_url.clone(), keypair);
                client
                    .connect()
                    .await
                    .map_err(|e| format!("linked nest {}: connect: {e}", target.nest_url))?;
                let bound_identity =
                    fauna_client::trust::connection_bound_identity(&client, &target.nest_url)
                        .await
                        .map_err(|e| {
                            format!("linked nest {}: bound identity: {e}", target.nest_url)
                        })?;
                Ok(LinkedConnection {
                    rpc: client,
                    bound_identity,
                })
            })
            .await
        }

        async fn anonymous(
            &self,
            target: &LinkedNestTarget,
        ) -> Result<LinkedConnection<AnonymousNestClient>, String> {
            bounded(target, async {
                let client = AnonymousNestClient::connect(&target.nest_url)
                    .await
                    .map_err(|e| format!("linked nest {}: connect: {e}", target.nest_url))?;
                let bound_identity =
                    fauna_client::trust::connection_bound_identity(&client, &target.nest_url)
                        .await
                        .map_err(|e| {
                            format!("linked nest {}: bound identity: {e}", target.nest_url)
                        })?;
                Ok(LinkedConnection {
                    rpc: client,
                    bound_identity,
                })
            })
            .await
        }
    }
}

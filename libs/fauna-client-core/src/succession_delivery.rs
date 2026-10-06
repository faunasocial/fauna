//! **The road** — the successor's devices carry a succession to every nest it
//! is owed at (`docs/goal/behavior/identity-succession.md` § Enforcement on the
//! home nest → *Every nest the identity is linked to*, the paragraphs **The
//! road** and **The stated bounds**).
//!
//! A nest applies a succession only against the registration chain it holds
//! itself, and nothing carries it there but a client: the nest that applied it
//! keeps the retired identity's burned pairing destinations as **owed nests**,
//! serves them to the successor alone on `fauna.recovery.succession.status`,
//! and clears one on `fauna.recovery.succession.owed_settle`. This module is
//! the delivery of one owed entry, and of the list:
//!
//! 1. The statement path for the entry's retired identity is read from the
//!    nest that keeps the list (`fauna.recovery.succession.lookup`: every hop
//!    from that identity forward, oldest first, the verbatim bytes).
//! 2. An **anonymous** connection to the owed nest's address is opened, and the
//!    identity it is bound to must be the entry's nest id — every answer below
//!    can settle the entry, so it is taken only from the nest the entry names.
//! 3. Each hop is submitted in order (`fauna.recovery.succession.submit`, a
//!    pre-identity kind). Landed and `fauna.recovery.already_succeeded` move on
//!    to the next hop.
//! 4. A refusal for lack of a usable chain — `fauna.recovery.not_registered`,
//!    or `fauna.recovery.signature_failed` because the chain there is behind —
//!    is answered when this device holds the hop's retired identity's seed: it
//!    signs in there as that identity. A `not_registered` sign-in means the
//!    nest holds no account for it, and the entry is settled. Otherwise it
//!    replays what the keeping nest's chain for that identity has and the owed
//!    nest's lacks, and submits again.
//! 5. Every hop landed (or already there) → the entry is settled at the
//!    keeping nest. Anything else stays owed, with its reason, and is asked
//!    again next pass: never dropped.
//!
//! Nothing here verifies a signature: the owed nest verifies the statement
//! against its own chain, and the replayed records link by link, exactly as
//! for any other submitter. No nest talks to another.
//!
//! Two callers, one body: every full pass of a seed-holding runtime of an
//! identity with predecessors (`fauna-account-plane`), against its bound nest
//! and each linked nest its secondary leg completes, and the Nests page's
//! both-ends link (`fauna-client-pair`), which delivers before it connects.
//! **Why this crate:** as for [`crate::recovery_chain`] — the ceremonies' home
//! sits above both callers.

use fauna_core::encoding::canonical_decode;
use fauna_core::recovery::SignedIdentitySuccession;
use fauna_protocol::recovery::{
    self as wire, OwedNest, SUCCESSION_OWED_SETTLE_KIND, SUCCESSION_STATUS_KIND,
};
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};

use crate::recovery_chain::{
    ChainPlan, ChainSide, fetch_registration_chain, plan_chain_reconcile,
    submit_registration_record,
};

/// `fauna.recovery.succession.submit` — pre-identity: the statement names the
/// account, and the nest verifies it against the chain it holds.
pub const SUCCESSION_SUBMIT_KIND: &str = "fauna.recovery.succession.submit";
/// `fauna.recovery.succession.lookup` — pre-identity.
pub const SUCCESSION_LOOKUP_KIND: &str = "fauna.recovery.succession.lookup";

/// The refusal codes the delivery branches on.
pub mod codes {
    pub const NOT_REGISTERED: &str = "fauna.recovery.not_registered";
    pub const ALREADY_SUCCEEDED: &str = "fauna.recovery.already_succeeded";
    pub const SIGNATURE_FAILED: &str = "fauna.recovery.signature_failed";
    pub const SUCCESSOR_EXISTS: &str = "fauna.recovery.successor_exists";
}

/// `fauna.recovery.succession.status`'s owed nests, for the account `rpc` is
/// authenticated as — empty for an account that is not a successor, or whose
/// owed nests are all settled.
///
/// # Errors
///
/// The request failed.
pub async fn fetch_owed_nests<R: RpcRequester>(rpc: &R) -> Result<Vec<OwedNest>, R::Error> {
    let reply: wire::SuccessionStatusReply = rpc
        .request(
            SUCCESSION_STATUS_KIND,
            wire::SuccessionStatusRequest::default(),
        )
        .await?;
    Ok(reply.owed_nests)
}

/// `fauna.recovery.succession.status`'s statement path into the account `rpc`
/// is authenticated as, oldest hop first, as the verbatim statement bytes —
/// what the link action submits at a nest before it connects there
/// ([`submit_statement_path`]). Empty for an account that succeeded nobody.
///
/// # Errors
///
/// The request failed.
pub async fn fetch_predecessor_statements<R: RpcRequester>(
    rpc: &R,
) -> Result<Vec<Vec<u8>>, R::Error> {
    let reply: wire::SuccessionStatusReply = rpc
        .request(
            SUCCESSION_STATUS_KIND,
            wire::SuccessionStatusRequest::default(),
        )
        .await?;
    Ok(reply
        .predecessor_statements
        .into_iter()
        .map(ByteBuf::into_vec)
        .collect())
}

/// `fauna.recovery.succession.lookup` for `actor_id`: every succession from it
/// forward, oldest first, as the verbatim statement bytes.
///
/// # Errors
///
/// The request failed.
pub async fn fetch_statement_path<R: RpcRequester>(
    rpc: &R,
    actor_id: &[u8; 32],
) -> Result<Vec<Vec<u8>>, R::Error> {
    let reply: wire::SuccessionLookupReply = rpc
        .request(
            SUCCESSION_LOOKUP_KIND,
            wire::SuccessionLookupRequest {
                actor_id: ByteBuf::from(actor_id.to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(reply
        .statements
        .into_iter()
        .map(ByteBuf::into_vec)
        .collect())
}

/// `fauna.recovery.succession.submit` of one verbatim statement.
///
/// # Errors
///
/// The nest refused the statement or the request failed.
pub async fn submit_statement<R: RpcRequester>(rpc: &R, statement: &[u8]) -> Result<(), R::Error> {
    let _: wire::SuccessionSubmitReply = rpc
        .request(
            SUCCESSION_SUBMIT_KIND,
            wire::SuccessionSubmitRequest {
                statement: ByteBuf::from(statement.to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(())
}

/// `fauna.recovery.succession.owed_settle` — clear one owed entry at the nest
/// that keeps it, for the account `rpc` is authenticated as.
///
/// # Errors
///
/// The nest refused the caller or the request failed.
pub async fn settle_owed_nest<R: RpcRequester>(
    rpc: &R,
    old_actor_id: &[u8],
    nest_id: &[u8],
) -> Result<(), R::Error> {
    let _: wire::SuccessionOwedSettleReply = rpc
        .request(
            SUCCESSION_OWED_SETTLE_KIND,
            wire::SuccessionOwedSettleRequest {
                old_actor_id: ByteBuf::from(old_actor_id.to_vec()),
                nest_id: ByteBuf::from(nest_id.to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(())
}

/// How a sign-in as a retired identity at an owed nest went
/// ([`OwedNestReach::sign_in_as`]).
pub enum SignIn<C> {
    /// This device does not hold that identity's seed.
    NoSeed,
    /// The nest holds no account for that identity
    /// (`fauna.auth.not_registered`).
    NoAccount,
    /// The nest answered something else, or could not be reached; the words
    /// are diagnostic.
    Failed(String),
    /// Signed in: a connection authenticated as that identity.
    Connected(C),
}

impl<C> SignIn<C> {
    /// The one reading of a sign-in failure every host shares: a
    /// `fauna.auth.not_registered` refusal is [`Self::NoAccount`], anything
    /// else [`Self::Failed`].
    pub fn from_error<E: RpcErrorClass + core::fmt::Display>(error: &E) -> Self {
        match error.as_rpc_error() {
            Some(refusal) if refusal.is_not_registered() => Self::NoAccount,
            _ => Self::Failed(error.to_string()),
        }
    }
}

/// How the delivery reaches an owed nest — the host's two connections to an
/// address, of two types: an anonymous connection is a client of its own
/// (natively `AnonymousNestClient`, on web `AnonymousWsRpcClient`), never a
/// signed-in one with the identity left off. Static dispatch, as
/// [`RpcRequester`].
#[allow(async_fn_in_trait)]
pub trait OwedNestReach {
    /// The anonymous connection every statement is submitted over.
    type Anon: RpcRequester<Error: RpcErrorClass>;
    /// A connection signed in as a retired identity — what a chain replay
    /// rides.
    type Signed: RpcRequester<Error: RpcErrorClass>;
    /// An anonymous connection to `url`, and the identity that connection is
    /// bound to — read through the host's one door for it (the origin's pin,
    /// else a possession proof over the connection; never the nest's own
    /// claim).
    async fn anonymous(&self, url: &str) -> Result<(Self::Anon, [u8; 32]), String>;
    /// Sign in at `url` as `actor_id`, a retired identity of this account —
    /// [`SignIn::NoSeed`] when this device does not hold its seed.
    async fn sign_in_as(&self, url: &str, actor_id: &[u8; 32]) -> SignIn<Self::Signed>;
}

/// What one owed entry's delivery did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Every hop of the statement path is at the owed nest now — `landed` of
    /// them by this delivery (zero: every one was already there), `replayed`
    /// registration records carried there first — and the entry is settled.
    Landed { landed: usize, replayed: usize },
    /// The owed nest holds no account for the retired identity; the entry is
    /// settled.
    NoAccount,
    /// The entry stays owed, and is asked again next pass.
    Owed(OwedReason),
}

/// Why an owed entry stays owed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwedReason {
    /// The burned pairing row carried no address.
    NoAddress,
    /// The entry names no 32-byte identity, or the path holds a statement that
    /// does not decode.
    Malformed(String),
    /// The keeping nest could not be read.
    Unread(String),
    /// The keeping nest serves no statement for the retired identity.
    NoStatement,
    /// The owed nest could not be reached.
    Unreachable(String),
    /// The address answered as another identity: nothing was sent.
    IdentityMismatch { presented: [u8; 32] },
    /// The owed nest holds an account for the successor already — one
    /// registered there by hand before the statement landed
    /// (`fauna.recovery.successor_exists`; stated bound 4): owed until that
    /// account is deleted there.
    SuccessorExists,
    /// The owed nest cannot verify the statement (no chain, or a chain
    /// behind), and this device holds no seed for the hop's retired identity
    /// to replay one.
    NoSeed { code: String },
    /// The owed nest holds a registration chain for the hop's retired
    /// identity that the keeping nest's does not extend — a RecoveryKey
    /// registered there first (stated bound 3). Nothing is replayed over it.
    ChainForked,
    /// The owed nest refused the statement, or a replayed record, with this
    /// code — or the sign-in for the replay failed.
    Refused(String),
    /// Every hop landed, and the keeping nest refused or never answered the
    /// settle. The next delivery finds them landed and settles.
    Unsettled(String),
}

/// Deliver one owed entry kept by `keeper` (the nest whose
/// `succession.status` served it, over the successor's own connection), to
/// the owed nest through `reach`. Settles the entry at `keeper` when the
/// delivery is done; never errs — what could not land is
/// [`Delivery::Owed`].
pub async fn deliver_owed_nest<K, H>(keeper: &K, reach: &H, owed: &OwedNest) -> Delivery
where
    K: RpcRequester,
    K::Error: RpcErrorClass,
    H: OwedNestReach,
{
    let owed_reason = |reason| Delivery::Owed(reason);
    let Some(url) = owed.nest_url.as_deref().filter(|u| !u.is_empty()) else {
        return owed_reason(OwedReason::NoAddress);
    };
    let (Ok(old), Ok(nest_id)) = (
        <[u8; 32]>::try_from(owed.old_actor_id.as_ref()),
        <[u8; 32]>::try_from(owed.nest_id.as_ref()),
    ) else {
        return owed_reason(OwedReason::Malformed(
            "an owed entry names no 32-byte id".into(),
        ));
    };
    let path = match fetch_statement_path(keeper, &old).await {
        Ok(path) if path.is_empty() => return owed_reason(OwedReason::NoStatement),
        Ok(path) => path,
        Err(e) => return owed_reason(OwedReason::Unread(e.to_string())),
    };
    let (conn, presented) = match reach.anonymous(url).await {
        Ok(reached) => reached,
        Err(e) => return owed_reason(OwedReason::Unreachable(e)),
    };
    if presented != nest_id {
        return owed_reason(OwedReason::IdentityMismatch { presented });
    }
    let mut delivered = Delivered::default();
    match deliver_path(keeper, reach, url, &conn, &path, &mut delivered).await {
        PathEnd::Landed => {}
        PathEnd::NoAccount => {
            return match settle_owed_nest(keeper, &old, &nest_id).await {
                Ok(()) => Delivery::NoAccount,
                Err(e) => owed_reason(OwedReason::Unsettled(e.to_string())),
            };
        }
        PathEnd::Owed(reason) => return owed_reason(reason),
    }
    match settle_owed_nest(keeper, &old, &nest_id).await {
        Ok(()) => Delivery::Landed {
            landed: delivered.landed,
            replayed: delivered.replayed,
        },
        Err(e) => owed_reason(OwedReason::Unsettled(e.to_string())),
    }
}

/// **The link action delivers first**: submit a statement path at a nest about
/// to be linked, over an anonymous connection to it, before the link signs in
/// there — so re-linking lands the succession at a nest no owed list named.
/// Landed and "already succeeded" count alike; nothing is settled, and no
/// chain is replayed (the link's own reconcile carries the successor's chain;
/// a retired identity's chain is the runtime's road). Returns how many hops
/// this call landed.
///
/// # Errors
///
/// The first hop the nest refused for any other reason, or that never reached
/// it — reported, and for the link never a reason to stop: the link writes no
/// account data under the retired identity.
pub async fn submit_statement_path<C>(conn: &C, path: &[Vec<u8>]) -> Result<usize, OwedReason>
where
    C: RpcRequester,
    C::Error: RpcErrorClass,
{
    let mut landed = 0;
    for statement in path {
        match submit_statement(conn, statement).await {
            Ok(()) => landed += 1,
            Err(e) => match e.as_rpc_error() {
                Some(r) if r.code == codes::ALREADY_SUCCEEDED => {}
                Some(r) if r.code == codes::SUCCESSOR_EXISTS => {
                    return Err(OwedReason::SuccessorExists);
                }
                Some(r) => return Err(OwedReason::Refused(r.code.clone())),
                None => return Err(OwedReason::Unreachable(e.to_string())),
            },
        }
    }
    Ok(landed)
}

/// Submit `path`'s hops at an owed nest over `conn`, replaying a hop's chain
/// where the nest cannot verify it — the statement path's whole delivery,
/// settling nothing ([`deliver_owed_nest`] settles).
async fn deliver_path<K, H>(
    keeper: &K,
    reach: &H,
    url: &str,
    conn: &H::Anon,
    path: &[Vec<u8>],
    delivered: &mut Delivered,
) -> PathEnd
where
    K: RpcRequester,
    H: OwedNestReach,
{
    for (hop, statement) in path.iter().enumerate() {
        let refusal = match submit_statement(conn, statement).await {
            Ok(()) => {
                delivered.landed += 1;
                continue;
            }
            Err(e) => match e.as_rpc_error() {
                Some(refusal) => refusal.code.clone(),
                None => return PathEnd::Owed(OwedReason::Unreachable(e.to_string())),
            },
        };
        match refusal.as_str() {
            codes::ALREADY_SUCCEEDED => continue,
            codes::SUCCESSOR_EXISTS => return PathEnd::Owed(OwedReason::SuccessorExists),
            codes::NOT_REGISTERED | codes::SIGNATURE_FAILED => {}
            _ => return PathEnd::Owed(OwedReason::Refused(refusal)),
        }
        let retired = match canonical_decode::<SignedIdentitySuccession>(statement) {
            Ok(signed) => signed.statement.old_actor_id.0,
            Err(e) => return PathEnd::Owed(OwedReason::Malformed(format!("hop {hop}: {e}"))),
        };
        match replay_chain(keeper, reach, url, &retired, &refusal).await {
            Replay::Carried(records) => delivered.replayed += records,
            // Only the first hop's identity can be absent where its successor
            // is owed: a later hop's is the account the hop before it moved.
            Replay::NoAccount if hop == 0 => return PathEnd::NoAccount,
            Replay::NoAccount => return PathEnd::Owed(OwedReason::Refused(refusal)),
            Replay::Owed(reason) => return PathEnd::Owed(reason),
        }
        match submit_statement(conn, statement).await {
            Ok(()) => delivered.landed += 1,
            Err(e) => match e.as_rpc_error() {
                Some(r) if r.code == codes::ALREADY_SUCCEEDED => {}
                Some(r) if r.code == codes::SUCCESSOR_EXISTS => {
                    return PathEnd::Owed(OwedReason::SuccessorExists);
                }
                Some(r) => return PathEnd::Owed(OwedReason::Refused(r.code.clone())),
                None => return PathEnd::Owed(OwedReason::Unreachable(e.to_string())),
            },
        }
    }
    PathEnd::Landed
}

#[derive(Default)]
struct Delivered {
    landed: usize,
    replayed: usize,
}

enum PathEnd {
    Landed,
    NoAccount,
    Owed(OwedReason),
}

enum Replay {
    /// This many records were carried; the statement may verify now.
    Carried(usize),
    NoAccount,
    Owed(OwedReason),
}

/// Sign in at the owed nest as `retired` and carry what the keeping nest's
/// registration chain for it has and the owed nest's lacks, oldest first.
/// `refusal` is the code the statement met, reported when nothing could be
/// carried that would change it.
async fn replay_chain<K, H>(
    keeper: &K,
    reach: &H,
    url: &str,
    retired: &[u8; 32],
    refusal: &str,
) -> Replay
where
    K: RpcRequester,
    H: OwedNestReach,
{
    let signed = match reach.sign_in_as(url, retired).await {
        SignIn::Connected(signed) => signed,
        SignIn::NoAccount => return Replay::NoAccount,
        SignIn::NoSeed => {
            return Replay::Owed(OwedReason::NoSeed {
                code: refusal.to_string(),
            });
        }
        SignIn::Failed(e) => return Replay::Owed(OwedReason::Refused(format!("sign-in: {e}"))),
    };
    let kept = match fetch_registration_chain(keeper, retired).await {
        Ok(chain) => chain,
        Err(e) => return Replay::Owed(OwedReason::Unread(e.to_string())),
    };
    let there = match fetch_registration_chain(&signed, retired).await {
        Ok(chain) => chain,
        Err(e) => return Replay::Owed(OwedReason::Unreachable(e.to_string())),
    };
    let records = match plan_chain_reconcile(&kept, &there) {
        Ok(ChainPlan::Forked) => return Replay::Owed(OwedReason::ChainForked),
        Ok(ChainPlan::Extend {
            side: ChainSide::Linked,
            records,
            ..
        }) if !records.is_empty() => records,
        // In step, or the owed nest ahead, or the next link seed-alone: the
        // chain is not what a replay could change.
        Ok(_) => return Replay::Owed(OwedReason::Refused(refusal.to_string())),
        Err(e) => return Replay::Owed(OwedReason::Malformed(e.to_string())),
    };
    for record in &records {
        if let Err(e) = submit_registration_record(&signed, record).await {
            return Replay::Owed(match e.as_rpc_error() {
                Some(r) => OwedReason::Refused(r.code.clone()),
                None => OwedReason::Unreachable(e.to_string()),
            });
        }
    }
    Replay::Carried(records.len())
}

/// Every owed entry `keeper` serves, delivered in served order
/// ([`deliver_owed_nest`]). An unreadable list is an error; an entry that
/// could not land is an [`OwedReason`] beside it.
///
/// # Errors
///
/// `fauna.recovery.succession.status` could not be read at `keeper`.
pub async fn deliver_owed_nests<K, H>(
    keeper: &K,
    reach: &H,
) -> Result<Vec<(OwedNest, Delivery)>, K::Error>
where
    K: RpcRequester,
    K::Error: RpcErrorClass,
    H: OwedNestReach,
{
    let mut out = Vec::new();
    for owed in fetch_owed_nests(keeper).await? {
        let delivery = deliver_owed_nest(keeper, reach, &owed).await;
        out.push((owed, delivery));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

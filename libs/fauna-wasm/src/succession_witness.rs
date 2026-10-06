//! The **member side** of a succession for the browser — the web twin of
//! `fauna-ffi`'s `succession_witness.rs` and of the `conv_backend.rs` blocks
//! linux and tui carry (`succession-propagation.md` § Propagation →
//! *MLS groups*).
//!
//! A member client with no [`SuccessionWitness`] registered degrades every
//! `GroupMetaMessage::Succession` to the bare add — the audience of a real
//! recovery ceremony renders "a stranger joined the group" and the participant
//! row keeps naming the retired identity, permanently and silently. web was the
//! last app in that state.
//!
//! **Two things here are web's own; everything else is shared.** The
//! verification policy ([`ChainWitness`]), the anchors
//! ([`ThreadParticipantAnchors`]), the harvest's whole per-pass policy
//! ([`PeerAnchorSweepState`]) and the state renderer
//! ([`witness::state_json`]) are all `fauna-client-recovery`'s, shared with the
//! six native apps. What is genuinely different is:
//!
//! 1. **The dial** ([`WebSuccessionChainSource`]). Every *native* platform
//!    reaches a foreign nest by one route, which is why that one is shared as
//!    `NativeSuccessionChainSource`; the browser has neither
//!    `AnonymousNestClient` nor an SRV resolver, so it derives the apex URL and
//!    dials over its own WebSocket transport.
//! 2. **The driver's clock.** There is no `tokio::spawn` on wasm32 and no
//!    [`ConversationsSession::start_receive_loop`], so nothing can park on a
//!    5 s timer; the sweep ticks from the JS-owned receive pump that already
//!    drives every other arm of the loop
//!    ([`WasmConversationsManager::poll_conversations`]). The *policy* it ticks
//!    is the shared pass, not a second copy — see [`PeerAnchorSweepState`].
//!
//! [`SuccessionWitness`]: fauna_conversations::backend::SuccessionWitness
//! [`ChainWitness`]: fauna_client_recovery::ChainWitness
//! [`ThreadParticipantAnchors`]: fauna_client_recovery::ThreadParticipantAnchors
//! [`PeerAnchorSweepState`]: fauna_client_recovery::harvest::PeerAnchorSweepState
//! [`witness::state_json`]: fauna_client_recovery::witness::state_json
//! [`ConversationsSession::start_receive_loop`]: fauna_conversations::ConversationsSession
//! [`WasmConversationsManager::poll_conversations`]: crate::conversations::WasmConversationsManager

use std::sync::Arc;

use fauna_client_recovery::AnchoredWalk;
use fauna_client_recovery::witness::SuccessionChainSource;
use fauna_conversations::ConversationsManager;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{ChainHead, VerifiedSuccession};

/// The whole round trip a member may spend on one unreachable anchor — the same
/// budget the native source names, for the same reason: `ChainWitness::verify`
/// is called inline by the inbound poll, so an unreachable anchor must cost
/// this much once and then degrade rather than stall the feed. The witness's
/// own per-session verdict memo is what keeps it to once.
const WITNESS_ROUND_TRIP_BUDGET_MS: u32 = 15_000;

/// The **browser** leg of the in-group succession witness: derive the peer's
/// nest URL from its handle domain, dial it anonymously over WS-RPC, and hand
/// the connection to the shared [`walk`](fauna_client_recovery::walk).
///
/// **Not `NativeSuccessionChainSource` with a different transport.** That one
/// resolves through `fauna_core::resolve::resolve_full_url`, which is
/// SRV-aware and lives behind fauna-core's `network` feature — off in the wasm
/// build, and unimplementable in a browser besides (no DNS API). web therefore
/// derives the apex URL the same way its other by-domain anonymous dials do
/// (`fauna_core::web::apex_url`), which is the browser's whole reach: a peer
/// nest on a non-standard port is unreachable from web until it publishes an
/// address the SPA can see. That is a *narrower* reach than native's, not a
/// different rule — every failure degrades the statement to the bare add,
/// exactly as an unreachable anchor does anywhere else.
///
/// **Anonymous is the only option, not a shortcut.** The verifying member holds
/// no account on the succeeded peer's nest; it works because
/// `succession.lookup` and `registration.chain` are pre-identity kinds
/// (`identity-succession.md` § Enforcement on the home nest).
///
/// **The domain is the anchor.** It comes from the handle the thread's
/// participant row has carried since the peer joined — never from the statement
/// under test, which is the "delivered chain" hole § The succession statement
/// forbids.
pub(crate) struct WebSuccessionChainSource;

/// The browser's one anonymous anchored dial: connect to `anchor_nest_url` and
/// run the shared [`walk`](fauna_client_recovery::walk) there, inside
/// [`WITNESS_ROUND_TRIP_BUDGET_MS`] — the wasm twin of
/// `fauna_client_recovery::witness::walk_at_nest_url`, under the same contract:
/// the URL is an anchor the caller already held (a handle domain's apex for the
/// witness; an event's *bound* organizer nest for the calendar's inbound rule),
/// never one read off the message asserting the succession. The same three
/// answers: [`AnchoredWalk::NeverSucceeded`] when the anchor answered so,
/// [`AnchoredWalk::Unsettled`] for unreachable, timed out, or a chain that
/// fails the rule.
pub(crate) async fn walk_at_nest_url(
    anchor_nest_url: &str,
    old: ActorId,
    known_head: Option<ChainHead>,
) -> AnchoredWalk {
    let Ok(anon) = fauna_rpc_wasm::AnonymousWsRpcClient::connect(anchor_nest_url) else {
        tracing::debug!(
            anchor = %anchor_nest_url,
            "no succession is verified: the anchor nest would not dial"
        );
        return AnchoredWalk::Unsettled;
    };
    // The bound is the whole walk, not one request: `walk` makes two
    // (`succession.lookup` then `registration.chain`), each with the
    // transport's own 30 s deadline, so without this a caller could spend a
    // minute inside one inbound poll. `select` rather than a deadline on each
    // call for the same reason native wraps the whole block in one `timeout`.
    let walked = std::pin::pin!(fauna_client_recovery::walk_outcome(anon, old, known_head));
    match futures_util::future::select(
        walked,
        gloo_timers::future::TimeoutFuture::new(WITNESS_ROUND_TRIP_BUDGET_MS),
    )
    .await
    {
        futures_util::future::Either::Left((verdict, _)) => verdict,
        futures_util::future::Either::Right(_) => {
            tracing::debug!(
                anchor = %anchor_nest_url,
                budget_ms = WITNESS_ROUND_TRIP_BUDGET_MS,
                "no succession is verified: the anchored walk timed out"
            );
            AnchoredWalk::Unsettled
        }
    }
}

#[async_trait::async_trait(?Send)]
impl SuccessionChainSource for WebSuccessionChainSource {
    async fn walk_from_domain(
        &self,
        handle_domain: &str,
        old: ActorId,
        known_head: Option<ChainHead>,
    ) -> Option<VerifiedSuccession> {
        walk_at_nest_url(&fauna_core::web::apex_url(handle_domain), old, known_head)
            .await
            .successor()
    }

    async fn walk_line_from_domain(
        &self,
        handle_domain: &str,
        old: ActorId,
        known_head: Option<ChainHead>,
    ) -> Option<Vec<VerifiedSuccession>> {
        // The same dial and the same whole-walk bound as the terminal walk
        // above: only what the shared body returns differs.
        let url = fauna_core::web::apex_url(handle_domain);
        let anon = fauna_rpc_wasm::AnonymousWsRpcClient::connect(&url).ok()?;
        let walked = std::pin::pin!(fauna_client_recovery::walk_line(anon, old, known_head));
        match futures_util::future::select(
            walked,
            gloo_timers::future::TimeoutFuture::new(WITNESS_ROUND_TRIP_BUDGET_MS),
        )
        .await
        {
            futures_util::future::Either::Left((line, _)) => line,
            futures_util::future::Either::Right(_) => None,
        }
    }
}

/// web's concrete in-group succession witness — the shared policy over the
/// shared anchors and web's own dialer. Named because the manager must call
/// [`observation`], which a `dyn SuccessionWitness` cannot answer.
///
/// The three types are the twin of tui's `TuiChainWitness`, linux's
/// `LinuxChainWitness` and fauna-ffi's `FfiChainWitness`; only the third
/// differs.
///
/// [`observation`]: fauna_client_recovery::ChainWitness::observation
pub(crate) type WebChainWitness = fauna_client_recovery::ChainWitness<
    fauna_client_recovery::ThreadParticipantAnchors,
    WebSuccessionChainSource,
>;

/// web's [`ParkedStatementRedrive`]: the raw backend + manager the JS-driven
/// receive loop is built around, since this app has no
/// [`ConversationsSession`] to hold.
///
/// Strong handles rather than `Weak` because it is built **per pass**, inside
/// the poll that already holds both — so it lives exactly as long as the pass
/// and keeps nothing alive between ticks, which is the property native's
/// `Weak` upgrade buys there.
///
/// [`ParkedStatementRedrive`]: fauna_client_recovery::harvest::ParkedStatementRedrive
/// [`ConversationsSession`]: fauna_conversations::ConversationsSession
pub(crate) struct WebParkedRedrive {
    pub(crate) backend: Arc<FaunaMlsBackend>,
    pub(crate) manager: Arc<ConversationsManager>,
}

#[async_trait::async_trait(?Send)]
impl fauna_client_recovery::harvest::ParkedStatementRedrive for WebParkedRedrive {
    async fn redrive(&self, old_actor: &ActorId) -> u32 {
        fauna_conversations::backends::fauna_mls::redrive_parked_successions(
            &self.backend,
            &self.manager,
            old_actor,
        )
        .await
    }

    async fn settled_unseeded(&self, old_actor: &ActorId) -> u32 {
        fauna_conversations::backends::fauna_mls::settle_parked_successions(
            &self.backend,
            &self.manager,
            old_actor,
        )
        .await
    }
}

//! The **member side** of a succession, for the four FFI apps (macOS, iOS,
//! windows, android) — `succession-aftermath.md` § Propagation → *MLS groups*.
//!
//! Its sibling `succession_aftermath.rs` is the side that *runs* a succession;
//! this is the side that *receives* one. A member client with no
//! [`SuccessionWitness`] registered degrades every `GroupMetaMessage::Succession`
//! to the bare add — the audience of a real recovery ceremony renders "a
//! stranger joined the group" and the participant row keeps naming the retired
//! identity, permanently and silently.
//!
//! **This module is glue and nothing else.** The verification policy, the
//! anchors, the native dial, the harvest sweep and the state renderer are all
//! `fauna-client-recovery`'s, shared with tui and linux; what cannot be shared
//! is the thread store the anchors read handles from and this session's
//! account-store seam, and both come from the session factory that calls [`wire`].
//! Everything here is the same three calls
//! `apps/fauna-linux/src/conversations/conv_backend.rs` and
//! `apps/fauna-tui/src/conversations/conv_backend.rs` make.
//!
//! [`SuccessionWitness`]: fauna_conversations::backend::SuccessionWitness

use std::sync::{Arc, Mutex};

use fauna_client::NestClient;
use fauna_conversations::ConversationsSession;

/// The concrete witness these apps register — the shared policy over the shared
/// anchors and the shared native dialer, identical to linux's
/// `LinuxChainWitness` and tui's `TuiChainWitness`.
///
/// Named because the holder below must call [`observation`], which a
/// `dyn SuccessionWitness` cannot answer.
///
/// [`observation`]: fauna_client_recovery::ChainWitness::observation
pub(crate) type FfiChainWitness = fauna_client_recovery::ChainWitness<
    fauna_client_recovery::ThreadParticipantAnchors,
    fauna_client_recovery::witness::NativeSuccessionChainSource,
>;

/// The two halves [`FfiNestClient::succession_witness_state_json`] renders: the
/// witness's own report, and the producer's per-peer log.
///
/// They are kept together and separate from the session because neither is
/// reachable through it — the session holds the witness behind a `dyn` that
/// cannot answer `observation()`, and never sees the harvest log at all.
///
/// [`FfiNestClient::succession_witness_state_json`]: crate::FfiNestClient::succession_witness_state_json
pub(crate) struct SuccessionReport {
    pub(crate) witness: Arc<FfiChainWitness>,
    pub(crate) harvest: Arc<fauna_client_recovery::harvest::HarvestLog>,
}

/// Late-populated like `SchedulingSessionHolder`: the report exists only once a
/// conversations session has been built, and the state provider may be polled
/// before that.
pub(crate) type SuccessionReportHolder = Arc<Mutex<Option<SuccessionReport>>>;

/// Register the witness on `session` and inject the harvest sweep, stashing the
/// second handles in `holder`.
///
/// Called unconditionally from the session factory. Neither half takes a
/// store: the anchors rest on the account plane, lent to the manager at the
/// account-store-ready edge (`conversation_seams::wire`), and read as an
/// unreadable store until then — reported, never inferred.
pub(crate) fn wire(
    session: &Arc<ConversationsSession>,
    nest: Arc<NestClient>,
    holder: &SuccessionReportHolder,
) {
    // The anchors' durable store (`fauna.state.peer-anchors`) is lent late,
    // through the manager, by the shared store-ready registration
    // (`crate::account_runtime`'s `conversation_seams::wire`) — nothing to hand
    // in here, so nothing to forget.
    let witness = Arc::new(fauna_client_recovery::ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::new(
            // `Weak`, never the manager itself: the witness is parked on the
            // backend this manager owns, so a strong handle here is a cycle
            // that outlives the session (the type's own doc).
            Arc::downgrade(&session.manager()),
        ),
        fauna_client_recovery::witness::NativeSuccessionChainSource,
    ));
    session.set_succession_witness(
        Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
    );

    // The producer half. Injected rather than spawned: the factory that calls
    // this is a synchronous UniFFI export with no runtime, and `fauna-ffi` owns
    // no fallback one on purpose (`crate::account_runtime`), so
    // `start_receive_loop` launches it first in its own prologue.
    let harvest: Arc<fauna_client_recovery::harvest::HarvestLog> = Default::default();
    session.set_peer_anchor_sweep_launcher(Arc::new(
        fauna_client_recovery::harvest::PeerAnchorSweep::new(
            Arc::downgrade(session),
            nest,
            Arc::clone(&harvest),
            fauna_client_recovery::harvest::PEER_ANCHOR_SWEEP_INTERVAL,
        ),
    ));

    *holder.lock().unwrap() = Some(SuccessionReport { witness, harvest });
}

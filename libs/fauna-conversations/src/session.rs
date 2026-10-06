//! Native UniFFI receive seam for the fauna-native MLS conversation rail.
//!
//! [`ConversationsSession`] is the native (Apple/Android/Windows/Linux) twin of
//! the wasm [`WasmConversationsManager`](../../fauna-wasm/src/conversations.rs)'s
//! FaunaMls half: it constructs a `FaunaMls`-wired [`ConversationsManager`] and
//! drives the shared-Rust receive path — welcome ingest + the inbound channel
//! poll — that the web app reaches through `future_to_promise` wrappers.
//!
//! Native apps reach conversations only through the UniFFI surface, and the
//! receive drivers ([`poll_inbound_conv`] / [`ingest_welcome`]) are plain Rust
//! free functions (they need `&FaunaMlsBackend`, not a `dyn RailBackend`), so
//! they are not UniFFI-callable on their own. This session Object holds the
//! concrete backend and exposes those free functions over the same
//! `#[fauna_uniffi_async::export]` async surface the manager's other async
//! methods use.
//!
//! All MLS crypto stays in `fauna-mls` / [`FaunaMlsBackend`] — this seam moves
//! **no** MLS state client-side (`docs/goal/ui/conversations.md` § Architectural
//! rules #2). The single observable surface is still the
//! [`ConversationsManager`] ([`Self::manager`]); the session only adds the
//! receive drivers (`docs/goal/ui/conversations.md` § State & data shape).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use fauna_client_moderation::{LocalDetection, LocalDetectionStore};
use fauna_core::identity::ActorId;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::ChannelId;

use crate::backend::{
    BackendError, ConversationsPush, ConversationsRpc, CustodyCeremonySink, FolderCustodySink,
    FolderGateSink, GroupReceptionKeys, InboundMailSource, InboxDrainSource, IndexBuilderLauncher,
    MlsSyncLauncher, OutboundMailSink, PeerAnchorSweepLauncher, RoomCeremonyRpc,
    RoomGenerationReader, RoomRosterReader, RoomRosterReporter, RoomSeams, SchedulingSink,
    SelfAddress, ShareEndpointsSink, SuccessionWitness,
};
// `ConvPushEvent` is used only by the native receive loop (`start_receive_loop`);
// the wasm SPA drives its own loop. `WelcomeChannelKind` is also the dispatch key
// of the target-agnostic `ingest_welcome_by_kind` below, which web drives too.
#[cfg(not(target_arch = "wasm32"))]
use crate::backend::ConvPushEvent;
use crate::backend::WelcomeChannelKind;
use crate::backend::{BridgedSink, BridgedSource};
use crate::backends::bridged::{BridgedBackend, poll_inbound_bridged};
use crate::backends::fauna_mls::{
    FaunaMlsBackend, ingest_scheduling_welcome, ingest_welcome, join_folder_welcome,
    poll_inbound_conv_past_key_in, poll_inbound_folder, poll_inbound_scheduling,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::backends::fauna_mls::{leave_folder, poll_inbound_conv};
use crate::backends::smtp::{SmtpBackend, poll_inbound_mail};
// Only `start_receive_loop`'s index-arm reopening uses this, and that whole impl
// block is `cfg(not(wasm32))` — so an ungated import warned on every wasm build.
#[cfg(not(target_arch = "wasm32"))]
use crate::index_sink::IndexableKind;
use crate::manager::ConversationsManager;
use fauna_core::data::ArrivalDisposition;

// NOTE (2026-08-17): the next word is deliberately still "clients".
// It is app-meaning and owed a rename, but this doc comment is copied verbatim
// into the UniFFI-generated Go binding
// (`libs/fauna-mail-go/fauna_conversations/fauna_conversations.go`), so renaming
// it obliges `just mail-bridge-ffi` in the same commit or `mail-bridge-ffi-check`
// goes RED. Deferred rather than burn a scarce build slot on one comment — fold
// it into the next commit that regenerates the binding for a real reason.
/// A FaunaMls-wired conversations session for the native UniFFI clients.
///
/// Constructed via [`Self::from_parts`] (plain Rust — takes a `dyn
/// ConversationsRpc` for dependency injection, so tests pass a mock nest; the FFI
/// factory in `fauna-ffi` passes the real `NestConversationsRpc`). The session
/// owns the wired [`ConversationsManager`] (the single observable surface the
/// client drives for snapshot/send/rename/…) and the concrete
/// [`FaunaMlsBackend`] the receive free functions need by `&` reference.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct ConversationsSession {
    /// The single observable surface (`docs/goal/ui/conversations.md` § State &
    /// data shape) — held the same way the crate models the manager (a uniffi
    /// `Object`, so behind an `Arc`). The FaunaMls backend is registered on it.
    manager: Arc<ConversationsManager>,
    /// The concrete FaunaMls backend. Held concretely (not as `dyn RailBackend`)
    /// because the receive free functions [`poll_inbound_conv`] / [`ingest_welcome`]
    /// take `&FaunaMlsBackend`. Native always has it, so it is **not** an
    /// `Option` (unlike the wasm wrapper, where a client-less receive-only
    /// manager can exist without one).
    fauna_mls: Arc<FaunaMlsBackend>,
    /// The logged-in user's canonical mail address (`<handle>@<domain>`) — the
    /// live cell (`conversations.md` § State & data shape → *Self-address:
    /// live, never baked*) shared with both rail backends, which read it at use
    /// time. Seeded from the string passed to `from_parts`/`from_manager`
    /// (possibly empty — construction never waits for identity resolution) and
    /// updated via [`Self::set_self_address`].
    self_address: Arc<SelfAddress>,
    /// Per-channel inbound paging cursor (the highest server sequence ingested
    /// for each bound channel). The native twin of the wasm wrapper's
    /// `conv_cursors`: the inbound poll reads + copies a channel's cursor, polls
    /// without holding the lock across the `.await`, then writes the advanced
    /// cursor back. Used by the manual [`Self::poll_conversations`] backstop; the
    /// detached [`Self::start_receive_loop`] task keeps its own cursor map.
    conv_cursors: Mutex<HashMap<ChannelId, i64>>,
    /// Per-scheduling-channel inbound paging cursor — the scheduling twin of
    /// [`Self::conv_cursors`], used by the manual [`Self::poll_scheduling`]
    /// backstop (the detached [`Self::start_receive_loop`] keeps its own map).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    scheduling_cursors: Mutex<HashMap<ChannelId, i64>>,
    /// Per-folder-channel inbound paging cursor — the folder twin of
    /// [`Self::scheduling_cursors`], used by the manual [`Self::poll_folders`]
    /// backstop (the detached [`Self::start_receive_loop`] keeps its own map).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    folder_cursors: Mutex<HashMap<ChannelId, i64>>,
    /// The inbound push source the receive loop subscribes to
    /// (`welcome.received` + `channel.message`). `None` in poll-only mode — tests that exercise only
    /// `ingest_welcome` / `poll_conversations`, and any caller without a live push
    /// plane (the loop then runs ticker-only). The FFI factory injects the real
    /// `NestConversationsPush` over the same connection. Read only by the
    /// native-only `start_receive_loop`; the wasm build never drives the loop.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    push: Option<Arc<dyn ConversationsPush>>,
    /// The inbound mail read-feed sources the receive path polls — the `INBOX`
    /// (`fauna.email.inbox.fetch`) and `Sent` (`fauna.email.sent.fetch`) mailboxes
    /// (`docs/goal/ui/conversations.md` § Receiving into the conversations view:
    /// nest-backed mail reads *both*, so own-MUA-sent mail surfaces too). `None`
    /// until [`Self::register_mail_receive`] wires them — the receive twin of
    /// [`Self::register_smtp`] (send): the FFI factory injects nest-backed sources
    /// right after construction, tests inject mocks, and a session with mail
    /// unconfigured simply never polls the rail. Read by the native
    /// [`Self::start_receive_loop`] (its own cursors) and the manual
    /// [`Self::poll_mail`] backstop. Each source does its own transport + decrypt
    /// behind the [`InboundMailSource`] seam; this crate stays crypto-free.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    mail_inbox: Mutex<Option<Arc<dyn InboundMailSource>>>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    mail_sent: Mutex<Option<Arc<dyn InboundMailSource>>>,
    /// The calendar-apply sink the scheduling drain routes a decrypted iMIP to —
    /// the mailbox-less CalDAV WS-RPC rail (`docs/goal/behavior/caldav-server.md`
    /// § Server-side auto-schedule, Half-1). `None` until
    /// [`Self::register_scheduling_sink`] wires it (the FFI factory injects a
    /// `NestSchedulingSink`); a session without it never drains scheduling
    /// channels. The sink does its own calendar transport + seal behind the
    /// [`SchedulingSink`] seam, so this crate stays crypto-free — the scheduling
    /// twin of [`Self::mail_inbox`]. Read by the native [`Self::start_receive_loop`]
    /// (its own cursors) and the [`Self::poll_scheduling`] backstop.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    scheduling_sink: Mutex<Option<Arc<dyn SchedulingSink>>>,
    /// The durable inbox-apply backstop the receive loop's ticker drains — the
    /// missed-push recovery rail (`docs/goal/architecture/api-layers.md` § Inbox &
    /// Messaging, layer 3). `None` until [`Self::register_inbox_drain`] wires it
    /// (the FFI factory + linux glue inject a `NestInboxDrainSource`); a session
    /// without it runs push-only (the pre-layer-3 behaviour). Each `drain_once`
    /// runs the shared `fauna_client_inbox::drain` (fetch → decode → dispatch →
    /// ack) behind the [`InboxDrainSource`] seam, so this crate gains no
    /// `RpcRequester` dependency — the drain twin of [`Self::scheduling_sink`].
    /// Read by the native [`Self::start_receive_loop`] (the ticker arm drives it
    /// before `poll_bound`, so the same tick polls any freshly-bound channel).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    inbox_drain: Mutex<Option<Arc<dyn InboxDrainSource>>>,
    /// The recipient contact gate for cross-user shared folders — the "auto for
    /// contacts, knock for strangers" decision seam (`docs/goal/ui/folders.md` §
    /// Sharing). `None` until [`Self::register_folder_gate`] wires it (the FFI
    /// factory injects a `NestFolderGate`); a session without it leaves every
    /// [`WelcomeChannelKind::Folder`] welcome un-acked (retained), the pre-gate
    /// behaviour. The gate reads the sharer's contact-status behind the
    /// [`FolderGateSink`] seam and returns an [`ArrivalDisposition`]; the receive
    /// rail acts on it (join off the chat rail / stage / drop) — the folder twin
    /// of [`Self::scheduling_sink`], keeping the contacts client out of this crate.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    folder_gate: Mutex<Option<Arc<dyn FolderGateSink>>>,
    /// The cross-device MLS state-sync launcher (`docs/goal/behavior/devices.md` §
    /// Cross-device MLS group-state sync, slice 5). `None` until
    /// [`Self::set_mls_sync_launcher`] wires it — the native FFI factory injects one
    /// for all three native legs (apple/windows/android); linux drives the plane via
    /// its own glib path and web via its wasm bootstrap, so both leave it unset.
    /// When set, [`Self::start_receive_loop`] runs it once, before the first poll
    /// (design §5 restore-before-first-poll), behind the crypto-free
    /// [`MlsSyncLauncher`] seam so this crate gains no `fauna-client-mls-sync`
    /// dependency — the launch twin of [`Self::folder_gate`].
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    mls_sync_launcher: Mutex<Option<Arc<dyn MlsSyncLauncher>>>,
    /// The content-index builder launcher (`docs/goal/behavior/content-index.md`
    /// § Ingest triggers, v1). `None` until [`Self::set_index_builder_launcher`]
    /// wires it — web registers none (no browser tantivy). When set,
    /// [`Self::start_receive_loop`] runs it once **before the first poll** and
    /// registers the observer it returns, which is what stops that launch's
    /// mailbox re-walk from going unindexed. Behind the tantivy-free
    /// [`IndexBuilderLauncher`] seam, so this crate gains no
    /// `fauna-client-index` dependency — the launch twin of
    /// [`Self::mls_sync_launcher`].
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    index_launcher: Mutex<Option<Arc<dyn IndexBuilderLauncher>>>,
    /// The peer-anchor harvest sweep launcher (`identity-succession.md` § The
    /// succession statement → *the peer-profile harvest*). `None` until
    /// [`Self::set_peer_anchor_sweep_launcher`] wires it — web registers none
    /// (this loop is wasm-excluded and its bootstrap drives its own). When set,
    /// [`Self::start_receive_loop`] launches it **first** in the prologue, ahead
    /// of the awaited replica restore, because the sweep is fire-and-forget and
    /// races a ceremony's statement rather than the loop. Behind the
    /// recovery-type-free [`PeerAnchorSweepLauncher`] seam so this crate gains no
    /// `fauna-client-recovery` dependency — the launch twin of
    /// [`Self::mls_sync_launcher`], and the producer half of the witness
    /// [`Self::set_succession_witness`] registers.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    peer_anchor_sweep_launcher: Mutex<Option<Arc<dyn PeerAnchorSweepLauncher>>>,
    /// Per-mailbox `after_uid` paging cursors `(inbox, sent)` for the manual
    /// [`Self::poll_mail`] backstop — the mail twin of [`Self::conv_cursors`].
    /// `after_uid` is monotonic per mailbox, so a fresh dedup `seen` set per poll
    /// is sufficient (it only guards within one call's multi-page drain); the
    /// cursor is read + copied, polled without the lock held across the `.await`,
    /// then the advanced value written back. The detached `start_receive_loop`
    /// keeps its own independent cursors.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    mail_cursors: Mutex<(u32, u32)>,
    /// The bridged rail's receive half ([`Self::register_bridged`]): the
    /// backend the rooms load into and the source the inbox is read from.
    bridged: Mutex<Option<BridgedFeed>>,
    /// The bridged inbox's row cursor and dedup set — ONE, shared by
    /// [`Self::poll_bridged`] and the receive loop, and held across the read:
    /// `ingest_inbound` is not idempotent, so two readers with cursors of
    /// their own would ingest a row twice.
    bridged_cursor: Arc<futures_util::lock::Mutex<(i64, HashSet<i64>)>>,
    /// The session-closed signal's sender ([`SessionClosed`]): never sent on,
    /// dropped with the session — which is the event every actor-scoped
    /// background task selects on (the detached [`Self::start_receive_loop`]
    /// task first among them, the linux re-injection guard — see that method).
    /// Native-only; the wasm build spawns no tasks and has no tokio.
    #[cfg(not(target_arch = "wasm32"))]
    closed: tokio::sync::watch::Sender<()>,
    /// The moderation **local-detection store** this session owns — the encrypted-mode
    /// social-content moderation signal (`docs/goal/behavior/moderation.md` § Layout &
    /// flow: the moderation queue is the union of the server obligation rows and the
    /// client's own post-decrypt local detections). A clone of this `Arc` is installed
    /// on [`Self::manager`] in [`Self::from_manager`], so the receive loop's post-decrypt
    /// classify hook ([`ConversationsManager::ingest_inbound_to_thread`]) `observe`s into
    /// it and the queue reader ([`Self::moderation_local_detections`]) reads it — one
    /// store, two ends. **Session-owned** (per-user lifetime, dropped on logout) so
    /// retained detections never outlive the session or leak across users — the
    /// per-session shape, not web's process-global (mirroring
    /// `fauna_client_mail_settings::InboxSpamScorer`'s per-session ownership).
    local_detections: Arc<Mutex<LocalDetectionStore>>,
    /// This session's **receive-cycle counters** — the completion observable for
    /// the detached loop's full sweep ([`ReceiveCycles`], which owns the
    /// contract). Cloned into the loop task, which is the only writer; every
    /// reader goes through [`Self::receive_cycles`].
    receive_cycles: Arc<ReceiveCycles>,
    /// The **poke** half ([`Self::poke_receive_cycle`]): a run-one-cycle-now
    /// signal the detached loop selects on beside its backstop ticker. Native
    /// only — the wasm build never spawns that loop and mirrors both halves on
    /// its own rail (`apps/fauna-web/src/lib/conversations.ts`).
    #[cfg(not(target_arch = "wasm32"))]
    receive_poke: Arc<tokio::sync::Notify>,
}

/// How many **full receive sweeps** this session's detached loop has begun and
/// finished — the completion observable convention 14 asks for beside a poke
/// (`docs/goal/architecture/e2e-conventions.md` § convention 14, mechanism 3),
/// published to the apps' e2e agents as `fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`.
///
/// **Contract.** Both counters start at 0 for a fresh session and only ever
/// increase. A cycle bumps `started` before it reads anything and `completed`
/// after its last rail has finished, so `completed` never runs ahead of
/// `started`. A *cycle* is the whole `full_sweep!` — durable inbox drain, then
/// the conversation, mail, scheduling and folder rails — whichever arm
/// triggered it (ticker, reconnect, or poke); the push arms' single-rail sweeps
/// are deliberately NOT cycles, since a caller asking "has a sweep run" means
/// the sweep that would have delivered *anything*.
///
/// ⚠ **Both halves are load-bearing — do not simplify to one counter**, for the
/// reason `ALERT_SWEEP_PASSES_KEY` states in full: a bare completion count
/// cannot distinguish "a cycle that began after my trigger finished" from "a
/// cycle already in flight when I triggered finished". Read `started` at the
/// trigger and wait for `completed` to exceed *that* value: only `started`
/// cycles existed then, so the (started+1)-th completion must have begun later.
/// That pigeonhole needs no count of how many loops are live, which is what
/// keeps it sound across the linux re-injection case (a replaced session's loop
/// runs until its next loop-top liveness check).
#[derive(Debug, Default)]
pub struct ReceiveCycles {
    started: std::sync::atomic::AtomicU64,
    completed: std::sync::atomic::AtomicU64,
    ended: std::sync::atomic::AtomicBool,
    /// [`ReceiveLoopExit::code`], `0` while the loop runs.
    exit: std::sync::atomic::AtomicU8,
}

/// Why the detached receive loop left — [`ReceiveCycles::exit`], the half of the
/// teardown observable an app can act on.
///
/// Two exits are designed and say nothing is wrong: the holder let go of the
/// session, or the engine was handed over. The third is a defect. `ended` alone
/// flips for all three, so a reader of `ended` could only cry wolf at every
/// account switch or stay silent on a dead rail — the silence being what let a
/// panicking pass deny web's receive rail for three days with no user told
/// (`conversation-rooms.md` § Implementation status today).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveLoopExit {
    /// The session was dropped ([`SessionClosed`]).
    SessionClosed,
    /// The engine this loop drives was retired — a hand-over
    /// ([`crate::backends::fauna_mls::EngineRetired`]).
    EngineRetired,
    /// A receive pass panicked. The loop is **not** restarted in place: the
    /// panic's broken invariant is unknowable — `std` locks the pass held are
    /// poisoned and the MLS engine's in-memory group state may be half-applied —
    /// and a re-armed loop could fold over that state and let the replica
    /// autosave persist it, turning a dead rail into lost group state. Nor does
    /// the session rebuild itself through the factory: retiring the poisoned
    /// predecessor takes locks the same panic may have poisoned. So the manager
    /// is told ([`crate::manager::ConversationsManager::receive_stopped`]) and
    /// the app says to restart it — recoverable by construction, since a fresh
    /// process restores from what was durably written
    /// (`ui/conversations.md` § Errors & edge cases).
    Panicked,
}

impl ReceiveLoopExit {
    /// The word `conv_receive_cycles.exit` carries
    /// (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`).
    pub fn as_wire_word(self) -> &'static str {
        match self {
            Self::SessionClosed => "closed",
            Self::EngineRetired => "retired",
            Self::Panicked => "panicked",
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::SessionClosed => 1,
            Self::EngineRetired => 2,
            Self::Panicked => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::SessionClosed),
            2 => Some(Self::EngineRetired),
            3 => Some(Self::Panicked),
            _ => None,
        }
    }
}

impl ReceiveCycles {
    /// Cycles begun. Bumped **before** the sweep touches any rail.
    pub fn started(&self) -> u64 {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Cycles finished. Bumped **after** the last rail of the sweep returns.
    pub fn completed(&self) -> u64 {
        self.completed.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn begin(&self) {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn finish(&self) {
        self.completed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the detached receive-loop task has **left** — the loop's positive
    /// teardown observable, and this third counter's reason for existing: the two
    /// above can only ever say a cycle happened, so "the loop is gone" had to be
    /// inferred from an *absence* of further cycles, which is the settle-sleep
    /// shape convention 14 forbids (`e2e-conventions.md` § convention 14). This
    /// says it positively, so a teardown assertion is a deadline poll like every
    /// other.
    ///
    /// Set by the loop task's own drop guard, so it holds for **every** exit —
    /// the session-closed arm, the engine-retired arm, and a panic alike.
    /// One-way: a session builds one loop, and a rebuild builds a new session.
    /// It does not say *which* exit — [`Self::exit`] does.
    pub fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn end(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Why the loop left, or `None` while it runs. Recorded **before** `ended`
    /// flips, so a reader that saw `ended` also sees the reason — with one
    /// exception that stays `None` forever: a runtime shutting down drops the
    /// parked task without it returning or panicking, and that only happens as
    /// the process ends.
    pub fn exit(&self) -> Option<ReceiveLoopExit> {
        ReceiveLoopExit::from_code(self.exit.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// First write wins — one loop, one exit.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn record_exit(&self, exit: ReceiveLoopExit) {
        let _ = self.exit.compare_exchange(
            0,
            exit.code(),
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        );
    }
}

/// The backstop cadence in seconds: `FAUNA_CONV_POLL_SECS` when it parses to a
/// positive integer, else [`DEFAULT_CONV_POLL_SECS`].
///
/// Pure, and separated from the `std::env::var` read purely so it is testable —
/// the cadence rule is the kind of interval logic convention 14 puts at tier_1
/// rather than leaving to an e2e to demonstrate. `0` is rejected on purpose: a
/// zero-second `tokio::time::interval` is a busy loop, so a mis-set override
/// falls back to the production cadence instead of spinning the client.
#[cfg(not(target_arch = "wasm32"))]
fn resolve_poll_secs(raw: Option<String>) -> u64 {
    raw.and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_CONV_POLL_SECS)
}

/// The `FAUNA_CONV_POLL_SECS` override [`resolve_poll_secs`] resolves against —
/// **only in a test-capable build**.
///
/// Compile-gated outer, env inner (`e2e-automation-surface-gating.md` §
/// convention 15), the `conv_push_source` / `always_resident::debounce_delay`
/// pair shape. ⚠ Until 2026-08-21 this read was ungated, alongside the push-arm
/// kill-switch it sits beside in `conv_push_source`'s own doc comment: a shipped
/// build honoured a caller-supplied cadence, so setting it huge muted the
/// missed-push recovery rail (`api-layers.md` § Inbox & Messaging, layer 3)
/// until the process restarted. A **lesser class** than the kill-switch — the
/// push arm stays live and a mis-set value falls back to the production cadence
/// — and it is gated for the 2026-08-02 ruling's conclusion (3) reason rather
/// than severity's: the allowlist is for reads production genuinely needs, and
/// this is a harness knob no deployment sets. It carries no `FAUNA_E2E_` prefix,
/// which is exactly why every prefix-keyed sweep walked past it (the
/// `FAUNA_KEYRING_APP` lesson: a harness variable is defined by who sets it, not
/// by how it is spelled).
///
/// Reuses this crate's existing `test-helpers` feature — already forwarded by
/// linux, tui and `fauna-ffi` — so every consumer that could reach the override
/// still can, with no manifest change.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(test, debug_assertions, feature = "test-helpers")
))]
fn poll_secs_override() -> Option<String> {
    std::env::var("FAUNA_CONV_POLL_SECS").ok()
}

/// Production twin of [`poll_secs_override`] — no env read exists in this build,
/// so the backstop cadence is [`DEFAULT_CONV_POLL_SECS`] by construction.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(test, debug_assertions, feature = "test-helpers"))
))]
fn poll_secs_override() -> Option<String> {
    None
}

impl ConversationsSession {
    /// Build a FaunaMls-wired session from its parts. **Plain Rust, not UniFFI**:
    /// it takes a `dyn ConversationsRpc` so tests inject a mock nest while the FFI
    /// factory passes the real `NestConversationsRpc`. Mirrors the wasm wrapper's
    /// `with_conversations` FaunaMls half (build engine → `ConversationsManager`
    /// → `FaunaMlsBackend::new` → `register_backend` → stash the concrete backend).
    ///
    /// Only the FaunaMls rail is registered here. The SMTP rail is added via
    /// [`Self::register_smtp`] (the native twin of the wasm wrapper's *second*
    /// `register_backend` in `with_conversations`) — the FFI factory
    /// (`fauna-ffi::FfiNestClient::conversations_session`) calls it with a
    /// `NestClient`-backed sink right after this constructor, and tests inject a
    /// recording sink. `from_parts` itself stays FaunaMls-only so the receive-path
    /// tests need no mail sink.
    ///
    /// Builds a fresh internal [`ConversationsManager`]. A Rust-native app that
    /// already owns a manager singleton (the linux `conversations::host::manager()`
    /// the GTK UI + the e2e mock backends observe) instead hands it to
    /// [`Self::from_manager`], so the session drives the same manager the UI reads —
    /// see that constructor.
    pub fn from_parts(
        engine: Arc<MlsEngine>,
        rpc: Arc<dyn ConversationsRpc>,
        self_address: String,
        self_actor: ActorId,
        push: Option<Arc<dyn ConversationsPush>>,
    ) -> Arc<Self> {
        Self::from_manager(
            ConversationsManager::new(),
            engine,
            rpc,
            self_address,
            self_actor,
            push,
        )
    }

    /// Build a FaunaMls-wired session over an **existing** [`ConversationsManager`]
    /// the caller already owns — the constructor a Rust-native app uses to keep
    /// its manager singleton as the single observable surface while still driving the
    /// shared receive loop. The FaunaMls rail is registered on the passed manager
    /// (idempotent — a repeat registration overwrites the entry, e.g. replacing an
    /// e2e mock FaunaMls backend with the real one), so `self.manager()` returns the
    /// **same** `Arc` the caller passed and its UI observes every ingest. The native
    /// twin of `from_parts`, differing only in who owns the manager; the FFI factory
    /// (which has no pre-existing manager) uses `from_parts`.
    pub fn from_manager(
        manager: Arc<ConversationsManager>,
        engine: Arc<MlsEngine>,
        rpc: Arc<dyn ConversationsRpc>,
        self_address: String,
        self_actor: ActorId,
        push: Option<Arc<dyn ConversationsPush>>,
    ) -> Arc<Self> {
        // ONE live cell for the whole session — both rails read it at use time,
        // so [`Self::set_self_address`] heals them together (`conversations.md`
        // § State & data shape → *Self-address: live, never baked*).
        let self_address = SelfAddress::new(self_address);
        let fauna_mls = Arc::new(FaunaMlsBackend::new_shared(
            engine,
            rpc,
            Arc::clone(&self_address),
            self_actor,
        ));
        manager.register_backend(fauna_mls.clone());
        // Own the moderation local-detection store here (per-session lifetime) and
        // install a clone on the manager, so the receive loop's post-decrypt classify
        // hook writes it and the queue reader reads the same store. A fresh login builds
        // a fresh session → a fresh store, so detections never leak across users even
        // when a Rust-native app keeps one manager singleton across logins.
        let local_detections = Arc::new(Mutex::new(LocalDetectionStore::new()));
        manager.set_local_detection_store(Arc::clone(&local_detections));
        Arc::new(Self {
            manager,
            fauna_mls,
            self_address,
            conv_cursors: Mutex::new(HashMap::new()),
            scheduling_cursors: Mutex::new(HashMap::new()),
            folder_cursors: Mutex::new(HashMap::new()),
            push,
            mail_inbox: Mutex::new(None),
            mail_sent: Mutex::new(None),
            scheduling_sink: Mutex::new(None),
            inbox_drain: Mutex::new(None),
            folder_gate: Mutex::new(None),
            mls_sync_launcher: Mutex::new(None),
            peer_anchor_sweep_launcher: Mutex::new(None),
            index_launcher: Mutex::new(None),
            mail_cursors: Mutex::new((0, 0)),
            bridged: Mutex::new(None),
            bridged_cursor: Arc::default(),
            #[cfg(not(target_arch = "wasm32"))]
            closed: tokio::sync::watch::channel(()).0,
            local_detections,
            receive_cycles: Arc::new(ReceiveCycles::default()),
            #[cfg(not(target_arch = "wasm32"))]
            receive_poke: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// This session's receive-cycle counters — see [`ReceiveCycles`] for the
    /// contract and why both halves are load-bearing. Handed out (rather than
    /// projected here) so each app's e2e leg is a shared one-line read via
    /// [`crate::state_json::conv_receive_cycles_json`] instead of its own
    /// bookkeeping (priority #2).
    pub fn receive_cycles(&self) -> Arc<ReceiveCycles> {
        Arc::clone(&self.receive_cycles)
    }

    /// **Run one receive cycle now** — convention 14's `run_now` poke applied to
    /// the client receive loop (`docs/goal/architecture/e2e-conventions.md`
    /// § convention 14, mechanism 3).
    ///
    /// Returns immediately: it signals the detached loop, which runs the *same*
    /// `full_sweep!` its backstop ticker runs — the real path, never a bypass —
    /// and bumps [`ReceiveCycles`] around it. The caller's barrier is therefore
    /// the counters, not this call: read `started` before poking, then wait for
    /// `completed` to exceed it.
    ///
    /// **Why not an awaited ack.** A oneshot reply would make this a true
    /// barrier, but it hangs when no loop is running (a session built and never
    /// started, an app poked pre-auth) because the reply sender then simply
    /// waits in a queue nobody drains. Signal-plus-counters degrades loudly
    /// instead: the counters never move, and the consumer's deadline poll fails
    /// naming the app and the key (convention 7's no-silent-skip rule) rather
    /// than blocking until the harness timeout.
    ///
    /// A poke that lands mid-sweep is not lost: the signal stores one permit, so
    /// the loop runs a fresh cycle as soon as the in-flight one returns — which
    /// is what keeps the counter contract's "began after my trigger" honest.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn poke_receive_cycle(&self) {
        self.receive_poke.notify_one();
    }

    /// The shared per-actor [`MlsEngine`] the FaunaMls backend drives — handed out
    /// so the owner-side **folder** content-key orchestration
    /// (`fauna_client_folders::orchestration::FoldersAuthor`, via the
    /// `FolderGroupCrypto` adapter on `Arc<MlsEngine>`) reuses the SAME engine over
    /// the SAME `mls_state.db` as conversations, rather than opening a second
    /// `MlsEngine` racing on the one SQLite file (the FFI/wasm `folders_*` author
    /// seam). A folder's MLS group thus lives on the same engine as the chat
    /// groups (welcome/commit reuse) and survives a crash. **Plain Rust, not
    /// UniFFI** (kept out of the `uniffi::export` impl): `Arc<MlsEngine>` is not a
    /// UniFFI type — the native `folders_*` free fns call this from inside
    /// `fauna-ffi`, never across the FFI boundary.
    pub fn engine(&self) -> Arc<MlsEngine> {
        self.fauna_mls.engine()
    }

    /// The conversation channels this account has joined, as raw 32-byte ids —
    /// the membership half of the account plane's content-scope set
    /// ([`FaunaMlsBackend::conv_channels`] owns the derivation and its
    /// residual). Shaped as plain bytes rather than `ChannelId` because the
    /// consumer is `fauna_sync_engine::account_runtime`'s `MembershipSource`,
    /// which must not depend on this crate.
    ///
    /// Local state: the engine knows what it joined with no nest in sight,
    /// which is what lets a nest-less replica still walk its channels.
    pub fn joined_conv_channels(&self) -> Vec<[u8; 32]> {
        self.fauna_mls
            .conv_channels()
            .into_iter()
            .map(|c| c.0)
            .collect()
    }

    /// The shared [`FaunaMlsBackend`] this session drives — handed to the slice-5
    /// cross-device MLS-sync leg so it can wire the device-owned-epoch plane over
    /// the *same* backend the receive loop polls: build
    /// `fauna_client_mls_sync::BackendCatchUp` over it and inject the
    /// [`crate::backend::CommitGate`] (`set_commit_gate`) + the
    /// [`crate::backend::ChannelCursor`] (`set_channel_cursor`). [`from_manager`]
    /// creates and registers the backend internally, so this accessor is the only
    /// handle the leg gets to it. **Plain Rust, not UniFFI** (kept out of the
    /// `uniffi::export` impl — `Arc<FaunaMlsBackend>` is not a UniFFI type; the
    /// native leg calls this from inside the client glue, never across FFI).
    ///
    /// [`from_manager`]: Self::from_manager
    pub fn backend(&self) -> Arc<FaunaMlsBackend> {
        Arc::clone(&self.fauna_mls)
    }

    /// Register the SMTP rail on this session's manager with the given outbound
    /// sink — the native twin of the *second* `register_backend` in the wasm
    /// wrapper's dual-rail `with_conversations`. **Plain Rust** (takes an injected
    /// `dyn OutboundMailSink`) so the FFI factory passes a `NestClient`-backed sink
    /// (`fauna-ffi`) while tests pass a recording sink — the crate itself stays
    /// transport-free (the sink performs the `fauna.email.send` WS-RPC). Idempotent:
    /// a repeat call overwrites the `Rail::Smtp` slot. After this, an email
    /// recipient resolves (`SmtpBackend::resolve_address`) and `manager.send` /
    /// `send_new_thread` route mail through the sink (`docs/goal/ui/conversations.md`
    /// § User actions: `dm-send-button`). The backend shares the session's live
    /// self-address cell, so it needs no re-registration when the address lands.
    pub fn register_smtp(&self, sink: Arc<dyn OutboundMailSink>) {
        self.manager
            .register_backend(Arc::new(SmtpBackend::new_shared(
                sink,
                Arc::clone(&self.self_address),
            )));
    }

    /// Wire the inbound mail read-feed sources (`INBOX` + `Sent`) the receive path
    /// polls — the **receive** twin of [`Self::register_smtp`] (send). Plain Rust
    /// (takes injected `dyn InboundMailSource`s) so the FFI factory passes
    /// nest-backed sources over `EmailClient` while tests pass mocks — the crate
    /// stays transport-free and crypto-free (each source does its own
    /// `fauna.email.{inbox,sent}.fetch` + `open_inbound_record` decrypt). Both
    /// mailboxes are read because mail sent from another MUA/device lands in the
    /// actor's own sealed `Sent` and must surface in the unified view too
    /// (`docs/goal/ui/conversations.md` § Receiving into the conversations view).
    /// Idempotent: a repeat call overwrites the sources. After this,
    /// [`Self::start_receive_loop`]'s ticker and [`Self::poll_mail`] drive mail
    /// into the manager via the shared [`poll_inbound_mail`] driver.
    pub fn register_mail_receive(
        &self,
        inbox: Arc<dyn InboundMailSource>,
        sent: Arc<dyn InboundMailSource>,
    ) {
        *self.mail_inbox.lock().unwrap() = Some(inbox);
        *self.mail_sent.lock().unwrap() = Some(sent);
    }

    /// Register the bridged rail — one backend for every bridge
    /// (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
    /// adapter*) — over the glue's two seams, send and receive together:
    /// `sink` resolves addresses and submits, `source` reads the rooms and the
    /// inbox. Plain Rust like [`Self::register_smtp`], so the native glue
    /// passes its nest-backed object and tests pass fakes. After this a typed
    /// far address resolves, `manager.send` routes a bridged thread through
    /// the sink, and [`Self::start_receive_loop`]'s ticker, its
    /// `ConvPushEvent::BridgedChanged` arm and [`Self::poll_bridged`] drive
    /// the inbox in through the shared [`poll_inbound_bridged`] driver.
    /// Idempotent: a repeat call replaces the backend and both seams.
    pub fn register_bridged(&self, sink: Arc<dyn BridgedSink>, source: Arc<dyn BridgedSource>) {
        let backend = Arc::new(BridgedBackend::new(sink));
        self.manager.register_backend(backend.clone());
        *self.bridged.lock().unwrap() = Some(BridgedFeed { backend, source });
    }

    /// One read of the bridged rooms and inbox — the bridged twin of
    /// [`Self::poll_mail`], and what the receive loop's sweep runs. `Ok(0)`
    /// until [`Self::register_bridged`] wired the rail. Returns the number of
    /// newly ingested messages.
    pub async fn poll_bridged(&self) -> Result<u32, BackendError> {
        let feed = self.bridged.lock().unwrap().clone();
        poll_bridged_feed(feed.as_ref(), &self.manager, &self.bridged_cursor).await
    }

    /// Wire the calendar-apply [`SchedulingSink`] the scheduling drain routes a
    /// decrypted iMIP to — the mailbox-less CalDAV WS-RPC rail
    /// (`docs/goal/behavior/caldav-server.md` § Server-side auto-schedule,
    /// Half-1). Plain Rust (takes an injected `dyn SchedulingSink`) so the FFI
    /// factory passes a nest-backed `NestSchedulingSink` (which holds the
    /// `CalDavClient` + lazily-derived MSEK) while tests pass a mock — the crate
    /// stays calendar/crypto-free. The receive twin of [`Self::register_smtp`]'s
    /// scheduling counterpart; idempotent (a repeat call overwrites the sink).
    /// After this, [`Self::start_receive_loop`]'s ticker and [`Self::poll_scheduling`]
    /// drain every [`WelcomeChannelKind::Scheduling`] channel the backend joins.
    pub fn register_scheduling_sink(&self, sink: Arc<dyn SchedulingSink>) {
        *self.scheduling_sink.lock().unwrap() = Some(sink);
    }

    /// Wire the durable inbox-apply backstop [`InboxDrainSource`] the receive
    /// loop's ticker drains — the missed-push recovery rail
    /// (`docs/goal/architecture/api-layers.md` § Inbox & Messaging, layer 3).
    /// Plain Rust (takes an injected `dyn InboxDrainSource`) so the native glue
    /// passes a `NestInboxDrainSource` (which holds the `Arc<NestClient>` +
    /// `Weak<ConversationsSession>` and drives the shared `fauna_client_inbox::drain`)
    /// while this crate gains no `RpcRequester` dependency — the drain twin of
    /// [`Self::register_scheduling_sink`]. Idempotent (a repeat call overwrites
    /// the source). After this, [`Self::start_receive_loop`]'s ticker recovers a
    /// Welcome whose best-effort push was missed from the durable queue.
    pub fn register_inbox_drain(&self, source: Arc<dyn InboxDrainSource>) {
        *self.inbox_drain.lock().unwrap() = Some(source);
    }

    /// Wire the recipient contact gate for cross-user shared folders — the "auto
    /// for contacts, knock for strangers" decision seam (`docs/goal/ui/folders.md`
    /// § Sharing). Plain Rust (takes an injected `dyn FolderGateSink`) so the FFI
    /// factory passes a nest-backed `NestFolderGate` (which holds a `ContactsClient`
    /// and maps `contact_arrival_disposition`) while tests pass a mock — the crate
    /// stays contacts-free (the receive twin of [`Self::register_scheduling_sink`]).
    /// Idempotent (a repeat call overwrites the gate). After this, both the receive
    /// loop's push arm and the durable-inbox drain route a [`WelcomeChannelKind::Folder`]
    /// welcome through the gate: `Auto` joins it off the chat rail + acks, `Knock`
    /// leaves it un-acked (the pending-share), `Suppress` acks-and-drops.
    pub fn register_folder_gate(&self, gate: Arc<dyn FolderGateSink>) {
        *self.folder_gate.lock().unwrap() = Some(gate);
    }

    /// Register the member content-key **custody-ingest** seam (Phase 0 — the read
    /// leg; `docs/goal/ui/folders.md` § Sharing). Plain pass-through to the
    /// FaunaMls backend (which holds the engine the open needs); the FFI/wasm
    /// factory passes a nest-backed `NestFolderCustodySink` (which holds the
    /// folders client + the folder-key store) while tests pass a mock, so this
    /// crate stays free of a `fauna-client-folders` dependency — the custody twin
    /// of [`Self::register_folder_gate`]. After this, [`join_folder_welcome`]
    /// and the folder commit poll fetch + open + merge the owner's content-key
    /// envelope into this member's own custody so a member (not just the owner) can
    /// decrypt the set's content.
    pub fn set_folder_custody_sink(&self, sink: Arc<dyn FolderCustodySink>) {
        self.fauna_mls.set_folder_custody_sink(sink);
    }

    /// Register the in-group succession-statement verifier
    /// ([`SuccessionWitness`] — `identity-succession.md` § Propagation → *MLS
    /// groups*). Plain pass-through to the FaunaMls backend, and the same
    /// priority-#2 shape as [`Self::set_folder_custody_sink`]: the app layer
    /// passes `fauna_client_recovery::ChainWitness`, which owns the anchor
    /// sources and the chain walk, so this crate keeps no recovery dependency.
    ///
    /// Unset is a supported state, not a misconfiguration — a session with no
    /// witness renders every succession pair as the bare add, identical to an
    /// older client that cannot decode the variant.
    pub fn set_succession_witness(&self, witness: Arc<dyn SuccessionWitness>) {
        self.fauna_mls.set_succession_witness(witness);
    }

    /// Register the **floor-roster report** seam ([`RoomRosterReporter`],
    /// `conversation-rooms.md` § The floor roster). Plain pass-through to the
    /// FaunaMls backend, the same priority-#2 shape as
    /// [`Self::set_succession_witness`]: the glue layer builds the object —
    /// in practice the same `ConversationsRpc` object it already built for the
    /// backend, which implements this trait too — and the backend reports
    /// through it after every membership commit it authors on a governed room.
    ///
    /// Unset is a supported state: the backend tallies every owed report
    /// (`FaunaMlsBackend::roster_report_counts`) and drops it. What that costs
    /// is the home nest's routing, custody-serving and succession-target
    /// precision — never confidentiality, which stays MLS's, and never the
    /// gesture that owed the report, which is already on the log.
    pub fn set_room_roster_reporter(&self, reporter: Arc<dyn RoomRosterReporter>) {
        self.fauna_mls.set_room_roster_reporter(reporter);
    }

    /// Register the floor-roster **read** seam ([`RoomRosterReader`]) — the
    /// read half of [`Self::set_room_roster_reporter`], and the one path that
    /// resolves a room member this device has never met
    /// (`conversation-rooms.md` § Implementation status today, the roster
    /// bullet). Wired with the same glue object as the reporter.
    ///
    /// Unset is a supported state, and a quiet one: such a member keeps
    /// rendering as its elided actor id, exactly as before the read existed.
    pub fn set_room_roster_reader(&self, reader: Arc<dyn RoomRosterReader>) {
        self.fauna_mls.set_room_roster_reader(reader);
    }

    /// Register the community class's **generation-read** seam
    /// ([`RoomGenerationReader`]) — the room's key material as far as this
    /// caller is entitled to see it, wired with the same glue object as the
    /// two roster seams (`conversation-rooms.md` § The three classes →
    /// *Community*).
    ///
    /// Unset is a supported state: a `RoomSealed` record simply stays
    /// unopened, which is exactly the declared absence the receive walk
    /// carried before any app called a room kind.
    pub fn set_room_generation_reader(&self, reader: Arc<dyn RoomGenerationReader>) {
        self.fauna_mls.set_room_generation_reader(reader);
    }

    /// Register the **group-reception key** seam
    /// ([`GroupReceptionKeys`]) — this account's own wrap-target
    /// keypairs, whose secret halves open the wraps the seam above serves.
    ///
    /// Its glue object is a *different* one: the records rest on the account
    /// plane rather than on the nest, so this is wired from wherever the app
    /// holds its account runtime, not from the conversations RPC. Unset gives
    /// the same quiet skip.
    pub fn set_group_reception_keys(&self, keys: Arc<dyn GroupReceptionKeys>) {
        self.fauna_mls.set_group_reception_keys(keys);
    }

    /// Register the **room-ceremony** seam ([`RoomCeremonyRpc`]) — founding a
    /// community room and publishing its generations.
    ///
    /// Wired with the same glue object as the conversations RPC (it is a
    /// separate trait on it, the `RoomRosterReporter` shape). Unset is *not* a
    /// quiet skip here: founding refuses by name, because a room founded on a
    /// device that cannot key it is a room nobody could ever send into.
    pub fn set_room_ceremony(&self, ceremony: Arc<dyn RoomCeremonyRpc>) {
        self.fauna_mls.set_room_ceremony(ceremony);
    }

    /// Register all four nest-backed room seams at once ([`RoomSeams`]):
    /// the roster report and read, the generation read and the ceremony.
    /// This is the call every glue site makes. The group-reception keys are
    /// registered separately, from the account-store-ready edge.
    pub fn set_room_seams(&self, seams: RoomSeams) {
        self.fauna_mls.set_room_seams(seams);
    }

    /// Found a **community room** and bind it to `thread_id`
    /// (`conversation-rooms.md` § The three classes → *Community*): mint or
    /// reuse this account's wrap target, run the birth ceremony, key the room
    /// over the floor it seats, and bind the channel its log lives on.
    ///
    /// # Errors
    /// As [`crate::backends::fauna_mls::FaunaMlsBackend::found_community_room`].
    pub async fn found_community_room(
        &self,
        thread_id: crate::thread::ThreadId,
        name: Option<String>,
    ) -> Result<ChannelId, BackendError> {
        self.fauna_mls.found_community_room(thread_id, name).await
    }

    /// Invite a principal into the community room `channel_id` hosts. The
    /// invitation is signed here and seats nobody — acceptance does
    /// (`conversation-rooms.md` § Join rules and invites).
    ///
    /// # Errors
    /// As [`crate::backends::fauna_mls::FaunaMlsBackend::invite_to_room`].
    pub async fn invite_to_room(
        &self,
        channel_id: &ChannelId,
        invitee: fauna_core::identity::ActorId,
        role: fauna_mls::room_policy::RoomRole,
        invitee_node: Option<String>,
    ) -> Result<(), BackendError> {
        self.fauna_mls
            .invite_to_room(channel_id, invitee, role, invitee_node)
            .await
    }

    /// Accept an invitation into the community room `room_id` names and bind
    /// it to `thread_id` — the act that seats this account on the floor.
    ///
    /// # Errors
    /// As [`crate::backends::fauna_mls::FaunaMlsBackend::accept_room_invite`].
    pub async fn accept_room_invite(
        &self,
        thread_id: crate::thread::ThreadId,
        room_id: [u8; 32],
        room_node: Option<String>,
    ) -> Result<ChannelId, BackendError> {
        self.fauna_mls
            .accept_room_invite(thread_id, room_id, room_node)
            .await
    }

    /// Cover a newly seated member of `channel_id` with the room's tip
    /// generation — the inviter's own act, once the invitee has accepted.
    ///
    /// # Errors
    /// As [`crate::backends::fauna_mls::FaunaMlsBackend::key_in_room_member`].
    pub async fn key_in_room_member(
        &self,
        channel_id: &ChannelId,
        target: fauna_core::identity::ActorId,
    ) -> Result<(), BackendError> {
        self.fauna_mls.key_in_room_member(channel_id, target).await
    }

    /// Register the custody-ceremony ingest seam ([`CustodyCeremonySink`] —
    /// W8.4 (account-data-plane.md § Workstreams), `account-data-plane.md` § Replica posture → *The custody grant +
    /// ceremony*). Plain pass-through to the FaunaMls backend, the same
    /// priority-#2 shape as [`Self::set_folder_custody_sink`]: the glue layer
    /// passes the ceremony machine's ingest (which owns decode + verify + the
    /// account-store capture), so this crate keeps no custody semantics.
    /// Unset is a supported state — payloads wait in the channel history for
    /// a capable session, tallied.
    pub fn set_custody_ceremony_sink(&self, sink: Arc<dyn CustodyCeremonySink>) {
        self.fauna_mls.set_custody_ceremony_sink(sink);
    }

    /// Register the share-set endpoint ingest seam ([`ShareEndpointsSink`] —
    /// the W8 share twin's slice F, `p2p.md` § Cross-user shared-set transfer
    /// → *Discovery*). The same priority-#2 pass-through: the glue layer
    /// passes an ingest that owns the binding
    /// (`fauna_peer_share::bind_share_advertisement`) and the account-plane
    /// write, so this crate keeps no share-plane semantics and takes no
    /// dependency on the gated plane. Unset is a supported state — the
    /// session caches no peer candidates and the nest-mediated path, which
    /// the contract keeps as the always-on source, is unaffected.
    pub fn set_share_endpoints_sink(&self, sink: Arc<dyn ShareEndpointsSink>) {
        self.fauna_mls.set_share_endpoints_sink(sink);
    }

    /// Post this device's own endpoint advertisement (the verbatim
    /// `fauna_core::share_endpoints::ShareEndpoints` bytes) to a shared set's
    /// channel — the discovery publish door, forwarding to the FaunaMls
    /// backend's application post. ⚠ **Not Commit-free**: that post runs the
    /// device-owned-epoch takeover — a self-`Update` **Commit** — first. On a
    /// claimed folder channel that takeover is *admitted* from any roster member
    /// (the roster-membership commit admission, `federation.md` §
    /// Cross-nest shared folders + channel append); a nest refuses the Commit
    /// of a member it has not admitted, so this send can fail on a pass. See `FaunaMlsBackend::send_share_endpoints`, which owns the
    /// full statement.
    pub async fn send_share_endpoints(
        &self,
        channel_hex: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BackendError> {
        self.fauna_mls
            .send_share_endpoints(channel_hex, bytes)
            .await
    }

    /// Post one custody-ceremony payload (the verbatim
    /// `CustodyCeremonyMessage` bytes) to the named conversation channel —
    /// the ceremony glue's send door, forwarding to the FaunaMls backend's
    /// application post. ⚠ **Not Commit-free** — see
    /// `FaunaMlsBackend::send_custody_payload`.
    pub async fn send_custody_payload(
        &self,
        channel_hex: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BackendError> {
        self.fauna_mls
            .send_custody_payload(channel_hex, bytes)
            .await
    }

    /// Post one A7 custody receipt (the verbatim signed envelope bytes) to
    /// the named conversation channel — the check-in cadence's send door,
    /// forwarding to the FaunaMls backend's `CustodyReceipt` body post.
    pub async fn send_custody_receipt(
        &self,
        channel_hex: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BackendError> {
        self.fauna_mls
            .send_custody_receipt(channel_hex, bytes)
            .await
    }

    /// Re-drive the in-group succession statements the witness refused for
    /// `old_actor` while no anchor was held. The app's peer-anchor harvest
    /// sweep calls this after a harvest that seeded something new for that
    /// peer — never because a statement asked (`identity-succession.md` § The
    /// succession statement → *the peer-profile harvest*, the re-drive rules).
    /// Pass-through to
    /// [`crate::backends::fauna_mls::redrive_parked_successions`].
    pub async fn redrive_parked_successions(&self, old_actor: &ActorId) -> u32 {
        crate::backends::fauna_mls::redrive_parked_successions(
            &self.fauna_mls,
            &self.manager,
            old_actor,
        )
        .await
    }

    /// The sweep settled `old_actor` for this session **without seeding
    /// anything** — release what waited on that harvest (same §, *the harvest
    /// wait*). Pass-through to
    /// [`crate::backends::fauna_mls::settle_parked_successions`].
    pub async fn settle_parked_successions(&self, old_actor: &ActorId) -> u32 {
        crate::backends::fauna_mls::settle_parked_successions(
            &self.fauna_mls,
            &self.manager,
            old_actor,
        )
        .await
    }

    /// Wire the content-index builder launcher — the async, MSEK-bearing
    /// login-time resume of this actor's local index
    /// (`docs/goal/behavior/content-index.md` § Ingest triggers, v1). Plain Rust
    /// (takes an injected `dyn IndexBuilderLauncher`) so the launcher owns the
    /// builder + rail publisher while this crate stays tantivy-free — the launch
    /// twin of [`Self::set_mls_sync_launcher`]. Idempotent (a repeat call
    /// overwrites the launcher). After this, [`Self::start_receive_loop`] runs it
    /// once **before the first poll** and registers the observer it returns, so
    /// the mailbox re-walk that same launch performs is indexed rather than
    /// missed. Native only: web registers none (no browser tantivy).
    pub fn set_index_builder_launcher(&self, launcher: Arc<dyn IndexBuilderLauncher>) {
        *self.index_launcher.lock().unwrap() = Some(launcher);
    }

    /// Whether a builder launcher is wired — i.e. whether this client *builds*
    /// the content index, as opposed to only querying an index another of the
    /// user's devices built and synced (`content-index.md` § Where queries run —
    /// *Build vs. query is not the same question*).
    ///
    /// Diagnostics, and the observable that lets a client's **builder** and its
    /// Task-delegation **picker** be pinned against each other. Those two are the
    /// pair the 2026-08-03 picker session found silently drifting apart, in both
    /// directions, when a per-(client, kind) rule is encoded as independent bits.
    pub fn builds_content_index(&self) -> bool {
        self.index_launcher.lock().unwrap().is_some()
    }

    /// Wire the cross-device MLS state-sync launcher — the async, crypto-bearing
    /// login-time launch of the replica plane (`docs/goal/behavior/devices.md` §
    /// Cross-device MLS group-state sync, slice 5). Plain Rust (takes an injected
    /// `dyn MlsSyncLauncher`) so the FFI factory passes a launcher that owns the
    /// `MlsStateSync` + `orchestration` handles while this crate stays MLS-type-free
    /// (the launch twin of [`Self::register_folder_gate`]). The three native legs
    /// (apple/windows/android) inherit the plane through this one injection with no
    /// per-app glue; linux + web wire it via their own platform paths and never
    /// call this. Idempotent (a repeat call overwrites the launcher). After this,
    /// [`Self::start_receive_loop`] runs it once, before the first poll (design §5
    /// restore-before-first-poll), so every restored channel resumes from its
    /// watermark.
    pub fn set_mls_sync_launcher(&self, launcher: Arc<dyn MlsSyncLauncher>) {
        *self.mls_sync_launcher.lock().unwrap() = Some(launcher);
    }

    /// Wire the peer-anchor harvest sweep — the **producer** half of the
    /// member-path succession anchor, whose consumer is the witness
    /// [`Self::set_succession_witness`] registers (`identity-succession.md`
    /// § The succession statement → *the peer-profile harvest*).
    ///
    /// Plain Rust behind an injected `dyn PeerAnchorSweepLauncher`, so a driver
    /// owning `fauna-client-recovery`'s sweep, the app's nest connection and its
    /// account store passes one while this crate stays recovery-type-free —
    /// the launch twin of [`Self::set_mls_sync_launcher`]. **Every** app that
    /// registers a witness should register this too: a witness with no producer
    /// answers `no_anchor` for exactly the Welcome-joined peers an in-group
    /// statement is about, and nothing anywhere reports the omission except the
    /// member rendering "a stranger joined" forever.
    ///
    /// Idempotent (a repeat call overwrites). After this,
    /// [`Self::start_receive_loop`] launches it once, first in the prologue.
    pub fn set_peer_anchor_sweep_launcher(&self, launcher: Arc<dyn PeerAnchorSweepLauncher>) {
        *self.peer_anchor_sweep_launcher.lock().unwrap() = Some(launcher);
    }

    /// Deliver a sealed iMIP to one **mailbox-less** Fauna attendee over the WS-RPC
    /// MLS welcome rail — the organizer SEND side of the mailbox-less CalDAV
    /// scheduling route (`docs/goal/behavior/caldav-server.md` § Server-side
    /// auto-schedule, Half-1; the send twin of the [`SchedulingSink`] receive
    /// drain). Delegates to the held [`FaunaMlsBackend::deliver_scheduling_imip`]
    /// (a one-off 1:1 group → `Scheduling` welcome → the iMIP as the first app
    /// message; never bound to a thread). Plain Rust — **not** uniffi-exported, so
    /// no binding regen: the native glue `fauna_client_conversations::NestImipDispatch`
    /// calls it from the shared organizer dispatch fork
    /// (`fauna_client_caldav::dispatch_imip_request`). `recipient_actor_hex` is the
    /// 64-hex attendee actor key; `peer_domain` is `Some(domain)` for a foreign nest
    /// / `None` same-nest. `Err(BackendError)` on a transport / key-package / MLS
    /// fault (the dispatch fork collects it best-effort — the roster is already
    /// persisted).
    pub async fn deliver_scheduling_imip(
        &self,
        recipient_actor_hex: &str,
        peer_domain: Option<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<(), BackendError> {
        let actor = ActorId::from_hex(recipient_actor_hex).map_err(|_| {
            BackendError::Internal(format!("invalid recipient actor id: {recipient_actor_hex}"))
        })?;
        self.fauna_mls
            .deliver_scheduling_imip(actor, peer_domain, raw_rfc5322)
            .await
    }
}

/// The room-post seam the feed installs (`FeedManager::set_room_post_keys`),
/// over the two owners a room-restricted post needs (`ui/feed.md` § Encryption
/// at rest → *Room-restricted — the app half*): the FaunaMls backend holds the
/// keys, both directions, and the manager holds which rooms there are and what
/// the user calls them. The key methods are the backend's own answers,
/// unchanged.
///
/// Its own type rather than only [`ConversationsSession`]'s impl because web
/// has no session: the wasm conversations manager holds the same manager and
/// backend itself, and builds this from them. The session's impl delegates
/// here, so every app's seam is this one body.
pub struct RoomPostSeam {
    manager: Arc<ConversationsManager>,
    fauna_mls: Arc<FaunaMlsBackend>,
}

impl RoomPostSeam {
    /// The seam over a FaunaMls-wired manager and that same manager's backend.
    #[must_use]
    pub fn new(manager: Arc<ConversationsManager>, fauna_mls: Arc<FaunaMlsBackend>) -> Self {
        Self { manager, fauna_mls }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_core::room_post::RoomPostKeys for RoomPostSeam {
    async fn room_post_seal_key(
        &self,
        room: [u8; 32],
    ) -> Result<
        (
            fauna_core::room_post::RoomPostSeal,
            zeroize::Zeroizing<[u8; 32]>,
        ),
        String,
    > {
        fauna_core::room_post::RoomPostKeys::room_post_seal_key(&*self.fauna_mls, room).await
    }

    async fn room_post_base_key(
        &self,
        room: [u8; 32],
        seal: fauna_core::room_post::RoomPostSeal,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
        fauna_core::room_post::RoomPostKeys::room_post_base_key(&*self.fauna_mls, room, seal).await
    }

    async fn room_post_rooms(&self) -> Vec<fauna_core::room_post::RoomPostRoom> {
        self.manager.room_post_rooms()
    }

    async fn room_home_nest_url(&self, room: [u8; 32]) -> Option<String> {
        fauna_core::room_post::RoomPostKeys::room_home_nest_url(&*self.fauna_mls, room).await
    }
}

impl ConversationsSession {
    /// This session's [`RoomPostSeam`] — two `Arc` clones, cheap per call.
    fn room_post_seam(&self) -> RoomPostSeam {
        RoomPostSeam::new(Arc::clone(&self.manager), Arc::clone(&self.fauna_mls))
    }
}

/// The session IS a room-post seam, so a native app installs it directly
/// (`feed.set_room_post_keys(session)`) — it is the one place that meets both
/// owners. Every answer is [`RoomPostSeam`]'s.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_core::room_post::RoomPostKeys for ConversationsSession {
    async fn room_post_seal_key(
        &self,
        room: [u8; 32],
    ) -> Result<
        (
            fauna_core::room_post::RoomPostSeal,
            zeroize::Zeroizing<[u8; 32]>,
        ),
        String,
    > {
        fauna_core::room_post::RoomPostKeys::room_post_seal_key(&self.room_post_seam(), room).await
    }

    async fn room_post_base_key(
        &self,
        room: [u8; 32],
        seal: fauna_core::room_post::RoomPostSeal,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
        fauna_core::room_post::RoomPostKeys::room_post_base_key(&self.room_post_seam(), room, seal)
            .await
    }

    async fn room_post_rooms(&self) -> Vec<fauna_core::room_post::RoomPostRoom> {
        fauna_core::room_post::RoomPostKeys::room_post_rooms(&self.room_post_seam()).await
    }

    async fn room_home_nest_url(&self, room: [u8; 32]) -> Option<String> {
        fauna_core::room_post::RoomPostKeys::room_home_nest_url(&self.room_post_seam(), room).await
    }
}

impl ConversationsSession {
    /// This session as a room-post seam that holds it **weakly** — what a feed
    /// manager whose lifetime the session cannot see installs
    /// (`transport-connection.md` § No dialer outlives its owner → *A holder the
    /// session cannot see never pins a dialer*). A strong install let every
    /// undisposed feed manager keep its session, and so the session's receive
    /// loop, alive past the sign-out that ended it. Once the session's own
    /// holders let go, every room post reads locked and none can be addressed:
    /// the same answers an unset seam gives.
    pub fn weak_room_post_keys(self: &Arc<Self>) -> Arc<dyn fauna_core::room_post::RoomPostKeys> {
        Arc::new(WeakRoomPostKeys(Arc::downgrade(self)))
    }
}

/// [`ConversationsSession::weak_room_post_keys`]'s seam.
struct WeakRoomPostKeys(std::sync::Weak<ConversationsSession>);

/// Why a departed session opens nothing — a reason for the reader, never a
/// key-shaped fallback (the trait's own contract).
const ROOM_POST_SESSION_CLOSED: &str = "the conversations session has closed";

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_core::room_post::RoomPostKeys for WeakRoomPostKeys {
    async fn room_post_seal_key(
        &self,
        room: [u8; 32],
    ) -> Result<
        (
            fauna_core::room_post::RoomPostSeal,
            zeroize::Zeroizing<[u8; 32]>,
        ),
        String,
    > {
        let Some(session) = self.0.upgrade() else {
            return Err(ROOM_POST_SESSION_CLOSED.into());
        };
        fauna_core::room_post::RoomPostKeys::room_post_seal_key(&*session, room).await
    }

    async fn room_post_base_key(
        &self,
        room: [u8; 32],
        seal: fauna_core::room_post::RoomPostSeal,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
        let Some(session) = self.0.upgrade() else {
            return Err(ROOM_POST_SESSION_CLOSED.into());
        };
        fauna_core::room_post::RoomPostKeys::room_post_base_key(&*session, room, seal).await
    }

    async fn room_post_rooms(&self) -> Vec<fauna_core::room_post::RoomPostRoom> {
        let Some(session) = self.0.upgrade() else {
            return Vec::new();
        };
        fauna_core::room_post::RoomPostKeys::room_post_rooms(&*session).await
    }

    async fn room_home_nest_url(&self, room: [u8; 32]) -> Option<String> {
        let session = self.0.upgrade()?;
        fauna_core::room_post::RoomPostKeys::room_home_nest_url(&*session, room).await
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl ConversationsSession {
    /// The wired [`ConversationsManager`] — the single observable surface
    /// (`docs/goal/ui/conversations.md` § State & data shape) the client drives
    /// for snapshot / send / rename / membership / recipient resolution. The
    /// FaunaMls backend is already registered on it.
    pub fn manager(&self) -> Arc<ConversationsManager> {
        self.manager.clone()
    }

    /// Update the logged-in account's canonical `<handle>@<domain>` — THE one
    /// self-heal call (`docs/goal/ui/conversations.md` § State & data shape →
    /// *Self-address: live, never baked*). Both rails read the live cell at use
    /// time, so the next SMTP send carries this `From:` (a session built before
    /// identity resolution stops refusing `no_handle`), the FaunaMls data plane
    /// routes same-nest peers against this domain, and the reply-all self-drop
    /// compares against this address. Call it from wherever identity state
    /// lands: login-time resolution, the background identity refresh, a
    /// server-side handle rename. Idempotent; nothing is rebuilt.
    pub fn set_self_address(&self, self_address: String) {
        self.self_address.set(self_address);
    }

    /// The session's retained post-decrypt **local detections**, newest-first — the
    /// client half of the moderation queue (`docs/goal/behavior/moderation.md`
    /// § Layout & flow). The queue VM unions these with the server
    /// `fauna.moderation.actions` rows via `fauna_client_moderation::merge_queue`
    /// (linux calls it directly; FFI/WASM clients over the shared façade). Empty
    /// until the receive loop classifies an incoming spam message post-decrypt.
    pub fn moderation_local_detections(&self) -> Vec<LocalDetection> {
        self.local_detections.lock().unwrap().snapshot()
    }

    /// Drop the local detection for `content_id` after the user trains a correction
    /// on that queue row (the train button → `fauna.moderation.train` also removes
    /// the client-side row, since it is corrected). No-op if it is a server row (not
    /// in this store) or already gone; returns `true` iff one was removed.
    pub fn moderation_remove_local_detection(&self, content_id: String) -> bool {
        self.local_detections.lock().unwrap().remove(&content_id)
    }

    /// The decrypted plaintext body behind one **local-detection** queue row, by
    /// its `content_id` (= the classified message's id) — the train-correction
    /// text source for the client-side tier-1 spam-model write
    /// (`MailSettingsMachine::train_spam_model_client`; the FFI twin of the
    /// linux-native `manager().message_body(..)` read). `None` once the message
    /// has aged out of the thread store — the correction then just clears the
    /// flag, exactly as before the client write path existed.
    pub fn moderation_message_body(&self, content_id: String) -> Option<String> {
        self.manager.message_body(&content_id)
    }

    /// This session's receive-cycle counters, JSON-encoded (`{"started": N,
    /// "completed": M, "exit": null | "closed" | "retired" | "panicked"}`) —
    /// the UniFFI twin of
    /// [`crate::state_json::conv_receive_cycles_json`] for apps that cross the
    /// FFI boundary (that function's `Option<&Arc<Self>>` signature isn't
    /// callable from a `&self` method; this computes the identical shape from
    /// the same [`ReceiveCycles`] getters — [`Self::receive_cycles`] owns the
    /// contract, this is not a second count). android's TestAgent re-parses the
    /// string into its own state object (the established `machine_method_result`
    /// passthrough shape, `TestAgent.kt::serializeState`) rather than
    /// re-deriving the counts in Kotlin; tui/linux/web call the free function
    /// directly and never need this.
    pub fn conv_receive_cycles_json(&self) -> String {
        let cycles = self.receive_cycles();
        serde_json::json!({
            "started": cycles.started(),
            "completed": cycles.completed(),
            "exit": cycles.exit().map(ReceiveLoopExit::as_wire_word),
        })
        .to_string()
    }

    /// The `data.conversation_threads` e2e state rows, JSON-encoded — the
    /// UniFFI twin of [`crate::state_json::conversation_threads_json`] for
    /// apps that cross the FFI boundary (that function takes `&ConversationsManager`
    /// directly, which a `&self` UniFFI method can supply from
    /// [`Self::manager`] but a free function outside this crate cannot). Same
    /// passthrough shape as [`Self::conv_receive_cycles_json`]: android's
    /// TestAgent re-parses the string into its own state object rather than
    /// re-deriving the row shape in Kotlin; tui/linux/web call the free
    /// function (or, for web, the wasm twin) directly and never need this.
    pub fn conversation_threads_json(&self) -> String {
        crate::state_json::conversation_threads_json(&self.manager).to_string()
    }

    /// UniFFI twin of [`Self::poke_receive_cycle`] — run one receive cycle now
    /// (convention 14 mechanism 3, `e2e-conventions.md`). Native only, matching
    /// [`Self::poke_receive_cycle`]'s own gate (wasm never spawns the detached
    /// receive loop this pokes).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn conv_receive_now(&self) {
        self.poke_receive_cycle();
    }

    /// This session's `mls_folded_commits` observable, JSON-encoded
    /// (`{channel_hex: count}`) — the UniFFI twin of
    /// [`crate::state_json::mls_folded_commits_json`], which this delegates to
    /// directly ([`Self::backend`] already returns the `&FaunaMlsBackend` it
    /// wants — no second count, unlike [`Self::conv_receive_cycles_json`]'s
    /// `Arc<Self>` mismatch). android's TestAgent re-parses the string into its
    /// own state object rather than re-deriving the counts in Kotlin.
    pub fn mls_folded_commits_json(&self) -> String {
        crate::state_json::mls_folded_commits_json(&self.backend()).to_string()
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl ConversationsSession {
    /// Process a same-nest MLS Welcome (`welcome_bytes` for `channel_id_hex`):
    /// join + bind the group, materialize its thread. Idempotent — a re-delivered
    /// Welcome for an already-bound channel returns the same thread id without a
    /// second join. Returns `Some(thread_id)` (the materialized thread's id
    /// string). Fed by the client's `fauna.conversations.welcome.received` push
    /// handler. The native twin of the wasm wrapper's `ingestWelcome`; all MLS
    /// crypto stays in [`FaunaMlsBackend`] (`docs/goal/ui/conversations.md` §
    /// Architectural rules #2).
    pub async fn ingest_welcome(
        &self,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
    ) -> Result<Option<String>, BackendError> {
        // Same-nest direct-ingest convenience (no home nest URL): cross-nest
        // Welcomes flow through `start_receive_loop`, which carries the home nest
        // URL off the push (`WelcomeNudge.home_nest_url`).
        let thread_id = ingest_welcome(
            &self.fauna_mls,
            &self.manager,
            &channel_id_hex,
            &welcome_bytes,
            "",
        )
        .await?;
        Ok(Some(thread_id.0))
    }

    /// Poll every bound FaunaMls channel for new ciphertext (fetch → decode →
    /// MLS-decrypt → ingest), advancing each channel's `seq` cursor. Returns the
    /// total number of messages ingested this pass. The reconnect / missed-push
    /// backstop the client's poll loop ticks — the native twin of the wasm
    /// wrapper's `pollConversations`.
    ///
    /// A per-channel failure is non-fatal — the next tick retries the whole sweep
    /// — so the loop keeps going and remembers the first error; it only returns
    /// `Err` when nothing was ingested **and** something failed (otherwise the
    /// partial success is the useful answer). The per-channel cursor is read +
    /// copied before the `.await` and written back after, so the `conv_cursors`
    /// lock is never held across the await.
    pub async fn poll_conversations(&self) -> Result<u32, BackendError> {
        adopt_sibling_groups_first(&self.fauna_mls).await;
        let mut total = 0u32;
        let mut first_err: Option<BackendError> = None;
        for channel in self.fauna_mls.bound_channels() {
            // Read + copy the cursor (no `conv_cursors` lock held across the
            // await), poll, then write the advanced cursor back. First encounter
            // seeds from the injected `ChannelCursor` (`resume_seq` — the restored
            // `history/<ch>` watermark on a device-synced client, else `0`), so a
            // restored device resumes from where this identity last folded rather
            // than re-walking pre-restore history.
            let mut cur = *self
                .conv_cursors
                .lock()
                .unwrap()
                .entry(channel)
                .or_insert_with(|| {
                    self.fauna_mls
                        .channel_cursor()
                        .map(|c| c.resume_seq(&channel))
                        .unwrap_or(0)
                });
            // Serialize this channel's inbound drain against a concurrent gated
            // commit ([`FaunaMlsBackend::channel_lock`]): a gated send stages a
            // pending the engine cannot `process_commit` over. Held across the
            // `.await`; the gate's own catch-up poll runs inside the gated section
            // and never re-takes it.
            let channel_lock = self.fauna_mls.channel_lock(&channel);
            let guard = channel_lock.lock().await;
            // Past the one-shot key-in stop, which exists for the receive
            // loop's catch-up window: this sweep has none to reopen, so a stop
            // here would only defer the newly readable room by a tick
            // (`poll_inbound_conv_past_key_in`).
            let polled = poll_inbound_conv_past_key_in(
                &self.fauna_mls,
                &self.manager,
                &channel,
                &mut cur,
                0,
            )
            .await;
            // An ownership offer the walk parked for this identity completes
            // here — after the walk, under the same lock, never inside it
            // (`FaunaMlsBackend::complete_ownership_offer_locked`).
            self.fauna_mls
                .complete_ownership_offer_locked(&channel)
                .await;
            match polled {
                Ok(outcome) => {
                    if outcome.awaiting_key {
                        tracing::debug!(
                            channel = %channel,
                            "community room not keyed in yet — the walk waits before its \
                             first sealed record"
                        );
                    } else if outcome.stalled {
                        tracing::error!(
                            channel = %channel,
                            "inbound poll stalled before an unincorporated commit — channel is \
                             behind the group's epoch until a resync heals it"
                        );
                    }
                    total += outcome.ingested as u32;
                    self.conv_cursors.lock().unwrap().insert(channel, cur);
                    // Report the folded seq back so the next `history/<ch>` save
                    // snapshots the right watermark (a no-op with no cursor seam).
                    if let Some(c) = self.fauna_mls.channel_cursor() {
                        c.advance(&channel, cur);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
            // Name any member the walk seated that this device has never met,
            // AFTER releasing the channel lock — `poll_bound`'s step, for its
            // reasons, so the two drain paths stay behaviourally identical.
            drop(guard);
            self.fauna_mls
                .tend_community_room(&self.manager, &channel)
                .await;
            // And the group-ful mirror of it: a failed best-effort birth
            // report leaves a room with no floor at its home, and a 1:1 never
            // retries it (`FaunaMlsBackend::backfill_floor_roster`).
            self.fauna_mls.backfill_floor_roster(&channel).await;
            self.fauna_mls
                .resolve_nameless_members(&self.manager, &channel)
                .await;
        }
        // Attachments the budget evicted and a render has since asked for
        // (`conversations.md` § Attachments → *Retention*).
        crate::backends::fauna_mls::refill_evicted_attachments(&self.fauna_mls, &self.manager)
            .await;
        match first_err {
            Some(e) if total == 0 => Err(e),
            _ => Ok(total),
        }
    }

    /// Poll both mail read-feeds (`INBOX` then `Sent`) for new sealed records
    /// (fetch → decode → HPKE-decrypt → ingest), advancing each mailbox's
    /// `after_uid` cursor. Returns the total ingested this pass. The reconnect /
    /// missed-tick backstop a client may call directly — the mail twin of
    /// [`Self::poll_conversations`]; [`Self::start_receive_loop`]'s ticker drives
    /// it automatically. A no-op (`Ok(0)`) until [`Self::register_mail_receive`]
    /// has wired the sources, or while mail stays unconfigured (the source returns
    /// an empty page). Same partial-success contract as `poll_conversations`: only
    /// `Err` when nothing was ingested **and** a mailbox failed. The cursor is
    /// read + copied before the `.await` and written back after, so the
    /// `mail_cursors` lock is never held across the await.
    pub async fn poll_mail(&self) -> Result<u32, BackendError> {
        let inbox = self.mail_inbox.lock().unwrap().clone();
        let sent = self.mail_sent.lock().unwrap().clone();
        let mut total = 0u32;
        let mut first_err: Option<BackendError> = None;
        if let Some(src) = &inbox {
            let mut uid = self.mail_cursors.lock().unwrap().0;
            let mut seen = HashSet::new();
            match poll_inbound_mail(src.as_ref(), &self.manager, &mut uid, &mut seen, 0).await {
                Ok(n) => {
                    total += n as u32;
                    self.mail_cursors.lock().unwrap().0 = uid;
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if let Some(src) = &sent {
            let mut uid = self.mail_cursors.lock().unwrap().1;
            let mut seen = HashSet::new();
            match poll_inbound_mail(src.as_ref(), &self.manager, &mut uid, &mut seen, 0).await {
                Ok(n) => {
                    total += n as u32;
                    self.mail_cursors.lock().unwrap().1 = uid;
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        // Mail attachments the budget evicted and a render has since asked for
        // (`conversations.md` § Attachments → *Retention*) — the twin of
        // `poll_conversations`' refill step.
        crate::backends::smtp::refill_evicted_mail_attachments(
            inbox.as_deref(),
            sent.as_deref(),
            &self.manager,
        )
        .await;
        // Owed `\Seen` writes out, flag changes made elsewhere in — after the
        // poll, so the first `INBOX` page has named the change baseline.
        if let Some(src) = &inbox {
            crate::backends::smtp::sync_mail_read_state(src.as_ref(), &self.manager).await;
        }
        match first_err {
            Some(e) if total == 0 => Err(e),
            _ => Ok(total),
        }
    }
}

// Scheduling-drain surface — the mailbox-less CalDAV iMIP rail
// (`docs/goal/behavior/caldav-server.md` § Server-side auto-schedule, Half-1).
// Native-only and **not** UniFFI-exported: the detached `start_receive_loop` is
// the production driver and no client calls a scheduling backstop directly, so
// keeping these off the UniFFI surface avoids a binding regen. Tests drive them
// directly; a future per-app backstop can promote them to `uniffi::export`.
#[cfg(not(target_arch = "wasm32"))]
impl ConversationsSession {
    /// Ingest a scheduling MLS Welcome through the session — join the one-off
    /// group + mark the channel scheduling (no chat thread). The scheduling twin
    /// of [`Self::ingest_welcome`], driven by the receive loop's `Welcome` arm for
    /// a [`WelcomeChannelKind::Scheduling`] nudge. Returns the joined channel id
    /// (hex).
    pub async fn ingest_scheduling_welcome(
        &self,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
    ) -> Result<String, BackendError> {
        // Same-nest direct-ingest convenience (no home nest URL); cross-nest
        // scheduling Welcomes flow through `start_receive_loop`.
        let channel =
            ingest_scheduling_welcome(&self.fauna_mls, &channel_id_hex, &welcome_bytes, "").await?;
        Ok(channel.to_string())
    }

    /// Accept a staged folder share — join the MLS group **off the chat rail**
    /// ([`join_folder_welcome`]: join + mark the folder channel for
    /// content-key / chunk reads, **no** chat thread). The recipient-accept twin
    /// of [`Self::ingest_scheduling_welcome`], driven by the pending-share
    /// `folder-share-accept-button` (via the FFI/wasm face).
    ///
    /// **Bypasses the contact gate on purpose** — the user has *explicitly*
    /// accepted this pending share, so no auto/knock/suppress decision applies
    /// (unlike [`Self::ingest_welcome_by_kind`], which gates a *pushed* folder
    /// welcome from the receive loop / drain). Idempotent — accepting an
    /// already-joined set is a no-op (the free fn short-circuits on the marked
    /// channel). `home_nest_url` is the share's home nest (blank same-nest, from
    /// the staged `WelcomeInbox.nest_url`); `welcome_ctx` carries the same
    /// envelope's home-nest-resolved set name (plaintext and/or sealed) and
    /// access grant (`WelcomeInbox.access` — **advisory-only**, so this client
    /// knows whether to offer a folder binding), recorded into the accept-time
    /// foreign-set record for a cross-nest share and ignored same-nest. Its
    /// `shared_by` is not read here (the gate is bypassed). Returns the joined
    /// channel id (hex).
    pub async fn join_folder_welcome(
        &self,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
        home_nest_url: String,
        welcome_ctx: FolderWelcomeContext,
    ) -> Result<String, BackendError> {
        let channel = join_folder_welcome(
            &self.fauna_mls,
            &channel_id_hex,
            &welcome_bytes,
            &home_nest_url,
            &welcome_ctx,
        )
        .await?;
        Ok(channel.to_string())
    }

    /// **Leave** a joined cross-user shared folder — the reverse of
    /// [`Self::join_folder_welcome`], driving the recipient's
    /// `folder-leave-button` (`docs/goal/ui/folders.md` § Sharing: "a
    /// `folder-leave-button` … to remove yourself"). Locally forgets the MLS group
    /// ([`leave_folder`]: `MlsEngine::forget_group` + unmark the folder channel),
    /// so the set drops from the recipient's `has_group`-filtered member-visible
    /// list. Addressed by the raw MLS `group_id` (hex) the member holds in their B3
    /// `FolderSummary` — the same value the nest-side `fauna.folders.leave`
    /// self-drop takes. Idempotent (forgetting an unknown / already-left group is a
    /// no-op). The FFI/wasm caller pairs this with `FoldersClient::leave` (the nest
    /// roster self-drop) — off the roster, the leaver stops receiving content-key
    /// rotations; a voluntary leave does not rotate the owner's content key. Returns
    /// the forgotten channel id (hex).
    /// The home-nest URL to route a leave/read for the set bound to
    /// `group_id_hex`, when it is a **foreign** (cross-nest) membership: the
    /// backend's RAM `channel_home` (recorded at this session's join) first,
    /// else the durable foreign-set record via the custody sink —
    /// so a relaunched client still resolves it. `None` ⇒ same-nest set (or no
    /// record): the plain local path. Callers thread it into
    /// `FoldersClient::leave_with_home` before calling
    /// [`Self::leave_folder`].
    pub async fn folder_home_url(&self, group_id_hex: &str) -> Option<String> {
        let raw = hex::decode(group_id_hex.trim())
            .ok()
            .filter(|b| !b.is_empty())?;
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&raw);
        if let Some(url) = self.fauna_mls.channel_home_url(&channel_id) {
            return (!url.is_empty()).then_some(url);
        }
        let sink = self.fauna_mls.folder_custody_sink()?;
        sink.foreign_home_url(&channel_id.0).await
    }

    pub async fn leave_folder(&self, group_id_hex: String) -> Result<String, BackendError> {
        let channel = leave_folder(&self.fauna_mls, &group_id_hex)?;
        // Cross-nest: drop the foreign-set record too, so a left
        // foreign set stops appearing in the member-visible list (the foreign
        // twin of the roster self-drop the caller pairs this with). Best-effort
        // and a no-op for a same-nest set (no record exists).
        if let Some(sink) = self.fauna_mls.folder_custody_sink() {
            sink.forget_foreign_set(&channel.0).await;
        }
        Ok(channel.to_string())
    }

    /// Ingest an MLS Welcome by its [`WelcomeChannelKind`] — the kind-dispatching
    /// twin of [`Self::ingest_welcome`] / [`Self::ingest_scheduling_welcome`]:
    /// a [`WelcomeChannelKind::Scheduling`] welcome joins the one-off group + marks
    /// the channel scheduling (no chat thread), a `Dm`/`Group` welcome joins + binds
    /// a thread. This is the **single dispatch** the receive loop's push arm and the
    /// durable-inbox drain backstop ([`InboxDrainSource`]) both route through, so the
    /// two never fork (the drain's [`crate::backend::InboxDrainSource`] glue impl
    /// maps the canonical `WelcomeInbox` envelope's `channel_type`/`group_id` onto a
    /// `WelcomeChannelKind` and calls this). Idempotent — re-applying a Welcome
    /// already ingested via the push arm is a no-op (the MLS engine + channel cursor
    /// dedup). `home_nest_url` is the cross-nest channel's home nest (blank
    /// same-nest), recorded so a later `channel.fetch` relays there. Pulling the
    /// pre-join history is the caller's job (the push arm polls inline; the drain
    /// rides the ticker's `poll_bound` / `poll_scheduling_feed` on the same tick).
    /// `welcome_ctx` carries the fields a [`WelcomeChannelKind::Folder`] welcome
    /// stamps — `shared_by` is the nest-stamped sharer id the recipient contact gate
    /// (registered via [`Self::register_folder_gate`]) reads to decide
    /// auto/knock/suppress; all four fields are `None` for every other kind (and an
    /// unstamped folder welcome, which then knocks — the safe default). Forwards
    /// 1:1 to the free [`ingest_welcome_by_kind`].
    pub async fn ingest_welcome_by_kind(
        &self,
        kind: WelcomeChannelKind,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
        home_nest_url: String,
        welcome_ctx: FolderWelcomeContext,
    ) -> Result<(), BackendError> {
        let gate = self.folder_gate.lock().unwrap().clone();
        ingest_welcome_by_kind(
            &self.fauna_mls,
            &self.manager,
            &kind,
            &channel_id_hex,
            &welcome_bytes,
            &home_nest_url,
            &welcome_ctx,
            &gate,
        )
        .await
    }

    /// Drain every scheduling channel the backend has joined — fetch → decode →
    /// MLS-decrypt → hand each iMIP to the registered [`SchedulingSink`] — advancing
    /// each channel's cursor. A no-op (`Ok(0)`) until
    /// [`Self::register_scheduling_sink`] wires a sink. The scheduling twin of
    /// [`Self::poll_conversations`]; [`Self::start_receive_loop`]'s ticker drives it
    /// automatically. Same partial-success contract: only `Err` when nothing was
    /// applied **and** a channel failed. The per-channel cursor is read + copied
    /// before the `.await` and written back after, so the lock is never held across
    /// it.
    pub async fn poll_scheduling(&self) -> Result<u32, BackendError> {
        let Some(sink) = self.scheduling_sink.lock().unwrap().clone() else {
            return Ok(0);
        };
        let mut total = 0u32;
        let mut first_err: Option<BackendError> = None;
        for channel in self.fauna_mls.scheduling_channels() {
            let mut cur = *self
                .scheduling_cursors
                .lock()
                .unwrap()
                .get(&channel)
                .unwrap_or(&0);
            match poll_inbound_scheduling(&self.fauna_mls, sink.as_ref(), &channel, &mut cur, 0)
                .await
            {
                Ok(n) => {
                    total += n as u32;
                    self.scheduling_cursors.lock().unwrap().insert(channel, cur);
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        match first_err {
            Some(e) if total == 0 => Err(e),
            _ => Ok(total),
        }
    }

    /// Drain every folder channel's membership commits — the remaining
    /// members' epoch-advance liveness for shared folders (5d(d)): applies
    /// owner-posted rotate-on-removal commits so this member's engine reaches
    /// the epoch the re-published content-key envelope is sealed under. The
    /// folder twin of [`Self::poll_scheduling`]; the channel set is derived
    /// from the engine ([`FaunaMlsBackend::folder_poll_channels`]), so it
    /// survives restarts. [`Self::start_receive_loop`]'s ticker drives it
    /// automatically; this is the manual backstop (and the seam tests drive).
    /// Returns the number of commits that advanced this device's epoch. Same
    /// partial-success contract as the other polls.
    pub async fn poll_folders(&self) -> Result<u32, BackendError> {
        let mut total = 0u32;
        let mut first_err: Option<BackendError> = None;
        for channel in self.fauna_mls.folder_poll_channels() {
            let mut cur = *self
                .folder_cursors
                .lock()
                .unwrap()
                .get(&channel)
                .unwrap_or(&0);
            let lock = self.fauna_mls.channel_lock(&channel);
            let guard = lock.lock().await;
            let polled = poll_inbound_folder(&self.fauna_mls, &channel, &mut cur, 0).await;
            drop(guard);
            match polled {
                Ok(outcome) => {
                    total += outcome.applied as u32;
                    // A stalled walk left `cur` before the unincorporated
                    // commit on purpose — persist that too, so the next pass
                    // retries the heal from the right place.
                    self.folder_cursors.lock().unwrap().insert(channel, cur);
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        match first_err {
            Some(e) if total == 0 => Err(e),
            _ => Ok(total),
        }
    }
}

/// Resolves once the [`ConversationsSession`] it was taken from has been
/// dropped — the signal every actor-scoped background task selects on, so a
/// torn-down session's handles are released *at the drop* rather than at the
/// task's next tick. Taken via [`ConversationsSession::closed`]; the session
/// holds the `watch` sender and never sends on it, so the only event a receiver
/// ever sees is the sender going away.
///
/// The consumers today: the session's own receive loop
/// ([`ConversationsSession::start_receive_loop`]), and app-side sweeps that ride
/// a session and hold its manager across a pass (tui's peer-anchor harvest).
/// The rule they share (`account-scoping.md` § Implementation status → the
/// `tui (in-memory)` ledger row): a loop serving an account ends on an
/// **event**, never a tick — the polled `Weak<()>` this replaced (2026-08-27)
/// let the receive loop keep strong `backend`/`manager` handles, and the MLS
/// engine's one-engine-per-store lock behind them, until the backstop's next
/// tick (30 s in production), so the post-succession sweep retry refused the
/// ceremony's own device and an A → B → A account switch found A's store held
/// by A's own zombie.
#[cfg(not(target_arch = "wasm32"))]
pub struct SessionClosed(tokio::sync::watch::Receiver<()>);

/// Flips [`ReceiveCycles::ended`] when the detached receive loop leaves.
///
/// Held by the loop's supervisor task and nothing else, so its drop *is* the
/// loop's exit — after [`supervise_receive_loop`] has recorded the reason, and
/// on every path, a cancelled task's included.
#[cfg(not(target_arch = "wasm32"))]
struct LoopEndGuard(Arc<ReceiveCycles>);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for LoopEndGuard {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Await the receive loop's task and record why it left — the one place a panic
/// inside a receive pass can be told apart from the loop's designed exits.
///
/// A panic unwinds out of the loop task's poll and tokio catches it there, so
/// nothing inside the task can observe it; its `JoinHandle` can, which is why
/// the loop runs as a task of its own under this one. On a panic the manager is
/// told ([`ConversationsManager::mark_receive_stopped`]), so every app's
/// `error-message` projection shows it, and the loop is deliberately not
/// re-armed — [`ReceiveLoopExit::Panicked`] states why.
///
/// The manager is held weakly: the supervisor must not extend the life of what
/// the loop served past the loop itself (`SessionClosed` states why promptness
/// is the contract).
#[cfg(not(target_arch = "wasm32"))]
async fn supervise_receive_loop(
    receive: tokio::task::JoinHandle<ReceiveLoopExit>,
    cycles: &ReceiveCycles,
    manager: &std::sync::Weak<ConversationsManager>,
    generation: u64,
) {
    match receive.await {
        Ok(exit) => cycles.record_exit(exit),
        Err(e) if e.is_panic() => {
            cycles.record_exit(ReceiveLoopExit::Panicked);
            let payload = e.into_panic();
            let why = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a non-text panic payload".to_string());
            tracing::error!(
                "the conversations receive loop panicked and has stopped — no message or mail \
                 arrives until the app restarts: {why}"
            );
            if let Some(manager) = manager.upgrade() {
                manager.mark_receive_stopped(generation);
            }
        }
        // Cancelled: the runtime is shutting down around the parked task. The
        // process is ending, so there is nobody left to tell.
        Err(_) => {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SessionClosed {
    /// Wait for the session to be dropped; returns at once if it already was.
    pub async fn wait(&mut self) {
        // `changed` is `Ok` only for a value the (never-sending) session sent,
        // and `Err` once the sender is gone — the latter is the event.
        while self.0.changed().await.is_ok() {}
    }

    /// Whether the session has already been dropped.
    pub fn is_closed(&self) -> bool {
        self.0.has_changed().is_err()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ConversationsSession {
    /// A [`SessionClosed`] for this session — take one per background task
    /// that rides the session, and `select!` on it beside the task's own wake.
    pub fn closed(&self) -> SessionClosed {
        SessionClosed(self.closed.subscribe())
    }
}

// Native-only: the detached push-driven receive loop spawns a tokio task and
// uses `tokio::time` — neither exists on the wasm build (the web app drives
// its own `future_to_promise` poll loop), so the whole loop machinery is
// `cfg(not(wasm32))`.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_CONV_POLL_SECS: u64 = 30;

/// The first step of every conversation sweep: ask the injected
/// [`SiblingGroupAdopter`] whether another of this account's devices joined a
/// group since the last look, and bind what it imported — BEFORE the sweep
/// walks `bound_channels()`, so a channel bound here is polled in this same
/// sweep rather than the next. A fault is warn-logged and the sweep proceeds
/// (the next trigger asks again); with no seam injected this is nothing.
/// Shared by [`ConversationsSession::poll_conversations`] and the receive
/// loop's `poll_bound`; the wasm `pollConversations` twin calls the same seam.
pub async fn adopt_sibling_groups_first(backend: &FaunaMlsBackend) {
    let Some(adopter) = backend.sibling_group_adopter() else {
        return;
    };
    match adopter.adopt_if_changed().await {
        Ok(0) => {}
        Ok(bound) => tracing::info!(
            bound,
            "mls-sync: bound {bound} channel(s) another of this account's devices joined"
        ),
        Err(e) => tracing::warn!("mls-sync: sibling-group adoption pass failed: {e}"),
    }
}

/// How the receive loop's push arm **reports** a Welcome ingest `Err` — the
/// level, and the line.
///
/// A door of its own rather than three inline branches, because the level is
/// the whole behaviour: the arm ingests through
/// [`ingest_welcome_by_kind`](crate::session::ingest_welcome_by_kind) and does
/// nothing else with the failure (it never acks, never retries), so this is the
/// only observable that distinguishes an expected outcome from a fault, and an
/// inline branch inside the loop is one no test can reach.
///
/// Two of the three arms are expected outcomes, each classified by its
/// **producer** rather than by guesswork here:
///
/// - A *knocked folder* Welcome returns `Err` by design — a pending-share
///   signal, not a fault. The arm doesn't ack, so the durable drain retains it
///   as the pending share; `debug`.
/// - A Welcome addressed to **no key package this device holds**
///   ([`BackendError::WelcomeNotAddressedHere`]) cannot be joined here and
///   re-delivery will not change that: the multi-device steady state on any
///   device launched before a sibling minted the addressed package
///   (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync →
///   *Who may consume a Welcome — any device that holds its key*). Its row
///   stays un-acked exactly as before and the targeted sibling-group import
///   delivers the group once the minting sibling's flush lands. On a two-device
///   account where one app stays open while the other replenishes the pool this
///   is *every* Welcome minted since that app launched, so it must sit below
///   `warn` — off the line a genuine fault produces, and out of the ring
///   `fauna-log` shows the user; `info`.
///
/// Everything else is a genuine ingest failure: `error`, the original line.
pub fn report_welcome_ingest_failure(kind: &WelcomeChannelKind, e: &BackendError) {
    if matches!(kind, WelcomeChannelKind::Folder { .. }) {
        tracing::debug!("folder welcome staged/retained: {e}")
    } else if matches!(e, BackendError::WelcomeNotAddressedHere) {
        tracing::info!(
            "welcome ({kind:?}) addressed to a key package this device does not hold — a sibling joins it; the targeted import binds it here"
        )
    } else {
        tracing::error!("ingest welcome ({kind:?}): {e}")
    }
}

/// One-time key packages to keep published on the nest so peers can fetch one
/// to add this actor to a group (`docs/goal/architecture/federation.md` § Key
/// packages). The target EVERY native replenish path tops the pool up to
/// through [`ConversationsManager::ensure_keypackages`](crate::ConversationsManager::ensure_keypackages):
/// the session-owned login replenish here, plus the settings manual-refresh
/// surfaces (linux `conv_backend::replenish_key_packages`, android
/// `host.manager.ensureKeypackages`) — all route through the durable
/// notify→autosave surface, never a raw engine mint whose init keys a later
/// provider swap would wipe (`docs/goal/behavior/devices.md` § Cross-device MLS
/// group-state sync). The web SPA mirrors the value in `conversations.ts`.
#[cfg(not(target_arch = "wasm32"))]
pub const KEYPACKAGE_TARGET: u64 = 20;

#[cfg(not(target_arch = "wasm32"))]
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl ConversationsSession {
    /// Start the detached push-driven receive loop and return immediately. The
    /// native twin of the linux `conv_backend.rs` `tokio::select!` loop: it
    /// subscribes the injected [`ConversationsPush`] (`welcome.received` +
    /// `channel.message`) and runs a backstop ticker, driving `ingest_welcome` +
    /// the inbound channel poll into the wired [`ConversationsManager`]
    /// (`docs/goal/ui/conversations.md` § Receiving into the conversations view,
    /// § MLS Welcome at-rest). All MLS crypto stays in [`FaunaMlsBackend`] (§
    /// Architectural rules #2); this only routes pushes to it.
    ///
    /// Spawned on the tokio runtime the `async_runtime = "tokio"` export drives,
    /// so the FFI caller (`App.xaml.cs` at login) gets a fire-and-forget start.
    /// With no push source injected the loop runs ticker-only (the
    /// reconnect/missed-push backstop). The task holds its own per-channel cursor
    /// map — independent of [`Self::poll_conversations`]'s, since both only ever
    /// dedup against the monotonic channel log.
    pub async fn start_receive_loop(&self) {
        let backend = Arc::clone(&self.fauna_mls);
        let manager = Arc::clone(&self.manager);
        let push = self.push.clone();
        // Mail read-feeds (if `register_mail_receive` wired them) ride this same
        // loop: the backstop ticker drives them, and a `ConvPushEvent::MailReceived`
        // wake (`fauna.mail.received`, carried over the same push seam since the loop
        // drives both rails) re-polls them promptly — the read-feed model
        // (`docs/goal/ui/conversations.md` § Receiving into the conversations view;
        // `docs/goal/behavior/smtp-server.md` § Inbound client receive → Arrival push).
        let mail_inbox = self.mail_inbox.lock().unwrap().clone();
        let mail_sent = self.mail_sent.lock().unwrap().clone();
        // The bridged rail (if `register_bridged` wired it) rides this loop
        // too: the ticker backstops it and a `ConvPushEvent::BridgedChanged`
        // wake re-reads it promptly, under the session's one row cursor.
        let bridged = self.bridged.lock().unwrap().clone();
        let bridged_cursor = Arc::clone(&self.bridged_cursor);
        // The calendar-apply sink (if `register_scheduling_sink` wired one) — the
        // mailbox-less CalDAV iMIP rail rides this same loop: a scheduling welcome
        // marks its channel (no chat thread), and the backstop ticker + a
        // `ChannelMessage` wake drain it to the sink (caldav-server.md § Server-side
        // auto-schedule, Half-1).
        let scheduling_sink = self.scheduling_sink.lock().unwrap().clone();
        // The durable inbox-apply backstop (if `register_inbox_drain` wired one) —
        // the missed-push recovery rail (`docs/goal/architecture/api-layers.md` §
        // Inbox & Messaging, layer 3). The backstop ticker drains it: a Welcome
        // whose best-effort `ConvPushEvent::Welcome` was missed (client offline at
        // push time) is recovered from the durable `fauna.inbox.*` queue. `None`
        // runs push-only (the pre-layer-3 behaviour).
        let inbox_drain = self.inbox_drain.lock().unwrap().clone();
        // The recipient contact gate (if `register_folder_gate` wired one) — a
        // cross-user shared folder Welcome (`WelcomeChannelKind::Folder`) routes
        // through it (auto-join / knock-stage / suppress) on both the push arm and
        // the drain. `None` leaves every folder Welcome un-acked (retained), the
        // pre-gate behaviour.
        let folder_gate = self.folder_gate.lock().unwrap().clone();
        // The backstop cadence — overridable via `FAUNA_CONV_POLL_SECS` for the
        // e2e (faster than the production interval); not user-facing, and the read
        // is compiled out of a release build with no `test-helpers` feature
        // (`poll_secs_override`, convention 15). Governs both the conv channel
        // sweep and the mail read-feeds in this unified loop.
        let poll_secs = resolve_poll_secs(poll_secs_override());
        // The cycle counters + the run-one-now poke this loop selects on
        // (`ReceiveCycles` / [`Self::poke_receive_cycle`] own the contract).
        let receive_cycles = Arc::clone(&self.receive_cycles);
        let receive_poke = Arc::clone(&self.receive_poke);
        // A render that misses an evicted attachment asks for the next cycle
        // now rather than at the ticker (`ConversationsManager::attachment_bytes`).
        self.manager.set_attachment_refill_poke(Arc::new({
            let poke = Arc::clone(&self.receive_poke);
            move || poke.notify_one()
        }));
        // A read owes the nest `\Seen` writes; this loop sends them at the poke.
        let mail_read_poke = Arc::new(tokio::sync::Notify::new());
        self.manager.set_mail_read_poke(Arc::new({
            let poke = Arc::clone(&mail_read_poke);
            move || poke.notify_one()
        }));

        // The session-closed signal ([`SessionClosed`]): when the caller drops the
        // session the loop exits — at the drop, as a `select!` arm, never at its
        // next tick. Native apps build one session per app launch, but the linux
        // e2e session-cached driver re-injects the authenticated session repeatedly
        // within one process — each re-injection rebuilds the session at
        // AuthSuccess. Linux keeps the live session in a process static and replaces
        // it on rebuild, so the previous loop's signal fires and it stops instead of
        // accumulating (each leaked loop would otherwise re-ingest the same inbound
        // every tick). Scoped per session — not a process-global generation — so
        // concurrent sessions (and parallel tests) don't supersede each other.
        //
        // ⚠ Promptness is the contract, not a nicety: this task holds `backend` and
        // `manager` strongly, and through them the MLS engine and its
        // one-engine-per-store lock on `mls_state.db`. The polled `Weak<()>` this
        // replaced (2026-08-27) held all of that until the backstop's next tick —
        // 30 s in production — during which the post-succession sweep retry
        // refused the ceremony's own device and an A → B → A account switch found
        // A's store held by A's own zombie (`account-scoping.md` § Implementation
        // status → the `tui (in-memory)` ledger row).
        let mut closed = self.closed();

        // The hand-over signal ([`crate::backends::fauna_mls::EngineRetired`]),
        // the second of this loop's two exits. `closed` above covers "my holder
        // let go of me"; this covers "my engine is no longer the one serving this
        // account", and on the path that matters the two are NOT the same event.
        //
        // The factory retires the predecessor's rail BEFORE building the
        // successor engine (`account-data-plane.md` § Multi-instance concurrency
        // → *a session factory RELEASES BEFORE IT BUILDS*), so when that build
        // then throws, no shell ever installs a successor and every shell that
        // reaches this seam keeps holding the predecessor: apple's
        // `ConversationsVM.activate` and windows' `_liveConvSession` both assign
        // only on the success path, inside a best-effort `try` whose catch arm
        // logs. `closed` therefore never fires, and before this arm the loop ran
        // for the life of the process — one leaked task per failed build, each
        // still holding `backend` and `manager` strongly. Making the release a
        // shell obligation was considered and rejected where the hand-over itself
        // was: the predecessor is held in three languages at once, so it has to be
        // independent of every remaining reference.
        //
        // Pinned by `receive_cycle_poke_tests.rs`
        // `retiring_the_engine_ends_the_loop_a_failed_build_left_installed`.
        let mut retired = backend.retired_watch();

        // Cross-device MLS state-sync launch (design §5 restore-before-first-poll):
        // if the native FFI factory injected a launcher, run it now — it restores
        // the replica, injects the device-owned-epoch gate + cursor, and attaches
        // the debounced autosave — awaited to completion *before* the poll loop
        // spawns, so every restored channel resumes from its watermark (not seq 0)
        // and a save can never clobber the real replica before restore lands. Unset
        // for linux (its glib path wires the plane) and web (this loop is
        // wasm-excluded), so those legs no-op here. The `await` runs inside the
        // native VM's fire-and-forget `Task`/coroutine, so it never blocks the UI.
        // Peer-anchor harvest sweep (`identity-succession.md` § The succession
        // statement → *the peer-profile harvest*) — the producer half of the
        // member-path anchor, started BEFORE the awaited restore just below.
        // Deliberately first: `launch` returns as soon as the sweep is running
        // (it owns its own cadence and ends with the session), and the race it
        // has to win is against a *ceremony's* statement, not against this loop
        // — so every second spent behind a replica restore is a member who may
        // never anchor (the measured 2026-08-10 ordering race, same §). Unset
        // for web, whose bootstrap drives its own and for whom this loop is
        // wasm-excluded anyway.
        let peer_anchor_sweep = self.peer_anchor_sweep_launcher.lock().unwrap().clone();
        if let Some(launcher) = peer_anchor_sweep {
            launcher.launch().await;
            // A sweep now runs, so the witness may hold a held-head verdict
            // until the sweep has settled that peer (same §, *the harvest
            // wait*). Armed HERE — synchronously, before the poll task below
            // can decode a statement — and only on this branch: a session
            // with no sweep has nothing that would ever end the wait.
            backend.arm_succession_harvest_wait().await;
        }

        let mls_sync_launcher = self.mls_sync_launcher.lock().unwrap().clone();
        if let Some(launcher) = mls_sync_launcher {
            launcher.launch().await;
        }

        // Content-index builder launch (`content-index.md` § Ingest triggers, v1)
        // — resume this actor's index off the `__index` rail and register the
        // observer, **awaited here, before the poll task spawns below**. That
        // ordering is the whole point and it is structural, not a race: the
        // client keeps no restart-durable mail cursor, so the poll loop re-pages
        // the entire mailbox from UID 0 every launch and the index seam fires for
        // all of it. An observer registered after the spawn would miss part or
        // all of that walk, and this launch's mail would simply not be
        // searchable. Resuming first also seeds the re-index guard, which is what
        // keeps the re-walk from re-publishing an index the actor already has.
        //
        // `None` is the normal not-ready answer (mail not enabled — no MSEK to
        // derive from — or a transient rail read failure). It is self-logged by
        // the launcher and never blocks the loop: nothing is lost, because the
        // next launch walks the same mailbox again.
        //
        // The handle is kept (not just handed to the manager) because the loop
        // below owes it one more thing: the **catch-up boundary**. The first
        // mail sweep of a session is the launch backlog and everything after it
        // is the trickle, and a builder under the advisory `index` lease gates
        // those two differently (`MessageIndexObserver::observe_catch_up_complete`
        // — `content-index.md` § Where the index is built). The boundary is a
        // structural fact of this loop; the signal is the only new machinery.
        let index_launcher = self.index_launcher.lock().unwrap().clone();
        let index_observer = match &index_launcher {
            Some(launcher) => match launcher.launch().await {
                Some(observer) => {
                    self.manager.set_index_observer(Arc::clone(&observer));
                    // Conversation catch-up **leg 2** — the attach-time walk
                    // (`content-index.md` § Ingest triggers, v1 → *The
                    // Conversation kind's catch-up*). Here rather than anywhere
                    // later because this is the first instant an observer
                    // exists, and the restore that filled the store with
                    // cross-device history ran two statements above without one
                    // (`ConversationsManager::walk_conversations_for_index`
                    // carries the full reasoning). Synchronous and cheap: a
                    // resumed builder drops an already-indexed message at the
                    // guard, and staging never does I/O. The gate it meets is
                    // decided: `launch()` returns only after the advisory
                    // lease's first answer (or its ceiling), since this walk is
                    // offered once per launch (`content-index.md` § Where the
                    // index is built, the launch-walk sub-bullet).
                    self.manager.walk_conversations_for_index();
                    Some(observer)
                }
                None => None,
            },
            None => None,
        };

        // Login-time key-package replenish: the one-time pool + the mandatory
        // last-resort KP (`federation.md` § Key packages), so peers can add us
        // after the pool drains. Session-owned and sequenced AFTER the launcher's
        // replica restore: a restore swaps the engine's provider storage, so a
        // package minted before it would lose its private init key — a peer who
        // fetched that package minted an unjoinable group (`devices.md` § Cross-
        // device MLS group-state sync; pinned by fauna-mls
        // `key_package_minted_before_provider_swap_loses_its_init_key`). Linux
        // wires its restore before calling this, so the ordering holds there
        // too; the web SPA (which never runs this loop) sequences its own
        // replenish after `restoreMlsState`. Best-effort + idempotent (a
        // transient failure retries next launch); the manager notifies its
        // observers on an actual mint, so the debounced replica autosave
        // persists the fresh init keys.
        if let Err(e) = self.manager.ensure_keypackages(KEYPACKAGE_TARGET).await {
            tracing::error!("ensure_keypackages: {e}");
        }
        if let Err(e) = self.manager.ensure_last_resort_keypackage().await {
            tracing::error!("ensure_last_resort_keypackage: {e}");
        }

        // The loop runs as a task of its own under a supervisor task, because a
        // panic inside a pass unwinds out of the task's poll where nothing inside
        // it can see — only its `JoinHandle` can ([`supervise_receive_loop`]).
        // The generation is claimed before the spawn, so the manager's dead-rail
        // report can only ever name this loop or a later one.
        let supervised_cycles = Arc::clone(&receive_cycles);
        let stopped_manager = Arc::downgrade(&self.manager);
        let generation = self.manager.begin_receive_loop();
        let receive = tokio::spawn(async move {
            let mut cursors: HashMap<ChannelId, i64> = HashMap::new();
            // Independent per-mailbox `after_uid` cursors for this loop (the manual
            // `poll_mail` keeps its own); monotonic per mailbox so a fresh dedup
            // set per poll suffices.
            let mut mail_inbox_uid: u32 = 0;
            let mut mail_sent_uid: u32 = 0;
            // Independent per-scheduling-channel cursor map for this loop (the
            // manual `poll_scheduling` keeps its own); monotonic per channel.
            let mut sched_cursors: HashMap<ChannelId, i64> = HashMap::new();
            // Independent per-folder-channel cursor map (the manual
            // `poll_folders` keeps its own). Starts at 0 each launch — safe:
            // the folder poll applies only commits, and re-applying a
            // past-epoch commit is a quiet skip.
            let mut fs_cursors: HashMap<ChannelId, i64> = HashMap::new();
            // `false` disables the push arm (no source, or it closed) — the ticker
            // then drives delivery alone, mirroring linux's `Option`-gated arm.
            let mut push_live = push.is_some();
            // Open until the first mail sweep of this session completes cleanly,
            // then closed forever (see `mail_sweep!` below).
            let mut catch_up_open = index_observer.is_some();
            // The Conversation kind's own boundary, tracked separately because
            // it closes on a different event: the first clean *conversation*
            // refold, not the first clean mail sweep (see `conv_sweep!`).
            let mut conv_catch_up_open = index_observer.is_some();
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(poll_secs));

            // The full sweep every rail needs when we may have missed arrivals:
            // durable inbox-apply backstop FIRST (recover any missed-push Welcome
            // from the durable queue, binding its channel), then each rail's
            // cursor poll — so the same sweep pulls that channel's pre-join
            // history (`api-layers.md` § Inbox & Messaging, layer 3). A per-item
            // apply failure is absorbed by the drain as a skip; a transport
            // failure is logged + retried next sweep (never fatal).
            //
            // A macro, not a closure: each arm needs its own `&mut` borrows of the
            // cursor maps, which one `FnMut` capturing them all could not hand out
            // twice. Both the ticker and the `Reconnected` wake expand it, so a
            // rail added to one can never silently skip the other — the drift this
            // exists to prevent (conversations shipped for months polling on the
            // ticker alone while every other surface re-pulled on reconnect).
            //
            // One mail poll, plus the **catch-up boundary** the content-index
            // builder needs. Both mail-polling sites in this loop expand it, for
            // the same anti-drift reason `full_sweep!` exists: the boundary is
            // "the whole mailbox has been walked from UID 0", and since these
            // cursors start at 0 that is whichever poll runs first — the ticker's
            // immediate first tick, a `Reconnected` wake, or a `MailReceived`
            // push that beat them. Signalling from only one site would leave the
            // gate open through a whole session whenever another won the race.
            //
            // **Only a clean sweep closes it.** `poll_mail_feeds` answers `false`
            // if any configured feed errored, and an unfinished walk must keep
            // counting as backlog: its cursor did not advance, so the next sweep
            // re-pages the same mail and gets another chance. Closing on a failed
            // sweep would reclassify the *whole remaining backlog* as trickle and
            // hand a stood-down seat exactly the N× full-corpus republish the
            // lease exists to prevent.
            // Attach an arm whose precondition arrived after launch, **before**
            // the poll that would carry its content past the seam.
            //
            // `launch()` is one-shot, in this loop's prologue, and an arm can be
            // un-buildable then and buildable a minute later — mail enabled
            // after login is the case a live user hits (no `mail.msek` at
            // launch, so no mail arm for the whole process life: the mail
            // arrives, renders, and is silently never staged for search until a
            // restart). A transient rail failure strands an arm identically.
            // The receive path already tolerates this by re-deriving lazily on
            // every poll (`MailKeyCache::get`); this is the indexing twin of
            // that same re-check, on the same cadence.
            //
            // **Before the poll, never after**: within a session the mail
            // cursors advance, so an arm attached after the fetch misses that
            // sweep's mail entirely, and nothing re-presents it until the next
            // launch — the very gap this closes, moved by one statement.
            //
            // A fresh arm gets a **fresh catch-up window**. The boundary this
            // loop already closed says nothing about an arm that did not exist
            // then, and with mail disabled a sweep is a graceful no-op that
            // answers *complete* — so the mail window normally closes on sweep 1,
            // before mail is even enabled. Inheriting that closure would
            // reclassify the whole mailbox re-walk as trickle, which the lease
            // never gates (the N× republish the boundary exists to prevent);
            // leaving it shut would make a stood-down seat withhold that arm's
            // backlog for the rest of the session. Reopening is per kind, for
            // the reason the boundary itself is.
            macro_rules! ensure_index_arm {
                ($kind:expr) => {{
                    if let Some(launcher) = &index_launcher
                        && launcher.ensure_arm($kind).await
                    {
                        match $kind {
                            IndexableKind::Mail => {
                                catch_up_open = true;
                                // Re-page the mailbox from the start, so the
                                // reopened window is honest about what the new
                                // arm is owed. Free in the enablement case (no
                                // fetch could advance a cursor while mail was
                                // off); in the transient-failure case it is the
                                // one re-page that gets this session's earlier
                                // mail to the arm. Re-ingest is a no-op — the
                                // manager dedups by message id, and the builder
                                // by `(kind, content_id)`.
                                mail_inbox_uid = 0;
                                mail_sent_uid = 0;
                            }
                            IndexableKind::Conversation => {
                                conv_catch_up_open = true;
                                // Leg 2 — the attach-time walk is what carries
                                // this kind's corpus (the thread store is
                                // in-memory and re-presents only at launch), so
                                // an arm attaching mid-session needs it exactly
                                // as the launch-time one did.
                                manager.walk_conversations_for_index();
                            }
                        }
                    }
                }};
            }

            // One read of the bridged rooms and inbox. A failure is logged and
            // retried at the next wake, like every other rail's.
            macro_rules! bridged_sweep {
                () => {{
                    if let Err(e) =
                        poll_bridged_feed(bridged.as_ref(), &manager, &bridged_cursor).await
                    {
                        tracing::error!("poll bridged inbox: {e}");
                    }
                }};
            }

            macro_rules! mail_sweep {
                () => {{
                    ensure_index_arm!(IndexableKind::Mail);
                    let swept = poll_mail_feeds(
                        &mail_inbox,
                        &mail_sent,
                        &manager,
                        &mut mail_inbox_uid,
                        &mut mail_sent_uid,
                    )
                    .await;
                    // Mail attachments the budget evicted and a render has
                    // since asked for — re-read from their records, after the
                    // walk so a just-arrived record is never read twice
                    // (`conversations.md` § Attachments → *Retention*). Here,
                    // not in `conv_sweep!`: it needs the mail sources, and a
                    // mail push wakes only this sweep.
                    crate::backends::smtp::refill_evicted_mail_attachments(
                        mail_inbox.as_deref(),
                        mail_sent.as_deref(),
                        &manager,
                    )
                    .await;
                    // The mail read-state backstop: owed `\Seen` writes out,
                    // flag changes made elsewhere in — after the poll, whose
                    // first `INBOX` page names the change baseline.
                    if let Some(src) = mail_inbox.as_deref() {
                        crate::backends::smtp::sync_mail_read_state(src, &manager).await;
                    }
                    if swept && catch_up_open {
                        catch_up_open = false;
                        if let Some(observer) = &index_observer {
                            observer.observe_catch_up_complete(IndexableKind::Mail);
                        }
                    }
                }};
            }

            // One conversation refold, plus the **Conversation catch-up
            // boundary** — the twin of `mail_sweep!` above, and a macro for the
            // same anti-drift reason: every site that folds bound channels must
            // be able to close the boundary, or a session whose first fold came
            // from a push nudge rather than the ticker would leave the gate open
            // for its whole life.
            //
            // **What the boundary means for this kind.** The ruling
            // (`content-index.md` § Ingest triggers, v1 → *The Conversation
            // kind's catch-up*) closes it on a **restore + initial refold that
            // completed without error**. The refold half is checked here. The
            // restore half is already discharged *structurally* by the time this
            // loop exists: `start_receive_loop`'s prologue awaits
            // `mls_sync_launcher.launch()` — which awaits the replica restore to
            // its end, retrying a transient failure with backoff — before the
            // index launch two statements below, and linux awaits its own
            // `wire_mls_state_sync` before calling this function at all. So
            // every restored message is in the store before the observer that
            // would classify it exists, which is exactly why leg 2 walks the
            // store at attach. `index_launch_ordering_tests.rs` pins that
            // ordering; if it ever changes, the restore's completion has to
            // become a condition here rather than a precondition, and leg 1
            // (`ConversationsManager::restore_channel_slice`) is what keeps the
            // corpus correct in the meantime.
            //
            // **Only a clean refold closes it**, for the reason the mail twin
            // documents: an errored or stalled channel leaves its cursor
            // unadvanced, so the next sweep re-pages the same backlog, and
            // closing early would reclassify the remainder as trickle — which
            // the lease never gates.
            macro_rules! conv_sweep {
                () => {{
                    ensure_index_arm!(IndexableKind::Conversation);
                    let mut fold = poll_bound(&backend, &manager, &mut cursors).await;
                    // Attachments the budget evicted and a render has since
                    // asked for — fetched again from where they rest, after
                    // the walk so a just-arrived message's own fetch is never
                    // repeated (`conversations.md` § Attachments → *Retention*).
                    crate::backends::fauna_mls::refill_evicted_attachments(&backend, &manager)
                        .await;
                    // A room whose key-in just landed: everything behind its
                    // wait is backlog, and the boundary has usually closed
                    // without it (an unkeyed room folds for now). So reopen the
                    // window BEFORE the re-walk that carries it — the late
                    // arm's rule, arriving from the other direction
                    // (`content-index-ingest.md` § Ingest triggers, v1 → *A
                    // community room waiting for its key-in*). Reopening a
                    // window that is still open is only this flag's own state,
                    // so the signal goes out solely on the closed→open edge.
                    if !fold.keyed_in.is_empty() {
                        if let Some(observer) = &index_observer
                            && !conv_catch_up_open
                        {
                            observer.observe_catch_up_reopened(IndexableKind::Conversation);
                        }
                        conv_catch_up_open = index_observer.is_some();
                        let again =
                            refold_channels(&backend, &manager, &mut cursors, &fold.keyed_in).await;
                        fold.complete &= again.complete;
                    }
                    let folded = fold.complete;
                    if folded && conv_catch_up_open {
                        conv_catch_up_open = false;
                        if let Some(observer) = &index_observer {
                            observer.observe_catch_up_complete(IndexableKind::Conversation);
                        }
                    }
                }};
            }

            macro_rules! full_sweep {
                () => {{
                    // The cycle counters bracket the whole sweep, and this is
                    // their ONLY site — the same single-site argument the macro
                    // itself rests on: every trigger (ticker, reconnect, poke)
                    // expands this, so no arm can acquire a half-counted cycle,
                    // and the push arms' single-rail sweeps stay deliberately
                    // uncounted (`ReceiveCycles` states why).
                    receive_cycles.begin();
                    if let Some(drain) = &inbox_drain
                        && let Err(e) = drain.drain_once().await
                    {
                        tracing::error!("inbox drain: {e}");
                    }
                    // The standing room invitations — an inbox kind the drain
                    // above deliberately leaves un-acked, because accepting is
                    // the user's decision (`conversation-rooms.md` § Join rules
                    // and invites). A peek, so re-listing every sweep is safe.
                    manager.refresh_room_invitations().await;
                    conv_sweep!();
                    mail_sweep!();
                    bridged_sweep!();
                    poll_scheduling_feed(&backend, &scheduling_sink, &mut sched_cursors).await;
                    poll_folder_feed(&backend, &mut fs_cursors).await;
                    receive_cycles.finish();
                }};
            }

            loop {
                // Exit once the caller drops the session (e.g. linux replacing it on
                // a re-injection) — keeps one live loop per session instead of one per
                // rebuild. Loop-top, so an in-flight delivery in the arm below always
                // completes first; and an arm of the `select!`, so a loop parked
                // between deliveries exits at the drop rather than at its next wake.
                if closed.is_closed() {
                    return ReceiveLoopExit::SessionClosed;
                }
                if retired.is_retired() {
                    return ReceiveLoopExit::EngineRetired;
                }
                tokio::select! {
                    _ = closed.wait() => {
                        return ReceiveLoopExit::SessionClosed;
                    }
                    // The hand-over (see `retired`'s binding above): exit at the
                    // factory's retire, not at the drop a failed successor build
                    // never performs.
                    _ = retired.wait() => {
                        return ReceiveLoopExit::EngineRetired;
                    }
                    _ = ticker.tick() => {
                        // The missed-push / disconnected-window backstop.
                        full_sweep!();
                    }
                    // The run-one-now poke ([`Self::poke_receive_cycle`]) — an
                    // arm of this same `select!` rather than a parallel task, so
                    // a poked sweep can no more overlap a ticked one than the
                    // ticker can overlap a push. It runs the identical
                    // `full_sweep!`: the poke changes *when* a cycle happens,
                    // never *what* it does, which is what lets a test drive the
                    // real delivery path instead of a shortened tick.
                    _ = receive_poke.notified() => {
                        full_sweep!();
                    }
                    // A thread read owes the nest `\Seen` writes
                    // (`ConversationsManager::read_thread`): send them now, not
                    // at the next sweep, so another device hears of the read
                    // promptly. Only the write — nothing else waits on a read.
                    _ = mail_read_poke.notified() => {
                        if let Some(src) = mail_inbox.as_deref() {
                            crate::backends::smtp::flush_owed_mail_seen(src, &manager).await;
                        }
                    }
                    ev = async { push.as_ref().unwrap().next_event().await }, if push_live => {
                        match ev {
                            // Welcomes carry the channel id — same-nest directly,
                            // cross-nest via the federation relay (which wraps
                            // `channel_id` alongside `nest_url`/`channel_type`), so a
                            // remote welcome drains the same as a local one; `None`
                            // only from a non-conforming peer that omits it, then skipped. A
                            // scheduling welcome (the mailbox-less CalDAV iMIP rail)
                            // marks its channel + drains to the calendar-apply sink,
                            // never the chat UI; a DM/group welcome binds a thread,
                            // then pulls any history posted before we joined.
                            Some(ConvPushEvent::Welcome(w)) => {
                                if let Some(channel_hex) = w.channel_id_hex {
                                    // Ingest via the SAME dispatch the drain backstop
                                    // uses (`ingest_welcome_by_kind`) — no fork. Then
                                    // pull pre-join history: a scheduling welcome drains
                                    // to the calendar-apply sink (never the chat UI), a
                                    // DM/group welcome polls its now-bound thread.
                                    let welcome_ctx = FolderWelcomeContext {
                                        shared_by: w.shared_by,
                                        set_name: w.set_name,
                                        access: w.access,
                                        home_nest_actor_id: w.home_nest_actor_id,
                                        shared_by_handle: w.shared_by_handle,
                                        shared_by_domain: w.shared_by_domain,
                                        set_name_seal: w.set_name_seal,
                                    };
                                    match ingest_welcome_by_kind(
                                        &backend, &manager, &w.kind, &channel_hex, &w.welcome_bytes,
                                        w.home_nest_url.as_deref().unwrap_or(""),
                                        &welcome_ctx,
                                        &folder_gate,
                                    )
                                    .await
                                    {
                                        Ok(()) => match w.kind {
                                            WelcomeChannelKind::Scheduling => {
                                                poll_scheduling_feed(
                                                    &backend, &scheduling_sink, &mut sched_cursors,
                                                )
                                                .await;
                                            }
                                            WelcomeChannelKind::Dm
                                            | WelcomeChannelKind::Group { .. } => {
                                                conv_sweep!();
                                            }
                                            // An auto-gated folder Welcome joined the group
                                            // off the chat rail (or a Blocked one was dropped);
                                            // either way there is nothing to poll here — a
                                            // shared set is synced by the engine, not the
                                            // conversation rail. (A knocked folder Welcome
                                            // takes the `Err` arm below and stays un-acked.)
                                            WelcomeChannelKind::Folder { .. } => {}
                                        },
                                        Err(e) => report_welcome_ingest_failure(&w.kind, &e),
                                    }
                                }
                            }
                            // New ciphertext somewhere — re-poll every bound chat
                            // channel AND every scheduling channel AND every
                            // folder channel (each cursor dedups; coalescing is
                            // fine). A folder owner's Remove-commit send push-
                            // nudges the roster, so this is what makes a remaining
                            // member's epoch advance prompt, not just within one
                            // backstop tick.
                            Some(ConvPushEvent::ChannelMessage) => {
                                conv_sweep!();
                                poll_scheduling_feed(&backend, &scheduling_sink, &mut sched_cursors)
                                    .await;
                                poll_folder_feed(&backend, &mut fs_cursors).await;
                            }
                            // Mail arrived (`fauna.mail.received`) — re-poll the mail
                            // read-feeds promptly (the per-mailbox cursor dedups). The
                            // loop drives both rails, so the mail wake rides the same
                            // push seam; this is what makes inbound mail prompt on the
                            // native apps, not just within one backstop-ticker cycle.
                            Some(ConvPushEvent::MailReceived) => {
                                mail_sweep!();
                            }
                            // A bridged room changed — a deposit, a room
                            // report or a receipt. Re-read the rooms and the
                            // inbox now; the row cursor dedups.
                            Some(ConvPushEvent::BridgedChanged) => {
                                bridged_sweep!();
                            }
                            // A flag on an `INBOX` message changed — another
                            // Fauna device read it, or a mail client marked it
                            // (un)read. One cursored drain, no mailbox re-poll
                            // (`mail-app-surface.md` § Read state).
                            Some(ConvPushEvent::MailFlagsChanged) => {
                                if let Some(src) = mail_inbox.as_deref() {
                                    crate::backends::smtp::sync_mail_read_state(src, &manager)
                                        .await;
                                }
                            }
                            // The socket came back. Pushes are transient, so whatever
                            // the nest tried to deliver while we were down was never
                            // broadcast to us — only a pull recovers it. Sweep every
                            // rail exactly as a tick does, so MLS delivery resumes
                            // with the reconnect instead of up to one 30 s backstop
                            // tick later (`transport.md` § Reconnect & resync — the
                            // signal every other live surface already re-pulls on).
                            Some(ConvPushEvent::Reconnected) => {
                                full_sweep!();
                            }
                            // An address book moved on the nest — reconcile it
                            // now rather than at the next master-kind sweep, so a
                            // card a MUA writes while the app is open becomes
                            // searchable without a relaunch (`content-index.md`
                            // § Ingest triggers, v1 → the contacts ruling: the
                            // push is freshness, the walk is correctness).
                            //
                            // Routed straight to the launcher rather than through
                            // a rail sweep because no rail owns this corpus: it
                            // is nest-resident and externally mutated, so the
                            // walk *is* the reconcile. Idempotent and
                            // ctag-suppressed, so a spurious or duplicated event
                            // costs one `list_addressbooks` and no publish.
                            Some(ConvPushEvent::AddressBookChanged) => {
                                if let Some(launcher) = index_launcher.as_ref() {
                                    launcher
                                        .corpus_changed(crate::backend::NestCorpus::AddressBook)
                                        .await;
                                }
                            }
                            // A file landed, moved or vanished in one of this
                            // actor's sets — on this device, another of theirs,
                            // or another member of a shared set. Same routing and
                            // same reasoning as the address book above: no rail
                            // owns this corpus, so the cross-set
                            // `fauna.media.list` drain *is* the reconcile, and it
                            // is idempotent (append-shaped + the stage-time
                            // guard), so a spurious or duplicated event costs one
                            // paged read and no publish.
                            Some(ConvPushEvent::SyncFilesChanged) => {
                                if let Some(launcher) = index_launcher.as_ref() {
                                    launcher
                                        .corpus_changed(crate::backend::NestCorpus::Files)
                                        .await;
                                }
                            }
                            // Push subscription closed — keep the ticker backstop.
                            None => push_live = false,
                        }
                    }
                }
            }
        });
        tokio::spawn(async move {
            // Flip the loop's teardown observable when the supervisor leaves —
            // after the reason is recorded, and by ANY path, a cancelled task
            // included. A guard rather than a line after the `await` for the same
            // reason the exits are `select!` arms and not tick checks: a future
            // path must not be able to forget it.
            let _ended = LoopEndGuard(Arc::clone(&supervised_cycles));
            supervise_receive_loop(receive, &supervised_cycles, &stopped_manager, generation).await;
        });
    }
}

/// The four home-nest-resolved / sharer-stamped fields a
/// [`WelcomeChannelKind::Folder`] welcome carries, grouped so a caller can't
/// transpose two of them — e.g. relay a set's *name* as its access *grant*, which
/// a positional signature let happen silently. Threaded through both halves of
/// [`ingest_welcome_by_kind`] (the free fn) /
/// [`ConversationsSession::ingest_welcome_by_kind`] (the method); `None` on every
/// field for a non-folder welcome. Prior art: `federation_pool::
/// originate_welcome_deliver` took its own `FedWelcomeDeliverRequest` for the same
/// reason.
#[derive(Clone, Debug, Default)]
pub struct FolderWelcomeContext {
    pub shared_by: Option<String>,
    pub set_name: Option<String>,
    pub access: Option<String>,
    pub home_nest_actor_id: Option<String>,
    /// The sharer's handle + handle domain (`WelcomePayload.shared_by_handle` /
    /// `shared_by_domain`) — the cross-nest owner label, paired only when the
    /// recipient's own nest bound the domain to the origin's key
    /// (`federation.md` § Cross-nest shared folders + channel append → *The
    /// cross-nest owner label*). Recorded on the accept-time foreign-set record
    /// when both halves are present. Display-only.
    pub shared_by_handle: Option<String>,
    /// See [`Self::shared_by_handle`].
    pub shared_by_domain: Option<String>,
    /// The set name sealed under the set's content keys — the only name a
    /// scrubbed cross-nest Welcome carries; [`join_folder_welcome`] opens it
    /// once the join has ingested custody.
    pub set_name_seal: Option<fauna_core::label_custody::SealedSetName>,
}

/// Ingest an MLS Welcome by [`WelcomeChannelKind`] — the single ingest dispatch
/// shared by the receive loop's **push arm** and the durable-inbox **drain**
/// backstop (`docs/goal/architecture/api-layers.md` § Inbox & Messaging — "Reuse
/// that code — don't fork it"). A [`WelcomeChannelKind::Scheduling`] welcome joins
/// the one-off group + marks the channel scheduling ([`ingest_scheduling_welcome`],
/// no chat thread); a `Dm`/`Group` welcome joins + binds a thread
/// ([`ingest_welcome`]). Both leaf fns are idempotent (a re-delivered Welcome for an
/// already-joined channel is a no-op), which is what makes a Welcome arriving via
/// *both* the push arm and the drain safe. `home_nest_url` is the cross-nest
/// channel's home nest (blank same-nest). Pulling pre-join history is the caller's
/// job — the push arm polls inline; the drain rides the ticker's `poll_bound` /
/// `poll_scheduling_feed` on the same tick (the drain runs first in the ticker arm).
///
/// **Target-agnostic on purpose.** The native rail reaches it through
/// [`ConversationsSession::ingest_welcome_by_kind`]; the web SPA has no session, so
/// `libs/fauna-wasm`'s `WebInboxApply` calls this free fn directly from its drain.
/// Both planes therefore run one dispatch — in particular one folder contact
/// gate — which is the whole point of the "don't fork it" rule above.
///
/// Still 9 args after [`FolderWelcomeContext`] folds in the four folder fields
/// (the method above drops under the limit at 6; this free fn additionally carries
/// `backend`/`manager`/`folder_gate`, which the method already holds as `self`
/// fields) — `#[allow]` stays here, deliberately, verified against clippy's
/// default 7-arg threshold rather than dropped on the (incorrect) assumption both
/// halves would clear it.
#[allow(clippy::too_many_arguments)]
pub async fn ingest_welcome_by_kind(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    kind: &WelcomeChannelKind,
    channel_id_hex: &str,
    welcome_bytes: &[u8],
    home_nest_url: &str,
    welcome_ctx: &FolderWelcomeContext,
    folder_gate: &Option<Arc<dyn FolderGateSink>>,
) -> Result<(), BackendError> {
    match kind {
        WelcomeChannelKind::Scheduling => {
            ingest_scheduling_welcome(backend, channel_id_hex, welcome_bytes, home_nest_url)
                .await?;
        }
        WelcomeChannelKind::Dm | WelcomeChannelKind::Group { .. } => {
            ingest_welcome(
                backend,
                manager,
                channel_id_hex,
                welcome_bytes,
                home_nest_url,
            )
            .await?;
        }
        WelcomeChannelKind::Folder { group_id_hex } => {
            // A cross-user shared folder Welcome (`docs/goal/ui/folders.md`
            // § Sharing). A shared folder is not a conversation, so it binds
            // neither a chat thread (`ingest_welcome`) nor a scheduling channel —
            // the recipient contact gate decides its fate from the sharer's
            // contact-status. Both the push arm and the durable-inbox drain route
            // here (no fork), but only the drain acks; the gate's tri-state maps
            // onto the drain's ack/retain contract:
            //   • no gate registered → `Err` (retain, un-acked) — the pre-gate
            //     behaviour, so a session without the gate never drops the Welcome.
            //   • `Auto` (a Confirmed/Accepted contact) → join the group off the
            //     chat rail (`join_folder_welcome`) then `Ok(())` → the drain acks.
            //   • `Suppress` (a Blocked sharer) → do NOT join, drop the roster row
            //     the share already wrote, `Ok(())` → ack-and-drop (the arrival
            //     never surfaces).
            //   • `Knock` (stranger / Pending / unstamped) → `Err` → un-acked, so the
            //     Welcome stays as the `folder-pending-share` for the recipient to
            //     accept/decline; never joined unbidden. `Err` here is a *pending*
            //     signal, not a fault (the drain absorbs it as a retained skip).
            let Some(gate) = folder_gate else {
                return Err(BackendError::Internal(
                    "folder gate not registered; welcome retained un-acked".to_string(),
                ));
            };
            match gate.arrival_for(welcome_ctx.shared_by.clone()).await {
                ArrivalDisposition::Auto => {
                    join_folder_welcome(
                        backend,
                        channel_id_hex,
                        welcome_bytes,
                        home_nest_url,
                        welcome_ctx,
                    )
                    .await?;
                }
                ArrivalDisposition::Suppress => {
                    // Blocked sharer — ack-and-drop: no join, fall through to
                    // `Ok(())`. But drop the roster row first: the owner's share
                    // rostered this recipient at Welcome delivery, *before* the
                    // block could gate anything, so a bare ack would leave them
                    // shared-with forever — the owner's list lying, and a
                    // post-unblock re-share degenerating into the add path's no-op
                    // access-refresh arm (`folders.md` § Sharing → *Adding the
                    // 2nd..Nth member*). Best-effort by seam contract: a failure
                    // must not retain a blocked sharer's Welcome, and the re-share
                    // heal already repairs a stale row.
                    gate.drop_roster_row(
                        group_id_hex.clone(),
                        (!home_nest_url.is_empty()).then(|| home_nest_url.to_string()),
                    )
                    .await;
                }
                ArrivalDisposition::Knock => {
                    return Err(BackendError::Internal(
                        "folder share pending recipient accept (knock — retained)".to_string(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Drive [`poll_inbound_conv`] over every channel the backend has bound, keeping a
/// per-channel cursor so each call only fetches newer entries. The loop twin of
/// [`ConversationsSession::poll_conversations`]'s sweep (a per-channel failure is
/// non-fatal — the next tick / nudge retries).
///
/// What one sweep of the bound channels answers the Conversation catch-up
/// boundary.
#[cfg(not(target_arch = "wasm32"))]
struct BoundFold {
    /// **Every** bound channel folded cleanly — the boundary's condition, the
    /// exact twin of [`poll_mail_feeds`]'s. A per-channel error leaves that
    /// channel's cursor unadvanced, and a `stalled` outcome means the walk
    /// stopped before a commit this device never incorporated; either way the
    /// fold is unfinished and the next sweep re-pages it, so the backlog must
    /// keep counting as backlog (`content-index-ingest.md` § Ingest triggers, v1
    /// → *The Conversation kind's catch-up*: only a refold that completed
    /// **without error** closes the boundary).
    complete: bool,
    /// The community rooms whose wait for a key-in **ended this sweep**: the
    /// walk met the first key it can open and stopped before that record
    /// ([`ConvPollOutcome::keyed_in`]). Everything behind that stop is the
    /// room's backlog, so the caller reopens the Conversation window before
    /// re-walking them ([`refold_channels`]).
    keyed_in: Vec<ChannelId>,
}

#[cfg(not(target_arch = "wasm32"))]
impl BoundFold {
    fn clean() -> Self {
        Self {
            complete: true,
            keyed_in: Vec::new(),
        }
    }

    fn absorb(&mut self, channel: ChannelId, fold: ChannelFold) {
        self.complete &= fold.complete;
        if fold.keyed_in {
            self.keyed_in.push(channel);
        }
    }
}

/// One channel's answer, as [`BoundFold`] accumulates it.
#[cfg(not(target_arch = "wasm32"))]
struct ChannelFold {
    complete: bool,
    keyed_in: bool,
}

/// Drive [`poll_inbound_conv`] over every channel the backend has bound.
#[cfg(not(target_arch = "wasm32"))]
async fn poll_bound(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    cursors: &mut HashMap<ChannelId, i64>,
) -> BoundFold {
    adopt_sibling_groups_first(backend).await;
    let mut fold = BoundFold::clean();
    for channel in backend.bound_channels() {
        fold.absorb(
            channel,
            fold_channel(backend, manager, cursors, channel).await,
        );
    }
    fold
}

/// Walk exactly `channels` again — the rooms [`BoundFold::keyed_in`] named,
/// once their catch-up window has been reopened, so the backlog their wait held
/// back reaches the index seam as backlog.
///
/// Deliberately **after** the sweep's own loop rather than in place of its stop:
/// the window is open across these walks, so the fewer channels and the shorter
/// the stretch, the smaller the chance a concurrent app-driven sweep's live
/// trickle is counted as backlog (harmless on the lease-holding seat; on a
/// stood-down one it withholds that message until the next boundary).
#[cfg(not(target_arch = "wasm32"))]
async fn refold_channels(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    cursors: &mut HashMap<ChannelId, i64>,
    channels: &[ChannelId],
) -> BoundFold {
    let mut fold = BoundFold::clean();
    for channel in channels.iter().copied() {
        fold.absorb(
            channel,
            fold_channel(backend, manager, cursors, channel).await,
        );
    }
    fold
}

/// One bound channel: fold it, then run the three post-walk steps it owes.
#[cfg(not(target_arch = "wasm32"))]
async fn fold_channel(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    cursors: &mut HashMap<ChannelId, i64>,
    channel: ChannelId,
) -> ChannelFold {
    let mut fold = ChannelFold {
        complete: true,
        keyed_in: false,
    };
    {
        // First encounter seeds from the injected `ChannelCursor` (`resume_seq` —
        // the restored `history/<ch>` watermark on a device-synced client, else
        // `0`), so a restored device resumes rather than re-walking history.
        let cursor = cursors.entry(channel).or_insert_with(|| {
            backend
                .channel_cursor()
                .map(|c| c.resume_seq(&channel))
                .unwrap_or(0)
        });
        // Serialize this channel's inbound drain against a concurrent gated commit
        // ([`FaunaMlsBackend::channel_lock`]); the gate's inner catch-up poll runs
        // inside the gated section and never re-takes it.
        let lock = backend.channel_lock(&channel);
        let guard = lock.lock().await;
        let polled = poll_inbound_conv(backend, manager, &channel, cursor, 0).await;
        // An ownership offer the walk parked for this identity completes here
        // — after the walk, under the same lock, never inside it
        // (`FaunaMlsBackend::complete_ownership_offer_locked`).
        backend.complete_ownership_offer_locked(&channel).await;
        match polled {
            Ok(outcome) => {
                fold.keyed_in = outcome.keyed_in;
                if outcome.stalled {
                    // The walk stopped before a commit this device never
                    // incorporated; `cursor` sits behind it, so reporting it is
                    // still correct (and required — the watermark must not run
                    // ahead of the crypto state, `devices.md` Rule 2). The strand
                    // stays loud: the next tick retries the heal.
                    if outcome.awaiting_key {
                        // A community room this account is not keyed into yet
                        // — an expected wait for the inviter's key-in, not a
                        // strand.
                        //
                        // **Folded for now, so the boundary may close without
                        // it.** The wait has no bound: it ends when an owner's
                        // or admin's device next polls, which may be never
                        // (`community-rooms.md` § Implementation status today →
                        // *A newcomer's walk waits for its key-in*). Counting it
                        // as an unfinished fold held the account's WHOLE
                        // Conversation catch-up open for as long as one room
                        // went unkeyed — every other channel's backlog withheld
                        // on a stood-down seat, and `observe_catch_up_complete`
                        // never fired. The room's own backlog is not lost by
                        // closing: its cursor stays before the record, and its
                        // key-in reopens this window before the re-walk
                        // (`content-index-ingest.md` § Ingest triggers, v1 → *A
                        // community room waiting for its key-in*).
                        tracing::debug!(
                            channel = %channel,
                            "community room not keyed in yet — the walk waits before its \
                             first sealed record"
                        );
                    } else {
                        tracing::error!(
                            channel = %channel,
                            "inbound poll stalled before an unincorporated commit — channel \
                             is behind the group's epoch until a resync heals it"
                        );
                        // An unfinished fold, so the catch-up backlog is
                        // unfinished.
                        fold.complete = false;
                    }
                }
                // Report the folded seq back so the next `history/<ch>` save
                // snapshots the right watermark (a no-op with no cursor seam).
                if let Some(c) = backend.channel_cursor() {
                    c.advance(&channel, *cursor);
                }
            }
            Err(e) => {
                tracing::error!("poll {channel}: {e}");
                fold.complete = false;
            }
        }
        // Name any member the walk seated that this device has never met
        // (`conversation-rooms.md` § Implementation status today, the roster
        // bullet). Deliberately **after the guard is dropped**, unlike the
        // ownership-offer completion above: this awaits a nest round trip,
        // and holding the channel lock across one would stall every
        // concurrent gated commit on this channel for its duration. Nothing
        // downstream waits on the answer — it only turns an elided actor id
        // into a name — so it is the one step in this loop that is safe to
        // run unlocked, and the only one that must.
        drop(guard);
        // A community room's floor, before the handle read: the floor names
        // its members in the same answer, so the read below finds nobody left
        // to ask about (`FaunaMlsBackend::tend_community_room`).
        backend.tend_community_room(manager, &channel).await;
        // The group-ful mirror: a failed best-effort birth report is never
        // retried, so the room's floor is still empty
        // (`FaunaMlsBackend::backfill_floor_roster`).
        backend.backfill_floor_roster(&channel).await;
        backend.resolve_nameless_members(manager, &channel).await;
    }
    fold
}

/// Drive [`poll_inbound_scheduling`] over every scheduling channel the backend has
/// joined (the mailbox-less CalDAV iMIP rail), each with its own cursor, handing
/// each decrypted iMIP to `sink`. A no-op when no sink is registered. The
/// scheduling twin of [`poll_bound`]: a per-channel failure is non-fatal (logged;
/// the next tick / nudge retries).
///
/// Target-agnostic and `pub` so the browser drives the identical loop from its own
/// JS receive tick (`libs/fauna-wasm`'s `pollScheduling`) — exactly as
/// [`poll_folder_feed`] is driven. Without it a mailbox-less web attendee joins the
/// `Scheduling` channel and never applies the iMIP sitting on it, so an invitation
/// sent from another calendar never reaches the SPA's Events page: the drain, not
/// the apply, is what the web leg of the mailbox-less rail was missing
/// (`docs/goal/behavior/caldav-server.md` § Server-side auto-schedule, Half-1 —
/// "needs a client-side scheduling-inbox drain loop").
pub async fn poll_scheduling_feed(
    backend: &FaunaMlsBackend,
    sink: &Option<Arc<dyn SchedulingSink>>,
    cursors: &mut HashMap<ChannelId, i64>,
) {
    let Some(sink) = sink else {
        return;
    };
    for channel in backend.scheduling_channels() {
        let cursor = cursors.entry(channel).or_insert(0);
        if let Err(e) = poll_inbound_scheduling(backend, sink.as_ref(), &channel, cursor, 0).await {
            tracing::error!("poll scheduling {channel}: {e}");
        }
    }
}

/// Drive [`poll_inbound_folder`] over every folder channel the engine holds
/// (derived — [`FaunaMlsBackend::folder_poll_channels`], restart-durable) —
/// the remaining members' epoch-advance liveness for shared folders (5d(d)):
/// applies owner-posted membership commits (rotate-on-removal) so this member
/// can open the re-published content-key envelope. The folder twin of
/// [`poll_scheduling_feed`]; per-channel failure is non-fatal. Serialized per
/// channel against a concurrent gated commit like [`poll_bound`] (the same
/// engine backs both rails).
///
/// Target-agnostic and `pub` so the browser drives the identical loop from its own
/// JS receive tick (`libs/fauna-wasm`'s `pollFolders`) — without it a web member
/// never applies the owner's rotate-on-removal commit and fails closed on the
/// re-published envelope, which is the whole liveness half of
/// `mls-group-key-material.md` § Rotate-on-removal.
pub async fn poll_folder_feed(backend: &FaunaMlsBackend, cursors: &mut HashMap<ChannelId, i64>) {
    for channel in backend.folder_poll_channels() {
        let cursor = cursors.entry(channel).or_insert(0);
        let lock = backend.channel_lock(&channel);
        // This walk holds the SAME per-channel lock an application send takes
        // for its epoch takeover (`fauna_mls::post_app_message`), so a long
        // walk defers every send on that channel behind it. These three lines
        // are what let a run attribute an idle stretch to the poll rather than
        // to the gate.
        tracing::debug!(%channel, cursor = *cursor, "folder poll: awaiting the channel lock");
        let _guard = lock.lock().await;
        tracing::debug!(%channel, cursor = *cursor, "folder poll: channel lock held; walking the inbound feed");
        if let Err(e) = poll_inbound_folder(backend, &channel, cursor, 0).await {
            tracing::error!("poll folder {channel}: {e}");
        }
        tracing::debug!(%channel, cursor = *cursor, "folder poll: walk done; releasing the channel lock");
    }
}

/// The bridged rail's receive half as [`ConversationsSession::register_bridged`]
/// wired it.
#[derive(Clone)]
struct BridgedFeed {
    backend: Arc<BridgedBackend>,
    source: Arc<dyn BridgedSource>,
}

/// Drive [`poll_inbound_bridged`] over the wired bridged feed, under the
/// session's one cursor — the lock is held across the read, so a manual poll
/// and the receive loop's sweep can never ingest a row twice. `Ok(0)` with no
/// feed registered.
async fn poll_bridged_feed(
    feed: Option<&BridgedFeed>,
    manager: &ConversationsManager,
    cursor: &futures_util::lock::Mutex<(i64, HashSet<i64>)>,
) -> Result<u32, BackendError> {
    let Some(feed) = feed else {
        return Ok(0);
    };
    let mut guard = cursor.lock().await;
    let (after_id, seen) = &mut *guard;
    let n = poll_inbound_bridged(
        feed.source.as_ref(),
        &feed.backend,
        manager,
        after_id,
        seen,
        0,
    )
    .await?;
    Ok(n as u32)
}

/// Drive [`poll_inbound_mail`] over the wired `INBOX` + `Sent` read-feeds (when
/// registered), each with its own `after_uid` cursor. The mail twin of
/// [`poll_bound`]: a per-mailbox failure is non-fatal (logged; the next tick
/// retries), and a fresh dedup set per call is sufficient because `after_uid` is
/// monotonic per mailbox.
///
/// Answers **whether every configured feed was polled to its tip without an
/// error** — the one thing the caller cannot infer from the cursors, since a
/// failed poll leaves them exactly where a successful empty one does. `true` for
/// a client with no mail feeds at all (nothing to walk is a completed walk); the
/// receive loop's `mail_sweep!` uses it to decide whether the launch catch-up
/// backlog is genuinely behind it. Both feeds are always attempted — a failing
/// INBOX must not silently skip Sent — so this is a fold over both results, not
/// an early return.
#[cfg(not(target_arch = "wasm32"))]
async fn poll_mail_feeds(
    inbox: &Option<Arc<dyn InboundMailSource>>,
    sent: &Option<Arc<dyn InboundMailSource>>,
    manager: &ConversationsManager,
    inbox_uid: &mut u32,
    sent_uid: &mut u32,
) -> bool {
    let mut complete = true;
    if let Some(src) = inbox {
        let mut seen = HashSet::new();
        if let Err(e) = poll_inbound_mail(src.as_ref(), manager, inbox_uid, &mut seen, 0).await {
            tracing::error!("poll inbox mail: {e}");
            complete = false;
        }
    }
    if let Some(src) = sent {
        let mut seen = HashSet::new();
        if let Err(e) = poll_inbound_mail(src.as_ref(), manager, sent_uid, &mut seen, 0).await {
            tracing::error!("poll sent mail: {e}");
            complete = false;
        }
    }
    complete
}

/// The backstop cadence rule ([`resolve_poll_secs`]) and the receive-cycle
/// counters' ordering contract — the tier_1 half convention 14 asks for, so the
/// e2e keeps one *wiring* proof (the poke) rather than demonstrating interval
/// arithmetic through a running client
/// (`docs/goal/architecture/e2e-conventions.md` § convention 14, mechanism 3).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod cadence_tests {
    use super::{DEFAULT_CONV_POLL_SECS, ReceiveCycles, resolve_poll_secs};

    #[test]
    fn an_unset_override_leaves_the_production_cadence() {
        assert_eq!(resolve_poll_secs(None), DEFAULT_CONV_POLL_SECS);
    }

    #[test]
    fn a_positive_override_is_honoured() {
        assert_eq!(resolve_poll_secs(Some("2".to_string())), 2);
    }

    /// `0` would make `tokio::time::interval` a busy loop, so it falls back
    /// rather than spinning the client — the case a mis-set override reaches.
    #[test]
    fn a_zero_override_falls_back_instead_of_spinning() {
        assert_eq!(
            resolve_poll_secs(Some("0".to_string())),
            DEFAULT_CONV_POLL_SECS
        );
    }

    /// Unparseable input is the same class as unset: the loop must still run a
    /// backstop, never no backstop at all.
    #[test]
    fn an_unparseable_override_falls_back() {
        assert_eq!(
            resolve_poll_secs(Some("soon".to_string())),
            DEFAULT_CONV_POLL_SECS
        );
        assert_eq!(
            resolve_poll_secs(Some("-1".to_string())),
            DEFAULT_CONV_POLL_SECS
        );
    }

    /// A fresh session has begun nothing and finished nothing — the zero a
    /// consumer's baseline read must be able to trust.
    #[test]
    fn a_fresh_counter_pair_is_zero() {
        let cycles = ReceiveCycles::default();
        assert_eq!((cycles.started(), cycles.completed()), (0, 0));
    }

    /// `completed` may never run ahead of `started`. The pigeonhole argument in
    /// [`ReceiveCycles`] rests on a cycle being counted at its *beginning*,
    /// before it reads any rail: a cycle in flight when the consumer took its
    /// baseline is exactly the one that must not satisfy the consumer's wait.
    #[test]
    fn a_cycle_in_flight_leaves_completed_behind_started() {
        let cycles = ReceiveCycles::default();
        cycles.begin();
        assert_eq!((cycles.started(), cycles.completed()), (1, 0));
        cycles.finish();
        assert_eq!((cycles.started(), cycles.completed()), (1, 1));
        cycles.begin();
        assert!(
            cycles.completed() < cycles.started(),
            "a consumer that read started=1 before its trigger waits for \
             completed>1; this in-flight cycle began earlier and must not be \
             what releases it"
        );
    }
}

/// [`ConversationsSession::weak_room_post_keys`] holds nothing alive: a seam
/// whose session has gone answers exactly what an unset seam does — every post
/// locked (with a reason), no room to address, no foreign home
/// (`transport-connection.md` § No dialer outlives its owner).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod weak_room_post_keys_tests {
    use super::WeakRoomPostKeys;
    use fauna_core::room_post::RoomPostKeys;

    #[tokio::test]
    async fn a_departed_session_opens_nothing() {
        let keys = WeakRoomPostKeys(std::sync::Weak::new());
        assert!(keys.room_post_seal_key([7; 32]).await.is_err());
        assert!(keys.room_post_rooms().await.is_empty());
        assert_eq!(keys.room_home_nest_url([7; 32]).await, None);
    }
}
